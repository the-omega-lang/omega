use omega_analyzer::Target;
use omega_analyzer::error::AnalysisErrorKind;
use omega_analyzer::resolver::ResolveError;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::diagnostics::ParseErrorKind;
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        Self::with_modules(source, &[])
    }

    fn with_modules(source: &str, modules: &[(&str, &str)]) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_alias_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        for (name, contents) in modules {
            fs::write(root.join(format!("{name}.omg")), contents).expect("write child module");
        }
        Self(root)
    }

    fn result(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(
            self.0.clone(),
            None,
            vec![ExternRoot {
                name: Ident("core".to_string()),
                dir: core_root(),
            }],
            Target::DEFAULT,
        )
        .expect("construct driver with the real core extern")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }

    fn expect_ok(&self) -> omega_driver::CompiledProgram {
        match self.result() {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
        }
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.result() {
            Ok(_) => panic!("expected this to be rejected, but it compiled"),
            Err(errors) => errors,
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn core_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/core")
        .canonicalize()
        .expect("runtime/core exists")
}

fn analysis_errors(errors: &[CompileError]) -> Vec<AnalysisErrorKind> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => {
                errors.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
            }
            _ => vec![],
        })
        .collect()
}

fn resolve_errors(errors: &[CompileError]) -> Vec<ResolveError> {
    analysis_errors(errors)
        .into_iter()
        .filter_map(|kind| match kind {
            AnalysisErrorKind::ModuleResolution(error) => Some(error),
            AnalysisErrorKind::UnresolvedType(
                omega_analyzer::error::TypeResolutionError::ModuleResolution(error),
            ) => Some(error),
            _ => None,
        })
        .collect()
}

fn rendered(errors: &[CompileError]) -> String {
    format!("{errors:#?}")
}

fn parse_errors(errors: &[CompileError]) -> Vec<ParseErrorKind> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Parse { errors, .. } => {
                errors.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
            }
            _ => vec![],
        })
        .collect()
}

#[test]
fn an_alias_target_resolves_at_its_declaration_site_not_at_the_use_site() {
    // `Secret` exists only in `helper`, and `main` never imports it. Naming it
    // through the alias must still work, and must not accidentally make the
    // use site resolve `Secret` in its own module.
    TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 {
            value: helper::Named = helper::make(4);
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Secret {
                exposed field: i32;
            }

            exposed alias Named = Secret;

            exposed make(field: i32) => Named {
                Named { field = field; }
            }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_exposed_alias_re_exports_a_hidden_declaration() {
    TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 {
            value := helper::Public::new(3);
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            struct Hidden {
                exposed field: i32;

                exposed new(field: i32) => Self {
                    Self { field = field; }
                }
            }

            exposed alias Public = Hidden;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn the_alias_is_its_own_visibility_gate() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 {
            value: helper::Local = helper::Open::new(3);
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Open {
                exposed field: i32;

                exposed new(field: i32) => Self {
                    Self { field = field; }
                }
            }

            alias Local = Open;
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::NotVisible { item, .. } if item.as_ref() == "Local")),
        "a hidden alias of an exposed declaration is still hidden: {}",
        rendered(&errors)
    );
}

#[test]
fn a_module_alias_does_not_expose_hidden_children() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        alias h = helper;

        entry_fn() => i32 {
            value: h::Hidden = h::make(3);
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            struct Hidden {
                exposed field: i32;
            }

            exposed make(field: i32) => i32 { field }
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::NotVisible { item, .. } if item.as_ref() == "Hidden")),
        "traversing a module alias still checks the item's own visibility: {}",
        rendered(&errors)
    );
}

#[test]
fn an_inaccessible_target_is_rejected_at_the_alias_declaration() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 { 0 }
        "#,
        &[
            (
                "helper",
                r#"
                import root::other;

                exposed alias Borrowed = other::Hidden;
                "#,
            ),
            (
                "other",
                r#"
                struct Hidden {
                    exposed field: i32;
                }
                "#,
            ),
        ],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::NotVisible { item, .. } if item.as_ref() == "Hidden")),
        "an alias cannot name what its own module cannot see: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_cannot_name_a_global_or_a_compile_time_value() {
    for source in [
        r#"
        counter: i32 = 0;

        alias Counter = counter;

        entry_fn() => i32 { 0 }
        "#,
        r#"
        comp LIMIT := 10;

        alias Limit = LIMIT;

        entry_fn() => i32 { 0 }
        "#,
    ] {
        let errors = TestPackage::new(source).expect_errors();
        assert!(
            resolve_errors(&errors)
                .iter()
                .any(|error| matches!(error, ResolveError::InvalidAliasTarget { .. })),
            "a value is not a nameable declaration: {}",
            rendered(&errors)
        );
    }
}

#[test]
fn an_alias_cannot_name_an_unsupported_declaration_kind() {
    let errors = TestPackage::new(
        r#"
        gap Clock {
            now() => i32;
        }

        alias Timer = Clock;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    let found = resolve_errors(&errors);
    assert!(
        found.iter().any(|error| matches!(
            error,
            ResolveError::InvalidAliasTarget { kind, .. } if *kind == "the gap"
        )),
        "expected the gap to be named as the rejected kind: {}",
        rendered(&errors)
    );
}

#[test]
fn a_local_alias_is_rejected() {
    let errors = TestPackage::new(
        r#"
        entry_fn() => i32 {
            alias Count = i32;
            0
        }
        "#,
    )
    .expect_errors();
    assert!(
        parse_errors(&errors)
            .iter()
            .any(|kind| matches!(kind, ParseErrorKind::AliasNotAllowedHere)),
        "expected AliasNotAllowedHere: {}",
        rendered(&errors)
    );
}

#[test]
fn an_expression_shaped_alias_target_is_rejected() {
    let errors = TestPackage::new(
        r#"
        alias Bad = 1 + 2;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        parse_errors(&errors).iter().any(|kind| matches!(
            kind,
            ParseErrorKind::Expected { expected: "a type", .. }
        )),
        "expected a type-position parse error: {}",
        rendered(&errors)
    );
}

#[test]
fn a_direct_alias_cycle_is_rejected() {
    let errors = TestPackage::new(
        r#"
        alias Loop = Loop;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::Cycle(_))),
        "expected a cycle diagnostic: {}",
        rendered(&errors)
    );
}

#[test]
fn a_cycle_behind_a_pointer_is_still_a_cycle() {
    // An alias has no nominal cell, so the ordinary recursive-type indirection
    // exception does not apply: there is nothing for `*Loop` to point at.
    let errors = TestPackage::new(
        r#"
        alias Loop = *Loop;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::Cycle(_))),
        "expected a cycle diagnostic: {}",
        rendered(&errors)
    );
}

#[test]
fn a_multi_hop_alias_cycle_reports_the_whole_chain_in_order() {
    let errors = TestPackage::new(
        r#"
        alias First = Second;
        alias Second = Third;
        alias Third = First;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    let names: Vec<Vec<String>> = resolve_errors(&errors)
        .into_iter()
        .filter_map(|error| match error {
            ResolveError::Cycle(path) => Some(
                path.into_iter()
                    .map(|segments| {
                        segments
                            .last()
                            .expect("a cycle entry names an item")
                            .as_ref()
                            .to_string()
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect();
    assert!(
        names.iter().any(|chain| {
            chain.first() == chain.last() && chain.len() == 4 && chain.contains(&"Second".into())
        }),
        "expected the resolution order to be preserved, got {names:?}"
    );
}

#[test]
fn a_generic_alias_checks_argument_count() {
    let errors = TestPackage::new(
        r#"
        struct Pair<A, B> {
            exposed a: A;
            exposed b: B;
        }

        alias Keyed<V> = Pair<i32, V>;

        entry_fn() => i32 {
            value: Keyed<i32, i32> = Keyed<i32, i32> { a = 1; b = 2; };
            value.a
        }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::GenericArgCountMismatch { item, expected: 1, found: 2, .. }
                if item.as_ref() == "Keyed"
        )),
        "expected an arity mismatch against the alias's own parameter list: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_owned_generic_bound_is_enforced() {
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Holder<T> {
            exposed value: T;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Counted<T: Countable> = Holder<T>;

        entry_fn() => i32 {
            value: Counted<Plain> = Counted<Plain> { value = Plain { field = 1; }; };
            value.value.field
        }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable"
        )),
        "expected the alias-owned bound to be checked: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_owned_generic_bound_is_enforced_in_a_plain_type_position() {
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Holder<T> {
            exposed value: T;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Counted<T: Countable> = Holder<T>;

        take(holder: *Counted<Plain>) => i32 {
            holder.value.field
        }

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable"
        )),
        "an alias bound is checked wherever the alias is written: {}",
        rendered(&errors)
    );
}

#[test]
fn a_defaulted_alias_generic_argument_still_satisfies_its_own_bound() {
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Holder<T> {
            exposed value: T;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Counted<T: Countable = Plain> = Holder<T>;

        entry_fn() => i32 {
            value: Counted = Counted { value = Plain { field = 1; }; };
            value.value.field
        }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable"
        )),
        "a defaulted alias argument must still be checked against its own bound: {}",
        rendered(&errors)
    );
}

#[test]
fn a_chained_alias_bound_is_enforced_from_every_link() {
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Holder<T> {
            exposed value: T;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Counted<T: Countable> = Holder<T>;
        alias Rewrapped<U: Countable> = Counted<U>;

        entry_fn() => i32 {
            value: Rewrapped<Plain> = Rewrapped<Plain> { value = Plain { field = 1; }; };
            value.value.field
        }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable"
        )),
        "every alias in a chain must have its own bound obligations checked: {}",
        rendered(&errors)
    );
}

#[test]
fn an_exposed_macro_alias_cannot_smuggle_a_narrower_dependency() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper::public_seeded;

        entry_fn() => i32 {
            public_seeded$(1)
        }
        "#,
        &[(
            "helper",
            r#"
            hidden_seed() => i32 { 7 }

            macro seeded($extra: expr) => {
                hidden_seed() + ($extra)
            }

            exposed alias public_seeded = seeded;
            "#,
        )],
    )
    .expect_errors();
    assert!(
        analysis_errors(&errors)
            .iter()
            .any(|kind| matches!(kind, AnalysisErrorKind::MacroDependencyTooPrivate { .. })),
        "an exposed alias makes the macro exposed for dependency checks: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_forwards_a_whole_overload_set() {
    TestPackage::new(
        r#"
        show(value: i32) => i32 { value }
        show(value: bool) => i32 { 1 }

        alias render = show;

        entry_fn() => i32 {
            render(2) + render(true)
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn an_alias_forwards_generic_inference_rather_than_fixing_one_instantiation() {
    TestPackage::new(
        r#"
        identity<T>(value: T) => T { value }

        alias same = identity;

        entry_fn() => i32 {
            flag := same(true);
            if flag { same(1) } else { 0 }
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn an_aliased_static_spec_parameter_matches_the_literal_spelling() {
    // Both functions must synthesize the same anonymous bounded generic, so
    // the aliased spelling accepts exactly what the literal one accepts.
    TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B { b(*self) => i32; }

        struct Foo { exposed tally: i32; }

        conform Foo to A { a(*self) => i32 { self.tally } }
        conform Foo to B { b(*self) => i32 { self.tally } }

        alias AB = spec A + B;

        literal(value: spec A + B) => i32 { value.a() + value.b() }
        aliased(value: AB) => i32 { value.a() + value.b() }

        entry_fn() => i32 {
            foo := Foo { tally = 1; };
            literal(foo) + aliased(foo)
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn an_alias_produces_no_symbol_of_its_own() {
    let program = TestPackage::new(
        r#"
        struct Pair<A, B> {
            exposed a: A;
            exposed b: B;
        }

        alias IntPair = Pair<i32, i32>;

        make() => IntPair {
            IntPair { a = 1; b = 2; }
        }

        entry_fn() => i32 {
            make().a
        }
        "#,
    )
    .expect_ok();
    let rendered = format!("{:?}", program.modules);
    assert!(
        !rendered.contains("IntPair"),
        "no alias name may survive into the checked tree"
    );
}
