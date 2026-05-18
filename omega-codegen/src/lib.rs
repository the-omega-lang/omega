use cranelift::{
    codegen::{
        self,
        ir::{FuncRef, StackSlot, function::DisplayFunction},
        packed_option::ReservedValue,
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
use omega_parser::{
    prelude::*,
    syntax::place::{Place, PlaceModifier, PlaceNode},
};
use std::{collections::HashMap, sync::Arc};

// TODO: Replace Strings with Places once the codebase is better structured
#[derive(Debug, Clone)]
pub enum CodegenError {
    NotImplemented(NodeId),
    UnresolvedType(NodeId, Ident),
    UnresolvedExpression(NodeId),
    InvalidNumber(NodeId),
    VerifierErrors(NodeId, VerifierErrors),
    UnresolvedScope(NodeId),
    Redeclaration(NodeId, Ident),
    Undeclared(NodeId, Place),
    TypeMismatch(NodeId),
    BadPlaceModifier(NodeId, PlaceModifier),
    NotAFunction(NodeId),
    BadExpression(NodeId),
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
    fbctx: FunctionBuilderContext,

    // Global state
    counter: u64, // for unique things
    strings: HashMap<String, DataId>,

    // Local state (must be cleared per scope)
    local_functions: HashMap<Ident, FuncRef>,
    local_strings: HashMap<String, Value>,
    codeblock_nodes: Vec<NodeId>,
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
                .map(|x| x.1.into_ir_type(codegen))
                .flatten()
                .collect(),
            _ => vec![codegen.pointer_type()],
        }
    }
}

impl Codegen {
    pub fn generate(
        module_name: &str,
        isa: &str,
        source: SourceModule,
        analysis: Analysis,
    ) -> Self {
        let isa = {
            let mut builder = settings::builder();

            builder.set("opt_level", "none").unwrap();
            builder.enable("is_pic").unwrap();

            let flags = settings::Flags::new(builder);

            isa::lookup_by_name(isa)
                .expect(&format!("Invalid ISA: {}", isa))
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
            fbctx: FunctionBuilderContext::new(),

            counter: 0,
            strings: HashMap::new(),

            local_functions: HashMap::new(),
            local_strings: HashMap::new(),
            codeblock_nodes: vec![],
            stack_slots: HashMap::new(),
            local_args: HashMap::new(),
        };

        codegen.update_all(source);

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
        place: &PlaceNode,
        builder: &mut FunctionBuilder,
    ) -> Result<&[(IRType, StackSlot)], CodegenError> {
        let id = place.id;
        let place = &place.place;

        let mut variable = self
            .stack_slots
            .get(place.0.as_ref())
            .ok_or_else(|| CodegenError::Undeclared(id, place.clone()))?
            .as_slice();

        let scope_id = self.codeblock_nodes.last().unwrap();
        let Some(scope) = self.analysis.get_codeblock_scope(scope_id) else {
            return Err(CodegenError::UnresolvedScope(id));
        };
        let mut current_type = scope
            .declared_variables
            .get(&place.0)
            .ok_or_else(|| CodegenError::UnresolvedType(id, place.0.clone()))?
            .clone();

        for modifier in &place.1 {
            match modifier {
                PlaceModifier::FieldAccess(field) => {
                    let ResolvedType::Struct(struct_type) = current_type else {
                        return Err(CodegenError::BadPlaceModifier(id, modifier.clone()));
                    };

                    let (index, accessed_field, ir_type) = struct_type
                        .fields
                        .iter()
                        .enumerate()
                        .find(|(_index, x)| &x.0 == field)
                        .map(|(index, x)| (index, x.clone(), x.1.clone().into_ir_type(&self)))
                        .ok_or_else(|| CodegenError::BadPlaceModifier(id, modifier.clone()))?;
                    current_type = accessed_field.1.clone();

                    variable = &variable[index..(index + ir_type.len())];
                }

                PlaceModifier::Index(expr) => {
                    todo!("IMPLEMENT");
                }
            }
        }

        Ok(variable)
    }

    // TODO: Reuse place code algorithm here in an abstracted manner.
    // TODO: Return result
    fn get_place_from_args(
        &mut self,
        place: &PlaceNode,
        builder: &mut FunctionBuilder,
    ) -> Result<Vec<Value>, CodegenError> {
        let id = place.id;
        let place = &place.place;

        let mut values = self
            .local_args
            .get(place.0.as_ref())
            .ok_or_else(|| CodegenError::Undeclared(id, place.clone()))?
            .to_vec();

        let scope_id = self.codeblock_nodes.last().unwrap();
        let Some(scope) = self.analysis.get_codeblock_scope(scope_id) else {
            return Err(CodegenError::UnresolvedScope(id));
        };
        let mut current_type = scope
            .declared_variables
            .get(&place.0)
            .ok_or_else(|| CodegenError::UnresolvedType(id, place.0.clone()))?
            .clone();

        for modifier in &place.1 {
            match modifier {
                PlaceModifier::FieldAccess(field) => {
                    let ResolvedType::Struct(struct_type) = current_type else {
                        return Err(CodegenError::BadPlaceModifier(id, modifier.clone()));
                    };

                    let (index, accessed_field, ir_type) = struct_type
                        .fields
                        .iter()
                        .enumerate()
                        .find(|(_index, x)| &x.0 == field)
                        .map(|(index, x)| (index, x.clone(), x.1.clone().into_ir_type(&self)))
                        .ok_or_else(|| CodegenError::BadPlaceModifier(id, modifier.clone()))?;
                    current_type = accessed_field.1.clone();

                    values = values[index..(index + ir_type.len())].to_vec();
                }

                PlaceModifier::Index(expr) => {
                    // let index_type = self
                    //     .analysis
                    //     .get_node_type(&expr.id)
                    //     .ok_or_else(|| CodegenError::UnresolvedExpression(expr.id))?
                    //     .to_owned();
                    // let element_ir_type = index_type.into_ir_type(&self);
                    let element_ir_type = current_type.clone().into_ir_type(&self);
                    let element_ir_size: u32 = element_ir_type.iter().map(|x| x.bytes()).sum();

                    let mut base = values[0];
                    let mut index = self.process_expr(builder, expr.clone())?[0];

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
        place_node: &PlaceNode,
        builder: &mut FunctionBuilder,
    ) -> Result<Vec<Value>, CodegenError> {
        let id = place_node.id;
        let place = &place_node.place;

        // TODO: Maybe reuse these in the actual functions.
        //       Maybe bring some of their code here since its mostly duplicated.
        let variable = self.stack_slots.get(place.0.as_ref());
        let values = self.local_args.get(place.0.as_ref());
        let function = self.functions.get(place.0.as_ref());

        match (variable, values, function) {
            (Some(_), None, None) => {
                let slots = self.get_place_from_stack(place_node, builder)?;
                Ok(slots
                    .iter()
                    .map(|slot| builder.ins().stack_load(slot.0.clone(), slot.1.clone(), 0))
                    .collect())
            }
            (None, Some(_), None) => self.get_place_from_args(place_node, builder),
            (None, None, Some(function)) => {
                // TODO: Implement properly.
                let func = self.get_func_ref_from_id(builder, function.clone());
                Ok(vec![builder.ins().func_addr(self.pointer_type(), func)])
            }
            _ => Err(CodegenError::Undeclared(id, place.clone())),
        }
    }

    fn make_function_sig(&self, resolved_fntype: ResolvedFunctionType) -> Signature {
        let ir_params = resolved_fntype
            .params
            .into_iter()
            .map(|param| param.1.into_ir_type(&self))
            .flatten();

        let mut sig = self.module.make_signature();
        for param in ir_params {
            sig.params.push(AbiParam::new(param));
        }

        if resolved_fntype.is_variadic {
            sig.call_conv = isa::CallConv::SystemV;
        }

        if *resolved_fntype.return_type != ResolvedType::Void {
            for param in resolved_fntype.return_type.into_ir_type(&self) {
                sig.returns.push(AbiParam::new(param));
            }
        }

        sig
    }

    fn update_extern_decl(
        &mut self,
        node_id: NodeId,
        extern_decl: ExternDeclarationStmt,
    ) -> Result<(), CodegenError> {
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

        self.strings.insert(s, id.clone());

        id
    }

    fn get_func_ref_from_name(
        &mut self,
        builder: &mut FunctionBuilder,
        function_name: &Ident,
    ) -> FuncRef {
        let func_ref = if let Some(fnref) = self.local_functions.get(function_name) {
            fnref.to_owned()
        } else {
            let global_id = self
                .functions
                .get(&function_name.0)
                .expect(&format!(
                    "Function not declared: {}",
                    function_name.as_ref()
                ))
                .to_owned();

            let fnref = self
                .module
                .declare_func_in_func(global_id, &mut builder.func);

            self.local_functions
                .insert(function_name.clone(), fnref.clone());

            fnref
        };

        func_ref
    }

    fn get_func_ref_from_id(&mut self, builder: &mut FunctionBuilder, func_id: FuncId) -> FuncRef {
        self.module.declare_func_in_func(func_id, &mut builder.func)
    }

    fn process_expr(
        &mut self,
        builder: &mut FunctionBuilder,
        node: ExpressionNode,
    ) -> Result<Vec<Value>, CodegenError> {
        match node.expression {
            Expression::String(s) => {
                if let Some(local_value) = self.local_strings.get(&s.0) {
                    return Ok(vec![local_value.to_owned()]);
                }

                let ptr_type = self.pointer_type();
                let str_id = if let Some(id) = self.strings.get(&s.0) {
                    id.to_owned()
                } else {
                    self.get_or_declare_global_string(s.0.clone())
                };

                let global_value = self.module.declare_data_in_func(str_id, &mut builder.func);
                let str_ptr = builder.ins().global_value(ptr_type, global_value);

                self.local_strings.insert(s.0, str_ptr.clone());

                Ok(vec![str_ptr])
            }
            // Expression::Number(i) => Ok(builder.ins().iconst::<i64>(types::I32, (i as i64).into())),
            Expression::FunctionCall(FunctionCallExpr { callee, args }) => {
                // let func_ref = self.get_func_ref_from_name(builder, &function_name);
                let Some(ResolvedType::Function(fntype)) = self.analysis.get_node_type(&callee.id)
                else {
                    return Err(CodegenError::NotAFunction(callee.id));
                };
                let fntype = fntype.clone();

                let Ok(func_addr) = self.process_expr(builder, *callee.clone()) else {
                    return Err(CodegenError::BadExpression(callee.id));
                };
                let Some(fnaddr) = func_addr.first() else {
                    return Err(CodegenError::NotAFunction(callee.id));
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
                                .push(AbiParam::new(builder.func.dfg.value_type(arg.clone())))
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

            Expression::Number(number_expr) => {
                let resolved_type = self
                    .analysis
                    .get_node_type(&node.id)
                    .ok_or_else(|| CodegenError::InvalidNumber(node.id))?;

                match resolved_type {
                    ResolvedType::I32 => {
                        let integer = &number_expr.integer_part.parse::<i32>().expect(&format!(
                            "Parser and analyzer claimed 'i32' for '{:?}'. It was not a valid 'i32'.",
                            number_expr
                        ));

                        Ok(vec![
                            builder.ins().iconst::<i64>(types::I32, *integer as i64),
                        ])
                    }

                    _ => Err(CodegenError::InvalidNumber(node.id)),
                }
            }

            Expression::Place(place) => self.get_place_value(&place, builder),

            Expression::Assignment(assignment) => {
                let values = self.process_expr(builder, *assignment.value)?;
                let slots = self.get_place_from_stack(&assignment.place, builder)?;

                if values.len() != slots.len() {
                    return Err(CodegenError::TypeMismatch(node.id));
                }

                for i in 0..values.len() {
                    let value = values[i];
                    let slot = slots[i].1;
                    let store = builder.ins().stack_store(value.clone(), slot, 0);
                }

                Ok(values)
            }

            // Expression::Index(index_expr) => {
            // let index_type = self
            //     .analysis
            //     .get_node_type(&index_expr.indexed.id)
            //     .ok_or_else(|| CodegenError::UnresolvedExpression(node.id))?
            //     .to_owned();
            // let element_ir_type = index_type.into_ir_type(&self);
            // let element_ir_size: u32 = element_ir_type.iter().map(|x| x.bytes()).sum();

            // let mut base = self.process_expr(builder, index_expr.indexed)?[0];
            // let mut index = self.process_expr(builder, index_expr.index)?[0];

            // let ptr_type = self.pointer_type();

            // if builder.func.dfg.value_type(base) != ptr_type {
            //     base = builder.ins().uextend(ptr_type, base);
            // }
            // if builder.func.dfg.value_type(index) != ptr_type {
            //     index = builder.ins().uextend(ptr_type, index);
            // }

            // let element_size = builder
            //     .ins()
            //     .iconst(self.pointer_type(), element_ir_size as i64);
            // let offset = builder.ins().imul(index, element_size);
            // let element_addr = builder.ins().iadd(base, offset);
            // let deref = element_ir_type
            //     .into_iter()
            //     .fold((vec![], 0u32), |mut acc, typ| {
            //         let result =
            //             builder
            //                 .ins()
            //                 .load(typ, MemFlags::new(), element_addr, acc.1 as i32);
            //         acc.0.push(result);
            //         acc.1 += typ.bytes();
            //         acc
            //     });

            // Ok(deref.0)
            // }
            _ => Err(CodegenError::NotImplemented(node.id)),
        }
    }

    fn process_decl(
        &mut self,
        node_id: NodeId,
        builder: &mut FunctionBuilder,
        decl: DeclarationStmt,
    ) -> Result<(), CodegenError> {
        if self.stack_slots.get(&decl.ident.to_string()).is_some() {
            return Err(CodegenError::Redeclaration(node_id, decl.ident));
        }

        let scope_id = self.codeblock_nodes.last().unwrap();
        let Some(scope) = self.analysis.get_codeblock_scope(scope_id) else {
            return Err(CodegenError::UnresolvedScope(node_id));
        };

        let Some(variable_type) = scope.declared_variables.get(&decl.ident) else {
            return Err(CodegenError::UnresolvedType(node_id, decl.ident));
        };

        let ir_type = variable_type.clone().into_ir_type(&self);

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
        node: StatementNode,
    ) -> Result<(), CodegenError> {
        match node.statement {
            Statement::Expression(expr) => self.process_expr(builder, expr).map(|_| {
                println!("WARNING: Discarted value for node: {}", node.id);
            }),
            Statement::Return(ret) => {
                let retval = self.process_expr(builder, ret.return_value)?;
                builder.ins().return_(&retval);
                Ok(())
            }
            Statement::Declaration(decl) => self.process_decl(node.id, builder, decl),
            Statement::Struct(_struct_def) => Ok(()), // Only analysis is necessary
            _ => Err(CodegenError::NotImplemented(node.id)),
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
        node_id: NodeId,
        function_def: FunctionDefinitionStmt,
        fntype: ResolvedFunctionType,
        mangled_symbol: String,
    ) -> Result<(), Vec<CodegenError>> {
        let mut sig = self.module.make_signature();
        if *fntype.return_type != ResolvedType::Void {
            let return_type = fntype.return_type.clone().into_ir_type(&self);
            return_type
                .into_iter()
                .for_each(|param| sig.returns.push(AbiParam::new(param)));
        }

        let scope = self
            .analysis
            .get_codeblock_scope(&node_id)
            .ok_or_else(|| vec![CodegenError::UnresolvedScope(node_id)])?;
        // let resolved_params = function_def
        //     .params
        //     .clone()
        //     .into_iter()
        //     .map(|param| {
        //         scope
        //             .declared_variables
        //             .get(&param.ident)
        //             .and_then(|resolved_type| Some(resolved_type.clone().into_ir_type(&self)))
        //             .ok_or(CodegenError::UnresolvedType(node_id, param.ident))
        //     })
        //     .collect::<Result<Vec<_>, CodegenError>>()
        //     .map_err(|e| vec![e])?;

        let resolved_params = fntype
            .params
            .clone()
            .into_iter()
            .map(|param| {
                scope
                    .declared_variables
                    .get(&param.0)
                    .and_then(|resolved_type| Some(resolved_type.clone().into_ir_type(&self)))
                    .ok_or(CodegenError::UnresolvedType(node_id, param.0))
            })
            .collect::<Result<Vec<_>, CodegenError>>()
            .map_err(|e| vec![e])?;

        println!("FUNCTION NAME: {:?}", function_def.function_name);
        println!("FUNCTION PARAMS: {:?}", resolved_params);

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

        // not sure how to bypass this issue of
        // double mutability as of now, other than this
        // forgive me.
        let ctx_func_ref = unsafe {
            let ptr = &mut self.ctx.func as *mut codegen::ir::Function;
            &mut *ptr
        };
        let fbctx_ref = unsafe {
            let ptr = &mut self.fbctx as *mut FunctionBuilderContext;
            &mut *ptr
        };

        let mut builder = FunctionBuilder::new(ctx_func_ref, fbctx_ref);
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
            .map(|param| {
                let value_count = param.1.clone().into_ir_type(&self).len();
                vec![&param.0].repeat(value_count)
            })
            .flatten()
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

        // Parse codeblock
        self.codeblock_nodes.push(node_id);
        for stmt in function_def.codeblock.0 {
            match self.process_statement(&mut builder, stmt) {
                Err(e) => errors.push(e),
                _ => {}
            }
        }
        self.codeblock_nodes.pop();

        if !errors.is_empty() {
            return Err(errors);
        }

        if *fntype.return_type == ResolvedType::Void {
            builder.ins().return_(&[]);
        }

        if let Err(err) = codegen::verify_function(&builder.func, self.isa.as_ref()) {
            return Err(vec![CodegenError::VerifierErrors(node_id, err)]);
        }

        builder.seal_block(entry_block);
        builder.finalize();

        self.module
            .define_function(function_id, &mut self.ctx)
            .unwrap();
        self.functions.insert(mangled_symbol, function_id);

        println!("FUNCTION: {}", self.ctx.func.display());

        self.clear_local();

        Ok(())
    }

    fn update_global_function_def(
        &mut self,
        node_id: NodeId,
        function_def: FunctionDefinitionStmt,
    ) -> Result<(), Vec<CodegenError>> {
        let ident = function_def.function_name.clone();
        let fntype = self
            .analysis
            .get_global_function_type(&ident)
            .ok_or_else(|| vec![CodegenError::UnresolvedType(node_id, ident.clone())])?
            .to_owned();

        self.update_function_def(node_id, function_def, fntype, ident.0)
    }

    fn update_struct_def(
        &mut self,
        node_id: NodeId,
        mut struct_def: StructStmt,
    ) -> Result<(), Vec<CodegenError>> {
        let mut errors = vec![];

        for (id, mut function) in struct_def.functions {
            let fntype = self
                .analysis
                .get_struct_function_type(node_id, &function.function_name)
                .ok_or_else(|| {
                    vec![CodegenError::UnresolvedType(
                        node_id,
                        function.function_name.clone(),
                    )]
                })?
                .to_owned();

            let mangled_symbol = format!(
                "{}::{}",
                struct_def.ident.as_ref(),
                function.function_name.as_ref()
            );
            match self.update_function_def(id, function, fntype, mangled_symbol) {
                Err(e) => errors.extend(e),
                _ => {}
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(())
    }

    fn update(&mut self, node: RootStatementNode) -> Result<(), Vec<CodegenError>> {
        match node.root_stmt {
            RootStatement::ExternDeclaration(extern_decl) => self
                .update_extern_decl(node.id, extern_decl)
                .map_err(|x| vec![x]),
            RootStatement::FunctionDefinition(fn_def) => {
                self.update_global_function_def(node.id, fn_def)
            }
            RootStatement::Struct(struct_def) => self.update_struct_def(node.id, struct_def), // Only analysis is necessary
            _ => Err(vec![CodegenError::NotImplemented(node.id)]),
        }
    }

    fn update_all(&mut self, source: SourceModule) {
        for node in source.nodes {
            match self.update(node) {
                Err(e) => self.errors.extend(e),
                _ => {}
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
