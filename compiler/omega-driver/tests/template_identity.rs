//! What a generic function's linker name says about the *declaration* it came
//! from, as opposed to the instantiation.
//!
//! Explicit bound selection can reach two declarations of one name at the same
//! concrete arguments, so their symbols have to differ. The other half of that
//! contract is just as load-bearing: one declaration must keep one name across
//! compilations, however it happened to be spelled, or two packages would stop
//! agreeing on the instantiation they both emit.

use omega_analyzer::Target;
use omega_driver::Driver;
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
            "omega_template_identity_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    /// Every function symbol this package defines.
    fn symbols(&self) -> Vec<String> {
        let program = Driver::new(self.0.clone(), None, vec![], Target::DEFAULT)
            .expect("construct driver")
            .compile(&[Ident("main".into())])
            .unwrap_or_else(|errors| panic!("expected this to compile, got: {errors:#?}"));
        let entry = program.entry.clone();
        omega_mir::lower_program(program.modules, &entry)
            .into_iter()
            .flat_map(|(_, module)| module.items)
            .flat_map(|item| match item {
                omega_mir::MirItem::FunctionDefinition(f) => vec![f.symbol],
                omega_mir::MirItem::Struct(s) => {
                    s.functions.into_iter().map(|f| f.symbol).collect()
                }
                _ => Vec::new(),
            })
            .collect()
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

/// A demangled symbol with any `{...}` declaration descriptor removed.
fn without_descriptor(symbol: &str) -> String {
    let mut out = String::with_capacity(symbol.len());
    let mut depth = 0usize;
    for character in symbol.chars() {
        match character {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ if depth == 0 => out.push(character),
            _ => {}
        }
    }
    out
}

/// The one symbol `source` emits for `main::<name>`.
fn symbol_of(source: &str, name: &str) -> String {
    let package = TestPackage::new(source);
    package_symbol(&package, name)
}

fn package_symbol(package: &TestPackage, name: &str) -> String {
    let prefix = format!("main::{name}");
    let matching: Vec<String> = package
        .symbols()
        .into_iter()
        .filter(|symbol| {
            omega_mangle::demangle(symbol)
                .is_some_and(|rendered| without_descriptor(&rendered).starts_with(&prefix))
        })
        .collect();
    let [symbol] = matching.as_slice() else {
        panic!("expected exactly one definition of '{name}', got {matching:#?}");
    };
    symbol.clone()
}

#[test]
fn anonymous_enum_template_members_are_a_flat_unordered_set() {
    let program = |typ: &str| {
        format!(
            "alias Both<X> = enum X | i64;
         pick<T>(value: {typ}) => i32 {{ 1 }}
         main() => void {{ f: (enum i32 | i64) => i32 = pick<i32>; }}"
        )
    };
    let expected = symbol_of(&program("enum T | i64"), "pick");
    for typ in ["enum i64 | T", "enum T | i64 | T", "enum Both<T> | i64"] {
        assert_eq!(expected, symbol_of(&program(typ), "pick"), "{typ}");
    }
    assert_ne!(expected, symbol_of(&program("enum i32 | i64"), "pick"));
}

#[test]
fn defaulted_nominal_types_have_one_template_representation() {
    let program = |typ: &str| {
        format!(
            "struct Box<T = i32> {{ value: T; }}
         pick<U>(value: {typ}) => i32 {{ 1 }}
         main() => void {{ f: (*Box<i32>) => i32 = pick<i32>; }}"
        )
    };
    assert_eq!(
        symbol_of(&program("*Box"), "pick"),
        symbol_of(&program("*Box<i32>"), "pick"),
    );
}

#[test]
fn spec_defaults_keep_their_module_and_cannot_capture_function_parameters() {
    let symbol = |bound: &str| {
        let package = TestPackage::new(&format!(
            "import self::other;
             import self::other::Holds;
             marker M {{}}
             meet Holds<other::Value, 2, other::Value, 2> for M {{}}
             pick<Value, comp Count: usize, T: {bound}>() => i32 {{ 1 }}
             main() => void {{ x := pick<i32, 3, M>(); }}"
        ));
        fs::write(
            package.0.join("other.omg"),
            "exposed marker Value {}
             comp Count := 2usize;
             exposed spec Holds<U = Value, comp N: usize = Count, V = U, comp K: usize = N> {}",
        )
        .unwrap();
        package_symbol(&package, "pick")
    };
    assert_eq!(
        symbol("Holds"),
        symbol("Holds<other::Value, 2, other::Value, 2>")
    );
}

#[test]
fn spec_defaults_preserve_symbolic_type_and_value_arguments() {
    let program = |bound: &str| {
        format!(
            "spec Holds<U, comp N: usize, V = U, comp K: usize = N> {{}}
         marker M {{}}
         meet Holds<M, 2, M, 2> for M {{}}
         pick<T: {bound}, comp N: usize>() => i32 {{ 1 }}
         main() => void {{ x := pick<M, 2>(); }}"
        )
    };
    assert_eq!(
        symbol_of(&program("Holds<T, N>"), "pick"),
        symbol_of(&program("Holds<T, N, T, N>"), "pick"),
    );
}

#[test]
fn substituted_comp_defaults_use_each_destination_slots_type() {
    let program = |bound: &str| {
        format!(
            "struct Box<comp N: usize> {{ value: i32; }}
         spec Holds<comp N: u8, comp K: usize = N, U = Box<N>> {{}}
         marker M {{}}
         meet Holds<2, 2, Box<2>> for M {{}}
         pick<T: {bound}>() => i32 {{ 1 }}
         main() => void {{ x := pick<M>(); }}"
        )
    };
    assert_eq!(
        symbol_of(&program("Holds<2>"), "pick"),
        symbol_of(&program("Holds<2, 2, Box<2>>"), "pick"),
    );
}

/// `Mark` conforms to `A`, to `B`, and to `Holds<Mark>` at the spec's default
/// `comp` argument, so every spelling exercised below has something to
/// instantiate at.
const PRELUDE: &str = r#"
    spec A { a_mark(*self) => i32; }
    spec B { b_mark(*self) => i32; }
    spec Holds<T, comp N: usize = 2> { held(*self) => T; }
    struct Mark { exposed value: i32; }
    meet A for Mark { a_mark(*self) => i32 { 1 } }
    meet B for Mark { b_mark(*self) => i32 { 2 } }
    meet Holds<Mark, 2> for Mark { held(*self) => Mark { *self } }
    meet Holds<Mark, 3> for Mark { held(*self) => Mark { *self } }
    alias Marked = A;
"#;

fn program(declaration: &str) -> String {
    format!(
        "{PRELUDE}
    @suppress(unused_parameter)
    pick{declaration} => i32 {{ 7 }}
    main() => void {{ used := pick(Mark {{ value = 1; }}); }}
"
    )
}

#[test]
fn a_bound_on_the_declarations_own_parameter_is_not_the_type_it_binds() {
    // Both instantiate as `pick<Mark>(Mark) => i32` and both require
    // `Mark: Holds<Mark>`; only the unsubstituted declaration tells them
    // apart, which is why the descriptor is taken before substitution.
    assert_ne!(
        symbol_of(&program("<T: Holds<T>>(value: T)"), "pick"),
        symbol_of(&program("<T: Holds<Mark>>(value: T)"), "pick"),
    );
}

#[test]
fn a_declarations_identity_survives_every_equivalent_spelling() {
    let canonical = symbol_of(&program("<T: A + B>(value: T)"), "pick");
    for equivalent in [
        // A parameter name is not identity.
        "<Thing: A + B>(value: Thing)",
        // A bound set is unordered.
        "<T: B + A>(value: T)",
        // A transparent alias is the spec it names.
        "<T: Marked + B>(value: T)",
        // A default decides an argument; once the argument is fixed it is
        // not part of which declaration was instantiated.
        "<T: A + B = Mark>(value: T)",
    ] {
        assert_eq!(
            canonical,
            symbol_of(&program(equivalent), "pick"),
            "'{equivalent}' is the same declaration",
        );
    }
}

#[test]
fn a_spec_default_normalizes_like_the_argument_it_stands_for() {
    assert_eq!(
        symbol_of(&program("<T: Holds<Mark>>(value: T)"), "pick"),
        symbol_of(&program("<T: Holds<Mark, 2>>(value: T)"), "pick"),
    );
}

#[test]
fn a_different_comp_argument_is_a_different_declaration() {
    // The value slot participates, so the same spec at two compile-time
    // arguments is two bounds -- and `2` is the default, not a wildcard.
    assert_ne!(
        symbol_of(&program("<T: Holds<Mark>>(value: T)"), "pick"),
        symbol_of(&program("<T: Holds<Mark, 3>>(value: T)"), "pick"),
    );
}

#[test]
fn a_declaration_with_no_generic_parameters_of_its_own_is_unchanged() {
    // The descriptor exists to separate declarations a selector can choose
    // between. Everything else keeps the symbol it already had.
    let concrete = symbol_of(
        &format!(
            "{PRELUDE}\n plain(value: i32) => i32 {{ value }}\n main() => void {{ used := plain(1); }}\n"
        ),
        "plain",
    );
    assert!(
        !omega_mangle::demangle(&concrete)
            .expect("a mangled symbol demangles")
            .contains('{'),
        "a concrete declaration carries no descriptor: {concrete}",
    );
}
