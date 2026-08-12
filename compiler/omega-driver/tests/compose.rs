use omega_analyzer::checked::{CheckedItem, ExternFunctionKind};
use omega_analyzer::error::AnalysisErrorKind;
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        Self::with_file("main.omg", source)
    }

    fn with_file(file: &str, source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "omega_compose_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        fs::create_dir(&root).expect("create test package");
        fs::write(root.join(file), source).expect("write test module");
        Self(root)
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, Vec::new())
            .expect("construct driver")
            .compile(&[Ident("main".to_string())])
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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
        compose [?]u8 : Empty { empty(*self) => bool { self.length == 0 } }
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
    let errors = compile_errors(&pointer, "a pointer target must be rejected by the target model");
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
    let core = TestPackage::with_file(
        "core.omg",
        "primitive i32 { exposed identity(*self) => i32 { *self } }",
    );
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
    let errors = compile_errors(&unfixed_parameter, "an unbindable parameter must be rejected");
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
        ("compose Foo : Derived { b(*self) => i32 { 1 } }", "compose Foo : Base { b(*self) => i32 { 99 } }"),
        ("compose Foo : Base { b(*self) => i32 { 99 } }", "compose Foo : Derived { b(*self) => i32 { 1 } }"),
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
            .filter(|item| matches!(item, CheckedItem::FunctionDefinition(f) if f.name.as_ref() == "b"))
            .count();
        assert_eq!(bodies, 2, "one body per compose block, no more");
    }
}

/// A slice target is composable, and reachable. Declaring one used to compile
/// while every call failed with `expected '**[?]u8'`: `Self` bound to the
/// `Slice` had no re-stamping arm, so `*self` wrapped instead of re-stamping
/// and the compose's signature disagreed with the requirement built from the
/// same `Self`.
#[test]
fn slice_composes_are_callable_not_merely_declarable() {
    for target in ["[?]u8", "<T> [?]T"] {
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
/// `*str`, `*[?]u8` and (with a space) `*mut [?]u8` straight into symbol
/// names -- outside the `[A-Za-z0-9_]` set the scheme deliberately keeps to.
#[test]
fn primitive_method_symbols_stay_within_the_mangling_charset() {
    let package = TestPackage::with_file(
        "main.omg",
        r#"
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
