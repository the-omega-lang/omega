use crate::ast::expression::Expression;
use crate::ast::identifier::Ident;
use crate::ast::statement::{
    AsmDescriptorKind, AsmDescriptorNode, DeclarationStmt, DeferStmt, ForInStmt, ForStmt,
    InlineAsmStmt, LoopStmt, ReturnStmt, Statement, StatementNode, WalrusStmt, WhileStmt,
};
use crate::ast::visibility::Visibility;
use crate::diagnostics::ParseErrorKind;
use crate::lexer::TokenKind;
use crate::parser::expression::{
    parse_codeblock, parse_expression, parse_range_or_expression,
    parse_statement_leading_expression,
};
use crate::parser::macro_syntax::parse_macro_invocation;
use crate::parser::{Parser, contextual, recovery};

/// Whether a fresh statement can begin at the current token. Recovery uses it
/// so a resynchronization point is a plausible statement start rather than any
/// identifier, which inside a broken expression is almost always an operand.
pub(crate) fn starts_statement(p: &Parser) -> bool {
    if matches!(
        p.peek(),
        TokenKind::At
            | TokenKind::If
            | TokenKind::Match
            | TokenKind::While
            | TokenKind::Loop
            | TokenKind::For
            | TokenKind::Return
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Defer
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Union
            | TokenKind::Spec
            | TokenKind::Alias
            | TokenKind::LBrace
    ) {
        return true;
    }
    if crate::parser::binding_modifiers_follow(p) || p.at_contextual(contextual::ASM) {
        return true;
    }
    // A bare name begins a statement only when what follows commits it to one:
    // a call, an assignment, a member/index chain, or a macro invocation.
    matches!(p.peek(), TokenKind::Ident(_))
        && matches!(
            p.peek_at(1),
            TokenKind::LParen
                | TokenKind::Dollar
                | TokenKind::Eq
                | TokenKind::Dot
                | TokenKind::LBracket
                | TokenKind::ColonColon
        )
}

pub fn parse_statement(p: &mut Parser) -> Option<StatementNode> {
    let start = p.peek_span();
    let (statement, block_shaped) = parse_statement_content(p)?;
    if block_shaped {
        p.eat(&TokenKind::Semi);
    } else if !p.expect_terminator(&TokenKind::Semi, "';'") {
        return None;
    }
    let span = start.to(p.last_span());
    Some(StatementNode { statement, span })
}

fn parse_statement_content(p: &mut Parser) -> Option<(Statement, bool)> {
    if let Some(prefix) = crate::parser::parse_binding_modifiers(p) {
        return parse_walrus_or_declaration(p, prefix.mutable, prefix.comp);
    }
    match p.peek() {
        TokenKind::Struct => {
            reject_local_type_decl(p, ParseErrorKind::StructNotAllowedHere);
            None
        }
        TokenKind::Enum => {
            reject_local_type_decl(p, ParseErrorKind::EnumNotAllowedHere);
            None
        }
        TokenKind::Union => {
            reject_local_type_decl(p, ParseErrorKind::UnionNotAllowedHere);
            None
        }
        TokenKind::Spec => {
            reject_local_type_decl(p, ParseErrorKind::SpecNotAllowedHere);
            None
        }
        TokenKind::Alias => {
            reject_local_type_decl(p, ParseErrorKind::AliasNotAllowedHere);
            None
        }
        TokenKind::While => Some((Statement::While(parse_while(p)?), true)),
        TokenKind::Loop => Some((Statement::Loop(parse_loop(p)?), true)),
        TokenKind::For => Some((parse_for(p)?, true)),
        TokenKind::Defer => {
            p.advance(); // 'defer'
            let (inner, block_shaped) = parse_statement_content(p)?;
            if matches!(inner, Statement::MacroInvocation(_)) {
                p.error(ParseErrorKind::MacroInvocationNotAllowedAfterDefer);
                return None;
            }
            Some((
                Statement::Defer(DeferStmt {
                    body: Box::new(inner),
                }),
                block_shaped,
            ))
        }
        TokenKind::Return => Some((Statement::Return(parse_return(p)?), false)),
        TokenKind::Break => {
            p.advance();
            Some((Statement::Break, false))
        }
        TokenKind::Continue => {
            p.advance();
            Some((Statement::Continue, false))
        }
        TokenKind::Ident(_) if p.at_contextual(contextual::ASM) && asm_statement_follows(p) => {
            Some((Statement::InlineAsm(parse_inline_asm(p)?), true))
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Dollar) => {
            let mark = p.mark();
            let inv = parse_macro_invocation(p)?;
            if p.check(&TokenKind::Semi) {
                return Some((Statement::MacroInvocation(inv), false));
            }
            p.reset(mark);
            let expr = parse_statement_leading_expression(p)?;
            Some((Statement::Expression(expr), false))
        }
        _ => {
            let expr = parse_statement_leading_expression(p)?;
            let block_shaped = matches!(
                expr.expression,
                Expression::Codeblock(_) | Expression::If(_) | Expression::Match(_)
            );
            Some((Statement::Expression(expr), block_shaped))
        }
    }
}

fn reject_local_type_decl(p: &mut Parser, kind: ParseErrorKind) {
    p.error(kind);
    p.advance(); // 'struct'/'enum'
    while !matches!(
        p.peek(),
        TokenKind::LBrace | TokenKind::RBrace | TokenKind::Eof
    ) {
        p.advance();
    }
    if p.check(&TokenKind::LBrace) {
        recovery::skip_balanced_group(p);
    }
}

fn parse_walrus_or_declaration(
    p: &mut Parser,
    mutable: bool,
    comp: bool,
) -> Option<(Statement, bool)> {
    match p.peek_at(1) {
        TokenKind::ColonEq => {
            let mut w = parse_walrus(p)?;
            w.mutable = mutable;
            w.comp = comp;
            Some((Statement::Walrus(w), false))
        }
        _ => {
            if comp {
                p.error(ParseErrorKind::Expected {
                    expected: "':=' ('comp' only supports inferred bindings)",
                    found: p.peek_at(1).describe(),
                });
                return None;
            }
            let mut decl = parse_declaration(p)?;
            decl.mutable = mutable;
            if p.eat(&TokenKind::Eq) {
                let value = parse_expression(p)?;
                Some((Statement::DeclarationWithInit(decl, value), false))
            } else {
                Some((Statement::Declaration(decl), false))
            }
        }
    }
}

pub fn parse_declaration(p: &mut Parser) -> Option<DeclarationStmt> {
    let (ident, origin) = p.expect_ident_with_origin()?;
    let name_span = p.last_span();
    p.expect(&TokenKind::Colon, "':'");
    let r#type = crate::parser::r#type::parse_type(p)?;
    Some(DeclarationStmt {
        ident,
        name_span,
        span: name_span.to(p.last_span()),
        origin,
        r#type,
        mutable: false,
        visibility: Visibility::default(),
        explicit_hidden_span: None,
    })
}

fn parse_return(p: &mut Parser) -> Option<ReturnStmt> {
    p.expect(&TokenKind::Return, "'return'");
    let return_value = parse_expression(p)?;
    Some(ReturnStmt { return_value })
}

fn parse_walrus(p: &mut Parser) -> Option<WalrusStmt> {
    let (ident, origin) = p.expect_ident_with_origin()?;
    p.expect(&TokenKind::ColonEq, "':='");
    let value = parse_expression(p)?;
    Some(WalrusStmt {
        ident,
        origin,
        value,
        mutable: false,
        comp: false,
        visibility: Visibility::default(),
    })
}

fn parse_while(p: &mut Parser) -> Option<WhileStmt> {
    p.expect(&TokenKind::While, "'while'");
    // Struct literals are restricted in condition position -- `while flag
    // { ... }` must mean "condition `flag`, then the body"; see
    // `Parser::restrict_struct_literals`.
    let condition = p.restrict_struct_literals(parse_expression)?;
    let body = parse_codeblock(p)?;
    Some(WhileStmt { condition, body })
}

fn parse_loop(p: &mut Parser) -> Option<LoopStmt> {
    p.expect(&TokenKind::Loop, "'loop'");
    let body = parse_codeblock(p)?;
    Some(LoopStmt { body })
}

fn parse_for(p: &mut Parser) -> Option<Statement> {
    p.expect(&TokenKind::For, "'for'");

    if is_for_in_lookahead(p) {
        return parse_for_in(p).map(|f| Statement::ForIn(Box::new(f)));
    }

    let init = parse_for_init(p);
    if !p.expect_terminator(&TokenKind::Semi, "';'") {
        return recover_for_header(p).map(|f| Statement::For(Box::new(f)));
    }
    // The whole `cond; post` header shares the same body-`{` ambiguity an
    // `if`/`while` condition has, so struct literals are restricted in both
    // clauses (the init clause needs no restriction -- its own `;` always
    // separates it from the body -- but restricting uniformly from here on
    // costs nothing and reads simpler).
    let condition = if p.check(&TokenKind::Semi) {
        None
    } else {
        p.restrict_struct_literals(parse_expression)
    };
    if !p.expect_terminator(&TokenKind::Semi, "';'") {
        return recover_for_header(p).map(|f| Statement::For(Box::new(f)));
    }
    let post = if p.check(&TokenKind::LBrace) {
        None
    } else {
        p.restrict_struct_literals(parse_expression)
    };
    let Some(body) = parse_codeblock(p) else {
        return recover_for_header(p).map(|f| Statement::For(Box::new(f)));
    };
    Some(Statement::For(Box::new(ForStmt {
        init,
        condition,
        post,
        body,
    })))
}

fn is_for_in_lookahead(p: &Parser) -> bool {
    let offset = usize::from(p.at_contextual(contextual::MUT));
    matches!(p.peek_at(offset), TokenKind::Ident(_))
        && (p.at_contextual_at(offset + 1, contextual::IN)
            || for_in_annotation_follows(p, offset + 1))
}

fn for_in_annotation_follows(p: &Parser, colon_offset: usize) -> bool {
    if !matches!(p.peek_at(colon_offset), TokenKind::Colon) {
        return false;
    }
    let mut offset = colon_offset + 1;
    let mut depth = 0usize;
    loop {
        match p.peek_at(offset) {
            TokenKind::Lt | TokenKind::LBracket | TokenKind::LParen => depth += 1,
            TokenKind::Gt | TokenKind::RBracket | TokenKind::RParen if depth > 0 => depth -= 1,
            TokenKind::Semi | TokenKind::LBrace | TokenKind::Eof => return false,
            TokenKind::Ident(name) if depth == 0 && name == contextual::IN => return true,
            _ => {}
        }
        offset += 1;
    }
}

fn parse_for_in(p: &mut Parser) -> Option<ForInStmt> {
    let mutable = p.eat_contextual(contextual::MUT);
    let TokenKind::Ident(binding) = p.peek().clone() else {
        unreachable!("is_for_in_lookahead already confirmed this token is an identifier");
    };
    p.advance(); // binding
    let binding = Ident(binding);
    let binding_type = if p.eat(&TokenKind::Colon) {
        Some(crate::parser::r#type::parse_type(p)?)
    } else {
        None
    };
    p.expect_contextual(contextual::IN);

    // Same body-`{` ambiguity `while`/the classic `for`'s own condition
    // clause has -- restricted for the same reason. Ranges are ordinary
    // expressions here, as in every other expression position.
    let iterator = p.restrict_struct_literals(parse_range_or_expression)?;
    let body = parse_codeblock(p)?;
    Some(ForInStmt {
        mutable,
        binding,
        binding_type,
        iterator,
        body,
    })
}

fn parse_for_init(p: &mut Parser) -> Option<Statement> {
    if p.check(&TokenKind::Semi) {
        return None;
    }
    if let Some(prefix) = crate::parser::parse_binding_modifiers(p) {
        return parse_walrus_or_declaration(p, prefix.mutable, prefix.comp)
            .map(|(statement, _)| statement);
    }
    parse_expression(p).map(Statement::Expression)
}

/// Confirms the committed `asm(...) => {` shape by structurally matching the
/// balanced descriptor-list parens and checking what follows them, mirroring
/// the lexer's own `at_asm_body_open` check that decided whether the `{`
/// afterward opened a raw-capture asm body.
fn asm_statement_follows(p: &Parser) -> bool {
    if !matches!(p.peek_at(1), TokenKind::LParen) {
        return false;
    }
    let mut depth = 0i32;
    let mut offset = 1usize;
    loop {
        match p.peek_at(offset) {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return matches!(p.peek_at(offset + 1), TokenKind::FatArrow)
                        && matches!(p.peek_at(offset + 2), TokenKind::LBrace);
                }
            }
            TokenKind::Eof => return false,
            _ => {}
        }
        offset += 1;
    }
}

fn parse_inline_asm(p: &mut Parser) -> Option<InlineAsmStmt> {
    p.expect_contextual(contextual::ASM);
    p.expect(&TokenKind::LParen, "'('");
    let mut descriptors = Vec::new();
    if !p.check(&TokenKind::RParen) {
        loop {
            descriptors.push(parse_asm_descriptor(p)?);
            if !p.eat(&TokenKind::Comma) || p.check(&TokenKind::RParen) {
                break;
            }
        }
    }
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    p.expect(&TokenKind::LBrace, "'{'");
    let body_span = p.peek_span();
    let body = if let TokenKind::AsmBody(_) = p.peek() {
        let TokenKind::AsmBody(text) = p.advance().kind else {
            unreachable!()
        };
        text
    } else {
        p.error(ParseErrorKind::Expected {
            expected: "raw assembly text",
            found: p.peek().describe(),
        });
        String::new()
    };
    p.expect(&TokenKind::RBrace, "'}'");
    Some(InlineAsmStmt {
        descriptors,
        body,
        body_span,
    })
}

fn parse_asm_descriptor(p: &mut Parser) -> Option<AsmDescriptorNode> {
    let start = p.peek_span();
    match p.peek() {
        TokenKind::Ident(_) if p.at_contextual(contextual::REG) => {
            p.advance();
            p.expect(&TokenKind::LParen, "'('");
            let expr = parse_expression(p)?;
            let physical = if p.eat(&TokenKind::Comma) {
                Some(expect_asm_string_literal(p)?)
            } else {
                None
            };
            p.expect(&TokenKind::RParen, "')'");
            Some(AsmDescriptorNode {
                kind: AsmDescriptorKind::Reg { expr, physical },
                span: start.to(p.last_span()),
            })
        }
        TokenKind::Ident(_) if p.at_contextual(contextual::COMP) => {
            p.advance();
            p.expect(&TokenKind::LParen, "'('");
            let (name, origin) = p.expect_ident_with_origin()?;
            p.expect(&TokenKind::RParen, "')'");
            Some(AsmDescriptorNode {
                kind: AsmDescriptorKind::Comp { name, origin },
                span: start.to(p.last_span()),
            })
        }
        TokenKind::Ident(_) if p.at_contextual(contextual::CLOBBER) => {
            p.advance();
            p.expect(&TokenKind::LParen, "'('");
            let register = expect_asm_string_literal(p)?;
            p.expect(&TokenKind::RParen, "')'");
            Some(AsmDescriptorNode {
                kind: AsmDescriptorKind::Clobber { register },
                span: start.to(p.last_span()),
            })
        }
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: "'reg', 'comp', or 'clobber'",
                found: p.peek().describe(),
            });
            None
        }
    }
}

fn expect_asm_string_literal(p: &mut Parser) -> Option<String> {
    if let TokenKind::Str(_) = p.peek() {
        let TokenKind::Str(s) = p.advance().kind else {
            unreachable!()
        };
        Some(s)
    } else {
        p.error(ParseErrorKind::Expected {
            expected: "a string literal",
            found: p.peek().describe(),
        });
        None
    }
}

fn recover_for_header(p: &mut Parser) -> Option<ForStmt> {
    let mut depth = 0usize;
    loop {
        match p.peek() {
            TokenKind::Eof => return None,
            TokenKind::LBrace if depth == 0 => {
                recovery::skip_balanced_group(p);
                return None;
            }
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                depth += 1;
                p.advance();
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                p.advance();
            }
            TokenKind::Foreign | TokenKind::Import | TokenKind::Macro if depth == 0 => return None,
            _ => {
                p.advance();
            }
        }
    }
}

#[cfg(test)]
mod tests;
