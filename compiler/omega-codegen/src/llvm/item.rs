use super::Codegen;
use crate::catalog::{Catalog, GlobalDecl};
use inkwell::module::Linkage;
use omega_mir::{EmissionUnit, MirDeclaration, MirItem};

impl<'ctx> Codegen<'ctx> {
    pub(super) fn emit_unit(&mut self, catalog: &Catalog, unit: EmissionUnit) {
        let owned_globals = Catalog::owned_globals(&unit);
        for function in &catalog.functions {
            self.declare_function_reference(function);
        }
        for global in &catalog.globals {
            if !owned_globals.contains(&global.id) {
                self.declare_global_reference(global);
            }
        }

        for item in &unit.items {
            self.configure_owned_item(item);
        }
        for item in unit.items {
            self.define_item(item);
        }
    }

    /// The owning source alone decides linkage, section, and storage. Every
    /// other object referring to the same symbol keeps the plain external
    /// declaration the catalog pass gave it.
    fn configure_owned_item(&mut self, item: &MirItem) {
        match item {
            // A foreign binding is a declaration in every object, including
            // the one whose source spells it: its storage lives elsewhere.
            MirItem::ForeignBinding(_) => {}
            MirItem::Declaration(declaration) => self.define_global(declaration),
            MirItem::ForeignFunction(f) => self.configure_foreign_function_owner(f),
            MirItem::FunctionDefinition(f) => self.configure_function_owner(f),
            MirItem::Struct(s) => {
                for f in &s.functions {
                    self.configure_function_owner(f);
                }
            }
            MirItem::Enum(e) => {
                for f in &e.functions {
                    self.configure_function_owner(f);
                }
            }
            MirItem::Union(u) => {
                for f in &u.functions {
                    self.configure_function_owner(f);
                }
            }
        }
    }

    fn define_item(&mut self, item: MirItem) {
        match item {
            MirItem::ForeignBinding(_) | MirItem::Declaration(_) => {}
            MirItem::ForeignFunction(f) => self.define_foreign_function_def(f),
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

    /// A data symbol this object does not own: correct type and alignment so
    /// uses type-check and relocate, but no storage, section, or initializer.
    fn declare_global_reference(&mut self, global: &GlobalDecl) {
        let total = omega_analyzer::layout::total_bytes(&global.r#type, self.pointer_bytes());
        let byte_array = self.context.i8_type().array_type(total.max(1));
        let value = self.module.add_global(byte_array, None, &global.symbol);
        value.set_linkage(Linkage::External);
        value.set_alignment(omega_analyzer::layout::type_alignment(&global.r#type));
        self.globals.insert(global.id, value);
    }

    fn define_global(&mut self, declaration: &MirDeclaration) {
        let symbol = &declaration.symbol;
        let total = omega_analyzer::layout::total_bytes(&declaration.r#type, self.pointer_bytes());
        let blob = declaration
            .initial_value
            .as_ref()
            .map(|value| self.build_const_blob(value, &declaration.r#type));

        // Use the initializer value type here; semantic layout already guarantees compatibility.
        let (r#type, initializer) = match blob {
            None => {
                let byte_array = self.context.i8_type().array_type(total.max(1));
                (byte_array.into(), byte_array.const_zero().into())
            }
            Some(blob) => self.materialize_blob(&blob),
        };

        let global = self.module.add_global(r#type, None, symbol);
        global.set_linkage(Linkage::External);
        global.set_alignment(omega_analyzer::layout::type_alignment(&declaration.r#type));
        if self.target.os != omega_analyzer::Os::MacOs {
            global.set_section(Some(&format!(".data.{symbol}")));
        }
        global.set_initializer(&initializer);
        self.globals.insert(declaration.id, global);
    }
}
