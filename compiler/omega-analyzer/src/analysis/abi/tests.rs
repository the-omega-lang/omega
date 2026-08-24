use super::*;
use crate::analysis::tests::{NoResolver, analyzer, dummy_struct_type, id, sp};

fn fn_type(convention: CallingConvention, params: Vec<ResolvedType>) -> ResolvedFunctionType {
    ResolvedFunctionType {
        params: params
            .into_iter()
            .enumerate()
            .map(|(index, ty)| ResolvedFunctionParam::described(Ident(format!("p{index}")), ty))
            .collect(),
        return_type: Box::new(ResolvedType::Void),
        is_variadic: false,
        self_mode: None,
        calling_convention: convention,
    }
}

fn dummy_union_type() -> ResolvedType {
    ResolvedType::Union(Rc::new(RefCell::new(ResolvedUnionType {
        id: id(1),
        name: Ident("U".into()),
        module_path: vec![],
        type_args: vec![],
        fields: vec![],
        functions: vec![],
        suppress: vec![],
    })))
}

fn dummy_enum_type() -> ResolvedType {
    ResolvedType::Enum {
        cell: Rc::new(RefCell::new(ResolvedEnumType {
            id: id(1),
            name: Ident("E".into()),
            module_path: vec![],
            type_args: vec![],
            tag_type: ResolvedType::U16,
            header: vec![],
            dynamic_fields: vec![],
            variants: vec![],
            functions: vec![],
            layout: crate::annotations::Layout::default(),
            suppress: vec![],
        })),
        variant: None,
    }
}

fn dummy_anonymous_enum_type() -> ResolvedType {
    ResolvedType::AnonymousEnum {
        shape: Rc::new(ResolvedAnonymousEnum::canonicalize(vec![
            ResolvedType::I32,
            ResolvedType::Bool,
        ])),
        variant: None,
    }
}

fn dummy_spec_object_type() -> ResolvedType {
    ResolvedType::SpecObject {
        shape: crate::resolved_type::ResolvedSpecShape::canonicalize(vec![]),
        mutable: false,
    }
}

fn supported_shapes() -> Vec<ResolvedType> {
    vec![
        ResolvedType::Bool,
        ResolvedType::Char,
        ResolvedType::I8,
        ResolvedType::I64,
        ResolvedType::ISize,
        ResolvedType::U8,
        ResolvedType::USize,
        ResolvedType::F32,
        ResolvedType::F64,
        ResolvedType::Pointer {
            pointee: Box::new(dummy_struct_type()),
            mutable: false,
        },
        ResolvedType::Array(Box::new(ResolvedType::U8), false),
        ResolvedType::Function(fn_type(CallingConvention::C, vec![dummy_struct_type()])),
    ]
}

fn unsupported_shapes() -> Vec<ResolvedType> {
    vec![
        ResolvedType::SizedArray(Box::new(ResolvedType::U8), 4),
        ResolvedType::Slice {
            item: Box::new(ResolvedType::U8),
            mutable: false,
        },
        ResolvedType::Str { mutable: false },
        dummy_struct_type(),
        dummy_union_type(),
        dummy_enum_type(),
        dummy_anonymous_enum_type(),
        dummy_spec_object_type(),
    ]
}

/// `foreign` linkage is not a calling convention. An Omega-convention
/// signature transports any Omega value through the shared `AbiSignature`,
/// so nothing here may be rejected for its shape.
#[test]
fn omega_convention_accepts_every_by_value_shape() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);

    for shape in supported_shapes().into_iter().chain(unsupported_shapes()) {
        assert!(
            a.check_signature_abi(
                id(2),
                sp(),
                &fn_type(CallingConvention::Omega, vec![shape.clone()])
            ),
            "omega convention rejected `{shape}` as a parameter"
        );
        let mut returns = fn_type(CallingConvention::Omega, vec![]);
        returns.return_type = Box::new(shape.clone());
        assert!(
            a.check_signature_abi(id(2), sp(), &returns),
            "omega convention rejected `{shape}` as a result"
        );
    }

    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}

#[test]
fn non_omega_conventions_accept_scalar_and_pointer_shapes() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);

    for convention in [CallingConvention::C, CallingConvention::SysV64] {
        for shape in supported_shapes() {
            assert!(
                a.check_signature_abi(id(2), sp(), &fn_type(convention, vec![shape.clone()])),
                "`{}` rejected `{shape}` as a parameter",
                convention.name()
            );
        }
    }

    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}

#[test]
fn non_omega_conventions_reject_composite_shapes_in_either_position() {
    for convention in [CallingConvention::C, CallingConvention::SysV64] {
        for shape in unsupported_shapes() {
            let mut resolver = NoResolver;
            let mut a = analyzer(&mut resolver);

            let mut returns = fn_type(convention, vec![]);
            returns.return_type = Box::new(shape.clone());
            assert!(!a.check_signature_abi(id(2), sp(), &fn_type(convention, vec![shape.clone()])));
            assert!(!a.check_signature_abi(id(2), sp(), &returns));

            let (errors, _, _) = a.finish();
            assert_eq!(errors.len(), 2, "`{shape}` under `{}`", convention.name());
            for error in errors {
                let AnalysisErrorKind::UnsupportedConventionByValue {
                    r#type,
                    convention: reported,
                } = error.kind
                else {
                    panic!("unexpected diagnostic for `{shape}`")
                };
                assert_eq!(r#type, shape);
                assert_eq!(reported, convention);
            }
        }
    }
}

/// Only the fixed signature is classified here; the variadic tail carries its
/// own promotion rules and is validated at the call site.
#[test]
fn variadic_tail_is_not_classified_by_the_signature_check() {
    let mut resolver = NoResolver;
    let mut a = analyzer(&mut resolver);

    let mut variadic = fn_type(CallingConvention::C, vec![ResolvedType::I32]);
    variadic.is_variadic = true;
    assert!(a.check_signature_abi(id(2), sp(), &variadic));

    let (errors, _, _) = a.finish();
    assert!(errors.is_empty());
}
