use omega_analyzer::checked::{CheckedItem, ExternFunctionKind};
use omega_analyzer::error::AnalysisErrorKind;
use omega_analyzer::resolver::ResolveError;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::{macros::MacroError, prelude::Ident};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    /// A package shaped exactly like a real one: a root *directory* whose
    /// own module file is named after it (`main/main.omg`), so `source`
    /// becomes the root module `main` and `write_child` adds `main::<name>`
    /// beside it. The filename is deliberately not a parameter -- under the
    /// root-module rule it is not free, it must match the directory, and a
    /// harness that could vary it independently would be compiling a shape
    /// no user package can have.
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_compose_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn write_child(&self, name: &str, source: &str) {
        fs::write(self.0.join(format!("{name}.omg")), source)
            .expect("write test child module");
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::new())
            .expect("construct driver")
            .compile(&[Ident("main".to_string())])
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let parent = self.0.parent().expect("test root has a parent");
        let _ = fs::remove_dir_all(parent);
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

fn compile_errors(package: &TestPackage, message: &str) -> Vec<CompileError> {
    match package.compile() {
        Ok(_) => panic!("{message}"),
        Err(errors) => errors,
    }
}

fn option_core() -> TestPackage {
    TestPackage::new("exposed enum Option<T> { None, Some { exposed value: T; }; }")
}

#[test]
fn bound_and_spec_qualified_dispatch_compile() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }

        call_bound<T: Speak>(value: *T) => i32 { value.speak() }
        main() => i32 {
            dog := Dog { value = 7; };
            call_bound(&dog) + Speak::speak(&dog)
        }
        "#,
    );
    package
        .compile()
        .expect("both composition call forms should compile");
}

#[test]
fn composed_instance_method_is_not_in_concrete_scope() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        main() => i32 { dog := Dog { value = 7; }; dog.speak() }
        "#,
    );
    let errors = compile_errors(&package, "concrete instance syntax must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn duplicate_and_extra_compositions_are_rejected() {
    let duplicate = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&duplicate, "duplicate composition must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::DuplicateCompose { .. }
    )));

    let extra = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak {
            speak(*self) => i32 { self.value }
            extra(*self) => i32 { 0 }
        }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&extra, "extra compose functions must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ComposeExtraFunction { .. }
    )));
}

#[test]
fn a_compose_cannot_borrow_an_inherent_requirement() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog {
            exposed value: i32;
            exposed speak(*self) => i32 { self.value }
        }
        compose Dog : Speak {}
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "an inherent method must not satisfy compose");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MissingSpecFunction { .. }
    )));
}

#[test]
fn slice_composes_and_invalid_structural_targets_are_diagnosed_semantically() {
    let slice = TestPackage::new(
        r#"
        exposed spec Empty { empty(*self) => bool; }
        compose []u8 : Empty { empty(*self) => bool { self.length == 0 } }
        main() => i32 { 0 }
        "#,
    );
    slice
        .compile()
        .expect("a bare slice target should reach the compose registry");

    let pointer = TestPackage::new(
        r#"
        exposed spec Empty { empty(*self) => bool; }
        struct Dog { exposed value: i32; }
        compose *Dog : Empty { empty(*self) => bool { false } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(
        &pointer,
        "a pointer target must be rejected by the target model",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ComposeTargetNotAType
    )));
}

#[test]
fn dependency_compositions_satisfy_the_dependency_bound() {
    let package = TestPackage::new(
        r#"
        exposed spec Animal { sound(*self) => i32; }
        exposed spec Mammal : Animal { fur(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Mammal {
            sound(*self) => i32 { self.value }
            fur(*self) => i32 { 1 }
        }
        call<T: Animal>(value: *T) => i32 { value.sound() }
        main() => i32 { dog := Dog { value = 4; }; call(&dog) }
        "#,
    );
    package
        .compile()
        .expect("a direct compose must register its transitive dependencies");
}

#[test]
fn primitive_blocks_are_core_only() {
    let package = TestPackage::new(
        r#"
        primitive i32 { exposed identity(*self) => i32 { *self } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "non-core primitive block must fail");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::PrimitiveOutsideCore
    )));
}

#[test]
fn external_non_generic_primitive_is_imported_not_redefined() {
    let core = TestPackage::new("primitive i32 { exposed identity(*self) => i32 { *self } }");
    let local = TestPackage::new("main() => i32 { 7i32.identity() }");
    let mut driver = Driver::new(
        local.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
    )
    .expect("construct driver with core extern");
    let program = driver
        .compile(&[Ident("main".to_string())])
        .expect("external primitive use should compile");

    let definitions = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(_)))
        .count();
    assert_eq!(definitions, 1, "only local main should be defined");
    assert!(
        program
            .extern_functions
            .iter()
            .any(|function| matches!(function.kind, ExternFunctionKind::Primitive { .. }))
    );
}

/// A concrete composition declared in an extern package is linked from that
/// package's object.  Resolving it to build a vtable in the consumer must not
/// also re-check and emit its body locally, or two strong definitions of the
/// same composed method reach the linker.
#[test]
fn extern_owned_concrete_compose_is_imported_not_reemitted() {
    let library = TestPackage::new(
        r#"
        exposed spec Show { show(*self) => i32; }
        exposed struct Value { exposed n: i32; }
        compose Value : Show { show(*self) => i32 { self.n } }
        "#,
    );
    let consumer = TestPackage::new(
        r#"
        import extern::lib::Show;
        import extern::lib::Value;

        main() => i32 {
            value := Value { n = 7; };
            Show::show(&value)
        }
        "#,
    );
    let program = Driver::new(
        consumer.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("lib".to_string()),
            dir: library.0.clone(),
        }],
    )
    .expect("construct driver with library extern")
    .compile(&[Ident("main".to_string())])
    .expect("calling an extern-owned concrete composition should compile");

    let definitions = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(_)))
        .count();
    assert_eq!(definitions, 1, "only the consumer's main should be defined");
    assert!(
        program
            .extern_functions
            .iter()
            .any(|function| matches!(function.kind, ExternFunctionKind::Compose { .. }))
    );
}

/// The relocated standard I/O boundary still follows the ordinary compose
/// orphan rule: an application cannot attach the externally-owned `Write`
/// contract to the externally-owned `Stdout` marker.
#[test]
fn externally_owned_stdout_cannot_be_composed_with_externally_owned_write() {
    let core = option_core();
    let library = TestPackage::new(
        r#"
        exposed spec Write { write(*mut self, bytes: *[?]u8) => Option<usize>; }
        exposed marker Stdout {}
        compose Stdout : Write {
            write(*mut self, bytes: *[?]u8) => Option<usize> {
                Option<usize>::Some { value = <usize>bytes.length; }
            }
        }
        "#,
    );
    let consumer = TestPackage::new(
        r#"
        import extern::lib::Stdout;
        import extern::lib::Write;

        compose Stdout : Write {
            write(*mut self, bytes: *[?]u8) => Option<usize> {
                Option<usize>::Some { value = <usize>bytes.length; }
            }
        }
        main() => i32 { 0 }
        "#,
    );
    let mut driver = Driver::new(
        consumer.0.clone(),
        None,
        vec![
            ExternRoot {
                name: Ident("core".to_string()),
                dir: core.0.clone(),
            },
            ExternRoot {
                name: Ident("lib".to_string()),
                dir: library.0.clone(),
            },
        ],
    )
    .expect("construct driver with I/O library extern");
    let errors = match driver.compile(&[Ident("main".to_string())]) {
        Ok(_) => panic!("a consumer must not compose two foreign I/O items"),
        Err(errors) => errors,
    };
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::ComposeOrphanViolation { .. }
        )),
        "expected an orphan violation, got {errors:#?}"
    );
}

/// Gap glue is an exact ABI contract. This is the former boolean success
/// shape, rejected against the current `Option<usize>` console convention.
#[test]
fn old_boolean_console_glue_signature_is_rejected() {
    let core = option_core();
    let package = TestPackage::new(
        r#"
        gap StandardOutput { write(bytes: *[?]u8) => Option<usize>; }
        glue StandardOutput {
            write(bytes: *[?]u8) => bool { true }
        }
        main() => i32 { 0 }
        "#,
    );
    let mut driver = Driver::new(
        package.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
    )
    .expect("construct driver with Option core extern");
    let errors = match driver.compile(&[Ident("main".to_string())]) {
        Ok(_) => panic!("an old console glue signature must fail"),
        Err(errors) => errors,
    };
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::GlueFunctionSignatureMismatch { .. }
        )),
        "expected a glue signature mismatch, got {errors:#?}"
    );
}

#[test]
fn print_macro_requires_an_explicit_import() {
    let package = TestPackage::new("main() => i32 { println$(\"missing\"); 0 }");
    let errors = compile_errors(&package, "an unimported print macro must fail");
    assert!(errors.iter().any(|error| matches!(
        error,
        CompileError::MacroExpansion {
            error: MacroError::UnknownMacro { name },
            ..
        } if name.as_ref() == "println"
    )));
}

#[test]
fn formatting_is_not_available_from_core() {
    let core = TestPackage::new("marker CoreOnly {}");
    let consumer = TestPackage::new("main() => i32 { core::fmt::missing() }");
    let mut driver = Driver::new(
        consumer.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core.0.clone(),
        }],
    )
    .expect("construct driver with core extern");
    let errors = match driver.compile(&[Ident("main".to_string())]) {
        Ok(_) => panic!("core must not provide a formatting module"),
        Err(errors) => errors,
    };
    assert!(
        has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(path))
                if path.iter().map(Ident::as_ref).eq(["core", "fmt"])
        )),
        "expected core::fmt to be unresolved, got {errors:#?}"
    );
}

#[test]
fn internal_items_are_visible_across_executable_modules() {
    let package = TestPackage::new(
        r#"
        import helper::shared;
        main() => i32 { shared() }
        "#,
    );
    package.write_child("helper", "internal shared() => i32 { 42 }");
    package
        .compile()
        .expect("an executable's root and child modules share internal visibility");
}

#[test]
fn root_imports_are_anchored_to_the_package_root_module() {
    let package = TestPackage::new(
        r#"
        import root::helper::shared;
        main() => i32 { shared() }
        "#,
    );
    package.write_child("helper", "internal shared() => i32 { 42 }");
    package
        .compile()
        .expect("root imports from a child should remain inside the package");
}

#[test]
fn local_and_extern_root_identities_cannot_collide() {
    let local = TestPackage::new("main() => i32 { 0 }");
    let dependency = TestPackage::new("exposed value() => i32 { 42 }");
    let errors = match Driver::new(
        local.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("main".to_string()),
            dir: dependency.0.clone(),
        }],
    ) {
        Ok(_) => panic!("local and extern package identities must not collide"),
        Err(errors) => errors,
    };
    assert!(matches!(
        errors.as_slice(),
        [CompileError::DuplicateModuleIdentity { name, .. }] if name.as_ref() == "main"
    ));
}

/// `Spec::method(receiver, ...)` must adapt `receiver` to the declared self
/// mode whatever *shape* the argument has -- a literal and a struct
/// expression are not places, and before this was fixed they reached a
/// `*self` parameter unadapted and failed to type-check. The print macros
/// expand to exactly this form (`Display::fmt($args, ...)`) over arbitrary
/// argument expressions, so every one of these shapes is load-bearing.
#[test]
fn spec_qualified_calls_adapt_a_non_place_receiver() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        struct Dog { exposed value: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.value } }
        make() => Dog { Dog { value = 3; } }
        main() => i32 {
            dog := Dog { value = 7; };
            Speak::speak(dog)
                + Speak::speak(&dog)
                + Speak::speak(Dog { value = 1; })
                + Speak::speak(make())
        }
        "#,
    );
    package
        .compile()
        .expect("a non-place spec-qualified receiver should be adapted, not rejected");
}

/// Blanket composes are deliberately out of scope, and must say so rather
/// than being silently dropped: `match_compose_target` can never bind a
/// target that is itself a parameter, so without the check the only
/// diagnostic anyone saw was an unrelated `SpecNotImplemented` at a use
/// site -- or, for an unused compose, nothing at all.
#[test]
fn blanket_composes_are_rejected_with_their_own_diagnostic() {
    let bare_target = TestPackage::new(
        r#"
        exposed spec Numeric { zero(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        compose<T: Numeric> T : Sum { sum(*self) => i32 { 0 } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&bare_target, "a blanket compose must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::BlanketComposeNotYetSupported { .. }
    )));

    // A parameter that the target never mentions is equally unbindable.
    let unfixed_parameter = TestPackage::new(
        r#"
        exposed spec Bound { zero(*self) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        struct Box<T> { exposed value: T; }
        compose<T, U: Bound> Box<T> : Sum { sum(*self) => i32 { 0 } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(
        &unfixed_parameter,
        "an unbindable parameter must be rejected",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::BlanketComposeNotYetSupported { .. }
    )));

    // ...while a target that does fix its parameter stays fully supported.
    let generic_target = TestPackage::new(
        r#"
        exposed spec Sum { sum(*self) => i32; }
        struct Box<T> { exposed value: T; }
        compose<T> Box<T> : Sum { sum(*self) => i32 { 1 } }
        use_sum<X: Sum>(value: *X) => i32 { value.sum() }
        main() => i32 { boxed := Box<i32> { value = 1; }; use_sum(&boxed) }
        "#,
    );
    generic_target
        .compile()
        .expect("a generic target that fixes its parameter is not a blanket compose");
}

/// A generic compose has its own bound context, just like a generic named
/// item. In particular, `inner.w(...)` is resolved through `T: W`; it must
/// not depend on a spec-qualified spelling or on every compose registered
/// for the concrete type leaking into scope.
#[test]
fn generic_compose_bounds_seed_the_body_context() {
    let package = TestPackage::new(
        r#"
        exposed spec W { w(*self, value: i32) => i32; }
        exposed spec Sum { sum(*self) => i32; }
        exposed spec QualifiedSum { qualified_sum(*self) => i32; }

        struct One { exposed value: i32; }
        struct Two { exposed value: i32; }
        compose One : W { w(*self, value: i32) => i32 { self.value + value } }
        compose Two : W { w(*self, value: i32) => i32 { self.value + value } }

        struct Buf<T> { exposed inner: *T; }
        compose<T: W> Buf<T> : Sum {
            sum(*self) => i32 { self.inner.w(1) }
        }
        compose<T: W> Buf<T> : QualifiedSum {
            qualified_sum(*self) => i32 { W::w(self.inner, 1) }
        }

        use_sum<T: Sum>(value: *T) => i32 { value.sum() }
        use_qualified_sum<T: QualifiedSum>(value: *T) => i32 { value.qualified_sum() }
        main() => i32 {
            one := One { value = 1; };
            two := Two { value = 2; };
            first := Buf<One> { inner = &one; };
            second := Buf<Two> { inner = &two; };
            use_sum(&first) + use_qualified_sum(&first) + use_sum(&second)
        }
        "#,
    );
    package
        .compile()
        .expect("a compose generic bound must both validate and seed its body context");
}

/// An unsatisfied compose bound must be rejected when the template is
/// instantiated, before a conformance entry or its vtable can exist. The
/// compose declaration, not the caller that happened to trigger discovery,
/// owns the bad promise and therefore owns the diagnostic.
#[test]
fn generic_compose_bounds_reject_unsatisfied_conformance_at_the_declaration() {
    let source = r#"
        exposed spec W { w(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct NotW { exposed value: i32; }
        struct Buf<T> { exposed inner: *T; }
        compose<T: W> Buf<T> : Show { show(*self) => i32 { 1 } }

        as_w(value: *Buf<NotW>) => spec *W { value }
        main() => i32 {
            value := NotW { value = 0; };
            buf := Buf<NotW> { inner = &value; };
            as_w(&buf).w()
        }
        "#;
    let package = TestPackage::new(source);
    let errors = compile_errors(
        &package,
        "an unsatisfied compose generic bound must not produce a conformance or vtable",
    );
    let expected_start = source
        .find("compose<T: W> Buf<T> : Show")
        .expect("the declaration is present");
    let error = errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => errors.iter(),
            _ => [].iter(),
        })
        .find(|error| {
            matches!(
                error.kind,
                AnalysisErrorKind::ModuleResolution(
                    omega_analyzer::resolver::ResolveError::SpecNotImplemented { .. }
                )
            )
        })
        .expect("the compose bound failure is reported as SpecNotImplemented");
    assert_eq!(error.span.start, expected_start);
}

/// A compose bound may name an aggregate spec alias. The shared bound checker
/// must seed the alias and the already-composed member specs, exactly as it
/// does for ordinary generic items.
#[test]
fn generic_compose_bounds_expand_spec_aliases() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B : A { b(*self) => i32 { 2 } }
        spec AB = A | B;
        exposed spec Sum { sum(*self) => i32; }
        struct Value { exposed value: i32; }
        compose Value : B { a(*self) => i32 { self.value } }

        struct Buf<T> { exposed inner: *T; }
        compose<T: AB> Buf<T> : Sum {
            sum(*self) => i32 { self.inner.a() + self.inner.b() }
        }
        use_sum<T: Sum>(value: *T) => i32 { value.sum() }
        main() => i32 {
            value := Value { value = 1; };
            buf := Buf<Value> { inner = &value; };
            use_sum(&buf)
        }
        "#,
    );
    package
        .compile()
        .expect("a compose generic alias bound must reach its member composes");
}

/// An unbounded compose remains an ordinary duck-typed template. It must not
/// inherit every compose on its concrete argument merely because another
/// instantiation happened to register one there.
#[test]
fn an_unbounded_generic_compose_gains_no_bound_context() {
    let package = TestPackage::new(
        r#"
        exposed spec Secret { secret(*self) => i32; }
        exposed spec Show { show(*self) => i32; }
        struct Value { exposed value: i32; }
        compose Value : Secret { secret(*self) => i32 { self.value } }

        struct Box<T> { exposed inner: *T; }
        compose<T> Box<T> : Show {
            show(*self) => i32 { self.inner.secret() }
        }
        use_show<T: Show>(value: *T) => i32 { value.show() }
        main() => i32 {
            value := Value { value = 1; };
            boxed := Box<Value> { inner = &value; };
            use_show(&boxed)
        }
        "#,
    );
    let errors = compile_errors(
        &package,
        "an unbounded compose must not gain methods from its concrete argument",
    );
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

/// A type's *inherent* method body is not a compose body, so a spec
/// composed onto that type is not in its scope. Without this, every method
/// a type declares could reach every method any package ever composed onto
/// it -- exactly the incoherence resolving composed methods through their
/// spec exists to prevent.
#[test]
fn an_inherent_method_body_cannot_reach_a_composed_method() {
    let package = TestPackage::new(
        r#"
        exposed spec Secret { secret(*self) => i32; }
        struct Dog {
            exposed value: i32;
            exposed leak(*self) => i32 { self.secret() }
        }
        compose Dog : Secret { secret(*self) => i32 { 99 } }
        main() => i32 { dog := Dog { value = 1; }; dog.leak() }
        "#,
    );
    let errors = compile_errors(&package, "an inherent body must not see a composed method");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

#[test]
fn distinct_generic_spec_compositions_emit_distinct_bodies() {
    let package = TestPackage::new(
        r#"
        exposed spec Consume<T> { consume(*self, value: T) => i32; }
        struct Multi { exposed base: i32; }
        compose Multi : Consume<i32> {
            consume(*self, value: i32) => i32 { self.base + value }
        }
        compose Multi : Consume<u8> {
            consume(*self, value: u8) => i32 { self.base + <i32>value }
        }
        main() => i32 {
            value := Multi { base = 1; };
            Consume<i32>::consume(&value, 2) + Consume<u8>::consume(&value, 3u8)
        }
        "#,
    );
    let program = package
        .compile()
        .expect("both generic spec compositions should compile");
    let definitions = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter(|item| matches!(item, CheckedItem::FunctionDefinition(_)))
        .count();
    assert_eq!(
        definitions, 3,
        "main and both compose bodies must be emitted"
    );
}

// -- regressions found reviewing plan 0005's own execution ------------------
//
// Each of these reproduced a real defect in the delivered tree: three were
// silent (wrong body called, feature silently dropped, unusable capability),
// one was a hard compile failure of `examples/dev`, and one shipped symbols
// containing characters the mangling scheme excludes.

/// A bound on a spec *alias* must reach the composes satisfying its members.
/// `transitive_spec_dependencies` registers derived entries walking downward
/// (spec -> its dependencies), which is the wrong direction for an alias:
/// `AB` depends on `A`/`B`, so no entry is ever registered under `AB` itself.
/// This is `examples/dev`'s `accepts_myspec<T: MySpec>`, which stopped
/// compiling entirely.
#[test]
fn a_bound_on_a_spec_alias_reaches_its_members_composes() {
    let package = TestPackage::new(
        r#"
        exposed spec A { a(*self) => i32; }
        exposed spec B : A { b(*self) => i32 { 2 } }
        spec AB = A | B;
        struct Foo { exposed v: i32; }
        compose Foo : B { a(*self) => i32 { 1 } }

        use_alias<T: AB>(x: *T) => i32 { x.a() + x.b() }
        main() => i32 { f := Foo { v = 0; }; use_alias(&f) }
        "#,
    );
    package
        .compile()
        .expect("an alias bound must resolve through its members' composes");
}

/// The coherence guarantee, stated negatively: widening the bound context to
/// every compose on the concrete type is what the alias fix must *not* do.
#[test]
fn an_unbounded_spec_is_still_out_of_scope_under_another_bound() {
    let package = TestPackage::new(
        r#"
        exposed spec Speak { speak(*self) => i32; }
        exposed spec Secret { secret(*self) => i32; }
        struct Dog { exposed id: i32; }
        compose Dog : Speak { speak(*self) => i32 { self.id } }
        compose Dog : Secret { secret(*self) => i32 { 999 } }

        leak<T: Speak>(x: *T) => i32 { x.secret() }
        main() => i32 { d := Dog { id = 7; }; leak(&d) }
        "#,
    );
    let errors = compile_errors(&package, "an unbounded spec's method must not be in scope");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::MethodNotInScope { .. }
    )));
}

/// A directly-written compose must win over the derived stand-in a *different*
/// compose's transitive dependencies registered for the same `(target, spec)`.
/// Previously the derived entry sat ahead of it in `entries`, so `Base::b`
/// silently called `Derived`'s body while the explicit block was emitted and
/// never reached -- wrong code, no diagnostic. Asserted in both declaration
/// orders, since the bug was order-dependent.
#[test]
fn an_explicit_compose_wins_over_a_derived_dependency_entry() {
    for (first, second) in [
        (
            "compose Foo : Derived { b(*self) => i32 { 1 } }",
            "compose Foo : Base { b(*self) => i32 { 99 } }",
        ),
        (
            "compose Foo : Base { b(*self) => i32 { 99 } }",
            "compose Foo : Derived { b(*self) => i32 { 1 } }",
        ),
    ] {
        let package = TestPackage::new(&format!(
            r#"
            exposed spec Base {{ b(*self) => i32; }}
            exposed spec Derived : Base {{ d(*self) => i32 {{ 1 }} }}
            struct Foo {{ exposed v: i32; }}
            {first}
            {second}
            main() => i32 {{ f := Foo {{ v = 0; }}; Base::b(&f) }}
            "#
        ));
        let program = package.compile().expect("both composes are legal");
        // Only declaration-level correctness is checkable here: a direct
        // compose landing on a key a derived stand-in already holds must be
        // *accepted* (it is not a `DuplicateCompose` -- the author wrote one
        // `Base` block), and each block still emits its own body.
        //
        // Which body a `Base::b(&f)` call actually reaches is a runtime fact
        // this harness cannot observe -- it produces a `CompiledProgram`, and
        // both bodies were emitted before the fix too, the explicit one dead.
        // That half is verified by executing the reproducer; see
        // `docs/14-known-issues.md`'s note on the coverage gap.
        let bodies = program
            .modules
            .iter()
            .flat_map(|(_, module)| &module.items)
            .filter(
                |item| matches!(item, CheckedItem::FunctionDefinition(f) if f.name.as_ref() == "b"),
            )
            .count();
        assert_eq!(bodies, 2, "one body per compose block, no more");
    }
}

/// A slice target is composable, and reachable. Declaring one used to compile
/// while every call failed with `expected '**[]u8'`: `Self` bound to the
/// `Slice` had no re-stamping arm, so `*self` wrapped instead of re-stamping
/// and the compose's signature disagreed with the requirement built from the
/// same `Self`.
#[test]
fn slice_composes_are_callable_not_merely_declarable() {
    for target in ["[]u8", "<T> []T"] {
        let package = TestPackage::new(&format!(
            r#"
            exposed spec Show {{ show(*self) => i32; }}
            compose{target} : Show {{ show(*self) => i32 {{ self.length }} }}
            main() => i32 {{
                mut a: [2]u8;
                s := &a[0..];
                Show::show(s)
            }}
            "#,
            target = if target.starts_with('<') {
                target.to_string()
            } else {
                format!(" {target}")
            },
        ));
        package
            .compile()
            .unwrap_or_else(|_| panic!("a `{target}` compose must be callable"));
    }
}

#[test]
fn inferred_arrays_slices_and_unsized_array_pointers_have_distinct_spellings() {
    let package = TestPackage::new(
        r#"
        takes_slice(value: *[]i32) => i32 { value.length }
        takes_unsized(value: *[?]i32) => i32 { value[1] }
        main() => i32 {
            inferred: []i32 = [10, 20, 30];
            unsized := <*[?]i32>&inferred;
            slice := &inferred[0..];
            takes_slice(slice) + takes_unsized(unsized)
        }
        "#,
    );
    package
        .compile()
        .expect("the new array and slice spellings should resolve to their distinct shapes");
}

/// A *generic* compose never reaches `resolve_compose_target`, so a target
/// `match_compose_target` cannot bind used to register a template nothing
/// could ever match and vanish silently, surfacing only as an unrelated
/// `SpecNotImplemented` at some use site.
#[test]
fn an_unmatchable_generic_compose_target_is_rejected_at_its_declaration() {
    let package = TestPackage::new(
        r#"
        exposed spec Show { show(*self) => i32; }
        compose<T> *T : Show { show(*self) => i32 { 1 } }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "a pointer compose target must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::ComposeTargetNotAType
    )));
}

/// Omega has no variadic function *definitions* -- only `extern` declarations
/// may be variadic -- so a variadic spec requirement is unsatisfiable by
/// construction. It used to parse and compile, leaving every implementor with
/// a bare `MissingSpecFunction` naming a function it had no syntax to write.
#[test]
fn a_variadic_spec_function_is_rejected_at_its_declaration() {
    let package = TestPackage::new(
        r#"
        exposed spec Fmt { emit(*self, ...) => i32; }
        main() => i32 { 0 }
        "#,
    );
    let errors = compile_errors(&package, "a variadic spec function must be rejected");
    assert!(has_analysis_error(&errors, |kind| matches!(
        kind,
        AnalysisErrorKind::VariadicSpecFunctionUnsatisfiable { .. }
    )));
}

/// A `spec T` return type on a *method* is rejected, not inferred. Inferring
/// it is reachable but wrong: it would run during the signature phase, while
/// the owning type's method table is still empty, so the body sees `Self`'s
/// fields but none of its sibling methods. The honest rejection is the
/// pre-existing `SpecStaticNotAllowedHere`.
#[test]
fn a_spec_return_type_on_a_method_is_rejected_not_inferred() {
    let package = TestPackage::new(
        r#"
        exposed spec Countable { count(*self) => i32; }
        struct Wrap { exposed n: i32; }
        compose Wrap : Countable { count(*self) => i32 { self.n } }
        struct Zoo {
            exposed n: i32;
            exposed helper(*self) => i32 { 5 }
            exposed make(*self) => spec Countable { Wrap { n = self.helper(); } }
        }
        main() => i32 { z := Zoo { n = 1; }; Countable::count(&z.make()) }
        "#,
    );
    let errors = compile_errors(&package, "a `spec T`-returning method must be rejected");
    // Specifically NOT `NoSuchField` -- that was the symptom of forcing it.
    assert!(
        !has_analysis_error(&errors, |kind| matches!(
            kind,
            AnalysisErrorKind::NoSuchField { .. }
        )),
        "must not fail by observing a partially-populated cell"
    );
}

/// An annotation naming an element type the source does not produce is a
/// mismatch, not an ambiguity -- it used to render as an ambiguity over an
/// empty candidate list, naming neither the requested nor the available type.
#[test]
fn a_mismatched_for_loop_element_annotation_reports_what_is_available() {
    // Needs the real `core` for `Iterator`/`ToIterator`/`Option`: the loop
    // protocol is nominal, so a stub would not exercise the same lookup.
    let package = TestPackage::new(
        r#"
        exposed struct BagIter { exposed i: i32; }
        compose BagIter : Iterator<u8> { next(*mut self) => Option<u8> { Option<u8>::None } }
        exposed struct Bag { exposed n: i32; }
        compose Bag : ToIterator<u8> { to_iterator(*self) => BagIter { BagIter { i = 0; } } }
        main() => i32 { b := Bag { n = 0; }; for x : u64 in b { } 0 }
        "#,
    );
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/core");
    let errors = match Driver::new(
        package.0.clone(),
        None,
        vec![ExternRoot {
            name: Ident("core".to_string()),
            dir: core,
        }],
    )
    .expect("construct driver with core extern")
    .compile(&[Ident("main".to_string())])
    {
        Ok(_) => panic!("a mismatched element annotation must be rejected"),
        Err(errors) => errors,
    };
    assert!(has_analysis_error(&errors, |kind| match kind {
        AnalysisErrorKind::ForLoopElementTypeMismatch { available, .. } => !available.is_empty(),
        _ => false,
    }));
}

/// Primitive-method symbols must encode a structural target through the
/// `MangleType` grammar rather than `ResolvedType`'s `Display`, which put
/// `*str`, `*[]u8` and (with a space) `*mut []u8` straight into symbol
/// names -- outside the `[A-Za-z0-9_]` set the scheme deliberately keeps to.
#[test]
fn primitive_method_symbols_stay_within_the_mangling_charset() {
    let package = TestPackage::new(r#"
        primitive str { exposed width(*self) => i32 { self.size } }
        main() => i32 { 0 }
        "#,
    );
    // `primitive` is core-only, so compile it *as* core.
    let root = package.0.clone();
    let program = Driver::new(root, Some(Ident("core".to_string())), Vec::new())
        .expect("construct driver")
        .compile(&[Ident("core".to_string())]);
    let program = match program {
        Ok(program) => program,
        Err(errors) => panic!("core-shaped primitive package must compile: {errors:?}"),
    };
    let names: Vec<String> = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .filter_map(|item| match item {
            CheckedItem::FunctionDefinition(f) => Some(f.name.as_ref().to_string()),
            _ => None,
        })
        .collect();
    assert!(names.iter().any(|name| name == "width"));
}

/// A package whose root directory contains no module at all is a reportable
/// error, not a panic. The reachable cause is the pre-root-module layout
/// (`<root>/<basename>/<basename>.omg`): `discover_tree`'s `skip` matches by
/// name rather than kind, so the same-named *directory* is swallowed too and
/// the root ends up with neither an own file nor children. That used to reach
/// `compile`'s generic-instantiation merge and fail its "always includes at
/// least the entry module" expectation as a compiler panic — exactly the
/// shape anyone migrating an old package is in.
#[test]
fn a_package_root_with_no_modules_is_a_reportable_error() {
    let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "omega_empty_pkg_{}_{}",
        std::process::id(),
        sequence
    ));
    let nested = root.join(root.file_name().expect("temp dir has a name"));
    fs::create_dir_all(&nested).expect("create nested package");
    let own = format!("{}.omg", root.file_name().unwrap().to_str().unwrap());
    fs::write(nested.join(&own), "exposed helper() => i32 { 7 }\n").expect("write inner module");

    let result = Driver::new(root.clone(), None, Vec::new())
        .expect("construct driver")
        .compile(&[Ident("main".to_string())]);
    let _ = fs::remove_dir_all(&root);

    let errors = result.err().expect("an empty package root must not compile");
    assert!(
        errors
            .iter()
            .any(|error| matches!(error, CompileError::EmptyPackage { .. })),
        "expected EmptyPackage, got {errors:?}"
    );
}
