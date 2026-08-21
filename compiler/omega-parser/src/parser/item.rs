use crate::ast::annotation::AnnotationNode;
use crate::ast::item::{ImportRoot, ImportStmt, Item, ItemNode};
use crate::ast::visibility::Visibility;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::macro_syntax::{parse_macro_definition, parse_macro_invocation};
use crate::parser::statement::parse_extern_declaration;
use crate::parser::{Parser, contextual, parse_path, recovery};

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
mod functions;

use annotations::{parse_annotations, reject_annotations};
use definitions::{parse_conform_def, parse_gap_def, parse_glue_def, parse_primitive_def};
pub use definitions::{
    parse_enum_def, parse_marker_def, parse_spec_def, parse_struct_def, parse_union_def,
};
pub use functions::parse_function_definition;
use functions::{parse_declaration_or_function_definition, parse_item_declaration_or_walrus};
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
        TokenKind::Extern => {
            reject_annotations(p, &annotations);
            let mut decl = parse_extern_declaration(p)?;
            decl.visibility = visibility;
            decl.explicit_hidden_span = parsed_visibility.explicit_hidden_span();
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::ExternDeclaration(decl)
        }
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
        TokenKind::Ident(name)
            if name == contextual::CONFORM
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

fn parse_import(p: &mut Parser, annotations: Vec<AnnotationNode>) -> Option<ImportStmt> {
    p.expect(&TokenKind::Import, "'import'");
    let reveal = p.eat_contextual(contextual::REVEAL);
    let root = if p.check(&TokenKind::Extern) && matches!(p.peek_at(1), TokenKind::ColonColon) {
        p.advance(); // 'extern'
        p.advance(); // '::'
        ImportRoot::Extern
    } else if p.at_contextual(contextual::ROOT) && matches!(p.peek_at(1), TokenKind::ColonColon) {
        p.advance(); // 'root'
        p.advance(); // '::'
        ImportRoot::ProjectRoot
    } else {
        ImportRoot::Local
    };
    let path = parse_path(p)?;
    p.expect_terminator(&TokenKind::Semi, "';'");
    Some(ImportStmt {
        annotations,
        reveal,
        root,
        path,
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
