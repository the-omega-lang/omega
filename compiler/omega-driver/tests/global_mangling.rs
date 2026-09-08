//! Symbol policy on ordinary module-level storage: the analyzer resolves it
//! once at signature time and the driver hands the same checked declaration to
//! the body pass, so nothing downstream has to re-derive it.

use omega_analyzer::Target;
use omega_analyzer::annotations::{ItemKind, ManglingMode, SymbolPolicy, SymbolVisibility};
use omega_analyzer::checked::{CheckedDeclaration, CheckedItem, NumberValue};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolved_type::ConstValue;
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
            "omega_global_mangling_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn result(&self) -> Result<CompiledProgram, Vec<CompileError>> {
        Driver::new(self.0.clone(), None, vec![], Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".into())], Target::DEFAULT)
    }

    fn expect_ok(&self) -> CompiledProgram {
        match self.result() {
            Ok(program) => program,
            Err(errors) => panic!("expected this package to compile, got: {errors:#?}"),
        }
    }

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.result() {
            Ok(_) => panic!("expected this package to be rejected, but it compiled"),
            Err(errors) => errors,
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn analysis_errors(errors: &[CompileError]) -> Vec<&AnalysisError> {
    errors
        .iter()
        .flat_map(|error| match error {
            CompileError::Analysis { errors, .. } => errors.iter().collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn global(program: &CompiledProgram, name: &str) -> CheckedDeclaration {
    program
        .modules
        .iter()
        .flat_map(|(_, module)| module.items.iter())
        .find_map(|item| match item {
            CheckedItem::Declaration(declaration) if declaration.ident.as_ref() == name => {
                Some(declaration.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a checked global named '{name}'"))
}

fn foreign_symbol(program: &CompiledProgram, name: &str) -> SymbolPolicy {
    program
        .modules
        .iter()
        .flat_map(|(_, module)| module.items.iter())
        .find_map(|item| match item {
            CheckedItem::ForeignBinding(binding) if binding.ident.as_ref() == name => {
                Some(binding.symbol.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a checked foreign binding named '{name}'"))
}

fn unsigned(value: u64) -> Option<ConstValue> {
    Some(ConstValue::Number(NumberValue::Unsigned(value)))
}

#[test]
fn every_global_form_keeps_its_resolved_policy_and_initializer() {
    let package = TestPackage::new(
        r#"
        @symbol(name = "forced_typed")
        typed : u32 = 1;

        @symbol(mangle = disabled)
        mut bare : u32;

        @symbol(name = "forced_inferred")
        mut inferred := 3u32;

        @symbol(mangle = enabled)
        explicit : u32 = 4;

        implicit : u32 = 5;

        @symbol(export)
        exported : u32 = 6;

        @symbol(export = enabled, name = "exported_exact")
        mut exported_exact : u32 = 7;

        main() => void {}
        "#,
    );
    let program = package.expect_ok();

    let typed = global(&program, "typed");
    assert_eq!(
        typed.symbol,
        SymbolPolicy::mangled(ManglingMode::Forced("forced_typed".to_string()))
    );
    assert_eq!(typed.initial_value, unsigned(1));

    let bare = global(&program, "bare");
    assert_eq!(bare.symbol, SymbolPolicy::mangled(ManglingMode::Disabled));
    assert_eq!(
        bare.initial_value, None,
        "a declaration without an initializer still has none"
    );
    assert!(bare.mutable);

    let inferred = global(&program, "inferred");
    assert_eq!(
        inferred.symbol,
        SymbolPolicy::mangled(ManglingMode::Forced("forced_inferred".to_string()))
    );
    assert_eq!(inferred.initial_value, unsigned(3));

    assert_eq!(
        global(&program, "explicit").symbol,
        SymbolPolicy::ordinary()
    );
    assert_eq!(
        global(&program, "implicit").symbol,
        SymbolPolicy::ordinary()
    );
    assert_eq!(global(&program, "implicit").initial_value, unsigned(5));

    // `export` on its own leaves the naming default alone, and combines with
    // an exact name without changing it.
    assert_eq!(
        global(&program, "exported").symbol,
        SymbolPolicy {
            mangling: ManglingMode::Enabled,
            visibility: SymbolVisibility::Default,
        }
    );
    let exported_exact = global(&program, "exported_exact");
    assert_eq!(
        exported_exact.symbol,
        SymbolPolicy {
            mangling: ManglingMode::Forced("exported_exact".to_string()),
            visibility: SymbolVisibility::Default,
        }
    );
    assert_eq!(exported_exact.initial_value, unsigned(7));
}

/// A foreign binding names storage another object owns, so both of its defaults
/// are the opposite of an ordinary global's: the written name, and a symbol
/// that may cross an image boundary.
#[test]
fn foreign_data_keeps_its_own_default_and_overrides() {
    let package = TestPackage::new(
        r#"
        foreign defaulted : u32;

        @symbol(name = "forced_external")
        foreign forced : u32;

        @symbol(mangle = enabled)
        foreign mangled : u32;

        @symbol(export = disabled)
        foreign in_image : u32;

        main() => void {}
        "#,
    );
    let program = package.expect_ok();

    assert_eq!(
        foreign_symbol(&program, "defaulted"),
        SymbolPolicy::foreign()
    );
    assert_eq!(
        foreign_symbol(&program, "forced"),
        SymbolPolicy {
            mangling: ManglingMode::Forced("forced_external".to_string()),
            visibility: SymbolVisibility::Default,
        }
    );
    assert_eq!(
        foreign_symbol(&program, "mangled"),
        SymbolPolicy {
            mangling: ManglingMode::Enabled,
            visibility: SymbolVisibility::Default,
        },
        "opting into mangling does not also opt out of crossing an image"
    );
    // The one thing `export` still decides here: a declaration that is in fact
    // resolved inside this image can say so.
    assert_eq!(
        foreign_symbol(&program, "in_image"),
        SymbolPolicy::mangled(ManglingMode::Disabled)
    );
}

/// Annotations are resolved while the signature is, so a global nothing reads
/// is still validated.
#[test]
fn an_unused_globals_annotation_is_still_diagnosed() {
    let package = TestPackage::new(
        r#"
        @symbol(name = "")
        never_read : u32 = 1;

        main() => void {}
        "#,
    );

    let errors = package.expect_errors();
    let analysis = analysis_errors(&errors);
    assert_eq!(analysis.len(), 1, "got: {analysis:#?}");
    assert!(matches!(
        &analysis[0].kind,
        AnalysisErrorKind::InvalidAnnotationArgs { name, .. } if name.as_ref() == "symbol"
    ));
}

type ErrorPredicate = fn(&AnalysisErrorKind) -> bool;

#[test]
fn invalid_global_annotations_report_their_own_kind() {
    let cases: [(&str, ErrorPredicate); 6] = [
        (
            "@symbol(mangle = sideways)\nvalue : u32 = 1;",
            |kind| matches!(kind, AnalysisErrorKind::InvalidAnnotationArgs { name, .. } if name.as_ref() == "symbol"),
        ),
        (
            "@symbol(mangle = disabled)\n@symbol(export)\nvalue : u32 = 1;",
            |kind| matches!(kind, AnalysisErrorKind::DuplicateAnnotation { name } if name.as_ref() == "symbol"),
        ),
        (
            "@symbol(name = \"exact\", mangle = enabled)\nvalue : u32 = 1;",
            |kind| matches!(kind, AnalysisErrorKind::InvalidAnnotationArgs { name, .. } if name.as_ref() == "symbol"),
        ),
        (
            "@mangling(disabled)\nvalue : u32 = 1;",
            |kind| matches!(kind, AnalysisErrorKind::UnknownAnnotation { name } if name.as_ref() == "mangling"),
        ),
        ("@suppress(unused_binding)\nvalue : u32 = 1;", |kind| {
            matches!(
                kind,
                AnalysisErrorKind::AnnotationNotApplicable { name, found, allowed }
                    if name.as_ref() == "suppress"
                        && *found == ItemKind::Global
                        && !allowed.contains(&ItemKind::Global)
            )
        }),
        (
            "@stray\nvalue : u32 = 1;",
            |kind| matches!(kind, AnalysisErrorKind::UnknownAnnotation { name } if name.as_ref() == "stray"),
        ),
    ];

    for (global_source, expected) in cases {
        let package = TestPackage::new(&format!("{global_source}\n\nmain() => void {{}}\n"));
        let errors = package.expect_errors();
        let analysis = analysis_errors(&errors);
        assert!(
            analysis.iter().any(|error| expected(&error.kind)),
            "unexpected diagnostics for {global_source:?}: {analysis:#?}"
        );
    }
}
