use cranelift::{
    codegen::{self, ir::FuncRef, packed_option::ReservedValue, verifier::VerifierErrors},
    prelude::{
        AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext, InstBuilder,
        Type as IRType, Value, isa, settings, types,
    },
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use omega_analyzer::{analysis::Analysis, resolved_type::ResolvedType};
use omega_parser::prelude::*;
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Clone)]
pub enum CodegenError {
    NotImplemented(NodeId),
    UnresolvedType(NodeId, Ident),
    VerifierErrors(NodeId, VerifierErrors),
}

pub struct Codegen {
    // Errors
    errors: Vec<CodegenError>,

    // State from previous steps
    analysis: Analysis,

    // Backend
    isa: Arc<dyn isa::TargetIsa>,
    pub module: ObjectModule,
    functions: HashMap<Ident, FuncId>,
    ctx: codegen::Context,
    fbctx: FunctionBuilderContext,

    // Global state
    counter: u64, // for unique things
    strings: HashMap<String, DataId>,

    // Local state (must be cleared per scope)
    local_functions: HashMap<Ident, FuncRef>,
    local_strings: HashMap<String, Value>,
}

trait IntoIRType {
    fn into_ir_type(self, codegen: &Codegen) -> IRType;
}

impl IntoIRType for ResolvedType {
    fn into_ir_type(self, codegen: &Codegen) -> IRType {
        match self {
            ResolvedType::Void => types::INVALID,
            ResolvedType::I32 => types::I32,
            ResolvedType::Char => types::I8,
            _ => codegen.module.target_config().pointer_type(),
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
        };

        codegen.update_all(source);

        codegen
    }

    fn clear_local(&mut self) {
        self.local_functions.clear();
        self.local_strings.clear();
        self.ctx.clear();
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
                let ir_params = resolved_fntype
                    .params
                    .into_iter()
                    .map(|param| param.1.into_ir_type(&self));

                let mut sig = self.module.make_signature();
                for param in ir_params {
                    sig.params.push(AbiParam::new(param));
                }

                if *resolved_fntype.return_type != ResolvedType::Void {
                    sig.returns.push(AbiParam::new(
                        resolved_fntype.return_type.into_ir_type(&self),
                    ));
                }

                let function_id = self
                    .module
                    .declare_function(&ident.0, Linkage::Import, &sig)
                    .unwrap();

                self.functions.insert(ident, function_id);

                Ok(())
            }

            other => Err(CodegenError::NotImplemented(node_id)),
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

    fn process_expr(
        &mut self,
        builder: &mut FunctionBuilder,
        node: ExpressionNode,
    ) -> Result<Value, CodegenError> {
        match node.expression {
            Expression::String(s) => {
                if let Some(local_value) = self.local_strings.get(&s.0) {
                    return Ok(local_value.to_owned());
                }

                let ptr_type = self.module.target_config().pointer_type();
                let str_id = if let Some(id) = self.strings.get(&s.0) {
                    id.to_owned()
                } else {
                    self.get_or_declare_global_string(s.0.clone())
                };

                let global_value = self.module.declare_data_in_func(str_id, &mut builder.func);
                let str_ptr = builder.ins().global_value(ptr_type, global_value);

                self.local_strings.insert(s.0, str_ptr.clone());

                Ok(str_ptr)
            }
            // Expression::Number(i) => Ok(builder.ins().iconst::<i64>(types::I32, (i as i64).into())),
            Expression::FunctionCall(FunctionCallExpr {
                function_name,
                args,
            }) => {
                let func_ref = if let Some(fnref) = self.local_functions.get(&function_name) {
                    fnref.to_owned()
                } else {
                    let global_id = self
                        .functions
                        .get(&function_name)
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

                let mut ir_args = vec![];
                for arg in args {
                    let value = self.process_expr(builder, arg)?;
                    ir_args.push(value);
                }

                let call = builder.ins().call(func_ref, &ir_args);

                // TODO: Handle function resolution at the scope level instead of global
                // let scope_ctx = &self.analysis.codeblock_scopes[&node.id];
                let fntype = self
                    .analysis
                    .get_global_function_type(&function_name)
                    .ok_or_else(|| CodegenError::UnresolvedType(node.id, function_name.clone()))?;

                if *fntype.return_type == ResolvedType::Void {
                    return Ok(Value::reserved_value());
                }

                Ok(builder.inst_results(call)[0])
            }

            _ => Err(CodegenError::NotImplemented(node.id)),
        }
    }

    fn process_statement(
        &mut self,
        builder: &mut FunctionBuilder,
        node: StatementNode,
    ) -> Result<(), CodegenError> {
        match node.statement {
            Statement::Expression(expr) => self.process_expr(builder, expr).map(|_| {
                println!("WARNING: Discarted value for node: {}", node.id);
                ()
            }),
            _ => Err(CodegenError::NotImplemented(node.id)),
        }
        // Statement::Return(expr) => {
        //     let value = self.process_expr(builder, expr);
        //     builder.ins().return_(&[value]);
        // }
    }

    fn update_function_def(
        &mut self,
        node_id: NodeId,
        function_def: FunctionDefinitionStmt,
    ) -> Result<(), Vec<CodegenError>> {
        let mut sig = self.module.make_signature();
        let ident = function_def.function_name;
        let fntype = self
            .analysis
            .get_global_function_type(&ident)
            .ok_or_else(|| vec![CodegenError::UnresolvedType(node_id, ident.clone())])?
            .to_owned();

        if *fntype.return_type != ResolvedType::Void {
            let return_type = fntype.return_type.clone().into_ir_type(&self);
            sig.returns.push(AbiParam::new(return_type.clone()));
        }

        let function_id = self
            .module
            .declare_function(ident.as_ref(), Linkage::Import, &sig)
            .unwrap();

        self.module
            .declare_function(ident.as_ref(), Linkage::Export, &sig)
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
        builder.switch_to_block(entry_block);
        // builder.seal_block(entry_block);

        let mut errors = vec![];
        for stmt in function_def.codeblock.0 {
            match self.process_statement(&mut builder, stmt) {
                Err(e) => errors.push(e),
                _ => {}
            }
        }
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
        self.functions.insert(ident, function_id);

        self.clear_local();

        Ok(())
    }

    fn update(&mut self, node: RootStatementNode) -> Result<(), Vec<CodegenError>> {
        match node.root_stmt {
            RootStatement::ExternDeclaration(extern_decl) => self
                .update_extern_decl(node.id, extern_decl)
                .map_err(|x| vec![x]),
            RootStatement::FunctionDefinition(fn_def) => self.update_function_def(node.id, fn_def),
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

    pub fn emit_object(self) -> Result<Vec<u8>, Vec<CodegenError>> {
        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        let product = self.module.finish();
        Ok(product.emit().unwrap())
    }
}
