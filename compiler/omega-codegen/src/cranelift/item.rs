use super::Codegen;
use cranelift_module::{DataDescription, Linkage, Module};
use omega_analyzer::checked::ExternFunctionRef;
use omega_analyzer::layout;
use omega_mir::{MirItem, MirLinkage};
use omega_parser::prelude::Ident;

fn cranelift_linkage(linkage: MirLinkage) -> Linkage {
    match linkage {
        MirLinkage::Export => Linkage::Export,
        MirLinkage::Weak => Linkage::Preemptible,
    }
}

impl Codegen {
    fn declare_item(&mut self, item: &MirItem, path: &[Ident]) {
        match item {
            // Externs are fully handled during declaration; there is no definition pass.
            MirItem::ExternDeclaration(extern_decl) => self.update_extern_decl(extern_decl.clone()),
            MirItem::FunctionDefinition(f) => {
                // Consume the MIR-provided symbol/linkage without backend-local renaming decisions.
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
            // Declare globals before materializing their initializer bytes.
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

    fn define_item(&mut self, item: MirItem) {
        match item {
            // This item has no definition-stage work.
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
            // This item has no definition-stage work.
            MirItem::Declaration(_) => {}
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
