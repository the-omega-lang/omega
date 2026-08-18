use crate::ast::annotation::{AnnotationArg, AnnotationNode, AnnotationValue};
use crate::ast::expression::NumberBase;
use crate::ast::generics::GenericParam;
use crate::ast::item::{
    ConformStmt, EnumHeaderField, EnumStmt, EnumVariantStmt, GapStmt, GlueStmt, ImportRoot,
    ImportStmt, Item, ItemNode, PrimitiveStmt, SpecFunctionStmt, SpecStmt, StructStmt, UnionStmt,
};
use crate::ast::statement::{DeclarationStmt, FunctionDefinitionStmt, WalrusStmt};
use crate::ast::visibility::Visibility;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::expression::{parse_codeblock, parse_expression};
use crate::parser::macro_syntax::{parse_macro_definition, parse_macro_invocation};
use crate::parser::statement::{parse_declaration, parse_extern_declaration};
use crate::parser::{Parser, contextual, parse_path, recovery};

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
    let (visibility, visibility_span) = parse_optional_visibility(p);
    let start = p.peek_span();

    if let Some(prefix) = crate::parser::parse_binding_prefix(p) {
        reject_annotations(p, &annotations);
        let item =
            parse_item_declaration_or_walrus(p, prefix.mutable, prefix.comp, visibility)?;
        let span = start.to(p.last_span());
        return Some(ItemNode { item, span });
    }

    let item = match p.peek() {
        TokenKind::Extern => {
            reject_annotations(p, &annotations);
            let mut decl = parse_extern_declaration(p)?;
            decl.visibility = visibility;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::ExternDeclaration(decl)
        }
        TokenKind::Import => {
            reject_visibility(p, visibility, visibility_span);
            p.advance();
            // Commit contextual `reveal` only in import position.
            let reveal = if p.at_contextual(contextual::REVEAL) {
                p.advance();
                true
            } else {
                false
            };
            // `root` is contextual here; `root` without `::` remains a normal module name.
            let root =
                if p.check(&TokenKind::Extern) && matches!(p.peek_at(1), TokenKind::ColonColon) {
                    p.advance(); // 'extern'
                    p.advance(); // '::'
                    ImportRoot::Extern
                } else if p.at_contextual(contextual::ROOT)
                    && matches!(p.peek_at(1), TokenKind::ColonColon)
                {
                    p.advance(); // 'root'
                    p.advance(); // '::'
                    ImportRoot::ProjectRoot
                } else {
                    ImportRoot::Local
                };
            let path = parse_path(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::Import(ImportStmt {
                annotations,
                reveal,
                root,
                path,
            })
        }
        TokenKind::Struct => Item::Struct(parse_struct_def(p, annotations, visibility)?),
        TokenKind::Enum => Item::Enum(parse_enum_def(p, annotations, visibility)?),
        TokenKind::Union => Item::Union(parse_union_def(p, annotations, visibility)?),
        // Commit contextual `marker` only once the following identifier proves the item shape.
        TokenKind::Ident(name)
            if name == contextual::MARKER && matches!(p.peek_at(1), TokenKind::Ident(_)) =>
        {
            Item::Struct(parse_marker_def(p, annotations, visibility)?)
        }
        TokenKind::Spec => Item::Spec(parse_spec_def(p, annotations, visibility)?),
        TokenKind::Ident(name) if name == contextual::GAP && matches!(p.peek_at(1), TokenKind::Ident(_)) => {
            reject_annotations(p, &annotations);
            reject_gap_glue_visibility(p, visibility, visibility_span);
            Item::Gap(parse_gap_def(p)?)
        }
        TokenKind::Ident(name) if name == contextual::GLUE && matches!(p.peek_at(1), TokenKind::Ident(_)) => {
            reject_annotations(p, &annotations);
            reject_gap_glue_visibility(p, visibility, visibility_span);
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
            reject_visibility(p, visibility, visibility_span);
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
            if visibility != Visibility::Hidden {
                p.error_at(
                    visibility_span.expect("non-hidden visibility has a span"),
                    ParseErrorKind::PrimitiveVisibility,
                );
            }
            Item::Primitive(parse_primitive_def(p)?)
        }
        TokenKind::Macro => {
            reject_annotations(p, &annotations);
            Item::MacroDefinition(parse_macro_definition(p, visibility)?)
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Dollar) => {
            reject_annotations(p, &annotations);
            reject_visibility(p, visibility, visibility_span);
            let inv = parse_macro_invocation(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::MacroInvocation(inv)
        }
        // Plain top-level walrus parses here; semantic analysis decides whether it is valid.
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::ColonEq) => {
            reject_annotations(p, &annotations);
            Item::Walrus(parse_item_walrus(p, false, false, visibility)?)
        }
        TokenKind::Ident(_) => {
            parse_declaration_or_function_definition(p, annotations, visibility)?
        }
        _ => {
            reject_visibility(p, visibility, visibility_span);
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

fn parse_optional_visibility(p: &mut Parser) -> (Visibility, Option<Span>) {
    // Commit contextual visibility only when a declaration shape follows; `exposed: T` is a field name.
    if matches!(p.peek_at(1), TokenKind::Colon | TokenKind::ColonEq) {
        return (Visibility::Hidden, None);
    }
    let span = p.peek_span();
    match p.peek() {
        TokenKind::Ident(name) if name == contextual::EXPOSED => {
            p.advance();
            (Visibility::Exposed, Some(span))
        }
        TokenKind::Ident(name) if name == contextual::INTERNAL => {
            p.advance();
            (Visibility::Internal, Some(span))
        }
        _ => (Visibility::Hidden, None),
    }
}

fn reject_visibility(p: &mut Parser, visibility: Visibility, span: Option<Span>) {
    if visibility != Visibility::Hidden {
        p.error_at(
            span.expect("non-Hidden visibility always has a span"),
            ParseErrorKind::VisibilityNotAllowedHere,
        );
    }
}

fn reject_gap_glue_visibility(p: &mut Parser, visibility: Visibility, span: Option<Span>) {
    if visibility != Visibility::Hidden {
        p.error_at(
            span.expect("non-Hidden visibility always has a span"),
            ParseErrorKind::GapOrGlueVisibility,
        );
    }
}

fn parse_annotations(p: &mut Parser) -> Vec<AnnotationNode> {
    let mut annotations = Vec::new();
    while p.check(&TokenKind::At) {
        match parse_annotation(p) {
            Some(annotation) => annotations.push(annotation),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    annotations
}

fn parse_annotation(p: &mut Parser) -> Option<AnnotationNode> {
    let start = p.peek_span();
    p.expect(&TokenKind::At, "'@'");
    let name = p.expect_ident()?;
    let mut args = Vec::new();
    if p.eat(&TokenKind::LParen) {
        if !p.check(&TokenKind::RParen) {
            loop {
                args.push(parse_annotation_arg(p)?);
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        p.expect(&TokenKind::RParen, "')'");
    }
    let span = start.to(p.last_span());
    Some(AnnotationNode { name, args, span })
}

fn parse_annotation_arg(p: &mut Parser) -> Option<AnnotationArg> {
    let ident = p.expect_ident()?;
    if !p.eat(&TokenKind::Eq) {
        return Some(AnnotationArg::Ident(ident));
    }
    match p.peek() {
        TokenKind::Number(n)
            if matches!(n.base, NumberBase::Decimal)
                && n.fractional_part.is_none()
                && n.explicit_type.is_none() =>
        {
            let value = n.integer_part.clone();
            p.advance();
            Some(AnnotationArg::KeyValue(
                ident,
                AnnotationValue::IntLiteral(value),
            ))
        }
        TokenKind::Ident(name) if name == contextual::SIZEOF => {
            p.advance(); // 'sizeof'
            p.advance(); // '<'
            let r#type = crate::parser::r#type::parse_type(p)?;
            p.expect_close_angle("'>'");
            Some(AnnotationArg::KeyValue(
                ident,
                AnnotationValue::Sizeof(r#type),
            ))
        }
        TokenKind::Str(_) => {
            let TokenKind::Str(s) = p.advance().kind else {
                unreachable!()
            };
            Some(AnnotationArg::KeyValue(
                ident,
                AnnotationValue::StrLiteral(s),
            ))
        }
        _ => {
            p.error(ParseErrorKind::Expected {
                expected: "a plain integer, 'sizeof<Type>', or a string literal",
                found: p.peek().describe(),
            });
            None
        }
    }
}

fn reject_annotations(p: &mut Parser, annotations: &[AnnotationNode]) {
    if let Some(first) = annotations.first() {
        p.error_at(first.span, ParseErrorKind::AnnotationNotAllowedHere);
    }
}

fn parse_declaration_or_function_definition(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
) -> Option<Item> {
    match p.peek_at(1) {
        TokenKind::Lt | TokenKind::LParen => Some(Item::FunctionDefinition(
            parse_function_definition(p, annotations, visibility)?,
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

fn parse_item_declaration_or_walrus(
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

fn parse_item_walrus(
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

fn parse_optional_generics(p: &mut Parser) -> Option<Vec<GenericParam>> {
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
    let ident = p.expect_ident()?;
    let name_span = p.last_span();
    // `+` after a type in bound position is unambiguous -- no type contains
    // `+` -- so the whole `A + B + C` conjunction is parsed greedily here.
    let bounds = if p.eat(&TokenKind::Colon) {
        let mut bounds = vec![crate::parser::r#type::parse_type(p)?];
        while p.eat(&TokenKind::Plus) {
            bounds.push(crate::parser::r#type::parse_type(p)?);
        }
        bounds
    } else {
        Vec::new()
    };
    let default = if p.eat(&TokenKind::Eq) {
        Some(crate::parser::r#type::parse_type(p)?)
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
        bounds,
        default,
    })
}

pub fn parse_struct_def(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
) -> Option<StructStmt> {
    p.expect(&TokenKind::Struct, "'struct'");
    parse_struct_or_marker_body(p, annotations, visibility, false)
}

pub fn parse_marker_def(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
) -> Option<StructStmt> {
    p.advance(); // 'marker' -- contextual keyword, already confirmed by the caller's lookahead
    parse_struct_or_marker_body(p, annotations, visibility, true)
}

fn parse_struct_or_marker_body(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    is_marker: bool,
) -> Option<StructStmt> {
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

    let fields = if is_marker { Vec::new() } else { parse_aggregate_fields(p) };
    let functions = parse_member_functions(p, MemberVisibility::Allowed);

    p.expect(&TokenKind::RBrace, "'}'");
    Some(StructStmt {
        annotations,
        visibility,
        ident,
        generics,
        fields,
        functions,
        is_marker,
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
        let (field_visibility, _) = parse_optional_visibility(p);
        match parse_declaration(p) {
            Some(mut decl) => {
                decl.visibility = field_visibility;
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
        let (visibility, visibility_span) = parse_optional_visibility(p);
        let visibility = match visibility_policy {
            MemberVisibility::Allowed => visibility,
            MemberVisibility::InheritedFromSpec => {
                if visibility != Visibility::Hidden {
                    p.error_at(
                        visibility_span.expect("non-hidden visibility has a span"),
                        ParseErrorKind::ConformMethodVisibility,
                    );
                }
                Visibility::Hidden
            }
        };
        match parse_function_definition(p, annotations, visibility) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    functions
}

fn field_follows(p: &Parser) -> bool {
    // A field named `exposed`/`internal` (`exposed: i32;`) is a field, not a
    // modifier with a missing name -- so the no-modifier reading is tried
    // too, matching `parse_optional_visibility`'s own commit rule.
    let modifier_offset = match p.peek() {
        TokenKind::Ident(name) if name == contextual::EXPOSED || name == contextual::INTERNAL => 1,
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
        match parse_spec_function(p) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    p.expect(&TokenKind::RBrace, "'}'");
    Some(SpecStmt {
        ident,
        visibility,
        generics,
        dependencies: Vec::new(),
        functions,
        is_alias: false,
        annotations,
    })
}

fn parse_spec_function(p: &mut Parser) -> Option<SpecFunctionStmt> {
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

fn parse_gap_def(p: &mut Parser) -> Option<GapStmt> {
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
        let Some(function) = parse_spec_function(p) else {
            recovery::synchronize_to_statement_boundary(p);
            continue;
        };
        if function.self_mode.is_some() {
            p.error_at(
                p.last_span(),
                ParseErrorKind::GapFunctionSelf {
                    name: function.ident.clone(),
                },
            );
        }
        if function.body.is_some() {
            p.error_at(
                p.last_span(),
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

fn parse_glue_def(p: &mut Parser) -> Option<GlueStmt> {
    p.advance(); // contextual `glue`, confirmed by the caller
    let gap = parse_path(p)?;
    reject_gap_glue_generics(p)?;
    p.expect(&TokenKind::LBrace, "'{'");
    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) {
        // Per-member recovery, same rule as `parse_gap_def` above.
        let Some(function) = parse_function_definition(p, Vec::new(), Visibility::Hidden) else {
            recovery::synchronize_to_statement_boundary(p);
            continue;
        };
        if !function.generics.is_empty() || function.self_mode.is_some() {
            p.error_at(
                p.last_span(),
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

fn parse_conform_def(p: &mut Parser) -> Option<ConformStmt> {
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

fn parse_primitive_def(p: &mut Parser) -> Option<PrimitiveStmt> {
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
            let (visibility, _) = parse_optional_visibility(p);
            let decl = parse_declaration(p)?;
            let span = start.to(p.last_span());
            header.push(EnumHeaderField {
                ident: decl.ident,
                name_span: decl.name_span,
                r#type: decl.r#type,
                visibility,
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
