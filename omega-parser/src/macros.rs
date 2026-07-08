//! Compile-time macro expansion: a pure `SourceModule -> SourceModule`
//! syntax transform. This is the *only* place `RootStatement::
//! MacroDefinition`/`MacroInvocation` and `Expression::MacroInvocation`
//! exist -- by the time [`expand`] returns successfully, none of them
//! remain anywhere in the tree (see `omega_hir::lower::Lowerer`'s
//! `unreachable!()` arms for those variants), so nothing downstream of
//! `omega-parser` (HIR lowering, analysis, codegen) needs any notion of
//! macros at all.
//!
//! A macro's body is captured as a raw [`Token`] list at parse time (see
//! `parser::macro_syntax`), substituted at each invocation, and fed
//! directly into the ordinary parser's token-based entry points
//! (`parser::expression::parse_expression`/`parser::item::parse_source_module`)
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
/// module, so a runaway recursive macro (`macro a() => expr { a!() }`)
/// produces a clean [`MacroError::ExpansionLimitExceeded`] instead of a
/// stack overflow.
const MAX_EXPANSIONS: u32 = 256;

#[derive(Debug)]
pub enum MacroError {
    DuplicateMacroDefinition { name: Ident },
    UnknownMetavariable { macro_name: Ident, metavar: Ident },
    UnknownMacro { name: Ident },
    ArgCountMismatch { macro_name: Ident, expected: usize, found: usize },
    FragmentMismatch { macro_name: Ident, param: Ident, expected: FragmentKind, errors: String },
    WrongOutputKindForPosition { macro_name: Ident, expected: MacroOutputKind, found: MacroOutputKind },
    ExpansionParseError { macro_name: Ident, errors: String },
    ExpansionLimitExceeded { macro_name: Ident },
}

impl fmt::Display for MacroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateMacroDefinition { name } => {
                write!(f, "macro '{}' is defined more than once", name.0)
            }
            Self::UnknownMetavariable { macro_name, metavar } => write!(
                f,
                "macro '{}' references unknown metavariable '${}' (not one of its own parameters)",
                macro_name.0, metavar.0
            ),
            Self::UnknownMacro { name } => write!(f, "no macro named '{}' is defined in this module", name.0),
            Self::ArgCountMismatch { macro_name, expected, found } => write!(
                f,
                "macro '{}' expects {expected} argument(s), found {found}",
                macro_name.0
            ),
            Self::FragmentMismatch { macro_name, param, expected, errors } => write!(
                f,
                "macro '{}': argument for '${}' does not parse as {expected:?}: {errors}",
                macro_name.0, param.0
            ),
            Self::WrongOutputKindForPosition { macro_name, expected, found } => write!(
                f,
                "macro '{}' produces {found:?}, not {expected:?}, and can't be used here",
                macro_name.0
            ),
            Self::ExpansionParseError { macro_name, errors } => {
                write!(f, "macro '{}' expanded into invalid syntax: {errors}", macro_name.0)
            }
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
/// module that contains only the five ordinary [`RootStatement`] variants
/// that existed before macros were added.
pub fn expand(module: SourceModule) -> Result<SourceModule, MacroError> {
    let (defs, items) = collect_definitions(module.nodes)?;
    for def in defs.values() {
        validate_body_metavars(def)?;
    }
    let mut budget = MAX_EXPANSIONS;
    let nodes = expand_item_list(items, &defs, &mut budget)?;
    Ok(SourceModule { nodes })
}

/// Splits `nodes` into macro definitions (by name, rejecting a duplicate
/// name outright) and everything else, in original order.
fn collect_definitions(
    nodes: Vec<RootStatementNode>,
) -> Result<(HashMap<Ident, MacroDefStmt>, Vec<RootStatementNode>), MacroError> {
    let mut defs = HashMap::new();
    let mut items = Vec::new();
    for node in nodes {
        match node.root_stmt {
            RootStatement::MacroDefinition(def) => {
                if defs.contains_key(&def.name) {
                    return Err(MacroError::DuplicateMacroDefinition { name: def.name });
                }
                defs.insert(def.name.clone(), def);
            }
            other => items.push(RootStatementNode { root_stmt: other, span: node.span }),
        }
    }
    Ok((defs, items))
}

/// Every `$name` in a definition's body must name one of that macro's own
/// parameters -- a real definition bug (a typo, most likely), not something
/// duck typing should hide, so this is checked once up front rather than
/// only surfacing confusingly if/when some invocation happens to reach it.
/// A flat scan, not a recursive tree walk: unlike the old `Token` model
/// (which nested a bracketed group's contents inside a `Token::Group`
/// variant), the lexer's token stream is entirely flat -- `(`/`)`/etc. are
/// ordinary tokens like any other, so a `$name` reference can never be
/// "nested" in a way this needs to recurse into.
fn validate_body_metavars(def: &MacroDefStmt) -> Result<(), MacroError> {
    for token in &def.body {
        if let TokenKind::Metavar(name) = &token.kind {
            let ident = Ident(name.clone());
            if !def.params.iter().any(|p| p.name == ident) {
                return Err(MacroError::UnknownMetavariable { macro_name: def.name.clone(), metavar: ident });
            }
        }
    }
    Ok(())
}

/// Walks a list of top-level items, splicing each `items`-output macro
/// invocation's expansion in place and recursing into every function/struct
/// body for `expr`-output invocations nested inside expressions.
fn expand_item_list(
    nodes: Vec<RootStatementNode>,
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<Vec<RootStatementNode>, MacroError> {
    let mut result = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node.root_stmt {
            RootStatement::MacroInvocation(inv) => {
                result.extend(expand_items_invocation(&inv, defs, budget)?);
            }
            RootStatement::FunctionDefinition(f) => result.push(RootStatementNode {
                root_stmt: RootStatement::FunctionDefinition(expand_function_def(f, defs, budget)?),
                span: node.span,
            }),
            RootStatement::Struct(s) => result.push(RootStatementNode {
                root_stmt: RootStatement::Struct(expand_struct_def(s, defs, budget)?),
                span: node.span,
            }),
            other @ (RootStatement::Declaration(_) | RootStatement::ExternDeclaration(_) | RootStatement::Import(_)) => {
                result.push(RootStatementNode { root_stmt: other, span: node.span });
            }
            RootStatement::MacroDefinition(_) => {
                unreachable!("macro definitions were already removed by collect_definitions")
            }
        }
    }
    Ok(result)
}

/// Expands one `items`-output invocation into its (recursively expanded)
/// replacement items -- recursing through `expand_item_list` again so an
/// invocation nested inside the expansion (either written directly in the
/// macro's body, or introduced via a substituted argument) is itself
/// expanded, with no separate token-level nested-invocation handling needed.
fn expand_items_invocation(
    inv: &MacroInvocationExpr,
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<Vec<RootStatementNode>, MacroError> {
    let def = defs.get(&inv.name).ok_or_else(|| MacroError::UnknownMacro { name: inv.name.clone() })?;
    if def.output != MacroOutputKind::Items {
        return Err(MacroError::WrongOutputKindForPosition {
            macro_name: inv.name.clone(),
            expected: MacroOutputKind::Items,
            found: def.output,
        });
    }
    let tokens = substitute_invocation(def, &inv.args, budget)?;
    let padded = with_eof(&tokens);
    let mut p = Parser::new(&padded);
    let nodes = crate::parser::item::parse_source_module(&mut p);
    let errors = p.into_errors();
    if !errors.is_empty() {
        return Err(MacroError::ExpansionParseError { macro_name: inv.name.clone(), errors: join_errors(&errors) });
    }
    expand_item_list(nodes, defs, budget)
}

/// Expands one `expr`-output invocation, recursing into the (possibly
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
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<ExpressionNode, MacroError> {
    let def = defs.get(&inv.name).ok_or_else(|| MacroError::UnknownMacro { name: inv.name.clone() })?;
    if def.output != MacroOutputKind::Expr {
        return Err(MacroError::WrongOutputKindForPosition {
            macro_name: inv.name.clone(),
            expected: MacroOutputKind::Expr,
            found: def.output,
        });
    }
    let tokens = substitute_invocation(def, &inv.args, budget)?;
    let padded = with_eof(&tokens);
    let mut p = Parser::new(&padded);
    let parsed = crate::parser::expression::parse_expression(&mut p);
    let fully_consumed = p.is_eof();
    let errors = p.into_errors();
    let node = match parsed {
        Some(node) if fully_consumed && errors.is_empty() => node,
        _ => {
            let message = if errors.is_empty() { "unexpected trailing tokens".to_string() } else { join_errors(&errors) };
            return Err(MacroError::ExpansionParseError { macro_name: inv.name.clone(), errors: message });
        }
    };
    expand_expr(node, defs, budget)
}

/// Validates argument count and each argument's shape against its
/// parameter's declared [`FragmentKind`], then substitutes every `$name` in
/// `def`'s body with the corresponding argument's tokens. Also where the
/// expansion budget (see [`MAX_EXPANSIONS`]) is spent -- one unit per
/// invocation, regardless of output kind.
fn substitute_invocation(
    def: &MacroDefStmt,
    args: &[Vec<Token>],
    budget: &mut u32,
) -> Result<Vec<Token>, MacroError> {
    if args.len() != def.params.len() {
        return Err(MacroError::ArgCountMismatch {
            macro_name: def.name.clone(),
            expected: def.params.len(),
            found: args.len(),
        });
    }
    if *budget == 0 {
        return Err(MacroError::ExpansionLimitExceeded { macro_name: def.name.clone() });
    }
    *budget -= 1;

    let mut subst: HashMap<Ident, &[Token]> = HashMap::new();
    for (param, arg) in def.params.iter().zip(args.iter()) {
        validate_fragment(def, param, arg)?;
        subst.insert(param.name.clone(), arg.as_slice());
    }
    Ok(substitute_tokens(&def.body, &subst))
}

/// Parses `arg` against `param`'s declared fragment grammar -- this is what
/// gives a fragment specifier real meaning (it constrains what can legally
/// be captured there) rather than being documentation only, and reports a
/// mismatch at the invocation site instead of letting it surface
/// confusingly deep inside expanded code.
fn validate_fragment(def: &MacroDefStmt, param: &MacroParam, arg: &[Token]) -> Result<(), MacroError> {
    let padded = with_eof(arg);
    let mut p = Parser::new(&padded);
    let result = match param.kind {
        FragmentKind::Expr => crate::parser::expression::parse_expression(&mut p).map(|_| ()),
        FragmentKind::Type => crate::parser::r#type::parse_type(&mut p).map(|_| ()),
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
    Err(MacroError::FragmentMismatch { macro_name: def.name.clone(), param: param.name.clone(), expected: param.kind, errors: message })
}

/// A flat substitution pass, mirroring `validate_body_metavars`'s "no
/// nesting to recurse into" note above.
fn substitute_tokens(body: &[Token], subst: &HashMap<Ident, &[Token]>) -> Vec<Token> {
    let mut out = Vec::new();
    for token in body {
        if let TokenKind::Metavar(name) = &token.kind {
            let ident = Ident(name.clone());
            let replacement = subst
                .get(&ident)
                .expect("unknown metavariable should have already been rejected by validate_body_metavars");
            out.extend(replacement.iter().cloned());
        } else {
            out.push(token.clone());
        }
    }
    out
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
    out.push(Token { kind: TokenKind::Eof, span: eof_span });
    out
}

fn join_errors(errors: &[ParseError]) -> String {
    errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

fn expand_function_def(
    f: FunctionDefinitionStmt,
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<FunctionDefinitionStmt, MacroError> {
    Ok(FunctionDefinitionStmt { codeblock: expand_codeblock(f.codeblock, defs, budget)?, ..f })
}

fn expand_struct_def(
    s: StructStmt,
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<StructStmt, MacroError> {
    let functions = s
        .functions
        .into_iter()
        .map(|f| expand_function_def(f, defs, budget))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StructStmt { functions, ..s })
}

fn expand_codeblock(
    cb: CodeblockExpr,
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<CodeblockExpr, MacroError> {
    let statements = cb
        .statements
        .into_iter()
        .map(|s| expand_stmt_node(s, defs, budget))
        .collect::<Result<Vec<_>, _>>()?;
    let tail = cb.tail.map(|t| expand_expr(*t, defs, budget).map(Box::new)).transpose()?;
    Ok(CodeblockExpr { statements, tail })
}

fn expand_if(if_expr: IfExpr, defs: &HashMap<Ident, MacroDefStmt>, budget: &mut u32) -> Result<IfExpr, MacroError> {
    let branches = if_expr
        .branches
        .into_iter()
        .map(|(cond, block)| Ok((expand_expr(cond, defs, budget)?, expand_codeblock(block, defs, budget)?)))
        .collect::<Result<Vec<_>, MacroError>>()?;
    let else_branch = if_expr.else_branch.map(|b| expand_codeblock(b, defs, budget)).transpose()?;
    Ok(IfExpr { branches, else_branch })
}

fn expand_stmt_node(
    node: StatementNode,
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<StatementNode, MacroError> {
    let span = node.span;
    let statement = expand_statement(node.statement, defs, budget)?;
    Ok(StatementNode { statement, span })
}

fn expand_statement(
    statement: Statement,
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<Statement, MacroError> {
    Ok(match statement {
        Statement::Declaration(decl) => Statement::Declaration(decl),
        Statement::DeclarationWithInit(decl, value) => {
            Statement::DeclarationWithInit(decl, expand_expr(value, defs, budget)?)
        }
        Statement::ExternDeclaration(decl) => Statement::ExternDeclaration(decl),
        Statement::Expression(expr) => Statement::Expression(expand_expr(expr, defs, budget)?),
        Statement::Return(ret) => {
            Statement::Return(ReturnStmt { return_value: expand_expr(ret.return_value, defs, budget)? })
        }
        Statement::Break => Statement::Break,
        Statement::Continue => Statement::Continue,
        Statement::Struct(s) => Statement::Struct(expand_struct_def(s, defs, budget)?),
        Statement::Walrus(w) => Statement::Walrus(WalrusStmt { value: expand_expr(w.value, defs, budget)?, ..w }),
        Statement::While(w) => Statement::While(WhileStmt {
            condition: expand_expr(w.condition, defs, budget)?,
            body: expand_codeblock(w.body, defs, budget)?,
        }),
        Statement::For(f) => {
            let f = *f;
            Statement::For(Box::new(ForStmt {
                init: f.init.map(|s| expand_statement(s, defs, budget)).transpose()?,
                condition: f.condition.map(|c| expand_expr(c, defs, budget)).transpose()?,
                post: f.post.map(|p| expand_expr(p, defs, budget)).transpose()?,
                body: expand_codeblock(f.body, defs, budget)?,
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
    defs: &HashMap<Ident, MacroDefStmt>,
    budget: &mut u32,
) -> Result<ExpressionNode, MacroError> {
    let span = node.span;
    if let Expression::MacroInvocation(inv) = node.expression {
        let expanded = expand_expr_invocation(&inv, defs, budget)?;
        return Ok(ExpressionNode { expression: expanded.expression, span });
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
            Expression::Deref(Box::new(DerefExpr { base: expand_expr(deref.base, defs, budget)? }))
        }
        Expression::AddressOf(addr) => {
            let addr = *addr;
            Expression::AddressOf(Box::new(AddressOfExpr { base: expand_expr(addr.base, defs, budget)? }))
        }
        Expression::Negate(neg) => {
            let neg = *neg;
            Expression::Negate(Box::new(NegateExpr { base: expand_expr(neg.base, defs, budget)? }))
        }
        Expression::Increment(incr) => {
            let incr = *incr;
            Expression::Increment(Box::new(IncrementExpr { base: expand_expr(incr.base, defs, budget)? }))
        }
        Expression::Decrement(decr) => {
            let decr = *decr;
            Expression::Decrement(Box::new(DecrementExpr { base: expand_expr(decr.base, defs, budget)? }))
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
        Expression::Bool(b) => Expression::Bool(b),
        Expression::Char(c) => Expression::Char(c),
        Expression::Codeblock(cb) => Expression::Codeblock(expand_codeblock(cb, defs, budget)?),
        Expression::If(if_expr) => Expression::If(Box::new(expand_if(*if_expr, defs, budget)?)),
        Expression::FunctionCall(call) => Expression::FunctionCall(FunctionCallExpr {
            callee: Box::new(expand_expr(*call.callee, defs, budget)?),
            args: call.args.into_iter().map(|a| expand_expr(a, defs, budget)).collect::<Result<Vec<_>, _>>()?,
        }),
        Expression::Assignment(assign) => {
            let assign = *assign;
            Expression::Assignment(Box::new(AssignmentExpr {
                target: expand_expr(assign.target, defs, budget)?,
                value: Box::new(expand_expr(*assign.value, defs, budget)?),
            }))
        }
        Expression::ArrayLiteral(lit) => Expression::ArrayLiteral(ArrayLiteralExpr {
            elements: lit.elements.into_iter().map(|e| expand_expr(e, defs, budget)).collect::<Result<Vec<_>, _>>()?,
        }),
        Expression::Slice(s) => {
            let s = *s;
            Expression::Slice(Box::new(SliceExpr {
                base: expand_expr(s.base, defs, budget)?,
                start: s.start.map(|e| expand_expr(e, defs, budget)).transpose()?,
                end: s.end.map(|e| expand_expr(e, defs, budget)).transpose()?,
            }))
        }
    };
    Ok(ExpressionNode { expression, span })
}
