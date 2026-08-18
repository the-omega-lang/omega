
use super::Codegen;
use cranelift_module::{DataDescription, DataId, Linkage, Module};
use omega_analyzer::resolved_type::{ResolvedSpecType, ResolvedType};
use omega_hir::HirId;
use std::cell::RefCell;
use std::rc::Rc;

impl Codegen {
    pub(super) fn vtable_for(
        &mut self,
        concrete: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_type_args: &[ResolvedType],
        slots: &[HirId],
    ) -> DataId {
        let key = slots.to_vec();
        if let Some(&id) = self.vtables.get(&key) {
            return id;
        }

        let ptr_bytes = self.pointer_type().bytes();
        let bytes = vec![0u8; slots.len() * ptr_bytes as usize];
        let mut desc = DataDescription::new();
        for (i, decl_id) in slots.iter().enumerate() {
            let func_id = *self
                .functions
                .get(decl_id)
                .expect("every method is declared before any vtable needs it");
            let fref = self.module.declare_func_in_data(func_id, &mut desc);
            desc.write_function_addr(i as u32 * ptr_bytes, fref);
        }
        desc.define(bytes.into_boxed_slice());

        // Use weak/preemptible vtable data when independently compiled units may emit the same table.
        let symbol = omega_mir::mangle::encode(&omega_mir::mangle::vtable_symbol(
            concrete,
            &spec.borrow().name,
            spec_type_args,
        ));
        let data_id = self
            .module
            .declare_data(&symbol, Linkage::Preemptible, false, false)
            .unwrap();
        self.module.define_data(data_id, &desc).unwrap();

        self.vtables.insert(key, data_id);
        data_id
    }
}
