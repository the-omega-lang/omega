pub mod ast;
pub mod diagnostics;
pub mod highlight;
pub mod lexer;
pub mod macros;
pub mod parser;
pub mod prelude;

use diagnostics::ParseError;
use prelude::*;

#[derive(Debug, Clone)]
pub struct SourceModule {
    pub nodes: Vec<ItemNode>,
}

impl SourceModule {
    pub fn parse(source_code: &str) -> Result<Self, Vec<ParseError>> {
        let (tokens, lex_errors) = lexer::tokenize(source_code);
        let mut parser = parser::Parser::new(&tokens);
        let nodes = parser::item::parse_source_module(&mut parser);

        let mut errors = lex_errors;
        errors.extend(parser.into_errors());
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self { nodes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::statement::Item;
    use crate::diagnostics::ParseErrorKind;

    /// Every `ParseErrorKind` `source` reports, in order -- lets a negative
    /// test assert the *specific* diagnostic rather than just "some error",
    /// which is what makes it a regression test instead of a smoke test.
    fn errors(source: &str) -> Vec<ParseErrorKind> {
        SourceModule::parse(source)
            .err()
            .expect("expected this source to be rejected")
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn parses_first_class_gap_and_glue_items() {
        let module = SourceModule::parse(
            "gap Allocator { alloc(size: usize) => *mut u8; }\n\
             glue core::platform::Allocator { alloc(size: usize) => *mut u8 { <*mut u8>0 } }",
        )
        .expect("gap and glue declarations should parse");
        assert!(matches!(module.nodes[0].item, Item::Gap(_)));
        assert!(matches!(module.nodes[1].item, Item::Glue(_)));
    }

    #[test]
    fn gap_rejects_a_body_and_self_parameter() {
        assert!(SourceModule::parse("gap Invalid { f(*self) => void { } }").is_err());
        assert!(matches!(
            errors("gap Invalid { f(x: i32) => void { } }").as_slice(),
            [ParseErrorKind::GapFunctionBody { .. }]
        ));
        assert!(matches!(
            errors("gap Invalid { f(*self) => void; }").as_slice(),
            [ParseErrorKind::GapFunctionSelf { .. }]
        ));
    }

    /// Both forms are implicitly global (see `PLAN.md`), so a visibility
    /// modifier is a syntax error rather than something analysis has to
    /// decide the meaning of later.
    #[test]
    fn gap_and_glue_reject_a_visibility_modifier() {
        for source in [
            "exposed gap Foo { f() => void; }",
            "internal gap Foo { f() => void; }",
            "exposed glue Foo { f() => void { } }",
            "internal glue Foo { f() => void { } }",
        ] {
            assert!(
                matches!(
                    errors(source).as_slice(),
                    [ParseErrorKind::GapOrGlueVisibility]
                ),
                "expected exactly one visibility rejection for `{source}`"
            );
        }
    }

    /// Exactly *one* error each -- the generics list is consumed as recovery
    /// (`reject_gap_glue_generics`) so the rest of the item still parses,
    /// rather than being resynchronized into a second and third report of
    /// the same mistake.
    #[test]
    fn gap_and_glue_reject_generics_without_cascading() {
        assert!(matches!(
            errors("gap Foo<T> { f() => void; }").as_slice(),
            [ParseErrorKind::GapOrGlueGeneric]
        ));
        assert!(matches!(
            errors("glue Foo<i32> { f() => void { } }").as_slice(),
            [ParseErrorKind::GapOrGlueGeneric]
        ));
    }

    #[test]
    fn parses_variadic_spec_functions_and_typed_for_in_bindings() {
        SourceModule::parse(
            "spec Log { write(*self, ...) => void; }\n\
             main() => void { for value : u8 in source { } }",
        )
        .expect("both new contextual grammar forms should parse");
    }

    /// `gap`/`glue` are contextual keywords recognized only at item
    /// position, and only when followed by another identifier -- so both
    /// stay usable as ordinary names, including as top-level bindings in
    /// the same file as a real declaration (the one-token lookahead case).
    #[test]
    fn gap_and_glue_stay_ordinary_identifiers() {
        let module = SourceModule::parse(
            "gap := 5;\n\
             glue : i32 = 5;\n\
             gap Real { f() => void; }\n\
             glue Real { f() => void { } }\n\
             uses_them() => i32 { gap := 1; glue := 2; return gap + glue; }\n\
             glue() => i32 { return 0; }",
        )
        .expect("`gap`/`glue` must stay usable as ordinary identifiers");
        assert!(matches!(module.nodes[0].item, Item::Walrus(_)));
        assert!(matches!(
            module.nodes[1].item,
            Item::DeclarationWithInit(..)
        ));
        assert!(matches!(module.nodes[2].item, Item::Gap(_)));
        assert!(matches!(module.nodes[3].item, Item::Glue(_)));
        assert!(matches!(module.nodes[4].item, Item::FunctionDefinition(_)));
        assert!(matches!(module.nodes[5].item, Item::FunctionDefinition(_)));
    }

    /// A module genuinely named `glue` (the only remaining first-party
    /// collision shape after `core::glue` was renamed to `core::platform`)
    /// must still be importable and path-referencable alongside a real
    /// `glue` declaration -- `glue` leads an item only when it isn't
    /// followed by `::`-style path continuation handled by other arms.
    #[test]
    fn a_module_named_glue_coexists_with_a_glue_declaration() {
        let module = SourceModule::parse(
            "import glue::helper;\n\
             gap Real { f() => void; }\n\
             glue Real { f() => void { glue::helper(); } }",
        )
        .expect("a user module named `glue` must still parse");
        assert!(matches!(module.nodes[0].item, Item::Import(_)));
        assert!(matches!(module.nodes[2].item, Item::Glue(_)));
    }

    #[test]
    fn parses_compose_and_primitive_items() {
        let module = SourceModule::parse(
            "spec Show { show(*self) => i32; }\n\
             struct Box<T> { value: T; }\n\
             compose<T> Box<T> : Show { show(*self) => i32 { 1 } }\n\
             primitive<T> [?]T { exposed is_empty(*self) => bool { self.length == 0 } }",
        )
        .expect("compose and primitive declarations should parse");
        assert!(matches!(module.nodes[2].item, Item::Compose(_)));
        assert!(matches!(module.nodes[3].item, Item::Primitive(_)));
    }

    #[test]
    fn compose_and_primitive_enforce_their_visibility_shapes() {
        assert!(matches!(
            errors("spec Show { show(*self) => i32; } struct S {} compose S : Show { exposed show(*self) => i32 { 1 } }").as_slice(),
            [ParseErrorKind::ComposeMethodVisibility]
        ));
        assert!(matches!(
            errors("exposed primitive i32 { exposed value(*self) => i32 { *self } }").as_slice(),
            [ParseErrorKind::PrimitiveVisibility]
        ));
    }

    #[test]
    fn compose_and_primitive_stay_contextual_identifiers() {
        let module = SourceModule::parse(
            "compose := 1;\n\
             primitive : i32 = 2;\n\
             compose() => i32 { primitive := 3; return primitive; }\n\
             primitive() => i32 { return compose; }",
        )
        .expect("compose and primitive must remain usable as ordinary identifiers");
        assert!(matches!(module.nodes[0].item, Item::Walrus(_)));
        assert!(matches!(
            module.nodes[1].item,
            Item::DeclarationWithInit(..)
        ));
        assert!(matches!(module.nodes[2].item, Item::FunctionDefinition(_)));
        assert!(matches!(module.nodes[3].item, Item::FunctionDefinition(_)));
    }

    #[test]
    fn rejects_removed_conformance_syntax() {
        assert!(SourceModule::parse("spec Ops for i32 { value(*self) => i32; }").is_err());
        assert!(
            SourceModule::parse("spec Ops { value(*self) => i32; } struct S : Ops {}").is_err()
        );
    }
}
