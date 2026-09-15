use super::*;
use crate::target::{Arch, Os};

fn raw(spelling: &str) -> RawDefinition {
    let body = spelling.strip_prefix("-D").expect("a -D option");
    match body.split_once('=') {
        Some((name, value)) => RawDefinition {
            spelling: spelling.to_string(),
            name: name.to_string(),
            value: Some(value.to_string()),
        },
        None => RawDefinition {
            spelling: spelling.to_string(),
            name: body.to_string(),
            value: None,
        },
    }
}

fn define(target: Target, options: &[&str]) -> Result<CompilerDefinitions, Vec<DefinitionError>> {
    let raw: Vec<RawDefinition> = options.iter().map(|option| raw(option)).collect();
    CompilerDefinitions::from_raw(target, &raw)
}

fn value(options: &[&str], name: &str) -> DefinitionValue {
    let definitions = define(Target::DEFAULT, options).expect("the options are valid");
    definitions
        .user(&Ident(name.into()))
        .expect("the definition was supplied")
        .clone()
}

fn rejected(target: Target, option: &str) -> String {
    let errors = define(target, &[option]).expect_err("the option must be rejected");
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn int(value: i128, signed: bool, width: u32) -> DefinitionValue {
    DefinitionValue::Int {
        value,
        r#type: IntType { signed, width },
    }
}

#[test]
fn a_name_without_a_value_defines_a_truth() {
    assert_eq!(
        value(&["-Denabled"], "enabled"),
        DefinitionValue::Bool(true)
    );
    assert_eq!(
        value(&["-Dflag=false"], "flag"),
        DefinitionValue::Bool(false)
    );
}

#[test]
fn every_literal_kind_keeps_the_kind_it_was_written_as() {
    assert_eq!(value(&["-Dn=123"], "n"), int(123, true, 32));
    assert_eq!(
        value(&["-Dratio=1.5"], "ratio"),
        DefinitionValue::Float {
            value: 1.5,
            width: 32
        }
    );
    assert_eq!(value(&["-Dc='x'"], "c"), DefinitionValue::Char('x'));
    assert_eq!(
        value(&["-Dlabel=\"release build\""], "label"),
        DefinitionValue::Str("release build".into())
    );
    assert_eq!(
        value(&["-Dbytes=b\"raw\""], "bytes"),
        DefinitionValue::ByteStr("raw".into())
    );
}

#[test]
fn escapes_and_unicode_decode_exactly_once() {
    assert_eq!(value(&[r#"-Dc='\n'"#], "c"), DefinitionValue::Char('\n'));
    assert_eq!(
        value(&[r#"-Dc='\u{1F600}'"#], "c"),
        DefinitionValue::Char('\u{1F600}')
    );
    assert_eq!(
        value(&[r#"-Dtext="a\tb\\c""#], "text"),
        DefinitionValue::Str("a\tb\\c".into())
    );
}

#[test]
fn a_suffix_selects_the_numeric_type_and_a_base_is_decoded() {
    assert_eq!(value(&["-Dn=255u8"], "n"), int(255, false, 8));
    assert_eq!(value(&["-Dn=0xff_u8"], "n"), int(255, false, 8));
    assert_eq!(value(&["-Dn=0b1010"], "n"), int(10, true, 32));
    assert_eq!(value(&["-Dn=0o17"], "n"), int(15, true, 32));
    assert_eq!(value(&["-Dn=1_000_000i64"], "n"), int(1_000_000, true, 64));
    assert_eq!(value(&["-Dn=-5"], "n"), int(-5, true, 32));
}

#[test]
fn a_signed_minimum_decodes_without_wrapping() {
    assert_eq!(value(&["-Dn=-128i8"], "n"), int(-128, true, 8));
    assert_eq!(
        value(&["-Dn=-9223372036854775808i64"], "n"),
        int(i64::MIN.into(), true, 64)
    );
    assert_eq!(
        value(&["-Dn=18446744073709551615u64"], "n"),
        int(u64::MAX.into(), false, 64)
    );
    assert!(rejected(Target::DEFAULT, "-Dn=-129i8").contains("does not fit"));
    assert!(rejected(Target::DEFAULT, "-Dn=128i8").contains("does not fit"));
    assert!(rejected(Target::DEFAULT, "-Dn=-1u8").contains("negative"));
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
    let definitions = define(wide, &["-Dn=65536usize"]).expect("64-bit usize holds 65536");
    assert_eq!(
        definitions.user(&Ident("n".into())),
        Some(&int(65536, false, 64))
    );
    assert!(rejected(narrow, "-Dn=65536usize").contains("16-bit"));
    assert!(rejected(narrow, "-Dn=-32769isize").contains("16-bit"));
}

#[test]
fn every_definition_is_validated_even_when_nothing_reads_it() {
    let errors = define(Target::DEFAULT, &["-Dgood=1", "-Dbad=300u8"])
        .expect_err("an unused definition is still checked");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().contains("300"));
}

#[test]
fn a_float_is_rounded_to_its_declared_width() {
    assert_eq!(
        value(&["-Dn=0.1"], "n"),
        DefinitionValue::Float {
            value: f64::from(0.1f32),
            width: 32
        }
    );
    assert_eq!(
        value(&["-Dn=0.1f64"], "n"),
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
                    value(&[&format!("-Dn={sign}{literal}{suffix}")], "n"),
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
    assert!(rejected(Target::DEFAULT, &format!("-Dn={huge}.0")).contains("finite"));
    assert!(
        define(Target::DEFAULT, &[&format!("-Dn={huge}.0f64")]).is_ok(),
        "the same magnitude is finite as an f64"
    );
}

#[test]
fn a_malformed_or_non_literal_value_is_rejected() {
    for option in [
        "-Dlabel=release",
        "-Dn=1 + 1",
        "-Dn=1u7",
        "-Dn=1.5u8",
        "-Dn=0x1.5",
        "-Dn=",
        "-Dn=sizeof<u32>",
        "-Dn=&[1]",
        "-Dn=\"unterminated",
    ] {
        assert!(
            define(Target::DEFAULT, &[option]).is_err(),
            "{option} must be rejected"
        );
    }
}

#[test]
fn a_name_must_be_one_plain_identifier() {
    for option in [
        "-D=1",
        "-D0abc",
        "-Dfoo-bar",
        "-Dif",
        "-Da::b",
        "-Dwith space",
    ] {
        assert!(
            rejected(Target::DEFAULT, option).contains("definition name"),
            "{option} must be rejected as a name"
        );
    }
}

#[test]
fn a_repeated_name_is_rejected_however_it_is_spelled() {
    for options in [
        ["-Dflag", "-Dflag"],
        ["-Dflag=1", "-Dflag=1"],
        ["-Dflag=1", "-Dflag=2"],
        ["-Dflag", "-Dflag=true"],
    ] {
        let message = define(Target::DEFAULT, &options)
            .expect_err("a repeated definition must be rejected")
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        assert!(message.contains("more than once"), "{message}");
    }
}

#[test]
fn builtins_describe_the_selected_target_and_are_not_user_definitions() {
    let definitions = define(
        Target {
            arch: Arch::Avr,
            os: Os::None,
        },
        &["-Dtarget_os=\"custom\""],
    )
    .expect("a user definition may share a builtin spelling");

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
