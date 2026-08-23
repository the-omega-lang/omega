mod expander;

use crate::ast::identifier::{ExpansionId, Ident, Origin};
use crate::ast::range::{RangeEnd, RangeExpr};
use crate::ast::statement::WalrusStmt;
use crate::diagnostics::ParseError;
use crate::lexer::{Token, TokenKind};
use crate::parser::Parser;
use crate::prelude::*;
use std::collections::HashMap;
use std::fmt;

const MAX_EXPANSIONS: u32 = 256;

#[derive(Debug, Default)]
pub struct ExpansionState {
    next_id: u32,
    origins: HashMap<ExpansionId, ExpansionOrigin>,
    environments: HashMap<Vec<Ident>, HashMap<Ident, MacroDefinitionStmt>>,
}

#[derive(Debug, Clone)]
struct ExpansionOrigin {
    defining_module: Vec<Ident>,
    /// `None` for an origin that only records a definition module without
    /// being a macro expansion, so macro dependency-leak checks skip it.
    macro_visibility: Option<Visibility>,
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
            .and_then(|id| self.origins.get(&id))
            .and_then(|entry| entry.macro_visibility)
    }

    /// Records a non-macro origin that only names the module a piece of
    /// syntax must resolve in. Alias expansion uses it so an alias target
    /// keeps resolving at the alias declaration site after it is substituted
    /// into a use site in another module.
    pub fn register_definition_module(&mut self, module: &[Ident]) -> Origin {
        let id = ExpansionId(self.next_id);
        self.next_id += 1;
        self.origins.insert(
            id,
            ExpansionOrigin {
                defining_module: module.to_vec(),
                macro_visibility: None,
            },
        );
        Origin(Some(id))
    }

    fn fresh_origin(&mut self, def: &MacroDefinitionStmt) -> Origin {
        let id = ExpansionId(self.next_id);
        self.next_id += 1;
        self.origins.insert(
            id,
            ExpansionOrigin {
                defining_module: def.defining_module.clone(),
                macro_visibility: Some(def.visibility),
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

    fn definition_for(
        &self,
        origin: Origin,
        ambient: &HashMap<Ident, MacroDefinitionStmt>,
        name: &Ident,
    ) -> Option<MacroDefinitionStmt> {
        let environment = self
            .defining_module(origin)
            .and_then(|module| self.environments.get(module))
            .unwrap_or(ambient);
        environment.get(name).cloned()
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

pub fn expand(
    module: SourceModule,
    imported: &HashMap<Ident, MacroDefinitionStmt>,
) -> Result<SourceModule, MacroError> {
    let mut state = ExpansionState::default();
    expand_with_origins(module, imported, &[], &mut state)
}

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
    let nodes = expander::Expander::new(&defs, state).expand_item_list(items)?;
    Ok(SourceModule { nodes })
}

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
        FragmentKind::Path => crate::parser::parse_path(&mut p).map(|_| ()),
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
