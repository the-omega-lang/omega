use super::annotations::parse_annotations;
use super::functions::parse_optional_generics;
use crate::ast::annotation::AnnotationNode;
use crate::ast::item::{
    ForeignBindingItem, ForeignBlockEntry, ForeignBlockItem, ForeignFunctionItem, Item,
};
use crate::ast::r#type::RawConvention;
use crate::ast::visibility::Visibility;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::expression::parse_codeblock;
use crate::parser::r#type::{parse_raw_convention, parse_type};
use crate::parser::{Parser, parse_param_decls, recovery};

pub(super) fn parse_foreign_item(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<Item> {
    p.expect(&TokenKind::Foreign, "'foreign'");
    let convention = parse_optional_convention(p)?;

    if p.check(&TokenKind::LBrace) {
        return Some(Item::ForeignBlock(parse_foreign_block(p, convention)?));
    }

    let ident = p.expect_ident()?;
    let name_span = p.last_span();

    if p.check(&TokenKind::Colon) {
        if let Some(convention) = &convention {
            p.error_at(convention.span, ParseErrorKind::ForeignConventionOnBinding);
        }
        return Some(Item::ForeignBinding(parse_foreign_binding_tail(
            p,
            annotations,
            visibility,
            explicit_hidden_span,
            ident,
            name_span,
        )?));
    }

    Some(Item::ForeignFunction(parse_foreign_function_tail(
        p,
        annotations,
        visibility,
        explicit_hidden_span,
        convention,
        ident,
        name_span,
    )?))
}

fn parse_optional_convention(p: &mut Parser) -> Option<Option<RawConvention>> {
    if p.check(&TokenKind::LParen) {
        Some(Some(parse_raw_convention(p)?))
    } else {
        Some(None)
    }
}

fn parse_foreign_binding_tail(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
    ident: crate::ast::identifier::Ident,
    name_span: Span,
) -> Option<ForeignBindingItem> {
    p.expect(&TokenKind::Colon, "':'");
    let r#type = parse_type(p)?;
    p.expect_terminator(&TokenKind::Semi, "';'");
    Some(ForeignBindingItem {
        annotations,
        visibility,
        explicit_hidden_span,
        ident,
        name_span,
        r#type,
    })
}

fn parse_foreign_function_tail(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
    convention: Option<RawConvention>,
    ident: crate::ast::identifier::Ident,
    name_span: Span,
) -> Option<ForeignFunctionItem> {
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LParen, "'('");
    let params = parse_param_decls(p);
    let is_variadic = if p.eat(&TokenKind::Comma) {
        p.expect(&TokenKind::DotDotDot, "'...'");
        true
    } else {
        false
    };
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let return_type_start = p.peek_span();
    let return_type = parse_type(p)?;
    let return_type_span = return_type_start.to(p.last_span());
    let signature_span = name_span.to(return_type_span);
    let body = if p.check(&TokenKind::LBrace) {
        Some(parse_codeblock(p)?)
    } else {
        p.expect_terminator(&TokenKind::Semi, "';'");
        None
    };
    Some(ForeignFunctionItem {
        annotations,
        visibility,
        explicit_hidden_span,
        convention,
        ident,
        name_span,
        signature_span,
        return_type_span,
        generics,
        params,
        is_variadic,
        return_type,
        body,
    })
}

fn parse_foreign_block(
    p: &mut Parser,
    convention: Option<RawConvention>,
) -> Option<ForeignBlockItem> {
    p.expect(&TokenKind::LBrace, "'{'");
    let mut entries = Vec::new();
    while !p.check(&TokenKind::RBrace) && !p.is_eof() {
        match parse_foreign_block_entry(p, convention.clone()) {
            Some(entry) => entries.push(entry),
            None => recovery::synchronize_to_item_boundary(p),
        }
    }
    p.expect(&TokenKind::RBrace, "'}'");
    Some(ForeignBlockItem {
        convention,
        entries,
    })
}

fn parse_foreign_block_entry(
    p: &mut Parser,
    block_convention: Option<RawConvention>,
) -> Option<ForeignBlockEntry> {
    let annotations = parse_annotations(p);
    let parsed_visibility = super::parse_optional_visibility(p);
    let visibility = parsed_visibility.value();
    let explicit_hidden_span = parsed_visibility.explicit_hidden_span();

    if p.check(&TokenKind::Foreign) {
        p.error(ParseErrorKind::NestedForeignBlock);
        // Recovery synchronizes to the next item boundary, and `Foreign` is
        // itself a boundary token -- without consuming it here, recovery
        // would stop immediately on the same token and the block's entry
        // loop would re-enter this branch forever.
        p.advance();
        return None;
    }

    let ident = p.expect_ident()?;
    let name_span = p.last_span();

    if p.check(&TokenKind::Colon) {
        return Some(ForeignBlockEntry::Binding(parse_foreign_binding_tail(
            p,
            annotations,
            visibility,
            explicit_hidden_span,
            ident,
            name_span,
        )?));
    }

    Some(ForeignBlockEntry::Function(parse_foreign_function_tail(
        p,
        annotations,
        visibility,
        explicit_hidden_span,
        block_convention,
        ident,
        name_span,
    )?))
}
