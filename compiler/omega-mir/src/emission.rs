use crate::mir::{MirItem, MirModule};
use omega_parser::prelude::Ident;
use std::collections::HashMap;
use std::path::PathBuf;

/// One physical source file of the local package being compiled.
///
/// `path` is relative to the package root and is the stable native emission
/// identity: it mirrors on-disk layout, so a declared package identity that
/// renames `module` never moves the artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmissionSource {
    pub module: Vec<Ident>,
    pub path: PathBuf,
}

/// The definitions one physical source file natively owns, and therefore the
/// contents of exactly one emitted artifact.
///
/// Native ownership is not optimization visibility: a unit may later be given
/// non-owning bodies or summaries to optimize against without changing which
/// definitions it emits.
#[derive(Debug, Clone)]
pub struct EmissionUnit {
    pub source: PathBuf,
    pub items: Vec<MirItem>,
}

/// Assigns every lowered definition to the source file that natively owns it,
/// producing exactly one unit per physical source, ordered by relative path.
///
/// Concrete generic/method/conformance bodies emitted here can be semantically
/// owned by an extern package and have no local source. Their symbols already
/// carry the declaring module's identity, so they only need a deterministic
/// local home: the source-bearing package root when there is one, otherwise
/// the first source in path order.
pub fn plan_emission(
    modules: Vec<(Vec<Ident>, MirModule)>,
    sources: &[EmissionSource],
) -> Vec<EmissionUnit> {
    let mut sources: Vec<&EmissionSource> = sources.iter().collect();
    if sources.is_empty() {
        return Vec::new();
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));

    let owners: HashMap<&[Ident], usize> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| (source.module.as_slice(), index))
        .collect();
    let generated_home = sources
        .iter()
        .position(|source| source.module.len() == 1)
        .unwrap_or(0);

    let mut units: Vec<EmissionUnit> = sources
        .iter()
        .map(|source| EmissionUnit {
            source: source.path.clone(),
            items: Vec::new(),
        })
        .collect();
    for (path, module) in modules {
        let unit = owners
            .get(path.as_slice())
            .copied()
            .unwrap_or(generated_home);
        units[unit].items.extend(module.items);
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_hir::ModuleId;

    fn ident(name: &str) -> Ident {
        Ident(name.to_string())
    }

    fn source(module: &[&str], path: &str) -> EmissionSource {
        EmissionSource {
            module: module.iter().copied().map(ident).collect(),
            path: PathBuf::from(path),
        }
    }

    fn module(items: usize) -> MirModule {
        MirModule {
            id: ModuleId(0),
            items: (0..items)
                .map(|index| {
                    MirItem::Declaration(crate::mir::MirDeclaration {
                        id: omega_hir::HirId {
                            module: ModuleId(0),
                            local: index as u32,
                        },
                        span: omega_parser::prelude::Span::default(),
                        ident: ident("g"),
                        r#type: omega_analyzer::resolved_type::ResolvedType::I32,
                        initial_value: None,
                        symbol: format!("g{index}"),
                    })
                })
                .collect(),
        }
    }

    #[test]
    fn every_source_owns_one_unit_in_relative_path_order() {
        let sources = [
            source(&["pkg", "sub", "leaf"], "sub/leaf.omg"),
            source(&["pkg"], "pkg.omg"),
            source(&["pkg", "child"], "child.omg"),
        ];
        let units = plan_emission(vec![(vec![ident("pkg")], module(1))], &sources);

        assert_eq!(
            units.iter().map(|unit| &unit.source).collect::<Vec<_>>(),
            vec![
                &PathBuf::from("child.omg"),
                &PathBuf::from("pkg.omg"),
                &PathBuf::from("sub/leaf.omg"),
            ]
        );
        assert_eq!(units[0].items.len(), 0, "a silent source still owns a unit");
        assert_eq!(units[1].items.len(), 1);
    }

    #[test]
    fn extern_owned_definitions_attach_to_the_root_source_without_a_new_unit() {
        let sources = [source(&["pkg"], "pkg.omg"), source(&["pkg", "a"], "a.omg")];
        let units = plan_emission(
            vec![
                (vec![ident("core"), ident("mem")], module(2)),
                (vec![ident("pkg")], module(1)),
            ],
            &sources,
        );

        assert_eq!(units.len(), 2, "no synthetic unit for an extern-owned body");
        assert_eq!(units[0].source, PathBuf::from("a.omg"));
        assert_eq!(units[0].items.len(), 0);
        assert_eq!(units[1].source, PathBuf::from("pkg.omg"));
        assert_eq!(units[1].items.len(), 3);
    }

    #[test]
    fn a_rootless_package_attaches_generated_definitions_to_its_first_source() {
        let sources = [
            source(&["pkg", "b"], "b.omg"),
            source(&["pkg", "a"], "a.omg"),
        ];
        let units = plan_emission(vec![(vec![ident("core")], module(2))], &sources);

        assert_eq!(units.len(), 2);
        assert_eq!(units[0].source, PathBuf::from("a.omg"));
        assert_eq!(units[0].items.len(), 2);
        assert_eq!(units[1].items.len(), 0);
    }
}
