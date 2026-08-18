use super::Codegen;
use inkwell::module::Linkage;
use omega_analyzer::checked::ExternFunctionRef;
use omega_mir::MirItem;
use omega_parser::prelude::Ident;

impl<'ctx> Codegen<'ctx> {
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
            // Globals are declared before initialization so references are order-independent.
            MirItem::Declaration(decl) => {
                let symbol = omega_mir::mangle::global_symbol_string(path, &decl.ident);
                let total = omega_analyzer::layout::total_bytes(&decl.r#type, self.pointer_bytes());
                let blob = decl
                    .initial_value
                    .as_ref()
                    .map(|value| self.build_const_blob(value, &decl.r#type));

                // Use the initializer value type here; semantic layout already guarantees compatibility.
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
