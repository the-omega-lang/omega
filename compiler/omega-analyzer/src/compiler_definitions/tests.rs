use super::*;
use crate::target::{Arch, Os};
use omega_parser::prelude::parse_literal;

/// Decodes one written literal exactly as a supplied definition's value is
/// decoded. What spelling delivered it is the caller's business, not this
/// module's.
fn decoded(target: Target, literal: &str) -> Result<DefinitionValue, String> {
    parse_literal(literal)
        .map_err(|error| error.to_string())
        .and_then(|literal| {
            decode_literal(&literal, target.pointer_bits()).map_err(|error| error.to_string())
        })
}

fn value(literal: &str) -> DefinitionValue {
    decoded(Target::DEFAULT, literal).unwrap_or_else(|error| panic!("{literal}: {error}"))
}

fn rejected(target: Target, literal: &str) -> String {
    decoded(target, literal).expect_err(&format!("{literal} must be rejected"))
}

fn int(value: i128, signed: bool, width: u32) -> DefinitionValue {
    DefinitionValue::Int {
        value,
        r#type: IntType { signed, width },
    }
}

#[test]
fn every_literal_kind_keeps_the_kind_it_was_written_as() {
    assert_eq!(value("true"), DefinitionValue::Bool(true));
    assert_eq!(value("false"), DefinitionValue::Bool(false));
    assert_eq!(value("123"), int(123, true, 32));
    assert_eq!(
        value("1.5"),
        DefinitionValue::Float {
            value: 1.5,
            width: 32
        }
    );
    assert_eq!(value("'x'"), DefinitionValue::Char('x'));
    assert_eq!(
        value("\"release build\""),
        DefinitionValue::Str("release build".into())
    );
    assert_eq!(value("b\"raw\""), DefinitionValue::ByteStr("raw".into()));
}

#[test]
fn escapes_and_unicode_decode_exactly_once() {
    assert_eq!(value(r"'\n'"), DefinitionValue::Char('\n'));
    assert_eq!(value(r"'\u{1F600}'"), DefinitionValue::Char('\u{1F600}'));
    assert_eq!(
        value(r#""a\tb\\c""#),
        DefinitionValue::Str("a\tb\\c".into())
    );
}

#[test]
fn a_suffix_selects_the_numeric_type_and_a_base_is_decoded() {
    assert_eq!(value("255u8"), int(255, false, 8));
    assert_eq!(value("0xff_u8"), int(255, false, 8));
    assert_eq!(value("0b1010"), int(10, true, 32));
    assert_eq!(value("0o17"), int(15, true, 32));
    assert_eq!(value("1_000_000i64"), int(1_000_000, true, 64));
    assert_eq!(value("-5"), int(-5, true, 32));
}

#[test]
fn a_signed_minimum_decodes_without_wrapping() {
    assert_eq!(value("-128i8"), int(-128, true, 8));
    assert_eq!(
        value("-9223372036854775808i64"),
        int(i64::MIN.into(), true, 64)
    );
    assert_eq!(
        value("18446744073709551615u64"),
        int(u64::MAX.into(), false, 64)
    );
    assert!(rejected(Target::DEFAULT, "-129i8").contains("does not fit"));
    assert!(rejected(Target::DEFAULT, "128i8").contains("does not fit"));
    assert!(rejected(Target::DEFAULT, "-1u8").contains("negative"));
}

#[test]
fn pointer_sized_suffixes_follow_the_selected_target() {
    let wide = Target {
        arch: Arch::X86_64,
        os: Os::Linux,
    };
    let narrow = Target {
        arch: Arch::Avr,
        os: Os::None,
    };
    assert_eq!(
        decoded(wide, "65536usize").expect("64-bit usize holds 65536"),
        int(65536, false, 64)
    );
    assert!(rejected(narrow, "65536usize").contains("16-bit"));
    assert!(rejected(narrow, "-32769isize").contains("16-bit"));
}

#[test]
fn a_float_is_rounded_to_its_declared_width() {
    assert_eq!(
        value("0.1"),
        DefinitionValue::Float {
            value: f64::from(0.1f32),
            width: 32
        }
    );
    assert_eq!(
        value("0.1f64"),
        DefinitionValue::Float {
            value: 0.1f64,
            width: 64
        }
    );
}

#[test]
fn f32_rounding_does_not_pass_through_f64() {
    // These decimals straddle an f32 midpoint but round to the same f64.
    for (literal, expected) in [
        ("1.000000059604644775390624", 1.0f32),
        ("1.000000059604644775390626", f32::from_bits(0x3f800001)),
    ] {
        for suffix in ["", "f32"] {
            for (sign, factor) in [("", 1.0), ("-", -1.0)] {
                assert_eq!(
                    value(&format!("{sign}{literal}{suffix}")),
                    DefinitionValue::Float {
                        value: f64::from(expected) * factor,
                        width: 32,
                    }
                );
            }
        }
    }
}

#[test]
fn an_unrepresentable_float_is_rejected() {
    let huge = "9".repeat(60);
    assert!(rejected(Target::DEFAULT, &format!("{huge}.0")).contains("finite"));
    assert!(
        decoded(Target::DEFAULT, &format!("{huge}.0f64")).is_ok(),
        "the same magnitude is finite as an f64"
    );
}

#[test]
fn a_malformed_or_non_literal_value_is_rejected() {
    for literal in [
        "release",
        "1 + 1",
        "1u7",
        "1.5u8",
        "0x1.5",
        "",
        "sizeof<u32>",
        "&[1]",
        "\"unterminated",
    ] {
        assert!(
            decoded(Target::DEFAULT, literal).is_err(),
            "{literal:?} must be rejected"
        );
    }
}

/// A configuration has one value per name. Which spelling supplied each one,
/// and how to report the conflict, belongs to whoever collected them.
#[test]
fn a_name_can_only_be_defined_once() {
    let mut definitions = CompilerDefinitions::new(Target::DEFAULT);
    let name = Ident("flag".into());
    assert!(definitions.define(name.clone(), DefinitionValue::Bool(true)));
    assert!(!definitions.define(name.clone(), DefinitionValue::Bool(false)));
    assert!(!definitions.define(name.clone(), int(1, true, 32)));
    assert_eq!(
        definitions.user(&name),
        Some(&DefinitionValue::Bool(true)),
        "a refused definition must not overwrite the one already there"
    );
}

#[test]
fn an_undefined_name_is_absent_rather_than_false() {
    let definitions = CompilerDefinitions::new(Target::DEFAULT);
    assert_eq!(definitions.user(&Ident("never_supplied".into())), None);
}

#[test]
fn builtins_describe_the_selected_target_and_are_not_user_definitions() {
    let mut definitions = CompilerDefinitions::new(Target {
        arch: Arch::Avr,
        os: Os::None,
    });
    assert!(definitions.define(
        Ident("target_os".into()),
        DefinitionValue::Str("custom".into())
    ));

    assert_eq!(
        definitions.builtin(&Ident("target_os".into())),
        Some(DefinitionValue::Str("none".into()))
    );
    assert_eq!(
        definitions.builtin(&Ident("target_arch".into())),
        Some(DefinitionValue::Str("avr".into()))
    );
    assert_eq!(
        definitions.builtin(&Ident("target_pointer_width".into())),
        Some(int(16, false, 32))
    );
    assert_eq!(
        definitions.builtin(&Ident("target_freestanding".into())),
        Some(DefinitionValue::Bool(true))
    );
    assert_eq!(
        definitions.user(&Ident("target_os".into())),
        Some(&DefinitionValue::Str("custom".into())),
        "the user definition keeps its own namespace"
    );
    assert_eq!(definitions.builtin(&Ident("target_env".into())), None);
}

#[test]
fn every_builtin_name_resolves_on_every_target() {
    for arch in [
        Arch::X86_64,
        Arch::X86,
        Arch::Armv7,
        Arch::Thumbv7em,
        Arch::Aarch64,
        Arch::Riscv32,
        Arch::Riscv64,
        Arch::Avr,
    ] {
        for &os in arch.supported_oses() {
            let definitions = CompilerDefinitions::new(Target { arch, os });
            for name in BUILTIN_NAMES {
                assert!(
                    definitions.builtin(&Ident(name.into())).is_some(),
                    "{name} must be defined for {arch:?}-{os:?}"
                );
            }
            assert_eq!(
                definitions.builtin(&Ident("target_freestanding".into())),
                Some(DefinitionValue::Bool(os == Os::None))
            );
        }
    }
}
