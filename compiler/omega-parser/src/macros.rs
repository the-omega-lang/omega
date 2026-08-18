//! Compile-time macro expansion: a pure `SourceModule -> SourceModule`
//! syntax transform. This is the only place `Item::
//! MacroDefinition`/`MacroInvocation` and `Expression::MacroInvocation`
//! exist -- nothing downstream of `omega-parser` needs any notion of macros.
//!
//! A macro's body is captured as a [`MacroBodyPiece`] tree at parse time and
//! substituted at each invocation, then fed directly into the ordinary
//! parser's token-based entry points -- no render-to-text-then-re-lex
//! round-trip. Generated tokens are re-anchored at the invocation's span
//! (call-site attribution) rather than keeping definition-site spans, since
//! a [`Span`] carries no source-file identity.
//!
//! A macro's body is never type- or syntax-checked on its own, only once
//! fully substituted at a specific invocation ("duck typed" expansion).

use crate::ast::identifier::{ExpansionId, Ident, Origin};
use crate::ast::range::{RangeEnd, RangeExpr};
use crate::ast::statement::WalrusStmt;
use crate::diagnostics::ParseError;
use crate::lexer::{Token, TokenKind};
use crate::parser::Parser;
use crate::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

/// Caps the total number of macro expansions performed while processing one
/// module, so a runaway recursive macro (`macro a() => { a$() }`)
/// produces a clean [`MacroError::ExpansionLimitExceeded`] instead of a
/// stack overflow.
const MAX_EXPANSIONS: u32 = 256;

/// Driver-owned provenance for macro expansions. It lives for one compilation
/// so expansion ids remain unique across every module expanded in that run.
#[derive(Debug, Default)]
pub struct ExpansionState {
    next_id: u32,
    origins: HashMap<ExpansionId, ExpansionOrigin>,
    environments: HashMap<Vec<Ident>, HashMap<Ident, MacroDefinitionStmt>>,
}

#[derive(Debug, Clone)]
struct ExpansionOrigin {
    defining_module: Vec<Ident>,
    macro_visibility: Visibility,
}

impl ExpansionState {
    pub fn defining_module(&self, origin: Origin) -> Option<&[Ident]> {
        origin.0.and_then(|id| {
            self.origins
                .get(&id)
                .map(|entry| entry.defining_module.as_slice())
        })
    }

    pub fn macro_visibility(&self, origin: Origin) -> Option<Visibility> {
        origin
            .0
            .and_then(|id| self.origins.get(&id).map(|entry| entry.macro_visibility))
    }

    fn fresh_origin(&mut self, def: &MacroDefinitionStmt) -> Origin {
        let id = ExpansionId(self.next_id);
        self.next_id += 1;
        self.origins.insert(
            id,
            ExpansionOrigin {
                defining_module: def.defining_module.clone(),
                macro_visibility: def.visibility,
            },
        );
        Origin(Some(id))
    }

    pub fn register_environment(
        &mut self,
        module: &[Ident],
        definitions: &HashMap<Ident, MacroDefinitionStmt>,
    ) {
        self.environments
            .insert(module.to_vec(), definitions.clone());
    }

    fn definition_environment(
        &self,
        module: &[Ident],
    ) -> Option<HashMap<Ident, MacroDefinitionStmt>> {
        self.environments.get(module).cloned()
    }

    /// The macro environment an invocation resolves in, chosen by where its
    /// name token was *written*: a body-emitted invocation resolves in the
    /// emitting macro's defining module, one that arrived through argument
    /// substitution keeps `ambient`. Selecting per invocation (rather than
    /// swapping the environment for a whole expanded subtree) keeps an
    /// invocation passed *as an argument* resolvable in the caller's module.
    fn environment_for<'a>(
        &'a self,
        origin: Origin,
        ambient: &'a HashMap<Ident, MacroDefinitionStmt>,
    ) -> Cow<'a, HashMap<Ident, MacroDefinitionStmt>> {
        match self
            .defining_module(origin)
            .map(|module| module.to_vec())
            .and_then(|module| self.definition_environment(&module))
        {
            Some(env) => Cow::Owned(env),
            None => Cow::Borrowed(ambient),
        }
    }
}

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
    let mut state = ExpansionState::default();
    expand_with_origins(module, imported, &[], &mut state)
}

/// Expands one module while recording where every body-emitted token was
/// written. The ordinary [`expand`] wrapper is retained for parser-only tests.
pub fn expand_with_origins(
    module: SourceModule,
    imported: &HashMap<Ident, MacroDefinitionStmt>,
    module_path: &[Ident],
    state: &mut ExpansionState,
) -> Result<SourceModule, MacroError> {
    let (own, items) = collect_definitions(module.nodes, module_path)?;
    let mut defs = imported.clone();
    defs.extend(own);
    state.register_environment(module_path, &defs);
    for def in defs.values() {
        validate_definition(def)?;
    }
    let nodes = Expander {
        defs: &defs,
        budget: MAX_EXPANSIONS,
        state,
    }
    .expand_item_list(items)?;
    Ok(SourceModule { nodes })
}

/// Splits `nodes` into macro definitions (by name, rejecting a duplicate
/// name outright) and everything else, in original order.
fn collect_definitions(
    nodes: Vec<ItemNode>,
    module_path: &[Ident],
) -> Result<(HashMap<Ident, MacroDefinitionStmt>, Vec<ItemNode>), MacroError> {
    let mut defs = HashMap::new();
    let mut items = Vec::new();
    for node in nodes {
        match node.item {
            Item::MacroDefinition(mut def) => {
                if defs.contains_key(&def.name) {
                    return Err(MacroError::DuplicateMacroDefinition { name: def.name });
                }
                def.defining_module = module_path.to_vec();
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
    origin: Origin,
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
                _ => {
                    let mut token = token.clone();
                    token.origin = origin;
                    out.push(token);
                }
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
                            let mut separator = separator.clone();
                            separator.origin = origin;
                            out.push(separator);
                        }
                    }
                    let one = bindings.with_element(name, element);
                    render(&rep.body, &one, variadic, origin, out);
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
        origin: Origin::default(),
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

/// The state every expansion step needs, in one place instead of a
/// `(defs, budget, state)` triple threaded by hand through seventeen
/// functions -- several of whose signatures were longer than their bodies.
///
/// `defs` is fixed for a whole module's expansion: an invocation may resolve
/// in a *different* environment (see `ExpansionState::environment_for`), but
/// that choice is local to the invocation and never changes what the
/// surrounding tree expands against.
struct Expander<'a> {
    defs: &'a HashMap<Ident, MacroDefinitionStmt>,
    /// Remaining expansions for this module -- see `MAX_EXPANSIONS`.
    budget: u32,
    state: &'a mut ExpansionState,
}

impl Expander<'_> {
    /// Walks a list of top-level items, splicing each macro invocation's
    /// expansion in place and recursing into every function/struct body for
    /// expression-position invocations nested inside expressions.
    fn expand_item_list(&mut self, nodes: Vec<ItemNode>) -> Result<Vec<ItemNode>, MacroError> {
        let mut result = Vec::with_capacity(nodes.len());
        for node in nodes {
            match node.item {
                Item::MacroInvocation(inv) => {
                    result.extend(self.expand_items_invocation(&inv, node.span)?);
                }
                Item::FunctionDefinition(f) => result.push(ItemNode {
                    item: Item::FunctionDefinition(self.expand_function_def(f)?),
                    span: node.span,
                }),
                Item::Struct(s) => result.push(ItemNode {
                    item: Item::Struct(self.expand_struct_def(s)?),
                    span: node.span,
                }),
                Item::Enum(e) => result.push(ItemNode {
                    item: Item::Enum(self.expand_enum_def(e)?),
                    span: node.span,
                }),
                Item::Union(u) => result.push(ItemNode {
                    item: Item::Union(self.expand_union_def(u)?),
                    span: node.span,
                }),
                Item::Spec(sp) => result.push(ItemNode {
                    item: Item::Spec(self.expand_spec_def(sp)?),
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
                        .map(|f| self.expand_function_def(f))
                        .collect::<Result<_, _>>()?;
                    result.push(ItemNode {
                        item: Item::Glue(glue),
                        span: node.span,
                    });
                }
                Item::Conform(mut conform) => {
                    conform.functions = conform
                        .functions
                        .into_iter()
                        .map(|f| self.expand_function_def(f))
                        .collect::<Result<_, _>>()?;
                    result.push(ItemNode {
                        item: Item::Conform(conform),
                        span: node.span,
                    });
                }
                Item::Primitive(mut primitive) => {
                    primitive.functions = primitive
                        .functions
                        .into_iter()
                        .map(|f| self.expand_function_def(f))
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
                        value: self.expand_expr(w.value)?,
                        ..w
                    }),
                    span: node.span,
                }),
                Item::DeclarationWithInit(decl, value) => result.push(ItemNode {
                    item: Item::DeclarationWithInit(decl, self.expand_expr(value)?),
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
    fn expand_items_invocation(&mut self, inv: &MacroInvocationExpr, call_span: Span) -> Result<Vec<ItemNode>, MacroError> {
        // Owned so the environment borrow ends before `substitute_invocation`
        // takes `self` mutably.
        let def = self
            .state
            .environment_for(inv.origin, self.defs)
            .get(&inv.name)
            .cloned()
            .ok_or_else(|| MacroError::UnknownMacro {
                name: inv.name.clone(),
            })?;
        let tokens = self.substitute_invocation(&def, &inv.args, call_span)?;
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
        self.expand_item_list(nodes)
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
    fn expand_expr_invocation(&mut self, inv: &MacroInvocationExpr, call_span: Span) -> Result<ExpressionNode, MacroError> {
        // Owned so the environment borrow ends before `substitute_invocation`
        // takes `self` mutably.
        let def = self
            .state
            .environment_for(inv.origin, self.defs)
            .get(&inv.name)
            .cloned()
            .ok_or_else(|| MacroError::UnknownMacro {
                name: inv.name.clone(),
            })?;
        let tokens = self.substitute_invocation(&def, &inv.args, call_span)?;
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
        self.expand_expr(node)
    }

    /// Expands a whole-statement invocation through the ordinary block-content
    /// grammar. A tail expression becomes an expression statement before being
    /// spliced into its surrounding block.
    fn expand_statements_invocation(&mut self, inv: &MacroInvocationExpr, call_span: Span) -> Result<Vec<StatementNode>, MacroError> {
        // Owned so the environment borrow ends before `substitute_invocation`
        // takes `self` mutably.
        let def = self
            .state
            .environment_for(inv.origin, self.defs)
            .get(&inv.name)
            .cloned()
            .ok_or_else(|| MacroError::UnknownMacro {
                name: inv.name.clone(),
            })?;
        let tokens = self.substitute_invocation(&def, &inv.args, call_span)?;
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
        self.expand_statement_list(cb.statements)
    }

    /// Validates argument count and each argument's shape against its
    /// parameter's declared [`FragmentKind`], then substitutes every `$name` in
    /// `def`'s body with the corresponding argument's tokens. Also where the
    /// expansion budget (see [`MAX_EXPANSIONS`]) is spent -- one unit per
    /// invocation, regardless of its position.
    fn substitute_invocation(&mut self, def: &MacroDefinitionStmt, args: &[Vec<Token>], call_span: Span) -> Result<Vec<Token>, MacroError> {
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
        if self.budget == 0 {
            return Err(MacroError::ExpansionLimitExceeded {
                macro_name: def.name.clone(),
            });
        }
        self.budget -= 1;

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
        let origin = self.state.fresh_origin(def);
        render(
            &def.body,
            &bindings,
            def.signature.variadic.as_ref().map(|p| &p.name),
            origin,
            &mut out,
        );
        // `Span` has no file identity. Macro definitions can come from an
        // imported module, so retaining their token spans would later render
        // offsets from that module against the caller's source file. Attribute
        // all generated code to the invocation instead: precise enough to find
        // the expansion and always guaranteed to belong to the rendered file.
        Ok(out
            .into_iter()
            .map(|token| Token {
                kind: token.kind,
                span: call_span,
                origin: token.origin,
            })
            .collect())
    }

    fn expand_function_def(&mut self, f: FunctionDefinitionStmt) -> Result<FunctionDefinitionStmt, MacroError> {
        Ok(FunctionDefinitionStmt {
            codeblock: self.expand_codeblock(f.codeblock)?,
            ..f
        })
    }

    /// A member-function list, expanded in place.
    ///
    /// Shared by all four item kinds that have one. Like
    /// `parser::item::parse_member_functions`, this shares an *operation*,
    /// not an item pipeline: `struct`/`enum`/`union` still each build their
    /// own node and stay the three separate pipelines `docs/README.md`
    /// calls for.
    fn expand_member_functions(
        &mut self,
        functions: Vec<FunctionDefinitionStmt>,
    ) -> Result<Vec<FunctionDefinitionStmt>, MacroError> {
        functions
            .into_iter()
            .map(|f| self.expand_function_def(f))
            .collect()
    }

    fn expand_struct_def(&mut self, s: StructStmt) -> Result<StructStmt, MacroError> {
        let functions = self.expand_member_functions(s.functions)?;
        Ok(StructStmt { functions, ..s })
    }

    fn expand_union_def(&mut self, u: UnionStmt) -> Result<UnionStmt, MacroError> {
        let functions = self.expand_member_functions(u.functions)?;
        Ok(UnionStmt { functions, ..u })
    }

    fn expand_enum_def(&mut self, e: EnumStmt) -> Result<EnumStmt, MacroError> {
        let variants = e
            .variants
            .into_iter()
            .map(|v| {
                let args = v
                    .args
                    .into_iter()
                    .map(|a| self.expand_expr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(EnumVariantStmt { args, ..v })
            })
            .collect::<Result<Vec<_>, MacroError>>()?;
        let functions = self.expand_member_functions(e.functions)?;
        Ok(EnumStmt {
            variants,
            functions,
            ..e
        })
    }

    fn expand_spec_def(&mut self, sp: SpecStmt) -> Result<SpecStmt, MacroError> {
        let functions = sp
            .functions
            .into_iter()
            .map(|f| {
                let body = f
                    .body
                    .map(|b| self.expand_codeblock(b))
                    .transpose()?;
                Ok(SpecFunctionStmt { body, ..f })
            })
            .collect::<Result<Vec<_>, MacroError>>()?;
        Ok(SpecStmt { functions, ..sp })
    }

    fn expand_codeblock(&mut self, cb: CodeblockExpr) -> Result<CodeblockExpr, MacroError> {
        let statements = self.expand_statement_list(cb.statements)?;
        let tail = cb
            .tail
            .map(|t| self.expand_expr(*t).map(Box::new))
            .transpose()?;
        Ok(CodeblockExpr {
            statements,
            tail,
            span: cb.span,
        })
    }

    fn expand_statement_list(&mut self, statements: Vec<StatementNode>) -> Result<Vec<StatementNode>, MacroError> {
        let mut result = Vec::with_capacity(statements.len());
        for node in statements {
            match node.statement {
                Statement::MacroInvocation(inv) => result.extend(self.expand_statements_invocation(&inv, node.span)?),
                statement => result.push(self.expand_stmt_node(StatementNode {
                    statement,
                    span: node.span,
                })?),
            }
        }
        Ok(result)
    }

    fn expand_if(&mut self, if_expr: IfExpr) -> Result<IfExpr, MacroError> {
        let branches = if_expr
            .branches
            .into_iter()
            .map(|(cond, block)| {
                Ok((
                    self.expand_expr(cond)?,
                    self.expand_codeblock(block)?,
                ))
            })
            .collect::<Result<Vec<_>, MacroError>>()?;
        let else_branch = if_expr
            .else_branch
            .map(|b| self.expand_codeblock(b))
            .transpose()?;
        Ok(IfExpr {
            branches,
            else_branch,
        })
    }

    fn expand_stmt_node(&mut self, node: StatementNode) -> Result<StatementNode, MacroError> {
        let span = node.span;
        let statement = self.expand_statement(node.statement)?;
        Ok(StatementNode { statement, span })
    }

    fn expand_statement(&mut self, statement: Statement) -> Result<Statement, MacroError> {
        Ok(match statement {
            Statement::MacroInvocation(_) => {
                unreachable!("statement invocations are spliced by expand_statement_list")
            }
            Statement::Declaration(decl) => Statement::Declaration(decl),
            Statement::DeclarationWithInit(decl, value) => {
                Statement::DeclarationWithInit(decl, self.expand_expr(value)?)
            }
            Statement::ExternDeclaration(decl) => Statement::ExternDeclaration(decl),
            Statement::Expression(expr) => {
                Statement::Expression(self.expand_expr(expr)?)
            }
            Statement::Return(ret) => Statement::Return(ReturnStmt {
                return_value: self.expand_expr(ret.return_value)?,
            }),
            Statement::Break => Statement::Break,
            Statement::Continue => Statement::Continue,
            Statement::Walrus(w) => Statement::Walrus(WalrusStmt {
                value: self.expand_expr(w.value)?,
                ..w
            }),
            Statement::While(w) => Statement::While(WhileStmt {
                condition: self.expand_expr(w.condition)?,
                body: self.expand_codeblock(w.body)?,
            }),
            Statement::Loop(l) => Statement::Loop(LoopStmt {
                body: self.expand_codeblock(l.body)?,
            }),
            Statement::For(f) => {
                let f = *f;
                Statement::For(Box::new(ForStmt {
                    init: f
                        .init
                        .map(|s| self.expand_statement(s))
                        .transpose()?,
                    condition: f
                        .condition
                        .map(|c| self.expand_expr(c))
                        .transpose()?,
                    post: f
                        .post
                        .map(|p| self.expand_expr(p))
                        .transpose()?,
                    body: self.expand_codeblock(f.body)?,
                }))
            }
            Statement::ForIn(f) => {
                let f = *f;
                Statement::ForIn(Box::new(ForInStmt {
                    iterator: self.expand_expr(f.iterator)?,
                    body: self.expand_codeblock(f.body)?,
                    ..f
                }))
            }
            Statement::Defer(d) => Statement::Defer(DeferStmt {
                body: Box::new(self.expand_statement(*d.body)?),
            }),
        })
    }

    /// Recursively expands every `Expression::MacroInvocation` found anywhere in
    /// `node`'s subtree. The `MacroInvocation` arm returns early rather than
    /// falling through to the generic rewrap at the bottom, specifically so the
    /// *outer* node keeps the invocation's own original (real, call-site) span
    /// while the expansion's own internal spans (also real now, but possibly
    /// from the macro's definition site) are left as they were parsed.
    fn expand_expr(&mut self, node: ExpressionNode) -> Result<ExpressionNode, MacroError> {
        let span = node.span;
        if let Expression::MacroInvocation(inv) = node.expression {
            let expanded = self.expand_expr_invocation(&inv, span)?;
            return Ok(ExpressionNode {
                expression: expanded.expression,
                span,
            });
        }

        let expression = match node.expression {
            Expression::MacroInvocation(_) => unreachable!("handled above"),
            Expression::Path(p) => Expression::Path(p),
            Expression::FieldAccess(access) => Expression::FieldAccess(Box::new(FieldAccessExpr {
                base: self.expand_expr(access.base)?,
                field: access.field,
            })),
            Expression::Index(index) => Expression::Index(Box::new(IndexExpr {
                base: self.expand_expr(index.base)?,
                index: self.expand_expr(index.index)?,
            })),
            Expression::Deref(deref) => Expression::Deref(Box::new(DerefExpr {
                base: self.expand_expr(deref.base)?,
            })),
            Expression::AddressOf(addr) => Expression::AddressOf(Box::new(AddressOfExpr {
                base: self.expand_expr(addr.base)?,
                mutable: addr.mutable,
            })),
            Expression::Negate(neg) => Expression::Negate(Box::new(NegateExpr {
                base: self.expand_expr(neg.base)?,
            })),
            Expression::BitNot(not) => Expression::BitNot(Box::new(BitNotExpr {
                base: self.expand_expr(not.base)?,
            })),
            Expression::Not(not) => Expression::Not(Box::new(NotExpr {
                base: self.expand_expr(not.base)?,
            })),
            Expression::Logical(logical) => Expression::Logical(Box::new(LogicalExpr {
                op: logical.op,
                left: self.expand_expr(logical.left)?,
                right: self.expand_expr(logical.right)?,
            })),
            Expression::Reveal(reveal) => Expression::Reveal(Box::new(RevealExpr {
                base: self.expand_expr(reveal.base)?,
            })),
            Expression::Comp(comp) => Expression::Comp(Box::new(CompExpr {
                base: self.expand_expr(comp.base)?,
            })),
            Expression::Cast(cast) => Expression::Cast(Box::new(CastExpr {
                target: cast.target,
                base: self.expand_expr(cast.base)?,
            })),
            // No `base` expression to recurse into, and a bare `Type` can never
            // contain a macro metavariable (`$name` is only meaningful in
            // expression position -- see `lexer::TokenKind::Metavar`'s doc
            // comment) -- a plain passthrough, like `Expression::Path` above.
            Expression::Sizeof(sizeof) => Expression::Sizeof(sizeof),
            Expression::Increment(incr) => Expression::Increment(Box::new(IncrementExpr {
                base: self.expand_expr(incr.base)?,
            })),
            Expression::Decrement(decr) => Expression::Decrement(Box::new(DecrementExpr {
                base: self.expand_expr(decr.base)?,
            })),
            Expression::BinaryOp(bin) => Expression::BinaryOp(Box::new(BinaryOpExpr {
                left: self.expand_expr(bin.left)?,
                op: bin.op,
                right: self.expand_expr(bin.right)?,
            })),
            Expression::Number(n) => Expression::Number(n),
            Expression::String(s) => Expression::String(s),
            Expression::ByteString(s) => Expression::ByteString(s),
            Expression::Bool(b) => Expression::Bool(b),
            Expression::Char(c) => Expression::Char(c),
            Expression::Codeblock(cb) => {
                Expression::Codeblock(self.expand_codeblock(cb)?)
            }
            Expression::If(if_expr) => {
                Expression::If(Box::new(self.expand_if(*if_expr)?))
            }
            Expression::FunctionCall(call) => Expression::FunctionCall(FunctionCallExpr {
                callee: Box::new(self.expand_expr(*call.callee)?),
                args: call
                    .args
                    .into_iter()
                    .map(|a| self.expand_expr(a))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            Expression::Assignment(assign) => Expression::Assignment(Box::new(AssignmentExpr {
                target: self.expand_expr(assign.target)?,
                value: Box::new(self.expand_expr(*assign.value)?),
            })),
            Expression::CompoundAssign(assign) => Expression::CompoundAssign(Box::new(CompoundAssignExpr {
                target: self.expand_expr(assign.target)?,
                op: assign.op,
                value: Box::new(self.expand_expr(*assign.value)?),
            })),
            Expression::ArrayLiteral(lit) => Expression::ArrayLiteral(ArrayLiteralExpr {
                elements: lit
                    .elements
                    .into_iter()
                    .map(|e| self.expand_expr(e))
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
                            value: self.expand_expr(f.value)?,
                        })
                    })
                    .collect::<Result<Vec<_>, MacroError>>()?,
            }),
            Expression::Slice(s) => Expression::Slice(Box::new(SliceExpr {
                base: self.expand_expr(s.base)?,
                range: self.expand_range(s.range)?,
            })),
            Expression::Match(m) => Expression::Match(Box::new(self.expand_match(*m)?)),
            Expression::Range(r) => Expression::Range(Box::new(self.expand_range(*r)?)),
        };
        Ok(ExpressionNode { expression, span })
    }

    fn expand_range(&mut self, range: RangeExpr) -> Result<RangeExpr, MacroError> {
        let end = match range.end {
            RangeEnd::Inclusive(e) => RangeEnd::Inclusive(self.expand_expr(e)?),
            RangeEnd::Exclusive(e) => RangeEnd::Exclusive(self.expand_expr(e)?),
            RangeEnd::Open => RangeEnd::Open,
        };
        Ok(RangeExpr {
            start: range
                .start
                .map(|e| self.expand_expr(e))
                .transpose()?,
            end,
            span: range.span,
        })
    }

    fn expand_match(&mut self, match_expr: MatchExpr) -> Result<MatchExpr, MacroError> {
        let scrutinee = self.expand_expr(match_expr.scrutinee)?;
        let arms = match_expr
            .arms
            .into_iter()
            .map(|arm| {
                let pattern = match arm.pattern {
                    Pattern::Value(v) => Pattern::Value(self.expand_expr(v)?),
                    Pattern::Range(r) => Pattern::Range(self.expand_range(r)?),
                };
                Ok(MatchArm {
                    pattern,
                    body: self.expand_expr(arm.body)?,
                    span: arm.span,
                })
            })
            .collect::<Result<Vec<_>, MacroError>>()?;
        let else_branch = match_expr
            .else_branch
            .map(|b| self.expand_codeblock(b))
            .transpose()?;
        Ok(MatchExpr {
            scrutinee,
            arms,
            else_branch,
            span: match_expr.span,
        })
    }
}
