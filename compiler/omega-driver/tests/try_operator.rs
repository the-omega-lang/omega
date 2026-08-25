//! `?` must survive semantic analysis as its own checked node and only become
//! ordinary control flow in MIR. These two facts are the architectural
//! contract the operator was designed around, so they are asserted directly
//! rather than inferred from runtime behavior.

use omega_analyzer::Target;
use omega_analyzer::checked::{
    CheckedBlock, CheckedExpr, CheckedExprNode, CheckedFunctionDef, CheckedItem, CheckedStmt,
    CheckedTry, CheckedTryKind,
};
use omega_driver::{CompileError, Driver, ExternRoot};
use omega_mir::body::{MirExpr, MirTerminator};
use omega_mir::mir::{MirFunctionBody, MirItem};
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
            "omega_try_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let root = parent.join("main");
        fs::create_dir_all(&root).expect("create test package");
        fs::write(root.join("main.omg"), source).expect("write root module");
        Self(root)
    }

    fn compile(&self) -> Result<omega_driver::CompiledProgram, Vec<CompileError>> {
        Driver::new(
            self.0.clone(),
            None,
            vec![ExternRoot {
                name: Ident("core".to_string()),
                dir: core_root(),
            }],
            Target::DEFAULT,
        )
        .expect("construct driver with the real core extern")
        .compile(&[Ident("main".to_string())], Target::DEFAULT)
    }

    fn expect_ok(&self) -> omega_driver::CompiledProgram {
        match self.compile() {
            Ok(program) => program,
            Err(errors) => panic!("expected this to compile, got: {errors:#?}"),
        }
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().expect("test root has a parent"));
    }
}

fn core_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/core")
        .canonicalize()
        .expect("runtime/core exists")
}

const OPTION_SOURCE: &str = "\
halve(n: i32) => Option<i32> {
	if n % 2 == 0 { Option<i32>::Some { value = n / 2; } } else { Option<i32>::None }
}

quarter(n: i32) => Option<i32> {
	half := halve(n)?;
	halve(half)
}

main() => void {}
";

const RESULT_SOURCE: &str = "\
struct Denied { exposed code: i32; }

alias AnyError = enum Denied | *str;

failing(n: i32) => Result<i32, *str> {
	if n > 0 { Result<i32, *str>::Ok { value = n; } } else { Result<i32, *str>::Err { error = \"no\"; } }
}

widened(n: i32) => Result<i32, AnyError> {
	Result<i32, AnyError>::Ok { value = failing(n)?; }
}

main() => void {}
";

fn function<'a>(program: &'a omega_driver::CompiledProgram, name: &str) -> &'a CheckedFunctionDef {
    program
        .modules
        .iter()
        .flat_map(|(_, module)| &module.items)
        .find_map(|item| match item {
            CheckedItem::FunctionDefinition(f) if f.name.as_ref() == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a checked function named `{name}`"))
}

fn find_try(block: &CheckedBlock) -> Option<&CheckedTry> {
    block
        .stmts
        .iter()
        .find_map(|stmt| match stmt {
            CheckedStmt::Declaration(_) => None,
            CheckedStmt::Expression(e) | CheckedStmt::Return(e) => try_in_expr(e),
            _ => None,
        })
        .or_else(|| block.tail.as_deref().and_then(try_in_expr))
}

fn try_in_expr(expr: &CheckedExprNode) -> Option<&CheckedTry> {
    match &expr.kind {
        CheckedExpr::Try(r#try) => Some(r#try),
        CheckedExpr::EnumConstruct(construct) => {
            construct.fields.iter().find_map(|f| try_in_expr(&f.value))
        }
        CheckedExpr::Assignment(assignment) => try_in_expr(&assignment.value),
        CheckedExpr::Codeblock(block) => find_try(block),
        _ => None,
    }
}

#[test]
fn an_option_try_survives_analysis_with_resolved_metadata() {
    let program = TestPackage::new(OPTION_SOURCE).expect_ok();
    let quarter = function(&program, "quarter");
    let r#try = find_try(&quarter.body).expect("`?` must still be a CheckedExpr::Try");

    assert_eq!(r#try.kind, CheckedTryKind::Option);
    assert_ne!(
        r#try.source.success_variant, r#try.source.failure_variant,
        "the two variants are resolved independently, not assumed by order"
    );
    assert!(
        r#try.source.failure_payload.is_none(),
        "`Option::None` carries no payload"
    );
    assert!(
        r#try.destination.failure_field.is_none(),
        "the destination `None` needs no field to build"
    );
    assert!(
        r#try.destination.error_coercion.is_identity(),
        "an Option propagation converts nothing"
    );
    assert!(
        matches!(
            r#try.operand.kind,
            CheckedExpr::FunctionCall(_) | CheckedExpr::Place(_)
        ),
        "the analyzed operand is preserved under the operator"
    );
}

#[test]
fn a_result_try_records_the_error_coercion_it_decided() {
    let program = TestPackage::new(RESULT_SOURCE).expect_ok();
    let widened = function(&program, "widened");
    let r#try = find_try(&widened.body).expect("`?` must still be a CheckedExpr::Try");

    assert_eq!(r#try.kind, CheckedTryKind::Result);
    assert!(
        r#try.source.failure_payload.is_some(),
        "`Result::Err` carries the error payload"
    );
    assert!(
        !r#try.destination.error_coercion.is_identity(),
        "`*str` reaches `enum Denied | *str` through a recorded conversion, \
         decided by the analyzer rather than by MIR"
    );
}

#[test]
fn mir_lowers_a_try_into_ordinary_branching_control_flow() {
    let program = TestPackage::new(OPTION_SOURCE).expect_ok();
    let modules = program
        .modules
        .iter()
        .map(|(path, module)| (path.clone(), module.clone()))
        .collect();
    let mir = omega_mir::lower_program(modules, &program.entry);

    let body = mir
        .iter()
        .flat_map(|(_, module)| &module.items)
        .find_map(|item| match item {
            MirItem::FunctionDefinition(f) if f.name.as_ref() == "quarter" => match &f.body {
                MirFunctionBody::Normal(body) => Some(body),
                MirFunctionBody::Naked(_) => None,
            },
            _ => None,
        })
        .expect("`quarter` is lowered to an ordinary MIR body");

    assert!(
        body.blocks
            .iter()
            .any(|block| matches!(block.terminator, MirTerminator::Branch { .. })),
        "the operator becomes a real branch on the operand's tag"
    );
    assert!(
        body.blocks.iter().any(|block| block
            .statements
            .iter()
            .any(|stmt| contains_enum_construct(&stmt.kind))),
        "the failure arm builds the enclosing function's own failure value"
    );
    assert!(
        body.blocks
            .iter()
            .filter(|block| matches!(block.terminator, MirTerminator::Return(_)))
            .count()
            <= 1,
        "both arms leave through the one shared exit, so `defer` still runs"
    );
}

fn contains_enum_construct(kind: &MirExpr) -> bool {
    match kind {
        MirExpr::EnumConstruct(_) => true,
        MirExpr::Assignment(assignment) => contains_enum_construct(&assignment.value.kind),
        _ => false,
    }
}
