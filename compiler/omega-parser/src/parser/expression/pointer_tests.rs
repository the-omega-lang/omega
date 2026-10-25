use crate::SourceModule;
use crate::ast::expression::{BinaryOp, Expression, ExpressionNode};
use crate::ast::item::Item;
use crate::ast::statement::Statement;
use crate::diagnostics::ParseErrorKind;

/// A compact rendering of an expression's tree, so a test states the
/// grouping it expects instead of walking nested nodes.
fn shape(node: &ExpressionNode) -> String {
    match &node.expression {
        Expression::Path(path) => path.path.head.as_ref().to_string(),
        Expression::Number(n) => n.integer_part.clone(),
        Expression::FieldAccess(f) => format!("(. {} {})", shape(&f.base), f.field.as_ref()),
        Expression::Deref(d) => format!("(.* {})", shape(&d.base)),
        Expression::AddressOf(a) => {
            format!("({} {})", if a.mutable { "^" } else { "*" }, shape(&a.base))
        }
        Expression::Index(i) => format!("([] {} {})", shape(&i.base), shape(&i.index)),
        Expression::Slice(s) => format!("([..] {})", shape(&s.base)),
        Expression::BinaryOp(b) => {
            let op = match b.op {
                BinaryOp::Mul => "*",
                BinaryOp::BitAnd => "&",
                BinaryOp::BitXor => "^",
                BinaryOp::Add => "+",
                other => panic!("no test spelling for {other:?}"),
            };
            format!("({op} {} {})", shape(&b.left), shape(&b.right))
        }
        Expression::Assignment(a) => format!("(= {} {})", shape(&a.target), shape(&a.value)),
        Expression::CompoundAssign(a) => {
            assert_eq!(a.op, BinaryOp::Mul, "only `*=` is spelled here");
            format!("(*= {} {})", shape(&a.target), shape(&a.value))
        }
        Expression::FunctionCall(call) => format!("(call {})", shape(&call.callee)),
        other => panic!("no test shape for {other:?}"),
    }
}

/// The shape of every expression statement in `body`.
fn shapes(body: &str) -> Vec<String> {
    let source = format!("f() => void {{ {body} }}");
    let module = SourceModule::parse(&source)
        .unwrap_or_else(|errors| panic!("`{source}` must parse: {errors:?}"));
    let Item::FunctionDefinition(f) = &module.nodes[0].item else {
        panic!("first item must be a function");
    };
    f.codeblock
        .as_ref()
        .unwrap()
        .statements
        .iter()
        .map(|statement| match &statement.statement {
            Statement::Expression(expr) => shape(expr),
            Statement::Walrus(walrus) => shape(&walrus.value),
            other => panic!("unexpected statement {other:?}"),
        })
        .collect()
}

#[test]
fn prefix_address_of_applies_to_the_whole_postfix_chain() {
    assert_eq!(
        shapes("*a.b.c; ^a[i]; s := *arr[0..<n];"),
        ["(* (. (. a b) c))", "(^ ([] a i))", "(* ([..] arr))"]
    );
}

#[test]
fn dereference_is_a_postfix_step_that_chains() {
    assert_eq!(
        shapes("p.*.f; p.*.*; p.*= v; p.**= v; p.*(); *p.*;"),
        [
            "(. (.* p) f)",
            "(.* (.* p))",
            "(= (.* p) v)",
            "(*= (.* p) v)",
            "(call (.* p))",
            "(* (.* p))",
        ]
    );
}

#[test]
fn binary_operators_keep_their_meaning_beside_the_pointer_operators() {
    assert_eq!(
        shapes("a & b; a ^ b; a * b.*; a ^ ^b; a * *b;"),
        [
            "(& a b)",
            "(^ a b)",
            "(* a (.* b))",
            "(^ a (^ b))",
            "(* a (* b))",
        ]
    );
}

#[test]
fn mut_is_an_ordinary_name() {
    assert_eq!(
        shapes("^mut := 1; mut = 2; p := *mut;"),
        ["1", "(= mut 2)", "(* mut)"]
    );
    SourceModule::parse("f(mut: i32) => void { } g(p: *mut) => void { }")
        .expect("`mut` names a parameter and a type");
}

fn errors(source: &str) -> Vec<(ParseErrorKind, String)> {
    SourceModule::parse(source)
        .err()
        .unwrap_or_else(|| panic!("`{source}` must be rejected"))
        .into_iter()
        .map(|error| {
            (
                error.kind,
                source[error.span.start..error.span.end].to_string(),
            )
        })
        .collect()
}

fn single_error(source: &str) -> (ParseErrorKind, String) {
    let errors = errors(source);
    assert_eq!(errors.len(), 1, "`{source}`: {errors:?}");
    errors.into_iter().next().unwrap()
}

#[test]
fn retired_address_of_spellings_are_reported_once() {
    assert_eq!(
        single_error("f() => void { p := &x; }"),
        (
            ParseErrorKind::LegacyAddressOf { mutable: false },
            "&".into()
        )
    );
    assert_eq!(
        single_error("f() => void { p := &mut x; }"),
        (
            ParseErrorKind::LegacyAddressOf { mutable: true },
            "&mut".into()
        )
    );
}

#[test]
fn retired_mut_spellings_are_reported_once() {
    for (source, spelling) in [
        ("f() => void { mut x := 1; }", "mut"),
        ("f() => void { mut comp x := 1; }", "mut"),
        ("mut global : i32 = 0;", "mut"),
        ("f(mut a: i32) => void { }", "mut"),
        ("f() => void { for mut x in xs { } }", "mut"),
        ("foreign mut errno : i32;", "mut"),
    ] {
        assert_eq!(
            single_error(source),
            (ParseErrorKind::LegacyMutBinding, spelling.into()),
            "{source}"
        );
    }
    assert_eq!(
        single_error("x : *mut i32 = 0;"),
        (
            ParseErrorKind::LegacyMutPointer { receiver: false },
            "*mut".into()
        )
    );
}

#[test]
fn retired_receiver_spellings_are_reported_once() {
    assert_eq!(
        single_error("struct S { f(mut self) => void { } }"),
        (ParseErrorKind::LegacyMutSelf, "mut self".into())
    );
    assert_eq!(
        single_error("struct S { f(*mut self) => void { } }"),
        (
            ParseErrorKind::LegacyMutPointer { receiver: true },
            "*mut self".into()
        )
    );
    assert_eq!(
        single_error("alias F = (mut self) => void;"),
        (ParseErrorKind::LegacyMutSelf, "mut self".into())
    );
}

#[test]
fn a_caret_dereference_is_reported_once() {
    assert_eq!(
        single_error("f() => void { p.^ = 1; }"),
        (ParseErrorKind::CaretDereference, ".^".into())
    );
}

#[test]
fn a_mutable_parameter_needs_a_body() {
    for (source, context) in [
        ("alias F = (^a: i32) => void;", "a function type"),
        (
            "foreign f(^a: i32) => void;",
            "a foreign function declaration",
        ),
        (
            "f(^a: i32) => void;",
            "a function declaration without a body",
        ),
        (
            "spec S { f(*self, ^a: i32) => void; }",
            "a spec function without a body",
        ),
    ] {
        assert_eq!(
            single_error(source),
            (
                ParseErrorKind::MutableParamWithoutBody { context },
                "^a".into()
            ),
            "{source}"
        );
    }
    SourceModule::parse("f(^a: i32) => void { a += 1; } foreign(c) g(^a: i32) => void { }")
        .expect("a declaration with a body may take a mutable parameter");
}
