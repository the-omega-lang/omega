use super::annotations::reject_annotations;
use crate::ast::annotation::AnnotationNode;
use crate::ast::generics::{GenericParam, GenericParamKind};
use crate::ast::item::Item;
use crate::ast::statement::{FunctionDefinitionStmt, WalrusStmt};
use crate::ast::visibility::Visibility;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::expression::{parse_codeblock, parse_expression};
use crate::parser::statement::parse_declaration;
use crate::parser::{Parser, contextual};

pub(super) fn parse_declaration_or_function_definition(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<Item> {
    match p.peek_at(1) {
        TokenKind::Lt | TokenKind::LParen => Some(Item::FunctionDefinition(
            parse_function_definition(p, annotations, visibility, explicit_hidden_span)?,
        )),
        _ => {
            reject_annotations(p, &annotations);
            let mut decl = parse_declaration(p)?;
            decl.visibility = visibility;
            if p.eat(&TokenKind::Eq) {
                let value = parse_expression(p)?;
                p.expect_terminator(&TokenKind::Semi, "';'");
                Some(Item::DeclarationWithInit(decl, value))
            } else {
                p.expect_terminator(&TokenKind::Semi, "';'");
                Some(Item::Declaration(decl))
            }
        }
    }
}

pub(super) fn parse_item_declaration_or_walrus(
    p: &mut Parser,
    mutable: bool,
    comp: bool,
    visibility: Visibility,
) -> Option<Item> {
    match p.peek_at(1) {
        TokenKind::ColonEq => Some(Item::Walrus(parse_item_walrus(
            p, mutable, comp, visibility,
        )?)),
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
            decl.visibility = visibility;
            if p.eat(&TokenKind::Eq) {
                let value = parse_expression(p)?;
                p.expect_terminator(&TokenKind::Semi, "';'");
                Some(Item::DeclarationWithInit(decl, value))
            } else {
                p.expect_terminator(&TokenKind::Semi, "';'");
                Some(Item::Declaration(decl))
            }
        }
    }
}

pub(super) fn parse_item_walrus(
    p: &mut Parser,
    mutable: bool,
    comp: bool,
    visibility: Visibility,
) -> Option<WalrusStmt> {
    let (ident, origin) = p.expect_ident_with_origin()?;
    p.expect(&TokenKind::ColonEq, "':='");
    let value = parse_expression(p)?;
    p.expect_terminator(&TokenKind::Semi, "';'");
    Some(WalrusStmt {
        ident,
        origin,
        value,
        mutable,
        comp,
        visibility,
    })
}

pub fn parse_function_definition(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<FunctionDefinitionStmt> {
    let ident = p.expect_ident()?;
    let name_span = p.last_span();
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LParen, "'('");
    let (self_mode, params) = crate::parser::parse_param_list(p);
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    // The whole declared type, not just its last token -- `=> *mut Node<T>`
    // must underline all of it, not the `T`.
    let return_type_start = p.peek_span();
    let return_type = crate::parser::r#type::parse_type(p)?;
    let return_type_span = return_type_start.to(p.last_span());
    let signature_span = name_span.to(return_type_span);
    let codeblock = parse_codeblock(p)?;
    Some(FunctionDefinitionStmt {
        annotations,
        visibility,
        explicit_hidden_span,
        ident,
        name_span,
        signature_span,
        return_type_span,
        generics,
        self_mode,
        params,
        return_type,
        codeblock,
    })
}

pub(super) fn parse_optional_generics(p: &mut Parser) -> Option<Vec<GenericParam>> {
    if !p.eat(&TokenKind::Lt) {
        return Some(Vec::new());
    }
    let mut seen_default = false;
    let mut generics = vec![parse_generic_param(p, &mut seen_default)?];
    while p.eat(&TokenKind::Comma) {
        generics.push(parse_generic_param(p, &mut seen_default)?);
    }
    p.expect_close_angle("'>'");
    Some(generics)
}

fn parse_generic_param(p: &mut Parser, seen_default: &mut bool) -> Option<GenericParam> {
    // `comp` is contextual, so it only introduces a value parameter when an
    // actual parameter name follows it; `<comp, T>` still declares a type
    // parameter spelled `comp`.
    let is_comp = p.at_contextual(contextual::COMP) && matches!(p.peek_at(1), TokenKind::Ident(_));
    if is_comp {
        p.advance();
    }
    let ident = p.expect_ident()?;
    let name_span = p.last_span();
    let kind = if is_comp {
        if !p.eat(&TokenKind::Colon) {
            p.error(ParseErrorKind::Expected {
                expected: "':' and the value type of a 'comp' generic parameter",
                found: p.peek().describe(),
            });
            return None;
        }
        GenericParamKind::Comp {
            value_type: crate::parser::r#type::parse_type(p)?,
        }
    } else {
        // `+` after a type in bound position is unambiguous -- no type
        // contains `+` -- so the whole `A + B + C` conjunction is parsed
        // greedily here.
        let bounds = if p.eat(&TokenKind::Colon) {
            let mut bounds = vec![crate::parser::r#type::parse_type(p)?];
            while p.eat(&TokenKind::Plus) {
                bounds.push(crate::parser::r#type::parse_type(p)?);
            }
            bounds
        } else {
            Vec::new()
        };
        GenericParamKind::Type { bounds }
    };
    let default = if p.eat(&TokenKind::Eq) {
        Some(crate::parser::r#type::parse_generic_arg(p)?)
    } else {
        None
    };
    if default.is_some() {
        *seen_default = true;
    } else if *seen_default {
        p.error_at(
            name_span,
            ParseErrorKind::DefaultGenericParamNotTrailing {
                name: ident.clone(),
            },
        );
    }
    Some(GenericParam {
        ident,
        kind,
        default,
    })
}
