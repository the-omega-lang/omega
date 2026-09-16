use omega_analyzer::error::AnalysisErrorKind;
use omega_analyzer::resolver::ResolveError;
use omega_analyzer::source_annotations::SourceAnnotationErrorKind;
use omega_analyzer::{Target, compiler_definitions::CompilerDefinitions};
use omega_driver::{CompileError, CompiledProgram, Driver, ExternRoot};
use omega_parser::prelude::Ident;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Package(PathBuf);
impl Package {
    fn new(source: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!(
                "omega_source_annotations_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .join("main");
        fs::create_dir_all(&root).unwrap();
        let package = Self(root);
        package.write("main.omg", source);
        package
    }
    fn write(&self, file: &str, source: &str) {
        let path = self.0.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    fn driver(&self, externs: Vec<ExternRoot>) -> Driver {
        Driver::new_with_definitions(
            self.0.clone(),
            None,
            externs,
            CompilerDefinitions::new(Target::DEFAULT),
        )
        .unwrap()
    }
    fn compile(&self) -> Result<CompiledProgram, Vec<CompileError>> {
        self.driver(vec![]).compile(&[Ident("main".into())])
    }
    fn errors(&self) -> Vec<CompileError> {
        self.compile().err().expect("must fail")
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}
fn tree(package: &Package, condition: &str, child: &str) {
    package.write(
        "thing/thing.omg",
        &format!("@[cond({condition})]\nexposed value: i32 = 1;"),
    );
    package.write("thing/child.omg", child);
}
fn unknown_module(errors: &[CompileError], expected: &[&str]) {
    assert!(errors.iter().any(|error| matches!(error, CompileError::Analysis { errors, .. } if errors.iter().any(|error| matches!(&error.kind, AnalysisErrorKind::ModuleResolution(ResolveError::UnknownModule(path)) if path.iter().map(Ident::as_ref).collect::<Vec<_>>() == expected)))), "{errors:?}");
}

#[test]
fn false_removes_subtree_and_emission_slots_without_reading_children() {
    let package = Package::new("exposed keep: i32 = 1;");
    tree(&package, "false", "this file is deliberately invalid @@@");
    let program = package.compile().unwrap();
    assert_eq!(program.sources.len(), 1);
    assert_eq!(program.sources[0].relative_path, PathBuf::from("main.omg"));
    for target in ["thing", "thing::child"] {
        package.write("main.omg", &format!("import self::{target}::value;"));
        let mut expected = vec!["main"];
        expected.extend(target.split("::"));
        unknown_module(&package.errors(), &expected);
    }
}

#[test]
fn true_keeps_parent_and_children() {
    let package =
        Package::new("import self::thing; import self::thing::child; exposed keep: i32 = 1;");
    tree(&package, "true", "exposed child_value: i32 = 2;");
    assert_eq!(package.compile().unwrap().sources.len(), 3);
}

#[test]
fn malformed_and_unevaluable_conditions_retain_modules_and_report_the_cause() {
    for condition in ["", "true, false", "flag = true"] {
        let package = Package::new(&format!("@[cond({condition})]\nexposed keep: i32 = 1;"));
        let errors = package.errors();
        assert!(
            matches!(&errors[0], CompileError::SourceAnnotation { error, .. } if matches!(error.kind, SourceAnnotationErrorKind::InvalidArguments { .. })),
            "{errors:?}"
        );
        assert_eq!(errors[0].module().unwrap()[0].as_ref(), "main");
    }
    let package = Package::new("@[cond(unknown)] exposed keep: i32 = 1;");
    assert!(
        package
            .errors()
            .iter()
            .any(|error| matches!(error, CompileError::Condition { .. }))
    );
    package.write(
        "main.omg",
        "@[cond(false)] @[cond(true)] exposed keep: i32 = 1;",
    );
    assert!(package.errors().iter().any(|error| matches!(error, CompileError::SourceAnnotation { error, .. } if matches!(error.kind, SourceAnnotationErrorKind::Duplicate { .. }))));
}

#[test]
fn false_skips_remaining_validation_but_not_parsing() {
    let package = Package::new("exposed keep: i32 = 1;");
    package.write(
        "off.omg",
        "@[bogus] @[suppress(a = b)] @[cond(false)] @inline(unknown) exposed x: i32 = 1;",
    );
    assert_eq!(package.compile().unwrap().sources.len(), 1);
    package.write("off.omg", "@[cond(false)] this is malformed @@@");
    assert!(
        package
            .errors()
            .iter()
            .any(|error| matches!(error, CompileError::Parse { .. }))
    );
}

#[test]
fn fully_disabled_package_has_its_own_diagnostic() {
    let package = Package::new("@[cond(false)]");
    package.write("child.omg", "invalid @@@");
    let errors = package.errors();
    assert!(matches!(
        &errors[0],
        CompileError::FullyDisabledPackage { .. }
    ));
    assert!(errors[0].module().is_none());
    assert!(
        errors[0].to_diagnostics()[0]
            .message
            .contains("was conditioned out")
    );
    fs::remove_file(package.0.join("main.omg")).unwrap();
    package.write("child.omg", "@[cond(false)]");
    assert!(matches!(
        &package.errors()[0],
        CompileError::FullyDisabledPackage { .. }
    ));
}

#[test]
fn extern_trees_use_the_same_selection_and_core_pruning_does_not_panic() {
    let package = Package::new("import core::thing::child::value;");
    let external = Package::new("@[cond(false)]");
    tree(&external, "true", "invalid @@@");
    let mut driver = package.driver(vec![ExternRoot {
        name: Ident("core".into()),
        dir: external.0.clone(),
    }]);
    let errors = driver.compile(&[Ident("main".into())]).err().unwrap();
    unknown_module(&errors, &["core", "thing", "child"]);
}

#[test]
fn source_suppression_covers_analyzer_and_driver_warnings_but_not_other_files() {
    let package = Package::new(
        "@[suppress(unused_variable, unused_import)]\nimport self::helper;\nexposed f() => void { unused := 1; }",
    );
    package.write("helper.omg", "exposed g() => void { other := 2; }");
    let program = package.compile().unwrap();
    assert!(
        program
            .warnings
            .iter()
            .all(|(module, warning)| module.len() != 1
                || !["unused_variable", "unused_import"].contains(&warning.kind.name())),
        "{:?}",
        program.warnings
    );
    assert!(
        program
            .warnings
            .iter()
            .any(|(module, warning)| module.len() == 2 && warning.kind.name() == "unused_variable")
    );
}

#[test]
fn unfilled_gap_cannot_be_suppressed_at_source_level() {
    let package = Package::new("@[suppress(unfilled_gap)]\ngap Missing { f() => void; }");
    assert!(
        package
            .compile()
            .unwrap()
            .warnings
            .iter()
            .any(|(_, warning)| warning.kind.name() == "unfilled_gap")
    );
}
