//! Builds `omega_mangle::Symbol`s from whatever `Codegen` already has on
//! hand (a `CheckedFunctionDef`/`CheckedStructDef`/... or an
//! `ExternFunctionRef`) and hands them to `omega_mangle::encode`. This is
//! the *only* place in the compiler that constructs an `omega_mangle`
//! type -- `omega_mangle` itself knows nothing about `ResolvedType`.
//!
//! `self` never needs special handling here: `HirFunctionDef::lower`
//! already prepends the self parameter to `params` before anything else
//! sees it (`omega-hir/src/lower.rs`'s `lower_function_def`), with its
//! resolved type already reflecting the declared self-mode (a plain
//! `Named` for by-value, a `Pointer` for by-pointer) -- so mapping
//! `fn_type.params` uniformly, in order, already spells out self's mode
//! exactly like any other parameter.

use omega_analyzer::resolved_type::{ResolvedFunctionType, ResolvedType};
use omega_mangle::{ManglePath, MangleType, Namespace, Symbol};
use omega_parser::prelude::Ident;

fn mangle_module_path(segments: &[Ident]) -> ManglePath {
    let mut iter = segments.iter();
    let first = iter.next().expect("a module path is never empty");
    let mut path = ManglePath::Root(first.as_ref().to_string());
    for seg in iter {
        // Intermediate module segments get an arbitrary, fixed namespace
        // tag (`Type`) -- there's no real Omega namespace a module itself
        // belongs to; this just needs to be consistent between encode and
        // decode, which it is (every path segment always carries some
        // namespace tag, mirroring RFC 2603's own uniform treatment).
        path = ManglePath::Nested(Box::new(path), Namespace::Type, seg.as_ref().to_string());
    }
    path
}

/// The path to a type-namespace item (struct/enum/union/spec): its
/// module path, its own name, and -- if it's a generic instantiation --
/// its concrete type arguments.
fn mangle_type_path(module_path: &[Ident], name: &Ident, type_args: &[ResolvedType]) -> ManglePath {
    let base = ManglePath::Nested(Box::new(mangle_module_path(module_path)), Namespace::Type, name.as_ref().to_string());
    if type_args.is_empty() { base } else { ManglePath::Generic(Box::new(base), type_args.iter().map(mangle_type).collect()) }
}

fn mangle_type(ty: &ResolvedType) -> MangleType {
    match ty {
        ResolvedType::Void => MangleType::Void,
        ResolvedType::Bool => MangleType::Bool,
        ResolvedType::Char => MangleType::Char,
        ResolvedType::I8 => MangleType::I8,
        ResolvedType::I16 => MangleType::I16,
        ResolvedType::I32 => MangleType::I32,
        ResolvedType::I64 => MangleType::I64,
        ResolvedType::ISize => MangleType::ISize,
        ResolvedType::U8 => MangleType::U8,
        ResolvedType::U16 => MangleType::U16,
        ResolvedType::U32 => MangleType::U32,
        ResolvedType::U64 => MangleType::U64,
        ResolvedType::USize => MangleType::USize,
        ResolvedType::F32 => MangleType::F32,
        ResolvedType::F64 => MangleType::F64,
        ResolvedType::Pointer { pointee, mutable } => MangleType::Pointer(Box::new(mangle_type(pointee)), *mutable),
        ResolvedType::Slice { item, mutable } => MangleType::Slice(Box::new(mangle_type(item)), *mutable),
        ResolvedType::Str { mutable } => MangleType::Str(*mutable),
        ResolvedType::Array(inner) => MangleType::Array(Box::new(mangle_type(inner))),
        ResolvedType::SizedArray(inner, len) => MangleType::SizedArray(Box::new(mangle_type(inner)), u64::from(*len)),
        ResolvedType::Function(fn_type) => {
            let (params, ret) = build_signature(fn_type);
            MangleType::Function(params, Box::new(ret), fn_type.is_variadic)
        }
        ResolvedType::Struct(cell) => {
            let cell = cell.borrow();
            MangleType::Named(mangle_type_path(&cell.module_path, &cell.name, &cell.type_args), None)
        }
        ResolvedType::Union(cell) => {
            let cell = cell.borrow();
            MangleType::Named(mangle_type_path(&cell.module_path, &cell.name, &cell.type_args), None)
        }
        ResolvedType::Enum { cell, variant } => {
            let cell = cell.borrow();
            let variant = variant.map(|v| v as u32);
            MangleType::Named(mangle_type_path(&cell.module_path, &cell.name, &cell.type_args), variant)
        }
        ResolvedType::Spec(cell) => {
            let cell = cell.borrow();
            MangleType::Named(mangle_type_path(&cell.module_path, &cell.name, &cell.type_args), None)
        }
        ResolvedType::SpecObject { spec, type_args, mutable } => {
            let cell = spec.borrow();
            let inner = MangleType::Named(mangle_type_path(&cell.module_path, &cell.name, type_args), None);
            MangleType::SpecObject(Box::new(inner), *mutable)
        }
    }
}

fn build_signature(fn_type: &ResolvedFunctionType) -> (Vec<MangleType>, MangleType) {
    (fn_type.params.iter().map(|(_, t)| mangle_type(t)).collect(), mangle_type(&fn_type.return_type))
}

/// A top-level function's symbol. The caller is responsible for the
/// program-entry-point special case (`main` in the entry module keeps
/// its bare, unmangled OS/linker symbol) -- that's a policy decision
/// about *which* symbol gets built at all, not something this module,
/// which only ever builds real `Symbol`s, needs to know about.
pub(crate) fn free_function_symbol(
    module_path: &[Ident],
    name: &Ident,
    type_args: &[ResolvedType],
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let leaf = ManglePath::Nested(Box::new(mangle_module_path(module_path)), Namespace::Value, name.as_ref().to_string());
    let path = if type_args.is_empty() { leaf } else { ManglePath::Generic(Box::new(leaf), type_args.iter().map(mangle_type).collect()) };
    let (params, ret) = build_signature(fn_type);
    Symbol { path, signature: Some((params, ret)), vendor_suffix: None }
}

/// A struct/enum/union method's symbol -- nested under its owner type's
/// own path (itself possibly generic), never a separate `impl`-block
/// root (Omega methods are declared directly on the type; see the
/// crate's design plan for why that means no `M`/`X`/`Y`-style
/// productions are needed at all, unlike RFC 2603).
pub(crate) fn method_symbol(
    module_path: &[Ident],
    owner_name: &Ident,
    owner_type_args: &[ResolvedType],
    method_name: &Ident,
    fn_type: &ResolvedFunctionType,
) -> Symbol {
    let owner = mangle_type_path(module_path, owner_name, owner_type_args);
    let path = ManglePath::Nested(Box::new(owner), Namespace::Value, method_name.as_ref().to_string());
    let (params, ret) = build_signature(fn_type);
    Symbol { path, signature: Some((params, ret)), vendor_suffix: None }
}

/// A `(concrete type, spec)` pair's vtable data symbol -- one concrete
/// type can carry a separate vtable per spec it's dynamically dispatched
/// through (see `Codegen::vtable_for`'s own `(concrete, spec)` cache
/// key), so both names need to appear, nested as
/// `<concrete>::<spec>::vtable`. That's an ordinary `vtable` identifier
/// in the value namespace nested under an ordinary spec-named
/// type-namespace segment -- there's no real "vtable" function to call
/// and no real "spec-implementation" type to name, but reusing the same
/// identifier/namespace machinery every other symbol already uses keeps
/// this strictly within `[A-Za-z0-9_]`. RFC 2603's own
/// `<vendor-specific-suffix>` production allows arbitrary bytes after a
/// literal `.`/`$`, which the RFC's own motivation section flags as a
/// real cross-platform portability problem in exactly this kind of
/// compiler-emitted, not merely tool-appended, position -- see
/// `omega_mangle::Symbol::vendor_suffix`'s doc comment -- so it's
/// deliberately not used here.
///
/// `concrete` is always a `Struct`/`Enum`/`Union` (a spec-object
/// coercion's pointee can never be anything else -- see
/// `Codegen::concrete_type_id`'s identical assumption).
pub(crate) fn vtable_symbol(concrete: &ResolvedType, spec_name: &Ident) -> Symbol {
    let MangleType::Named(concrete_path, _) = mangle_type(concrete) else {
        unreachable!("a spec-object coercion's concrete pointee is always struct/enum/union, which always mangles to MangleType::Named");
    };
    let with_spec = ManglePath::Nested(Box::new(concrete_path), Namespace::Type, spec_name.as_ref().to_string());
    let path = ManglePath::Nested(Box::new(with_spec), Namespace::Value, "vtable".to_string());
    Symbol { path, signature: None, vendor_suffix: None }
}

pub(crate) fn encode(symbol: &Symbol) -> String {
    omega_mangle::encode(symbol)
}
