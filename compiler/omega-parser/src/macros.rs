mod expander;

use crate::ast::identifier::{ExpansionId, Ident, Origin};
use crate::ast::range::{RangeEnd, RangeExpr};
use crate::ast::statement::WalrusStmt;
use crate::diagnostics::ParseError;
use crate::lexer::{Token, TokenKind};
use crate::parser::Parser;
use crate::prelude::*;
use omega_diagnostics::SourceFile;
use std::collections::{HashMap, HashSet};
use std::fmt;

const MAX_EXPANSIONS: u32 = 256;

#[derive(Debug, Default)]
pub struct ExpansionState {
    next_id: u32,
    origins: HashMap<ExpansionId, ExpansionOrigin>,
    environments: HashMap<Vec<Ident>, HashMap<Ident, MacroDefinitionStmt>>,
    /// Macro names a module's own source actually invoked. Expansion consumes
    /// macro bindings before HIR exists, so this is the only record that a
    /// macro import was used.
    invocations: HashSet<(Vec<Ident>, Ident)>,
}

#[derive(Debug, Clone)]
struct ExpansionOrigin {
    defining_module: Vec<Ident>,
    /// Absent for the synthetic origins that only name a resolution module
    /// (alias expansion), which author no syntax of their own.
    macro_site: Option<MacroSite>,
}

#[derive(Debug, Clone)]
struct MacroSite {
    name: Ident,
    definition: Span,
    invocation_module: Vec<Ident>,
    invocation: Span,
    parent: Origin,
}

/// Where macro-authored syntax was written, plus the invocation chain that
/// brought it into the module being compiled (innermost invocation first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroAuthorship {
    pub macro_name: Ident,
    pub defining_module: Vec<Ident>,
    pub definition: Span,
    pub expansion: Vec<MacroInvocationSite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroInvocationSite {
    pub macro_name: Ident,
    pub module: Vec<Ident>,
    pub span: Span,
}

impl ExpansionState {
    pub fn defining_module(&self, origin: Origin) -> Option<&[Ident]> {
        origin.0.and_then(|id| {
            self.origins
                .get(&id)
                .map(|entry| entry.defining_module.as_slice())
        })
    }

    /// Records an origin for syntax that is not a macro expansion. Alias
    /// expansion uses it so an alias target keeps resolving at the alias
    /// declaration site after it is substituted into a use site in another
    /// module.
    pub fn register_definition_module(&mut self, module: &[Ident]) -> Origin {
        self.insert(ExpansionOrigin {
            defining_module: module.to_vec(),
            macro_site: None,
        })
    }

    /// Where the syntax carrying `origin` was authored. `None` means the
    /// syntax is the module's own: either written there directly, or
    /// substituted into an expansion by its caller.
    pub fn authorship(&self, origin: Origin) -> Option<MacroAuthorship> {
        let entry = origin.0.and_then(|id| self.origins.get(&id))?;
        let site = entry.macro_site.as_ref()?;

        let mut expansion = vec![MacroInvocationSite {
            macro_name: site.name.clone(),
            module: site.invocation_module.clone(),
            span: site.invocation,
        }];
        let mut parent = site.parent;
        // Nested expansions form a chain, and `insert` only ever links to an
        // already-recorded parent, so this walk terminates.
        while let Some(outer) = parent
            .0
            .and_then(|id| self.origins.get(&id))
            .and_then(|entry| entry.macro_site.as_ref())
        {
            expansion.push(MacroInvocationSite {
                macro_name: outer.name.clone(),
                module: outer.invocation_module.clone(),
                span: outer.invocation,
            });
            parent = outer.parent;
        }

        Some(MacroAuthorship {
            macro_name: site.name.clone(),
            defining_module: entry.defining_module.clone(),
            definition: site.definition,
            expansion,
        })
    }

    fn insert(&mut self, origin: ExpansionOrigin) -> Origin {
        let id = ExpansionId(self.next_id);
        self.next_id += 1;
        self.origins.insert(id, origin);
        Origin(Some(id))
    }

    fn fresh_origin(
        &mut self,
        def: &MacroDefinitionStmt,
        invocation_module: &[Ident],
        invocation: Span,
        parent: Origin,
    ) -> Origin {
        self.insert(ExpansionOrigin {
            defining_module: def.defining_module.clone(),
            macro_site: Some(MacroSite {
                name: def.name.clone(),
                definition: def.span,
                invocation_module: invocation_module.to_vec(),
                invocation,
                parent,
            }),
        })
    }

    /// Records that `module`'s own source invoked `name`. Invocations written
    /// inside a macro body are not recorded: they resolve in the defining
    /// module's environment, not through this module's imports.
    pub(crate) fn record_invocation(&mut self, module: &[Ident], name: &Ident, origin: Origin) {
        if self.authorship(origin).is_some() {
            return;
        }
        self.invocations.insert((module.to_vec(), name.clone()));
    }

    pub fn invoked_macro(&self, module: &[Ident], name: &Ident) -> bool {
        self.invocations.contains(&(module.to_vec(), name.clone()))
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

/// Where a macro failure is actionable: the declaration it is about, the
/// invocation being expanded, or both.
#[derive(Debug, Default, Clone)]
pub struct MacroErrorSite {
    pub definition: Option<(Vec<Ident>, Span)>,
    pub invocation: Option<Span>,
}

#[derive(Debug)]
pub struct MacroError {
    pub kind: MacroErrorKind,
    pub site: MacroErrorSite,
}

impl MacroError {
    pub fn new(kind: MacroErrorKind) -> Self {
        Self {
            kind,
            site: MacroErrorSite::default(),
        }
    }

    pub fn at_definition(mut self, module: &[Ident], span: Span) -> Self {
        self.site.definition = Some((module.to_vec(), span));
        self
    }

    pub fn at_invocation(mut self, span: Span) -> Self {
        self.site.invocation = Some(span);
        self
    }
}

impl From<MacroErrorKind> for MacroError {
    fn from(kind: MacroErrorKind) -> Self {
        Self::new(kind)
    }
}

impl fmt::Display for MacroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

#[derive(Debug)]
pub enum MacroErrorKind {
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
    MalformedBuiltinDeclaration {
        builtin: MacroBuiltin,
    },
    BuiltinWithoutSourceContext {
        builtin: MacroBuiltin,
    },
    SourceLocationOutOfRange {
        builtin: MacroBuiltin,
        value: usize,
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

impl fmt::Display for MacroErrorKind {
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
            Self::MalformedBuiltinDeclaration { builtin } => write!(
                f,
                "'{}::{}' is compiler-implemented and must be declared as an exposed macro with \
                 no parameters and an empty body",
                MacroBuiltin::MODULE.join("::"),
                builtin.name()
            ),
            Self::BuiltinWithoutSourceContext { builtin } => write!(
                f,
                "'{}' needs the invoking module's source, which this expansion has no access to",
                builtin.name()
            ),
            Self::SourceLocationOutOfRange { builtin, value } => write!(
                f,
                "'{}' is {value} here, which does not fit in the 'u32' it expands to",
                builtin.name()
            ),
        }
    }
}

/// Template-only expansion with no module identity or source text. A
/// compiler-backed builtin invoked through this path fails rather than
/// inventing a source location.
pub fn expand(
    module: SourceModule,
    imported: &HashMap<Ident, MacroDefinitionStmt>,
) -> Result<SourceModule, MacroError> {
    let mut state = ExpansionState::default();
    expand_with_origins(module, imported, &[], None, &mut state)
}

/// `source` is the file being expanded, not the file a macro was defined in:
/// `file$`/`line$`/`column$` describe the invocation site even when the
/// invocation was written inside another macro's body.
pub fn expand_with_origins(
    module: SourceModule,
    imported: &HashMap<Ident, MacroDefinitionStmt>,
    module_path: &[Ident],
    source: Option<&SourceFile>,
    state: &mut ExpansionState,
) -> Result<SourceModule, MacroError> {
    let (own, items) = collect_definitions(module.nodes, module_path)?;
    let mut defs = imported.clone();
    defs.extend(own);
    state.register_environment(module_path, &defs);
    for def in defs.values() {
        validate_definition(def)?;
    }
    let nodes =
        expander::Expander::new(&defs, module_path, source, state).expand_item_list(items)?;
    Ok(SourceModule { nodes })
}

/// Attaches the facts a raw parsed definition cannot know: which module
/// declared it, and therefore whether it is one of the compiler-backed
/// `core::builtins` declarations. Every path that binds a *declaration* to
/// its module goes through here, so a re-collected definition can never be
/// classified differently from a cached one, and the compiler/core contract
/// is enforced on the declaration itself rather than on later copies of it
/// such as macro aliases.
pub fn bind_definition(
    def: &mut MacroDefinitionStmt,
    module_path: &[Ident],
) -> Result<(), MacroError> {
    def.defining_module = module_path.to_vec();
    def.builtin = MacroBuiltin::canonical(module_path, &def.name);
    let Some(builtin) = def.builtin else {
        return Ok(());
    };
    let well_formed = def.visibility == Visibility::Exposed
        && def.signature.fixed.is_empty()
        && def.signature.variadic.is_none()
        && def.body.is_empty();
    if !well_formed {
        return Err(
            MacroError::new(MacroErrorKind::MalformedBuiltinDeclaration { builtin })
                .at_definition(module_path, def.span),
        );
    }
    Ok(())
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
                    return Err(MacroError::new(MacroErrorKind::DuplicateMacroDefinition {
                        name: def.name,
                    })
                    .at_definition(module_path, def.span));
                }
                bind_definition(&mut def, module_path)?;
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
    fn at(def: &MacroDefinitionStmt, kind: MacroErrorKind) -> MacroError {
        MacroError::new(kind).at_definition(&def.defining_module, def.span)
    }

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
                        return Err(at(
                            def,
                            MacroErrorKind::UnknownMetavariable {
                                macro_name: def.name.clone(),
                                metavar: ident,
                            },
                        ));
                    }
                    if variadic.is_some_and(|v| *v == ident) {
                        if !in_repetition {
                            return Err(at(
                                def,
                                MacroErrorKind::VariadicOutsideRepetition {
                                    macro_name: def.name.clone(),
                                    metavar: ident,
                                },
                            ));
                        }
                        mentions_variadic = true;
                    }
                }
                MacroBodyPiece::Token(_) => {}
                MacroBodyPiece::Repetition(rep) => {
                    if variadic.is_none() {
                        return Err(at(
                            def,
                            MacroErrorKind::RepetitionWithoutVariadic {
                                macro_name: def.name.clone(),
                            },
                        ));
                    }
                    if !walk(def, &rep.body, true)? {
                        return Err(at(
                            def,
                            MacroErrorKind::RepetitionMissingVariadic {
                                macro_name: def.name.clone(),
                            },
                        ));
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
    Err(MacroError::new(MacroErrorKind::FragmentMismatch {
        macro_name: def.name.clone(),
        param: param.name.clone(),
        expected: param.kind,
        errors: message,
    })
    .at_definition(&def.defining_module, def.span))
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
