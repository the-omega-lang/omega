use omega_analyzer::Target;
use omega_analyzer::error::AnalysisErrorKind;
use omega_analyzer::error::AnalysisWarningKind;
use omega_driver::{CompileError, Driver, ExternRoot};
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
            "omega_naked_fn_test_{}_{}",
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

    fn expect_errors(&self) -> Vec<CompileError> {
        match self.compile() {
            Ok(_) => panic!("expected this to be rejected, but it compiled"),
            Err(errors) => errors,
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

fn has_analysis_error(
    errors: &[CompileError],
    predicate: impl Fn(&AnalysisErrorKind) -> bool,
) -> bool {
    errors.iter().any(|error| match error {
        CompileError::Analysis { errors, .. } => errors.iter().any(|error| predicate(&error.kind)),
        _ => false,
    })
}

fn mir_functions(program: omega_driver::CompiledProgram) -> Vec<omega_mir::MirFunctionDef> {
    let entry = program.entry.clone();
    let mir = omega_mir::lower_program(program.modules, &entry);
    mir.into_iter()
        .flat_map(|(_, module)| module.items)
        .filter_map(|item| match item {
            omega_mir::MirItem::FunctionDefinition(f) => Some(f),
            _ => None,
        })
        .collect()
}

// -- Acceptance --

#[test]
fn zero_arg_naked_accepted_without_omega_return() {
    TestPackage::new(
        "@naked\n\
         get_magic() => i32 {\n\
             asm() => {\n\
                 mov eax, 123\n\
                 ret\n\
             }\n\
         }",
    )
    .expect_ok();
}

#[test]
fn naked_with_explicit_empty_parens_accepted() {
    TestPackage::new(
        "@naked()\n\
         get_magic() => i32 {\n\
             asm() => {\n\
                 mov eax, 123\n\
                 ret\n\
             }\n\
         }",
    )
    .expect_ok();
}

#[test]
fn naked_comp_and_clobber_descriptors_accepted() {
    TestPackage::new(
        "comp MAGIC := 123i32;\n\
         @naked\n\
         get_magic() => i32 {\n\
             asm(comp(MAGIC), clobber(\"rax\")) => {\n\
                 mov eax, $MAGIC\n\
                 ret\n\
             }\n\
         }",
    )
    .expect_ok();
}

#[test]
fn naked_method_with_mut_self_receiver_validates_as_one_asm_body() {
    TestPackage::new(
        "exposed struct Foo {\n\
             exposed x: i32;\n\
             @naked\n\
             exposed magic(mut self) => i32 {\n\
                 asm() => {\n\
                     mov eax, 123\n\
                     ret\n\
                 }\n\
             }\n\
         }",
    )
    .expect_ok();
}

#[test]
fn naked_params_are_kept_in_signature_but_produce_no_unused_warning() {
    let program = TestPackage::new(
        "comp MAGIC := 123i32;\n\
         @naked\n\
         get_magic(x: i32) => i32 {\n\
             asm(comp(MAGIC)) => {\n\
                 mov eax, $MAGIC\n\
                 ret\n\
             }\n\
         }",
    )
    .expect_ok();

    assert!(
        !program.warnings.iter().any(|(_, warning)| matches!(
            warning.kind,
            AnalysisWarningKind::UnusedParameter { .. }
        )),
        "a naked function's ABI-only parameter must not be warned as unused"
    );

    let functions = mir_functions(program);
    let get_magic = functions
        .iter()
        .find(|f| f.name.as_ref() == "get_magic")
        .expect("the naked function is present");
    assert_eq!(get_magic.params.len(), 1);
    assert_eq!(get_magic.params[0].ident.as_ref(), "x");
}

// -- Structural MIR shape --

#[test]
fn naked_function_lowers_to_naked_mir_body() {
    let program = TestPackage::new(
        "@naked\n\
         get_magic() => i32 {\n\
             asm() => {\n\
                 mov eax, 123\n\
                 ret\n\
             }\n\
         }\n\
         ordinary() => i32 { 1 }",
    )
    .expect_ok();

    let functions = mir_functions(program);
    let get_magic = functions
        .iter()
        .find(|f| f.name.as_ref() == "get_magic")
        .expect("the naked function is present");
    assert!(
        matches!(get_magic.body, omega_mir::MirFunctionBody::Naked(_)),
        "a naked function must lower to MirFunctionBody::Naked, not a normal frame body"
    );

    let ordinary = functions
        .iter()
        .find(|f| f.name.as_ref() == "ordinary")
        .expect("the ordinary function is present");
    assert!(matches!(
        ordinary.body,
        omega_mir::MirFunctionBody::Normal(_)
    ));
}

// -- Rejections --

#[test]
fn naked_reg_descriptor_is_rejected() {
    let source = "@naked\n\
         bad(x: i32) => void {\n\
             asm(reg(x)) => { nop }\n\
         }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmRegInNakedFunction)
    ));
}

#[test]
fn naked_unbound_param_reference_is_rejected() {
    // Proves parameters are ABI-only: with 'reg' forbidden, nothing can ever
    // bind '$x' inside a naked asm, so an unqualified reference to it is
    // just unknown text, not an implicit parameter binding.
    let source = "@naked\n\
         add_one(x: i32) => i32 {\n\
             asm() => {\n\
                 mov eax, $x\n\
                 ret\n\
             }\n\
         }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AsmUnknownBinding { text } if text == "$x")
    ));
}

#[test]
fn naked_empty_body_is_rejected() {
    let source = "@naked\nbad() => void {\n}";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::InvalidNakedBody)
    ));
}

#[test]
fn naked_multiple_statements_is_rejected() {
    let source = "@naked\n\
         bad() => void {\n\
             asm() => { nop }\n\
             asm() => { nop }\n\
         }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::InvalidNakedBody)
    ));
}

#[test]
fn naked_non_asm_statement_is_rejected() {
    let source = "@naked\nbad() => void {\n    x := 1i32;\n}";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::InvalidNakedBody)
    ));
}

#[test]
fn naked_tail_expression_is_rejected() {
    let source = "@naked\n\
         bad() => i32 {\n\
             asm() => { nop }\n\
             123\n\
         }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::InvalidNakedBody)
    ));
}

#[test]
fn naked_with_arguments_is_rejected() {
    let source = "@naked(foo)\nbad() => void {\n    asm() => { nop }\n}";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::InvalidAnnotationArgs { name, .. } if name.as_ref() == "naked")
    ));
}

#[test]
fn naked_and_inline_conflict_is_rejected() {
    let source = "@naked\n@inline\nbad() => void {\n    asm() => { nop }\n}";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::NakedInlineConflict)
    ));
}

#[test]
fn inline_and_naked_conflict_is_rejected_regardless_of_order() {
    let source = "@inline\n@naked\nbad() => void {\n    asm() => { nop }\n}";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::NakedInlineConflict)
    ));
}

#[test]
fn naked_on_a_non_function_item_is_rejected() {
    let source = "@naked\nexposed struct Bad { exposed x: i32; }";
    assert!(has_analysis_error(
        &TestPackage::new(source).expect_errors(),
        |kind| matches!(kind, AnalysisErrorKind::AnnotationNotApplicable { name, .. } if name.as_ref() == "naked")
    ));
}

// -- Codegen shape --

fn codegen_ir(
    program: omega_driver::CompiledProgram,
    opt_level: omega_codegen::OptLevel,
) -> String {
    let extern_functions = program.extern_functions.clone();
    let entry = program.entry.clone();
    let modules = omega_mir::lower_program(program.modules, &entry);
    let request = omega_codegen::CodegenRequest {
        module_name: "main".to_string(),
        target: Target::DEFAULT,
        opt_level,
        emit: omega_codegen::EmitKind::Ir,
        modules,
        entry,
        extern_functions,
    };
    match omega_codegen::generate(request).expect("codegen succeeds") {
        omega_codegen::EmitOutput::Text(text) => text,
        omega_codegen::EmitOutput::Object(_) => unreachable!("EmitKind::Ir always emits text"),
    }
}

#[test]
fn naked_function_emits_naked_noinline_and_unreachable_with_no_frame_setup() {
    let program = TestPackage::new(
        "@naked\n\
         get_magic() => i32 {\n\
             asm() => {\n\
                 mov eax, 123\n\
                 ret\n\
             }\n\
         }",
    )
    .expect_ok();
    let ir = codegen_ir(program, omega_codegen::OptLevel::O0);

    assert!(ir.contains(" naked "), "missing 'naked' attribute:\n{ir}");
    assert!(
        ir.contains(" noinline "),
        "missing 'noinline' attribute:\n{ir}"
    );
    assert!(
        ir.contains("unreachable"),
        "naked function body must end in 'unreachable':\n{ir}"
    );
    assert!(
        !ir.contains("alloca"),
        "a naked function body must never allocate a local frame:\n{ir}"
    );
    assert!(
        !ir.contains("ret i32") && !ir.contains("ret void"),
        "a naked function must not get an ordinary LLVM 'ret', only 'unreachable':\n{ir}"
    );
}

#[test]
fn naked_function_x86_64_asm_has_no_compiler_generated_frame_setup() {
    for opt_level in [omega_codegen::OptLevel::O0, omega_codegen::OptLevel::O3] {
        let program = TestPackage::new(
            "@naked\n\
             get_magic() => i32 {\n\
                 asm() => {\n\
                     mov eax, 123\n\
                     ret\n\
                 }\n\
             }",
        )
        .expect_ok();
        let extern_functions = program.extern_functions.clone();
        let entry = program.entry.clone();
        let modules = omega_mir::lower_program(program.modules, &entry);
        let request = omega_codegen::CodegenRequest {
            module_name: "main".to_string(),
            target: Target::DEFAULT,
            opt_level,
            emit: omega_codegen::EmitKind::Asm,
            modules,
            entry,
            extern_functions,
        };
        let asm = match omega_codegen::generate(request).expect("codegen succeeds") {
            omega_codegen::EmitOutput::Text(text) => text,
            omega_codegen::EmitOutput::Object(_) => unreachable!("EmitKind::Asm always emits text"),
        };

        // This package defines exactly one function, so a whole-module scan
        // for compiler-generated frame setup is unambiguous.
        assert!(
            asm.contains("123"),
            "expected the naked body's literal to survive emission at {opt_level:?}:\n{asm}"
        );
        assert!(
            !asm.lines()
                .any(|line| line.trim_start().starts_with("push")),
            "no compiler-generated 'push' is expected around a naked body at {opt_level:?}:\n{asm}"
        );
        assert!(
            !asm.lines().any(|line| line.trim_start().starts_with("pop")),
            "no compiler-generated 'pop' is expected around a naked body at {opt_level:?}:\n{asm}"
        );
        assert!(
            !asm.contains("sub") || !asm.contains("rsp"),
            "no compiler-generated stack adjustment is expected around a naked body at {opt_level:?}:\n{asm}"
        );
    }
}
