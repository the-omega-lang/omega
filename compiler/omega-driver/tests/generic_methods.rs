//! Driver-level behavior of generic member and static functions: a
//! declaration with its own generic parameters is a template, and each call
//! that determines those arguments materializes one instantiation with its
//! own identity, symbol, and weak linkage.

use omega_analyzer::Target;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestWorkspace {
    parent: PathBuf,
    main: PathBuf,
    provider: Option<PathBuf>,
}

impl TestWorkspace {
    fn new(main_source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_generic_method_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let main = parent.join("main");
        fs::create_dir_all(&main).expect("create main package");
        fs::write(main.join("main.omg"), main_source).expect("write root module");
        Self {
            parent,
            main,
            provider: None,
        }
    }

    fn with_provider(mut self, source: &str) -> Self {
        let provider = self.parent.join("provider");
        fs::create_dir_all(&provider).expect("create provider package");
        fs::write(provider.join("provider.omg"), source).expect("write provider module");
        self.provider = Some(provider);
        self
    }

    fn externs(&self) -> Vec<ExternRoot> {
        self.provider
            .iter()
            .map(|dir| ExternRoot {
                name: Ident("provider".to_string()),
                dir: dir.clone(),
            })
            .collect()
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.main.clone(), None, self.externs(), Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.compile() {
            Ok(_) => panic!("expected this to be rejected, but it compiled"),
            Err(errors) => errors,
        }
    }

    /// The symbol and linkage of every function this package defines.
    fn definitions(&self) -> Vec<(String, omega_mir::MirLinkage)> {
        let program = match self.compile() {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
        };
        let entry = program.entry.clone();
        omega_mir::lower_program(program.modules, &entry)
            .into_iter()
            .flat_map(|(_, module)| module.items)
            .flat_map(|item| match item {
                omega_mir::MirItem::FunctionDefinition(f) => vec![(f.symbol, f.linkage)],
                omega_mir::MirItem::Struct(s) => s
                    .functions
                    .into_iter()
                    .map(|f| (f.symbol, f.linkage))
                    .collect(),
                omega_mir::MirItem::Union(u) => u
                    .functions
                    .into_iter()
                    .map(|f| (f.symbol, f.linkage))
                    .collect(),
                omega_mir::MirItem::Enum(e) => e
                    .functions
                    .into_iter()
                    .map(|f| (f.symbol, f.linkage))
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// The symbols of the instantiations of `name`, which are the definitions
    /// whose demangled path names it.
    fn instantiations_of(&self, name: &str) -> Vec<String> {
        let needle = format!("::{name}<");
        self.definitions()
            .into_iter()
            .map(|(symbol, _)| omega_mangle::demangle(&symbol).unwrap_or(symbol))
            .filter(|symbol| symbol.contains(&needle))
            .collect()
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn resolve_errors(errors: &[CompileError]) -> Vec<String> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => {
                errors.iter().map(|error| error.kind.to_string()).collect()
            }
            CompileError::Resolve { error, .. } => vec![error.to_string()],
            other => vec![format!("{other:?}")],
        })
        .collect()
}

#[test]
fn each_argument_list_produces_its_own_instantiation() {
    let workspace = TestWorkspace::new(
        r#"
        struct Holder {
            exposed value: i32;
            exposed echo<T>(*self, thing: T) => T { thing }
        }
        main() => void {
            h := Holder { value = 1; };
            a := h.echo(1u8);
            b := h.echo(2u32);
            c := h.echo(3u8);
        }
        "#,
    );
    let mut instantiations = workspace.instantiations_of("echo");
    instantiations.sort();
    assert_eq!(
        instantiations,
        vec![
            "main::Holder::self::echo<u32>(*main::Holder, u32) -> u32".to_string(),
            "main::Holder::self::echo<u8>(*main::Holder, u8) -> u8".to_string(),
        ],
        "two argument lists, and the repeated one shares its instantiation",
    );
}

#[test]
fn an_instantiation_is_weak_so_two_packages_can_define_it() {
    let workspace = TestWorkspace::new(
        r#"
        import provider::Holder;
        main() => void {
            h := Holder { value = 1; };
            a := h.echo(1u8);
        }
        "#,
    )
    .with_provider(
        r#"
        exposed struct Holder {
            exposed value: i32;
            exposed echo<T>(*self, thing: T) => T { thing }
        }
        "#,
    );
    let instantiated: Vec<_> = workspace
        .definitions()
        .into_iter()
        .filter(|(symbol, _)| {
            omega_mangle::demangle(symbol).is_some_and(|name| name.contains("::echo<"))
        })
        .collect();
    let [(_, linkage)] = instantiated.as_slice() else {
        panic!("expected exactly one instantiation, got {instantiated:#?}");
    };
    assert_eq!(*linkage, omega_mir::MirLinkage::Weak);
}

#[test]
fn an_uninstantiated_template_emits_nothing() {
    let workspace = TestWorkspace::new(
        r#"
        struct Holder {
            exposed value: i32;
            exposed echo<T>(*self, thing: T) => T { thing }
        }
        main() => void {
            h := Holder { value = 1; };
        }
        "#,
    );
    assert!(workspace.instantiations_of("echo").is_empty());
}

#[test]
fn an_owner_instantiation_is_part_of_a_method_instantiation_identity() {
    let workspace = TestWorkspace::new(
        r#"
        struct Pair<A> {
            exposed a: A;
            exposed with<B>(*self, b: B) => B { b }
        }
        main() => void {
            first := Pair<i32> { a = 1; };
            second := Pair<u8> { a = 2u8; };
            x := first.with(3u8);
            y := second.with(4u8);
        }
        "#,
    );
    let mut instantiations = workspace.instantiations_of("with");
    instantiations.sort();
    assert_eq!(
        instantiations,
        vec![
            "main::Pair<i32>::self::with<u8>(*main::Pair<i32>, u8) -> u8".to_string(),
            "main::Pair<u8>::self::with<u8>(*main::Pair<u8>, u8) -> u8".to_string(),
        ],
    );
}

#[test]
fn two_generic_declarations_of_one_name_cannot_be_ranked() {
    let errors = TestWorkspace::new(
        r#"
        struct Holder {
            exposed value: i32;
            exposed twin<T>(*self, thing: T) => T { thing }
            exposed twin<T>(*self, first: T, second: T) => T { second }
        }
        main() => void {
            h := Holder { value = 1; };
            x := h.twin(1);
        }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|message| message.contains("declares more than one generic 'twin'")),
        "expected the generic-overload rejection, got: {:#?}",
        resolve_errors(&errors)
    );
}

#[test]
fn a_static_spec_parameter_makes_a_method_a_template() {
    // `f(x: spec S)` normalizes into an anonymous bounded generic, so a
    // method written that way is instantiated per argument type like any
    // other generic declaration.
    let workspace = TestWorkspace::new(
        r#"
        exposed spec Describable {
            describe(*self) => *str;
        }
        struct Widget { exposed name: *str; }
        meet Describable for Widget {
            describe(*self) => *str { self.name }
        }
        struct Holder {
            exposed value: i32;
            exposed tell(*self, item: spec Describable) => *str { item.describe() }
        }
        main() => void {
            h := Holder { value = 1; };
            w := Widget { name = "widget"; };
            told := h.tell(w);
        }
        "#,
    );
    assert_eq!(workspace.instantiations_of("tell").len(), 1);
}
