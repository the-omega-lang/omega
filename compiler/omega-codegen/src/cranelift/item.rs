//! Declaring and defining every item across every compiled module -- the
//! two-pass sweep (`update_all`) that makes cross-module calls work
//! regardless of import direction.

use super::Codegen;
use cranelift_module::{DataDescription, Linkage, Module};
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::layout;
use omega_mir::{MirItem, MirLinkage};
use omega_parser::prelude::Ident;

/// `MirLinkage`'s Cranelift counterpart -- the one remaining mapping from
/// a MIR-carried fact to a backend-native value. `Preemptible` maps to a
/// genuine weak ELF/Mach-O/COFF symbol (`cranelift-object`'s
/// `translate_linkage`, `let weak = linkage == Linkage::Preemptible`),
/// empirically confirmed to let a linker silently fold multiple
/// independently-compiled definitions of the *same* symbol name into one,
/// rather than erroring on "multiple definition" the way two strong
/// symbols with the same name always would. Every separate `omgc`
/// invocation that instantiates e.g. `CustomStruct<i32>` still fully
/// regenerates its own copy locally (nothing here skips that -- there is
/// no cross-process build cache), exactly like Rust/C++ generics: the
/// deduplication happens once, at final link time, not at compile time.
/// This is only sound because a generic instantiation's mangled symbol is
/// a pure function of `(module_path, name, type_args)` -- two independent
/// compilations of the exact same instantiation are therefore guaranteed
/// to produce byte-identical bodies under the identical name, which is the
/// actual precondition weak-symbol folding relies on (the linker trusts
/// the name, it doesn't diff the bytes). An ordinary, non-generic symbol
/// keeps strong linkage unconditionally -- two *different* object files
/// defining the same non-generic symbol is always a genuine user error,
/// and should still be a hard link error, not silently tolerated.
fn cranelift_linkage(linkage: MirLinkage) -> Linkage {
    match linkage {
        MirLinkage::Export => Linkage::Export,
        MirLinkage::Weak => Linkage::Preemptible,
    }
}

impl Codegen {
    /// Declares every function/method/extern in one item -- pass 1 of 2
    /// (see `update_all`).
    fn declare_item(&mut self, item: &MirItem, path: &[Ident]) {
        match item {
            // Externs have no body to split across two passes -- fully
            // handled here, in one shot.
            MirItem::ExternDeclaration(extern_decl) => self.update_extern_decl(extern_decl.clone()),
            MirItem::FunctionDefinition(f) => {
                // The symbol and linkage were decided once, at lowering
                // (`MirFunctionDef::symbol`/`linkage`) -- the mangling
                // dispatch that used to live here moved to
                // `omega_mir::lower` verbatim, so a second backend can
                // never disagree with this one about what a function is
                // called or how strongly it's defined.
                self.declare_function_def(f, f.symbol.clone(), cranelift_linkage(f.linkage));
            }
            MirItem::Struct(s) => {
                for f in &s.functions {
                    self.declare_function_def(f, f.symbol.clone(), cranelift_linkage(f.linkage));
                }
            }
            MirItem::Enum(e) => {
                for f in &e.functions {
                    self.declare_function_def(f, f.symbol.clone(), cranelift_linkage(f.linkage));
                }
            }
            MirItem::Union(u) => {
                for f in &u.functions {
                    self.declare_function_def(f, f.symbol.clone(), cranelift_linkage(f.linkage));
                }
            }
            // A top-level global (`ident: Type;`, `Storage::Global`) --
            // zero-initialized when `initial_value` is `None` (a plain
            // `ident : Type;`, or `mut`), or built from real bytes via
            // `write_const_element` when it's `Some` (a non-`comp`
            // `ident := comp expr();`, see `CheckedDeclaration::
            // initial_value`'s doc comment) -- the exact same call
            // `build_const_data` makes for an anonymous rodata blob,
            // just against this global's own real, named symbol instead.
            // `Export` (strong) linkage unconditionally: a global is never
            // a generic instantiation the way a function/method can be, so
            // there's no multi-definition-folding need for `Preemptible`
            // here (see `linkage_for`'s own doc comment for why that
            // distinction matters at all), and unlike `build_const_data`'s
            // blobs, this symbol must never be deduplicated by content --
            // two different globals that merely start out byte-identical
            // can still diverge after a `mut` write. `writable: true`
            // regardless of the source-level `mut`/plain distinction --
            // that's enforced at analysis time (only a `mut` binding's
            // `CheckedAssignment` ever exists in the checked tree at all),
            // not by object-file memory protection, exactly like a local's
            // stack slot is never protected either.
            MirItem::Declaration(decl) => {
                let symbol = omega_mir::mangle::global_symbol_string(path, &decl.ident);
                let total = layout::total_bytes(&decl.r#type, self.pointer_bytes());
                let data_id = self
                    .module
                    .declare_data(&symbol, Linkage::Export, true, false)
                    .unwrap();
                let mut desc = DataDescription::new();
                match &decl.initial_value {
                    None => desc.define_zeroinit(total as usize),
                    Some(value) => {
                        let mut bytes = vec![0u8; total as usize];
                        self.write_const_element(&mut desc, &mut bytes, 0, value, &decl.r#type);
                        desc.define(bytes.into_boxed_slice());
                    }
                }
                self.module.define_data(data_id, &desc).unwrap();
                self.globals.insert(decl.id, data_id);
            }
        }
    }

    /// Defines every function/method body in one item -- pass 2 of 2, run
    /// only after every item across every module has already been
    /// declared.
    fn define_item(&mut self, item: MirItem) {
        match item {
            // Already fully handled by `declare_item` -- an extern has no
            // body to define.
            MirItem::ExternDeclaration(_) => {}
            MirItem::FunctionDefinition(f) => self.define_function_def(f),
            MirItem::Struct(s) => {
                for f in s.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Enum(e) => {
                for f in e.functions {
                    self.define_function_def(f);
                }
            }
            MirItem::Union(u) => {
                for f in u.functions {
                    self.define_function_def(f);
                }
            }
            // Already fully handled by `declare_item` -- there's no
            // separate initializer body to define (see its own doc comment).
            MirItem::Declaration(_) => {}
        }
    }

    /// Two full passes over every item across every compiled module: first
    /// declare everything (so any `FuncId` a cross-module call needs
    /// already exists, regardless of import direction -- see
    /// `declare_function_def`'s doc comment), then define every body.
    /// Mirrors the identical signature/body split `omega_analyzer::
    /// Analyzer` does for the same underlying reason.
    pub(super) fn update_all(
        &mut self,
        modules: Vec<(Vec<Ident>, omega_mir::MirModule)>,
        extern_functions: Vec<ExternFunctionRef>,
    ) {
        for (path, module) in &modules {
            for item in &module.items {
                self.declare_item(item, path);
            }
        }
        for extern_fn in &extern_functions {
            self.declare_extern_function(extern_fn);
        }
        for (_, module) in modules {
            for item in module.items {
                self.define_item(item);
            }
        }
    }
}
