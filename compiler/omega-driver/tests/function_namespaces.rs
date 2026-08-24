use omega_analyzer::Target;
use omega_analyzer::error::AnalysisErrorKind;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// A workspace holding a `main` package and, optionally, a separately
/// compiled `provider` package reached as an extern root.
struct TestWorkspace {
    parent: PathBuf,
    main: PathBuf,
    provider: Option<PathBuf>,
}

impl TestWorkspace {
    fn new(main_source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_fn_namespace_test_{}_{}",
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

    /// Compiles the provider on its own, exactly as a separate invocation
    /// would, and returns the symbols it defines.
    fn provider_symbols(&self) -> Vec<String> {
        let dir = self.provider.clone().expect("workspace has a provider");
        let program = Driver::new(dir, None, Vec::new(), Target::DEFAULT)
            .expect("construct provider driver")
            .compile(&[Ident("provider".to_string())], Target::DEFAULT)
            .expect("provider compiles on its own");
        let entry = program.entry.clone();
        omega_mir::lower_program(program.modules, &entry)
            .into_iter()
            .flat_map(|(_, module)| module.items)
            .flat_map(|item| match item {
                omega_mir::MirItem::FunctionDefinition(f) => vec![f.symbol],
                omega_mir::MirItem::Struct(s) => {
                    s.functions.into_iter().map(|f| f.symbol).collect()
                }
                omega_mir::MirItem::Union(u) => u.functions.into_iter().map(|f| f.symbol).collect(),
                omega_mir::MirItem::Enum(e) => e.functions.into_iter().map(|f| f.symbol).collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    fn expect_ok(&self) -> omega_driver::CompiledProgram {
        match self.compile() {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
        }
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.compile() {
            Ok(_) => panic!("expected this to be rejected, but it compiled"),
            Err(errors) => errors,
        }
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn has_analysis_error(
    errors: &[CompileError],
    predicate: impl Fn(&AnalysisErrorKind) -> bool,
) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors.iter().any(|error| predicate(&error.kind)),
        _ => false,
    })
}

#[test]
fn a_static_and_a_member_may_share_a_name_and_parameter_shape() {
    // The static's explicit `*Thing` parameter is exactly the member's
    // synthetic `*self` parameter, which is the pair the single-namespace
    // rule used to reject.
    TestWorkspace::new(
        r#"
        struct Thing {
            exposed v: i32;
            exposed same(other: *Thing) => i32 { other.v }
            exposed same(*self) => i32 { self.v }
        }
        main() => void { }
        "#,
    )
    .expect_ok();
}

#[test]
fn a_duplicate_inside_one_namespace_is_still_rejected() {
    let errors = TestWorkspace::new(
        r#"
        struct Thing {
            exposed v: i32;
            exposed same(other: *Thing) => i32 { other.v }
            exposed same(again: *Thing) => i32 { again.v }
        }
        main() => void { }
        "#,
    )
    .expect_errors();
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::Redeclaration { .. }
    )));
}

#[test]
fn receiver_spelling_alone_is_still_not_an_overload_selector() {
    let errors = TestWorkspace::new(
        r#"
        struct Thing {
            exposed v: i32;
            exposed same(*self) => i32 { self.v }
            exposed same(*mut self) => i32 { self.v }
        }
        main() => void { }
        "#,
    )
    .expect_errors();
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::AmbiguousSelfOverload { .. }
    )));
}

#[test]
fn a_union_and_a_named_enum_get_the_same_namespace_independence() {
    TestWorkspace::new(
        r#"
        union Bits {
            exposed word: u32;
            exposed halve(word: u32) => u32 { word }
            exposed halve(*self) => u32 { self.word }
        }

        enum Signal {
            Idle,
            Busy;

            exposed label(s: *Signal) => i32 { 0 }
            exposed label(*self) => i32 { 1 }
        }

        main() => void { }
        "#,
    )
    .expect_ok();
}

#[test]
fn an_inherent_static_does_not_hide_a_conforming_member() {
    // Precedence is per namespace: the inherent `describe` occupies only the
    // static namespace, leaving `Thing::self::describe` to the conformance.
    TestWorkspace::new(
        r#"
        struct Thing {
            exposed v: i32;
            exposed describe(other: *Thing) => i32 { other.v }
        }

        exposed spec Described {
            describe(*self) => i32;
        }

        conform Thing to Described {
            describe(*self) => i32 { self.v + 1 }
        }

        main() => void {
            t := Thing { v = 1; };
            <void>Thing::describe(&t);
            <void>Thing::self::describe(&t);
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn a_member_reference_links_to_the_providers_member_symbol() {
    // The mangled member path is a cross-process contract: a separately
    // compiled consumer has to reconstruct the provider's exact symbol, and
    // the same-signature static sibling has to stay a different one.
    let workspace = TestWorkspace::new(
        r#"
        import provider::Thing;

        main() => void {
            t := Thing { v = 1; };
            <void>Thing::same(&t);
            <void>Thing::self::same(&t);
        }
        "#,
    )
    .with_provider(
        r#"
        exposed struct Thing {
            exposed v: i32;
            exposed same(other: *Thing) => i32 { other.v }
            exposed same(*self) => i32 { self.v }
        }
        "#,
    );

    let defined = workspace.provider_symbols();
    let member_definitions: Vec<&String> = defined
        .iter()
        .filter(|symbol| symbol.contains("4self"))
        .collect();
    assert_eq!(
        member_definitions.len(),
        1,
        "exactly one definition nests under `self`, got {defined:?}"
    );
    let static_definitions: Vec<&String> = defined
        .iter()
        .filter(|symbol| symbol.contains("4same") && !symbol.contains("4self"))
        .collect();
    assert_eq!(static_definitions.len(), 1, "got {defined:?}");
    assert_ne!(member_definitions[0], static_definitions[0]);

    let referenced: Vec<String> = workspace
        .expect_ok()
        .extern_functions
        .iter()
        .map(omega_mir::mangle::extern_function_ref_symbol)
        .collect();
    assert!(
        referenced.contains(member_definitions[0]),
        "consumer must reference the provider's member symbol; referenced {referenced:?}"
    );
    assert!(
        referenced.contains(static_definitions[0]),
        "consumer must reference the provider's static symbol; referenced {referenced:?}"
    );
}
