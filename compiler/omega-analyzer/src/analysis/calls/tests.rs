use super::*;
use crate::analysis::tests::{NoResolver, analyzer, id, sp};
use crate::generics::pattern::{ArgumentPattern, TypePattern};
use crate::generics::{GenericParam, normalize_function};
use omega_hir::{HirItem, ModuleId, lower_module};
use omega_parser::SourceModule;

fn function(source: &str) -> HirFunctionDef {
    let ast = SourceModule::parse(source).unwrap();
    lower_module(ModuleId(1), &ast)
        .items
        .into_iter()
        .find_map(|item| match item {
            HirItem::FunctionDefinition(f) => Some(f),
            _ => None,
        })
        .expect("a function definition")
}

fn owner_params(names: &[&str]) -> Vec<GenericParam> {
    let written: Vec<_> = names
        .iter()
        .map(|name| omega_hir::HirGenericParam::r#type(Ident((*name).into()), vec![], None))
        .collect();
    GenericParam::declared(id(100), &written)
}

/// `Owner<T>::name<T>`: one combined signature, owner positions first. The
/// member's `T` shadows the owner's lexically, yet both keep their own
/// position, and `Self` is the owner applied to the owner's positions only.
#[test]
fn an_owner_and_member_signature_keeps_shadowed_parameters_distinct() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let f = function("name<T>(owner: Self, value: T) => T { value }");
    let normalized = normalize_function(a.resolver, &ModulePath::default(), &f).unwrap();
    let owner = owner_params(&["T"]);
    let owner_path = ModulePath::from_names(&["pkg"]).join(Ident("Owner".into()));
    let self_pattern = a
        .owner_self_pattern(id(0), sp(), &owner_path, &owner)
        .unwrap();
    let template = a
        .normalized_overload_template(&normalized, &owner, Some(&self_pattern))
        .expect("the signature builds");

    assert_eq!(template.generics.len(), 2);
    assert_ne!(template.generics[0].id, template.generics[1].id);
    assert_eq!(
        template.params,
        vec![
            TypePattern::Nominal(
                owner_path,
                vec![ArgumentPattern::Type(Box::new(TypePattern::Parameter(0)))],
            ),
            TypePattern::Parameter(1),
        ]
    );
    assert_eq!(template.return_type, TypePattern::Parameter(1));
}

/// A `spec S` parameter's type is its own anonymous position, reached by
/// identity rather than by any name.
#[test]
fn a_static_spec_parameter_is_its_own_unnamed_position() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);
    let f = function("take<A>(first: A, second: spec Show) => A { first }");
    let normalized = normalize_function(a.resolver, &ModulePath::default(), &f).unwrap();

    assert_eq!(normalized.generics.len(), 2);
    assert!(normalized.generics[1].name.is_none());
    assert_eq!(normalized.generics[1].label(), "spec Show");
    assert_eq!(normalized.static_spec_position(1), Some(1));
    assert_eq!(normalized.static_spec_position(0), None);
}
