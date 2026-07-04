use cranelift::{
    codegen::{
        self,
        ir::{FuncRef, StackSlot},
    },
    prelude::{
        AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext, InstBuilder, MemFlags,
        Signature, StackSlotData, StackSlotKind, Type as IRType, Value, isa, settings, types,
    },
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use omega_analyzer::{
    checked::{
        CheckedAssignment, CheckedExpr, CheckedExprNode, CheckedExternDecl, CheckedFunctionCall,
        CheckedFunctionDef, CheckedItem, CheckedModule, CheckedPlace, CheckedPlaceRoot,
        CheckedProjection, CheckedStmt, CheckedStructDef, Storage,
    },
    resolved_type::{ResolvedFunctionType, ResolvedStructType, ResolvedType},
};
use omega_hir::HirId;
use std::{collections::HashMap, sync::Arc};

/// Codegen never fails: everything it would otherwise need to reject was
/// already enforced while building the `CheckedModule` (place validity, type
/// compatibility, field/index existence, redeclaration). What remains here
/// are cases the language genuinely hasn't decided yet (array memory layout,
/// global data storage, ...) -- those `panic!`/`todo!()` rather than
/// returning an error, since there is no rejectable *user* input left by the
/// time codegen runs, only unimplemented compiler features.
pub struct Codegen {
    // Backend
    isa: Arc<dyn isa::TargetIsa>,
    pub module: ObjectModule,
    functions: HashMap<HirId, FuncId>,
    ctx: codegen::Context,

    // Global state
    counter: u64, // for unique things
    strings: HashMap<String, DataId>,

    // Local state (must be cleared per function)
    local_strings: HashMap<String, Value>,
    local_args: HashMap<HirId, Vec<Value>>,
    stack_slots: HashMap<HirId, Vec<(IRType, StackSlot)>>,
}

trait IntoIRType {
    fn into_ir_type(self, codegen: &Codegen) -> Vec<IRType>;
}

impl IntoIRType for ResolvedType {
    fn into_ir_type(self, codegen: &Codegen) -> Vec<IRType> {
        match self {
            ResolvedType::Void => vec![],
            ResolvedType::I32 => vec![types::I32],
            ResolvedType::Char => vec![types::I8],
            ResolvedType::Struct(struct_type) => struct_type
                .fields
                .into_iter()
                .flat_map(|x| x.1.into_ir_type(codegen))
                .collect(),
            _ => vec![codegen.pointer_type()],
        }
    }
}

/// Slices a `FieldAccess` projection's already-resolved `field_index` out of
/// an already-materialized value list (either stack-slot descriptors or SSA
/// argument values -- the two callers below are otherwise identical here,
/// which is why this is shared). No name search, no failure path: the
/// checked module already picked this exact index out of `struct_type`.
fn project_field_access<T: Clone>(
    codegen: &Codegen,
    values: &[T],
    struct_type: &ResolvedStructType,
    field_index: usize,
) -> Vec<T> {
    let start: usize = struct_type.fields[..field_index]
        .iter()
        .map(|(_, r#type)| r#type.clone().into_ir_type(codegen).len())
        .sum();
    let len = struct_type.fields[field_index].1.clone().into_ir_type(codegen).len();

    values[start..start + len].to_vec()
}

impl Codegen {
    pub fn generate(module_name: &str, isa: &str, checked: CheckedModule) -> Self {
        let isa = {
            let mut builder = settings::builder();

            builder.set("opt_level", "none").unwrap();
            builder.enable("is_pic").unwrap();

            let flags = settings::Flags::new(builder);

            isa::lookup_by_name(isa)
                .unwrap_or_else(|_| panic!("Invalid ISA: {}", isa))
                .finish(flags)
                .unwrap()
        };

        let module = {
            let translation_unit_name = module_name.bytes().collect::<Vec<_>>();
            let libcall_names = cranelift_module::default_libcall_names();
            let builder =
                ObjectBuilder::new(isa.clone(), translation_unit_name, libcall_names).unwrap();
            ObjectModule::new(builder)
        };

        let mut codegen = Self {
            isa,
            module,
            functions: HashMap::new(),
            ctx: codegen::Context::new(),

            counter: 0,
            strings: HashMap::new(),

            local_strings: HashMap::new(),
            stack_slots: HashMap::new(),
            local_args: HashMap::new(),
        };

        codegen.update_all(checked);

        codegen
    }

    fn clear_local(&mut self) {
        self.local_strings.clear();
        self.ctx.clear();
        self.stack_slots.clear();
        self.local_args.clear();
    }

    /// Resolves a place rooted in a stack-resident local (`Storage::Local`)
    /// down to its underlying stack-slot descriptors.
    fn get_place_from_stack(&mut self, place: &CheckedPlace) -> Vec<(IRType, StackSlot)> {
        let CheckedPlaceRoot::Variable { decl_id, r#type, .. } = &place.root else {
            todo!("place roots that aren't a bare variable (e.g. `foo().bar`) are not yet implemented");
        };

        let mut variable = self
            .stack_slots
            .get(decl_id)
            .unwrap_or_else(|| panic!("checked module guarantees {decl_id:?} was declared before this use"))
            .clone();
        let mut current_type = r#type.clone();

        for projection in &place.projections {
            match projection {
                CheckedProjection::FieldAccess { index, r#type, .. } => {
                    let ResolvedType::Struct(struct_type) = &current_type else {
                        unreachable!("checked module guarantees field projections are only built against a struct type");
                    };
                    variable = project_field_access(self, &variable, struct_type, *index);
                    current_type = r#type.clone();
                }

                CheckedProjection::Index { .. } => {
                    // Indexing through a stack-resident local requires
                    // deciding how array locals are laid out in memory
                    // (packed inline vs. a pointer to externally-allocated
                    // data) -- the language doesn't specify this yet
                    // (`ResolvedType::Array` doesn't even carry a length),
                    // so this is a real gap, not a bug.
                    todo!("stack-resident array indexing: array memory layout is not decided yet");
                }
            }
        }

        variable
    }

    /// Resolves a place rooted in a function parameter (`Storage::Parameter`)
    /// down to its underlying SSA value(s).
    fn get_place_from_args(&mut self, place: &CheckedPlace, builder: &mut FunctionBuilder) -> Vec<Value> {
        let CheckedPlaceRoot::Variable { decl_id, r#type, .. } = &place.root else {
            todo!("place roots that aren't a bare variable (e.g. `foo().bar`) are not yet implemented");
        };

        let mut values = self
            .local_args
            .get(decl_id)
            .unwrap_or_else(|| panic!("checked module guarantees {decl_id:?} was bound as a parameter before this use"))
            .clone();
        let mut current_type = r#type.clone();

        for projection in &place.projections {
            match projection {
                CheckedProjection::FieldAccess { index, r#type, .. } => {
                    let ResolvedType::Struct(struct_type) = &current_type else {
                        unreachable!("checked module guarantees field projections are only built against a struct type");
                    };
                    values = project_field_access(self, &values, struct_type, *index);
                    current_type = r#type.clone();
                }

                CheckedProjection::Index { index_expr, item_type } => {
                    let element_ir_type = current_type.clone().into_ir_type(self);
                    let element_ir_size: u32 = element_ir_type.iter().map(|x| x.bytes()).sum();

                    let mut base = values[0];
                    let mut index = self.process_expr(builder, (**index_expr).clone())[0];

                    let ptr_type = self.pointer_type();

                    if builder.func.dfg.value_type(base) != ptr_type {
                        base = builder.ins().uextend(ptr_type, base);
                    }
                    if builder.func.dfg.value_type(index) != ptr_type {
                        index = builder.ins().uextend(ptr_type, index);
                    }

                    let element_size = builder
                        .ins()
                        .iconst(self.pointer_type(), element_ir_size as i64);
                    let offset = builder.ins().imul(index, element_size);
                    let element_addr = builder.ins().iadd(base, offset);
                    let deref = element_ir_type
                        .into_iter()
                        .fold((vec![], 0u32), |mut acc, typ| {
                            let result = builder.ins().load(
                                typ,
                                MemFlags::new(),
                                element_addr,
                                acc.1 as i32,
                            );
                            acc.0.push(result);
                            acc.1 += typ.bytes();
                            acc
                        });

                    values = deref.0;
                    current_type = item_type.clone();
                }
            }
        }

        values
    }

    fn get_place_value(&mut self, place: &CheckedPlace, builder: &mut FunctionBuilder) -> Vec<Value> {
        let CheckedPlaceRoot::Variable { decl_id, storage, .. } = &place.root else {
            todo!("place roots that aren't a bare variable (e.g. `foo().bar`) are not yet implemented");
        };

        match storage {
            Storage::Local => {
                let slots = self.get_place_from_stack(place);
                slots
                    .iter()
                    .map(|slot| builder.ins().stack_load(slot.0, slot.1, 0))
                    .collect()
            }
            Storage::Parameter => self.get_place_from_args(place, builder),
            Storage::Function => {
                let function = *self.functions.get(decl_id).unwrap_or_else(|| {
                    panic!("checked module guarantees {decl_id:?} was declared as a function before this use")
                });
                let func = self.get_func_ref_from_id(builder, function);
                vec![builder.ins().func_addr(self.pointer_type(), func)]
            }
            Storage::Global => todo!("global/extern data storage is not yet implemented"),
        }
    }

    fn make_function_sig(&self, resolved_fntype: ResolvedFunctionType) -> Signature {
        let ir_params = resolved_fntype
            .params
            .into_iter()
            .flat_map(|param| param.1.into_ir_type(self));

        let mut sig = self.module.make_signature();
        for param in ir_params {
            sig.params.push(AbiParam::new(param));
        }

        if resolved_fntype.is_variadic {
            sig.call_conv = isa::CallConv::SystemV;
        }

        if *resolved_fntype.return_type != ResolvedType::Void {
            for param in resolved_fntype.return_type.into_ir_type(self) {
                sig.returns.push(AbiParam::new(param));
            }
        }

        sig
    }

    fn update_extern_decl(&mut self, extern_decl: CheckedExternDecl) {
        match extern_decl.r#type {
            ResolvedType::Function(resolved_fntype) => {
                let sig = self.make_function_sig(resolved_fntype);

                let function_id = self
                    .module
                    .declare_function(&extern_decl.ident.0, Linkage::Import, &sig)
                    .unwrap();

                self.functions.insert(extern_decl.id, function_id);
            }

            _ => todo!("extern data declarations (non-function externs) are not yet implemented"),
        }
    }

    fn unique_symbol(&mut self) -> String {
        let sym = format!("__sym_{}", self.counter);
        self.counter += 1;
        sym
    }

    fn get_or_declare_global_string(&mut self, s: String) -> DataId {
        let sym = self.unique_symbol();
        let id = self
            .module
            .declare_data(&sym, Linkage::Local, false, false)
            .unwrap();

        let mut str_desc = DataDescription::new();
        let mut str_bytes = s.clone().into_bytes();
        str_bytes.push(b'\0'); // null terminator
        str_desc.define(str_bytes.into_boxed_slice());
        self.module.define_data(id, &str_desc).unwrap();

        self.strings.insert(s, id);

        id
    }

    fn get_func_ref_from_id(&mut self, builder: &mut FunctionBuilder, func_id: FuncId) -> FuncRef {
        self.module.declare_func_in_func(func_id, builder.func)
    }

    fn process_expr(&mut self, builder: &mut FunctionBuilder, node: CheckedExprNode) -> Vec<Value> {
        match node.kind {
            CheckedExpr::String(s) => {
                if let Some(local_value) = self.local_strings.get(&s) {
                    return vec![*local_value];
                }

                let ptr_type = self.pointer_type();
                let str_id = if let Some(id) = self.strings.get(&s) {
                    *id
                } else {
                    self.get_or_declare_global_string(s.clone())
                };

                let global_value = self.module.declare_data_in_func(str_id, builder.func);
                let str_ptr = builder.ins().global_value(ptr_type, global_value);

                self.local_strings.insert(s, str_ptr);

                vec![str_ptr]
            }

            CheckedExpr::FunctionCall(CheckedFunctionCall { callee, fn_type, args }) => {
                // Checked module guarantees the callee resolves to exactly
                // one Function-typed value -- there is no way to construct a
                // Function-typed expression other than a `Storage::Function`
                // place root, which always yields a single address.
                let fnaddr = self.process_expr(builder, *callee)[0];

                let mut ir_args = vec![];
                for arg in args {
                    let value = self.process_expr(builder, arg);
                    ir_args.push(value);
                }
                let ir_args = ir_args.into_iter().flatten().collect::<Vec<_>>();

                let call = if fn_type.is_variadic {
                    // Cranelift does not currently support variadic functions.
                    // To bypass that, we previously set the call convention to SysV
                    // and we are now going to "cast" the function pointer according
                    // to which arguments are being passed after the pre-determined params.
                    let mut sig = self.make_function_sig(fn_type.clone());
                    if ir_args.len() > sig.params.len() {
                        for arg in &ir_args[sig.params.len()..] {
                            sig.params
                                .push(AbiParam::new(builder.func.dfg.value_type(*arg)))
                        }
                    }
                    let sigref = builder.import_signature(sig);
                    builder.ins().call_indirect(sigref, fnaddr, &ir_args)
                } else {
                    let sig = self.make_function_sig(fn_type.clone());
                    let sigref = builder.import_signature(sig);
                    builder.ins().call_indirect(sigref, fnaddr, &ir_args)
                };

                if *fn_type.return_type == ResolvedType::Void {
                    return vec![];
                }

                builder.inst_results(call).to_vec()
            }

            CheckedExpr::Number(value) => {
                vec![builder.ins().iconst::<i64>(types::I32, value as i64)]
            }

            CheckedExpr::Place(place) => self.get_place_value(&place, builder),

            CheckedExpr::Assignment(CheckedAssignment { target, value }) => {
                let values = self.process_expr(builder, *value);

                let CheckedPlaceRoot::Variable { storage, .. } = &target.root else {
                    todo!("assignment into a place rooted in a non-variable expression is not yet implemented");
                };

                match storage {
                    Storage::Local => {
                        let slots = self.get_place_from_stack(&target);
                        // Checked module guarantees `values` and `slots` have
                        // the same length: analysis only accepts an
                        // assignment whose value has the exact same resolved
                        // type as its target, and `into_ir_type` is a pure
                        // function of that type.
                        for (value, slot) in values.iter().zip(slots.iter()) {
                            builder.ins().stack_store(*value, slot.1, 0);
                        }
                    }
                    Storage::Parameter => {
                        todo!("assignment into a function parameter is not yet implemented");
                    }
                    Storage::Function | Storage::Global => {
                        unreachable!(
                            "checked module guarantees an assignment target resolves to a mutable variable, not a function or global"
                        );
                    }
                }

                values
            }

            CheckedExpr::Codeblock(_) => todo!("codeblock expressions are not yet implemented"),
        }
    }

    fn process_decl(&mut self, builder: &mut FunctionBuilder, decl: omega_analyzer::checked::CheckedDeclaration) {
        let ir_type = decl.r#type.into_ir_type(self);

        let stack_slots = ir_type
            .into_iter()
            .map(|typ| {
                (
                    typ,
                    builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        typ.bytes(),
                        16,
                    )),
                )
            })
            .collect::<Vec<_>>();

        self.stack_slots.insert(decl.id, stack_slots);
    }

    fn process_statement(&mut self, builder: &mut FunctionBuilder, stmt: CheckedStmt) {
        match stmt {
            CheckedStmt::Expression(expr) => {
                self.process_expr(builder, expr);
            }
            CheckedStmt::Return(expr) => {
                let retval = self.process_expr(builder, expr);
                builder.ins().return_(&retval);
            }
            CheckedStmt::Declaration(decl) => self.process_decl(builder, decl),
            CheckedStmt::Struct(_) => {} // Only analysis is necessary
            CheckedStmt::ExternDeclaration(_) => {
                todo!("extern declarations inside a function body are not yet implemented");
            }
        }
    }

    fn demangle(symbol: &str) -> String {
        if !symbol.contains("::") {
            return symbol.to_owned();
        }

        format!("___omg_{}", symbol.replace("::", "_"))
    }

    fn update_function_def(&mut self, function_def: CheckedFunctionDef, mangled_symbol: String) {
        let node_id = function_def.id;
        let fntype = function_def.fn_type();

        let mut sig = self.module.make_signature();
        if *fntype.return_type != ResolvedType::Void {
            let return_type = fntype.return_type.clone().into_ir_type(self);
            return_type
                .into_iter()
                .for_each(|param| sig.returns.push(AbiParam::new(param)));
        }

        // Add parameters to signature
        for param in &function_def.params {
            let ir_type = param.r#type.clone().into_ir_type(self);
            sig.params.push(AbiParam::new(ir_type[0])); // Simple types only for now. TODO: Fix.
        }

        let demangled_symbol = Self::demangle(&mangled_symbol);

        let function_id = self
            .module
            .declare_function(&demangled_symbol, Linkage::Import, &sig)
            .unwrap();

        self.module
            .declare_function(&demangled_symbol, Linkage::Export, &sig)
            .unwrap();

        // Registered as soon as it's declared (not after its body is fully
        // defined below) so a function can call itself recursively.
        self.functions.insert(node_id, function_id);

        // Move `ctx` out of `self` for the duration of the build so the rest of
        // this function can still freely borrow `self` (e.g. `into_ir_type(&self)`,
        // `self.process_statement(...)`) while `builder` holds onto it.
        let mut ctx = std::mem::replace(&mut self.ctx, codegen::Context::new());
        let mut fbctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        builder.func.signature = sig;

        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        let block_params = builder.block_params(entry_block).to_vec();

        // Some identifiers (e.g structs) have more than one value per identifier.
        // For that reason, lets make a helper array that repeats the param's own
        // id N times, where N is the amount of values it has.
        let argmap = function_def
            .params
            .iter()
            .flat_map(|param| {
                let value_count = param.r#type.clone().into_ir_type(self).len();
                vec![param.id; value_count]
            })
            .collect::<Vec<_>>();
        for (i, arg) in block_params.iter().enumerate() {
            self.local_args.entry(argmap[i]).or_default().push(*arg);
        }
        builder.switch_to_block(entry_block);

        // Process function body
        for stmt in function_def.body {
            self.process_statement(&mut builder, stmt);
        }

        if *fntype.return_type == ResolvedType::Void {
            builder.ins().return_(&[]);
        }

        if let Err(err) = codegen::verify_function(builder.func, self.isa.as_ref()) {
            panic!("cranelift verifier rejected generated IR for a function (internal codegen bug): {err:?}");
        }

        builder.seal_block(entry_block);
        builder.finalize();

        self.module.define_function(function_id, &mut ctx).unwrap();
        self.ctx = ctx;

        self.clear_local();
    }

    fn update_global_function_def(&mut self, function_def: CheckedFunctionDef) {
        let mangled_symbol = function_def.name.0.clone();
        self.update_function_def(function_def, mangled_symbol);
    }

    fn update_struct_def(&mut self, struct_def: CheckedStructDef) {
        for function in struct_def.functions {
            let mangled_symbol = format!("{}::{}", struct_def.name.as_ref(), function.name.as_ref());
            self.update_function_def(function, mangled_symbol);
        }
    }

    fn update(&mut self, item: CheckedItem) {
        match item {
            CheckedItem::ExternDeclaration(extern_decl) => self.update_extern_decl(extern_decl),
            CheckedItem::FunctionDefinition(fn_def) => self.update_global_function_def(fn_def),
            CheckedItem::Struct(struct_def) => self.update_struct_def(struct_def),
            CheckedItem::Declaration(_) => {
                todo!("global data declarations are not yet implemented");
            }
        }
    }

    fn update_all(&mut self, checked: CheckedModule) {
        for item in checked.items {
            self.update(item);
        }
    }

    pub fn pointer_type(&self) -> IRType {
        self.module.target_config().pointer_type()
    }

    pub fn emit_object(self) -> Vec<u8> {
        let product = self.module.finish();
        product.emit().unwrap()
    }
}
