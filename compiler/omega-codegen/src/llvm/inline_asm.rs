use super::Codegen;
use super::leaf;
use inkwell::InlineAsmDialect;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use omega_analyzer::Arch;
use omega_analyzer::layout::{self, Leaf};
use omega_mir::{MirAsmOperandKind, MirInlineAsm};
use std::collections::HashMap;

/// What a source `$name`/`$N` binding resolves to once operands have been
/// lowered: a `reg` becomes an LLVM template slot number, a `comp` becomes
/// its pre-rendered literal text (computed once by the analyzer).
enum Resolution {
    Reg(usize),
    CompText(String),
}

impl Resolution {
    fn text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Resolution::Reg(index) => std::borrow::Cow::Owned(format!("${index}")),
            Resolution::CompText(text) => std::borrow::Cow::Borrowed(text),
        }
    }
}

struct RegOperand<'ctx> {
    llvm_type: BasicTypeEnum<'ctx>,
    constraint_class: &'static str,
    physical: Option<String>,
    value: BasicValueEnum<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    /// Lowers one `asm(...) => { ... }` statement to a single side-effecting
    /// LLVM inline-asm indirect call. Every `reg` becomes an early-clobber
    /// read-write (`+&`) operand so LLVM never assumes the asm body leaves
    /// it unread, and its result is always discarded -- Omega's `reg` has no
    /// implicit writeback into the source expression.
    pub(super) fn process_inline_asm(&mut self, asm: &MirInlineAsm) {
        let pointer_bytes = self.pointer_bytes();
        let mut reg_operands: Vec<RegOperand<'ctx>> = Vec::new();
        let mut positional: Vec<Resolution> = Vec::with_capacity(asm.operands.len());
        let mut named: HashMap<String, Resolution> = HashMap::new();

        for operand in &asm.operands {
            let resolution = match &operand.kind {
                MirAsmOperandKind::Reg { value, physical } => {
                    let leaf = layout::leaves_of(&value.r#type, pointer_bytes)
                        .into_iter()
                        .next()
                        .expect(
                            "analysis already rejected 'reg' operands that don't occupy exactly one register",
                        );
                    let llvm_type = leaf::llvm_type(self.context, leaf, self.target);
                    let constraint_class = register_class(self.target.arch, leaf);
                    let llvm_value = self.process_expr(value)[0];
                    let index = reg_operands.len();
                    reg_operands.push(RegOperand {
                        llvm_type,
                        constraint_class,
                        physical: physical.clone(),
                        value: llvm_value,
                    });
                    Resolution::Reg(index)
                }
                MirAsmOperandKind::Comp { text } => Resolution::CompText(text.clone()),
            };
            if let Some(name) = &operand.binding_name {
                named.insert(name.as_ref().to_string(), clone_resolution(&resolution));
            }
            positional.push(resolution);
        }

        let mut constraints: Vec<String> = reg_operands
            .iter()
            .map(|reg| {
                let class = match &reg.physical {
                    Some(physical) => format!("{{{physical}}}"),
                    None => reg.constraint_class.to_string(),
                };
                format!("+&{class}")
            })
            .collect();
        constraints.extend(asm.clobbers.iter().map(|reg| format!("~{{{reg}}}")));
        constraints.extend(
            status_clobbers(self.target.arch)
                .iter()
                .map(|c| c.to_string()),
        );
        constraints.push("~{memory}".to_string());
        let constraints = constraints.join(",");

        let param_types: Vec<inkwell::types::BasicMetadataTypeEnum> =
            reg_operands.iter().map(|r| r.llvm_type.into()).collect();
        let fn_type = match reg_operands.as_slice() {
            [] => self.context.void_type().fn_type(&param_types, false),
            [single] => single.llvm_type.fn_type(&param_types, false),
            multiple => {
                let types: Vec<BasicTypeEnum> = multiple.iter().map(|r| r.llvm_type).collect();
                self.context
                    .struct_type(&types, false)
                    .fn_type(&param_types, false)
            }
        };

        // X86/X86-64 use LLVM's Intel dialect only; other targets use the
        // one dialect their LLVM backend defines. Omega exposes no per-arch
        // or per-statement dialect switch.
        let dialect = match self.target.arch {
            Arch::X86_64 | Arch::X86 => Some(InlineAsmDialect::Intel),
            _ => None,
        };
        let assembly = render_template(&asm.template, &positional, &named);
        let args: Vec<BasicValueEnum> = reg_operands.iter().map(|r| r.value).collect();
        let metadata_args: Vec<inkwell::values::BasicMetadataValueEnum> =
            args.iter().map(|v| (*v).into()).collect();

        let asm_ptr = self.context.create_inline_asm(
            fn_type,
            assembly,
            constraints,
            true,
            false,
            dialect,
            false,
        );
        self.builder
            .build_indirect_call(fn_type, asm_ptr, &metadata_args, "")
            .expect(
                "inline-asm IR construction always succeeds; assembler-level rejection \
                 (bad instructions/registers) can only surface at object/asm emission",
            );
    }
}

fn clone_resolution(resolution: &Resolution) -> Resolution {
    match resolution {
        Resolution::Reg(index) => Resolution::Reg(*index),
        Resolution::CompText(text) => Resolution::CompText(text.clone()),
    }
}

/// The generic LLVM register-class constraint letter for a scalar/pointer
/// leaf on the given target. Every leaf Omega's analyzer accepts for `reg`
/// (integers, pointers, `f32`/`f64`) has a legal single-register home on
/// each currently supported `Arch`, so this mapping is total.
fn register_class(arch: Arch, leaf: Leaf) -> &'static str {
    let is_float = matches!(leaf, Leaf::F32 | Leaf::F64);
    match (arch, is_float) {
        (Arch::X86_64 | Arch::X86, false) => "r",
        (Arch::X86_64 | Arch::X86, true) => "x",
        (Arch::Armv7 | Arch::Thumbv7em, false) => "r",
        (Arch::Armv7 | Arch::Thumbv7em, true) => "t",
        (Arch::Aarch64, false) => "r",
        (Arch::Aarch64, true) => "w",
        (Arch::Riscv32 | Arch::Riscv64, false) => "r",
        (Arch::Riscv32 | Arch::Riscv64, true) => "f",
        // AVR has no floating-point registers, so every operand class is `r`.
        (Arch::Avr, _) => "r",
    }
}

/// Conservative target status/flags clobbers appended alongside the
/// mandatory memory clobber -- every asm is treated as able to read/write
/// arbitrary memory and destroy condition flags, since Omega never inspects
/// the body to prove otherwise.
fn status_clobbers(arch: Arch) -> &'static [&'static str] {
    match arch {
        Arch::X86_64 | Arch::X86 => &["~{dirflag}", "~{fpsr}", "~{flags}"],
        Arch::Armv7
        | Arch::Thumbv7em
        | Arch::Aarch64
        | Arch::Riscv32
        | Arch::Riscv64
        | Arch::Avr => &[],
    }
}

/// Rewrites source `$name`/`$N` bindings into their LLVM template slot or
/// literal constant text. `$$` is left untouched -- LLVM's own inline-asm
/// template syntax defines `$$` as one literal `$`, so collapsing it here
/// would be a double-unescape. Anything else starting with `$` (a backend's
/// own `${...}` template syntax) is copied through unchanged, matching the
/// same recognition rule the analyzer used to validate these bindings.
fn render_template(
    body: &str,
    positional: &[Resolution],
    named: &HashMap<String, Resolution>,
) -> String {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0usize;
    let mut flushed = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        if body[i..].starts_with("$$") {
            i += 2;
            continue;
        }
        let rest = &body[i + 1..];
        if let Some(name_len) = ident_prefix_len(rest) {
            let name = &rest[..name_len];
            let resolution = named
                .get(name)
                .expect("analysis already validated every '$name' binding resolves");
            out.push_str(&body[flushed..i]);
            out.push_str(&resolution.text());
            i += 1 + name_len;
            flushed = i;
        } else if let Some(digit_len) = digit_prefix_len(rest) {
            let index: usize = rest[..digit_len]
                .parse()
                .expect("digit_prefix_len only matches ASCII digits");
            let resolution = positional
                .get(index)
                .expect("analysis already validated every '$N' binding is in range");
            out.push_str(&body[flushed..i]);
            out.push_str(&resolution.text());
            i += 1 + digit_len;
            flushed = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&body[flushed..]);
    out
}

fn ident_prefix_len(s: &str) -> Option<usize> {
    let mut len = 0;
    for c in s.chars() {
        if c.is_ascii_alphabetic() || c == '_' {
            len += c.len_utf8();
        } else if len > 0 && c.is_ascii_digit() {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    (len > 0).then_some(len)
}

fn digit_prefix_len(s: &str) -> Option<usize> {
    let len = s.bytes().take_while(u8::is_ascii_digit).count();
    (len > 0).then_some(len)
}
