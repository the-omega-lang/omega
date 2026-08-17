//! `omega-hir` had no tests at all before this file, despite owning four
//! desugarings that every later pass depends on being correct (see the
//! crate doc). These cover exactly those, plus the two invariants the rest
//! of the compiler assumes: ids are unique, and spans are real.

use omega_hir::{
    HirExpr, HirItem, HirPlaceRoot, HirProjection, HirRangeEnd, HirStmt, ModuleId, lower_module,
};
use omega_parser::SourceModule;
use omega_parser::prelude::SelfMode;

fn lower(source: &str) -> omega_hir::HirModule {
    let ast = SourceModule::parse(source).expect("test source must parse");
    lower_module(ModuleId(0), &ast)
}

/// The single function/method in a lowered module's first struct, or the
/// module's first top-level function.
fn only_function(module: &omega_hir::HirModule) -> &omega_hir::HirFunctionDef {
    module
        .items
        .iter()
        .find_map(|item| match item {
            HirItem::FunctionDefinition(f) => Some(f),
            HirItem::Struct(s) => s.functions.first(),
            _ => None,
        })
        .expect("expected a function")
}

// --- Desugaring 1: `self` insertion -------------------------------------

#[test]
fn pointer_self_becomes_a_leading_self_parameter() {
    let module = lower("struct S { x: i32; get(*self) => i32 { self.x } }");
    let f = only_function(&module);
    assert_eq!(f.self_mode, Some(SelfMode::Pointer));
    assert_eq!(f.params.len(), 1, "self must be inserted as a parameter");
    assert_eq!(f.params[0].ident.as_ref(), "self");
}

#[test]
fn a_free_function_gets_no_self_parameter() {
    let module = lower("free(a: i32) => i32 { a }");
    let f = only_function(&module);
    assert_eq!(f.self_mode, None);
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].ident.as_ref(), "a");
}

// --- Desugaring 2: `mut self` shadowing ---------------------------------

#[test]
fn by_value_mut_self_gets_a_shadowing_binding_first() {
    let module = lower("struct S { x: i32; take(mut self) => i32 { self.x } }");
    let f = only_function(&module);
    assert_eq!(f.self_mode, Some(SelfMode::MutValue));
    let HirStmt::WalrusDeclaration(w) = &f.body.stmts[0] else {
        panic!("expected the synthesized `mut self := self;` first");
    };
    assert_eq!(w.ident.as_ref(), "self");
    assert!(w.mutable, "the shadow must be a mutable binding");
}

#[test]
fn plain_by_value_self_gets_no_shadow() {
    let module = lower("struct S { x: i32; take(self) => i32 { self.x } }");
    let f = only_function(&module);
    assert!(
        !matches!(f.body.stmts.first(), Some(HirStmt::WalrusDeclaration(w)) if w.ident.as_ref() == "self"),
        "immutable `self` needs no shadow"
    );
}

// --- Desugaring 3: `spec T` parameters ----------------------------------

#[test]
fn each_spec_parameter_becomes_its_own_bound_generic() {
    let module = lower(
        "spec Foo { f(*self) => i32; }\n\
         takes(a: spec Foo, b: spec Foo) => i32 { 0 }",
    );
    let f = only_function(&module);
    assert_eq!(
        f.generics.len(),
        2,
        "two `spec Foo` parameters must not share one generic"
    );
    for g in &f.generics {
        assert!(g.ident.as_ref().starts_with('$'), "synthesized name");
        assert_eq!(g.bounds.len(), 1, "each carries its spec as a bound");
    }
}

#[test]
fn spec_parameters_are_found_behind_a_pointer() {
    let module = lower(
        "spec Foo { f(*self) => i32; }\n\
         takes(a: *spec Foo) => i32 { 0 }",
    );
    assert_eq!(only_function(&module).generics.len(), 1);
}

// --- Desugaring 4: place-chain flattening -------------------------------

#[test]
fn a_projection_chain_flattens_in_source_order() {
    let module = lower("f(a: i32) => i32 { x.y[0].z; 0 }");
    let f = only_function(&module);
    let HirStmt::Expression(node) = &f.body.stmts[0] else {
        panic!("expected an expression statement");
    };
    let HirExpr::Place(place) = &node.expr else {
        panic!("expected a place");
    };
    assert!(matches!(place.root, HirPlaceRoot::Path(_)));
    assert!(
        matches!(
            place.projections.as_slice(),
            [
                HirProjection::FieldAccess(_),
                HirProjection::Index(_),
                HirProjection::FieldAccess(_)
            ]
        ),
        "expected .y then [0] then .z, got {:?}",
        place.projections
    );
}

#[test]
fn a_non_place_base_roots_at_an_expression() {
    let module = lower("f(a: i32) => i32 { g().field; 0 }");
    let f = only_function(&module);
    let HirStmt::Expression(node) = &f.body.stmts[0] else {
        panic!("expected an expression statement");
    };
    let HirExpr::Place(place) = &node.expr else {
        panic!("expected a place");
    };
    assert!(matches!(place.root, HirPlaceRoot::Expr(_)));
    assert_eq!(place.projections.len(), 1);
}

// --- Invariants ---------------------------------------------------------

#[test]
fn every_range_spelling_survives_lowering_distinctly() {
    // The three spellings must stay distinguishable: flattening them into
    // `Option<end> + bool` is what let an "inclusive range with no end"
    // become representable. See `HirRangeEnd`.
    for (source, expect_open, expect_inclusive) in [
        ("f() => void { for i in 0..=3 { } }", false, true),
        ("f() => void { for i in 0..<3 { } }", false, false),
        ("f() => void { for i in 0.. { } }", true, true),
    ] {
        let module = lower(source);
        let f = only_function(&module);
        let HirStmt::ForIn(for_in) = &f.body.stmts[0] else {
            panic!("expected a for-in statement for `{source}`");
        };
        let HirExpr::Range(range) = &for_in.iterator.expr else {
            panic!("expected a range iterator for `{source}`");
        };
        assert_eq!(
            matches!(range.end, HirRangeEnd::Open),
            expect_open,
            "openness of `{source}`"
        );
        assert_eq!(range.inclusive(), expect_inclusive, "inclusivity of `{source}`");
    }
}

#[test]
fn every_id_in_a_lowered_module_is_unique() {
    let module = lower(
        "spec Sp { m(*self) => i32; }\n\
         struct S { a: i32; b: i32; get(*self) => i32 { self.a } }\n\
         enum E(tag: i16) { A(0), B(1); which(*self) => i32 { 0 } }\n\
         top(x: i32) => i32 { y := x; for i in 0..<3 { } y }",
    );
    let mut ids = Vec::new();
    collect_ids(&module, &mut ids);
    let before = ids.len();
    ids.sort_unstable_by_key(|id| (id.module.0, id.local));
    ids.dedup();
    assert_eq!(before, ids.len(), "HirIds must be unique within a module");
    // Guards against `collect_ids` silently walking nothing, which would
    // make the uniqueness assertion above vacuously true.
    assert!(before > 10, "expected ids from every item, got {before}");
}

/// Ids of the item-level nodes and their immediate members -- enough to
/// catch a shared counter being reset or reused, which is what this guards.
fn collect_ids(module: &omega_hir::HirModule, out: &mut Vec<omega_hir::HirId>) {
    for item in &module.items {
        match item {
            HirItem::FunctionDefinition(f) => {
                out.push(f.id);
                out.extend(f.params.iter().map(|p| p.id));
            }
            HirItem::Struct(s) => {
                out.push(s.id);
                out.extend(s.fields.iter().map(|f| f.id));
                for f in &s.functions {
                    out.push(f.id);
                    out.extend(f.params.iter().map(|p| p.id));
                }
            }
            HirItem::Enum(e) => {
                out.push(e.id);
                out.extend(e.header.iter().map(|h| h.id));
                out.extend(e.variants.iter().map(|v| v.id));
                for f in &e.functions {
                    out.push(f.id);
                    out.extend(f.params.iter().map(|p| p.id));
                }
            }
            HirItem::Spec(sp) => {
                out.push(sp.id);
                for f in &sp.functions {
                    out.push(f.id);
                    out.extend(f.params.iter().map(|p| p.id));
                }
            }
            _ => {}
        }
    }
}

#[test]
fn a_field_carries_its_own_span_not_the_structs() {
    // The regression that made a duplicate field underline the whole
    // struct: fields inherited the enclosing declaration's span.
    let source = "struct S { alpha: i32; beta: i32; }";
    let module = lower(source);
    let HirItem::Struct(s) = &module.items[0] else {
        panic!("expected a struct");
    };
    let alpha = &s.fields[0];
    let beta = &s.fields[1];
    assert_ne!(alpha.span, beta.span, "each field needs its own span");
    assert_eq!(&source[alpha.name_span.start..alpha.name_span.end], "alpha");
    assert_eq!(&source[beta.name_span.start..beta.name_span.end], "beta");
    assert!(
        alpha.span.end - alpha.span.start < source.len(),
        "a field's span must not cover the whole struct"
    );
}

#[test]
fn a_function_return_type_span_covers_the_written_type() {
    let source = "f(a: i32) => *mut i32 { <*mut i32>0 }";
    let module = lower(source);
    let f = only_function(&module);
    assert_eq!(
        &source[f.return_type_span.start..f.return_type_span.end],
        "*mut i32",
        "the whole declared type, not just its last token"
    );
    assert_eq!(&source[f.name_span.start..f.name_span.end], "f");
}
