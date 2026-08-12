//! Building a `spec *Spec` dynamic-dispatch vtable -- see `Codegen::
//! vtable_for`'s own doc comment.

use super::Codegen;
use crate::mangle;
use cranelift_module::{DataDescription, DataId, Linkage, Module};
use omega_analyzer::resolved_type::{ResolvedSpecType, ResolvedType};
use omega_hir::HirId;
use std::cell::RefCell;
use std::rc::Rc;

impl Codegen {
    /// Lazily builds (and memoizes) the vtable data object for `slots` -- a
    /// compiler-generated, module-level array of function pointers, one per
    /// entry of `slots` in order, each pointing at that entry's own
    /// already-declared method (mirrors a `Str`/`Slice` constant's
    /// static-data-with-relocations shape, just relocating to function
    /// symbols via `declare_func_in_data`/`write_function_addr` instead of
    /// `declare_data_in_data`/`write_data_addr`).
    ///
    /// `slots` -- one concrete method's `decl_id` per vtable slot, already
    /// fully resolved by `Analyzer::type_implements_spec` (see
    /// `MirSpecCoerce::slots`'s doc comment) -- is both the cache key and
    /// the vtable's entire content: unlike an earlier version of this
    /// function, nothing here re-derives *which* concrete method satisfies
    /// a given slot by matching names. That used to be sound (`by the time
    /// codegen runs, every name collision is already known-identical`), but
    /// stopped being true once conformance checking started
    /// allowing one implementor to satisfy the same generic spec at two
    /// different type arguments via two same-named overloads -- codegen has
    /// no way to tell those apart by name alone, so it no longer tries to;
    /// it just plays back the answer analysis already worked out. Keying
    /// the cache on `slots` itself (rather than `(concrete, spec)`) is also
    /// strictly more precise: two coercions that happen to resolve to the
    /// exact same ordered method list produce byte-identical vtables no
    /// matter which concrete type or spec they came from, so sharing one
    /// copy is always correct, not just when the identity happens to match
    /// too.
    ///
    /// `concrete`/`spec`/`spec_type_args` are only needed for the vtable's
    /// own linker *symbol* -- unlike `slots`' `HirId`s, which are only
    /// meaningful within this one compilation, the symbol name must be a
    /// pure function of stable, cross-translation-unit-meaningful identity
    /// (the concrete type's name, the spec's name, its type arguments) so
    /// two separate compilations that coerce the same concrete type to the
    /// same spec instantiation agree on one linkable symbol -- see
    /// `mangle::vtable_symbol`. `concrete`'s methods are guaranteed already
    /// `declare_item`'d (never yet *defined* -- codegen visits every item's
    /// declarations before any body, see `declare_item`'s own doc comment)
    /// by the time any expression (necessarily inside some function body)
    /// could coerce it, so `self.functions` always already has every
    /// `FuncId` this needs.
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

        // `Preemptible` (weak), not `Local`, for the same reason a generic
        // instantiation's own symbol is (see `linkage_for`): a vtable's
        // content is a pure function of `slots`, which is itself a pure,
        // deterministic function of `(concrete, spec, spec_type_args)` (see
        // this method's own doc comment) -- so two separate compilations
        // that both coerce the same concrete type to the same spec
        // instantiation are guaranteed to build byte-identical vtables
        // under the identical symbol name, and are just as safe (and
        // worth) folding into one copy at link time as a generic
        // function/method instantiation is.
        let symbol = mangle::encode(&mangle::vtable_symbol(
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
