use cranelift::{
    codegen::{self, ir::FuncRef},
    prelude::{
        AbiParam, Configurable, FunctionBuilderContext, Type as IRType, Value, isa, settings, types,
    },
};
use cranelift_module::{DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use omega_analyzer::{analysis::Analysis, resolved_type::ResolvedType};
use omega_parser::prelude::*;
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Clone)]
pub enum CodegenError {
    NotImplemented(NodeId),
    UnresolvedType(NodeId, Ident),
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

                sig.returns.push(AbiParam::new(
                    resolved_fntype.return_type.into_ir_type(&self),
                ));

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

    // fn unique_symbol(&mut self) -> String {
    //     let sym = format!("__sym_{}", self.counter);
    //     self.counter += 1;
    //     sym
    // }

    // fn get_or_declare_global_string(&mut self, s: String) -> DataId {
    //     let sym = self.unique_symbol();
    //     let id = self
    //         .module
    //         .declare_data(&sym, Linkage::Local, false, false)
    //         .unwrap();

    //     let mut str_desc = DataDescription::new();
    //     let mut str_bytes = s.clone().into_bytes();
    //     str_bytes.push(b'\0'); // null terminator
    //     str_desc.define(str_bytes.into_boxed_slice());
    //     self.module.define_data(id, &str_desc).unwrap();

    //     self.strings.insert(s, id.clone());

    //     id
    // }

    // fn process_expr(&mut self, builder: &mut FunctionBuilder, expr: Expr) -> Value {
    //     match expr {
    //         Expr::Str(s) => {
    //             if let Some(local_value) = self.local_strings.get(&s) {
    //                 return local_value.to_owned();
    //             }

    //             let ptr_type = self.module.target_config().pointer_type();
    //             let str_id = if let Some(id) = self.strings.get(&s) {
    //                 id.to_owned()
    //             } else {
    //                 self.get_or_declare_global_string(s.clone())
    //             };

    //             let global_value = self.module.declare_data_in_func(str_id, &mut builder.func);
    //             let str_ptr = builder.ins().global_value(ptr_type, global_value);

    //             self.local_strings.insert(s, str_ptr.clone());

    //             str_ptr
    //         }
    //         Expr::I32(i) => builder.ins().iconst::<i64>(types::I32, (i as i64).into()),
    //     }
    // }

    // fn process_statement(&mut self, builder: &mut FunctionBuilder, stmt: Statement) {
    //     match stmt {
    //         Statement::Call {
    //             function_name,
    //             args,
    //         } => {
    //             let func_ref = if let Some(fnref) = self.local_functions.get(&function_name) {
    //                 fnref.to_owned()
    //             } else {
    //                 let global_id = self
    //                     .functions
    //                     .get(&function_name)
    //                     .expect(&format!("Function not declared: {}", function_name))
    //                     .to_owned();

    //                 let fnref = self
    //                     .module
    //                     .declare_func_in_func(global_id, &mut builder.func);

    //                 self.local_functions.insert(function_name, fnref.clone());

    //                 fnref
    //             };

    //             let mut ir_args = vec![];
    //             for arg in args {
    //                 let value = self.process_expr(builder, arg);
    //                 ir_args.push(value);
    //             }

    //             let call = builder.ins().call(func_ref, &ir_args);
    //             let _retval = builder.inst_results(call)[0];
    //         }

    //         Statement::Return(expr) => {
    //             let value = self.process_expr(builder, expr);
    //             builder.ins().return_(&[value]);
    //         }
    //     }
    // }

    // fn update_function_def(&mut self, function_def: FunctionDef) {
    //     let mut sig = self.module.make_signature();
    //     let ident = function_def.ident;
    //     let return_type = function_def.return_type.into_ir(&self.module);

    //     sig.returns.push(AbiParam::new(return_type.clone()));

    //     let function_id = self
    //         .module
    //         .declare_function(&ident, Linkage::Import, &sig)
    //         .unwrap();

    //     self.module
    //         .declare_function(&ident, Linkage::Export, &sig)
    //         .unwrap();

    //     // not sure how to bypass this issue of
    //     // double mutability as of now, other than this
    //     // forgive me.
    //     let ctx_func_ref = unsafe {
    //         let ptr = &mut self.ctx.func as *mut codegen::ir::Function;
    //         &mut *ptr
    //     };
    //     let fbctx_ref = unsafe {
    //         let ptr = &mut self.fbctx as *mut FunctionBuilderContext;
    //         &mut *ptr
    //     };

    //     let mut builder = FunctionBuilder::new(ctx_func_ref, fbctx_ref);
    //     builder.func.signature = sig;

    //     let entry_block = builder.create_block();
    //     builder.switch_to_block(entry_block);
    //     // builder.seal_block(entry_block);

    //     for stmt in function_def.codeblock.statements {
    //         self.process_statement(&mut builder, stmt);
    //     }

    //     if let Err(err) = codegen::verify_function(&builder.func, self.isa.as_ref()) {
    //         panic!("Verifier error on function '{ident}': {err}");
    //     }

    //     builder.seal_block(entry_block);
    //     builder.finalize();

    //     self.module
    //         .define_function(function_id, &mut self.ctx)
    //         .unwrap();
    //     self.functions.insert(ident, function_id);

    //     self.clear_local();
    // }

    fn update(&mut self, node: RootStatementNode) -> Result<(), CodegenError> {
        match node.root_stmt {
            RootStatement::ExternDeclaration(extern_decl) => {
                self.update_extern_decl(node.id, extern_decl)
            }
            // RootStatement::FunctionDefinition(fn_def) => self.update_function_def(fn_def),
            _ => Err(CodegenError::NotImplemented(node.id)),
        }
    }

    fn update_all(&mut self, source: SourceModule) {
        for node in source.nodes {
            match self.update(node) {
                Err(e) => self.errors.push(e),
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
