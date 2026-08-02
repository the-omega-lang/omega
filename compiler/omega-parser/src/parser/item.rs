use crate::ast::annotation::{AnnotationArg, AnnotationNode, AnnotationValue};
use crate::ast::generics::GenericParam;
use crate::ast::self_mode::SelfMode;
use crate::ast::r#type::Type;
use crate::ast::statement::{
    Item, ItemNode, declaration::DeclarationStmt,
    r#enum::{EnumHeaderField, EnumStmt, EnumVariantStmt},
    function_definition::FunctionDefinitionStmt, import::{ImportRoot, ImportStmt},
    spec::{SpecFunctionStmt, SpecStmt}, r#struct::StructStmt,
    union::UnionStmt, walrus::WalrusStmt,
};
use crate::ast::visibility::Visibility;
use crate::diagnostics::{ParseErrorKind, Span};
use crate::lexer::TokenKind;
use crate::parser::expression::{parse_codeblock, parse_expression};
use crate::parser::macro_syntax::{parse_macro_definition, parse_macro_invocation};
use crate::parser::statement::{parse_declaration, parse_extern_declaration};
use crate::parser::{Parser, parse_path, recovery};

/// Parses a whole source file's top-level items, recovering after each
/// failed one (see `recovery::synchronize_to_item_boundary`) so a single
/// mistake reports one error and the rest of the file still gets checked,
/// rather than aborting on the first problem.
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

    // `mut`/`comp` are both contextual keywords here (see `lexer::
    // TokenKind`'s doc comment), each only recognized leading a binding
    // declaration -- never anywhere else, so both stay usable as ordinary
    // identifiers (and `comp` also as its own prefix *expression*, see
    // `parser::expression::parse_unary`) in every other position. Only
    // committed to once the *whole* `[mut] [comp] ident (':='/':')` shape
    // is confirmed below -- mirrors `parser::statement`'s identical
    // combined lookahead exactly. `mut comp x := ...;` parses (both flags
    // recognized) but is rejected during analysis (`AnalysisErrorKind::
    // MutCompBinding`), not here.
    let mut_offset = if matches!(p.peek(), TokenKind::Ident(name) if name == "mut") { 1 } else { 0 };
    let comp_offset = if matches!(p.peek_at(mut_offset), TokenKind::Ident(name) if name == "comp") { 1 } else { 0 };
    let ident_offset = mut_offset + comp_offset;
    if (mut_offset > 0 || comp_offset > 0)
        && matches!(p.peek_at(ident_offset), TokenKind::Ident(_))
        && matches!(p.peek_at(ident_offset + 1), TokenKind::ColonEq | TokenKind::Colon)
    {
        reject_annotations(p, &annotations);
        let mutable = mut_offset > 0;
        let comp = comp_offset > 0;
        if mutable {
            p.advance(); // 'mut'
        }
        if comp {
            p.advance(); // 'comp'
        }
        let item = parse_item_declaration_or_walrus(p, mutable, comp, visibility)?;
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
            // `reveal` is a contextual keyword here too (same text-
            // comparison pattern as `root`/`mut` below) -- see
            // `ImportStmt::reveal`'s doc comment.
            let reveal = if matches!(p.peek(), TokenKind::Ident(name) if name == "reveal") {
                p.advance();
                true
            } else {
                false
            };
            // `root::`/`extern::` are contextual keywords here (matching
            // `mut`'s own text-comparison pattern above, and `lexer::
            // TokenKind`'s general "stay a plain token, recognized by
            // position" philosophy) -- `extern` is already a real keyword
            // token, `root` an ordinary `Ident` whose text is checked; only
            // committed to when immediately followed by `::`, so a module
            // genuinely named `root` still parses as an ordinary `Local`
            // import (`import root;` alone, with no trailing `::`).
            let root = if p.check(&TokenKind::Extern) && matches!(p.peek_at(1), TokenKind::ColonColon) {
                p.advance(); // 'extern'
                p.advance(); // '::'
                ImportRoot::Extern
            } else if matches!(p.peek(), TokenKind::Ident(name) if name == "root")
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
            Item::Import(ImportStmt { annotations, reveal, root, path })
        }
        TokenKind::Struct => Item::Struct(parse_struct_def(p, annotations, visibility)?),
        TokenKind::Enum => Item::Enum(parse_enum_def(p, annotations, visibility)?),
        TokenKind::Union => Item::Union(parse_union_def(p, annotations, visibility)?),
        // `marker` is contextual, matching `mut`/`comp`/`reveal`'s own
        // precedent (`lexer::TokenKind`'s doc comment) -- only committed to
        // once followed by another identifier (the marker's own name),
        // exactly like `mut`/`comp` above are only committed to once the
        // *whole* binding shape is confirmed. This keeps `marker` usable as
        // an ordinary function/variable name everywhere else (in
        // particular, `marker(...)` -- a call/function definition named
        // `marker` -- is never followed by a bare `Ident`, so it falls
        // through to the ordinary function-definition arm below untouched).
        TokenKind::Ident(name) if name == "marker" && matches!(p.peek_at(1), TokenKind::Ident(_)) => {
            Item::Struct(parse_marker_def(p, annotations, visibility)?)
        }
        TokenKind::Spec => {
            reject_annotations(p, &annotations);
            Item::Spec(parse_spec_def(p, visibility)?)
        }
        TokenKind::Macro => {
            reject_annotations(p, &annotations);
            reject_visibility(p, visibility, visibility_span);
            Item::MacroDefinition(parse_macro_definition(p)?)
        }
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::Bang) => {
            reject_annotations(p, &annotations);
            reject_visibility(p, visibility, visibility_span);
            let inv = parse_macro_invocation(p)?;
            p.expect_terminator(&TokenKind::Semi, "';'");
            Item::MacroInvocation(inv)
        }
        // No leading `mut`/`comp` (handled above) -- `ident := value;`, a
        // plain (non-`comp`) top-level walrus. Still parses (`comp` isn't
        // required by the grammar), rejected during analysis instead (see
        // `Item::Walrus`'s doc comment).
        TokenKind::Ident(_) if matches!(p.peek_at(1), TokenKind::ColonEq) => {
            reject_annotations(p, &annotations);
            Item::Walrus(parse_item_walrus(p, false, false, visibility)?)
        }
        TokenKind::Ident(_) => parse_declaration_or_function_definition(p, annotations, visibility)?,
        _ => {
            reject_visibility(p, visibility, visibility_span);
            p.error(ParseErrorKind::Expected { expected: "a top-level item", found: p.peek().describe() });
            return None;
        }
    };
    let span = start.to(p.last_span());
    Some(ItemNode { item, span })
}

/// An optional leading `exposed`/`internal` -- contextual keywords, same
/// text-comparison recognition as `mut` (see `lexer::TokenKind`'s doc
/// comment). Returns the visibility (defaulting to `Hidden` when neither
/// is written) and, when one was, its own span -- for `reject_visibility`
/// to anchor an error at, for item kinds with nowhere to store one.
fn parse_optional_visibility(p: &mut Parser) -> (Visibility, Option<Span>) {
    let span = p.peek_span();
    match p.peek() {
        TokenKind::Ident(name) if name == "exposed" => {
            p.advance();
            (Visibility::Exposed, Some(span))
        }
        TokenKind::Ident(name) if name == "internal" => {
            p.advance();
            (Visibility::Internal, Some(span))
        }
        _ => (Visibility::Hidden, None),
    }
}

/// Errors (without aborting the surrounding item) if `visibility` isn't the
/// default -- for item kinds that have nowhere to store one at all
/// (`import`/macro definition/macro invocation). Same precedent as
/// `reject_annotations`.
fn reject_visibility(p: &mut Parser, visibility: Visibility, span: Option<Span>) {
    if visibility != Visibility::Hidden {
        p.error_at(span.expect("non-Hidden visibility always has a span"), ParseErrorKind::VisibilityNotAllowedHere);
    }
}

/// Zero or more `@name(args)` annotations, one per line, immediately above
/// an item -- see `AnnotationNode`'s doc comment. Consumes nothing (and
/// allocates nothing) when no `@` is present, the overwhelmingly common
/// case.
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

/// `@name` or `@name(arg, arg, ...)` -- parens (and their whole contents)
/// are optional: an absent `(...)` means the same thing an empty `()` would
/// (zero arguments), for an annotation whose resolver gives every argument
/// a default (see `omega_analyzer::annotations::resolve`, e.g. bare
/// `@inline` means `@inline(always)`).
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

/// `ident` (`always`, `enabled`, a `@suppress` warning name, ...) or
/// `ident = value`, where `value` is a plain integer (kept as raw decimal
/// digit text, exactly like `parser::type::parse_array_size`'s "shape, not
/// value" convention: no base prefix, suffix, or fraction is accepted here,
/// so a malformed numeric shape is rejected at parse time rather than
/// silently misread later), `sizeof<Type>` (`align = 4`, `pack =
/// sizeof<usize>`), or a string literal (`force = "some_symbol_name"`).
fn parse_annotation_arg(p: &mut Parser) -> Option<AnnotationArg> {
    let ident = p.expect_ident()?;
    if !p.eat(&TokenKind::Eq) {
        return Some(AnnotationArg::Ident(ident));
    }
    match p.peek() {
        TokenKind::Number(n)
            if matches!(n.base, crate::ast::expression::number::NumberBase::Decimal)
                && n.fractional_part.is_none()
                && n.explicit_type.is_none() =>
        {
            let value = n.integer_part.clone();
            p.advance();
            Some(AnnotationArg::KeyValue(ident, AnnotationValue::IntLiteral(value)))
        }
        TokenKind::Ident(name) if name == "sizeof" && matches!(p.peek_at(1), TokenKind::Lt) => {
            p.advance(); // 'sizeof'
            p.advance(); // '<'
            let r#type = crate::parser::r#type::parse_type(p)?;
            p.expect(&TokenKind::Gt, "'>'");
            Some(AnnotationArg::KeyValue(ident, AnnotationValue::Sizeof(r#type)))
        }
        TokenKind::Str(_) => {
            let TokenKind::Str(s) = p.advance().kind else { unreachable!() };
            Some(AnnotationArg::KeyValue(ident, AnnotationValue::StrLiteral(s)))
        }
        _ => {
            p.error(
                ParseErrorKind::Expected { expected: "a plain integer, 'sizeof<Type>', or a string literal", found: p.peek().describe() },
            );
            None
        }
    }
}

/// Errors (without aborting the surrounding item) if `annotations` is
/// non-empty -- for item kinds that have nowhere to store an annotation list
/// at all (`extern`/`import`/plain declarations/macros/specs). Anchored at
/// the first annotation's own span, not wherever parsing has reached by
/// the time the surrounding item finishes.
fn reject_annotations(p: &mut Parser, annotations: &[AnnotationNode]) {
    if let Some(first) = annotations.first() {
        p.error_at(first.span, ParseErrorKind::AnnotationNotAllowedHere);
    }
}

/// A leading identifier could start either a plain `Declaration`
/// (`ident: Type;`) or a `FunctionDefinition` (`ident<generics>(params) =>
/// Type { ... }`) -- disambiguated with a single-token lookahead, no
/// backtracking needed: only a function definition can have `<generics>` or
/// `(params)` at all in this position, so seeing `<` or `(` immediately
/// after the name is already conclusive on its own, without needing to look
/// *past* the (possibly absent, possibly multi-token) generics list first.
fn parse_declaration_or_function_definition(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
) -> Option<Item> {
    match p.peek_at(1) {
        TokenKind::Lt | TokenKind::LParen => {
            Some(Item::FunctionDefinition(parse_function_definition(p, annotations, visibility)?))
        }
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

/// `ident := value;` / `ident : Type;` at item position -- `mutable`/`comp`
/// are already known by the time this runs (see `parse_item`'s combined
/// lookahead). `p.peek_at(1)` (the token right after `ident`) is what tells
/// the two shapes apart, exactly like `parser::statement::
/// parse_walrus_or_declaration`'s identical local-scope dispatch. `comp` on
/// the typed (`:`) shape reports a clean error rather than silently
/// dropping the flag -- `comp` only makes sense on an inferred binding.
fn parse_item_declaration_or_walrus(
    p: &mut Parser,
    mutable: bool,
    comp: bool,
    visibility: Visibility,
) -> Option<Item> {
    match p.peek_at(1) {
        TokenKind::ColonEq => Some(Item::Walrus(parse_item_walrus(p, mutable, comp, visibility)?)),
        _ => {
            if comp {
                p.error(ParseErrorKind::Expected { expected: "':=' ('comp' only supports inferred bindings)", found: p.peek_at(1).describe() });
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

fn parse_item_walrus(p: &mut Parser, mutable: bool, comp: bool, visibility: Visibility) -> Option<WalrusStmt> {
    let ident = p.expect_ident()?;
    p.expect(&TokenKind::ColonEq, "':='");
    let value = parse_expression(p)?;
    p.expect_terminator(&TokenKind::Semi, "';'");
    Some(WalrusStmt { ident, value, mutable, comp, visibility })
}

/// `name<T, U, ...>(params) => ReturnType { body }` -- shared verbatim
/// between a top-level function definition and a struct method (see
/// `parse_struct_def`), exactly like the old grammar's single
/// `FunctionDefinitionStmt::parser` was. `annotations` is whatever
/// `parse_annotations` already consumed immediately above this function --
/// passed in rather than parsed here, since the caller (a member-function
/// loop, or `parse_declaration_or_function_definition`) needs to see them
/// *before* deciding this is a function at all.
pub fn parse_function_definition(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
) -> Option<FunctionDefinitionStmt> {
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    p.expect(&TokenKind::LParen, "'('");
    let (self_mode, params) = parse_param_list(p);
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let return_type = crate::parser::r#type::parse_type(p)?;
    let codeblock = parse_codeblock(p)?;
    Some(FunctionDefinitionStmt {
        annotations,
        visibility,
        ident,
        generics,
        self_mode,
        params,
        return_type,
        codeblock,
    })
}

/// `<T, U: Bound, ...>` -- optional, at least one name if present. Each
/// name may carry a single optional spec bound (`T: Animal`) -- see
/// `GenericParam`'s doc comment for why only one is ever parsed here.
fn parse_optional_generics(p: &mut Parser) -> Option<Vec<GenericParam>> {
    if !p.eat(&TokenKind::Lt) {
        return Some(Vec::new());
    }
    let mut generics = vec![parse_generic_param(p)?];
    while p.eat(&TokenKind::Comma) {
        generics.push(parse_generic_param(p)?);
    }
    p.expect(&TokenKind::Gt, "'>'");
    Some(generics)
}

fn parse_generic_param(p: &mut Parser) -> Option<GenericParam> {
    let ident = p.expect_ident()?;
    let bound = if p.eat(&TokenKind::Colon) { Some(crate::parser::r#type::parse_type(p)?) } else { None };
    Some(GenericParam { ident, bound })
}

/// `: Spec, Spec, ...` -- the specs a struct/union/enum implements,
/// parsed right after the generics list. Absent entirely (no leading `:`)
/// is the overwhelmingly common case, returning an empty list. Shares its
/// comma-separated-`Type`-list shape with a spec's own `: Dep, Dep`
/// dependency clause (see `parse_spec_def`) -- both mean "must also
/// satisfy these specs," just said from opposite sides (a concrete type
/// implementing one, vs. a spec requiring one).
fn parse_optional_implements(p: &mut Parser) -> Option<Vec<Type>> {
    if !p.eat(&TokenKind::Colon) {
        return Some(Vec::new());
    }
    let mut specs = vec![crate::parser::r#type::parse_type(p)?];
    while p.eat(&TokenKind::Comma) {
        specs.push(crate::parser::r#type::parse_type(p)?);
    }
    Some(specs)
}

/// `self` / `mut self` / `*self` / `*mut self` (optionally followed by `,
/// ident: Type, ...`), or just `ident: Type, ...` -- see
/// `crate::parser::parse_self_mode`. Returns `(self_mode, params)`.
fn parse_param_list(p: &mut Parser) -> (Option<SelfMode>, Vec<DeclarationStmt>) {
    match crate::parser::parse_self_mode(p) {
        Some(mode) => {
            let rest = if p.eat(&TokenKind::Comma) { parse_declaration_list(p) } else { Vec::new() };
            (Some(mode), rest)
        }
        None => (None, parse_declaration_list(p)),
    }
}

/// Zero or more `ident: Type` pairs, comma-separated -- a comma is only
/// consumed if another declaration actually follows, so a trailing comma
/// before `)`/`}` is left unconsumed (a real parse error at the caller,
/// matching the old grammar's plain `separated_by`, which doesn't tolerate
/// one either) rather than silently swallowed.
fn parse_declaration_list(p: &mut Parser) -> Vec<DeclarationStmt> {
    let mut decls = Vec::new();
    if !matches!(p.peek(), TokenKind::Ident(_)) {
        return decls;
    }
    while let Some(decl) = parse_declaration(p) {
        decls.push(decl);
        if matches!(p.peek(), TokenKind::Comma) && matches!(p.peek_at(1), TokenKind::Ident(_)) {
            p.advance();
        } else {
            break;
        }
    }
    decls
}

/// `struct Name<T, ...> { field: Type; ... method(...) => T { ... } ... }`
/// -- fields and methods are structurally two separate phases, fields
/// always first (matching the old grammar's `declarations_parser.repeated()`
/// *then* `functions_parser.repeated()`, not an interleaved single loop):
/// once the field-shaped lookahead (`Ident` + `:`) stops matching, the
/// struct body is assumed to be all methods from there on.
pub fn parse_struct_def(p: &mut Parser, annotations: Vec<AnnotationNode>, visibility: Visibility) -> Option<StructStmt> {
    p.expect(&TokenKind::Struct, "'struct'");
    parse_struct_or_marker_body(p, annotations, visibility, false)
}

/// `marker Name<T, ...> : Spec1, Spec2 { method(...) => T { ... } ... }` --
/// `marker`'s own doc comment (`ast::statement::struct::StructStmt::
/// is_marker`) covers the *semantics*; here it's purely a grammar fact that
/// a marker's field-list section doesn't exist: `parse_struct_or_marker_body`
/// below is handed `is_marker = true` and never even calls `field_follows`,
/// so `marker Foo { x: i32; }` fails to parse as a field at all (it falls
/// through into the *functions* loop, which then rejects `x: i32;` as an
/// invalid method) rather than being silently accepted and only rejected
/// later during analysis.
pub fn parse_marker_def(p: &mut Parser, annotations: Vec<AnnotationNode>, visibility: Visibility) -> Option<StructStmt> {
    p.advance(); // 'marker' -- contextual keyword, already confirmed by the caller's lookahead
    parse_struct_or_marker_body(p, annotations, visibility, true)
}

/// The shared tail both `struct` and `marker` parse into, after their own
/// leading keyword: name, generics, `implements`, an optional field-list
/// section (skipped entirely for a marker -- see `parse_marker_def`), and
/// the trailing method list.
fn parse_struct_or_marker_body(
    p: &mut Parser,
    annotations: Vec<AnnotationNode>,
    visibility: Visibility,
    is_marker: bool,
) -> Option<StructStmt> {
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    let implements = parse_optional_implements(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

    let mut fields = Vec::new();
    if !is_marker {
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
    }

    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) || p.check(&TokenKind::At) {
        let fn_annotations = parse_annotations(p);
        let (fn_visibility, _) = parse_optional_visibility(p);
        match parse_function_definition(p, fn_annotations, fn_visibility) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }

    p.expect(&TokenKind::RBrace, "'}'");
    Some(StructStmt { annotations, visibility, ident, generics, implements, fields, functions, is_marker })
}

/// Whether a field declaration (as opposed to the start of the methods
/// section) follows at the current position -- an `Ident` + `:` lookahead,
/// same as before, extended to see past an optional leading
/// `exposed`/`internal` (which would otherwise break the lookahead: `exposed
/// name: Type;` has `peek() = Ident("exposed")`, `peek_at(1) =
/// Ident("name")`, not `Colon`).
fn field_follows(p: &Parser) -> bool {
    let offset = match p.peek() {
        TokenKind::Ident(name) if name == "exposed" || name == "internal" => 1,
        _ => 0,
    };
    matches!(p.peek_at(offset), TokenKind::Ident(_)) && matches!(p.peek_at(offset + 1), TokenKind::Colon)
}

/// `union Name<T, ...> { field: Type; ... method(...) => T { ... } ... }`
/// -- identical shape and parsing strategy to `parse_struct_def`; the only
/// difference is semantic (fields overlap in storage instead of being laid
/// out sequentially), which is entirely an analyzer/codegen concern.
pub fn parse_union_def(p: &mut Parser, annotations: Vec<AnnotationNode>, visibility: Visibility) -> Option<UnionStmt> {
    p.expect(&TokenKind::Union, "'union'");
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    let implements = parse_optional_implements(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

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

    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) || p.check(&TokenKind::At) {
        let fn_annotations = parse_annotations(p);
        let (fn_visibility, _) = parse_optional_visibility(p);
        match parse_function_definition(p, fn_annotations, fn_visibility) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }

    p.expect(&TokenKind::RBrace, "'}'");
    Some(UnionStmt { annotations, visibility, ident, generics, implements, fields, functions })
}

/// `spec Name<T, ...> : Dep, Dep for Target { functions }` (declaration
/// form) or `spec Name<T, ...> = Dep | Dep | Dep;` (alias form) -- see
/// `SpecStmt`'s doc comment for the two forms' shared meaning, and for what
/// the optional `for Target` clause (decl form only) does. The leading
/// `:`/`=` token is what disambiguates the two forms; both keep parsing a
/// `Type`-list afterward (`,`-separated for `:`, `|`-separated for `=`),
/// just with different terminators (`{ ... }` vs `;`).
pub fn parse_spec_def(p: &mut Parser, visibility: Visibility) -> Option<SpecStmt> {
    p.expect(&TokenKind::Spec, "'spec'");
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;

    if p.eat(&TokenKind::Eq) {
        let mut dependencies = vec![crate::parser::r#type::parse_type(p)?];
        while p.eat(&TokenKind::Pipe) {
            dependencies.push(crate::parser::r#type::parse_type(p)?);
        }
        if p.check(&TokenKind::LBrace) {
            p.error(ParseErrorKind::SpecAliasCannotDeclareFunctions);
            recovery::skip_balanced_group(p);
        } else {
            p.expect_terminator(&TokenKind::Semi, "';'");
        }
        return Some(SpecStmt { ident, visibility, generics, dependencies, functions: Vec::new(), target: None });
    }

    let dependencies = parse_optional_implements(p)?;
    let target = if p.eat(&TokenKind::For) {
        Some(crate::parser::r#type::parse_type(p)?)
    } else {
        None
    };
    p.expect(&TokenKind::LBrace, "'{'");
    let mut functions = Vec::new();
    while matches!(p.peek(), TokenKind::Ident(_)) {
        match parse_spec_function(p) {
            Some(f) => functions.push(f),
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }
    p.expect(&TokenKind::RBrace, "'}'");
    Some(SpecStmt { ident, visibility, generics, dependencies, functions, target })
}

/// `name(params) => Ret;` (required -- every implementor must provide a
/// matching method) or `name(params) => Ret { block }` (default -- used
/// as-is unless overridden). No per-function generics here (unlike
/// `parse_function_definition`) -- not part of the language's spec design.
fn parse_spec_function(p: &mut Parser) -> Option<SpecFunctionStmt> {
    let ident = p.expect_ident()?;
    p.expect(&TokenKind::LParen, "'('");
    let (self_mode, params) = parse_param_list(p);
    p.expect(&TokenKind::RParen, "')'");
    p.expect(&TokenKind::FatArrow, "'=>'");
    let return_type = crate::parser::r#type::parse_type(p)?;
    let body = if p.check(&TokenKind::LBrace) {
        Some(parse_codeblock(p)?)
    } else {
        p.expect_terminator(&TokenKind::Semi, "';'");
        None
    };
    Some(SpecFunctionStmt { ident, self_mode, params, return_type, body })
}

/// `enum Name<T, ...>(header) { [dynamic_fields] Variant(args) { fields }, ...; functions }`
/// -- see `EnumStmt`'s doc comment for the full shape. The optional shared
/// dynamic fields (if any) come first, parsed exactly like `parse_struct_def`'s
/// field loop; a variant name is never followed by `:`, so the same `Ident`
/// + `:` lookahead unambiguously tells the two apart. Variants are
/// separated by `,` (optional after a `{...}` body, so a body can be
/// followed directly by the next variant); the variant list ends at `}`
/// (no functions) or at a `;`, after which only function definitions may
/// follow -- Java's "constants first, then a `;`, then members" rule.
pub fn parse_enum_def(p: &mut Parser, annotations: Vec<AnnotationNode>, visibility: Visibility) -> Option<EnumStmt> {
    p.expect(&TokenKind::Enum, "'enum'");
    let ident = p.expect_ident()?;
    let generics = parse_optional_generics(p)?;
    let implements = parse_optional_implements(p)?;
    let header = parse_enum_header(p)?;
    p.expect(&TokenKind::LBrace, "'{'");

    // The optional shared-dynamic-fields section -- same lookahead and loop
    // body `parse_struct_def`'s field loop uses, just spliced here, before
    // the variant list, instead of a struct's `{...}`.
    let mut dynamic_fields = Vec::new();
    while field_follows(p) {
        let (field_visibility, _) = parse_optional_visibility(p);
        match parse_declaration(p) {
            Some(mut decl) => {
                decl.visibility = field_visibility;
                dynamic_fields.push(decl);
                p.expect_terminator(&TokenKind::Semi, "';'");
            }
            None => recovery::synchronize_to_statement_boundary(p),
        }
    }

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
        // After a `{...}` body the separator is optional -- the body's own
        // closing brace already delimits the variant (see Example 3 in the
        // language design).
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
        while matches!(p.peek(), TokenKind::Ident(_)) || p.check(&TokenKind::At) {
            let fn_annotations = parse_annotations(p);
            let (fn_visibility, _) = parse_optional_visibility(p);
            match parse_function_definition(p, fn_annotations, fn_visibility) {
                Some(f) => functions.push(f),
                None => recovery::synchronize_to_statement_boundary(p),
            }
        }
    }

    p.expect(&TokenKind::RBrace, "'}'");
    Some(EnumStmt { annotations, visibility, ident, generics, implements, header, dynamic_fields, variants, functions })
}

/// The optional `(name: Type, ...)` header after the enum's name -- each
/// entry keeps its own span (unlike struct fields) because header entries
/// have position-sensitive rules (`tag` must be the first one) worth an
/// error pointing at the exact entry.
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
            header.push(EnumHeaderField { ident: decl.ident, r#type: decl.r#type, visibility, span });
            if !p.eat(&TokenKind::Comma) {
                break;
            }
        }
    }
    p.expect(&TokenKind::RParen, "')'");
    Some(header)
}

/// `Name`, `Name(args)`, `Name { fields }`, or `Name(args) { fields }`.
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
        if !p.check(&TokenKind::RBrace) {
            p.error(ParseErrorKind::Expected {
                expected: "a field (`name: Type;`) or '}'",
                found: p.peek().describe(),
            });
            return None;
        }
        p.advance(); // '}'
    }

    Some(EnumVariantStmt { ident, span, args, fields })
}

/// Whether the `Ident` at the current position starts a *function
/// definition* rather than a variant -- a pure token-lookahead check (no
/// consumption, no speculative errors): a `<` right after the name can only
/// be a function's generics in this position, and a `(...)` whose matching
/// `)` is followed by `=>` can only be a function's parameter list (a
/// variant's `(args)` is never followed by `=>`).
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
