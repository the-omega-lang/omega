use super::*;
use omega_hir::{HirItem, ModuleId, lower_module};
use omega_parser::SourceModule;

fn function(source: &str) -> HirFunctionDef {
    let ast = SourceModule::parse(source).unwrap();
    let module = lower_module(ModuleId(0), &ast);
    module
        .items
        .into_iter()
        .find_map(|item| match item {
            HirItem::FunctionDefinition(f) => Some(f),
            _ => None,
        })
        .expect("a function definition")
}

fn path(segments: &[&str]) -> Vec<Ident> {
    segments.iter().map(|s| Ident(s.to_string())).collect()
}

fn classify(module: &[&str], source: &str) -> Result<Option<CompilerFunction>, AnalysisErrorKind> {
    compiler_function(&path(module), &function(source))
}

const VOLATILE: &[&str] = &["core", "volatile"];

#[test]
fn recognizes_the_exact_declarations() {
    assert!(matches!(
        classify(VOLATILE, "exposed read_volatile<T>(location: *T) => T;"),
        Ok(Some(CompilerFunction::ReadVolatile))
    ));
    assert!(matches!(
        classify(
            VOLATILE,
            "exposed write_volatile<T>(location: *mut T, value: T) => void;"
        ),
        Ok(Some(CompilerFunction::WriteVolatile))
    ));
}

#[test]
fn ordinary_function_with_a_body_is_not_compiler_implemented() {
    assert!(matches!(classify(&["app"], "f() => void {}"), Ok(None)));
    assert!(matches!(
        classify(VOLATILE, "exposed helper() => void {}"),
        Ok(None)
    ));
}

#[test]
fn bodyless_function_elsewhere_is_rejected() {
    assert!(matches!(
        classify(&["app"], "f<T>(x: T) => void;"),
        Err(AnalysisErrorKind::FunctionWithoutBody { .. })
    ));
    assert!(matches!(
        classify(
            &["app", "volatile"],
            "exposed read_volatile<T>(location: *T) => T;"
        ),
        Err(AnalysisErrorKind::FunctionWithoutBody { .. })
    ));
}

#[test]
fn recognized_name_with_the_wrong_shape_is_malformed() {
    for source in [
        "exposed read_volatile<T>(location: *T) => T { *location }",
        "read_volatile<T>(location: *T) => T;",
        "@inline exposed read_volatile<T>(location: *T) => T;",
        "exposed read_volatile<T: Copy>(location: *T) => T;",
        "exposed read_volatile<T = u8>(location: *T) => T;",
        "exposed read_volatile<T, U>(location: *T) => T;",
        "exposed read_volatile(location: *u32) => u32;",
        "exposed read_volatile<T>(location: *mut T) => T;",
        "exposed read_volatile<T>(address: *T) => T;",
        "exposed read_volatile<T>(location: *T) => void;",
        "exposed write_volatile<T>(location: *T, value: T) => void;",
        "exposed write_volatile<T>(location: *mut T, value: T) => T;",
        "exposed write_volatile<T>(location: *mut T) => void;",
    ] {
        assert!(
            matches!(
                classify(VOLATILE, source),
                Err(AnalysisErrorKind::MalformedCompilerFunction { .. })
            ),
            "{source}"
        );
    }
}
