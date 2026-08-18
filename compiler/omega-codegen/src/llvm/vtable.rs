use super::Codegen;
use inkwell::module::Linkage;
use omega_analyzer::resolved_type::{ResolvedSpecType, ResolvedType};
use omega_hir::HirId;
use std::cell::RefCell;
use std::rc::Rc;

impl<'ctx> Codegen<'ctx> {
    pub(super) fn vtable_for(
        &mut self,
        concrete: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_type_args: &[ResolvedType],
        slots: &[HirId],
    ) -> inkwell::values::GlobalValue<'ctx> {
        let key = slots.to_vec();
        if let Some(&global) = self.vtables.get(&key) {
            return global;
        }

        // Vtable slots preserve the analyzer-resolved method order exactly.
        let fn_ptrs: Vec<inkwell::values::BasicValueEnum> = slots
            .iter()
            .map(|decl_id| {
                let function = *self
                    .functions
                    .get(decl_id)
                    .expect("every method is declared before any vtable needs it");
                function.as_global_value().as_pointer_value().into()
            })
            .collect();
        let array_type = self.ptr_type().array_type(slots.len() as u32);
        let init = self.ptr_type().const_array(
            &fn_ptrs
                .iter()
                .map(|v| v.into_pointer_value())
                .collect::<Vec<_>>(),
        );

        let symbol = omega_mir::mangle::encode(&omega_mir::mangle::vtable_symbol(
            concrete,
            &spec.borrow().name,
            spec_type_args,
        ));
        let global = self.module.add_global(array_type, None, &symbol);
        global.set_linkage(Linkage::WeakODR);
        global.set_initializer(&init);
        global.set_constant(true);
        global.set_alignment(self.pointer_bytes());
        if self.target.os != omega_analyzer::Os::MacOs {
            global.set_section(Some(&format!(".rodata.{symbol}")));
        }

        self.vtables.insert(key, global);
        global
    }
}
