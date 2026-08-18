use omega_parser::SourceModule;
use omega_parser::parser::contextual;
use omega_parser::prelude::Item;

#[test]
fn every_contextual_keyword_stays_an_ordinary_identifier() {
    for word in contextual::ALL {
        let source = format!(
            "{word} := 1;\n\
             use_it() => i32 {{ {word} := 2; return {word}; }}\n\
             {word}_fn() => i32 {{ return 0; }}"
        );
        let module = SourceModule::parse(&source)
            .unwrap_or_else(|e| panic!("`{word}` must stay usable as an identifier: {e:?}"));
        assert!(
            matches!(module.nodes[0].item, Item::Walrus(_)),
            "`{word}` should have parsed as a top-level binding"
        );
        assert_eq!(module.nodes.len(), 3, "`{word}`: expected three items");
    }
}

#[test]
fn every_contextual_keyword_works_as_a_parameter_and_field_name() {
    for word in contextual::ALL {
        let params = if *word == contextual::SELF {
            format!("a: i32, {word}: i32")
        } else {
            format!("{word}: i32")
        };
        let source = format!(
            "struct Holder {{ {word}: i32; }}\n\
             takes({params}) => i32 {{ return 0; }}"
        );
        SourceModule::parse(&source)
            .unwrap_or_else(|e| panic!("`{word}` must work as a parameter and field name: {e:?}"));
    }
}

#[test]
fn the_registry_has_no_duplicates() {
    let mut sorted = contextual::ALL.to_vec();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "duplicate entry in contextual::ALL");
}
