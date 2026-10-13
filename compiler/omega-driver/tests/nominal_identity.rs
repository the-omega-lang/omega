//! Nominal types are interned once per instantiation, and a definition read
//! before it is written is a reported cycle rather than a default.

use omega_analyzer::Target;
use omega_analyzer::resolved_type::{ResolvedGenericArg, ResolvedType};
use omega_analyzer::type_store::{NominalRef, TypeStore};
use omega_driver::{CompileError, CompiledProgram, Driver};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_nominal_identity_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write module");
        Self(root)
    }

    fn compile(&self) -> Result<CompiledProgram, String> {
        let driver =
            Driver::new(self.0.clone(), None, vec![], Target::DEFAULT).expect("construct driver");
        driver
            .compile(&omega_hir::ModulePath::new([Ident("main".into())]))
            .result
            .map_err(|errors| {
                errors
                    .iter()
                    .flat_map(CompileError::to_diagnostics)
                    .map(|diagnostic| diagnostic.message)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn named<'a>(types: &'a TypeStore, name: &str) -> Vec<&'a NominalRef> {
    types
        .nominals()
        .filter(|nominal| nominal.name.as_ref() == name)
        .collect()
}

fn type_arg(nominal: &NominalRef) -> &ResolvedType {
    match nominal.generic_args.as_slice() {
        [ResolvedGenericArg::Type(r#type)] => r#type,
        other => panic!("expected one type argument, got {other:?}"),
    }
}

#[test]
fn each_instantiation_is_interned_exactly_once() {
    let package = TestPackage::new(
        r#"
        struct Holder<T> { value: T; }
        main() => void {
            a := Holder<i32> { value = 1; };
            b := Holder<i32> { value = 2; };
            c := Holder<u8> { value = 3u8; };
        }
        "#,
    );
    let program = package.compile().expect("compiles");
    let holders = named(&program.types, "Holder");

    let of = |arg: &ResolvedType| {
        holders
            .iter()
            .filter(|nominal| type_arg(nominal) == arg)
            .count()
    };
    assert_eq!(of(&ResolvedType::I32), 1, "{holders:#?}");
    assert_eq!(of(&ResolvedType::U8), 1, "{holders:#?}");
    assert_eq!(holders.len(), 2, "{holders:#?}");
    assert_ne!(holders[0].id(), holders[1].id());
}

#[test]
fn a_recursive_generic_type_refers_to_its_own_identity() {
    let package = TestPackage::new(
        r#"
        struct Node<T> { next: *Node<T>; value: T; }
        main() => void {
            n := Node<i32> { next = <*Node<i32>>0usize; value = 1; };
        }
        "#,
    );
    let program = package.compile().expect("compiles");
    let [node] = named(&program.types, "Node")[..] else {
        panic!("expected one Node instantiation");
    };
    let fields = program
        .types
        .fields(node)
        .expect("the definition was written");
    let ResolvedType::Pointer { pointee, .. } = &fields[0].r#type else {
        panic!("`next` is a pointer");
    };
    assert_eq!(**pointee, ResolvedType::Struct(node.clone()));
}

#[test]
fn a_definition_that_needs_its_own_layout_is_a_cycle() {
    let package = TestPackage::new(
        r#"
        enum Kind(size: usize) { Small(sizeof<Kind>), Large(2usize); }
        main() => void {}
        "#,
    );
    let errors = package.compile().err().expect("rejected");
    assert!(
        errors == "cyclic resolution dependency: main::Kind",
        "{errors}"
    );
}
