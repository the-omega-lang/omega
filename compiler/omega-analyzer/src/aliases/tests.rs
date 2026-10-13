use super::*;
use crate::resolver::{ItemNamespace, ResolvedAlias};
use omega_parser::prelude::{Origin, Path, Visibility};

/// Alias expansion depends only on name resolution, so this mock implements
/// `Names` alone: `app` declares `alias Word = u32` and nothing else.
struct OneAlias;

impl Names for OneAlias {
    fn macro_origin_module(&self, _origin: Origin) -> Option<ModulePath> {
        None
    }

    fn resolve_explicit_anchor(
        &self,
        _origin_module: &ModulePath,
        _path: &Path,
    ) -> Option<Result<Vec<Ident>, ResolveError>> {
        None
    }

    fn resolve_module_path(
        &mut self,
        _accessor: &ModulePath,
        _absolute_path: &ModulePath,
    ) -> Result<Option<ModulePath>, ResolveError> {
        Ok(None)
    }

    fn declared_item_visibility(&mut self, _absolute_path: &ItemPath) -> Option<Visibility> {
        unreachable!("alias expansion never asks for declared visibility")
    }

    fn resolve_import_alias(
        &mut self,
        _module_path: &ModulePath,
        _alias: &Ident,
    ) -> Result<Option<ImportTarget>, ResolveError> {
        Ok(None)
    }

    fn resolve_visible_alias(
        &mut self,
        _accessor: &ModulePath,
        alias_module: &ModulePath,
        name: &Ident,
        _bypass_visibility: bool,
    ) -> Result<Option<ResolvedAlias>, ResolveError> {
        Ok(
            (*alias_module == app() && name.as_ref() == "Word").then(|| ResolvedAlias::Type {
                generics: Vec::new(),
                r#type: named("u32"),
            }),
        )
    }

    fn ambient_core_candidates(
        &mut self,
        _accessor: &ModulePath,
        _name: &Ident,
    ) -> Result<Option<ItemPath>, ResolveError> {
        unreachable!("alias expansion never consults ambient core")
    }

    fn import_alias_names(&mut self, _module_path: &ModulePath) -> Vec<Ident> {
        unreachable!("alias expansion never lists import names")
    }

    fn is_item_visible(&mut self, _accessor: &ModulePath, _absolute_path: &ItemPath) -> bool {
        unreachable!("alias expansion gates visibility through resolve_visible_alias")
    }

    fn similar_item_name(
        &mut self,
        _module_path: &ModulePath,
        _target: &Ident,
        _namespace: ItemNamespace,
    ) -> Option<Ident> {
        unreachable!("alias expansion reports no suggestions")
    }
}

fn app() -> ModulePath {
    ModulePath::from(vec![Ident("app".into())])
}

fn named(name: &str) -> Type {
    Type::Named(Path::from(Ident(name.into())))
}

#[test]
fn expansion_needs_only_the_names_capability() {
    let written = Type::Generic(
        Path::from(Ident("Box".into())),
        vec![GenericArg::Type(named("Word"))],
    );
    let expanded = expand_type_alias(&mut OneAlias, &app(), written).unwrap();
    assert_eq!(
        expanded,
        Type::Generic(
            Path::from(Ident("Box".into())),
            vec![GenericArg::Type(named("u32"))],
        )
    );
}
