use omega_analyzer::Target;
use omega_analyzer::annotation_eval::ConditionErrorKind;
use omega_analyzer::checked::CheckedItem;
use omega_analyzer::compiler_definitions::{CompilerDefinitions, DefinitionValue, decode_literal};
use omega_driver::{CompileError, CompiledProgram, Driver, ExternRoot};
use omega_parser::prelude::{Ident, parse_literal};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// One `name[=literal]` per option, the same shape a `-D` option carries
/// after `omgc` has split it.
fn definitions(options: &[&str]) -> CompilerDefinitions {
    let target = Target::DEFAULT;
    let mut definitions = CompilerDefinitions::new(target);
    for option in options {
        let (name, value) = match option.split_once('=') {
            Some((name, text)) => {
                let literal = parse_literal(text).expect("the test literal parses");
                let value = decode_literal(&literal, target.pointer_bits())
                    .expect("the test literal decodes");
                (name, value)
            }
            None => (*option, DefinitionValue::Bool(true)),
        };
        assert!(definitions.define(Ident(name.into()), value));
    }
    definitions
}

struct TestPackage(PathBuf);

impl TestPackage {
    fn new(source: &str) -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "omega_conditional_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn child(&self, name: &str, source: &str) -> &Self {
        fs::write(self.0.join(format!("{name}.omg")), source).expect("write child module");
        self
    }

    fn driver(&self, options: &[&str]) -> Driver {
        self.driver_with_externs(options, vec![])
    }

    fn driver_with_externs(&self, options: &[&str], externs: Vec<ExternRoot>) -> Driver {
        Driver::new_with_definitions(self.0.clone(), None, externs, definitions(options))
            .expect("construct driver")
    }

    fn compile(&self, options: &[&str]) -> CompiledProgram {
        self.driver(options)
            .compile(&[Ident("main".into())])
            .unwrap_or_else(|errors| {
                panic!(
                    "package should compile with {options:?}: {}",
                    render(&errors)
                )
            })
    }

    fn compile_errors(&self, options: &[&str]) -> Vec<CompileError> {
        match self.driver(options).compile(&[Ident("main".into())]) {
            Ok(_) => panic!("the package should not have compiled with {options:?}"),
            Err(errors) => errors,
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn render(errors: &[CompileError]) -> String {
    errors
        .iter()
        .flat_map(CompileError::to_diagnostics)
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Every name the compilation kept, so a test can state exactly which
/// declarations survived rather than only that it compiled.
fn names(program: &CompiledProgram) -> Vec<String> {
    let mut names: Vec<String> = program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .map(|item| match item {
            CheckedItem::Declaration(declaration) => declaration.ident.to_string(),
            CheckedItem::ForeignBinding(binding) => binding.ident.to_string(),
            CheckedItem::ForeignFunction(f) => f.name.to_string(),
            CheckedItem::FunctionDefinition(f) => f.name.to_string(),
            CheckedItem::Struct(s) => s.name.to_string(),
            CheckedItem::Enum(e) => e.name.to_string(),
            CheckedItem::Union(u) => u.name.to_string(),
        })
        .collect();
    names.sort();
    names
}

fn condition_error(errors: &[CompileError]) -> &omega_analyzer::annotation_eval::ConditionError {
    errors
        .iter()
        .find_map(|error| match error {
            CompileError::Condition { error, .. } => Some(error),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a condition error, got {}", render(errors)))
}

#[test]
fn a_definition_selects_between_mutually_exclusive_declarations() {
    let package = TestPackage::new(
        r#"
        @cond(def::fast)
        exposed limit : i32 = 1;

        @cond(not(def::fast))
        exposed limit : i32 = 2;

        entry_fn() => i32 { limit }
        "#,
    );
    assert!(names(&package.compile(&["fast"])).contains(&"limit".to_string()));
    assert!(names(&package.compile(&[])).contains(&"limit".to_string()));
}

#[test]
fn a_disabled_item_claims_no_name_and_is_never_checked() {
    let package = TestPackage::new(
        r#"
        @cond(false)
        exposed broken : NoSuchType = missing_call();

        @cond(false)
        exposed broken() => void { also_missing(); }

        @cond(false)
        @inline(nonsense)
        @suppress(not_a_warning)
        exposed also_broken : i32 = 1;

        @cond(false)
        struct Broken { exposed field: NoSuchType; }

        @cond(false)
        meet NoSuchSpec for NoSuchType { }

        @cond(false)
        glue NoSuchGap { f() => i32 { 1 } }

        @cond(false)
        primitive NoSuchPrimitive { }

        @cond(false)
        import no::such::module;

        @cond(false)
        alias Broken = NoSuchType;

        exposed kept : i32 = 7;
        "#,
    );
    assert_eq!(names(&package.compile(&[])), ["kept"]);
}

#[test]
fn a_disabled_item_can_be_enabled_by_the_same_source() {
    let package = TestPackage::new(
        r#"
        @cond(def::extra)
        exposed extra() => i32 { 9 }

        @cond(def::extra)
        exposed Holder : i32 = 1;

        exposed kept : i32 = 7;
        "#,
    );
    assert_eq!(names(&package.compile(&[])), ["kept"]);
    assert_eq!(
        names(&package.compile(&["extra"])),
        ["Holder", "extra", "kept"]
    );
}

#[test]
fn a_condition_selects_declarations_across_modules() {
    let package = TestPackage::new(
        r#"
        import self::helper::pick;
        entry_fn() => i32 { pick() }
        "#,
    );
    package.child(
        "helper",
        r#"
        @cond(def::fast)
        exposed pick() => i32 { 1 }

        @cond(not(def::fast))
        exposed pick() => i32 { 2 }
        "#,
    );
    package.compile(&["fast"]);
    package.compile(&[]);
}

#[test]
fn an_import_of_a_disabled_declaration_fails_normally() {
    let package = TestPackage::new(
        r#"
        import self::helper::pick;
        entry_fn() => i32 { pick() }
        "#,
    );
    package.child(
        "helper",
        r#"
        @cond(def::fast)
        exposed pick() => i32 { 1 }
        "#,
    );
    package.compile(&["fast"]);
    let errors = package.compile_errors(&[]);
    assert!(
        render(&errors).contains("pick"),
        "a reference to a removed declaration must fail like any unknown name: {}",
        render(&errors)
    );
}

#[test]
fn a_disabled_macro_definition_binds_no_macro() {
    let package = TestPackage::new(
        r#"
        @cond(def::with_macro)
        macro pick() => { exposed generated : i32 = 3; }

        pick$();
        "#,
    );
    assert!(names(&package.compile(&["with_macro"])).contains(&"generated".to_string()));
    assert!(
        render(&package.compile_errors(&[])).contains("no macro named 'pick'"),
        "a removed macro definition must not stay bound"
    );
}

#[test]
fn a_disabled_macro_invocation_is_never_expanded() {
    let package = TestPackage::new(
        r#"
        @cond(false)
        no_such_macro$();

        exposed kept : i32 = 1;
        "#,
    );
    assert_eq!(names(&package.compile(&[])), ["kept"]);
}

#[test]
fn a_generated_item_is_filtered_before_its_body_is_expanded() {
    let package = TestPackage::new(
        r#"
        macro pair($name: ident) => {
            @cond(def::on)
            exposed $name : i32 = 1;

            @cond(def::expand_more)
            no_such_macro$();
        }

        pair$(value);
        "#,
    );
    assert!(
        names(&package.compile(&["on"])).contains(&"value".to_string()),
        "the generated declaration survives while the generated false invocation \
         beside it is never looked up"
    );
    assert!(!names(&package.compile(&[])).contains(&"value".to_string()));
}

#[test]
fn a_condition_written_by_a_macro_reports_its_origin() {
    let package = TestPackage::new(
        r#"
        import self::helper::gate;
        gate$(value);
        "#,
    );
    package.child(
        "helper",
        r#"
        exposed macro gate($name: ident) => {
            @cond(equals(def::absent, 1))
            exposed $name : i32 = 1;
        }
        "#,
    );
    let errors = package.compile_errors(&[]);
    let CompileError::Condition {
        error, definition, ..
    } = errors
        .iter()
        .find(|error| matches!(error, CompileError::Condition { .. }))
        .unwrap_or_else(|| panic!("expected a condition error, got {}", render(&errors)))
    else {
        unreachable!("just matched")
    };
    assert!(matches!(
        error.kind,
        ConditionErrorKind::MissingDefinition(_)
    ));
    assert!(
        definition.is_some(),
        "a macro-authored condition must point at the macro that wrote it"
    );
}

#[test]
fn malformed_generated_conditions_report_the_annotation_author() {
    for (annotation, duplicate) in [
        ("@cond", false),
        ("@cond()", false),
        ("@cond($flag, true)", false),
        ("@cond(when = $flag)", false),
        ("@cond(false) @cond()", true),
        ("@cond(false) @cond($flag)", true),
    ] {
        let package = TestPackage::new("import self::helper::gate;\ngate$(true);\n");
        package.child(
            "helper",
            &format!("exposed macro gate($flag: expr) => {{ {annotation} exposed x : i32 = 1; }}"),
        );
        let mut driver = package.driver(&[]);
        let errors = driver
            .compile(&[Ident("main".into())])
            .err()
            .expect("invalid condition");
        let failure = errors
            .iter()
            .find(|error| matches!(error, CompileError::Condition { .. }))
            .unwrap_or_else(|| panic!("{annotation}: {}", render(&errors)));
        let CompileError::Condition {
            error, definition, ..
        } = failure
        else {
            unreachable!()
        };
        if duplicate {
            assert!(matches!(
                error.kind,
                ConditionErrorKind::DuplicateCondition { .. }
            ));
        } else {
            assert!(matches!(error.kind, ConditionErrorKind::MalformedCondition));
        }
        let definition = definition.expect("the annotation must retain its macro author");
        assert_eq!(
            definition.source,
            driver
                .source_id(&[Ident("main".into()), Ident("helper".into())])
                .unwrap()
        );
        assert!(
            failure.to_diagnostics()[0]
                .labels
                .iter()
                .any(|label| label.message == "macro defined here")
        );
    }
}

#[test]
fn an_ambient_core_macro_is_selected_under_the_same_configuration() {
    let package = TestPackage::new("pick$();\n");
    let core = package.0.parent().unwrap().join("core");
    fs::create_dir(&core).expect("create core package");
    fs::write(
        core.join("core.omg"),
        r#"
        @cond(def::on)
        exposed macro pick() => { exposed enabled : i32 = 1; }

        @cond(not(def::on))
        exposed macro pick() => { exposed fallback : i32 = 2; }
    "#,
    )
    .expect("write core macros");
    for (options, selected) in [(&["on"][..], "enabled"), (&[][..], "fallback")] {
        let program = package
            .driver_with_externs(
                options,
                vec![ExternRoot {
                    name: Ident("core".into()),
                    dir: core.clone(),
                }],
            )
            .compile(&[Ident("main".into())])
            .unwrap_or_else(|errors| panic!("{}", render(&errors)));
        assert_eq!(names(&program), [selected]);
    }
}

#[test]
fn a_condition_error_is_reported_rather_than_read_as_false() {
    let package = TestPackage::new(
        r#"
        @cond(equals(def::missing, 1))
        exposed a : i32 = 1;
        "#,
    );
    let errors = package.compile_errors(&[]);
    assert!(matches!(
        condition_error(&errors).kind,
        ConditionErrorKind::MissingDefinition(_)
    ));
}

#[test]
fn a_repeated_condition_is_an_error_even_when_the_first_is_false() {
    let package = TestPackage::new(
        r#"
        @cond(false)
        @cond(false)
        exposed a : i32 = 1;
        "#,
    );
    let errors = package.compile_errors(&[]);
    assert!(matches!(
        condition_error(&errors).kind,
        ConditionErrorKind::DuplicateCondition { .. }
    ));
}

#[test]
fn an_invalid_condition_is_reported_from_a_module_that_nothing_imports() {
    let package = TestPackage::new(
        r#"
        exposed kept : i32 = 1;
        "#,
    );
    package.child(
        "unused",
        r#"
        @cond(nonsense_call())
        exposed a : i32 = 1;
        "#,
    );
    let errors = package.compile_errors(&[]);
    assert!(matches!(
        condition_error(&errors).kind,
        ConditionErrorKind::UnknownCall(_)
    ));
}

#[test]
fn a_file_left_empty_by_its_conditions_still_owns_an_artifact() {
    let package = TestPackage::new(
        r#"
        exposed kept : i32 = 1;
        "#,
    );
    package.child(
        "empty",
        r#"
        @cond(false)
        exposed gone : i32 = 1;
        "#,
    );
    let program = package.compile(&[]);
    assert_eq!(names(&program), ["kept"]);
    assert!(
        program
            .sources
            .iter()
            .any(|source| source.relative_path.to_string_lossy().contains("empty")),
        "a file whose items were all removed still parses and owns an output slot"
    );
}

#[test]
fn a_syntax_error_in_disabled_source_is_still_a_syntax_error() {
    let package = TestPackage::new(
        r#"
        @cond(false)
        exposed a : i32 = ;
        "#,
    );
    assert!(
        package
            .compile_errors(&[])
            .iter()
            .any(|error| matches!(error, CompileError::Parse { .. })),
        "a condition decides which declarations exist, not which source is read"
    );
}

#[test]
fn two_enabled_declarations_of_one_name_still_collide() {
    let package = TestPackage::new(
        r#"
        @cond(def::both)
        exposed value : i32 = 1;

        @cond(def::both)
        exposed value : i32 = 2;
        "#,
    );
    package.compile(&[]);
    assert!(!package.compile_errors(&["both"]).is_empty());
}
