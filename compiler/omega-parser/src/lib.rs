//! Omega's lexer, parser, macro expander, and syntax tree.
//! [`prelude`] is this crate's supported surface; the module layout under
//! [`ast`] and [`parser`] is an implementation detail.

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
    use crate::ast::item::Item;
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
    fn parses_conform_and_primitive_items() {
        let module = SourceModule::parse(
            "spec Show { show(*self) => i32; }\n\
             struct Box<T> { value: T; }\n\
             conform<T> Box<T> to Show { show(*self) => i32 { 1 } }\n\
             primitive<T> []T { exposed is_empty(*self) => bool { self.length == 0 } }",
        )
        .expect("conform and primitive declarations should parse");
        assert!(matches!(module.nodes[2].item, Item::Conform(_)));
        assert!(matches!(module.nodes[3].item, Item::Primitive(_)));
    }

    #[test]
    fn conform_and_primitive_enforce_their_visibility_shapes() {
        assert!(matches!(
            errors("spec Show { show(*self) => i32; } struct S {} conform S to Show { exposed show(*self) => i32 { 1 } }").as_slice(),
            [ParseErrorKind::ConformMethodVisibility]
        ));
        assert!(matches!(
            errors("exposed primitive i32 { exposed value(*self) => i32 { *self } }").as_slice(),
            [ParseErrorKind::PrimitiveVisibility]
        ));
    }

    #[test]
    fn conform_to_and_primitive_stay_contextual_identifiers() {
        let module = SourceModule::parse(
            "conform := 1;\n\
             primitive : i32 = 2;\n\
             to := 3;\n\
             conform() => i32 { primitive := 3; return primitive; }\n\
             primitive() => i32 { return conform; }\n\
             to() => i32 { return to; }",
        )
        .expect("conform, to and primitive must remain usable as ordinary identifiers");
        assert!(matches!(module.nodes[0].item, Item::Walrus(_)));
        assert!(matches!(
            module.nodes[1].item,
            Item::DeclarationWithInit(..)
        ));
        assert!(matches!(module.nodes[2].item, Item::Walrus(_)));
        assert!(matches!(module.nodes[3].item, Item::FunctionDefinition(_)));
        assert!(matches!(module.nodes[4].item, Item::FunctionDefinition(_)));
        assert!(matches!(module.nodes[5].item, Item::FunctionDefinition(_)));
    }

    #[test]
    fn rejects_removed_conformance_syntax() {
        assert!(SourceModule::parse("spec Ops for i32 { value(*self) => i32; }").is_err());
        assert!(
            SourceModule::parse("spec Ops { value(*self) => i32; } struct S : Ops {}").is_err()
        );
        // `conform Target : Spec` -- the separator the `to` spelling replaced.
        assert!(matches!(
            errors("spec Show { show(*self) => i32; } struct S {} conform S : Show { show(*self) => i32 { 1 } }")
                .as_slice(),
            [ParseErrorKind::Expected { expected: "to", .. }, ..]
        ));
    }
    #[test]
    fn chained_comparison_reports_its_own_error() {
        assert!(matches!(
            errors("a := b < c < d;").as_slice(),
            [ParseErrorKind::ChainedComparison]
        ));
    }

    /// A `glue` function may be neither generic nor `self`-taking. This has
    /// its own error rather than reusing `Expected`, whose `found` field is
    /// documented as built from a `TokenKind`, not prose.
    #[test]
    fn glue_rejects_generic_and_self_taking_functions() {
        for source in [
            "glue Foo { f<T>(x: T) => void { } }",
            "glue Foo { f(*self) => void { } }",
        ] {
            assert!(
                errors(source)
                    .iter()
                    .any(|e| matches!(e, ParseErrorKind::GlueFunctionShape { .. })),
                "`{source}` should report GlueFunctionShape, got {:?}",
                errors(source)
            );
        }
    }

    /// The names of the members the *last* item in `source` parsed, and how
    /// many errors were reported. `SourceModule::parse` discards the tree
    /// whenever anything failed, which is exactly what a recovery test needs
    /// to see, so this drives the parser directly.
    fn recovered_members(source: &str) -> (Vec<String>, usize) {
        let (tokens, lex_errors) = lexer::tokenize(source);
        let mut parser = parser::Parser::new(&tokens);
        let nodes = parser::item::parse_source_module(&mut parser);
        let error_count = lex_errors.len() + parser.into_errors().len();
        let names: Vec<Ident> = match &nodes.last().expect("at least one item").item {
            Item::Conform(c) => c.functions.iter().map(|f| f.ident.clone()).collect(),
            Item::Primitive(pr) => pr.functions.iter().map(|f| f.ident.clone()).collect(),
            Item::Gap(g) => g.functions.iter().map(|f| f.ident.clone()).collect(),
            Item::Glue(g) => g.functions.iter().map(|f| f.ident.clone()).collect(),
            Item::Struct(s) => s.functions.iter().map(|f| f.ident.clone()).collect(),
            other => panic!("unexpected trailing item {other:?}"),
        };
        (
            names.into_iter().map(|i| i.as_ref().to_string()).collect(),
            error_count,
        )
    }

    /// Every item body recovers per member: one malformed declaration
    /// reports one error, and the members after it still parse. `conform`,
    /// `primitive`, `gap` and `glue` used to abandon the whole item on the
    /// first bad member while `struct`/`union`/`enum` recovered -- the same
    /// mistake reported differently depending on which block it was in.
    #[test]
    fn every_item_body_recovers_per_member() {
        // `?` is not a type, so `bad` is malformed; `good` must still land
        // in the *same* item rather than being lost with it.
        for source in [
            "struct S { bad(*self) => ? { } good(*self) => i32 { 1 } }",
            "spec Sp { m(*self) => i32; }\n\
             struct S {}\n\
             conform S to Sp { bad(*self) => ? { } good(*self) => i32 { 1 } }",
            "primitive i32 { bad(*self) => ? { } good(*self) => i32 { 1 } }",
            "gap G { bad() => ?; good() => i32; }",
            "glue G { bad() => ? { } good() => i32 { 1 } }",
        ] {
            let (members, error_count) = recovered_members(source);
            assert_eq!(
                members,
                ["good"],
                "`{source}` should keep parsing after the bad member"
            );
            assert_eq!(
                error_count, 1,
                "`{source}` should report exactly one error, not cascade"
            );
        }
    }

    /// `ParseError::to_diagnostic` is now the single definition site for
    /// every error's text, and `Display` reads its headline back from there.
    /// The compiler still forces a *new* variant to be given an arm (that
    /// match is exhaustive), but nothing forces the arm it gets to actually
    /// say anything -- an arm returning a bare `Diagnostic::error("")` would
    /// compile and render a blank error. This asserts the two properties
    /// every arm must have: a non-empty headline, and at least one label to
    /// anchor it.
    #[test]
    fn every_parse_error_renders_a_headline_and_a_label() {
        use crate::ast::identifier::Ident;
        use crate::diagnostics::{ParseError, Span};

        let name = || Ident("f".to_string());
        let kinds = [
            ParseErrorKind::Expected {
                expected: "a type",
                found: "';'".to_string(),
            },
            ParseErrorKind::UnterminatedString,
            ParseErrorKind::UnterminatedChar,
            ParseErrorKind::UnterminatedComment,
            ParseErrorKind::EvenMultilineStringDelimiter { count: 4 },
            ParseErrorKind::UnterminatedGroup { open: '(' },
            ParseErrorKind::UnterminatedGroup { open: '[' },
            ParseErrorKind::UnterminatedGroup { open: '{' },
            ParseErrorKind::InvalidCharacter('\u{7}'),
            ParseErrorKind::InvalidUnicodeEscape("D800".to_string()),
            ParseErrorKind::InvalidCharLiteral,
            ParseErrorKind::StructLiteralNotAllowedHere,
            ParseErrorKind::EnumFunctionBeforeSemi,
            ParseErrorKind::EnumNotAllowedHere,
            ParseErrorKind::StructNotAllowedHere,
            ParseErrorKind::UnionNotAllowedHere,
            ParseErrorKind::SpecNotAllowedHere,
            ParseErrorKind::SpecAliasCannotDeclareFunctions,
            ParseErrorKind::RangeMissingEnd,
            ParseErrorKind::OpenRangeHasEnd,
            ParseErrorKind::ChainedComparison,
            ParseErrorKind::NestingTooDeep { limit: 64 },
            ParseErrorKind::AnnotationNotAllowedHere,
            ParseErrorKind::VisibilityNotAllowedHere,
            ParseErrorKind::GapOrGlueVisibility,
            ParseErrorKind::ConformMethodVisibility,
            ParseErrorKind::PrimitiveVisibility,
            ParseErrorKind::GapOrGlueGeneric,
            ParseErrorKind::GapFunctionBody { name: name() },
            ParseErrorKind::GapFunctionSelf { name: name() },
            ParseErrorKind::GlueFunctionShape { name: name() },
            ParseErrorKind::DefaultGenericParamNotTrailing { name: name() },
            ParseErrorKind::SpecDependenciesRemoved,
            ParseErrorKind::MacroInvocationNotAllowedAfterDefer,
            ParseErrorKind::VariadicMacroParamNotLast,
            ParseErrorKind::InvalidMacroSeparator,
            ParseErrorKind::NestedMacroRepetition,
            ParseErrorKind::ImportInMacroBody,
        ];

        for kind in kinds {
            let rendered = ParseError::new(Span::new(0, 1), kind.clone()).to_diagnostic();
            assert!(
                !rendered.message.trim().is_empty(),
                "{kind:?} renders no headline"
            );
            assert!(!rendered.labels.is_empty(), "{kind:?} renders no label");
            // `Display` must agree with the headline it now reads back.
            assert_eq!(kind.to_string(), rendered.message, "{kind:?}");
        }
    }
}
