use cranelift::{
    codegen::{
        self,
        ir::{FuncRef, StackSlot},
        verifier::VerifierErrors,
    },
    prelude::{
        AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext, InstBuilder, MemFlags,
        Signature, StackSlotData, StackSlotKind, Type as IRType, Value, isa, settings, types,
    },
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use omega_analyzer::{
    analysis::Analysis,
    resolved_type::{ResolvedFunctionType, ResolvedType},
};
use omega_hir::{
    HirAssignment, HirExpr, HirExprNode, HirExternDeclaration, HirFunctionCall, HirFunctionDef,
    HirId, HirItem, HirModule, HirPlace, HirPlaceRoot, HirProjection, HirStmt, HirStructDef,
};
use omega_parser::prelude::{Ident, Type};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Clone)]
pub enum CodegenError {
    NotImplemented(HirId),
    UnresolvedType(HirId, Ident),
    UnresolvedExpression(HirId),
    InvalidNumber(HirId),
    VerifierErrors(HirId, VerifierErrors),
    UnresolvedScope(HirId),
    Redeclaration(HirId, Ident),
    Undeclared(HirId, Ident),
    TypeMismatch(HirId),
    BadFieldAccess(HirId, Ident),
    NotAFunction(HirId),
    BadExpression(HirId),
    NotAPlace(HirId),
}

pub struct Codegen {
    // Errors
    errors: Vec<CodegenError>,

    // State from previous steps
    analysis: Analysis,

    // Backend
    isa: Arc<dyn isa::TargetIsa>,
    pub module: ObjectModule,
    functions: HashMap<String, FuncId>,
    ctx: codegen::Context,

    // Global state
    counter: u64, // for unique things
    strings: HashMap<String, DataId>,

    // Local state (must be cleared per scope)
    local_functions: HashMap<Ident, FuncRef>,
    local_strings: HashMap<String, Value>,
    codeblock_nodes: Vec<HirId>,
    local_args: HashMap<String, Vec<Value>>,
    stack_slots: HashMap<String, Vec<(IRType, StackSlot)>>,
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

/// Walks a `FieldAccess` projection against an already-materialized value
/// list (either stack-slot descriptors or SSA argument values -- the two
/// callers below are otherwise identical here, which is why this is shared).
fn project_field_access<T: Clone>(
    codegen: &Codegen,
    node_id: HirId,
    values: &[T],
    current_type: ResolvedType,
    field: &Ident,
) -> Result<(Vec<T>, ResolvedType), CodegenError> {
    let ResolvedType::Struct(struct_type) = current_type else {
        return Err(CodegenError::BadFieldAccess(node_id, field.clone()));
    };

    let (index, accessed_field, ir_type) = struct_type
        .fields
        .iter()
        .enumerate()
        .find(|(_index, x)| &x.0 == field)
        .map(|(index, x)| (index, x.clone(), x.1.clone().into_ir_type(codegen)))
        .ok_or_else(|| CodegenError::BadFieldAccess(node_id, field.clone()))?;

    Ok((values[index..(index + ir_type.len())].to_vec(), accessed_field.1))
}

impl Codegen {
    pub fn generate(module_name: &str, isa: &str, hir: HirModule, analysis: Analysis) -> Self {
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
            errors: vec![],
            analysis,

            isa,
            module,
            functions: HashMap::new(),
            ctx: codegen::Context::new(),

            counter: 0,
            strings: HashMap::new(),

            local_functions: HashMap::new(),
            local_strings: HashMap::new(),
            codeblock_nodes: vec![],
            stack_slots: HashMap::new(),
            local_args: HashMap::new(),
        };

        codegen.update_all(hir);

        codegen
    }

    fn clear_local(&mut self) {
        self.local_functions.clear();
        self.local_strings.clear();
        self.ctx.clear();
        self.stack_slots.clear();
        self.local_args.clear();
    }

    fn get_place_from_stack(
        &mut self,
        node_id: HirId,
        place: &HirPlace,
    ) -> Result<Vec<(IRType, StackSlot)>, CodegenError> {
        let HirPlaceRoot::Ident(ident) = &place.root else {
            todo!("Non-ident place roots are not implemented");
        };

        let mut variable = self
            .stack_slots
            .get(ident.as_ref())
            .cloned()
            .ok_or_else(|| CodegenError::Undeclared(node_id, ident.clone()))?;

        let scope_id = self.codeblock_nodes.last().unwrap();
        let Some(scope) = self.analysis.get_codeblock_scope(scope_id) else {
            return Err(CodegenError::UnresolvedScope(node_id));
        };
        let mut current_type = scope
            .declared_variables
            .get(ident)
            .ok_or_else(|| CodegenError::UnresolvedType(node_id, ident.clone()))?
            .clone();

        for projection in &place.projections {
            match projection {
                HirProjection::FieldAccess(field) => {
                    let (sliced, next_type) =
                        project_field_access(self, node_id, &variable, current_type, field)?;
                    variable = sliced;
                    current_type = next_type;
                }

                HirProjection::Index(_) => {
                    // Indexing through a stack-resident local requires deciding
                    // how array locals are laid out in memory (packed inline vs.
                    // a pointer to externally-allocated data) -- the language
                    // doesn't specify this yet (`Type::Array` doesn't even carry
                    // a length), so this stays a graceful error rather than a
                    // guessed-at implementation.
                    return Err(CodegenError::NotImplemented(node_id));
                }
            }
        }

        Ok(variable)
    }

    fn get_place_from_args(
        &mut self,
        node_id: HirId,
        place: &HirPlace,
        builder: &mut FunctionBuilder,
    ) -> Result<Vec<Value>, CodegenError> {
        let HirPlaceRoot::Ident(ident) = &place.root else {
            todo!("Non-ident place roots are not implemented");
        };
        let ident = ident.clone();

        let mut values = self
            .local_args
            .get(ident.as_ref())
            .ok_or_else(|| CodegenError::Undeclared(node_id, ident.clone()))?
            .to_vec();

        let scope_id = self.codeblock_nodes.last().unwrap();
        let Some(scope) = self.analysis.get_codeblock_scope(scope_id) else {
            return Err(CodegenError::UnresolvedScope(node_id));
        };
        let mut current_type = scope
            .declared_variables
            .get(&ident)
            .ok_or_else(|| CodegenError::UnresolvedType(node_id, ident.clone()))?
            .clone();

        for projection in &place.projections {
            match projection {
                HirProjection::FieldAccess(field) => {
                    let (sliced, next_type) =
                        project_field_access(self, node_id, &values, current_type, field)?;
                    values = sliced;
                    current_type = next_type;
                }

                HirProjection::Index(index_expr) => {
                    let element_ir_type = current_type.clone().into_ir_type(self);
                    let element_ir_size: u32 = element_ir_type.iter().map(|x| x.bytes()).sum();

                    let mut base = values[0];
                    let mut index = self.process_expr(builder, (**index_expr).clone())?[0];

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
                }
            }
        }

        Ok(values)
    }

    fn get_place_value(
        &mut self,
        node_id: HirId,
        place: &HirPlace,
        builder: &mut FunctionBuilder,
    ) -> Result<Vec<Value>, CodegenError> {
        let HirPlaceRoot::Ident(ident) = &place.root else {
            todo!("Non-ident place roots are not implemented");
        };
        let ident = ident.clone();

        let variable = self.stack_slots.get(ident.as_ref());
        let values = self.local_args.get(ident.as_ref());
        let function = self.functions.get(ident.as_ref());

        match (variable, values, function) {
            (Some(_), None, None) => {
                let slots = self.get_place_from_stack(node_id, place)?;
                Ok(slots
                    .iter()
                    .map(|slot| builder.ins().stack_load(slot.0, slot.1, 0))
                    .collect())
            }
            (None, Some(_), None) => self.get_place_from_args(node_id, place, builder),
            (None, None, Some(function)) => {
                let function = *function;
                let func = self.get_func_ref_from_id(builder, function);
                Ok(vec![builder.ins().func_addr(self.pointer_type(), func)])
            }
            _ => Err(CodegenError::Undeclared(node_id, ident.clone())),
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

    fn update_extern_decl(&mut self, extern_decl: HirExternDeclaration) -> Result<(), CodegenError> {
        let node_id = extern_decl.id;
        let ident = extern_decl.ident;
        match extern_decl.r#type {
            Type::Function(_) => {
                let resolved_fntype = self
                    .analysis
                    .get_global_function_type(&ident)
                    .ok_or_else(|| CodegenError::UnresolvedType(node_id, ident.clone()))?
                    .to_owned();

                let sig = self.make_function_sig(resolved_fntype);

                let function_id = self
                    .module
                    .declare_function(&ident.0, Linkage::Import, &sig)
                    .unwrap();

                self.functions.insert(ident.0, function_id);

                Ok(())
            }

            _ => Err(CodegenError::NotImplemented(node_id)),
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

    fn process_expr(
        &mut self,
        builder: &mut FunctionBuilder,
        node: HirExprNode,
    ) -> Result<Vec<Value>, CodegenError> {
        let node_id = node.id;

        match node.expr {
            HirExpr::String(s) => {
                if let Some(local_value) = self.local_strings.get(&s.0) {
                    return Ok(vec![local_value.to_owned()]);
                }

                let ptr_type = self.pointer_type();
                let str_id = if let Some(id) = self.strings.get(&s.0) {
                    id.to_owned()
                } else {
                    self.get_or_declare_global_string(s.0.clone())
                };

                let global_value = self.module.declare_data_in_func(str_id, builder.func);
                let str_ptr = builder.ins().global_value(ptr_type, global_value);

                self.local_strings.insert(s.0, str_ptr);

                Ok(vec![str_ptr])
            }

            HirExpr::FunctionCall(HirFunctionCall { callee, args }) => {
                let callee_id = callee.id;
                let Some(ResolvedType::Function(fntype)) = self.analysis.get_node_type(&callee_id)
                else {
                    return Err(CodegenError::NotAFunction(callee_id));
                };
                let fntype = fntype.clone();

                let Ok(func_addr) = self.process_expr(builder, *callee) else {
                    return Err(CodegenError::BadExpression(callee_id));
                };
                let Some(fnaddr) = func_addr.first() else {
                    return Err(CodegenError::NotAFunction(callee_id));
                };

                let mut ir_args = vec![];
                for arg in args {
                    let value = self.process_expr(builder, arg)?;
                    ir_args.push(value);
                }
                let ir_args = ir_args.into_iter().flatten().collect::<Vec<_>>();

                let call = if fntype.is_variadic {
                    // Cranelift does not currently support variadic functions.
                    // To bypass that, we previously set the call convention to SysV
                    // and we are now going to "cast" the function pointer according
                    // to which arguments are being passed after the pre-determined params.
                    let mut sig = self.make_function_sig(fntype.clone());
                    if ir_args.len() > sig.params.len() {
                        for arg in &ir_args[sig.params.len()..] {
                            sig.params
                                .push(AbiParam::new(builder.func.dfg.value_type(*arg)))
                        }
                    }
                    let sigref = builder.import_signature(sig);
                    builder.ins().call_indirect(sigref, *fnaddr, &ir_args)
                } else {
                    let sig = self.make_function_sig(fntype.clone());
                    let sigref = builder.import_signature(sig);
                    builder.ins().call_indirect(sigref, *fnaddr, &ir_args)
                };

                if *fntype.return_type == ResolvedType::Void {
                    return Ok(vec![]);
                }

                Ok(builder.inst_results(call).to_vec())
            }

            HirExpr::Number(number_expr) => {
                let resolved_type = self
                    .analysis
                    .get_node_type(&node_id)
                    .ok_or(CodegenError::InvalidNumber(node_id))?;

                match resolved_type {
                    ResolvedType::I32 => {
                        let integer = &number_expr.integer_part.parse::<i32>().unwrap_or_else(|_| panic!("Parser and analyzer claimed 'i32' for '{:?}'. It was not a valid 'i32'.",
                            number_expr));

                        Ok(vec![
                            builder.ins().iconst::<i64>(types::I32, *integer as i64),
                        ])
                    }

                    _ => Err(CodegenError::InvalidNumber(node_id)),
                }
            }

            HirExpr::Place(place) => self.get_place_value(node_id, &place, builder),

            HirExpr::Assignment(HirAssignment { place, value }) => {
                let values = self.process_expr(builder, *value)?;

                let place_id = place.id;
                let HirExpr::Place(place_shape) = &place.expr else {
                    return Err(CodegenError::NotAPlace(place_id));
                };
                let slots = self.get_place_from_stack(place_id, place_shape)?;

                if values.len() != slots.len() {
                    return Err(CodegenError::TypeMismatch(node_id));
                }

                for i in 0..values.len() {
                    let value = values[i];
                    let slot = slots[i].1;
                    builder.ins().stack_store(value, slot, 0);
                }

                Ok(values)
            }

            HirExpr::Codeblock(_) => Err(CodegenError::NotImplemented(node_id)),
        }
    }

    fn process_decl(
        &mut self,
        builder: &mut FunctionBuilder,
        decl: omega_hir::HirDeclaration,
    ) -> Result<(), CodegenError> {
        if self.stack_slots.contains_key(&decl.ident.to_string()) {
            return Err(CodegenError::Redeclaration(decl.id, decl.ident));
        }

        let scope_id = self.codeblock_nodes.last().unwrap();
        let Some(scope) = self.analysis.get_codeblock_scope(scope_id) else {
            return Err(CodegenError::UnresolvedScope(decl.id));
        };

        let Some(variable_type) = scope.declared_variables.get(&decl.ident) else {
            return Err(CodegenError::UnresolvedType(decl.id, decl.ident));
        };

        let ir_type = variable_type.clone().into_ir_type(self);

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
        self.stack_slots
            .insert(decl.ident.to_string(), stack_slots.clone());

        Ok(())
    }

    fn process_statement(
        &mut self,
        builder: &mut FunctionBuilder,
        stmt: HirStmt,
    ) -> Result<(), CodegenError> {
        match stmt {
            HirStmt::Expression(expr) => self.process_expr(builder, expr).map(|_| ()),
            HirStmt::Return(expr) => {
                let retval = self.process_expr(builder, expr)?;
                builder.ins().return_(&retval);
                Ok(())
            }
            HirStmt::Declaration(decl) => self.process_decl(builder, decl),
            HirStmt::Struct(_) => Ok(()), // Only analysis is necessary
            HirStmt::ExternDeclaration(decl) => Err(CodegenError::NotImplemented(decl.id)),
        }
    }

    fn demangle(symbol: &str) -> String {
        if !symbol.contains("::") {
            return symbol.to_owned();
        }

        format!("___omg_{}", symbol.replace("::", "_"))
    }

    fn update_function_def(
        &mut self,
        function_def: HirFunctionDef,
        fntype: ResolvedFunctionType,
        mangled_symbol: String,
    ) -> Result<(), Vec<CodegenError>> {
        let node_id = function_def.id;
        let mut sig = self.module.make_signature();
        if *fntype.return_type != ResolvedType::Void {
            let return_type = fntype.return_type.clone().into_ir_type(self);
            return_type
                .into_iter()
                .for_each(|param| sig.returns.push(AbiParam::new(param)));
        }

        let scope = self
            .analysis
            .get_codeblock_scope(&node_id)
            .ok_or_else(|| vec![CodegenError::UnresolvedScope(node_id)])?;

        let resolved_params = fntype
            .params
            .clone()
            .into_iter()
            .map(|param| {
                scope
                    .declared_variables
                    .get(&param.0).map(|resolved_type| resolved_type.clone().into_ir_type(self))
                    .ok_or(CodegenError::UnresolvedType(node_id, param.0))
            })
            .collect::<Result<Vec<_>, CodegenError>>()
            .map_err(|e| vec![e])?;

        // Add parameters to signature
        for param in resolved_params {
            sig.params.push(AbiParam::new(param[0])) // Simple types only for now. TODO: Fix.
        }

        let demangled_symbol = Self::demangle(&mangled_symbol);

        let function_id = self
            .module
            .declare_function(&demangled_symbol, Linkage::Import, &sig)
            .unwrap();

        self.module
            .declare_function(&demangled_symbol, Linkage::Export, &sig)
            .unwrap();

        // Move `ctx` out of `self` for the duration of the build so the rest of
        // this function can still freely borrow `self` (e.g. `into_ir_type(&self)`,
        // `self.process_statement(...)`) while `builder` holds onto it.
        let mut ctx = std::mem::replace(&mut self.ctx, codegen::Context::new());
        let mut fbctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        builder.func.signature = sig;

        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        let block_params = builder.block_params(entry_block);

        // Some identifiers (e.g structs) have more than one value per identifier.
        // For that reason, lets make a helper array that repeats the identifier
        // N times, where N is the amount of values it has.
        let argmap = fntype
            .params
            .iter()
            .flat_map(|param| {
                let value_count = param.1.clone().into_ir_type(self).len();
                [&param.0].repeat(value_count)
            })
            .collect::<Vec<_>>();
        for i in 0..block_params.len() {
            let ident = argmap[i];
            let arg = block_params[i];
            if let Some(entry) = self.local_args.get_mut(&ident.to_string()) {
                entry.push(arg);
                continue;
            }

            self.local_args.insert(ident.to_string(), vec![arg]);
        }
        builder.switch_to_block(entry_block);

        let mut errors = vec![];

        // Process function body
        self.codeblock_nodes.push(node_id);
        for stmt in function_def.body {
            if let Err(e) = self.process_statement(&mut builder, stmt) {
                errors.push(e);
            }
        }
        self.codeblock_nodes.pop();

        if !errors.is_empty() {
            self.ctx = ctx;
            return Err(errors);
        }

        if *fntype.return_type == ResolvedType::Void {
            builder.ins().return_(&[]);
        }

        if let Err(err) = codegen::verify_function(builder.func, self.isa.as_ref()) {
            self.ctx = ctx;
            return Err(vec![CodegenError::VerifierErrors(node_id, err)]);
        }

        builder.seal_block(entry_block);
        builder.finalize();

        self.module.define_function(function_id, &mut ctx).unwrap();
        self.ctx = ctx;
        self.functions.insert(mangled_symbol, function_id);

        self.clear_local();

        Ok(())
    }

    fn update_global_function_def(
        &mut self,
        function_def: HirFunctionDef,
    ) -> Result<(), Vec<CodegenError>> {
        let node_id = function_def.id;
        let ident = function_def.name.clone();
        let fntype = self
            .analysis
            .get_global_function_type(&ident)
            .ok_or_else(|| vec![CodegenError::UnresolvedType(node_id, ident.clone())])?
            .to_owned();

        self.update_function_def(function_def, fntype, ident.0)
    }

    fn update_struct_def(&mut self, struct_def: HirStructDef) -> Result<(), Vec<CodegenError>> {
        let node_id = struct_def.id;
        let mut errors = vec![];

        for function in struct_def.functions {
            let fntype = self
                .analysis
                .get_struct_function_type(node_id, &function.name)
                .ok_or_else(|| vec![CodegenError::UnresolvedType(node_id, function.name.clone())])?
                .to_owned();

            let mangled_symbol = format!("{}::{}", struct_def.name.as_ref(), function.name.as_ref());
            if let Err(e) = self.update_function_def(function, fntype, mangled_symbol) {
                errors.extend(e);
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(())
    }

    fn update(&mut self, item: HirItem) -> Result<(), Vec<CodegenError>> {
        match item {
            HirItem::ExternDeclaration(extern_decl) => {
                self.update_extern_decl(extern_decl).map_err(|x| vec![x])
            }
            HirItem::FunctionDefinition(fn_def) => self.update_global_function_def(fn_def),
            HirItem::Struct(struct_def) => self.update_struct_def(struct_def),
            HirItem::Declaration(decl) => Err(vec![CodegenError::NotImplemented(decl.id)]),
        }
    }

    fn update_all(&mut self, hir: HirModule) {
        for item in hir.items {
            if let Err(e) = self.update(item) {
                self.errors.extend(e);
            }
        }
    }

    pub fn pointer_type(&self) -> IRType {
        self.module.target_config().pointer_type()
    }

    pub fn emit_object(self) -> Result<Vec<u8>, Vec<CodegenError>> {
        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        let product = self.module.finish();
        Ok(product.emit().unwrap())
    }
}
