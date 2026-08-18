//! Declaring and defining every item across every compiled module -- the
//! two-pass sweep (`update_all`) that makes cross-module calls work
//! regardless of import direction, exactly like `cranelift/item.rs`.

use super::Codegen;
use inkwell::module::Linkage;
use omega_analyzer::checked::ExternFunctionRef;
use omega_mir::MirItem;
use omega_parser::prelude::Ident;

impl<'ctx> Codegen<'ctx> {
    /// Declares every function/method/extern/global in one item -- pass 1
    /// of 2.
    fn declare_item(&mut self, item: &MirItem, path: &[Ident]) {
        match item {
            MirItem::ExternDeclaration(extern_decl) => self.declare_extern_decl(extern_decl),
            MirItem::FunctionDefinition(f) => self.declare_function_def(f),
            MirItem::Struct(s) => {
                for f in &s.functions {
                    self.declare_function_def(f);
                }
            }
            MirItem::Enum(e) => {
                for f in &e.functions {
                    self.declare_function_def(f);
                }
            }
            MirItem::Union(u) => {
                for f in &u.functions {
                    self.declare_function_def(f);
                }
            }
            // A top-level global -- zero-initialized when `initial_value` is
            // `None`, else built from real bytes (see `build_const_blob`).
            // Strong linkage unconditionally (never a generic instantiation);
            // writable regardless of source-level `mut`, enforced at
            // analysis time rather than by object-file memory protection.
            MirItem::Declaration(decl) => {
                let symbol = omega_mir::mangle::global_symbol_string(path, &decl.ident);
                let total = omega_analyzer::layout::total_bytes(&decl.r#type, self.pointer_bytes());
                let blob = decl
                    .initial_value
                    .as_ref()
                    .map(|value| self.build_const_blob(value, &decl.r#type));

                // Declared with whatever type its own initializer has, not
                // unconditionally a byte array: a global's declared type and
                // initializer type must agree or the IR is invalid, and a
                // pointer-embedding initializer materializes as a packed
                // struct rather than bytes. Downstream access is unaffected
                // either way -- always through an opaque `ptr` and offset.
                let (r#type, initializer) = match blob {
                    None => {
                        let byte_array = self.context.i8_type().array_type(total.max(1));
                        (byte_array.into(), byte_array.const_zero().into())
                    }
                    Some(blob) => self.materialize_blob(&blob),
                };

                let global = self.module.add_global(r#type, None, &symbol);
                global.set_linkage(Linkage::External);
                global.set_alignment(omega_analyzer::layout::type_alignment(&decl.r#type));
                if self.target.os != omega_analyzer::Os::MacOs {
                    global.set_section(Some(&format!(".data.{symbol}")));
                }
                global.set_initializer(&initializer);
                self.globals.insert(decl.id, global);
            }
        }
    }

    /// Pass 2 of 2 -- define every body (everything else was fully handled
    /// by the declare pass).
    fn define_item(&mut self, item: MirItem) {
        match item {
            MirItem::ExternDeclaration(_) | MirItem::Declaration(_) => {}
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
        }
    }

    /// Two full passes over every item across every compiled module --
    /// declare everything first (so any function a cross-module call or a
    /// vtable needs already exists), then define every body.
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
