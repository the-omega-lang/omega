//! The compiler-implemented `core::builtins` macros: which declarations are
//! backed by the compiler, what they substitute, and which invocation site
//! they describe.

use omega_diagnostics::SourceFile;
use omega_parser::macros::{self, MacroError};
use omega_parser::prelude::{Expression, Ident, Item, MacroDefinitionStmt, SourceModule};
use std::collections::HashMap;

const BUILTINS: &str = "core::builtins";

const DECLARATIONS: &str = "exposed macro file() => { }\n\
                            exposed macro line() => { }\n\
                            exposed macro column() => { }\n";

fn module_path(path: &str) -> Vec<Ident> {
    path.split("::").map(|s| Ident(s.to_string())).collect()
}

fn expand_at(
    path: &str,
    source: &str,
    file: Option<&SourceFile>,
    imported: &HashMap<Ident, MacroDefinitionStmt>,
) -> Result<SourceModule, MacroError> {
    macros::expand_with_origins(
        SourceModule::parse(source).expect("source parses"),
        imported,
        &module_path(path),
        file,
        &mut macros::ExpansionState::default(),
    )
}

fn expand_builtins(source: &str) -> (SourceFile, SourceModule) {
    let file = SourceFile::new("probe.omg", source);
    let module =
        expand_at(BUILTINS, source, Some(&file), &HashMap::new()).expect("expansion succeeds");
    (file, module)
}

/// The tail expression of the `index`-th top-level function.
fn probe(module: &SourceModule, index: usize) -> &Expression {
    let Item::FunctionDefinition(function) = &module.nodes[index].item else {
        panic!("expected a function definition");
    };
    &function
        .codeblock
        .tail
        .as_ref()
        .expect("probe function has a tail expression")
        .expression
}

fn number(expression: &Expression) -> (&str, Option<&str>) {
    let Expression::Number(number) = expression else {
        panic!("expected a number literal, found {expression:?}");
    };
    (
        number.integer_part.as_str(),
        number.explicit_type.as_ref().map(Ident::as_ref),
    )
}

fn text(expression: &Expression) -> &str {
    let Expression::String(string) = expression else {
        panic!("expected a string literal, found {expression:?}");
    };
    &string.0
}

fn location_of(file: &SourceFile, source: &str, needle: &str) -> (usize, usize) {
    file.line_col(source.find(needle).expect("invocation is present"))
}

#[test]
fn the_canonical_declarations_substitute_source_location_literals() {
    let source = format!(
        "{DECLARATIONS}probe_file() => void {{ file$() }}\n\
         probe_line() => void {{ line$() }}\n\
         probe_column() => void {{ column$() }}\n"
    );
    let (file, module) = expand_builtins(&source);

    assert_eq!(text(probe(&module, 0)), "probe.omg");

    let (line, _) = location_of(&file, &source, "line$()");
    assert_eq!(line, 5);
    assert_eq!(
        number(probe(&module, 1)),
        (line.to_string().as_str(), Some("u32"))
    );

    let (_, column) = location_of(&file, &source, "column$()");
    assert_eq!(column, 26);
    assert_eq!(
        number(probe(&module, 2)),
        (column.to_string().as_str(), Some("u32"))
    );
}

#[test]
fn a_same_named_macro_outside_core_builtins_stays_an_ordinary_template() {
    let source = "exposed macro file() => { 7 }\nprobe() => void { file$() }\n";
    let file = SourceFile::new("probe.omg", source);
    let module =
        expand_at("app::helper", source, Some(&file), &HashMap::new()).expect("expansion succeeds");

    assert_eq!(number(probe(&module, 0)), ("7", None));
}

#[test]
fn a_canonical_declaration_must_match_the_compiler_contract() {
    for source in [
        "macro file() => { }",
        "shared macro line() => { }",
        "exposed macro column($at: expr) => { }",
        "exposed macro file($rest: expr...) => { }",
        "exposed macro line() => { 1 }",
    ] {
        let file = SourceFile::new("probe.omg", source);
        let error = expand_at(BUILTINS, source, Some(&file), &HashMap::new())
            .err()
            .unwrap_or_else(|| panic!("expected `{source}` to be rejected"));
        assert!(
            matches!(error, MacroError::MalformedBuiltinDeclaration { .. }),
            "expected a builtin-contract error for `{source}`, found {error}"
        );
    }
}

#[test]
fn a_builtin_takes_no_arguments_at_its_invocation() {
    let source = format!("{DECLARATIONS}probe() => void {{ file$(1) }}\n");
    let file = SourceFile::new("probe.omg", source.as_str());
    let error = expand_at(BUILTINS, &source, Some(&file), &HashMap::new())
        .err()
        .expect("an argument to a zero-parameter builtin is an error");

    assert!(
        matches!(error, MacroError::ArgCountMismatch { .. }),
        "expected an arity error, found {error}"
    );
}

#[test]
fn a_builtin_without_source_context_fails_rather_than_inventing_a_location() {
    let source = format!("{DECLARATIONS}probe() => void {{ line$() }}\n");
    let error = expand_at(BUILTINS, &source, None, &HashMap::new())
        .err()
        .expect("a builtin needs the invoking module's source");

    assert!(
        matches!(error, MacroError::BuiltinWithoutSourceContext { .. }),
        "expected a missing-source error, found {error}"
    );
}

#[test]
fn locations_use_the_diagnostic_line_and_display_column_rules() {
    // The invocation sits behind one tab and one multibyte character, so a
    // byte offset and a display column disagree here. `column$()` must report
    // what a rendered diagnostic caret would: a tab is `SourceFile`'s tab
    // width and `π` is a single column.
    let source = format!("{DECLARATIONS}probe() => void {{\n\t\"π\" ; column$() }}\n");
    let file = SourceFile::new("wide.omg", source.as_str());
    let module =
        expand_at(BUILTINS, &source, Some(&file), &HashMap::new()).expect("expansion succeeds");

    assert_eq!(location_of(&file, &source, "column$()"), (5, 11));
    assert_eq!(number(probe(&module, 0)), ("11", Some("u32")));
}

#[test]
fn a_builtin_inside_a_wrapper_macro_reports_the_wrapper_call_site() {
    let source = format!(
        "{DECLARATIONS}exposed macro here() => {{ line$() }}\n\
         exposed macro here_indirect() => {{ here$() }}\n\
         direct() => void {{ line$() }}\n\
         wrapped() => void {{ here$() }}\n\
         nested() => void {{ here_indirect$() }}\n"
    );
    let (_, module) = expand_builtins(&source);

    assert_eq!(number(probe(&module, 0)), ("6", Some("u32")));
    assert_eq!(number(probe(&module, 1)), ("7", Some("u32")));
    assert_eq!(number(probe(&module, 2)), ("8", Some("u32")));
}

#[test]
fn an_alias_of_a_builtin_keeps_builtin_behavior_at_the_alias_call_site() {
    // The driver binds a macro alias by cloning the target definition under
    // the alias's own name and visibility; the builtin discriminator must
    // survive that clone.
    let Item::MacroDefinition(mut target) = SourceModule::parse(DECLARATIONS)
        .expect("declarations parse")
        .nodes
        .into_iter()
        .nth(1)
        .expect("three declarations")
        .item
    else {
        panic!("expected a macro definition");
    };
    macros::bind_definition(&mut target, &module_path(BUILTINS)).expect("canonical declaration");

    let alias = MacroDefinitionStmt {
        name: Ident("where_am_i".into()),
        ..target
    };
    let mut imported = HashMap::new();
    imported.insert(alias.name.clone(), alias);

    let source = "probe() => void { where_am_i$() }\n";
    let file = SourceFile::new("probe.omg", source);
    let module =
        expand_at("app::helper", source, Some(&file), &imported).expect("expansion succeeds");

    assert_eq!(number(probe(&module, 0)), ("1", Some("u32")));
}
