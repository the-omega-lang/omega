use super::*;
use crate::target::{Arch, Os};

const AARCH64: Target = Target {
    arch: Arch::Aarch64,
    os: Os::Linux,
};

#[test]
fn no_convention_resolves_to_implicit_omega() {
    let ctx = Context::new(Target::DEFAULT);
    assert_eq!(
        ctx.resolve_convention(None).unwrap(),
        CallingConvention::Omega
    );
}

#[test]
fn c_convention_is_accepted_on_any_target() {
    for target in [Target::DEFAULT, AARCH64] {
        let ctx = Context::new(target);
        let name = Ident("c".into());
        assert_eq!(
            ctx.resolve_convention(Some(&name)).unwrap(),
            CallingConvention::C,
            "target {target}"
        );
    }
}

#[test]
fn sysv64_is_accepted_on_x86_64_and_rejected_elsewhere() {
    let name = Ident("sysv64".into());

    let x86_64 = Context::new(Target::DEFAULT);
    assert_eq!(
        x86_64.resolve_convention(Some(&name)).unwrap(),
        CallingConvention::SysV64
    );

    let aarch64 = Context::new(AARCH64);
    let err = aarch64.resolve_convention(Some(&name)).unwrap_err();
    assert!(matches!(
        err,
        TypeResolutionError::CallingConventionNotAvailable {
            convention: CallingConvention::SysV64,
            target: AARCH64,
            ..
        }
    ));
}

#[test]
fn unknown_convention_name_is_rejected() {
    let ctx = Context::new(Target::DEFAULT);
    let name = Ident("stdcall".into());
    let err = ctx.resolve_convention(Some(&name)).unwrap_err();
    assert!(matches!(
        err,
        TypeResolutionError::UnknownCallingConvention { .. }
    ));
}

fn binding(local: u32, r#type: ResolvedType, mutable: bool) -> VarBinding {
    VarBinding {
        decl_id: HirId {
            module: omega_hir::ModuleId(0),
            local,
        },
        storage: Storage::Local,
        r#type,
        span: Span::new(local as usize, local as usize + 1),
        narrowed: false,
        mutable,
        used: false,
        written: false,
    }
}

fn declare(ctx: &mut Context, name: &str, local: u32, r#type: ResolvedType, mutable: bool) {
    ctx.declare(
        Ident(name.into()),
        Origin::default(),
        binding(local, r#type, mutable),
        DeclarationPolicy::Shadow,
    )
    .expect("a shadowing declaration never collides");
}

#[test]
fn a_shadowing_declaration_wins_the_name_but_both_stay_in_the_scope() {
    let mut ctx = Context::new(Target::DEFAULT);
    ctx.enter_scope();
    declare(&mut ctx, "x", 1, ResolvedType::I32, true);
    declare(&mut ctx, "x", 2, ResolvedType::Bool, false);

    let found = ctx
        .find_variable(&Ident("x".into()), Origin::default())
        .expect("`x` is bound");
    assert_eq!(found.decl_id.local, 2);
    assert_eq!(found.r#type, ResolvedType::Bool);
    assert!(!found.mutable);

    let scope = ctx.leave_scope();
    let declared: Vec<u32> = scope.bindings().map(|(_, b)| b.decl_id.local).collect();
    assert_eq!(
        declared,
        vec![1, 2],
        "both declarations keep their identity"
    );
}

#[test]
fn a_unique_declaration_still_rejects_a_second_binding_of_the_same_name() {
    let mut ctx = Context::new(Target::DEFAULT);
    ctx.enter_scope();
    declare(&mut ctx, "p", 1, ResolvedType::I32, false);
    let err = ctx
        .declare(
            Ident("p".into()),
            Origin::default(),
            binding(2, ResolvedType::I32, false),
            DeclarationPolicy::Unique,
        )
        .expect_err("a unique declaration collides");
    assert_eq!(err.0, Ident("p".into()));
    assert_eq!(err.1, Span::new(1, 2), "the first declaration is reported");
}

#[test]
fn leaving_an_inner_scope_reveals_the_outer_binding() {
    let mut ctx = Context::new(Target::DEFAULT);
    ctx.enter_scope();
    declare(&mut ctx, "x", 1, ResolvedType::I32, false);
    ctx.enter_scope();
    declare(&mut ctx, "x", 2, ResolvedType::Bool, false);
    assert_eq!(
        ctx.find_variable(&Ident("x".into()), Origin::default())
            .unwrap()
            .decl_id
            .local,
        2
    );
    ctx.leave_scope();
    assert_eq!(
        ctx.find_variable(&Ident("x".into()), Origin::default())
            .unwrap()
            .decl_id
            .local,
        1
    );
}

#[test]
fn a_shadowed_binding_is_still_reachable_by_its_own_hir_id() {
    let mut ctx = Context::new(Target::DEFAULT);
    ctx.enter_scope();
    declare(&mut ctx, "x", 1, ResolvedType::I32, true);
    ctx.mark_used(HirId {
        module: omega_hir::ModuleId(0),
        local: 1,
    });
    declare(&mut ctx, "x", 2, ResolvedType::I32, true);
    ctx.mark_written(HirId {
        module: omega_hir::ModuleId(0),
        local: 2,
    });

    let scope = ctx.leave_scope();
    let state: Vec<(u32, bool, bool)> = scope
        .bindings()
        .map(|(_, b)| (b.decl_id.local, b.used, b.written))
        .collect();
    assert_eq!(state, vec![(1, true, false), (2, false, true)]);
}

#[test]
fn a_macro_origin_name_does_not_shadow_the_same_spelling_from_the_caller() {
    let mut ctx = Context::new(Target::DEFAULT);
    ctx.enter_scope();
    declare(&mut ctx, "x", 1, ResolvedType::I32, false);
    let macro_origin = Origin(Some(ExpansionId(7)));
    ctx.declare(
        Ident("x".into()),
        macro_origin,
        binding(2, ResolvedType::Bool, false),
        DeclarationPolicy::Shadow,
    )
    .unwrap();

    assert_eq!(
        ctx.find_variable(&Ident("x".into()), Origin::default())
            .unwrap()
            .decl_id
            .local,
        1
    );
    assert_eq!(
        ctx.find_variable(&Ident("x".into()), macro_origin)
            .unwrap()
            .decl_id
            .local,
        2
    );
}
