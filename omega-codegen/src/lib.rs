use cranelift::{
    codegen::{
        self,
        ir::{BlockArg, FuncRef, StackSlot},
    },
    prelude::{
        AbiParam, Block, Configurable, FloatCC, FunctionBuilder, FunctionBuilderContext, InstBuilder,
        IntCC, MemFlags, Signature, StackSlotData, StackSlotKind, TrapCode, Type as IRType, Value,
        isa, settings, types,
    },
};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use omega_analyzer::{
    checked::{
        CheckedAddressOf, CheckedArrayLiteral, CheckedAssignment, CheckedBinaryOp, CheckedBlock,
        CheckedBreak, CheckedContinue, CheckedDeclaration, CheckedDefer, CheckedExpr,
        CheckedExprNode, CheckedExternDeclaration, CheckedFor, CheckedFunctionCall, CheckedFunctionDef,
        CheckedIf, CheckedItem, CheckedModule, CheckedPlace, CheckedPlaceRoot, CheckedProjection,
        CheckedSlice, CheckedStmt, CheckedStructLiteral, CheckedWhile, NumberValue, Storage,
    },
    resolved_type::{NumericKind, ResolvedFunctionType, ResolvedStructType, ResolvedType},
};
use omega_hir::{BinaryOp, HirId};
use omega_parser::prelude::Ident;
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
    /// One stack slot per local, sized to its type's total byte size (not
    /// one slot per scalar leaf) -- a prerequisite for `&`/`*`: a local
    /// needs a single address, and three independent per-leaf slots have
    /// three unrelated addresses. Field access within it is a byte offset
    /// (see `total_bytes`/`field_byte_offset`), not a leaf-count slice.
    stack_slots: HashMap<HirId, StackSlot>,
    /// The current function's single shared exit point: every `return`,
    /// wherever it's nested (inside an `if`/`while`/`for`), jumps here
    /// instead of emitting its own `return_` directly -- this block is the
    /// only place that actually does. Set once at the start of
    /// `update_function_def`, read by `process_statement`'s `Return` arm.
    /// See `BlockOutcome`'s doc comment for why a single shared exit point
    /// is what makes early returns inside nested control flow tractable.
    return_block: Option<Block>,
    /// One entry per loop currently being emitted (innermost last), pushed
    /// by `emit_while`/`emit_for` around their body and popped once it's
    /// done. `break`/`continue` (see `process_statement`) look their target
    /// up by the `HirId` the checked module already resolved (see
    /// `CheckedBreak`/`CheckedContinue.loop_id`) rather than always reading
    /// `.last()` -- today those always coincide (analysis only ever
    /// resolves to the innermost enclosing loop), but keying by id here
    /// means a future labeled `break 'outer;` needs no codegen changes at
    /// all, only a different resolution rule in analysis.
    loop_stack: Vec<(HirId, LoopTargets)>,
    /// One 1-byte stack slot per `defer` in the function currently being
    /// built, `false` until that `defer`'s own statement executes -- see
    /// `collect_defer_ids`. Allocated and zero-initialized in the entry
    /// block, all at once, before the body is walked (a flag must start
    /// `false` regardless of which path through the function actually
    /// runs, which a lazy per-branch initialization couldn't guarantee).
    defer_flags: HashMap<HirId, StackSlot>,
    /// A `defer`'s deferred body, stashed here by `process_statement`'s
    /// `Defer` arm (moved out of the `CheckedStmt` at its own position in
    /// the walk) for the shared epilogue to actually generate code for,
    /// once, after the whole function body has been walked and every flag
    /// is known.
    defer_bodies: HashMap<HirId, CheckedBlock>,
}

/// Where `break`/`continue` jump to for one loop. `continue_blk` is *not*
/// always the loop's condition-check block: for a `for` loop it's a
/// dedicated block that runs the post-clause before jumping to the
/// condition check (`continue` must still run `i++` in `for (...; ...;
/// i++)`), whereas for a `while` it's the condition check directly. Callers
/// (`process_statement`) don't need to know which -- they just jump here.
#[derive(Clone, Copy)]
struct LoopTargets {
    break_blk: Block,
    continue_blk: Block,
}

/// Where a resolved place's underlying storage lives, for both the read
/// (producing values) and write (storing values) case:
enum PlaceStorage {
    /// Already-materialized SSA values (a `Storage::Parameter` that hasn't
    /// been dereferenced through) -- readable, but has no address: there is
    /// no memory location backing a bare SSA value.
    Values(Vec<Value>),
    /// A byte offset into one compile-time-known stack slot (`Storage::Local`,
    /// before any `Deref`).
    Slot { slot: StackSlot, offset: u32 },
    /// A byte offset from a runtime pointer value -- the state from the
    /// first `Deref` projection onward (explicit `*`, or a seamless
    /// pointer-to-struct field access), since the pointee isn't known until
    /// runtime.
    Address { base: Value, offset: u32 },
}

/// The result of emitting a `{ ... }` block, or anything shaped like one
/// (an `if`'s branch, a `while`/`for` body): either it fell off the end
/// normally (`Value`, the block's tail-expression leaves -- empty for
/// `Void`/no tail), or it unconditionally `return`ed (`Diverged`) -- in
/// which case the cranelift block it was building is *already* terminated
/// (by a jump to the function's shared `return_block`, see `Codegen::
/// return_block`), and the caller must not emit anything else into it
/// (another terminator in the same block is invalid IR).
///
/// This is what makes an early `return` nested inside an `if`/`while`/`for`
/// tractable without full reachability analysis: every exit funnels through
/// one `jump` to one shared block, so "did this branch already leave" is
/// just "did processing it report `Diverged`," checked locally at each
/// merge point (`emit_if`'s `then`/`else`, `emit_block`'s per-statement
/// loop) rather than needing a whole-function control-flow graph.
enum BlockOutcome {
    Value(Vec<Value>),
    Diverged,
}

trait IntoIRType {
    fn into_ir_type(self, codegen: &Codegen) -> Vec<IRType>;
}

impl IntoIRType for ResolvedType {
    fn into_ir_type(self, codegen: &Codegen) -> Vec<IRType> {
        match self {
            ResolvedType::Void => vec![],
            // `Bool` is a plain 0/1 byte -- cranelift's integer types are
            // sign-agnostic and there's no dedicated boolean IR type to use
            // instead (see `ResolvedType::Bool`'s doc comment).
            ResolvedType::Bool => vec![types::I8],
            // A decoded 4-byte Unicode scalar value, not a byte -- see
            // `ResolvedType::Char`'s doc comment for why this isn't `I8`.
            ResolvedType::Char => vec![types::I32],
            ResolvedType::I8 | ResolvedType::U8 => vec![types::I8],
            ResolvedType::I16 | ResolvedType::U16 => vec![types::I16],
            ResolvedType::I32 | ResolvedType::U32 => vec![types::I32],
            ResolvedType::I64 | ResolvedType::U64 => vec![types::I64],
            // Unlike every other numeric variant, this one's IR type is
            // genuinely target-dependent -- `into_ir_type` already takes
            // `&Codegen`, so this is the one place `usize`/`isize` actually
            // track the real pointer width rather than a hardcoded 64 bits
            // (see `ResolvedType::USize`/`ISize`'s doc comments).
            ResolvedType::USize | ResolvedType::ISize => vec![codegen.pointer_type()],
            ResolvedType::F32 => vec![types::F32],
            ResolvedType::F64 => vec![types::F64],
            ResolvedType::Struct(struct_type) => struct_type
                .borrow()
                .fields
                .iter()
                .flat_map(|x| x.1.clone().into_ir_type(codegen))
                .collect(),
            // `N` copies of the item type's own leaves, back to back -- the
            // same packed, no-padding layout a `Struct`'s fields get.
            ResolvedType::SizedArray(item_type, size) => {
                let item_leaves = item_type.into_ir_type(codegen);
                std::iter::repeat_n(item_leaves, size as usize).flatten().collect()
            }
            // A fat pointer: a data pointer plus an `i32` length. See
            // `ResolvedType::Slice`'s doc comment for why this is a distinct
            // variant rather than `Pointer(Array(_))`.
            ResolvedType::Slice(_) => vec![codegen.pointer_type(), types::I32],
            // `Pointer`, `Function`, and the legacy unsized `Array` (see its
            // doc comment) are all a single thin pointer value.
            ResolvedType::Pointer(_) | ResolvedType::Function(_) | ResolvedType::Array(_) => {
                vec![codegen.pointer_type()]
            }
        }
    }
}

/// Slices a `FieldAccess` projection's already-resolved `field_index` out of
/// an already-materialized value list (a `PlaceStorage::Values`, i.e. a
/// `Storage::Parameter` that hasn't been dereferenced through -- positional,
/// by leaf count, since there's no memory/byte offset for a bare SSA value).
/// No name search, no failure path: the checked module already picked this
/// exact index out of `struct_type`.
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

/// `jump`'s block arguments are `BlockArg`, not bare `Value` -- this just
/// wraps each one, for the handful of `jump` call sites that pass along a
/// block's already-materialized leaf values.
fn block_args(values: &[Value]) -> Vec<BlockArg> {
    values.iter().map(|v| BlockArg::from(*v)).collect()
}

/// A resolved type's total in-memory size, in bytes: the sum of its scalar
/// leaves' sizes (`into_ir_type` already flattens a struct recursively into
/// its leaves, so this needs no separate struct case). Layout is packed --
/// each field is placed at the raw running byte sum of its predecessors,
/// with no alignment padding. x86_64 tolerates unaligned loads/stores with
/// no correctness issue, so this is safe; it's just not C-ABI-compatible
/// layout, consistent with the rest of this codegen not implementing true
/// C-ABI struct-passing conventions at function boundaries either (structs
/// are passed as flattened positional scalars, not per SysV aggregate rules).
fn total_bytes(r#type: ResolvedType, codegen: &Codegen) -> u32 {
    r#type.into_ir_type(codegen).iter().map(|t| t.bytes()).sum()
}

/// A `FieldAccess` projection's already-resolved `field_index`'s packed byte
/// offset within `struct_type` -- the memory-backed (`Slot`/`Address`)
/// counterpart to `project_field_access`'s positional (`Values`) slicing.
fn field_byte_offset(struct_type: &ResolvedStructType, field_index: usize, codegen: &Codegen) -> u32 {
    struct_type.fields[..field_index]
        .iter()
        .map(|(_, r#type)| total_bytes(r#type.clone(), codegen))
        .sum()
}

/// Every `defer`'s `HirId` reachable inside `block`, in declaration (program)
/// order -- the full set a function's flags need to be allocated and
/// zero-initialized for, computed once up front (see `define_function_def`)
/// so that initialization can happen unconditionally in the entry block,
/// before the body itself is walked for real.
///
/// This has to be a genuine full recursive walk of every
/// `CheckedStmt`/`CheckedExpr`/`CheckedPlace` shape that can embed a nested
/// `CheckedBlock` or `CheckedExprNode` -- not just the statement-position
/// `If`/`Codeblock` cases `emit_expr_stmt` itself dispatches on. Analysis
/// places no restriction against a `defer` nested inside a compound
/// expression (e.g. `x := if cond { defer cleanup(); 1 } else { 2 };`), and
/// codegen's own `process_expr` does reach and run such a defer's flag-set
/// from any expression position -- so a narrower walk here would silently
/// miss allocating (and zero-initializing) that defer's flag, which the
/// epilogue would then either panic looking up or -- undetected -- skip
/// running entirely. The only deliberate exclusion is `CheckedStmt::
/// Struct`'s `.functions`: a locally-nested struct's methods are
/// independent function scopes, matching `process_statement`'s own
/// `CheckedStmt::Struct(_) => false // Only analysis is necessary` --
/// codegen never touches their bodies from the enclosing function's pass
/// regardless.
fn collect_defer_ids(block: &CheckedBlock, out: &mut Vec<HirId>) {
    for stmt in &block.stmts {
        collect_defer_ids_stmt(stmt, out);
    }
    if let Some(tail) = &block.tail {
        collect_defer_ids_expr(tail, out);
    }
}

fn collect_defer_ids_stmt(stmt: &CheckedStmt, out: &mut Vec<HirId>) {
    match stmt {
        CheckedStmt::Declaration(_) | CheckedStmt::ExternDeclaration(_) | CheckedStmt::Break(_)
        | CheckedStmt::Continue(_) => {}
        // Independent function scope -- never recursed into, see this
        // function's doc comment.
        CheckedStmt::Struct(_) => {}
        CheckedStmt::Expression(e) | CheckedStmt::Return(e) => collect_defer_ids_expr(e, out),
        CheckedStmt::While(w) => {
            collect_defer_ids_expr(&w.condition, out);
            collect_defer_ids(&w.body, out);
        }
        CheckedStmt::For(f) => {
            for s in &f.init {
                collect_defer_ids_stmt(s, out);
            }
            collect_defer_ids_expr(&f.condition, out);
            if let Some(post) = &f.post {
                collect_defer_ids_expr(post, out);
            }
            collect_defer_ids(&f.body, out);
        }
        CheckedStmt::Defer(d) => {
            out.push(d.id);
            // Always empty in practice -- analysis rejects a `defer` nested
            // inside another `defer`'s body outright -- but walked anyway
            // for uniformity rather than relying on that invariant here too.
            collect_defer_ids(&d.body, out);
        }
    }
}

fn collect_defer_ids_expr(expr: &CheckedExprNode, out: &mut Vec<HirId>) {
    match &expr.kind {
        CheckedExpr::Number(_) | CheckedExpr::Bool(_) | CheckedExpr::Char(_) | CheckedExpr::String(_) => {}
        CheckedExpr::Place(p) => collect_defer_ids_place(p, out),
        CheckedExpr::FunctionCall(call) => {
            collect_defer_ids_expr(&call.callee, out);
            for arg in &call.args {
                collect_defer_ids_expr(arg, out);
            }
        }
        CheckedExpr::Assignment(a) => {
            collect_defer_ids_place(&a.target, out);
            collect_defer_ids_expr(&a.value, out);
        }
        CheckedExpr::AddressOf(a) => collect_defer_ids_place(&a.place, out),
        CheckedExpr::Negate(e) => collect_defer_ids_expr(e, out),
        CheckedExpr::BinaryOp(b) => {
            collect_defer_ids_expr(&b.left, out);
            collect_defer_ids_expr(&b.right, out);
        }
        CheckedExpr::Codeblock(block) => collect_defer_ids(block, out),
        CheckedExpr::If(if_expr) => {
            for (cond, block) in &if_expr.branches {
                collect_defer_ids_expr(cond, out);
                collect_defer_ids(block, out);
            }
            if let Some(else_branch) = &if_expr.else_branch {
                collect_defer_ids(else_branch, out);
            }
        }
        CheckedExpr::ArrayLiteral(lit) => {
            for e in &lit.elements {
                collect_defer_ids_expr(e, out);
            }
        }
        CheckedExpr::StructLiteral(lit) => {
            for f in &lit.fields {
                collect_defer_ids_expr(&f.value, out);
            }
        }
        CheckedExpr::Slice(s) => {
            collect_defer_ids_place(&s.base, out);
            if let Some(start) = &s.start {
                collect_defer_ids_expr(start, out);
            }
            if let Some(end) = &s.end {
                collect_defer_ids_expr(end, out);
            }
        }
    }
}

fn collect_defer_ids_place(place: &CheckedPlace, out: &mut Vec<HirId>) {
    if let CheckedPlaceRoot::Expr(e) = &place.root {
        collect_defer_ids_expr(e, out);
    }
    for proj in &place.projections {
        if let CheckedProjection::Index { index_expr, .. } = proj {
            collect_defer_ids_expr(index_expr, out);
        }
    }
}

impl Codegen {
    pub fn generate(
        module_name: &str,
        isa: &str,
        modules: Vec<(Vec<Ident>, CheckedModule)>,
        entry: &[Ident],
    ) -> Self {
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
            return_block: None,
            loop_stack: Vec::new(),
            defer_flags: HashMap::new(),
            defer_bodies: HashMap::new(),
        };

        codegen.update_all(modules, entry);

        codegen
    }

    fn clear_local(&mut self) {
        self.local_strings.clear();
        self.ctx.clear();
        self.stack_slots.clear();
        self.local_args.clear();
        self.return_block = None;
        self.loop_stack.clear();
        self.defer_flags.clear();
        self.defer_bodies.clear();
    }

    /// Walks a place's root and projections once, tracking where its
    /// storage currently lives -- switching from `Slot`/`Values` to
    /// `Address` the moment a `Deref` (explicit, or an array `Index`'s
    /// implicit pointer arithmetic) happens, since the pointee isn't known
    /// until runtime. This is the one general mechanism behind reading,
    /// writing, and taking the address of a place, regardless of how many
    /// derefs/field accesses/indices got it there.
    fn resolve_place_storage(
        &mut self,
        place: &CheckedPlace,
        builder: &mut FunctionBuilder,
    ) -> (PlaceStorage, ResolvedType) {
        let (mut current, mut current_type) = match &place.root {
            CheckedPlaceRoot::Variable { decl_id, storage, r#type } => {
                let current = match storage {
                    Storage::Local => {
                        let slot = *self.stack_slots.get(decl_id).unwrap_or_else(|| {
                            panic!("checked module guarantees {decl_id:?} was declared before this use")
                        });
                        PlaceStorage::Slot { slot, offset: 0 }
                    }
                    Storage::Parameter => {
                        let values = self.local_args.get(decl_id).cloned().unwrap_or_else(|| {
                            panic!("checked module guarantees {decl_id:?} was bound as a parameter before this use")
                        });
                        PlaceStorage::Values(values)
                    }
                    Storage::Function => {
                        unreachable!(
                            "a function reference is never itself further-projected; calls resolve it directly via get_place_value"
                        );
                    }
                    Storage::Global => todo!("global/extern data storage is not yet implemented"),
                };
                (current, r#type.clone())
            }
            // A temporary as the root of a projection chain -- `foo().bar`,
            // `Vec2 { ... }.x`, or a method call's implicit `&self` on
            // either: materialized into an anonymous stack slot so the rest
            // of the projection walk (including taking its address) has
            // ordinary memory to work against, exactly like a local's slot
            // -- the temporary just has no name and no declaration to key
            // `stack_slots` by.
            CheckedPlaceRoot::Expr(expr) => {
                let r#type = expr.r#type.clone();
                let values = self.process_expr(builder, (**expr).clone());
                let size = total_bytes(r#type.clone(), self);
                let slot = builder
                    .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 4));
                let storage = PlaceStorage::Slot { slot, offset: 0 };
                self.store_scalars(builder, &storage, &values);
                (storage, r#type)
            }
        };

        for projection in &place.projections {
            match projection {
                CheckedProjection::FieldAccess { index, r#type, .. } => {
                    let ResolvedType::Struct(struct_type) = &current_type else {
                        unreachable!("checked module guarantees field projections are only built against a struct type");
                    };
                    let struct_type = struct_type.clone();
                    let struct_type = struct_type.borrow();
                    current = match current {
                        PlaceStorage::Values(values) => {
                            PlaceStorage::Values(project_field_access(self, &values, &struct_type, *index))
                        }
                        PlaceStorage::Slot { slot, offset } => PlaceStorage::Slot {
                            slot,
                            offset: offset + field_byte_offset(&struct_type, *index, self),
                        },
                        PlaceStorage::Address { base, offset } => PlaceStorage::Address {
                            base,
                            offset: offset + field_byte_offset(&struct_type, *index, self),
                        },
                    };
                    current_type = r#type.clone();
                }

                CheckedProjection::Deref { r#type } => {
                    let ptr_value = self.load_scalars(builder, &current, &current_type)[0];
                    current = PlaceStorage::Address { base: ptr_value, offset: 0 };
                    current_type = r#type.clone();
                }

                CheckedProjection::Index { index_expr, item_type } => {
                    // The element size comes from `item_type` (the resolved
                    // element type analysis already picked out), not from
                    // flattening `current_type` itself -- the container's own
                    // `into_ir_type` (a single thin pointer for `Array`, or
                    // N*item leaves for `SizedArray`) has nothing to do with
                    // one element's size.
                    let element_ir_size = total_bytes(item_type.clone(), self);

                    let mut base = match &current_type {
                        // Inline contiguous storage: index off the storage's
                        // own address, not a pointer value loaded from it --
                        // there is no pointer to load, the elements live
                        // directly in `current`.
                        ResolvedType::SizedArray(_, _) => self.place_storage_address(builder, &current),
                        // `Array` (the legacy thin-pointer unsized form,
                        // e.g. `argv`) *is* a pointer value; `Slice`'s first
                        // flattened leaf is its data pointer (the second,
                        // its length, isn't needed for a single-element
                        // index).
                        ResolvedType::Array(_) | ResolvedType::Slice(_) => {
                            self.load_scalars(builder, &current, &current_type)[0]
                        }
                        _ => unreachable!(
                            "checked module guarantees Index projections only apply to Array/SizedArray/Slice"
                        ),
                    };
                    let mut index = self.process_expr(builder, (**index_expr).clone())[0];

                    let ptr_type = self.pointer_type();
                    if builder.func.dfg.value_type(base) != ptr_type {
                        base = builder.ins().uextend(ptr_type, base);
                    }
                    if builder.func.dfg.value_type(index) != ptr_type {
                        index = builder.ins().uextend(ptr_type, index);
                    }

                    let element_size = builder.ins().iconst(ptr_type, element_ir_size as i64);
                    let offset = builder.ins().imul(index, element_size);
                    let element_addr = builder.ins().iadd(base, offset);

                    current = PlaceStorage::Address { base: element_addr, offset: 0 };
                    current_type = item_type.clone();
                }

                CheckedProjection::SliceLength => {
                    // A slice is flattened as [data pointer, i32 length] (see
                    // `ResolvedType::Slice`'s `into_ir_type`) -- `.length` is
                    // just the second leaf, at a byte offset of one pointer's
                    // width past the start of the slice's own storage.
                    let ptr_size = self.pointer_type().bytes();
                    current = match current {
                        PlaceStorage::Values(values) => PlaceStorage::Values(vec![values[1]]),
                        PlaceStorage::Slot { slot, offset } => {
                            PlaceStorage::Slot { slot, offset: offset + ptr_size }
                        }
                        PlaceStorage::Address { base, offset } => {
                            PlaceStorage::Address { base, offset: offset + ptr_size }
                        }
                    };
                    current_type = ResolvedType::I32;
                }
            }
        }

        (current, current_type)
    }

    /// The runtime address backing `storage` -- the same address-resolution
    /// `AddressOf` needs, but also needed by `SizedArray` indexing (which
    /// must index off the storage's own address, having no pointer value to
    /// load) and slice construction from a `SizedArray` base.
    fn place_storage_address(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage) -> Value {
        let ptr_type = self.pointer_type();
        match storage {
            PlaceStorage::Values(_) => {
                todo!("taking the address of a function parameter is not yet implemented");
            }
            PlaceStorage::Slot { slot, offset } => builder.ins().stack_addr(ptr_type, *slot, *offset as i32),
            PlaceStorage::Address { base, offset: 0 } => *base,
            PlaceStorage::Address { base, offset } => {
                let offset_val = builder.ins().iconst(ptr_type, *offset as i64);
                builder.ins().iadd(*base, offset_val)
            }
        }
    }

    /// Reads every scalar leaf of `r#type` out of `storage`, in leaf order.
    fn load_scalars(
        &mut self,
        builder: &mut FunctionBuilder,
        storage: &PlaceStorage,
        r#type: &ResolvedType,
    ) -> Vec<Value> {
        if let PlaceStorage::Values(values) = storage {
            return values.clone();
        }

        let mut result = Vec::new();
        let mut rel_offset = 0u32;
        for leaf in r#type.clone().into_ir_type(self) {
            let value = match storage {
                PlaceStorage::Slot { slot, offset } => {
                    builder.ins().stack_load(leaf, *slot, (*offset + rel_offset) as i32)
                }
                PlaceStorage::Address { base, offset } => {
                    builder.ins().load(leaf, MemFlags::new(), *base, (*offset + rel_offset) as i32)
                }
                PlaceStorage::Values(_) => unreachable!("handled above"),
            };
            result.push(value);
            rel_offset += leaf.bytes();
        }
        result
    }

    /// Writes `values` (one per scalar leaf, in leaf order) into `storage`.
    fn store_scalars(&mut self, builder: &mut FunctionBuilder, storage: &PlaceStorage, values: &[Value]) {
        let mut rel_offset = 0u32;
        for value in values {
            let leaf = builder.func.dfg.value_type(*value);
            match storage {
                PlaceStorage::Values(_) => {
                    todo!("assignment into a function parameter is not yet implemented");
                }
                PlaceStorage::Slot { slot, offset } => {
                    builder.ins().stack_store(*value, *slot, (*offset + rel_offset) as i32);
                }
                PlaceStorage::Address { base, offset } => {
                    builder.ins().store(MemFlags::new(), *value, *base, (*offset + rel_offset) as i32);
                }
            }
            rel_offset += leaf.bytes();
        }
    }

    fn get_place_value(&mut self, place: &CheckedPlace, builder: &mut FunctionBuilder) -> Vec<Value> {
        // A function reference has no memory backing at all -- just a
        // symbol address -- so it's handled before the general
        // storage-resolution path (checked module guarantees this root
        // never carries further projections, see `resolve_place_storage`).
        if let CheckedPlaceRoot::Variable { decl_id, storage: Storage::Function, .. } = &place.root {
            let function = *self.functions.get(decl_id).unwrap_or_else(|| {
                panic!("checked module guarantees {decl_id:?} was declared as a function before this use")
            });
            let func = self.get_func_ref_from_id(builder, function);
            return vec![builder.ins().func_addr(self.pointer_type(), func)];
        }

        let (storage, r#type) = self.resolve_place_storage(place, builder);
        self.load_scalars(builder, &storage, &r#type)
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

    fn update_extern_decl(&mut self, extern_decl: CheckedExternDeclaration) {
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

    /// C's variadic calling convention requires the caller to promote each
    /// *variadic* argument (never a fixed/named one, whose width is fixed by
    /// the callee's prototype) before passing it: any integer narrower than
    /// `int` is sign/zero-extended to 32 bits, and `float` is promoted to
    /// `double` -- otherwise a callee like `printf` (which reads variadic
    /// arguments according to those default-promoted widths, per its format
    /// string) would read garbage. Only applies to `arg_type`s that flatten
    /// to exactly one IR leaf (every numeric primitive does); called
    /// unconditionally on every variadic argument, so anything else (a
    /// pointer, already the right width) just passes through unchanged.
    fn promote_variadic_arg(&mut self, builder: &mut FunctionBuilder, value: Value, arg_type: &ResolvedType) -> Value {
        match arg_type.numeric_kind() {
            Some(NumericKind::Float(width)) if width < 64 => builder.ins().fpromote(types::F64, value),
            Some(NumericKind::Signed(width)) if width < 32 => builder.ins().sextend(types::I32, value),
            Some(NumericKind::Unsigned(width)) if width < 32 => builder.ins().uextend(types::I32, value),
            // `Bool` isn't `numeric_kind`-classified (see its doc comment),
            // but it's still an 8-bit integer that needs the same promotion.
            None if *arg_type == ResolvedType::Bool => builder.ins().uextend(types::I32, value),
            _ => value,
        }
    }

    /// Runs every statement in `block`, stopping early (without touching the
    /// now-terminated current cranelift block again) the moment one of them
    /// diverges, then evaluates the tail expression (if any and if still
    /// reachable). See `BlockOutcome`'s doc comment.
    fn emit_block(&mut self, builder: &mut FunctionBuilder, block: CheckedBlock) -> BlockOutcome {
        for stmt in block.stmts {
            if self.process_statement(builder, stmt) {
                return BlockOutcome::Diverged;
            }
        }
        match block.tail {
            Some(tail) => self.emit_expr_stmt(builder, *tail),
            None => BlockOutcome::Value(vec![]),
        }
    }

    /// Evaluates an expression in a position where divergence matters (a
    /// statement, or a block's tail): `If`/bare `Codeblock` are the only
    /// expression kinds that can possibly diverge (by ending in an
    /// unconditional `return`), so they're routed through the
    /// `BlockOutcome`-aware path; everything else can never diverge and
    /// goes through the ordinary `process_expr`.
    fn emit_expr_stmt(&mut self, builder: &mut FunctionBuilder, expr: CheckedExprNode) -> BlockOutcome {
        if matches!(&expr.kind, CheckedExpr::If(_) | CheckedExpr::Codeblock(_)) {
            let result_leaves = expr.r#type.clone().into_ir_type(self);
            match expr.kind {
                CheckedExpr::If(CheckedIf { branches, else_branch }) => {
                    self.emit_if(builder, branches.into_iter(), else_branch, &result_leaves)
                }
                CheckedExpr::Codeblock(block) => self.emit_block(builder, block),
                _ => unreachable!("checked by the matches! above"),
            }
        } else {
            BlockOutcome::Value(self.process_expr(builder, expr))
        }
    }

    /// Builds one `if`/`else if` branch's `brif` and recurses on the rest of
    /// the chain for its `else` -- so `if a {..} else if b {..} else {..}`
    /// becomes, structurally, `if a {..} else { if b {..} else {..} } `.
    /// `branches` is an iterator (rather than a slice) precisely so the
    /// recursive call can hand off "everything after the one I just
    /// consumed" by simply passing the same, now-advanced iterator along.
    ///
    /// `result_leaves` (computed once by the caller, from the whole `if`
    /// expression's resolved type) is used for the merge block's params at
    /// every recursion depth, since analysis already guarantees every
    /// branch/else agrees on that type.
    fn emit_if(
        &mut self,
        builder: &mut FunctionBuilder,
        mut branches: std::vec::IntoIter<(CheckedExprNode, CheckedBlock)>,
        else_branch: Option<CheckedBlock>,
        result_leaves: &[IRType],
    ) -> BlockOutcome {
        let Some((cond, then_body)) = branches.next() else {
            return match else_branch {
                Some(b) => self.emit_block(builder, b),
                None => BlockOutcome::Value(vec![]),
            };
        };

        let cond_value = self.process_expr(builder, cond)[0];

        let then_blk = builder.create_block();
        let else_blk = builder.create_block();
        let merge_blk = builder.create_block();
        for ty in result_leaves {
            builder.append_block_param(merge_blk, *ty);
        }

        builder.ins().brif(cond_value, then_blk, &[], else_blk, &[]);

        builder.switch_to_block(then_blk);
        builder.seal_block(then_blk);
        let then_outcome = self.emit_block(builder, then_body);
        if let BlockOutcome::Value(values) = &then_outcome {
            builder.ins().jump(merge_blk, &block_args(values));
        }

        builder.switch_to_block(else_blk);
        builder.seal_block(else_blk);
        let else_outcome = self.emit_if(builder, branches, else_branch, result_leaves);
        if let BlockOutcome::Value(values) = &else_outcome {
            builder.ins().jump(merge_blk, &block_args(values));
        }

        builder.switch_to_block(merge_blk);
        if matches!(then_outcome, BlockOutcome::Diverged) && matches!(else_outcome, BlockOutcome::Diverged) {
            // Both paths already jumped to the function's shared return
            // block -- this merge point is provably unreachable, but
            // cranelift still requires every block to end in a terminator.
            // A trap satisfies that without pretending the block is live.
            builder.ins().trap(TrapCode::unwrap_user(1));
            builder.seal_block(merge_blk);
            BlockOutcome::Diverged
        } else {
            builder.seal_block(merge_blk);
            BlockOutcome::Value(builder.block_params(merge_blk).to_vec())
        }
    }

    /// `header_blk` holds the condition check and is re-entered on every
    /// iteration (the back-edge `jump` below), so it can't be sealed until
    /// that back-edge (its second predecessor, after the initial jump into
    /// it) either exists or is known not to (`body`'s outcome).
    ///
    /// `loop_id` is this loop's own `HirId` (from `CheckedWhile.id`) -- the
    /// same identity `break`/`continue` inside `body` resolved against in
    /// analysis (`CheckedBreak`/`CheckedContinue.loop_id`), so pushing
    /// `(loop_id, targets)` here and popping it once `body` is fully
    /// processed is what lets `process_statement` find the right target
    /// for a `break`/`continue` at any nesting depth inside `body`,
    /// including through further nested loops (each pushes its own entry;
    /// an inner loop's `break` finds *its own* `.last()` first).
    fn emit_while(&mut self, builder: &mut FunctionBuilder, loop_id: HirId, condition: CheckedExprNode, body: CheckedBlock) {
        let header_blk = builder.create_block();
        let body_blk = builder.create_block();
        let exit_blk = builder.create_block();

        builder.ins().jump(header_blk, &[]);

        builder.switch_to_block(header_blk);
        let cond_value = self.process_expr(builder, condition)[0];
        builder.ins().brif(cond_value, body_blk, &[], exit_blk, &[]);

        builder.switch_to_block(body_blk);
        builder.seal_block(body_blk);
        self.loop_stack.push((loop_id, LoopTargets { break_blk: exit_blk, continue_blk: header_blk }));
        let body_outcome = self.emit_block(builder, body);
        self.loop_stack.pop();
        if let BlockOutcome::Value(_) = body_outcome {
            builder.ins().jump(header_blk, &[]);
        }
        builder.seal_block(header_blk);

        builder.switch_to_block(exit_blk);
        builder.seal_block(exit_blk);
    }

    /// Same shape as `emit_while`, plus a one-time `init` before the loop.
    /// `condition` is mandatory here (unlike the parser's/HIR's, see
    /// `CheckedFor`'s doc comment for why analysis enforces that) -- so,
    /// unlike a hypothetical always-true loop, `exit_blk` is always
    /// statically guaranteed a real predecessor (the condition's
    /// false-branch), never needing the same "trap an unreachable block"
    /// treatment `emit_if` does.
    ///
    /// Unlike `while`, `continue`'s target here is a dedicated
    /// `continue_blk` rather than `header_blk` directly: C-style `continue`
    /// still has to run the post-clause (`i++` in `for (...; ...; i++)`)
    /// before re-checking the condition, so both the body's normal
    /// fallthrough *and* any `continue` inside it jump to `continue_blk`,
    /// which runs `post` once and then jumps to `header_blk` itself.
    fn emit_for(
        &mut self,
        builder: &mut FunctionBuilder,
        loop_id: HirId,
        init: Vec<CheckedStmt>,
        condition: CheckedExprNode,
        post: Option<CheckedExprNode>,
        body: CheckedBlock,
    ) {
        for stmt in init {
            self.process_statement(builder, stmt);
        }

        let header_blk = builder.create_block();
        let continue_blk = builder.create_block();
        let body_blk = builder.create_block();
        let exit_blk = builder.create_block();

        builder.ins().jump(header_blk, &[]);

        builder.switch_to_block(header_blk);
        let cond_value = self.process_expr(builder, condition)[0];
        builder.ins().brif(cond_value, body_blk, &[], exit_blk, &[]);

        builder.switch_to_block(body_blk);
        builder.seal_block(body_blk);
        self.loop_stack.push((loop_id, LoopTargets { break_blk: exit_blk, continue_blk }));
        let body_outcome = self.emit_block(builder, body);
        self.loop_stack.pop();
        if let BlockOutcome::Value(_) = body_outcome {
            builder.ins().jump(continue_blk, &[]);
        }

        // Predecessors: the body's normal fallthrough (just above, if it
        // didn't diverge) and any `continue` inside it (already emitted,
        // during `emit_block` above) -- both always precede this point.
        builder.switch_to_block(continue_blk);
        builder.seal_block(continue_blk);
        if let Some(post) = post {
            self.process_expr(builder, post);
        }
        builder.ins().jump(header_blk, &[]);
        builder.seal_block(header_blk);

        builder.switch_to_block(exit_blk);
        builder.seal_block(exit_blk);
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

                let fixed_count = fn_type.params.len();
                let mut ir_args = vec![];
                for (i, arg) in args.into_iter().enumerate() {
                    let arg_type = arg.r#type.clone();
                    let mut value = self.process_expr(builder, arg);
                    // Only the variadic tail needs default-argument
                    // promotion; a fixed/named parameter's width is already
                    // pinned by the callee's declared signature.
                    if fn_type.is_variadic && i >= fixed_count && let [v] = value.as_mut_slice() {
                        *v = self.promote_variadic_arg(builder, *v, &arg_type);
                    }
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
                // The one and only leaf of `node.r#type`'s own flattening --
                // every resolved numeric type is exactly one IR leaf -- picks
                // the concrete width/kind to narrow `value` into. `value`
                // itself is already range-checked against this same type by
                // analysis (see `Analyzer::analyze_number`), so this never
                // has to reject anything, only narrow losslessly.
                let ir_type = node.r#type.clone().into_ir_type(self)[0];
                let result = match value {
                    NumberValue::Signed(v) => builder.ins().iconst(ir_type, v),
                    NumberValue::Unsigned(v) => builder.ins().iconst(ir_type, v as i64),
                    NumberValue::Float(v) if ir_type == types::F32 => builder.ins().f32const(v as f32),
                    NumberValue::Float(v) => builder.ins().f64const(v),
                };
                vec![result]
            }

            CheckedExpr::Bool(b) => vec![builder.ins().iconst(types::I8, b as i64)],

            // Cranelift has no dedicated char/codepoint type -- a `char`'s
            // one IR leaf is just its `u32` codepoint stored in an `I32`
            // (see `Char`'s `into_ir_type` arm).
            CheckedExpr::Char(c) => vec![builder.ins().iconst(types::I32, c as i64)],

            CheckedExpr::Place(place) => self.get_place_value(&place, builder),

            CheckedExpr::Assignment(CheckedAssignment { target, value }) => {
                let values = self.process_expr(builder, *value);
                // Uniformly covers assignment to a local, through any depth
                // of explicit/seamless deref (`*ptr = 5;`, `ptr.field = 5;`),
                // and through array indexing -- whatever `target` resolved
                // to, `store_scalars` only cares whether it has an address
                // (`todo!()`s itself for the one case that doesn't yet,
                // `Storage::Parameter` with no deref in between).
                let (storage, _) = self.resolve_place_storage(&target, builder);
                self.store_scalars(builder, &storage, &values);
                values
            }

            CheckedExpr::AddressOf(CheckedAddressOf { place }) => {
                let (storage, _) = self.resolve_place_storage(&place, builder);
                vec![self.place_storage_address(builder, &storage)]
            }

            CheckedExpr::Negate(base) => {
                // Checked module guarantees only signed ints or floats reach
                // here (see `Analyzer`'s `HirExpr::Negate` arm) -- `fneg` for
                // the latter, `ineg` (two's-complement negation) for the
                // former.
                let is_float = matches!(base.r#type.numeric_kind(), Some(NumericKind::Float(_)));
                let value = self.process_expr(builder, *base)[0];
                let result = if is_float { builder.ins().fneg(value) } else { builder.ins().ineg(value) };
                vec![result]
            }

            CheckedExpr::BinaryOp(CheckedBinaryOp { op, left, right }) => {
                // Checked module guarantees both operands share the same
                // numeric resolved type (see `Analyzer`'s `HirExpr::BinaryOp`
                // arm), so either one's `numeric_kind` picks the right
                // instruction for the whole operation.
                let kind = left
                    .r#type
                    .numeric_kind()
                    .expect("checked module guarantees BinaryOp operands are numeric");
                let left = self.process_expr(builder, *left)[0];
                let right = self.process_expr(builder, *right)[0];
                // Division/modulo by zero traps at the instruction level --
                // consistent with this language having no other runtime
                // safety net (no bounds checks either), so no special
                // handling is needed here.
                let result = match (op, kind) {
                    (BinaryOp::Add, NumericKind::Float(_)) => builder.ins().fadd(left, right),
                    (BinaryOp::Add, _) => builder.ins().iadd(left, right),
                    (BinaryOp::Sub, NumericKind::Float(_)) => builder.ins().fsub(left, right),
                    (BinaryOp::Sub, _) => builder.ins().isub(left, right),
                    (BinaryOp::Mul, NumericKind::Float(_)) => builder.ins().fmul(left, right),
                    (BinaryOp::Mul, _) => builder.ins().imul(left, right),
                    (BinaryOp::Div, NumericKind::Float(_)) => builder.ins().fdiv(left, right),
                    (BinaryOp::Div, NumericKind::Signed(_)) => builder.ins().sdiv(left, right),
                    (BinaryOp::Div, NumericKind::Unsigned(_)) => builder.ins().udiv(left, right),
                    (BinaryOp::Rem, NumericKind::Signed(_)) => builder.ins().srem(left, right),
                    (BinaryOp::Rem, NumericKind::Unsigned(_)) => builder.ins().urem(left, right),
                    (BinaryOp::Rem, NumericKind::Float(_)) => {
                        unreachable!("checked module rejects '%' on float operands")
                    }
                    (cmp, NumericKind::Float(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => FloatCC::Equal,
                            BinaryOp::Ne => FloatCC::NotEqual,
                            BinaryOp::Lt => FloatCC::LessThan,
                            BinaryOp::Le => FloatCC::LessThanOrEqual,
                            BinaryOp::Gt => FloatCC::GreaterThan,
                            BinaryOp::Ge => FloatCC::GreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().fcmp(cc, left, right)
                    }
                    (cmp, NumericKind::Signed(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => IntCC::Equal,
                            BinaryOp::Ne => IntCC::NotEqual,
                            BinaryOp::Lt => IntCC::SignedLessThan,
                            BinaryOp::Le => IntCC::SignedLessThanOrEqual,
                            BinaryOp::Gt => IntCC::SignedGreaterThan,
                            BinaryOp::Ge => IntCC::SignedGreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().icmp(cc, left, right)
                    }
                    (cmp, NumericKind::Unsigned(_)) => {
                        let cc = match cmp {
                            BinaryOp::Eq => IntCC::Equal,
                            BinaryOp::Ne => IntCC::NotEqual,
                            BinaryOp::Lt => IntCC::UnsignedLessThan,
                            BinaryOp::Le => IntCC::UnsignedLessThanOrEqual,
                            BinaryOp::Gt => IntCC::UnsignedGreaterThan,
                            BinaryOp::Ge => IntCC::UnsignedGreaterThanOrEqual,
                            _ => unreachable!("not a comparison op"),
                        };
                        builder.ins().icmp(cc, left, right)
                    }
                };
                vec![result]
            }

            CheckedExpr::Codeblock(block) => match self.emit_block(builder, block) {
                BlockOutcome::Value(values) => values,
                // Only reachable if a bare `{}` used as a sub-expression
                // (not a statement/tail, where `emit_expr_stmt` would have
                // handled it) ends in an unconditional `return` -- a
                // pathological, unlikely-to-occur case (there's no
                // meaningful value to produce here regardless) that this
                // simply treats as `Void`.
                BlockOutcome::Diverged => vec![],
            },

            CheckedExpr::If(CheckedIf { branches, else_branch }) => {
                let result_leaves = node.r#type.clone().into_ir_type(self);
                match self.emit_if(builder, branches.into_iter(), else_branch, &result_leaves) {
                    BlockOutcome::Value(values) => values,
                    BlockOutcome::Diverged => vec![],
                }
            }

            CheckedExpr::ArrayLiteral(CheckedArrayLiteral { elements, .. }) => {
                // Each element contributes its own leaves, in order -- the
                // exact flattening `ResolvedType::SizedArray`'s `into_ir_type`
                // expects, so the result is usable anywhere a `SizedArray`
                // value already is (assignment, a walrus's inferred value, ...).
                elements.into_iter().flat_map(|e| self.process_expr(builder, e)).collect()
            }

            CheckedExpr::StructLiteral(CheckedStructLiteral { fields }) => {
                // Values are evaluated in the order the user wrote them
                // (their side effects must run in source order), but the
                // result's leaves are concatenated in *declared field*
                // order -- the exact flattening `ResolvedType::Struct`'s
                // `into_ir_type` expects, so the result is usable anywhere
                // a struct value already is. The checked module guarantees
                // every declared field appears exactly once.
                let ResolvedType::Struct(struct_type) = &node.r#type else {
                    unreachable!("checked module guarantees a struct literal's own type is a struct");
                };
                let field_count = struct_type.borrow().fields.len();
                let mut per_field: Vec<Option<Vec<Value>>> = vec![None; field_count];
                for field in fields {
                    per_field[field.field_index] = Some(self.process_expr(builder, field.value));
                }
                per_field
                    .into_iter()
                    .map(|leaves| leaves.expect("checked module guarantees every field is initialized"))
                    .flatten()
                    .collect()
            }

            CheckedExpr::Slice(CheckedSlice { base, item_type, start, end }) => {
                let (storage, base_type) = self.resolve_place_storage(&base, builder);
                let ptr_type = self.pointer_type();

                // A slice's data pointer and full length, however `base` is
                // actually stored: a `SizedArray`'s elements live inline, so
                // the pointer is the storage's own address and the length is
                // a compile-time constant; a `Slice` already carries both as
                // its two flattened leaves.
                let (data_ptr, full_len) = match &base_type {
                    ResolvedType::SizedArray(_, size) => {
                        let ptr = self.place_storage_address(builder, &storage);
                        let len = builder.ins().iconst(types::I32, *size as i64);
                        (ptr, len)
                    }
                    ResolvedType::Slice(_) => {
                        let leaves = self.load_scalars(builder, &storage, &base_type);
                        (leaves[0], leaves[1])
                    }
                    _ => unreachable!("checked module guarantees a slice's base is SizedArray or Slice"),
                };

                let elem_size = total_bytes(item_type, self) as i64;

                let start_val = match start {
                    Some(e) => self.process_expr(builder, *e)[0],
                    None => builder.ins().iconst(types::I32, 0),
                };
                let end_val = match end {
                    Some(e) => self.process_expr(builder, *e)[0],
                    None => full_len,
                };

                let mut start_ext = start_val;
                if builder.func.dfg.value_type(start_ext) != ptr_type {
                    start_ext = builder.ins().uextend(ptr_type, start_ext);
                }
                let elem_size_val = builder.ins().iconst(ptr_type, elem_size);
                let byte_offset = builder.ins().imul(start_ext, elem_size_val);
                let new_ptr = builder.ins().iadd(data_ptr, byte_offset);
                let new_len = builder.ins().isub(end_val, start_val);

                vec![new_ptr, new_len]
            }
        }
    }

    fn process_decl(&mut self, builder: &mut FunctionBuilder, decl: CheckedDeclaration) {
        let size = total_bytes(decl.r#type, self);
        // `StackSlotData::new`'s third parameter is an alignment *shift*
        // (alignment = 2^align_shift), not a byte count -- `4` means
        // 16-byte-aligned, which is what was actually intended here. This
        // was previously (harmlessly, by luck) passing `16` directly,
        // requesting a 65536-byte alignment per slot; with few enough
        // locals the resulting bloated stack frame still happened to work,
        // but it's a real bug that surfaces once a function has enough
        // locals (see the regression this fixes: a large function's
        // earlier float locals started reading back as zero once later
        // additions pushed the slot count high enough).
        let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, 4));
        self.stack_slots.insert(decl.id, slot);
    }

    /// Returns `true` if this statement unconditionally diverged (jumped to
    /// the shared `return_block`) -- the current cranelift block is then
    /// already terminated, and the caller (`emit_block`'s loop) must not
    /// process any further statements against it.
    fn process_statement(&mut self, builder: &mut FunctionBuilder, stmt: CheckedStmt) -> bool {
        match stmt {
            CheckedStmt::Expression(expr) => matches!(self.emit_expr_stmt(builder, expr), BlockOutcome::Diverged),
            CheckedStmt::Return(expr) => {
                let retval = self.process_expr(builder, expr);
                let return_block = self.return_block.expect("set at the start of every function body");
                builder.ins().jump(return_block, &block_args(&retval));
                true
            }
            CheckedStmt::Declaration(decl) => {
                self.process_decl(builder, decl);
                false
            }
            CheckedStmt::Struct(_) => false, // Only analysis is necessary
            CheckedStmt::ExternDeclaration(_) => {
                todo!("extern declarations inside a function body are not yet implemented");
            }
            CheckedStmt::While(CheckedWhile { id, condition, body, .. }) => {
                self.emit_while(builder, id, condition, body);
                false
            }
            CheckedStmt::For(for_loop) => {
                let CheckedFor { id, init, condition, post, body, .. } = *for_loop;
                self.emit_for(builder, id, init, condition, post, body);
                false
            }
            CheckedStmt::Break(CheckedBreak { loop_id, .. }) => {
                let target = self.loop_target(loop_id).break_blk;
                builder.ins().jump(target, &[]);
                true
            }
            CheckedStmt::Continue(CheckedContinue { loop_id, .. }) => {
                let target = self.loop_target(loop_id).continue_blk;
                builder.ins().jump(target, &[]);
                true
            }
            CheckedStmt::Defer(CheckedDefer { id, body, .. }) => {
                let slot = *self
                    .defer_flags
                    .get(&id)
                    .expect("every defer's flag is allocated by collect_defer_ids before the body is walked");
                let true_val = builder.ins().iconst(types::I8, 1);
                builder.ins().stack_store(true_val, slot, 0);
                self.defer_bodies.insert(id, body);
                false
            }
        }
    }

    /// Looks up `loop_id`'s targets by identity rather than assuming
    /// `.last()` -- see `Codegen::loop_stack`'s doc comment for why.
    fn loop_target(&self, loop_id: HirId) -> LoopTargets {
        self.loop_stack
            .iter()
            .rev()
            .find(|(id, _)| *id == loop_id)
            .map(|(_, targets)| *targets)
            .expect("checked module guarantees a break/continue's loop_id is a currently-enclosing loop")
    }

    fn demangle(symbol: &str) -> String {
        if !symbol.contains("::") {
            return symbol.to_owned();
        }

        format!("___omg_{}", symbol.replace("::", "_"))
    }

    /// A `(path, name)` pair alone no longer uniquely identifies a compiled
    /// function: two different instantiations of the same generic function/
    /// method share both, but must never share a link-time symbol. `id` is
    /// a real per-file `HirId` for an ordinary (non-generic) function --
    /// nothing to add there -- but a generic instantiation's `id` is a
    /// synthetic one (`omega_hir::SYNTHETIC_MODULE`, minted once per
    /// instantiation by `omega_driver::Driver::compute_item`), so appending
    /// its already-unique `local` counter as a suffix is guaranteed
    /// collision-free and a complete no-op for every existing non-generic
    /// symbol.
    fn instantiation_suffix(id: HirId) -> String {
        if id.module == omega_hir::SYNTHETIC_MODULE { format!("$${}", id.local) } else { String::new() }
    }

    /// A top-level function's mangled symbol -- its full module path plus its
    /// own name, except the program's literal entry point (`main`, in the
    /// `entry` module), which must keep the bare unmangled symbol `"main"`
    /// the OS/linker looks for. Every *other* function, including other
    /// entry-module functions, gets module-path-qualified: these mangled
    /// names are entirely internal/opaque (never typed by a human, never an
    /// external symbol), so changing them from today's single-module scheme
    /// has no observable effect on program behavior.
    fn mangled_symbol(path: &[Ident], entry: &[Ident], name: &Ident, id: HirId) -> String {
        if path == entry && name.as_ref() == "main" {
            return "main".to_string();
        }
        format!(
            "{}::{}{}",
            path.iter().map(Ident::as_ref).collect::<Vec<_>>().join("::"),
            name.as_ref(),
            Self::instantiation_suffix(id)
        )
    }

    /// A struct method's mangled symbol -- same reasoning as
    /// `mangled_symbol`, but a method is never itself the program's entry
    /// point, so there's no bare-symbol exception to check for here.
    fn mangled_method_symbol(path: &[Ident], struct_name: &Ident, method_name: &Ident, id: HirId) -> String {
        format!(
            "{}::{}::{}{}",
            path.iter().map(Ident::as_ref).collect::<Vec<_>>().join("::"),
            struct_name.as_ref(),
            method_name.as_ref(),
            Self::instantiation_suffix(id)
        )
    }

    /// A function/method's cranelift `Signature`, built the same way
    /// regardless of whether it's being declared (pass 1) or defined (pass
    /// 2) -- a pure function of `function_def`'s own checked shape, so
    /// recomputing it in both passes (rather than threading it through) is
    /// cheap and keeps the two passes independent of each other.
    fn function_signature(&self, function_def: &CheckedFunctionDef) -> Signature {
        let fntype = function_def.fn_type();
        let mut sig = self.module.make_signature();
        if *fntype.return_type != ResolvedType::Void {
            let return_type = fntype.return_type.clone().into_ir_type(self);
            return_type
                .into_iter()
                .for_each(|param| sig.returns.push(AbiParam::new(param)));
        }
        for param in &function_def.params {
            // Every leaf, not just the first: a struct/slice passed by value
            // flattens to several scalar params -- the same flattening call
            // sites (`make_function_sig`) and the entry block's `argmap`
            // already use, which all three must agree on.
            for leaf in param.r#type.clone().into_ir_type(self) {
                sig.params.push(AbiParam::new(leaf));
            }
        }
        sig
    }

    /// Declares (but doesn't yet define the body of) a function or method --
    /// signature/symbol registration only, split out from what used to be
    /// one `update_function_def` specifically so *every* function across
    /// *every* compiled module can be declared (and therefore have a
    /// `FuncId` any other module's body can already look up) before *any*
    /// body starts being built. Without this split, a cross-module call in
    /// either import direction would panic the first time one module's body
    /// needed another module's not-yet-declared `FuncId` (see the plan's
    /// "codegen declare/define split" note).
    fn declare_function_def(&mut self, function_def: &CheckedFunctionDef, mangled_symbol: String) -> FuncId {
        let sig = self.function_signature(function_def);
        let demangled_symbol = Self::demangle(&mangled_symbol);

        let function_id = self
            .module
            .declare_function(&demangled_symbol, Linkage::Import, &sig)
            .unwrap();

        self.module
            .declare_function(&demangled_symbol, Linkage::Export, &sig)
            .unwrap();

        self.functions.insert(function_def.id, function_id);
        function_id
    }

    /// Builds a function/method's body -- everything `update_function_def`
    /// used to do after declaring, now looking up the `FuncId` every item
    /// across every module already got in the declare pass, rather than
    /// declaring (and re-registering) it itself.
    fn define_function_def(&mut self, function_def: CheckedFunctionDef) {
        let function_id = *self
            .functions
            .get(&function_def.id)
            .expect("declared for every item, across every module, before any body is defined");
        let sig = self.function_signature(&function_def);
        let fntype = function_def.fn_type();

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

        // One 1-byte flag per `defer` in this function, allocated and
        // zero-initialized here -- unconditionally, before the body is
        // walked for real -- so a path that never reaches a given `defer`
        // reads back `false` in the epilogue rather than uninitialized
        // stack memory. See `collect_defer_ids`'s doc comment for why this
        // has to be a full up-front pre-pass rather than lazy allocation at
        // each defer's own position.
        let mut defer_order = Vec::new();
        collect_defer_ids(&function_def.body, &mut defer_order);
        for &id in &defer_order {
            let slot = builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 1, 0));
            let false_val = builder.ins().iconst(types::I8, 0);
            builder.ins().stack_store(false_val, slot, 0);
            self.defer_flags.insert(id, slot);
        }

        // Every `return` in this function body, however deeply nested
        // inside `if`/`while`/`for`, jumps here instead of emitting its own
        // `return_` -- this is the only block that actually does, once
        // every path through the body has either jumped here or (falling
        // off the end normally) is about to, right below. See
        // `Codegen::return_block`/`BlockOutcome`'s doc comments.
        let return_leaves = fntype.return_type.clone().into_ir_type(self);
        let return_block = builder.create_block();
        for ty in &return_leaves {
            builder.append_block_param(return_block, *ty);
        }
        self.return_block = Some(return_block);

        if let BlockOutcome::Value(values) = self.emit_block(&mut builder, function_def.body) {
            builder.ins().jump(return_block, &block_args(&values));
        }

        builder.switch_to_block(return_block);
        builder.seal_block(return_block);
        let final_values = builder.block_params(return_block).to_vec();

        // Deferred cleanup, in reverse declaration order (FILO -- a
        // later-declared resource may depend on an earlier one, so it's
        // torn down first), checked once here rather than duplicated at
        // every `return`/fallthrough site: every exit already funnels
        // through this one shared `return_block` above, so this is the one
        // place that needs to run regardless of which path got here. Each
        // defer's flag (`false` unless its own statement actually executed
        // -- see `process_statement`'s `Defer` arm) makes this correct with
        // no reachability analysis: a defer whose statement never ran along
        // the path that reached this exit is simply a no-op check here.
        for id in defer_order.iter().rev() {
            let slot = self.defer_flags[id];
            let body = self
                .defer_bodies
                .remove(id)
                .expect("every collected defer is visited unconditionally during the compile-time walk above");
            let flag = builder.ins().stack_load(types::I8, slot, 0);
            let run_blk = builder.create_block();
            let after_blk = builder.create_block();
            builder.ins().brif(flag, run_blk, &[], after_blk, &[]);

            builder.switch_to_block(run_blk);
            builder.seal_block(run_blk);
            let outcome = self.emit_block(&mut builder, body);
            assert!(
                matches!(outcome, BlockOutcome::Value(_)),
                "a defer body can never diverge -- analysis rejects return/break/continue inside one"
            );
            builder.ins().jump(after_blk, &[]);

            builder.switch_to_block(after_blk);
            builder.seal_block(after_blk);
        }

        builder.ins().return_(&final_values);

        if let Err(err) = codegen::verify_function(builder.func, self.isa.as_ref()) {
            panic!("cranelift verifier rejected generated IR for a function (internal codegen bug): {err:?}");
        }

        builder.seal_block(entry_block);
        builder.finalize();

        self.module.define_function(function_id, &mut ctx).unwrap();
        self.ctx = ctx;

        self.clear_local();
    }

    /// Declares every function/method/extern in one item -- pass 1 of 2 (see
    /// `update_all`).
    fn declare_item(&mut self, item: &CheckedItem, path: &[Ident], entry: &[Ident]) {
        match item {
            // Externs have no body to split across two passes -- fully
            // handled here, in one shot.
            CheckedItem::ExternDeclaration(extern_decl) => self.update_extern_decl(extern_decl.clone()),
            CheckedItem::FunctionDefinition(f) => {
                let mangled = Self::mangled_symbol(path, entry, &f.name, f.id);
                self.declare_function_def(f, mangled);
            }
            CheckedItem::Struct(s) => {
                for f in &s.functions {
                    let mangled = Self::mangled_method_symbol(path, &s.name, &f.name, f.id);
                    self.declare_function_def(f, mangled);
                }
            }
            CheckedItem::Declaration(_) => {
                todo!("global data declarations are not yet implemented");
            }
        }
    }

    /// Defines every function/method body in one item -- pass 2 of 2, run
    /// only after every item across every module has already been declared.
    fn define_item(&mut self, item: CheckedItem) {
        match item {
            // Already fully handled by `declare_item` -- an extern has no
            // body to define.
            CheckedItem::ExternDeclaration(_) => {}
            CheckedItem::FunctionDefinition(f) => self.define_function_def(f),
            CheckedItem::Struct(s) => {
                for f in s.functions {
                    self.define_function_def(f);
                }
            }
            CheckedItem::Declaration(_) => {
                todo!("global data declarations are not yet implemented");
            }
        }
    }

    /// Two full passes over every item across every compiled module: first
    /// declare everything (so any `FuncId` a cross-module call needs already
    /// exists, regardless of import direction -- see `declare_function_def`'s
    /// doc comment), then define every body. Mirrors the identical
    /// signature/body split `omega_analyzer::Analyzer` does for the same
    /// underlying reason.
    fn update_all(&mut self, modules: Vec<(Vec<Ident>, CheckedModule)>, entry: &[Ident]) {
        for (path, checked) in &modules {
            for item in &checked.items {
                self.declare_item(item, path, entry);
            }
        }
        for (_, checked) in modules {
            for item in checked.items {
                self.define_item(item);
            }
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
