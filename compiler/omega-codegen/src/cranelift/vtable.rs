//! Building a `spec *Spec` dynamic-dispatch vtable for one `(concrete
//! type, spec)` pair -- see `Codegen::vtable_for`'s own doc comment.

use super::Codegen;
use crate::mangle;
use cranelift_module::{DataDescription, DataId, Linkage, Module};
use omega_analyzer::resolved_type::{ResolvedSpecType, ResolvedType};
use omega_hir::HirId;
use omega_parser::prelude::Ident;
use std::cell::RefCell;
use std::rc::Rc;

/// `concrete`'s own declaring `HirId` -- the identity half of a vtable's
/// `(concrete type, spec)` cache key. Every spec-object coercion's pointee
/// is always a struct/enum/union (see `Analyzer::coerce_to_expected`) --
/// nothing else ever implements a spec.
pub(super) fn concrete_type_id(concrete: &ResolvedType) -> HirId {
    match concrete {
        ResolvedType::Struct(cell) => cell.borrow().id,
        ResolvedType::Enum { cell, .. } => cell.borrow().id,
        ResolvedType::Union(cell) => cell.borrow().id,
        other => unreachable!("a spec-object coercion's concrete pointee is always struct/enum/union, found {other}"),
    }
}

/// `concrete`'s own name, plus every method it already carries (name +
/// declaring `HirId`, including a spec-default instantiation -- by the
/// time codegen runs, one is indistinguishable from an ordinary override;
/// see `Analyzer::resolve_implements_clause`) -- everything `vtable_for`
/// needs to resolve each of the spec's flattened slot names to a concrete
/// `FuncId`.
fn concrete_type_name_and_functions(concrete: &ResolvedType) -> (Ident, Vec<(Ident, HirId)>) {
    let functions = |fs: &[(Ident, omega_analyzer::resolved_type::ResolvedMethod)]| {
        fs.iter().map(|(name, m)| (name.clone(), m.decl_id)).collect()
    };
    match concrete {
        ResolvedType::Struct(cell) => {
            let cell = cell.borrow();
            (cell.name.clone(), functions(&cell.functions))
        }
        ResolvedType::Enum { cell, .. } => {
            let cell = cell.borrow();
            (cell.name.clone(), functions(&cell.functions))
        }
        ResolvedType::Union(cell) => {
            let cell = cell.borrow();
            (cell.name.clone(), functions(&cell.functions))
        }
        other => unreachable!("a spec-object coercion's concrete pointee is always struct/enum/union, found {other}"),
    }
}

/// `spec`'s full, ordered, deduplicated slot-name list -- dependencies (in
/// declaration order) before `spec`'s own functions, first-seen name wins
/// -- structurally identical to (and must stay in lockstep with)
/// `Analyzer::flatten_spec`'s own walk, which is what decided
/// `MirDynamicCall::slot_index` for every call through this same spec.
/// Unlike `flatten_spec`, this never needs to resolve a raw signature or
/// detect a conflict: by the time codegen runs, the program already
/// passed analysis, so every name collision here is already
/// known-identical.
fn flatten_spec_slot_names(spec: &Rc<RefCell<ResolvedSpecType>>) -> Vec<Ident> {
    let mut out = Vec::new();
    flatten_spec_slot_names_into(spec, &mut out);
    out
}

fn flatten_spec_slot_names_into(spec: &Rc<RefCell<ResolvedSpecType>>, out: &mut Vec<Ident>) {
    let spec = spec.borrow();
    for (dependency, _) in &spec.dependencies {
        flatten_spec_slot_names_into(dependency, out);
    }
    for (name, _) in &spec.functions {
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
}

impl Codegen {
    /// Lazily builds (and memoizes) the vtable data object for `(concrete,
    /// spec)` -- a compiler-generated, module-level array of function
    /// pointers, one per slot in `flatten_spec_slot_names`'s order, each
    /// pointing at `concrete`'s own already-declared method for that name
    /// (mirrors a `Str`/`Slice` constant's static-data-with-relocations
    /// shape, just relocating to function symbols via
    /// `declare_func_in_data`/`write_function_addr` instead of
    /// `declare_data_in_data`/`write_data_addr`). `concrete`'s methods are
    /// guaranteed already `declare_item`'d (never yet *defined* -- codegen
    /// visits every item's declarations before any body, see
    /// `declare_item`'s own doc comment) by the time any expression
    /// (necessarily inside some function body) could coerce it, so
    /// `self.functions` always already has every `FuncId` this needs.
    pub(super) fn vtable_for(&mut self, concrete: &ResolvedType, spec: &Rc<RefCell<ResolvedSpecType>>) -> DataId {
        let key = (concrete_type_id(concrete), spec.borrow().id);
        if let Some(&id) = self.vtables.get(&key) {
            return id;
        }

        let slot_names = flatten_spec_slot_names(spec);
        let (concrete_name, concrete_functions) = concrete_type_name_and_functions(concrete);

        let ptr_bytes = self.pointer_type().bytes();
        let bytes = vec![0u8; slot_names.len() * ptr_bytes as usize];
        let mut desc = DataDescription::new();
        for (i, name) in slot_names.iter().enumerate() {
            let decl_id = concrete_functions
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| {
                    panic!("mir body guarantees '{concrete_name}' provides '{name}' (required by spec '{}')", spec.borrow().name.as_ref())
                });
            let func_id = *self.functions.get(&decl_id).expect("every method is declared before any vtable needs it");
            let fref = self.module.declare_func_in_data(func_id, &mut desc);
            desc.write_function_addr(i as u32 * ptr_bytes, fref);
        }
        desc.define(bytes.into_boxed_slice());

        // `Preemptible` (weak), not `Local`, for the same reason a generic
        // instantiation's own symbol is (see `linkage_for`): a vtable's
        // content is a pure function of `(concrete, spec)` -- relocations
        // to method symbols whose own names are themselves stable, in a
        // slot order `flatten_spec_slot_names` derives deterministically
        // from the spec's own declaration -- so two separate compilations
        // that both coerce the same concrete type to the same spec are
        // guaranteed to build byte-identical vtables under the identical
        // name, and are just as safe (and worth) folding into one copy at
        // link time as a generic function/method instantiation is.
        let symbol = mangle::encode(&mangle::vtable_symbol(concrete, &spec.borrow().name));
        let data_id = self.module.declare_data(&symbol, Linkage::Preemptible, false, false).unwrap();
        self.module.define_data(data_id, &desc).unwrap();

        self.vtables.insert(key, data_id);
        data_id
    }
}
