//! Evaluation of `@cond(...)` conditions and `config$` values against compiler definitions.
//!
//! Evaluation is a pure function of the syntax it is given and the configuration it is
//! handed: it never resolves a name, reads a type, or queries an item.

use crate::compiler_definitions::{
    CompilerDefinitions, DefinitionValue, LiteralValueError, declared_numeric_kind, decode_literal,
    decode_number, default_numeric_kind, validate_literal,
};
use crate::resolved_type::NumericKind;
use omega_parser::prelude::{
    AnnotationArg, AnnotationExpr, AnnotationExprKind, AnnotationLiteral, AnnotationNode, Ident,
    ItemNode, NumberBase, NumberExpr, Origin, Span,
};
use std::fmt;

/// The only namespace a condition may qualify a name with. It holds the
/// definitions the command line supplied, and nothing else: it is not a
/// module, and it never takes part in ordinary name resolution.
pub const DEFINITION_NAMESPACE: &str = "def";

const DEFAULT_OPERATOR: &str = "default";

#[derive(Debug, Clone)]
pub struct ConditionError {
    pub span: Span,
    /// Provenance of the offending syntax, so a condition written inside a
    /// macro body can be attributed to that macro.
    pub origin: Origin,
    pub kind: ConditionErrorKind,
}

#[derive(Debug, Clone)]
pub enum ConditionErrorKind {
    MalformedCondition,
    DuplicateCondition {
        first: Span,
    },
    UnknownName(Ident),
    UnknownNamespace(Ident),
    UnknownCall(Ident),
    MissingDefinition(Ident),
    ExpectedBoolean {
        found: String,
    },
    ExpectedValue {
        found: String,
    },
    ExpectedList {
        found: String,
    },
    WrongArity {
        call: Ident,
        expected: &'static str,
        found: usize,
    },
    MismatchedKinds {
        left: String,
        right: String,
    },
    NotOrderable {
        found: String,
    },
    InvalidLiteral(LiteralValueError),
    DefaultNeedsDefinition {
        found: String,
    },
    DefaultNeedsLiteral {
        found: String,
    },
    DefaultKindMismatch {
        definition: &'static str,
        fallback: &'static str,
    },
}

impl fmt::Display for ConditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ConditionErrorKind::MalformedCondition => f.write_str(
                "'@cond' takes exactly one condition, written as a single positional argument",
            ),
            ConditionErrorKind::DuplicateCondition { .. } => {
                f.write_str("this item already has a '@cond' annotation")
            }
            ConditionErrorKind::UnknownName(name) => write!(
                f,
                "'{name}' is not a compiler definition -- a definition supplied on the command \
                 line is read as '{DEFINITION_NAMESPACE}::{name}'"
            ),
            ConditionErrorKind::UnknownNamespace(namespace) => write!(
                f,
                "'{namespace}' is not a condition namespace -- only \
                 '{DEFINITION_NAMESPACE}::<name>' names a compiler definition"
            ),
            ConditionErrorKind::UnknownCall(name) => {
                write!(f, "'{name}' is not a condition operator")
            }
            ConditionErrorKind::MissingDefinition(name) => write!(
                f,
                "compiler definition '{name}' is not defined, so it has no value to compare"
            ),
            ConditionErrorKind::ExpectedBoolean { found } => {
                write!(f, "expected a boolean condition, found {found}")
            }
            ConditionErrorKind::ExpectedValue { found } => {
                write!(f, "expected a value, found {found}")
            }
            ConditionErrorKind::ExpectedList { found } => {
                write!(f, "expected a list written as '&[...]', found {found}")
            }
            ConditionErrorKind::WrongArity {
                call,
                expected,
                found,
            } => write!(f, "'{call}' takes {expected}, found {found}"),
            ConditionErrorKind::MismatchedKinds { left, right } => write!(
                f,
                "{left} and {right} are different kinds of value and cannot be compared"
            ),
            ConditionErrorKind::NotOrderable { found } => {
                write!(f, "{found} has no ordering -- only numbers can be ordered")
            }
            ConditionErrorKind::InvalidLiteral(error) => write!(f, "{error}"),
            ConditionErrorKind::DefaultNeedsDefinition { found } => write!(
                f,
                "'default' takes '{DEFINITION_NAMESPACE}::<name>' as its first operand, found \
                 {found}"
            ),
            ConditionErrorKind::DefaultNeedsLiteral { found } => write!(
                f,
                "'default' takes a literal as its fallback, found {found}"
            ),
            ConditionErrorKind::DefaultKindMismatch {
                definition,
                fallback,
            } => write!(
                f,
                "the definition is {definition} but its fallback is {fallback} -- both must be \
                 the same kind of value"
            ),
        }
    }
}

impl ConditionError {
    fn new(expr: &AnnotationExpr, kind: ConditionErrorKind) -> Self {
        Self {
            span: expr.span,
            origin: expr.origin,
            kind,
        }
    }

    /// The label shown under the offending syntax. The message already says
    /// what went wrong, so this only points at it.
    pub fn label(&self) -> &'static str {
        match self.kind {
            ConditionErrorKind::DuplicateCondition { .. } => "duplicate condition",
            _ => "in this condition",
        }
    }
}

/// Decides whether an item survives its conditions and consumes them, so no
/// later phase can see, re-evaluate, or act on a condition again.
pub fn item_is_enabled(
    definitions: &CompilerDefinitions,
    node: &mut ItemNode,
) -> Result<bool, ConditionError> {
    if node.conditions.is_empty() {
        return Ok(true);
    }
    let conditions = std::mem::take(&mut node.conditions);
    // A repeated condition is a contradiction in the source regardless of
    // what either one evaluates to, so it is rejected before evaluation.
    if let Some(second) = conditions.get(1) {
        return Err(ConditionError {
            span: second.span,
            origin: second.origin,
            kind: ConditionErrorKind::DuplicateCondition {
                first: conditions[0].span,
            },
        });
    }
    evaluate(definitions, &conditions[0])
}

pub fn evaluate(
    definitions: &CompilerDefinitions,
    annotation: &AnnotationNode,
) -> Result<bool, ConditionError> {
    let [AnnotationArg::Positional(condition)] = annotation.args.as_slice() else {
        return Err(ConditionError {
            span: annotation.span,
            origin: annotation.origin,
            kind: ConditionErrorKind::MalformedCondition,
        });
    };
    Evaluator { definitions }.boolean(condition)
}

/// Evaluates one condition expression in value position and returns the
/// literal it stands for. A definition, a `default`, or a written literal is
/// returned as written, so an unsuffixed number stays adaptable to its use; a
/// builtin becomes a literal of its declared type; an operator call becomes a
/// boolean. Unlike a boolean position, an absent bare definition is an error.
pub fn evaluate_value(
    definitions: &CompilerDefinitions,
    expr: &AnnotationExpr,
) -> Result<AnnotationLiteral, ConditionError> {
    let evaluator = Evaluator { definitions };
    let pointer_bits = definitions.target().pointer_bits();
    let checked = |expr: &AnnotationExpr, literal: &AnnotationLiteral| {
        validate_literal(literal, pointer_bits)
            .map(|()| literal.clone())
            .map_err(|error| ConditionError::new(expr, ConditionErrorKind::InvalidLiteral(error)))
    };
    match &expr.kind {
        AnnotationExprKind::Literal(literal) => checked(expr, literal),
        AnnotationExprKind::Name(name) => Ok(value_literal(&evaluator.builtin(expr, name)?)),
        AnnotationExprKind::Qualified { namespace, name } => {
            evaluator.check_namespace(expr, namespace)?;
            checked(expr, evaluator.supplied(expr, name)?)
        }
        AnnotationExprKind::Call { name, args } if name.as_ref() == DEFAULT_OPERATOR => {
            let operands = evaluator.default_operands(expr, name, args)?;
            let definition = operands
                .definition
                .map(|literal| checked(operands.definition_expr, literal))
                .transpose()?;
            let fallback = checked(operands.fallback_expr, operands.fallback)?;
            Ok(definition.unwrap_or(fallback))
        }
        AnnotationExprKind::Call { name, args } => evaluator
            .call(expr, name, args)
            .map(AnnotationLiteral::Bool),
        _ => Err(ConditionError::new(
            expr,
            ConditionErrorKind::ExpectedValue {
                found: expr.describe().to_string(),
            },
        )),
    }
}

/// A decoded value written back as a literal of its own type.
fn value_literal(value: &DefinitionValue) -> AnnotationLiteral {
    let number = |negative: bool, integer_part: String, fractional_part, suffix: String| {
        AnnotationLiteral::Number {
            negative,
            value: NumberExpr {
                base: NumberBase::Decimal,
                integer_part,
                fractional_part,
                explicit_type: Some(Ident(suffix)),
            },
        }
    };
    match value {
        DefinitionValue::Bool(value) => AnnotationLiteral::Bool(*value),
        DefinitionValue::Char(value) => AnnotationLiteral::Char(*value),
        DefinitionValue::Str(value) => AnnotationLiteral::Str(value.clone()),
        DefinitionValue::ByteStr(value) => AnnotationLiteral::ByteStr(value.clone()),
        DefinitionValue::Int { value, r#type } => number(
            *value < 0,
            value.unsigned_abs().to_string(),
            None,
            format!("{}{}", if r#type.signed { "i" } else { "u" }, r#type.width),
        ),
        DefinitionValue::Float { value, width } => {
            let text = value.abs().to_string();
            let (integer, fraction) = text.split_once('.').unwrap_or((&text, "0"));
            number(
                value.is_sign_negative(),
                integer.to_string(),
                Some(fraction.to_string()),
                format!("f{width}"),
            )
        }
    }
}

/// The kind of value a literal is written as, before any numeric context
/// decides its width.
fn literal_kind(literal: &AnnotationLiteral) -> &'static str {
    match literal {
        AnnotationLiteral::Number { value, .. }
            if value.fractional_part.is_some()
                || value
                    .explicit_type
                    .as_ref()
                    .is_some_and(|suffix| matches!(suffix.as_ref(), "f32" | "f64")) =>
        {
            "a float"
        }
        AnnotationLiteral::Number { .. } => "an integer",
        AnnotationLiteral::Bool(_) => "a boolean",
        AnnotationLiteral::Char(_) => "a character",
        AnnotationLiteral::Str(_) => "a string",
        AnnotationLiteral::ByteStr(_) => "a byte string",
    }
}

fn declared_kind(literal: &AnnotationLiteral, pointer_bits: u32) -> Option<NumericKind> {
    match literal {
        AnnotationLiteral::Number { value, .. } => {
            declared_numeric_kind(value, pointer_bits).ok().flatten()
        }
        _ => None,
    }
}

struct DefaultOperands<'e> {
    definition_expr: &'e AnnotationExpr,
    definition: Option<&'e AnnotationLiteral>,
    fallback_expr: &'e AnnotationExpr,
    fallback: &'e AnnotationLiteral,
}

struct Evaluator<'a> {
    definitions: &'a CompilerDefinitions,
}

impl Evaluator<'_> {
    /// A boolean-expected position. Only here does an absent definition mean
    /// `false`; a supplied value of another kind is still an error, because
    /// nothing in Omega converts a value to a truth.
    fn boolean(&self, expr: &AnnotationExpr) -> Result<bool, ConditionError> {
        match &expr.kind {
            AnnotationExprKind::Literal(AnnotationLiteral::Bool(value)) => Ok(*value),
            AnnotationExprKind::Name(name) => {
                let value = self.builtin(expr, name)?;
                self.expect_bool(expr, &value)
            }
            AnnotationExprKind::Qualified { namespace, name } => {
                self.check_namespace(expr, namespace)?;
                match self.definitions.user(name) {
                    None => Ok(false),
                    Some(literal) => {
                        let value = self.literal(expr, literal, None)?;
                        self.expect_bool(expr, &value)
                    }
                }
            }
            AnnotationExprKind::Call { name, args } if name.as_ref() == DEFAULT_OPERATOR => {
                let value = self.default(expr, name, args, None)?;
                self.expect_bool(expr, &value)
            }
            AnnotationExprKind::Call { name, args } => self.call(expr, name, args),
            _ => Err(ConditionError::new(
                expr,
                ConditionErrorKind::ExpectedBoolean {
                    found: expr.describe().to_string(),
                },
            )),
        }
    }

    fn expect_bool(
        &self,
        expr: &AnnotationExpr,
        value: &DefinitionValue,
    ) -> Result<bool, ConditionError> {
        value.as_bool().ok_or_else(|| {
            ConditionError::new(
                expr,
                ConditionErrorKind::ExpectedBoolean {
                    found: value.kind_name().to_string(),
                },
            )
        })
    }

    /// A value position. Every definition named here must exist: a comparison
    /// against a definition that was never supplied has no answer.
    fn value(
        &self,
        expr: &AnnotationExpr,
        context: Option<NumericKind>,
    ) -> Result<DefinitionValue, ConditionError> {
        match &expr.kind {
            AnnotationExprKind::Literal(literal) => self.literal(expr, literal, context),
            AnnotationExprKind::Name(name) => self.builtin(expr, name),
            AnnotationExprKind::Qualified { namespace, name } => {
                self.check_namespace(expr, namespace)?;
                let literal = self.supplied(expr, name)?;
                self.literal(expr, literal, context)
            }
            AnnotationExprKind::Call { name, args } if name.as_ref() == DEFAULT_OPERATOR => {
                self.default(expr, name, args, context)
            }
            AnnotationExprKind::Call { name, args } => {
                self.call(expr, name, args).map(DefinitionValue::Bool)
            }
            _ => Err(ConditionError::new(
                expr,
                ConditionErrorKind::ExpectedValue {
                    found: expr.describe().to_string(),
                },
            )),
        }
    }

    fn literal(
        &self,
        expr: &AnnotationExpr,
        literal: &AnnotationLiteral,
        context: Option<NumericKind>,
    ) -> Result<DefinitionValue, ConditionError> {
        let pointer_bits = self.definitions.target().pointer_bits();
        let invalid = |error| ConditionError::new(expr, ConditionErrorKind::InvalidLiteral(error));
        let AnnotationLiteral::Number { negative, value } = literal else {
            return decode_literal(literal, pointer_bits).map_err(invalid);
        };
        let kind = match declared_numeric_kind(value, pointer_bits).map_err(invalid)? {
            Some(declared) => declared,
            None => context
                .filter(|kind| {
                    matches!(kind, NumericKind::Float(_)) == value.fractional_part.is_some()
                })
                .unwrap_or_else(|| default_numeric_kind(value)),
        };
        decode_number(value, *negative, kind).map_err(invalid)
    }

    /// The numeric type an operand already has, which its unsuffixed
    /// counterpart adopts. An operand whose own decoding fails contributes no
    /// context; the failure is reported when it is evaluated.
    fn established_kind(&self, expr: &AnnotationExpr) -> Option<NumericKind> {
        let pointer_bits = self.definitions.target().pointer_bits();
        match &expr.kind {
            AnnotationExprKind::Literal(AnnotationLiteral::Number { value, .. }) => {
                declared_numeric_kind(value, pointer_bits).ok().flatten()
            }
            AnnotationExprKind::Name(name) => self
                .definitions
                .builtin(name)
                .and_then(|value| value.numeric_kind()),
            AnnotationExprKind::Qualified { namespace, name }
                if namespace.as_ref() == DEFINITION_NAMESPACE =>
            {
                self.definitions
                    .user(name)
                    .and_then(|literal| declared_kind(literal, pointer_bits))
            }
            AnnotationExprKind::Call { name, args }
                if name.as_ref() == DEFAULT_OPERATOR && args.len() == 2 =>
            {
                self.established_kind(&args[0])
                    .or_else(|| self.established_kind(&args[1]))
            }
            _ => None,
        }
    }

    fn supplied(
        &self,
        expr: &AnnotationExpr,
        name: &Ident,
    ) -> Result<&AnnotationLiteral, ConditionError> {
        self.definitions.user(name).ok_or_else(|| {
            ConditionError::new(expr, ConditionErrorKind::MissingDefinition(name.clone()))
        })
    }

    /// `default(def::name, literal)`, decoded in `context`. Each operand is
    /// also decoded in the other's declared kind, so a suffix on either side
    /// types both.
    fn default(
        &self,
        expr: &AnnotationExpr,
        name: &Ident,
        args: &[AnnotationExpr],
        context: Option<NumericKind>,
    ) -> Result<DefinitionValue, ConditionError> {
        let operands = self.default_operands(expr, name, args)?;
        let definition = operands
            .definition
            .map(|literal| {
                self.literal(
                    operands.definition_expr,
                    literal,
                    self.established_kind(operands.fallback_expr).or(context),
                )
            })
            .transpose()?;
        let fallback = self.literal(
            operands.fallback_expr,
            operands.fallback,
            self.established_kind(operands.definition_expr).or(context),
        )?;
        Ok(definition.unwrap_or(fallback))
    }

    /// The shape and kind checks `default` makes whatever position it is in.
    /// The kinds are compared as written, so the check never depends on which
    /// operand is chosen.
    fn default_operands<'e>(
        &'e self,
        expr: &AnnotationExpr,
        name: &Ident,
        args: &'e [AnnotationExpr],
    ) -> Result<DefaultOperands<'e>, ConditionError> {
        let [definition_expr, fallback_expr] =
            self.exact(expr, name, args, "a definition and a literal")?;
        let AnnotationExprKind::Qualified {
            namespace,
            name: definition_name,
        } = &definition_expr.kind
        else {
            return Err(ConditionError::new(
                definition_expr,
                ConditionErrorKind::DefaultNeedsDefinition {
                    found: definition_expr.describe().to_string(),
                },
            ));
        };
        self.check_namespace(definition_expr, namespace)?;
        let AnnotationExprKind::Literal(fallback) = &fallback_expr.kind else {
            return Err(ConditionError::new(
                fallback_expr,
                ConditionErrorKind::DefaultNeedsLiteral {
                    found: fallback_expr.describe().to_string(),
                },
            ));
        };
        let definition = self.definitions.user(definition_name);
        if let Some(literal) = definition {
            let (definition_kind, fallback_kind) = (literal_kind(literal), literal_kind(fallback));
            if definition_kind != fallback_kind {
                return Err(ConditionError::new(
                    expr,
                    ConditionErrorKind::DefaultKindMismatch {
                        definition: definition_kind,
                        fallback: fallback_kind,
                    },
                ));
            }
        }
        Ok(DefaultOperands {
            definition_expr,
            definition,
            fallback_expr,
            fallback,
        })
    }

    fn builtin(
        &self,
        expr: &AnnotationExpr,
        name: &Ident,
    ) -> Result<DefinitionValue, ConditionError> {
        self.definitions
            .builtin(name)
            .ok_or_else(|| ConditionError::new(expr, ConditionErrorKind::UnknownName(name.clone())))
    }

    fn check_namespace(
        &self,
        expr: &AnnotationExpr,
        namespace: &Ident,
    ) -> Result<(), ConditionError> {
        if namespace.as_ref() == DEFINITION_NAMESPACE {
            Ok(())
        } else {
            Err(ConditionError::new(
                expr,
                ConditionErrorKind::UnknownNamespace(namespace.clone()),
            ))
        }
    }

    /// Every operator is checked left to right and every operand is
    /// evaluated, so a mistake in an operand that could not change the answer
    /// is still reported.
    fn call(
        &self,
        expr: &AnnotationExpr,
        name: &Ident,
        args: &[AnnotationExpr],
    ) -> Result<bool, ConditionError> {
        match name.as_ref() {
            "not" => {
                let [operand] = self.exact(expr, name, args, "exactly one condition")?;
                Ok(!self.boolean(operand)?)
            }
            "all" | "any" => {
                let mut results = Vec::with_capacity(args.len());
                for arg in args {
                    results.push(self.boolean(arg)?);
                }
                Ok(match name.as_ref() {
                    "all" => results.iter().all(|value| *value),
                    _ => results.iter().any(|value| *value),
                })
            }
            "equals" => {
                let (left, right) = self.two_values(expr, name, args)?;
                self.equals(expr, &left, &right)
            }
            "less" | "less_equal" | "greater" | "greater_equal" => {
                let (left, right) = self.two_values(expr, name, args)?;
                let ordering = self.ordering(expr, &left, &right)?;
                Ok(match name.as_ref() {
                    "less" => ordering.is_lt(),
                    "less_equal" => ordering.is_le(),
                    "greater" => ordering.is_gt(),
                    _ => ordering.is_ge(),
                })
            }
            "in" => {
                let [candidate, list] = self.exact(expr, name, args, "a value and a list")?;
                let value = self.value(candidate, None)?;
                let AnnotationExprKind::List(elements) = &list.kind else {
                    return Err(ConditionError::new(
                        list,
                        ConditionErrorKind::ExpectedList {
                            found: list.describe().to_string(),
                        },
                    ));
                };
                let mut found = false;
                for element in elements {
                    let element_value = self.value(element, value.numeric_kind())?;
                    found |= self.equals(element, &value, &element_value)?;
                }
                Ok(found)
            }
            _ => Err(ConditionError::new(
                expr,
                ConditionErrorKind::UnknownCall(name.clone()),
            )),
        }
    }

    fn exact<'e, const N: usize>(
        &self,
        expr: &AnnotationExpr,
        name: &Ident,
        args: &'e [AnnotationExpr],
        expected: &'static str,
    ) -> Result<[&'e AnnotationExpr; N], ConditionError> {
        if args.len() != N {
            return Err(ConditionError::new(
                expr,
                ConditionErrorKind::WrongArity {
                    call: name.clone(),
                    expected,
                    found: args.len(),
                },
            ));
        }
        Ok(std::array::from_fn(|index| &args[index]))
    }

    /// Both operands of a comparison, each decoded in the other's numeric
    /// context so an unsuffixed literal means the same thing on either side.
    fn two_values(
        &self,
        expr: &AnnotationExpr,
        name: &Ident,
        args: &[AnnotationExpr],
    ) -> Result<(DefinitionValue, DefinitionValue), ConditionError> {
        let [left, right] = self.exact(expr, name, args, "exactly two values")?;
        let left_value = self.value(left, self.established_kind(right))?;
        let right_value = self.value(right, self.established_kind(left))?;
        Ok((left_value, right_value))
    }

    fn equals(
        &self,
        expr: &AnnotationExpr,
        left: &DefinitionValue,
        right: &DefinitionValue,
    ) -> Result<bool, ConditionError> {
        use DefinitionValue::*;
        match (left, right) {
            (Bool(a), Bool(b)) => Ok(a == b),
            (Char(a), Char(b)) => Ok(a == b),
            (Str(a), Str(b)) => Ok(a == b),
            (ByteStr(a), ByteStr(b)) => Ok(a.as_bytes() == b.as_bytes()),
            (Int { value: a, .. }, Int { value: b, .. }) => Ok(a == b),
            (Float { value: a, .. }, Float { value: b, .. }) => Ok(a == b),
            _ => Err(self.mismatch(expr, left, right)),
        }
    }

    fn ordering(
        &self,
        expr: &AnnotationExpr,
        left: &DefinitionValue,
        right: &DefinitionValue,
    ) -> Result<std::cmp::Ordering, ConditionError> {
        use DefinitionValue::*;
        match (left, right) {
            (Int { value: a, .. }, Int { value: b, .. }) => Ok(a.cmp(b)),
            (Float { value: a, .. }, Float { value: b, .. }) => a.partial_cmp(b).ok_or_else(|| {
                ConditionError::new(
                    expr,
                    ConditionErrorKind::NotOrderable {
                        found: left.kind_name().to_string(),
                    },
                )
            }),
            (Int { .. } | Float { .. }, Int { .. } | Float { .. }) => {
                Err(self.mismatch(expr, left, right))
            }
            _ => {
                let unordered = if left.numeric_kind().is_none() {
                    left
                } else {
                    right
                };
                Err(ConditionError::new(
                    expr,
                    ConditionErrorKind::NotOrderable {
                        found: unordered.kind_name().to_string(),
                    },
                ))
            }
        }
    }

    fn mismatch(
        &self,
        expr: &AnnotationExpr,
        left: &DefinitionValue,
        right: &DefinitionValue,
    ) -> ConditionError {
        ConditionError::new(
            expr,
            ConditionErrorKind::MismatchedKinds {
                left: left.kind_name().to_string(),
                right: right.kind_name().to_string(),
            },
        )
    }
}

#[cfg(test)]
mod tests;
