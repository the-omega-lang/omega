use super::annotations::parse_annotations;
use super::functions::{parse_function_definition, parse_optional_generics};
use super::{ParsedVisibility, parse_optional_visibility};
use crate::ast::annotation::AnnotationNode;
use crate::ast::item::{
    ConformStmt, EnumHeaderField, EnumStmt, EnumVariantStmt, GapStmt, GlueStmt, PrimitiveStmt,
    SpecFunctionStmt, SpecStmt, StructStmt, UnionStmt,
};
use crate::ast::statement::{DeclarationStmt, FunctionDefinitionStmt};
use crate::ast::visibility::Visibility;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::expression::parse_codeblock;
use crate::parser::statement::parse_declaration;
use crate::parser::{Parser, contextual, parse_path, recovery};

pub fn parse_struct_def(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<StructStmt> {
    p.expect(&TokenKind::Struct, "'struct'");
    parse_struct_or_marker_body(
        p,
        annotations,
        visibility,
        explicit_hidden_span,
        StructKind::Ordinary,
    )
}

pub fn parse_marker_def(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<StructStmt> {
    p.advance(); // 'marker' -- contextual keyword, already confirmed by the caller's lookahead
    parse_struct_or_marker_body(
        p,
        annotations,
        visibility,
        explicit_hidden_span,
        StructKind::Marker,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StructKind {
    Ordinary,
    Marker,
}

fn parse_struct_or_marker_body(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
    kind: StructKind,
) -> Option<StructStmt> {
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

    let fields = match kind {
        StructKind::Ordinary => parse_aggregate_fields(p),
        StructKind::Marker => Vec::new(),
    };
    let functions = parse_member_functions(p, MemberVisibility::Allowed);

    p.expect(&TokenKind::RBrace, "'}'");
    Some(StructStmt {
        annotations,
        visibility,
        explicit_hidden_span,
        ident,
        generics,
        fields,
        functions,
        is_marker: kind == StructKind::Marker,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberVisibility {
    Allowed,
    InheritedFromSpec,
}

fn parse_aggregate_fields(p: &mut Parser) -> Vec<DeclarationStmt> {
    let mut fields = Vec::new();
    while field_follows(p) {
        let parsed_visibility = parse_optional_visibility(p);
        match parse_declaration(p) {
            Some(mut decl) => {
                decl.visibility = parsed_visibility.value();
                decl.explicit_hidden_span = parsed_visibility.explicit_hidden_span();
                fields.push(decl);
                p.expect_terminator(&TokenKind::Semi, "';'");
            }
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    fields
}

fn parse_member_functions(
    p: &mut Parser,
    visibility_policy: MemberVisibility,
) -> Vec<FunctionDefinitionStmt> {
    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) || p.check(&TokenKind::At) {
        let annotations = parse_annotations(p);
        if p.check(&TokenKind::RBrace) && !annotations.is_empty() {
            p.error_at(annotations[0].span, ParseErrorKind::AnnotationWithoutItem);
            break;
        }
        let parsed_visibility = parse_optional_visibility(p);
        let (visibility, explicit_hidden_span) = match visibility_policy {
            MemberVisibility::Allowed => (
                parsed_visibility.value(),
                parsed_visibility.explicit_hidden_span(),
            ),
            MemberVisibility::InheritedFromSpec => {
                if let ParsedVisibility::Explicit { span, .. } = parsed_visibility {
                    p.error_at(span, ParseErrorKind::ConformMethodVisibility);
                }
                (Visibility::Hidden, None)
            }
        };
        match parse_function_definition(p, annotations, visibility, explicit_hidden_span) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    functions
}

fn field_follows(p: &Parser) -> bool {
    // A field named `exposed`/`shared` (`exposed: i32;`) is a field, not a
    // modifier with a missing name -- so the no-modifier reading is tried
    // too, matching `parse_optional_visibility`'s own commit rule.
    let modifier_offset = match p.peek() {
        TokenKind::Ident(name)
            if name == contextual::EXPOSED
                || name == contextual::SHARED
                || name == contextual::HIDDEN =>
        {
            1
        }
        _ => 0,
    };
    [modifier_offset, 0].into_iter().any(|offset| {
        matches!(p.peek_at(offset), TokenKind::Ident(_))
            && matches!(p.peek_at(offset + 1), TokenKind::Colon)
    })
}

pub fn parse_union_def(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<UnionStmt> {
    p.expect(&TokenKind::Union, "'union'");
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

    let fields = parse_aggregate_fields(p);
    let functions = parse_member_functions(p, MemberVisibility::Allowed);

    p.expect(&TokenKind::RBrace, "'}'");
    Some(UnionStmt {
        annotations,
        visibility,
        explicit_hidden_span,
        ident,
        generics,
        fields,
        functions,
    })
}

pub fn parse_spec_def(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<SpecStmt> {
    p.expect(&TokenKind::Spec, "'spec'");
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;

    if p.eat(&TokenKind::Eq) {
        let mut dependencies = vec![crate::parser::r#type::parse_type(p)?];
        while p.eat(&TokenKind::Plus) {
            dependencies.push(crate::parser::r#type::parse_type(p)?);
        }
        if p.check(&TokenKind::LBrace) {
            p.error(ParseErrorKind::SpecAliasCannotDeclareFunctions);
            recovery::skip_balanced_group(p);
        } else {
            p.expect_terminator(&TokenKind::Semi, "';'");
        }
        return Some(SpecStmt {
            ident,
            visibility,
            explicit_hidden_span,
            generics,
            dependencies,
            functions: Vec::new(),
            is_alias: true,
            annotations,
        });
    }

    // `:` here is the removed provisioning form (`spec X : A, B`) --
    // reported with a dedicated error naming both replacements rather than
    // left to the ordinary "'{'" expectation, then skipped so the rest of
    // the file still parses.
    if p.check(&TokenKind::Colon) {
        p.error(ParseErrorKind::SpecDependenciesRemoved);
        while matches!(
            p.peek(),
            TokenKind::Colon | TokenKind::Comma | TokenKind::Ident(_) | TokenKind::Lt
        ) {
            p.advance();
        }
    }
    p.expect(&TokenKind::LBrace, "'{'");
    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) {
        match parse_spec_function(
            p,
            SpecMemberVisibility::Allowed {
                spec_visibility: visibility,
            },
        ) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    p.expect(&TokenKind::RBrace, "'}'");
    Some(SpecStmt {
        ident,
        visibility,
        explicit_hidden_span,
        generics,
        dependencies: Vec::new(),
        functions,
        is_alias: false,
        annotations,
    })
}

#[derive(Clone, Copy)]
enum SpecMemberVisibility {
    Allowed { spec_visibility: Visibility },
    Rejected,
}

fn parse_spec_function(p: &mut Parser, policy: SpecMemberVisibility) -> Option<SpecFunctionStmt> {
    let parsed_visibility = parse_optional_visibility(p);
    let visibility = match policy {
        SpecMemberVisibility::Allowed { spec_visibility } => match parsed_visibility {
            ParsedVisibility::Explicit { visibility, span } => {
                if visibility > spec_visibility {
                    p.error_at(
                        span,
                        ParseErrorKind::SpecMethodVisibilityExceedsSpec {
                            member_visibility: visibility,
                            spec_visibility,
                        },
                    );
                }
                visibility
            }
            // No modifier written -- a spec member inherits its spec's own
            // visibility by default, unlike every other declaration kind
            // (which defaults to `hidden`).
            ParsedVisibility::Hidden => spec_visibility,
        },
        SpecMemberVisibility::Rejected => {
            if let ParsedVisibility::Explicit { span, .. } = parsed_visibility {
                p.error_at(span, ParseErrorKind::GapOrGlueVisibility);
            }
            Visibility::Hidden
        }
    };
    let explicit_hidden_span = parsed_visibility.explicit_hidden_span();
    let ident = p.expect_ident()?;
    let name_span = p.last_span();
    p.expect(&TokenKind::LParen, "'('");
    let (self_mode, params) = crate::parser::parse_param_list(p);
    let is_variadic = if p.eat(&TokenKind::Comma) {
        p.expect(&TokenKind::DotDotDot, "'...'");
        true
    } else if p.eat(&TokenKind::DotDotDot) {
        // `parse_param_list` consumes the comma after a `self` mode before
        // discovering whether a following identifier exists, so `(*self,
        // ...)` reaches us positioned directly at `...`.
        true
    } else {
        false
    };
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let return_type_start = p.peek_span();
    let return_type = crate::parser::r#type::parse_type(p)?;
    let return_type_span = return_type_start.to(p.last_span());
    let signature_span = name_span.to(return_type_span);
    let body = if p.check(&TokenKind::LBrace) {
        Some(parse_codeblock(p)?)
    } else {
        p.expect_terminator(&TokenKind::Semi, "';'");
        None
    };
    Some(SpecFunctionStmt {
        ident,
        name_span,
        signature_span,
        return_type_span,
        visibility,
        explicit_hidden_span,
        self_mode,
        params,
        is_variadic,
        return_type,
        body,
    })
}

fn reject_gap_glue_generics(p: &mut Parser) -> Option<()> {
    if !p.check(&TokenKind::Lt) {
        return Some(());
    }
    p.error(ParseErrorKind::GapOrGlueGeneric);
    parse_optional_generics(p)?;
    Some(())
}

pub(super) fn parse_gap_def(p: &mut Parser) -> Option<GapStmt> {
    p.advance(); // contextual `gap`, confirmed by the caller
    let ident = p.expect_ident()?;
    reject_gap_glue_generics(p)?;
    p.expect(&TokenKind::LBrace, "'{'");
    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) {
        // Per-member recovery, matching every other item body (see
        // `parse_member_functions`): one malformed declaration reports one
        // error and the rest of the gap still parses. The loop always
        // consumes at least the leading identifier `parse_spec_function`
        // starts on, so recovery can never stall here.
        let Some(function) = parse_spec_function(p, SpecMemberVisibility::Rejected) else {
            recovery::synchronize_to_statement_boundary(p);
            continue;
        };
        if function.self_mode.is_some() {
            p.error_at(
                function.name_span,
                ParseErrorKind::GapFunctionSelf {
                    name: function.ident.clone(),
                },
            );
        }
        if let Some(body) = &function.body {
            p.error_at(
                body.span,
                ParseErrorKind::GapFunctionBody {
                    name: function.ident.clone(),
                },
            );
        }
        functions.push(function);
    }
    p.expect(&TokenKind::RBrace, "'}'");
    Some(GapStmt { ident, functions })
}

pub(super) fn parse_glue_def(p: &mut Parser) -> Option<GlueStmt> {
    p.advance(); // contextual `glue`, confirmed by the caller
    let gap = parse_path(p)?;
    reject_gap_glue_generics(p)?;
    p.expect(&TokenKind::LBrace, "'{'");
    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) {
        // Per-member recovery, same rule as `parse_gap_def` above.
        let Some(function) = parse_function_definition(p, Vec::new(), Visibility::Hidden, None)
        else {
            recovery::synchronize_to_statement_boundary(p);
            continue;
        };
        if !function.generics.is_empty() || function.self_mode.is_some() {
            p.error_at(
                function.name_span,
                ParseErrorKind::GlueFunctionShape {
                    name: function.ident.clone(),
                },
            );
        }
        functions.push(function);
    }
    p.expect(&TokenKind::RBrace, "'}'");
    Some(GlueStmt { gap, functions })
}

pub(super) fn parse_conform_def(p: &mut Parser) -> Option<ConformStmt> {
    p.advance();
    let generics = parse_optional_generics(p)?;
    let target = crate::parser::r#type::parse_type(p)?;
    // `to` is contextual: only this conformance position gives it keyword meaning.
    p.expect_contextual(crate::parser::contextual::TO);
    let spec = crate::parser::r#type::parse_type(p)?;
    p.expect(&TokenKind::LBrace, "'{'");
    let functions = parse_member_functions(p, MemberVisibility::InheritedFromSpec);
    p.expect(&TokenKind::RBrace, "'}'");
    Some(ConformStmt {
        generics,
        target,
        spec,
        functions,
    })
}

pub(super) fn parse_primitive_def(p: &mut Parser) -> Option<PrimitiveStmt> {
    p.advance();
    let generics = parse_optional_generics(p)?;
    let target = crate::parser::r#type::parse_type(p)?;
    p.expect(&TokenKind::LBrace, "'{'");
    let functions = parse_member_functions(p, MemberVisibility::Allowed);
    p.expect(&TokenKind::RBrace, "'}'");
    Some(PrimitiveStmt {
        generics,
        target,
        functions,
    })
}

pub fn parse_enum_def(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    explicit_hidden_span: Option<Span>,
) -> Option<EnumStmt> {
    p.expect(&TokenKind::Enum, "'enum'");
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    let header = parse_enum_header(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

    // The optional shared-dynamic-fields section -- same lookahead and loop
    // body `parse_struct_def`'s field loop uses, just spliced here, before
    // the variant list, instead of a struct's `{...}`.
    let dynamic_fields = parse_aggregate_fields(p);

    let mut variants = Vec::new();
    let mut functions_follow = false;
    while let TokenKind::Ident(_) = p.peek() {
        // A function definition where a variant is expected means the user
        // forgot (or misplaced) the `;` that ends the variant list -- report
        // exactly that, once, and hand the rest of the body to the function
        // loop below rather than mangling it as variants.
        if enum_function_follows(p) {
            p.error(ParseErrorKind::EnumFunctionBeforeSemi);
            functions_follow = true;
            break;
        }
        let variant = parse_enum_variant(p)?;
        let had_body = !variant.fields.is_empty();
        variants.push(variant);

        if p.eat(&TokenKind::Comma) {
            continue;
        }
        if p.eat(&TokenKind::Semi) {
            functions_follow = true;
            break;
        }
        if p.check(&TokenKind::RBrace) {
            break;
        }
        // A variant body closes itself, so no extra separator is required.
        if had_body && matches!(p.peek(), TokenKind::Ident(_)) {
            continue;
        }
        // A function definition right after a variant is the same missing-`;`
        // mistake the loop-top check catches -- report it identically and
        // let the function loop take over, instead of a generic separator
        // error.
        if matches!(p.peek(), TokenKind::Ident(_)) && enum_function_follows(p) {
            p.error(ParseErrorKind::EnumFunctionBeforeSemi);
            functions_follow = true;
            break;
        }
        p.error(ParseErrorKind::Expected {
            expected: "',', ';', or '}' after this variant",
            found: p.peek().describe(),
        });
        return None;
    }

    let mut functions = Vec::new();
    if functions_follow {
        functions = parse_member_functions(p, MemberVisibility::Allowed);
    }

    p.expect(&TokenKind::RBrace, "'}'");
    Some(EnumStmt {
        annotations,
        visibility,
        explicit_hidden_span,
        ident,
        generics,
        header,
        dynamic_fields,
        variants,
        functions,
    })
}

fn parse_enum_header(p: &mut Parser) -> Option<Vec<EnumHeaderField>> {
    let mut header = Vec::new();
    if !p.eat(&TokenKind::LParen) {
        return Some(header);
    }
    if !p.check(&TokenKind::RParen) {
        loop {
            let start = p.peek_span();
            let parsed_visibility = parse_optional_visibility(p);
            let decl = parse_declaration(p)?;
            let span = start.to(p.last_span());
            header.push(EnumHeaderField {
                ident: decl.ident,
                name_span: decl.name_span,
                r#type: decl.r#type,
                visibility: parsed_visibility.value(),
                explicit_hidden_span: parsed_visibility.explicit_hidden_span(),
                span,
            });
            if !p.eat(&TokenKind::Comma) {
                break;
            }
        }
    }
    p.expect(&TokenKind::RParen, "')'");
    Some(header)
}

fn parse_enum_variant(p: &mut Parser) -> Option<EnumVariantStmt> {
    let span = p.peek_span();
    let ident = p.expect_ident()?;

    let mut args = Vec::new();
    if p.eat(&TokenKind::LParen) {
        if !p.check(&TokenKind::RParen) {
            loop {
                args.push(p.allow_struct_literals(crate::parser::expression::parse_expression)?);
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        p.expect(&TokenKind::RParen, "')'");
    }

    let mut fields = Vec::new();
    if p.eat(&TokenKind::LBrace) {
        fields = parse_aggregate_fields(p);
        if !p.check(&TokenKind::RBrace) {
            p.error(ParseErrorKind::Expected {
                expected: "a field (`name: Type;`) or '}'",
                found: p.peek().describe(),
            });
            return None;
        }
        p.advance(); // '}'
    }

    Some(EnumVariantStmt {
        ident,
        span,
        args,
        fields,
    })
}

fn enum_function_follows(p: &Parser) -> bool {
    match p.peek_at(1) {
        TokenKind::Lt => true,
        TokenKind::LParen => {
            let mut depth = 0usize;
            let mut i = 1;
            loop {
                match p.peek_at(i) {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => {
                        depth -= 1;
                        if depth == 0 {
                            return matches!(p.peek_at(i + 1), TokenKind::FatArrow);
                        }
                    }
                    TokenKind::Eof => return false,
                    _ => {}
                }
                i += 1;
            }
        }
        _ => false,
    }
}
