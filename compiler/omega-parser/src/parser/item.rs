use crate::ast::annotation::AnnotationNode;
use crate::ast::identifier::{Ident, Path};
use crate::ast::item::{
    AliasItem, AliasTarget, ImportKind, ImportNode, ImportStmt, Item, ItemNode,
};
use crate::ast::r#type::Type;
use crate::ast::visibility::Visibility;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::macro_syntax::{parse_macro_definition, parse_macro_invocation};
use crate::parser::{Parser, contextual, parse_path_anchor, recovery};

#[derive(Clone, Copy)]
enum ParsedVisibility {
    Hidden,
    Explicit { visibility: Visibility, span: Span },
}

impl ParsedVisibility {
    fn value(self) -> Visibility {
        match self {
            Self::Hidden => Visibility::Hidden,
            Self::Explicit { visibility, .. } => visibility,
        }
    }

    fn explicit_span(self) -> Option<Span> {
        match self {
            Self::Hidden => None,
            Self::Explicit { span, .. } => Some(span),
        }
    }

    fn explicit_hidden_span(self) -> Option<Span> {
        match self {
            Self::Explicit {
                visibility: Visibility::Hidden,
                span,
            } => Some(span),
            _ => None,
        }
    }
}

mod annotations;
mod definitions;
mod foreign;
mod functions;

use annotations::{parse_annotations, reject_annotations};
use definitions::{parse_conform_def, parse_gap_def, parse_glue_def, parse_primitive_def};
pub use definitions::{
    parse_enum_def, parse_marker_def, parse_spec_def, parse_struct_def, parse_union_def,
};
use foreign::parse_foreign_item;
pub use functions::parse_function_definition;
use functions::{
    parse_declaration_or_function_definition, parse_item_declaration_or_walrus,
    parse_optional_generics,
};
pub fn parse_source_module(p: &mut Parser) -> Vec<ItemNode> {
    let mut nodes = Vec::new();
    while !p.is_eof() {
        match parse_item(p) {
            Some(node) => nodes.push(node),
            None => recovery::synchronize_to_item_boundary(p),
        }
    }
    nodes
}

pub fn parse_item(p: &mut Parser) -> Option<ItemNode> {
    let annotations = parse_annotations(p);
    let parsed_visibility = parse_optional_visibility(p);
    let visibility = parsed_visibility.value();
    let start = p.peek_span();

    if p.is_eof() && !annotations.is_empty() {
        p.error_at(annotations[0].span, ParseErrorKind::AnnotationWithoutItem);
        return None;
    }

    if let Some(prefix) = crate::parser::parse_binding_modifiers(p) {
        reject_annotations(p, &annotations);
        let item = parse_item_declaration_or_walrus(p, prefix.mutable, prefix.comp, visibility)?;
        let span = start.to(p.last_span());
        return Some(ItemNode { item, span });
    }

    let item = match p.peek() {
        TokenKind::Foreign => parse_foreign_item(
            p,
            annotations,
            visibility,
            parsed_visibility.explicit_hidden_span(),
        )?,
        TokenKind::Import => {
            reject_visibility(p, parsed_visibility);
            Item::Import(parse_import(p, annotations)?)
        }
        TokenKind::Struct => Item::Struct(parse_struct_def(
            p,
            annotations,
            visibility,
            parsed_visibility.explicit_hidden_span(),
        )?),
        TokenKind::Enum => Item::Enum(parse_enum_def(
            p,
            annotations,
            visibility,
            parsed_visibility.explicit_hidden_span(),
        )?),
        TokenKind::Union => Item::Union(parse_union_def(
            p,
            annotations,
            visibility,
            parsed_visibility.explicit_hidden_span(),
        )?),
        // Commit contextual `marker` only once the following identifier proves the item shape.
        TokenKind::Ident(name)
            if name == contextual::MARKER && matches!(p.peek_at(1), TokenKind::Ident(_)) =>
        {
            Item::Struct(parse_marker_def(
                p,
                annotations,
                visibility,
                parsed_visibility.explicit_hidden_span(),
            )?)
        }
        TokenKind::Spec => Item::Spec(parse_spec_def(
            p,
            annotations,
            visibility,
            parsed_visibility.explicit_hidden_span(),
        )?),
        TokenKind::Ident(name)
            if name == contextual::GAP && matches!(p.peek_at(1), TokenKind::Ident(_)) =>
        {
            reject_annotations(p, &annotations);
            reject_gap_glue_visibility(p, parsed_visibility);
            Item::Gap(parse_gap_def(p)?)
        }
        TokenKind::Ident(name)
            if name == contextual::GLUE && matches!(p.peek_at(1), TokenKind::Ident(_)) =>
        {
            reject_annotations(p, &annotations);
            reject_gap_glue_visibility(p, parsed_visibility);
            Item::Glue(parse_glue_def(p)?)
        }
        // The spec position after `meet` is always a path, so only a declaration
        // generic list or a path head can start a conformance. Widening this set
        // would steal `meet` from ordinary identifier use without ever matching a
        // well-formed declaration.
        TokenKind::Ident(name)
            if name == contextual::MEET
                && matches!(p.peek_at(1), TokenKind::Ident(_) | TokenKind::Lt) =>
        {
            reject_annotations(p, &annotations);
            reject_visibility(p, parsed_visibility);
            Item::Conform(parse_conform_def(p)?)
        }
        TokenKind::Ident(name)
            if name == contextual::PRIMITIVE
                && matches!(
                    p.peek_at(1),
                    TokenKind::Ident(_)
                        | TokenKind::Lt
                        | TokenKind::LBracket
                        | TokenKind::Star
                        | TokenKind::Spec
                ) =>
        {
            reject_annotations(p, &annotations);
            if let ParsedVisibility::Explicit { span, .. } = parsed_visibility {
                p.error_at(span, ParseErrorKind::PrimitiveVisibility);
            }
            Item::Primitive(parse_primitive_def(p)?)
        }
        TokenKind::Macro => {
            reject_annotations(p, &annotations);
            Item::MacroDefinition(parse_macro_definition(p, visibility)?)
        }
        TokenKind::Alias => {
            reject_annotations(p, &annotations);
            Item::Alias(parse_alias_def(
                p,
                visibility,
                parsed_visibility.explicit_hidden_span(),
            )?)
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Dollar) => {
            reject_annotations(p, &annotations);
            reject_visibility(p, parsed_visibility);
            let inv = parse_macro_invocation(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::MacroInvocation(inv)
        }
        TokenKind::Ident(_) => parse_declaration_or_function_definition(
            p,
            annotations,
            visibility,
            parsed_visibility.explicit_hidden_span(),
        )?,
        _ => {
            reject_visibility(p, parsed_visibility);
            p.error(ParseErrorKind::Expected {
                expected: "a top-level item",
                found: p.peek().describe(),
            });
            return None;
        }
    };
    let span = start.to(p.last_span());
    Some(ItemNode { item, span })
}

/// `alias Name<G...> = <type or path>;`. The right-hand side is parsed with
/// the ordinary type grammar, which is what rejects expression-shaped targets
/// without the parser needing to know what any name denotes. A bare
/// `Type::Named` is kept as `AliasTarget::Path` because a path may name a
/// module, macro, or function, none of which is a type.
fn parse_alias_def(
    p: &mut Parser,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<AliasItem> {
    p.expect(&TokenKind::Alias, "'alias'");
    let ident = p.expect_ident()?;
    let name_span = p.last_span();
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::Eq, "'='");
    let target_start = p.peek_span();
    let target = crate::parser::r#type::parse_type(p)?;
    let target_span = target_start.to(p.last_span());
    p.expect_terminator(&TokenKind::Semi, "';'");
    Some(AliasItem {
        visibility,
        explicit_hidden_span,
        ident,
        name_span,
        generics,
        target: match target {
            Type::Named(path) => AliasTarget::Path(path),
            other => AliasTarget::Type(other),
        },
        target_span,
    })
}

fn parse_import(p: &mut Parser, annotations: Vec<AnnotationNode>) -> Option<ImportStmt> {
    let start = p.peek_span();
    p.expect(&TokenKind::Import, "'import'");
    let reveal = p.eat_contextual(contextual::REVEAL);
    let anchor = parse_path_anchor(p);
    let (head, origin) = p.expect_ident_with_origin()?;
    let tail = parse_import_segments(p)?;
    let path = Path {
        anchor,
        head,
        tail,
        origin,
    };
    let kind = parse_import_kind(p)?;
    p.expect_terminator(&TokenKind::Semi, "';'");
    Some(ImportStmt {
        annotations,
        reveal,
        path,
        span: start.to(p.last_span()),
        kind,
    })
}

/// Path segments after the first one, stopping before a `::{` group so the
/// caller can attach it. An import prefix is otherwise an ordinary path.
fn parse_import_segments(p: &mut Parser) -> Option<Vec<Ident>> {
    let mut segments = Vec::new();
    while p.check(&TokenKind::ColonColon) && !matches!(p.peek_at(1), TokenKind::LBrace) {
        p.advance();
        segments.push(p.expect_ident()?);
    }
    Some(segments)
}

/// What follows a prefix: a `::{ ... }` group, an `as` rename, or nothing.
fn parse_import_kind(p: &mut Parser) -> Option<ImportKind> {
    if p.check(&TokenKind::ColonColon) {
        p.advance();
        return p.descend(parse_import_group).map(ImportKind::Group);
    }
    let as_span = p.peek_span();
    if !p.eat_contextual(contextual::AS) {
        return Some(ImportKind::Leaf { alias: None });
    }
    let alias = p.expect_ident()?;
    if p.check(&TokenKind::ColonColon) {
        // The `as` is the offending token, not the `::` that exposes it as a
        // prefix rename: the reader has to delete the rename, not the path.
        p.error_at(as_span, ParseErrorKind::ImportAliasOnPrefix);
        return None;
    }
    Some(ImportKind::Leaf { alias: Some(alias) })
}

fn parse_import_group(p: &mut Parser) -> Option<Vec<ImportNode>> {
    let open = p.peek_span();
    p.expect(&TokenKind::LBrace, "'{'");
    let mut entries = Vec::new();
    while !p.check(&TokenKind::RBrace) {
        if p.is_eof() {
            p.error_at(open, ParseErrorKind::UnterminatedGroup { open: '{' });
            return None;
        }
        entries.push(parse_import_entry(p)?);
        if !p.eat(&TokenKind::Comma) {
            break;
        }
    }
    p.expect(&TokenKind::RBrace, "'}'");
    if entries.is_empty() {
        p.error_at(open.to(p.last_span()), ParseErrorKind::EmptyImportGroup);
        return None;
    }
    Some(entries)
}

fn parse_import_entry(p: &mut Parser) -> Option<ImportNode> {
    let start = p.peek_span();
    let reveal = p.eat_contextual(contextual::REVEAL);
    let segments = if p.at_contextual(contextual::SELF) {
        if matches!(p.peek_at(1), TokenKind::ColonColon) {
            p.error_at(
                start.to(p.peek_span()),
                ParseErrorKind::ImportSelfNotTerminal,
            );
            return None;
        }
        p.advance();
        Vec::new()
    } else {
        let mut segments = vec![p.expect_ident()?];
        segments.extend(parse_import_segments(p)?);
        segments
    };
    let kind = parse_import_kind(p)?;
    Some(ImportNode {
        reveal,
        segments,
        span: start.to(p.last_span()),
        kind,
    })
}

fn parse_optional_visibility(p: &mut Parser) -> ParsedVisibility {
    // Commit contextual visibility only when a declaration shape follows; `exposed: T` is a field name.
    if matches!(p.peek_at(1), TokenKind::Colon | TokenKind::ColonEq) {
        return ParsedVisibility::Hidden;
    }
    let span = p.peek_span();
    match p.peek() {
        TokenKind::Ident(name) if name == contextual::EXPOSED => {
            p.advance();
            ParsedVisibility::Explicit {
                visibility: Visibility::Exposed,
                span,
            }
        }
        TokenKind::Ident(name) if name == contextual::SHARED => {
            p.advance();
            ParsedVisibility::Explicit {
                visibility: Visibility::Shared,
                span,
            }
        }
        TokenKind::Ident(name) if name == contextual::HIDDEN => {
            p.advance();
            ParsedVisibility::Explicit {
                visibility: Visibility::Hidden,
                span,
            }
        }
        _ => ParsedVisibility::Hidden,
    }
}

fn reject_visibility(p: &mut Parser, visibility: ParsedVisibility) {
    if let Some(span) = visibility.explicit_span() {
        p.error_at(span, ParseErrorKind::VisibilityNotAllowedHere);
    }
}

fn reject_gap_glue_visibility(p: &mut Parser, visibility: ParsedVisibility) {
    if let Some(span) = visibility.explicit_span() {
        p.error_at(span, ParseErrorKind::GapOrGlueVisibility);
    }
}
