
use omega_analyzer::layout::{self, Leaf};
use omega_analyzer::resolved_type::{NumericKind, ResolvedFunctionType, ResolvedType};
use omega_analyzer::Target;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiReturn {
    Void,
    Direct(Vec<Leaf>),
    Indirect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiSignature {
    pub params: Vec<Leaf>,
    pub ret: AbiReturn,
}

impl AbiSignature {
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

pub fn variadic_promotion(ty: &ResolvedType, target: Target) -> Option<NumericKind> {
    match ty.numeric_kind(target.pointer_bits()) {
        Some(NumericKind::Float(width)) if width < 64 => Some(NumericKind::Float(64)),
        Some(NumericKind::Signed(width)) if width < 32 => Some(NumericKind::Signed(32)),
        Some(NumericKind::Unsigned(width)) if width < 32 => Some(NumericKind::Unsigned(32)),
        // `bool` still participates in the integer-width ABI path despite not being a numeric source type.
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
