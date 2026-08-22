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
