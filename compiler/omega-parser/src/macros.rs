//! Compile-time macro expansion: a pure `SourceModule -> SourceModule`
//! syntax transform. This is the *only* place `Item::
//! MacroDefinition`/`MacroInvocation` and `Expression::MacroInvocation`
//! exist -- by the time [`expand`] returns successfully, none of them
//! remain anywhere in the tree (see `omega_hir::lower::Lowerer`'s
//! `unreachable!()` arms for those variants), so nothing downstream of
//! `omega-parser` (HIR lowering, analysis, codegen) needs any notion of
//! macros at all.
//!
//! A macro's body is captured as a [`MacroBodyPiece`] tree at parse time
//! (see `parser::macro_syntax`) -- ordinary tokens plus, where the author
//! wrote `$...(sep){ ... }`, a repetition -- substituted at each invocation,
//! and fed directly into the ordinary parser's token-based entry points, one
//! per invocation position (`parser::item::parse_source_module`,
//! `parser::expression::parse_block_contents`,
//! `parser::expression::parse_expression`)
//! -- no render-to-text-then-re-lex round-trip. Every individual token keeps
//! whichever real span it was originally lexed with (from the macro
//! definition's body, or from the invocation's arguments) -- composite spans
//! built while re-parsing a spliced token stream are always well-formed
//! (`start <= end`) because `Span::to` is `min`/`max` construction rather
//! than first-token/last-token linearity (see `Span`'s own doc comment); a
//! node built from tokens mixing both origins may not describe one single
//! contiguous file range, but it can never be inverted.
//!
//! A macro's body is never type-checked or even syntax-checked on its own,
//! only once fully substituted with concrete arguments at a specific
//! invocation, matching "duck typed" expansion: whatever the substituted
//! code does or doesn't support is discovered the same way it would be for
//! hand-written code.

use crate::ast::statement::walrus::WalrusStmt;
use crate::diagnostics::ParseError;
use crate::lexer::{Token, TokenKind};
use crate::parser::Parser;
use crate::prelude::*;
use std::collections::HashMap;
use std::fmt;

/// Caps the total number of macro expansions performed while processing one
/// module, so a runaway recursive macro (`macro a() => { a$() }`)
/// produces a clean [`MacroError::ExpansionLimitExceeded`] instead of a
/// stack overflow.
const MAX_EXPANSIONS: u32 = 256;

#[derive(Debug)]
pub enum MacroError {
    DuplicateMacroDefinition {
        name: Ident,
    },
    UnknownMetavariable {
        macro_name: Ident,
        metavar: Ident,
    },
    UnknownMacro {
        name: Ident,
    },
    ArgCountMismatch {
        macro_name: Ident,
        expected: Arity,
        found: usize,
    },
    FragmentMismatch {
        macro_name: Ident,
        param: Ident,
        expected: FragmentKind,
        errors: String,
    },
    VariadicOutsideRepetition {
        macro_name: Ident,
        metavar: Ident,
    },
    RepetitionWithoutVariadic {
        macro_name: Ident,
    },
    RepetitionMissingVariadic {
        macro_name: Ident,
    },
    ExpansionParseError {
        macro_name: Ident,
        position: MacroPosition,
        errors: String,
    },
    /// Reached when an item-position expansion itself produces a definition:
    /// item reparsing permits `macro`, but definitions are only collected from
    /// the source module before expansion.
    MacroDefinitionInExpansion {
        macro_name: Ident,
    },
    ExpansionLimitExceeded {
        macro_name: Ident,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum Arity {
    Exact(usize),
    AtLeast(usize),
}

impl fmt::Display for Arity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(n) => write!(f, "{n}"),
            Self::AtLeast(n) => write!(f, "at least {n}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MacroPosition {
    Item,
    Statement,
    Expression,
}

impl fmt::Display for MacroPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Item => "item",
                Self::Statement => "statement",
                Self::Expression => "expression",
            }
        )
    }
}

impl fmt::Display for MacroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMacroDefinition { name } => {
                write!(f, "macro '{}' is defined more than once", name.0)
            }
            Self::UnknownMetavariable {
                macro_name,
                metavar,
            } => write!(
                f,
                "macro '{}' references unknown metavariable '${}' (not one of its own parameters)",
                macro_name.0, metavar.0
            ),
            Self::UnknownMacro { name } => {
                write!(f, "no macro named '{}' is defined in this module", name.0)
            }
            Self::ArgCountMismatch {
                macro_name,
                expected,
                found,
            } => write!(
                f,
                "macro '{}' expects {expected} argument(s), found {found}",
                macro_name.0
            ),
            Self::FragmentMismatch {
                macro_name,
                param,
                expected,
                errors,
            } => write!(
                f,
                "macro '{}': argument for '${}' does not parse as {expected:?}: {errors}",
                macro_name.0, param.0
            ),
            Self::VariadicOutsideRepetition {
                macro_name,
                metavar,
            } => write!(
                f,
                "macro '{}' references variadic metavariable '${}' outside a repetition",
                macro_name.0, metavar.0
            ),
            Self::RepetitionWithoutVariadic { macro_name } => write!(
                f,
                "macro '{}' contains a repetition but has no variadic parameter",
                macro_name.0
            ),
            Self::RepetitionMissingVariadic { macro_name } => write!(
                f,
                "macro '{}' contains a repetition whose body does not reference its variadic parameter",
                macro_name.0
            ),
            Self::ExpansionParseError {
                macro_name,
                position,
                errors,
            } => {
                write!(
                    f,
                    "macro '{}' does not expand to a valid {position} here: {errors}",
                    macro_name.0
                )
            }
            Self::MacroDefinitionInExpansion { macro_name } => write!(
                f,
                "macro expansion produced definition '{}' -- macro definitions must appear in source",
                macro_name.0
            ),
            Self::ExpansionLimitExceeded { macro_name } => write!(
                f,
                "macro expansion did not terminate (exceeded {MAX_EXPANSIONS} expansions) while \
                 expanding '{}' -- check for runaway recursive macro calls",
                macro_name.0
            ),
        }
    }
}

/// Expands every macro definition and invocation in `module`, returning a
/// module that contains only the five ordinary [`Item`] variants
/// that existed before macros were added.
pub fn expand(
    module: SourceModule,
    imported: &HashMap<Ident, MacroDefinitionStmt>,
) -> Result<SourceModule, MacroError> {
    let (own, items) = collect_definitions(module.nodes)?;
    let mut defs = imported.clone();
    defs.extend(own);
    for def in defs.values() {
        validate_definition(def)?;
    }
    let mut budget = MAX_EXPANSIONS;
    let nodes = expand_item_list(items, &defs, &mut budget)?;
    Ok(SourceModule { nodes })
}

/// Splits `nodes` into macro definitions (by name, rejecting a duplicate
/// name outright) and everything else, in original order.
fn collect_definitions(
    nodes: Vec<ItemNode>,
) -> Result<(HashMap<Ident, MacroDefinitionStmt>, Vec<ItemNode>), MacroError> {
    let mut defs = HashMap::new();
    let mut items = Vec::new();
    for node in nodes {
        match node.item {
            Item::MacroDefinition(def) => {
                if defs.contains_key(&def.name) {
                    return Err(MacroError::DuplicateMacroDefinition { name: def.name });
                }
                defs.insert(def.name.clone(), def);
            }
            other => items.push(ItemNode {
                item: other,
                span: node.span,
            }),
        }
    }
    Ok((defs, items))
}

/// Definition-time checks, all of them real definition bugs (a typo, most
/// likely) rather than something duck typing should hide, so all are made
/// once up front rather than only surfacing confusingly if/when some
/// invocation happens to reach them:
///
/// - every `$name` names one of that macro's own parameters;
/// - the variadic parameter is only referenced *inside* a repetition (it
///   has no single value anywhere else);
/// - a repetition only appears in a macro that declares a variadic;
/// - a repetition's body actually mentions the variadic -- otherwise it
///   would emit N identical copies, which is always a bug.
///
/// This recurses only into [`MacroBodyPiece::Repetition`]: a bracketed group
/// is *not* nesting here, since the lexer's token stream is flat (`(`/`)`/
/// etc. are ordinary tokens like any other), so repetition is the only
/// construct a `$name` reference can be nested inside.
fn validate_definition(def: &MacroDefinitionStmt) -> Result<(), MacroError> {
    fn walk(
        def: &MacroDefinitionStmt,
        body: &[MacroBodyPiece],
        in_repetition: bool,
    ) -> Result<bool, MacroError> {
        let variadic = def.signature.variadic.as_ref().map(|p| &p.name);
        let mut mentions_variadic = false;
        for piece in body {
            match piece {
                MacroBodyPiece::Token(Token {
                    kind: TokenKind::Metavar(name),
                    ..
                }) => {
                    let ident = Ident(name.clone());
                    let known = def.signature.fixed.iter().any(|p| p.name == ident)
                        || variadic.is_some_and(|v| *v == ident);
                    if !known {
                        return Err(MacroError::UnknownMetavariable {
                            macro_name: def.name.clone(),
                            metavar: ident,
                        });
                    }
                    if variadic.is_some_and(|v| *v == ident) {
                        if !in_repetition {
                            return Err(MacroError::VariadicOutsideRepetition {
                                macro_name: def.name.clone(),
                                metavar: ident,
                            });
                        }
                        mentions_variadic = true;
                    }
                }
                MacroBodyPiece::Token(_) => {}
                MacroBodyPiece::Repetition(rep) => {
                    if variadic.is_none() {
                        return Err(MacroError::RepetitionWithoutVariadic {
                            macro_name: def.name.clone(),
                        });
                    }
                    if !walk(def, &rep.body, true)? {
                        return Err(MacroError::RepetitionMissingVariadic {
                            macro_name: def.name.clone(),
                        });
                    }
                }
            }
        }
        Ok(mentions_variadic)
    }
    walk(def, &def.body, false).map(|_| ())
}

/// Walks a list of top-level items, splicing each macro invocation's
/// expansion in place and recursing into every function/struct body for
/// expression-position invocations nested inside expressions.
fn expand_item_list(
    nodes: Vec<ItemNode>,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<Vec<ItemNode>, MacroError> {
    let mut result = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node.item {
            Item::MacroInvocation(inv) => {
                result.extend(expand_items_invocation(&inv, defs, budget)?);
            }
            Item::FunctionDefinition(f) => result.push(ItemNode {
                item: Item::FunctionDefinition(expand_function_def(f, defs, budget)?),
                span: node.span,
            }),
            Item::Struct(s) => result.push(ItemNode {
                item: Item::Struct(expand_struct_def(s, defs, budget)?),
                span: node.span,
            }),
            Item::Enum(e) => result.push(ItemNode {
                item: Item::Enum(expand_enum_def(e, defs, budget)?),
                span: node.span,
            }),
            Item::Union(u) => result.push(ItemNode {
                item: Item::Union(expand_union_def(u, defs, budget)?),
                span: node.span,
            }),
            Item::Spec(sp) => result.push(ItemNode {
                item: Item::Spec(expand_spec_def(sp, defs, budget)?),
                span: node.span,
            }),
            Item::Gap(gap) => result.push(ItemNode {
                item: Item::Gap(gap),
                span: node.span,
            }),
            Item::Glue(mut glue) => {
                glue.functions = glue
                    .functions
                    .into_iter()
                    .map(|f| expand_function_def(f, defs, budget))
                    .collect::<Result<_, _>>()?;
                result.push(ItemNode {
                    item: Item::Glue(glue),
                    span: node.span,
                });
            }
            Item::Compose(mut compose) => {
                compose.functions = compose
                    .functions
                    .into_iter()
                    .map(|f| expand_function_def(f, defs, budget))
                    .collect::<Result<_, _>>()?;
                result.push(ItemNode {
                    item: Item::Compose(compose),
                    span: node.span,
                });
            }
            Item::Primitive(mut primitive) => {
                primitive.functions = primitive
                    .functions
                    .into_iter()
                    .map(|f| expand_function_def(f, defs, budget))
                    .collect::<Result<_, _>>()?;
                result.push(ItemNode {
                    item: Item::Primitive(primitive),
                    span: node.span,
                });
            }
            other @ (Item::Declaration(_) | Item::ExternDeclaration(_) | Item::Import(_)) => {
                result.push(ItemNode {
                    item: other,
                    span: node.span,
                });
            }
            Item::Walrus(w) => result.push(ItemNode {
                item: Item::Walrus(WalrusStmt {
                    value: expand_expr(w.value, defs, budget)?,
                    ..w
                }),
                span: node.span,
            }),
            Item::DeclarationWithInit(decl, value) => result.push(ItemNode {
                item: Item::DeclarationWithInit(decl, expand_expr(value, defs, budget)?),
                span: node.span,
            }),
            Item::MacroDefinition(def) => {
                return Err(MacroError::MacroDefinitionInExpansion {
                    macro_name: def.name,
                });
            }
        }
    }
    Ok(result)
}

/// Expands one item-position invocation into its (recursively expanded)
/// replacement items -- recursing through `expand_item_list` again so an
/// invocation nested inside the expansion (either written directly in the
/// macro's body, or introduced via a substituted argument) is itself
/// expanded, with no separate token-level nested-invocation handling needed.
fn expand_items_invocation(
    inv: &MacroInvocationExpr,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<Vec<ItemNode>, MacroError> {
    let def = defs
        .get(&inv.name)
        .ok_or_else(|| MacroError::UnknownMacro {
            name: inv.name.clone(),
        })?;
    let tokens = substitute_invocation(def, &inv.args, budget)?;
    let padded = with_eof(&tokens);
    let mut p = Parser::new(&padded);
    let nodes = crate::parser::item::parse_source_module(&mut p);
    let errors = p.into_errors();
    if !errors.is_empty() {
        return Err(MacroError::ExpansionParseError {
            macro_name: inv.name.clone(),
            position: MacroPosition::Item,
            errors: join_errors(&errors),
        });
    }
    expand_item_list(nodes, defs, budget)
}

/// Expands one expression-position invocation, recursing into the (possibly
/// invocation-containing) result the same way `expand_items_invocation`
/// does. The returned node's *own* span is the freshly parsed expression's;
/// the caller (`expand_expr`) is the one that pins the invocation's
/// original (real, call-site) span onto the outer wrapping node -- kept
/// deliberately, even though every token now carries a real span: a
/// min/max composite of tokens mixing the invocation site and the macro's
/// (possibly much earlier or later in the file) definition site would be a
/// well-formed but not especially meaningful span for a top-level
/// diagnostic to point at, whereas the call site always is.
fn expand_expr_invocation(
    inv: &MacroInvocationExpr,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<ExpressionNode, MacroError> {
    let def = defs
        .get(&inv.name)
        .ok_or_else(|| MacroError::UnknownMacro {
            name: inv.name.clone(),
        })?;
    let tokens = substitute_invocation(def, &inv.args, budget)?;
    let padded = with_eof(&tokens);
    let mut p = Parser::new(&padded);
    let parsed = crate::parser::expression::parse_expression(&mut p);
    let fully_consumed = p.is_eof();
    let errors = p.into_errors();
    let node = match parsed {
        Some(node) if fully_consumed && errors.is_empty() => node,
        _ => {
            let message = if errors.is_empty() {
                "unexpected trailing tokens".to_string()
            } else {
                join_errors(&errors)
            };
            return Err(MacroError::ExpansionParseError {
                macro_name: inv.name.clone(),
                position: MacroPosition::Expression,
                errors: message,
            });
        }
    };
    expand_expr(node, defs, budget)
}

/// Expands a whole-statement invocation through the ordinary block-content
/// grammar. A tail expression becomes an expression statement before being
/// spliced into its surrounding block.
fn expand_statements_invocation(
    inv: &MacroInvocationExpr,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<Vec<StatementNode>, MacroError> {
    let def = defs
        .get(&inv.name)
        .ok_or_else(|| MacroError::UnknownMacro {
            name: inv.name.clone(),
        })?;
    let tokens = substitute_invocation(def, &inv.args, budget)?;
    let padded = with_eof(&tokens);
    let mut p = Parser::new(&padded);
    let parsed = p.allow_struct_literals(crate::parser::expression::parse_block_contents);
    let fully_consumed = p.is_eof();
    let errors = p.into_errors();
    let mut cb = match parsed {
        Some(cb) if fully_consumed && errors.is_empty() => cb,
        _ => {
            let message = if errors.is_empty() {
                "unexpected trailing tokens".to_string()
            } else {
                join_errors(&errors)
            };
            return Err(MacroError::ExpansionParseError {
                macro_name: inv.name.clone(),
                position: MacroPosition::Statement,
                errors: message,
            });
        }
    };
    if let Some(tail) = cb.tail.take() {
        cb.statements.push(StatementNode {
            span: tail.span,
            statement: Statement::Expression(*tail),
        });
    }
    expand_statement_list(cb.statements, defs, budget)
}

/// Validates argument count and each argument's shape against its
/// parameter's declared [`FragmentKind`], then substitutes every `$name` in
/// `def`'s body with the corresponding argument's tokens. Also where the
/// expansion budget (see [`MAX_EXPANSIONS`]) is spent -- one unit per
/// invocation, regardless of its position.
fn substitute_invocation(
    def: &MacroDefinitionStmt,
    args: &[Vec<Token>],
    budget: &mut u32,
) -> Result<Vec<Token>, MacroError> {
    let fixed_len = def.signature.fixed.len();
    let expected = if def.signature.variadic.is_some() {
        Arity::AtLeast(fixed_len)
    } else {
        Arity::Exact(fixed_len)
    };
    if (def.signature.variadic.is_some() && args.len() < fixed_len)
        || (def.signature.variadic.is_none() && args.len() != fixed_len)
    {
        return Err(MacroError::ArgCountMismatch {
            macro_name: def.name.clone(),
            expected,
            found: args.len(),
        });
    }
    if *budget == 0 {
        return Err(MacroError::ExpansionLimitExceeded {
            macro_name: def.name.clone(),
        });
    }
    *budget -= 1;

    let mut bindings = Bindings::default();
    for (param, arg) in def.signature.fixed.iter().zip(args.iter()) {
        validate_fragment(def, param, arg)?;
        bindings.0.insert(param.name.clone(), Binding::One(arg));
    }
    if let Some(param) = &def.signature.variadic {
        for arg in &args[fixed_len..] {
            validate_fragment(def, param, arg)?;
        }
        bindings
            .0
            .insert(param.name.clone(), Binding::Many(&args[fixed_len..]));
    }
    let mut out = Vec::new();
    render(
        &def.body,
        &bindings,
        def.signature.variadic.as_ref().map(|p| &p.name),
        &mut out,
    );
    Ok(out)
}

/// Parses `arg` against `param`'s declared fragment grammar -- this is what
/// gives a fragment specifier real meaning (it constrains what can legally
/// be captured there) rather than being documentation only, and reports a
/// mismatch at the invocation site instead of letting it surface
/// confusingly deep inside expanded code.
fn validate_fragment(
    def: &MacroDefinitionStmt,
    param: &MacroParam,
    arg: &[Token],
) -> Result<(), MacroError> {
    let padded = with_eof(arg);
    let mut p = Parser::new(&padded);
    let result = match param.kind {
        FragmentKind::Expr => crate::parser::expression::parse_expression(&mut p).map(|_| ()),
        FragmentKind::Type => crate::parser::r#type::parse_type(&mut p).map(|_| ()),
        FragmentKind::Ident => p.expect_ident().map(|_| ()),
    };
    let fully_consumed = p.is_eof();
    let errors = p.into_errors();
    if result.is_some() && fully_consumed && errors.is_empty() {
        return Ok(());
    }
    let message = if errors.is_empty() {
        "unexpected trailing tokens".to_string()
    } else {
        join_errors(&errors)
    };
    Err(MacroError::FragmentMismatch {
        macro_name: def.name.clone(),
        param: param.name.clone(),
        expected: param.kind,
        errors: message,
    })
}

#[derive(Clone, Copy)]
enum Binding<'a> {
    One(&'a [Token]),
    Many(&'a [Vec<Token>]),
}

#[derive(Default, Clone)]
struct Bindings<'a>(HashMap<Ident, Binding<'a>>);

impl<'a> Bindings<'a> {
    fn with_element(&self, name: &Ident, tokens: &'a [Token]) -> Self {
        let mut next = self.clone();
        next.0.insert(name.clone(), Binding::One(tokens));
        next
    }
}

fn render(
    body: &[MacroBodyPiece],
    bindings: &Bindings<'_>,
    variadic: Option<&Ident>,
    out: &mut Vec<Token>,
) {
    for piece in body {
        match piece {
            MacroBodyPiece::Token(token) => match &token.kind {
                TokenKind::Metavar(name) => match bindings
                    .0
                    .get(&Ident(name.clone()))
                    .expect("definition was validated")
                {
                    Binding::One(tokens) => out.extend(tokens.iter().cloned()),
                    Binding::Many(_) => unreachable!("variadics outside repetition are rejected"),
                },
                _ => out.push(token.clone()),
            },
            MacroBodyPiece::Repetition(rep) => {
                let name = variadic.expect("repetitions without a variadic parameter are rejected");
                let Binding::Many(elements) =
                    bindings.0.get(name).expect("variadic parameter is bound")
                else {
                    unreachable!()
                };
                for (index, element) in elements.iter().enumerate() {
                    if index != 0 {
                        if let Some(separator) = &rep.separator {
                            out.push(separator.clone());
                        }
                    }
                    let one = bindings.with_element(name, element);
                    render(&rep.body, &one, variadic, out);
                }
            }
        }
    }
}

/// The parser's entry points expect a token slice ending in `Eof` (see
/// `Parser::new`'s doc comment) -- a spliced/substituted token slice has no
/// such sentinel of its own, so one is synthesized here. Its span is
/// otherwise meaningless (these tokens don't span one contiguous file
/// range to begin with -- see this module's top doc comment), so it just
/// reuses the last real token's span, a reasonable place for a "found end
/// of input" error to point at.
fn with_eof(tokens: &[Token]) -> Vec<Token> {
    let eof_span = tokens.last().map(|t| t.span).unwrap_or_default();
    let mut out = tokens.to_vec();
    out.push(Token {
        kind: TokenKind::Eof,
        span: eof_span,
    });
    out
}

fn join_errors(errors: &[ParseError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn expand_function_def(
    f: FunctionDefinitionStmt,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<FunctionDefinitionStmt, MacroError> {
    Ok(FunctionDefinitionStmt {
        codeblock: expand_codeblock(f.codeblock, defs, budget)?,
        ..f
    })
}

fn expand_struct_def(
    s: StructStmt,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<StructStmt, MacroError> {
    let functions = s
        .functions
        .into_iter()
        .map(|f| expand_function_def(f, defs, budget))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StructStmt { functions, ..s })
}

fn expand_union_def(
    u: UnionStmt,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<UnionStmt, MacroError> {
    let functions = u
        .functions
        .into_iter()
        .map(|f| expand_function_def(f, defs, budget))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UnionStmt { functions, ..u })
}

fn expand_enum_def(
    e: EnumStmt,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<EnumStmt, MacroError> {
    let variants = e
        .variants
        .into_iter()
        .map(|v| {
            let args = v
                .args
                .into_iter()
                .map(|a| expand_expr(a, defs, budget))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(EnumVariantStmt { args, ..v })
        })
        .collect::<Result<Vec<_>, MacroError>>()?;
    let functions = e
        .functions
        .into_iter()
        .map(|f| expand_function_def(f, defs, budget))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EnumStmt {
        variants,
        functions,
        ..e
    })
}

fn expand_spec_def(
    sp: SpecStmt,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<SpecStmt, MacroError> {
    let functions = sp
        .functions
        .into_iter()
        .map(|f| {
            let body = f
                .body
                .map(|b| expand_codeblock(b, defs, budget))
                .transpose()?;
            Ok(SpecFunctionStmt { body, ..f })
        })
        .collect::<Result<Vec<_>, MacroError>>()?;
    Ok(SpecStmt { functions, ..sp })
}

fn expand_codeblock(
    cb: CodeblockExpr,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<CodeblockExpr, MacroError> {
    let statements = expand_statement_list(cb.statements, defs, budget)?;
    let tail = cb
        .tail
        .map(|t| expand_expr(*t, defs, budget).map(Box::new))
        .transpose()?;
    Ok(CodeblockExpr { statements, tail })
}

fn expand_statement_list(
    statements: Vec<StatementNode>,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<Vec<StatementNode>, MacroError> {
    let mut result = Vec::with_capacity(statements.len());
    for node in statements {
        match node.statement {
            Statement::MacroInvocation(inv) => {
                result.extend(expand_statements_invocation(&inv, defs, budget)?)
            }
            statement => result.push(expand_stmt_node(
                StatementNode {
                    statement,
                    span: node.span,
                },
                defs,
                budget,
            )?),
        }
    }
    Ok(result)
}

fn expand_if(
    if_expr: IfExpr,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<IfExpr, MacroError> {
    let branches = if_expr
        .branches
        .into_iter()
        .map(|(cond, block)| {
            Ok((
                expand_expr(cond, defs, budget)?,
                expand_codeblock(block, defs, budget)?,
            ))
        })
        .collect::<Result<Vec<_>, MacroError>>()?;
    let else_branch = if_expr
        .else_branch
        .map(|b| expand_codeblock(b, defs, budget))
        .transpose()?;
    Ok(IfExpr {
        branches,
        else_branch,
    })
}

fn expand_stmt_node(
    node: StatementNode,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<StatementNode, MacroError> {
    let span = node.span;
    let statement = expand_statement(node.statement, defs, budget)?;
    Ok(StatementNode { statement, span })
}

fn expand_statement(
    statement: Statement,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<Statement, MacroError> {
    Ok(match statement {
        Statement::MacroInvocation(_) => {
            unreachable!("statement invocations are spliced by expand_statement_list")
        }
        Statement::Declaration(decl) => Statement::Declaration(decl),
        Statement::DeclarationWithInit(decl, value) => {
            Statement::DeclarationWithInit(decl, expand_expr(value, defs, budget)?)
        }
        Statement::ExternDeclaration(decl) => Statement::ExternDeclaration(decl),
        Statement::Expression(expr) => Statement::Expression(expand_expr(expr, defs, budget)?),
        Statement::Return(ret) => Statement::Return(ReturnStmt {
            return_value: expand_expr(ret.return_value, defs, budget)?,
        }),
        Statement::Break => Statement::Break,
        Statement::Continue => Statement::Continue,
        Statement::Walrus(w) => Statement::Walrus(WalrusStmt {
            value: expand_expr(w.value, defs, budget)?,
            ..w
        }),
        Statement::While(w) => Statement::While(WhileStmt {
            condition: expand_expr(w.condition, defs, budget)?,
            body: expand_codeblock(w.body, defs, budget)?,
        }),
        Statement::Loop(l) => Statement::Loop(LoopStmt {
            body: expand_codeblock(l.body, defs, budget)?,
        }),
        Statement::For(f) => {
            let f = *f;
            Statement::For(Box::new(ForStmt {
                init: f
                    .init
                    .map(|s| expand_statement(s, defs, budget))
                    .transpose()?,
                condition: f
                    .condition
                    .map(|c| expand_expr(c, defs, budget))
                    .transpose()?,
                post: f.post.map(|p| expand_expr(p, defs, budget)).transpose()?,
                body: expand_codeblock(f.body, defs, budget)?,
            }))
        }
        Statement::ForIn(f) => {
            let f = *f;
            Statement::ForIn(Box::new(ForInStmt {
                iterator: expand_expr(f.iterator, defs, budget)?,
                body: expand_codeblock(f.body, defs, budget)?,
                ..f
            }))
        }
        Statement::Defer(d) => Statement::Defer(DeferStmt {
            body: Box::new(expand_statement(*d.body, defs, budget)?),
        }),
    })
}

/// Recursively expands every `Expression::MacroInvocation` found anywhere in
/// `node`'s subtree. The `MacroInvocation` arm returns early rather than
/// falling through to the generic rewrap at the bottom, specifically so the
/// *outer* node keeps the invocation's own original (real, call-site) span
/// while the expansion's own internal spans (also real now, but possibly
/// from the macro's definition site) are left as they were parsed.
fn expand_expr(
    node: ExpressionNode,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<ExpressionNode, MacroError> {
    let span = node.span;
    if let Expression::MacroInvocation(inv) = node.expression {
        let expanded = expand_expr_invocation(&inv, defs, budget)?;
        return Ok(ExpressionNode {
            expression: expanded.expression,
            span,
        });
    }

    let expression = match node.expression {
        Expression::MacroInvocation(_) => unreachable!("handled above"),
        Expression::Path(p) => Expression::Path(p),
        Expression::FieldAccess(access) => {
            let access = *access;
            Expression::FieldAccess(Box::new(FieldAccessExpr {
                base: expand_expr(access.base, defs, budget)?,
                field: access.field,
            }))
        }
        Expression::Index(index) => {
            let index = *index;
            Expression::Index(Box::new(IndexExpr {
                base: expand_expr(index.base, defs, budget)?,
                index: expand_expr(index.index, defs, budget)?,
            }))
        }
        Expression::Deref(deref) => {
            let deref = *deref;
            Expression::Deref(Box::new(DerefExpr {
                base: expand_expr(deref.base, defs, budget)?,
            }))
        }
        Expression::AddressOf(addr) => {
            let addr = *addr;
            Expression::AddressOf(Box::new(AddressOfExpr {
                base: expand_expr(addr.base, defs, budget)?,
                mutable: addr.mutable,
            }))
        }
        Expression::Negate(neg) => {
            let neg = *neg;
            Expression::Negate(Box::new(NegateExpr {
                base: expand_expr(neg.base, defs, budget)?,
            }))
        }
        Expression::BitNot(not) => {
            let not = *not;
            Expression::BitNot(Box::new(BitNotExpr {
                base: expand_expr(not.base, defs, budget)?,
            }))
        }
        Expression::Reveal(reveal) => {
            let reveal = *reveal;
            Expression::Reveal(Box::new(RevealExpr {
                base: expand_expr(reveal.base, defs, budget)?,
            }))
        }
        Expression::Comp(comp) => {
            let comp = *comp;
            Expression::Comp(Box::new(CompExpr {
                base: expand_expr(comp.base, defs, budget)?,
            }))
        }
        Expression::Cast(cast) => {
            let cast = *cast;
            Expression::Cast(Box::new(CastExpr {
                target: cast.target,
                base: expand_expr(cast.base, defs, budget)?,
            }))
        }
        // No `base` expression to recurse into, and a bare `Type` can never
        // contain a macro metavariable (`$name` is only meaningful in
        // expression position -- see `lexer::TokenKind::Metavar`'s doc
        // comment) -- a plain passthrough, like `Expression::Path` above.
        Expression::Sizeof(sizeof) => Expression::Sizeof(sizeof),
        Expression::Increment(incr) => {
            let incr = *incr;
            Expression::Increment(Box::new(IncrementExpr {
                base: expand_expr(incr.base, defs, budget)?,
            }))
        }
        Expression::Decrement(decr) => {
            let decr = *decr;
            Expression::Decrement(Box::new(DecrementExpr {
                base: expand_expr(decr.base, defs, budget)?,
            }))
        }
        Expression::BinaryOp(bin) => {
            let bin = *bin;
            Expression::BinaryOp(Box::new(BinaryOpExpr {
                left: expand_expr(bin.left, defs, budget)?,
                op: bin.op,
                right: expand_expr(bin.right, defs, budget)?,
            }))
        }
        Expression::Number(n) => Expression::Number(n),
        Expression::String(s) => Expression::String(s),
        Expression::ByteString(s) => Expression::ByteString(s),
        Expression::Bool(b) => Expression::Bool(b),
        Expression::Char(c) => Expression::Char(c),
        Expression::Codeblock(cb) => Expression::Codeblock(expand_codeblock(cb, defs, budget)?),
        Expression::If(if_expr) => Expression::If(Box::new(expand_if(*if_expr, defs, budget)?)),
        Expression::FunctionCall(call) => Expression::FunctionCall(FunctionCallExpr {
            callee: Box::new(expand_expr(*call.callee, defs, budget)?),
            args: call
                .args
                .into_iter()
                .map(|a| expand_expr(a, defs, budget))
                .collect::<Result<Vec<_>, _>>()?,
        }),
        Expression::Assignment(assign) => {
            let assign = *assign;
            Expression::Assignment(Box::new(AssignmentExpr {
                target: expand_expr(assign.target, defs, budget)?,
                value: Box::new(expand_expr(*assign.value, defs, budget)?),
            }))
        }
        Expression::CompoundAssign(assign) => {
            let assign = *assign;
            Expression::CompoundAssign(Box::new(CompoundAssignExpr {
                target: expand_expr(assign.target, defs, budget)?,
                op: assign.op,
                value: Box::new(expand_expr(*assign.value, defs, budget)?),
            }))
        }
        Expression::ArrayLiteral(lit) => Expression::ArrayLiteral(ArrayLiteralExpr {
            elements: lit
                .elements
                .into_iter()
                .map(|e| expand_expr(e, defs, budget))
                .collect::<Result<Vec<_>, _>>()?,
        }),
        Expression::StructLiteral(lit) => Expression::StructLiteral(StructLiteralExpr {
            path: lit.path,
            fields: lit
                .fields
                .into_iter()
                .map(|f| {
                    Ok(StructLiteralField {
                        name: f.name,
                        name_span: f.name_span,
                        value: expand_expr(f.value, defs, budget)?,
                    })
                })
                .collect::<Result<Vec<_>, MacroError>>()?,
        }),
        Expression::Slice(s) => {
            let s = *s;
            Expression::Slice(Box::new(SliceExpr {
                base: expand_expr(s.base, defs, budget)?,
                range: expand_range(s.range, defs, budget)?,
            }))
        }
        Expression::Match(m) => Expression::Match(Box::new(expand_match(*m, defs, budget)?)),
        Expression::Range(r) => Expression::Range(Box::new(expand_range(*r, defs, budget)?)),
    };
    Ok(ExpressionNode { expression, span })
}

fn expand_range(
    range: RangeExpr,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<RangeExpr, MacroError> {
    let end = match range.end {
        RangeEnd::Inclusive(e) => RangeEnd::Inclusive(expand_expr(e, defs, budget)?),
        RangeEnd::Exclusive(e) => RangeEnd::Exclusive(expand_expr(e, defs, budget)?),
        RangeEnd::Open => RangeEnd::Open,
    };
    Ok(RangeExpr {
        start: range
            .start
            .map(|e| expand_expr(e, defs, budget))
            .transpose()?,
        end,
        span: range.span,
    })
}

fn expand_match(
    match_expr: MatchExpr,
    defs: &HashMap<Ident, MacroDefinitionStmt>,
    budget: &mut u32,
) -> Result<MatchExpr, MacroError> {
    let scrutinee = expand_expr(match_expr.scrutinee, defs, budget)?;
    let arms = match_expr
        .arms
        .into_iter()
        .map(|arm| {
            let pattern = match arm.pattern {
                Pattern::Value(v) => Pattern::Value(expand_expr(v, defs, budget)?),
                Pattern::Range(r) => Pattern::Range(expand_range(r, defs, budget)?),
            };
            Ok(MatchArm {
                pattern,
                body: expand_expr(arm.body, defs, budget)?,
                span: arm.span,
            })
        })
        .collect::<Result<Vec<_>, MacroError>>()?;
    let else_branch = match_expr
        .else_branch
        .map(|b| expand_codeblock(b, defs, budget))
        .transpose()?;
    Ok(MatchExpr {
        scrutinee,
        arms,
        else_branch,
        span: match_expr.span,
    })
}
