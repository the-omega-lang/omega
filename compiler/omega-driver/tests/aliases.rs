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
fn an_unused_structural_alias_with_an_unknown_target_is_rejected_at_declaration() {
    // `Bogus` is never used anywhere, but its target must still be validated:
    // an alias declaration is not just a lazy forwarding rule.
    let errors = TestPackage::new(
        r#"
        alias Bogus = NoSuchType;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::UnknownItem { item, .. } if item.as_ref() == "NoSuchType")),
        "an unused alias's target must still be validated: {}",
        rendered(&errors)
    );
}

#[test]
fn an_unused_generic_alias_with_an_unknown_nested_target_is_rejected_at_declaration() {
    let errors = TestPackage::new(
        r#"
        struct Holder<T> {
            exposed value: T;
        }

        alias Bogus<T> = Holder<NoSuchType>;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::UnknownItem { item, .. } if item.as_ref() == "NoSuchType")),
        "an unused generic alias's nested target must still be validated: {}",
        rendered(&errors)
    );
}

#[test]
fn an_unused_alias_generic_bound_naming_an_unknown_spec_is_rejected_at_declaration() {
    let errors = TestPackage::new(
        r#"
        struct Holder<T> {
            exposed value: T;
        }

        alias Bogus<T: NoSuchSpec> = Holder<T>;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::UnknownItem { item, .. } if item.as_ref() == "NoSuchSpec")),
        "an unused alias's own generic bound must still be validated: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_owned_generic_parameter_is_a_legal_placeholder_in_its_own_bounds_and_defaults() {
    // A structural alias's target, bounds, and defaults may all freely
    // reference the alias's own generic parameters without those references
    // ever being treated as missing declarations.
    TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Pair<A, B> {
            exposed a: A;
            exposed b: B;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Both<A: Countable, B: Countable = A> = Pair<A, B>;

        entry_fn() => i32 { 0 }
        "#,
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
fn a_qualified_reference_to_a_hidden_type_reports_not_visible_not_unknown_module() {
    // `Hidden` itself is not exposed, even though its `new` static is. The
    // module prefix `helper::Hidden` is not a module, so resolution retries
    // it as `helper::Hidden::new` -- the retry must surface the real
    // `NotVisible` cause instead of falling back to a misleading
    // `UnknownModule`.
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 {
            value := helper::Hidden::new(3);
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
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::NotVisible { item, .. } if item.as_ref() == "Hidden")),
        "a qualified reference through a hidden type must report NotVisible, not UnknownModule: {}",
        rendered(&errors)
    );
    assert!(
        !resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::UnknownModule(path) if path.last().map(Ident::as_ref) == Some("Hidden"))),
        "the misleading UnknownModule reading must not surface once the real cause is known: {}",
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
fn a_plain_path_alias_can_be_directly_imported() {
    TestPackage::with_modules(
        r#"
        import self::helper::Public;

        entry_fn() => i32 {
            value: Public = Public { field = 3; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            struct Hidden {
                exposed field: i32;
            }

            exposed alias Public = Hidden;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_structural_generic_alias_can_be_directly_imported_and_used_bare() {
    TestPackage::with_modules(
        r#"
        import self::helper::Counted;

        entry_fn() => i32 {
            value: Counted<i32> = Counted<i32> { value = 3; };
            value.value
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Holder<T> {
                exposed value: T;
            }

            exposed alias Counted<T> = Holder<T>;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_hidden_alias_cannot_be_directly_imported() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper::Secret;

        entry_fn() => i32 {
            value: Secret = Secret { field = 1; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            struct Hidden {
                exposed field: i32;
            }

            alias Secret = Hidden;
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::NotVisible { item, .. } if item.as_ref() == "Secret")),
        "a hidden alias must not be importable: {}",
        rendered(&errors)
    );
}

#[test]
fn a_self_anchored_alias_reference_resolves_in_the_current_module() {
    TestPackage::new(
        r#"
        struct Holder<T> {
            exposed value: T;
        }

        alias Counted<T> = Holder<T>;

        entry_fn() => i32 {
            value: self::Counted<i32> = self::Counted<i32> { value = 3; };
            value.value
        }
        "#,
    )
    .expect_ok();
}

#[test]
fn a_generic_alias_with_a_self_anchored_target_declares_and_expands() {
    TestPackage::new(
        r#"
        struct Holder<T> {
            exposed value: T;
        }

        alias Counted<T> = self::Holder<T>;

        entry_fn() => i32 {
            value: Counted<i32> = Counted<i32> { value = 3; };
            value.value
        }
        "#,
    )
    .expect_ok();
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
            ParseErrorKind::Expected {
                expected: "a type",
                ..
            }
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
fn a_hidden_intermediate_alias_in_a_cross_module_chain_is_rejected() {
    // `helper::Secret` is hidden, so `main`'s alias may not forward through
    // it even though `Secret`'s own target (`Hidden`) would otherwise be
    // reachable via the chain -- each alias in a cross-module chain is its
    // own visibility gate.
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        alias Rewrapped = helper::Secret;

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Hidden {
                exposed field: i32;
            }

            alias Secret = Hidden;
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::NotVisible { item, .. } if item.as_ref() == "Secret")),
        "a hidden intermediate alias must not be forwarded through: {}",
        rendered(&errors)
    );
}

#[test]
fn a_visible_intermediate_alias_in_a_cross_module_chain_still_re_exports() {
    // The mirror of the previous case: once every link in the chain is
    // visible from where it is named, the chain must still work end to end.
    TestPackage::with_modules(
        r#"
        import self::helper;

        alias Rewrapped = helper::Public;

        entry_fn() => i32 {
            value: Rewrapped = Rewrapped { field = 4; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            struct Hidden {
                exposed field: i32;
            }

            exposed alias Public = Hidden;
            "#,
        )],
    )
    .expect_ok();
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
fn a_defaulted_alias_generic_argument_works_in_aggregate_construction_position() {
    // No outer type annotation seeds the expected type here, so the struct
    // literal `Counted { ... }` must resolve its own generic identity purely
    // through item-position lookup rather than inheriting an already-expanded
    // type from a declared annotation.
    TestPackage::new(
        r#"
        struct Holder<T> {
            exposed value: T;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Counted<T = Plain> = Holder<T>;

        entry_fn() => i32 {
            value := Counted { value = Plain { field = 7; }; };
            value.value.field
        }
        "#,
    )
    .expect_ok();
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
fn an_exposed_macro_alias_transfers_only_the_capability_to_invoke() {
    TestPackage::with_modules(
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

            # The alias re-exports the name; expansion still happens with
            # `helper`'s own rights, so the hidden dependency stays hidden.
            exposed alias public_seeded = seeded;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_macro_alias_naming_a_hidden_cross_module_macro_is_rejected() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        alias sneaky = helper::hidden_helper_macro;

        entry_fn() => i32 {
            sneaky$(1)
        }
        "#,
        &[(
            "helper",
            r#"
            macro hidden_helper_macro($extra: expr) => {
                $extra
            }
            "#,
        )],
    )
    .expect_errors();
    assert!(
        !rendered(&errors).is_empty(),
        "a macro alias must not be able to name a hidden cross-module macro"
    );
}

#[test]
fn a_macro_alias_naming_a_visible_cross_module_macro_still_expands() {
    TestPackage::with_modules(
        r#"
        import self::helper;

        alias visible = helper::exposed_helper_macro;

        entry_fn() => i32 {
            visible$(3)
        }
        "#,
        &[(
            "helper",
            r#"
            exposed macro exposed_helper_macro($extra: expr) => {
                $extra
            }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_self_anchored_macro_alias_target_resolves() {
    TestPackage::new(
        r#"
        macro shout($extra: expr) => {
            $extra
        }

        alias echo = self::shout;

        entry_fn() => i32 {
            echo$(5)
        }
        "#,
    )
    .expect_ok();
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
fn an_exposed_alias_re_exports_a_hidden_cross_module_overload_set() {
    // Every `show` overload is hidden in `helper`; the alias itself is
    // exposed. `main` may call through the alias even though it could never
    // name `helper::show` directly.
    TestPackage::with_modules(
        r#"
        import self::helper::render;

        entry_fn() => i32 {
            render(2) + render(true)
        }
        "#,
        &[(
            "helper",
            r#"
            show(value: i32) => i32 { value }
            show(value: bool) => i32 { 1 }

            exposed alias render = show;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_import_used_only_through_an_alias_rhs_is_not_reported_unused() {
    let program = TestPackage::with_modules(
        r#"
        import self::helper;

        alias Local = helper::Remote;

        entry_fn() => i32 {
            value: Local = Local { field = 1; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Remote {
                exposed field: i32;
            }
            "#,
        )],
    )
    .expect_ok();
    assert!(
        !program.warnings.iter().any(|(_, warning)| matches!(
            warning.kind,
            omega_analyzer::error::AnalysisWarningKind::UnusedImport { .. }
        )),
        "an import consumed only through an alias RHS must not warn as unused: {:?}",
        program
            .warnings
            .iter()
            .map(|(_, w)| &w.kind)
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_import_used_only_through_an_alias_expanded_bound_is_not_reported_unused() {
    let program = TestPackage::with_modules(
        r#"
        import self::helper;

        alias Conjoined = spec helper::Countable + helper::Named;

        thing<T: Conjoined>(value: T) => i32 { 0 }

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            exposed spec Countable {
                count(*self) => i32;
            }

            exposed spec Named {
                name(*self) => i32;
            }
            "#,
        )],
    )
    .expect_ok();
    assert!(
        !program.warnings.iter().any(|(_, warning)| matches!(
            warning.kind,
            omega_analyzer::error::AnalysisWarningKind::UnusedImport { .. }
        )),
        "an import consumed only through an alias-expanded conjunction bound must not warn as unused: {:?}",
        program
            .warnings
            .iter()
            .map(|(_, w)| &w.kind)
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_import_used_only_through_a_directly_imported_alias_is_not_reported_unused() {
    let program = TestPackage::with_modules(
        r#"
        import self::helper::Local;

        entry_fn() => i32 {
            value: Local = Local { field = 1; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Remote {
                exposed field: i32;
            }

            exposed alias Local = Remote;
            "#,
        )],
    )
    .expect_ok();
    assert!(
        !program.warnings.iter().any(|(_, warning)| matches!(
            warning.kind,
            omega_analyzer::error::AnalysisWarningKind::UnusedImport { .. }
        )),
        "an import naming an alias directly must not warn as unused: {:?}",
        program
            .warnings
            .iter()
            .map(|(_, w)| &w.kind)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_genuinely_unused_import_still_warns() {
    let program = TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Remote {
                exposed field: i32;
            }
            "#,
        )],
    )
    .expect_ok();
    assert!(
        program.warnings.iter().any(|(_, warning)| matches!(
            warning.kind,
            omega_analyzer::error::AnalysisWarningKind::UnusedImport { .. }
        )),
        "a genuinely unused import must still warn: {:?}",
        program
            .warnings
            .iter()
            .map(|(_, w)| &w.kind)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_module_qualified_reference_to_an_aliased_hidden_overload_set_still_works() {
    // `helper::render` names an alias, not a plain function, so the
    // module-qualified call path must recognize it as one too and forward
    // the frozen candidate set instead of re-checking each candidate's own
    // (hidden) visibility against `main`.
    TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 {
            helper::render(2) + helper::render(true)
        }
        "#,
        &[(
            "helper",
            r#"
            show(value: i32) => i32 { value }
            show(value: bool) => i32 { 1 }

            exposed alias render = show;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_hidden_alias_of_an_overload_set_cannot_be_directly_imported() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper::render;

        entry_fn() => i32 {
            render(2) + render(true)
        }
        "#,
        &[(
            "helper",
            r#"
            exposed show(value: i32) => i32 { value }
            exposed show(value: bool) => i32 { 1 }

            alias render = show;
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors)
            .iter()
            .any(|error| matches!(error, ResolveError::NotVisible { item, .. } if item.as_ref() == "render")),
        "a hidden alias of an overload set must not be importable: {}",
        rendered(&errors)
    );
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

        meet A for Foo { a(*self) => i32 { self.tally } }
        meet B for Foo { b(*self) => i32 { self.tally } }

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

#[test]
fn a_revealed_import_of_a_hidden_alias_resolves_lazily() {
    // The import gate is passed by `reveal`; the alias's own gate must not
    // then reject the very same reference when the lazy path is finally
    // resolved.
    TestPackage::with_modules(
        r#"
        import reveal self::helper::Secret;

        entry_fn() => i32 {
            value: Secret = Secret { field = 1; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Hidden {
                exposed field: i32;
            }

            alias Secret = Hidden;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_revealed_import_of_a_hidden_structural_alias_resolves_lazily() {
    TestPackage::with_modules(
        r#"
        import reveal self::helper::Counted;

        entry_fn() => i32 {
            value: Counted<i32> = Counted<i32> { value = 3; };
            value.value
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Holder<T> {
                exposed value: T;
            }

            alias Counted<T> = Holder<T>;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_hidden_structural_alias_cannot_be_imported_without_reveal() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper::Counted;

        entry_fn() => i32 {
            value: Counted<i32> = Counted<i32> { value = 3; };
            value.value
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Holder<T> {
                exposed value: T;
            }

            alias Counted<T> = Holder<T>;
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::NotVisible { item, .. } if item.as_ref() == "Counted"
        )),
        "a hidden structural alias must not be importable: {}",
        rendered(&errors)
    );
}

#[test]
fn a_revealed_import_of_a_hidden_generic_template_resolves_lazily() {
    // Not an alias at all: the same lazy import path carries the same
    // authorization for an ordinary generic template.
    TestPackage::with_modules(
        r#"
        import reveal self::helper::Holder;

        entry_fn() => i32 {
            value: Holder<i32> = Holder<i32> { value = 3; };
            value.value
        }
        "#,
        &[(
            "helper",
            r#"
            struct Holder<T> {
                exposed value: T;
            }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_qualified_reference_to_a_hidden_structural_alias_is_rejected() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        entry_fn() => i32 {
            value: helper::Counted<i32> = helper::Counted<i32> { value = 3; };
            value.value
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Holder<T> {
                exposed value: T;
            }

            alias Counted<T> = Holder<T>;
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::NotVisible { item, .. } if item.as_ref() == "Counted"
        )),
        "a hidden structural alias is not nameable from another module: {}",
        rendered(&errors)
    );
}

#[test]
fn a_structural_alias_whose_rhs_names_an_inaccessible_structural_alias_is_rejected() {
    let errors = TestPackage::with_modules(
        r#"
        import self::outer;

        entry_fn() => i32 {
            value: outer::Visible<i32> = outer::Visible<i32> { value = 3; };
            value.value
        }
        "#,
        &[
            (
                "outer",
                r#"
                import super::inner;

                exposed alias Visible<T> = inner::Secret<T>;
                "#,
            ),
            (
                "inner",
                r#"
                exposed struct Holder<T> {
                    exposed value: T;
                }

                alias Secret<T> = Holder<T>;
                "#,
            ),
        ],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::NotVisible { item, .. } if item.as_ref() == "Secret"
        )),
        "an outer alias may not smuggle a hidden structural alias: {}",
        rendered(&errors)
    );
}

#[test]
fn a_visible_structural_alias_chain_re_exports_without_exposing_the_final_target() {
    // Each link is visible from the module that names it; `main` never has
    // to be able to see `inner::Holder` itself.
    TestPackage::with_modules(
        r#"
        import self::outer::Visible;

        entry_fn() => i32 {
            value: Visible<i32> = Visible<i32> { value = 3; };
            value.value
        }
        "#,
        &[
            (
                "outer",
                r#"
                import super::inner;

                exposed alias Visible<T> = inner::Shared<T>;
                "#,
            ),
            (
                "inner",
                r#"
                struct Holder<T> {
                    exposed value: T;
                }

                exposed alias Shared<T> = Holder<T>;
                "#,
            ),
        ],
    )
    .expect_ok();
}

#[test]
fn an_alias_bound_reached_only_through_another_alias_rhs_is_enforced() {
    // `Inner`'s own bound is never written at the use site: it appears only
    // inside `Outer`'s right-hand side, and must still be checked against
    // the argument `Outer` was actually given.
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Pair<A, B> {
            exposed first: A;
            exposed second: B;
        }

        struct Holder<T> {
            exposed value: T;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Inner<T: Countable> = Holder<T>;
        alias Outer<T> = Pair<Inner<T>, T>;

        take(value: *Outer<Plain>) => i32 { 0 }

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable"
        )),
        "a bound owned by an alias nested in another alias's RHS must be checked: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_bound_reached_only_through_a_default_is_enforced() {
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

        alias Inner<T: Countable> = Holder<T>;
        alias Defaulted<T = Inner<Plain>> = Holder<T>;

        take(value: *Defaulted) => i32 { 0 }

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable"
        )),
        "an alias introduced by a default still owns its bounds: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_bound_referring_to_an_earlier_alias_parameter_is_enforced() {
    let errors = TestPackage::new(
        r#"
        exposed spec Convert<T> {
            convert(*self) => i32;
        }

        struct Pair<A, B> {
            exposed first: A;
            exposed second: B;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Linked<A, B: Convert<A>> = Pair<A, B>;

        take(value: *Linked<i32, Plain>) => i32 { 0 }

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Convert"
        )),
        "a bound naming an earlier alias parameter must be checked against it: {}",
        rendered(&errors)
    );
}

#[test]
fn an_argument_substituted_twice_reports_its_bound_failure_once() {
    // `Duo<T> = Pair<T, T>` substitutes the same argument into two
    // positions. The argument is normalized once, before substitution, so
    // its own alias-owned bound failure is reported once rather than once
    // per occurrence.
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Pair<A, B> {
            exposed first: A;
            exposed second: B;
        }

        struct Holder<T> {
            exposed value: T;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Inner<T: Countable> = Holder<T>;
        alias Duo<T> = Pair<T, T>;

        take(value: *Duo<Inner<Plain>>) => i32 { 0 }

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    let failures: Vec<_> = resolve_errors(&errors)
        .into_iter()
        .filter(|error| {
            matches!(error, ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable")
        })
        .collect();
    assert_eq!(
        failures.len(),
        1,
        "one written argument must produce one bound diagnostic: {}",
        rendered(&errors)
    );
}

#[test]
fn an_unused_alias_with_too_few_generic_arguments_is_rejected_at_declaration() {
    let errors = TestPackage::new(
        r#"
        struct Pair<A, B> {
            exposed first: A;
            exposed second: B;
        }

        alias Halved<T> = Pair<T>;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::GenericArgCountMismatch { item, expected: 2, found: 1, .. }
                if item.as_ref() == "Pair"
        )),
        "a malformed application must be reported even in an unused alias: {}",
        rendered(&errors)
    );
}

#[test]
fn an_unused_alias_with_too_many_generic_arguments_is_rejected_at_declaration() {
    let errors = TestPackage::new(
        r#"
        struct Holder<T> {
            exposed value: T;
        }

        alias Overfull<T> = Holder<T, T>;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::GenericArgCountMismatch { item, expected: 1, found: 2, .. }
                if item.as_ref() == "Holder"
        )),
        "too many arguments must be reported at the alias declaration: {}",
        rendered(&errors)
    );
}

#[test]
fn a_function_inside_structural_alias_syntax_is_not_a_type() {
    let errors = TestPackage::new(
        r#"
        helper() => i32 { 0 }

        alias Bad<T> = *helper;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::InvalidAliasTarget { kind, .. } if *kind == "the function"
        )),
        "a function is not a type, even where a bare alias could name it: {}",
        rendered(&errors)
    );
}

#[test]
fn a_module_inside_structural_alias_syntax_is_not_a_type() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        alias Bad<T> = *helper;

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Remote {
                exposed field: i32;
            }
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::InvalidAliasTarget { kind, .. } if *kind == "the module"
        )),
        "a module is not a type: {}",
        rendered(&errors)
    );
}

#[test]
fn a_non_spec_member_of_a_conjunction_alias_is_rejected_at_declaration() {
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Conjoined = spec Countable + Plain;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::InvalidAliasTarget { kind, .. } if *kind == "the non-spec declaration"
        )),
        "a conjunction member must be a spec: {}",
        rendered(&errors)
    );
}

#[test]
fn an_unused_alias_naming_an_inaccessible_qualified_target_is_rejected() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper;

        alias Bad<T> = helper::Hidden<T>;

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            struct Hidden<T> {
                exposed value: T;
            }
            "#,
        )],
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::NotVisible { item, .. } if item.as_ref() == "Hidden"
        )),
        "an inaccessible target must be rejected at the declaration: {}",
        rendered(&errors)
    );
}

#[test]
fn an_unused_alias_with_an_invalid_nested_generic_application_is_rejected() {
    let errors = TestPackage::new(
        r#"
        struct Pair<A, B> {
            exposed first: A;
            exposed second: B;
        }

        struct Holder<T> {
            exposed value: T;
        }

        alias Bad<T> = Holder<Pair<T>>;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::GenericArgCountMismatch { item, expected: 2, found: 1, .. }
                if item.as_ref() == "Pair"
        )),
        "a nested application is validated too: {}",
        rendered(&errors)
    );
}

#[test]
fn an_unused_alias_whose_unqualified_target_comes_from_an_import_validates() {
    // `Remote` is not declared in `main`; it is bound there by the import.
    // Declaration validation must use the same binding rules a use site
    // would, rather than assuming an unqualified target is local.
    TestPackage::with_modules(
        r#"
        import self::helper::Remote;

        alias Local<T> = Remote<T>;

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Remote<T> {
                exposed value: T;
            }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_structural_alias_may_name_a_fully_qualified_top_level_path_without_importing_it() {
    // Naming `main::helper::Holder` must not also require importing
    // `main`, and the target must still resolve at every use site.
    TestPackage::with_modules(
        r#"
        alias Wrapped<T> = main::helper::Holder<T>;

        entry_fn() => i32 {
            value: Wrapped<i32> = Wrapped<i32> { value = 3; };
            value.value
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Holder<T> {
                exposed value: T;
            }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_overload_alias_forwards_only_the_candidates_its_declaration_site_can_name() {
    // `provider` exposes one `show` and hides the other. `exporter`'s alias
    // freezes exactly what `exporter` can name, and `main` gets that set --
    // never the hidden candidate, and never less than the exposed one.
    TestPackage::with_modules(
        r#"
        import self::exporter::render;

        entry_fn() => i32 {
            render(2)
        }
        "#,
        &[
            (
                "exporter",
                r#"
                import super::provider;

                exposed alias render = provider::show;
                "#,
            ),
            (
                "provider",
                r#"
                exposed show(value: i32) => i32 { value }

                show(value: bool) => i32 { 1 }
                "#,
            ),
        ],
    )
    .expect_ok();
}

#[test]
fn an_overload_alias_never_forwards_a_candidate_its_declaration_site_cannot_name() {
    let errors = TestPackage::with_modules(
        r#"
        import self::exporter::render;

        entry_fn() => i32 {
            render(true)
        }
        "#,
        &[
            (
                "exporter",
                r#"
                import super::provider;

                exposed alias render = provider::show;
                "#,
            ),
            (
                "provider",
                r#"
                exposed show(value: i32) => i32 { value }

                show(value: bool) => i32 { 1 }
                "#,
            ),
        ],
    )
    .expect_errors();
    assert!(
        analysis_errors(&errors)
            .iter()
            .any(|kind| matches!(kind, AnalysisErrorKind::NoMatchingOverload { name, .. } if name.as_ref() == "show")),
        "the hidden candidate must not be reachable through the alias: {}",
        rendered(&errors)
    );
}

#[test]
fn an_overload_alias_chain_does_not_reintroduce_hidden_candidates() {
    let errors = TestPackage::with_modules(
        r#"
        import self::relay::again;

        entry_fn() => i32 {
            again(true)
        }
        "#,
        &[
            (
                "relay",
                r#"
                import super::exporter;

                exposed alias again = exporter::render;
                "#,
            ),
            (
                "exporter",
                r#"
                import super::provider;

                exposed alias render = provider::show;
                "#,
            ),
            (
                "provider",
                r#"
                exposed show(value: i32) => i32 { value }

                show(value: bool) => i32 { 1 }
                "#,
            ),
        ],
    )
    .expect_errors();
    assert!(
        analysis_errors(&errors)
            .iter()
            .any(|kind| matches!(kind, AnalysisErrorKind::NoMatchingOverload { name, .. } if name.as_ref() == "show")),
        "an alias chain must forward the already-frozen set, not reopen the group: {}",
        rendered(&errors)
    );
}

#[test]
fn a_direct_import_of_an_overload_set_keeps_caller_side_filtering() {
    let errors = TestPackage::with_modules(
        r#"
        import self::provider::show;

        entry_fn() => i32 {
            show(2) + show(true)
        }
        "#,
        &[(
            "provider",
            r#"
            exposed show(value: i32) => i32 { value }

            show(value: bool) => i32 { 1 }
            "#,
        )],
    )
    .expect_errors();
    assert!(
        analysis_errors(&errors)
            .iter()
            .any(|kind| matches!(kind, AnalysisErrorKind::NoMatchingOverload { name, .. } if name.as_ref() == "show")),
        "a plain import still filters candidates against the caller: {}",
        rendered(&errors)
    );
}

#[test]
fn a_revealed_import_of_an_overload_set_bypasses_candidate_visibility() {
    TestPackage::with_modules(
        r#"
        import reveal self::provider::show;

        entry_fn() => i32 {
            show(2) + show(true)
        }
        "#,
        &[(
            "provider",
            r#"
            exposed show(value: i32) => i32 { value }

            show(value: bool) => i32 { 1 }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_overload_selected_as_a_value_uses_the_same_frozen_set_as_a_call() {
    // Selecting a candidate by expected function type must see exactly the
    // candidates a call through the same alias would see.
    TestPackage::with_modules(
        r#"
        import self::exporter::render;

        entry_fn() => i32 {
            chosen: (value: i32) => i32 = render;
            chosen(2)
        }
        "#,
        &[
            (
                "exporter",
                r#"
                import super::provider;

                exposed alias render = provider::show;
                "#,
            ),
            (
                "provider",
                r#"
                exposed show(value: i32) => i32 { value }

                show(value: bool) => i32 { 1 }
                "#,
            ),
        ],
    )
    .expect_ok();
}

#[test]
fn an_alias_may_name_a_macro_reached_through_an_import() {
    // The import binds `shout` in `main` exactly as a local definition
    // would, so a bare alias target may name it; expansion still uses the
    // original definition module for hygiene.
    TestPackage::with_modules(
        r#"
        import self::helper::shout;

        alias echo = shout;

        entry_fn() => i32 {
            echo$(5)
        }
        "#,
        &[(
            "helper",
            r#"
            exposed macro shout($extra: expr) => {
                $extra
            }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_alias_cannot_name_a_hidden_macro_imported_without_reveal() {
    let errors = TestPackage::with_modules(
        r#"
        import self::helper::whisper;

        alias echo = whisper;

        entry_fn() => i32 {
            echo$(5)
        }
        "#,
        &[(
            "helper",
            r#"
            macro whisper($extra: expr) => {
                $extra
            }
            "#,
        )],
    )
    .expect_errors();
    assert!(
        !rendered(&errors).is_empty(),
        "a hidden imported macro must not become nameable through an alias"
    );
}

#[test]
fn a_revealed_import_makes_a_hidden_macro_aliasable() {
    TestPackage::with_modules(
        r#"
        import reveal self::helper::whisper;

        alias echo = whisper;

        entry_fn() => i32 {
            echo$(5)
        }
        "#,
        &[(
            "helper",
            r#"
            macro whisper($extra: expr) => {
                $extra
            }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_import_used_only_by_a_static_spec_parameter_is_not_reported_unused() {
    // `f(x: spec S)` normalizes into a generic bound, and that bound is the
    // only consumer of the import. Import accounting must follow the
    // normalized signature, not the written generics.
    let program = TestPackage::with_modules(
        r#"
        import self::helper;

        take(value: spec helper::Countable) => i32 { 0 }

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            exposed spec Countable {
                count(*self) => i32;
            }
            "#,
        )],
    )
    .expect_ok();
    assert!(
        !program.warnings.iter().any(|(_, warning)| matches!(
            warning.kind,
            omega_analyzer::error::AnalysisWarningKind::UnusedImport { .. }
        )),
        "a static-spec parameter consumes its import: {:?}",
        program
            .warnings
            .iter()
            .map(|(_, w)| &w.kind)
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_import_used_only_by_an_aliased_static_spec_parameter_is_not_reported_unused() {
    let program = TestPackage::with_modules(
        r#"
        import self::helper;

        alias Bound = spec helper::Countable;

        take(value: Bound) => i32 { 0 }

        entry_fn() => i32 { 0 }
        "#,
        &[(
            "helper",
            r#"
            exposed spec Countable {
                count(*self) => i32;
            }
            "#,
        )],
    )
    .expect_ok();
    assert!(
        !program.warnings.iter().any(|(_, warning)| matches!(
            warning.kind,
            omega_analyzer::error::AnalysisWarningKind::UnusedImport { .. }
        )),
        "the aliased spelling must consume the import the same way: {:?}",
        program
            .warnings
            .iter()
            .map(|(_, w)| &w.kind)
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_aliased_static_spec_parameter_reports_the_real_spec_not_a_synthesized_parameter() {
    // Static-spec normalization introduces a synthesized parameter name.
    // It is an internal identity and must never reach a source-facing
    // diagnostic, whether the parameter was written literally or through an
    // alias.
    let errors = TestPackage::new(
        r#"
        exposed spec Countable {
            count(*self) => i32;
        }

        struct Plain {
            exposed field: i32;
        }

        alias Bound = spec Countable;

        take(value: Bound) => i32 { 0 }

        entry_fn() => i32 {
            take(Plain { field = 1; })
        }
        "#,
    )
    .expect_errors();
    let rendered = rendered(&errors);
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::SpecNotImplemented { spec, .. } if spec.as_ref() == "Countable"
        )),
        "the real spec must be named: {rendered}"
    );
    assert!(
        !rendered.contains("$Param"),
        "a synthesized generic parameter must not appear in a diagnostic: {rendered}"
    );
}

#[test]
fn an_alias_carries_its_authorization_into_a_generic_call() {
    // A locally declared alias resolves to its target's own absolute path,
    // so the authorization the alias chain established travels with that
    // path: the generic-call route must not re-gate the hidden target
    // against this module, which could never name it directly.
    TestPackage::with_modules(
        r#"
        import self::helper;

        alias same = helper::exported;

        entry_fn() => i32 {
            same(41) + 1
        }
        "#,
        &[(
            "helper",
            r#"
            identity<T>(value: T) => T { value }

            exposed alias exported = identity;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_alias_carries_its_authorization_into_aggregate_construction() {
    TestPackage::with_modules(
        r#"
        import self::helper;

        alias Counted = helper::exported;

        entry_fn() => i32 {
            explicit := Counted<i32> { value = 3; };
            inferred := Counted { value = 4; };
            explicit.value + inferred.value
        }
        "#,
        &[(
            "helper",
            r#"
            struct Holder<T> {
                exposed value: T;
            }

            exposed alias exported = Holder;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_alias_carries_its_authorization_into_a_static_member_call() {
    TestPackage::with_modules(
        r#"
        import self::helper;

        alias Counted = helper::exported;

        entry_fn() => i32 {
            explicit := Counted<i32>::make(4);
            inferred := Counted::make(5);
            explicit.value + inferred.value
        }
        "#,
        &[(
            "helper",
            r#"
            struct Holder<T> {
                exposed value: T;

                exposed make(value: T) => Self {
                    Self { value = value; }
                }
            }

            exposed alias exported = Holder;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_revealed_hidden_alias_can_be_realiased_without_losing_authorization() {
    TestPackage::with_modules(
        r#"
        import reveal self::helper::Secret;

        alias Local = Secret;

        entry_fn() => i32 {
            value: Local = Local { field = 7; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Hidden {
                exposed field: i32;
            }

            alias Secret = Hidden;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_revealed_hidden_structural_alias_can_be_realiased_without_losing_authorization() {
    TestPackage::with_modules(
        r#"
        import reveal self::helper::Secret;

        alias Local<T> = Secret<T>;

        entry_fn() => i32 {
            value: Local<i32> = Local<i32> { field = 7; };
            value.field
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Holder<T> {
                exposed field: T;
            }

            alias Secret<T> = Holder<T>;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_local_overload_alias_keeps_its_declaration_site_candidate_set() {
    let errors = TestPackage::with_modules(
        r#"
        import self::exporter;

        entry_fn() => i32 {
            exporter::exercise()
        }
        "#,
        &[
            (
                "exporter",
                r#"
                import super::provider;

                alias render = provider::show;

                exposed exercise() => i32 {
                    render(true)
                }
                "#,
            ),
            (
                "provider",
                r#"
                exposed show(value: i32) => i32 { value }

                show(value: bool) => i32 { 1 }
                "#,
            ),
        ],
    )
    .expect_errors();
    assert!(
        analysis_errors(&errors).iter().any(|kind| matches!(
            kind,
            AnalysisErrorKind::NoMatchingOverload { name, .. } if name.as_ref() == "show"
        )),
        "local use must not re-expand a frozen overload alias to hidden candidates: {}",
        rendered(&errors)
    );
}

#[test]
fn an_anchored_module_alias_is_a_module_binding_in_type_literal_and_value_paths() {
    TestPackage::with_modules(
        r#"
        alias kid = self::child;

        entry_fn() => i32 {
            value: self::kid::Public = self::kid::Public { field = 5; };
            self::kid::read(&value)
        }
        "#,
        &[(
            "child",
            r#"
            exposed struct Public {
                exposed field: i32;
            }

            exposed read(value: *Public) => i32 { value.field }
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn a_directly_imported_module_alias_behaves_like_the_target_module() {
    TestPackage::with_modules(
        r#"
        import self::exporter::api;

        entry_fn() => i32 {
            value: api::Public = api::Public { field = 6; };
            api::read(&value)
        }
        "#,
        &[
            (
                "exporter",
                r#"
                exposed alias api = super::provider;
                "#,
            ),
            (
                "provider",
                r#"
                exposed struct Public {
                    exposed field: i32;
                }

                exposed read(value: *Public) => i32 { value.field }
                "#,
            ),
        ],
    )
    .expect_ok();
}

#[test]
fn a_revealed_imported_alias_keeps_authorization_for_an_overloaded_static_call() {
    TestPackage::with_modules(
        r#"
        import reveal self::helper::Secret;

        entry_fn() => i32 {
            Secret::pick(true)
        }
        "#,
        &[(
            "helper",
            r#"
            exposed struct Holder {
                exposed field: i32;

                exposed pick(value: i32) => i32 { value }
                exposed pick(value: bool) => i32 { 1 }
            }

            alias Secret = Holder;
            "#,
        )],
    )
    .expect_ok();
}

#[test]
fn an_alias_owned_generic_bound_must_name_a_spec_even_if_unused() {
    let errors = TestPackage::new(
        r#"
        struct Holder {}

        alias Bad<T: Holder> = T;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::InvalidAliasTarget { kind, .. } if *kind == "the non-spec declaration"
        )),
        "a generic bound may not accept an ordinary type merely because its name resolves: {}",
        rendered(&errors)
    );
}

#[test]
fn an_alias_of_a_non_spec_cannot_masquerade_as_a_spec_member() {
    let errors = TestPackage::new(
        r#"
        struct Holder<T> {}

        alias Fake<T> = Holder<T>;
        alias Bad = spec Fake<i32>;

        entry_fn() => i32 { 0 }
        "#,
    )
    .expect_errors();
    assert!(
        resolve_errors(&errors).iter().any(|error| matches!(
            error,
            ResolveError::InvalidAliasTarget { kind, .. }
                if *kind == "the non-spec declaration" || *kind == "the non-spec type"
        )),
        "a structural alias used in spec position must resolve to a spec: {}",
        rendered(&errors)
    );
}

#[test]
fn an_import_can_traverse_an_intermediate_module_alias() {
    TestPackage::with_modules(
        r#"
        import self::exporter::api::Public;
        import self::exporter::api::read;

        entry_fn() => i32 {
            value: Public = Public { field = 9; };
            read(&value)
        }
        "#,
        &[
            (
                "exporter",
                r#"
                exposed alias api = super::provider;
                "#,
            ),
            (
                "provider",
                r#"
                exposed struct Public {
                    exposed field: i32;
                }

                exposed read(value: *Public) => i32 { value.field }
                "#,
            ),
        ],
    )
    .expect_ok();
}
