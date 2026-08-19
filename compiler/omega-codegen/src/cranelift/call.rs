use super::Codegen;
use super::place::PlaceStorage;
use cranelift::codegen::ir::{FuncRef, Inst, StackSlot};
use cranelift::prelude::{
    AbiParam, FunctionBuilder, InstBuilder, StackSlotData, StackSlotKind, Value, types,
};
use cranelift_module::{FuncId, Module};
use omega_analyzer::layout;
use omega_analyzer::resolved_type::{NumericKind, ResolvedFunctionType, ResolvedType};

impl Codegen {
    pub(super) fn get_func_ref_from_id(
        &mut self,
        builder: &mut FunctionBuilder,
        func_id: FuncId,
    ) -> FuncRef {
        self.module.declare_func_in_func(func_id, builder.func)
    }

    pub(super) fn promote_variadic_arg(
        &mut self,
        builder: &mut FunctionBuilder,
        value: Value,
        arg_type: &ResolvedType,
    ) -> Value {
        // C variadic promotion is shared; this backend only translates the promoted type.
        match crate::abi::variadic_promotion(arg_type, self.target) {
            Some(NumericKind::Float(_)) => builder.ins().fpromote(types::F64, value),
            Some(NumericKind::Signed(_)) => builder.ins().sextend(types::I32, value),
            Some(NumericKind::Unsigned(_)) => builder.ins().uextend(types::I32, value),
            None => value,
        }
    }

    pub(super) fn maybe_sret_arg(
        &mut self,
        builder: &mut FunctionBuilder,
        fn_type: &ResolvedFunctionType,
        ir_args: &mut Vec<Value>,
    ) -> Option<StackSlot> {
        self.needs_sret(&fn_type.return_type).then(|| {
            let shift = layout::stack_align_shift(layout::type_alignment(&fn_type.return_type));
            let size = layout::total_bytes(&fn_type.return_type, self.pointer_bytes());
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                shift,
            ));
            let pointer = builder.ins().stack_addr(self.pointer_type(), slot, 0);
            ir_args.insert(0, pointer);
            slot
        })
    }

    pub(super) fn emit_call_indirect(
        &mut self,
        builder: &mut FunctionBuilder,
        fnaddr: Value,
        fn_type: &ResolvedFunctionType,
        ir_args: &[Value],
    ) -> Inst {
        // Cranelift variadic calls use a fixed signature synthesized for the concrete call site.
        let mut sig = self.make_function_sig(fn_type.clone());
        if fn_type.is_variadic && ir_args.len() > sig.params.len() {
            for arg in &ir_args[sig.params.len()..] {
                sig.params
                    .push(AbiParam::new(builder.func.dfg.value_type(*arg)));
            }
        }
        let sigref = builder.import_signature(sig);
        builder.ins().call_indirect(sigref, fnaddr, ir_args)
    }

    pub(super) fn call_result(
        &mut self,
        builder: &mut FunctionBuilder,
        fn_type: &ResolvedFunctionType,
        sret_slot: Option<StackSlot>,
        call: Inst,
    ) -> Vec<Value> {
        if *fn_type.return_type == ResolvedType::Void {
            return vec![];
        }
        match sret_slot {
            Some(slot) => {
                let storage = PlaceStorage::Slot { slot, offset: 0 };
                self.load_scalars(builder, &storage, &fn_type.return_type)
            }
            None => builder.inst_results(call).to_vec(),
        }
    }

}
