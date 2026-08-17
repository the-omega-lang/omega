//! The calling convention -- the *one* home for the ABI facts every
//! backend must agree on, so a `core.o` built with one backend always
//! links against a `main.o` built with the other (which `justfile`'s
//! recipes do as a matter of course).
//!
//! This crate (or the MIR) used to leave these decisions to the backend:
//! the Cranelift backend re-derived sret-vs-registers, parameter
//! flattening, and C variadic promotion from `ResolvedFunctionType` on
//! its own, and a second backend left to do the same could silently
//! disagree -- disagreement here is a mislink. Now both backends consume
//! [`AbiSignature`], built once from `(Target, ResolvedFunctionType)`.
//!
//! The convention itself is *mirrored, not fixed*: it is x86_64-shaped
//! (see [`AbiReturn`]'s doc comment), internally consistent for
//! Omega-to-Omega calls on every target, and **not** the platform C ABI.
//! That is recorded debt, not a bug to fix here -- see
//! `docs/14-known-issues.md`'s "Design debt worth watching", and the
//! aggregate-across-`extern` rejection in `omega_driver` that keeps the
//! C boundary honest until the real C ABI lands.

use omega_analyzer::layout::{self, Leaf};
use omega_analyzer::resolved_type::{NumericKind, ResolvedFunctionType, ResolvedType};
use omega_analyzer::Target;

/// How a call's result comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiReturn {
    /// No return value at all (`void`) -- `never` takes this same shape:
    /// nothing ever reads a call's result in a position typed `never`
    /// (the callee doesn't return at all, so there's no return-value ABI
    /// to negotiate).
    Void,
    /// The flattened return leaves, in registers.
    Direct(Vec<Leaf>),
    /// Returned through a hidden struct-return pointer (the caller
    /// allocates the slot and passes its address as an implicit first
    /// parameter) -- x86_64 SysV has exactly two integer return registers
    /// (rax/rdx), so any value flattening to more than two leaves can't
    /// come back by value. (Two int + two float leaves would technically
    /// still fit, but classifying leaf register classes buys nothing over
    /// this simple, always-correct rule.) This threshold is an x86_64 fact
    /// currently applied to every arch -- see the module doc comment.
    Indirect,
}

/// The full calling-convention shape of one function type: every
/// parameter, flattened to scalar leaves, plus the return convention.
/// Built once, consumed by every backend -- definitions, extern
/// declarations, and call sites all read the same signature, so they can
/// never disagree about parameter flattening or the hidden struct-return
/// pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiSignature {
    pub params: Vec<Leaf>,
    pub ret: AbiReturn,
}

impl AbiSignature {
    /// The one builder: `(Target, ResolvedFunctionType)` is everything the
    /// convention needs to know.
    pub fn build(target: Target, fn_type: &ResolvedFunctionType) -> AbiSignature {
        AbiSignature {
            params: fn_type
                .params
                .iter()
                .flat_map(|(_, ty)| layout::leaves_of(ty, target.pointer_bytes()))
                .collect(),
            ret: AbiReturn::for_type(target, &fn_type.return_type),
        }
    }
}

impl AbiReturn {
    /// The return convention for one type on its own -- `needs_sret`'s
    /// answer, in the same vocabulary the full signature uses.
    pub fn for_type(target: Target, return_type: &ResolvedType) -> AbiReturn {
        if *return_type == ResolvedType::Void || *return_type == ResolvedType::Never {
            return AbiReturn::Void;
        }
        let leaves = layout::leaves_of(return_type, target.pointer_bytes());
        if leaves.len() > 2 {
            AbiReturn::Indirect
        } else {
            AbiReturn::Direct(leaves)
        }
    }
}

/// The C default-argument-promotion a *variadic* call applies to one
/// argument: floats below `f64` promote to `f64`, integer types below
/// 32 bits promote to `i32` (signed) / `u32` (unsigned), and `bool`
/// promotes to `u32`. Returns the numeric kind the argument must be
/// *presented* as; `None` means "pass unchanged". The *decision* is a C
/// ABI rule (shared, once); *emitting* the conversion is each backend's
/// own business.
pub fn variadic_promotion(ty: &ResolvedType, target: Target) -> Option<NumericKind> {
    match ty.numeric_kind(target.pointer_bits()) {
        Some(NumericKind::Float(width)) if width < 64 => Some(NumericKind::Float(64)),
        Some(NumericKind::Signed(width)) if width < 32 => Some(NumericKind::Signed(32)),
        Some(NumericKind::Unsigned(width)) if width < 32 => Some(NumericKind::Unsigned(32)),
        // `Bool` isn't `numeric_kind`-classified (see its doc comment),
        // but it's still an 8-bit integer that needs the same promotion.
        _ if *ty == ResolvedType::Bool => Some(NumericKind::Unsigned(32)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_analyzer::resolved_type::ResolvedFunctionType;

    fn fn_type(params: &[ResolvedType], ret: ResolvedType) -> ResolvedFunctionType {
        ResolvedFunctionType {
            params: params
                .iter()
                .enumerate()
                .map(|(i, t)| (omega_parser::prelude::Ident(format!("p{i}")), t.clone()))
                .collect(),
            return_type: Box::new(ret),
            is_variadic: false,
            self_mode: None,
        }
    }

    #[test]
    fn void_and_never_have_no_return_value() {
        for ret in [ResolvedType::Void, ResolvedType::Never] {
            assert_eq!(
                AbiSignature::build(Target::DEFAULT, &fn_type(&[], ret)).ret,
                AbiReturn::Void
            );
        }
    }

    #[test]
    fn one_and_two_leaves_return_directly() {
        assert_eq!(
            AbiSignature::build(Target::DEFAULT, &fn_type(&[], ResolvedType::I32)).ret,
            AbiReturn::Direct(vec![Leaf::I32])
        );
        assert_eq!(
            AbiReturn::for_type(Target::DEFAULT, &ResolvedType::Slice { item: Box::new(ResolvedType::U8), mutable: false }),
            AbiReturn::Direct(vec![Leaf::Ptr, Leaf::I32])
        );
    }

    #[test]
    fn three_leaves_return_indirectly() {
        let ret = ResolvedType::SizedArray(Box::new(ResolvedType::I64), 3);
        assert_eq!(AbiReturn::for_type(Target::DEFAULT, &ret), AbiReturn::Indirect);
        assert_eq!(
            AbiSignature::build(Target::DEFAULT, &fn_type(&[], ret)).ret,
            AbiReturn::Indirect
        );
    }

    #[test]
    fn parameters_flatten_to_leaves() {
        let params = vec![
            ResolvedType::I8,
            ResolvedType::Slice { item: Box::new(ResolvedType::U8), mutable: false },
            ResolvedType::I64,
        ];
        assert_eq!(
            AbiSignature::build(Target::DEFAULT, &fn_type(&params, ResolvedType::Void)).params,
            vec![Leaf::I8, Leaf::Ptr, Leaf::I32, Leaf::I64]
        );
    }

    #[test]
    fn variadic_promotion_is_a_c_abi_rule() {
        let t = Target::DEFAULT;
        assert_eq!(variadic_promotion(&ResolvedType::U8, t), Some(NumericKind::Unsigned(32)));
        assert_eq!(variadic_promotion(&ResolvedType::I16, t), Some(NumericKind::Signed(32)));
        assert_eq!(variadic_promotion(&ResolvedType::F32, t), Some(NumericKind::Float(64)));
        assert_eq!(variadic_promotion(&ResolvedType::Bool, t), Some(NumericKind::Unsigned(32)));
        assert_eq!(variadic_promotion(&ResolvedType::I32, t), None);
        assert_eq!(variadic_promotion(&ResolvedType::F64, t), None);
        assert_eq!(variadic_promotion(&ResolvedType::I64, t), None);
    }
}
