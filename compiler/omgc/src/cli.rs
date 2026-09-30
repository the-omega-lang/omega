use omega_analyzer::Target;
use omega_analyzer::compiler_definitions::{CompilerDefinitions, DefinitionValue, decode_literal};
use omega_codegen::{EmitKind, OptLevel};
use omega_diagnostics::{BOLD, CYAN, Diagnostic, paint};
use omega_driver::{ExternRoot, basename};
use omega_parser::prelude::Ident;
use std::path::PathBuf;

const USAGE: &str = "omgc [<name>=]<entry-dir> -o <output-dir> [OPTIONS]";

const MODULE_NAME_RULE: &str = "module names must be valid Omega identifiers (ASCII \
     letters/digits/underscore, not starting with a digit, and not a reserved keyword); Omega \
     does not normalize names automatically";

pub(crate) enum Command {
    Help,
    Compile(Args),
}

pub(crate) struct Args {
    pub(crate) entry_dir: PathBuf,
    /// Every `-D` option in the order it was written, still undecoded: the
    /// values are validated against the final target, not against whichever
    /// target had been selected when the option was read.
    pub(crate) definitions: Vec<DefinitionOption>,
    pub(crate) output_dir: PathBuf,
    pub(crate) externs: Vec<ExternRoot>,
    pub(crate) name: Option<Ident>,
    pub(crate) opt_level: OptLevel,
    pub(crate) target: Target,
    pub(crate) emit: EmitKind,
    pub(crate) verbose: bool,
}

pub(crate) fn parse(args: &[String]) -> Result<Command, Diagnostic> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(Command::Help);
    }

    parse_compile(args).map(Command::Compile)
}

fn parse_compile(args: &[String]) -> Result<Args, Diagnostic> {
    let mut entry_dir: Option<PathBuf> = None;
    let mut definitions = Vec::new();
    let mut output_dir = None;
    let mut externs = Vec::new();
    let mut name = None;
    let mut opt_level = OptLevel::default();
    let mut target = Target::DEFAULT;
    let mut emit = EmitKind::default();
    let mut verbose = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--import=") {
            externs.push(parse_import(arg, value)?);
        } else if arg == "-D" {
            let definition = iter.next().ok_or_else(|| {
                Diagnostic::error("expected a definition after '-D'")
                    .with_help("write '-D <name>' or '-D <name>=<literal>'")
            })?;
            definitions.push(parse_definition(&format!("-D {definition}"), definition));
        } else if let Some(value) = arg.strip_prefix("-D") {
            definitions.push(parse_definition(arg, value));
        } else if arg == "-o" {
            let dir = iter.next().ok_or_else(|| {
                Diagnostic::error("expected a directory path after '-o'")
                    .with_help("write '-o <output-dir>'")
            })?;
            output_dir = Some(PathBuf::from(dir));
        } else if let Some(value) = arg.strip_prefix("-O") {
            opt_level = value.parse().map_err(Diagnostic::error)?;
        } else if let Some(value) = arg.strip_prefix("--target=") {
            target = Target::parse(value).map_err(|error| Diagnostic::error(error.to_string()))?;
        } else if let Some(value) = arg.strip_prefix("--emit=") {
            emit = value.parse().map_err(Diagnostic::error)?;
        } else if arg == "-v" || arg == "--verbose" {
            verbose = true;
        } else if arg.starts_with('-') {
            return Err(Diagnostic::error(format!("unknown flag '{arg}'"))
                .with_help("run 'omgc --help' to list the accepted options"));
        } else if let Some(first) = &entry_dir {
            return Err(
                Diagnostic::error(format!("unexpected extra argument '{arg}'")).with_note(format!(
                    "the entry directory was already given as '{}'; omgc compiles one \
                         package per invocation",
                    first.display()
                )),
            );
        } else {
            let (explicit_name, dir) = parse_entry(arg)?;
            name = explicit_name;
            entry_dir = Some(dir);
        }
    }

    let entry_dir = entry_dir.ok_or_else(|| {
        Diagnostic::error("no package directory given")
            .with_help(format!("usage: {USAGE}"))
            .with_note("run 'omgc --help' for all options")
    })?;
    let output_dir = output_dir.ok_or_else(|| {
        Diagnostic::error("no output directory given")
            .with_help("pass '-o <output-dir>'; omgc writes one artifact per source file into it")
    })?;

    Ok(Args {
        entry_dir,
        definitions,
        output_dir,
        externs,
        name,
        opt_level,
        target,
        emit,
        verbose,
    })
}

/// One `-D` option as the command line spelled it, before its name or value
/// is known to be well formed. Repeated names are kept, in input order, so
/// each conflict can be reported against the two options that caused it.
#[derive(Debug, Clone)]
pub(crate) struct DefinitionOption {
    /// The option as written, quoted back in diagnostics.
    spelling: String,
    name: String,
    /// `None` for `-Dname`, which defines a boolean truth.
    value: Option<String>,
}

/// `name` or `name=<literal>`. Only the first `=` separates them, so a value
/// may contain one; the value itself stays undecoded here.
fn parse_definition(spelling: &str, value: &str) -> DefinitionOption {
    let (name, value) = match value.split_once('=') {
        Some((name, value)) => (name, Some(value.to_string())),
        None => (value, None),
    };
    DefinitionOption {
        spelling: spelling.to_string(),
        name: name.to_string(),
        value,
    }
}

/// Turns the collected options into the compilation's configuration.
///
/// This runs once the target is final, so an option's meaning never depends
/// on where it sits relative to `--target`, and every option is checked --
/// including one nothing reads. Each failure is reported against the option
/// that caused it, so all of them are collected rather than only the first.
pub(crate) fn resolve_definitions(
    target: Target,
    options: &[DefinitionOption],
) -> Result<CompilerDefinitions, Vec<Diagnostic>> {
    let mut definitions = CompilerDefinitions::new(target);
    let mut spellings: Vec<(&str, &str)> = Vec::new();
    let mut errors: Vec<Diagnostic> = Vec::new();

    for option in options {
        let invalid = |reason: &str| {
            Diagnostic::error(format!(
                "invalid definition '{}': {reason}",
                option.spelling
            ))
        };
        if !omega_parser::lexer::is_valid_identifier(&option.name) {
            errors.push(
                invalid(&format!("'{}' is not a valid definition name", option.name)).with_note(
                    "a definition name is a single Omega identifier (ASCII \
                     letters/digits/underscore, not starting with a digit, and not a keyword)",
                ),
            );
            continue;
        }
        if let Some((_, first)) = spellings.iter().find(|(name, _)| *name == option.name) {
            errors.push(
                Diagnostic::error(format!(
                    "definition '{}' is defined more than once ('{first}' and '{}')",
                    option.name, option.spelling
                ))
                .with_note("one invocation has one value for each definition"),
            );
            continue;
        }
        let value = match option.value.as_deref() {
            None => Ok(DefinitionValue::Bool(true)),
            Some("") => Err(invalid("a definition needs a value after '='")
                .with_help("write '-Dname' for a boolean truth")),
            Some(text) => omega_parser::prelude::parse_literal(text)
                .map_err(|error| error.to_string())
                .and_then(|literal| {
                    decode_literal(&literal, target.pointer_bits()).map_err(|e| e.to_string())
                })
                .map_err(|reason| invalid(&reason)),
        };
        match value {
            Ok(value) => {
                spellings.push((&option.name, &option.spelling));
                // The duplicate check above already refused a repeated name,
                // so this is the first and only definition of it.
                assert!(definitions.define(Ident(option.name.clone()), value));
            }
            Err(diagnostic) => errors.push(diagnostic),
        }
    }

    if errors.is_empty() {
        Ok(definitions)
    } else {
        Err(errors)
    }
}

/// The compiled package is written like an import: an optional declared
/// identity, then its root directory (`[<name>=]<dir>`).
fn parse_entry(arg: &str) -> Result<(Option<Ident>, PathBuf), Diagnostic> {
    let (explicit_name, dir) = split_declared_root(arg).map_err(|reason| {
        Diagnostic::error(format!("invalid entry argument '{arg}': {reason}"))
            .with_help("write '<name>=<dir>', or the bare directory to infer the name")
    })?;
    let name = explicit_name
        .map(|raw| {
            validate_module_name(
                raw.as_ref(),
                format!("declared by the entry argument '{arg}'"),
            )
            .map_err(|diagnostic| diagnostic.with_help("declare a name that is a valid identifier"))
        })
        .transpose()?;

    Ok((name, dir))
}

fn parse_import(flag: &str, value: &str) -> Result<ExternRoot, Diagnostic> {
    let (explicit_name, dir) = split_declared_root(value).map_err(|reason| {
        Diagnostic::error(format!("invalid --import flag '{flag}': {reason}"))
            .with_help("write '--import=<name>=<dir>', or '--import=<dir>' to infer the name")
    })?;
    let name = match explicit_name {
        Some(raw) => validate_module_name(raw.as_ref(), format!("declared by '{flag}'")).map_err(
            |diagnostic| diagnostic.with_help("declare a name that is a valid identifier"),
        )?,
        None => {
            let Some(physical_name) = basename(&dir) else {
                return Err(Diagnostic::error(format!(
                    "invalid --import flag '{flag}': '{}' has no usable directory name",
                    dir.display()
                ))
                .with_note(
                    "a package root's own module file is named after its directory, so the \
                     directory must be named explicitly",
                ));
            };
            validate_module_name(
                physical_name.as_ref(),
                format!("inferred from the directory name in '{flag}'"),
            )
            .map_err(|diagnostic| {
                diagnostic.with_help("pass --import=<name>=<dir> to declare a different name")
            })?
        }
    };

    Ok(ExternRoot { name, dir })
}

/// The one place that turns a raw CLI-supplied or filesystem-inferred
/// string into a trusted module-identity `Ident`: it must be a spelling the
/// parser itself could tokenize as an identifier, matching
/// `docs/language/modules-and-imports.md`'s no-normalization rule.
///
/// `origin` says where the spelling came from; callers attach the `help`
/// that fits it.
pub(crate) fn validate_module_name(
    name: &str,
    origin: impl std::fmt::Display,
) -> Result<Ident, Diagnostic> {
    if omega_parser::lexer::is_valid_identifier(name) {
        Ok(Ident(name.to_string()))
    } else {
        Err(Diagnostic::error(format!(
            "'{name}' ({origin}) is not a valid Omega module name"
        ))
        .with_note(MODULE_NAME_RULE))
    }
}

/// Only the first `=` separates the name, as in `-D`, so the directory may
/// contain one; a bare directory containing `=` therefore needs a name.
fn split_declared_root(value: &str) -> Result<(Option<Ident>, PathBuf), String> {
    match value.split_once('=') {
        Some(("", _)) => Err("the name before '=' cannot be empty".to_string()),
        Some((name, dir)) => Ok((Some(Ident(name.to_string())), PathBuf::from(dir))),
        None => Ok((None, PathBuf::from(value))),
    }
}

fn help_option(colors: bool, flag: &str, desc: &str) {
    let padded = format!("{flag:<26}");
    println!("    {} {desc}", paint(colors, CYAN, &padded));
}

pub(crate) fn print_help() {
    let colors = crate::use_colors(std::io::stdout());
    println!("{}", paint(colors, BOLD, "omgc"));
    println!("The Omega compiler\n");
    println!("{}", paint(colors, BOLD, "USAGE:"));
    println!("    {USAGE}\n");
    println!("{}", paint(colors, BOLD, "ARGS:"));
    help_option(
        colors,
        "[<name>=]<entry-dir>",
        "Root directory of the package to compile (identity defaults to the directory basename)",
    );
    println!();
    println!("{}", paint(colors, BOLD, "OPTIONS:"));
    help_option(
        colors,
        "-o <dir>",
        "Output directory: one artifact per source file, mirroring the source tree (required)",
    );
    help_option(colors, "-O<0-3>", "Optimization level (default: 0)");
    help_option(
        colors,
        "--target=<arch>-<os>",
        &format!(
            "Target to compile for, e.g. aarch64-linux or avr-none (default: {})",
            Target::DEFAULT
        ),
    );
    help_option(
        colors,
        "--emit=<obj|ir|asm>",
        "What to emit: object file (default), backend IR, or assembly",
    );
    help_option(
        colors,
        "-D<name>[=<literal>]",
        "Define a compiler definition read as 'def::<name>' (repeatable; no value means 'true')",
    );
    help_option(
        colors,
        "--import=[<name>=]<dir>",
        "Register an external module root (repeatable; name defaults to the directory basename)",
    );
    help_option(colors, "-v, --verbose", "Print progress information");
    help_option(colors, "-h, --help", "Print this help message");
}

#[cfg(test)]
mod tests {
    use super::*;
    use omega_analyzer::{Arch, Os};
    use omega_diagnostics::Footer;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn parse_error(values: &[&str]) -> Diagnostic {
        match parse(&args(values)) {
            Ok(_) => panic!("expected {values:?} to be rejected"),
            Err(diagnostic) => diagnostic,
        }
    }

    fn helps(diagnostic: &Diagnostic) -> Vec<&str> {
        diagnostic
            .footers
            .iter()
            .filter_map(|footer| match footer {
                Footer::Help(help) => Some(help.as_str()),
                Footer::Note(_) => None,
            })
            .collect()
    }

    #[test]
    fn help_does_not_require_compile_arguments() {
        assert!(matches!(parse(&args(&["--help"])), Ok(Command::Help)));
    }

    #[test]
    fn parses_minimal_compile_command() {
        let Ok(Command::Compile(parsed)) = parse(&args(&["src", "-o", "out"])) else {
            panic!("expected compile command");
        };
        assert_eq!(parsed.entry_dir, PathBuf::from("src"));
        assert_eq!(parsed.output_dir, PathBuf::from("out"));
        assert_eq!(parsed.opt_level, OptLevel::O0);
        assert_eq!(parsed.emit, EmitKind::Obj);
    }

    #[test]
    fn parses_codegen_options_and_explicit_extern_identity() {
        let Ok(Command::Compile(parsed)) = parse(&args(&[
            "src",
            "-o",
            "out",
            "-O3",
            "--emit=asm",
            "--import=core=deps/core",
            "--verbose",
        ])) else {
            panic!("expected compile command");
        };
        assert_eq!(parsed.opt_level, OptLevel::O3);
        assert_eq!(parsed.emit, EmitKind::Asm);
        assert!(parsed.verbose);
        assert_eq!(parsed.externs.len(), 1);
        assert_eq!(parsed.externs[0].name.as_ref(), "core");
        assert_eq!(parsed.externs[0].dir, PathBuf::from("deps/core"));
    }

    #[test]
    fn parses_every_requested_cross_compilation_target() {
        for (flag, arch, os) in [
            ("--target=aarch64-linux", Arch::Aarch64, Os::Linux),
            ("--target=x86_64-windows", Arch::X86_64, Os::Windows),
            ("--target=avr-none", Arch::Avr, Os::None),
        ] {
            let Ok(Command::Compile(parsed)) = parse(&args(&["src", "-o", "out", flag])) else {
                panic!("expected {flag} to parse");
            };
            assert_eq!(parsed.target, Target { arch, os });
        }
    }

    #[test]
    fn omitting_the_target_flag_keeps_the_documented_default() {
        let Ok(Command::Compile(parsed)) = parse(&args(&["src", "-o", "out"])) else {
            panic!("expected compile command");
        };
        assert_eq!(parsed.target, Target::DEFAULT);
    }

    #[test]
    fn rejects_unknown_and_malformed_targets() {
        for invalid in [
            "--target=sparc-linux",
            "--target=avr-vxworks",
            "--target=avr",
            "--target=avr-macos",
        ] {
            assert!(
                parse(&args(&["src", "-o", "out", invalid])).is_err(),
                "{invalid} must be rejected before compilation"
            );
        }
    }

    #[test]
    fn rejects_invalid_codegen_options() {
        for invalid in ["-Ofast", "--emit=wat"] {
            assert!(parse(&args(&["src", "-o", "out", invalid])).is_err());
        }
    }

    #[test]
    fn rejects_backend_flag_as_unknown() {
        for invalid in ["--backend=llvm", "--backend=cranelift"] {
            assert!(parse(&args(&["src", "-o", "out", invalid])).is_err());
        }
    }

    #[test]
    fn rejects_missing_output_and_extra_positionals() {
        assert!(parse(&args(&["src"])).is_err());
        assert!(parse(&args(&["src", "other", "-o", "out"])).is_err());
    }

    #[test]
    fn a_missing_entry_directory_points_at_the_usage() {
        let diagnostic = parse_error(&["-o", "out"]);
        assert_eq!(diagnostic.message, "no package directory given");
        assert!(
            helps(&diagnostic).iter().any(|help| help.contains(USAGE)),
            "{:?}",
            diagnostic.footers
        );
    }

    #[test]
    fn a_missing_output_directory_points_at_the_flag() {
        let diagnostic = parse_error(&["src"]);
        assert_eq!(diagnostic.message, "no output directory given");
        assert!(
            helps(&diagnostic)
                .iter()
                .any(|help| help.contains("-o <output-dir>")),
            "{:?}",
            diagnostic.footers
        );
    }

    #[test]
    fn an_unknown_flag_points_at_the_help() {
        let diagnostic = parse_error(&["src", "-o", "out", "--bogus"]);
        assert_eq!(diagnostic.message, "unknown flag '--bogus'");
        assert!(
            helps(&diagnostic)
                .iter()
                .any(|help| help.contains("--help"))
        );
    }

    #[test]
    fn rejects_invalid_declared_entry_name() {
        for invalid in ["foo-bar", "0abc", "if", ""] {
            let err = parse_error(&[&format!("{invalid}=src"), "-o", "out"]).message;
            assert!(err.contains(&format!("'{invalid}=src'")), "{err}");
        }
    }

    #[test]
    fn rejects_legacy_name_flag_as_unknown() {
        let err = parse_error(&["src", "-o", "out", "--name=my_pkg"]).message;
        assert!(err.contains("--name"), "{err}");
    }

    #[test]
    fn rejects_legacy_extern_flag_as_unknown() {
        let err = parse_error(&["src", "-o", "out", "--extern=core=deps/core"]).message;
        assert!(err.contains("--extern"), "{err}");
    }

    #[test]
    fn rejects_invalid_explicit_extern_name() {
        let err = parse_error(&["src", "-o", "out", "--import=foo-bar=deps/core"]).message;
        assert!(err.contains("foo-bar"), "{err}");
    }

    #[test]
    fn rejects_invalid_inferred_extern_basename_without_an_override() {
        let diagnostic = parse_error(&["src", "-o", "out", "--import=deps/foo-bar"]);
        assert!(
            diagnostic.message.contains("foo-bar"),
            "{}",
            diagnostic.message
        );
        assert!(
            helps(&diagnostic)
                .iter()
                .any(|help| help.contains("--import=<name>=<dir>")),
            "{:?}",
            diagnostic.footers
        );
    }

    #[test]
    fn accepts_valid_entry_and_extern_identities() {
        let Ok(Command::Compile(parsed)) = parse(&args(&[
            "my_pkg=src",
            "-o",
            "out",
            "--import=core=deps/core",
        ])) else {
            panic!("expected compile command");
        };
        assert_eq!(parsed.name.as_ref().map(Ident::as_ref), Some("my_pkg"));
        assert_eq!(parsed.entry_dir, PathBuf::from("src"));
        assert_eq!(parsed.externs[0].name.as_ref(), "core");
    }

    #[test]
    fn entry_without_an_explicit_identity_leaves_the_name_inferred() {
        let Ok(Command::Compile(parsed)) = parse(&args(&["deps/core", "-o", "out"])) else {
            panic!("expected compile command");
        };
        assert!(parsed.name.is_none());
        assert_eq!(parsed.entry_dir, PathBuf::from("deps/core"));
    }

    fn definitions(values: &[&str]) -> Vec<DefinitionOption> {
        let Ok(Command::Compile(parsed)) = parse(&args(&[&["src", "-o", "out"], values].concat()))
        else {
            panic!("expected compile command for {values:?}");
        };
        parsed.definitions
    }

    #[test]
    fn a_definition_is_collected_attached_or_separated() {
        let collected = definitions(&["-Dflag", "-Dcount=12", "-D", "other=1", "-D", "bare"]);
        let spelled: Vec<(&str, Option<&str>)> = collected
            .iter()
            .map(|entry| (entry.name.as_str(), entry.value.as_deref()))
            .collect();
        assert_eq!(
            spelled,
            [
                ("flag", None),
                ("count", Some("12")),
                ("other", Some("1")),
                ("bare", None),
            ]
        );
    }

    /// Only the first `=` separates a definition, so a value may contain one.
    #[test]
    fn a_value_keeps_everything_after_the_first_equals() {
        let collected = definitions(&["-Dexpr=\"a=b\""]);
        assert_eq!(collected[0].name, "expr");
        assert_eq!(collected[0].value.as_deref(), Some("\"a=b\""));
    }

    /// Splitting keeps every occurrence; `resolve_definitions` is where a
    /// repeated name becomes the conflict it is.
    #[test]
    fn repeated_definitions_are_preserved_in_input_order() {
        let collected = definitions(&["-Dflag=1", "-Dflag=2"]);
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].value.as_deref(), Some("1"));
        assert_eq!(collected[1].value.as_deref(), Some("2"));
    }

    #[test]
    fn an_empty_definition_name_or_a_missing_argument_is_rejected() {
        assert_eq!(definitions(&["-D=1"])[0].name, "");
        assert!(parse(&args(&["src", "-o", "out", "-D"])).is_err());
    }

    #[test]
    fn a_definition_does_not_collide_with_other_flags() {
        let Ok(Command::Compile(parsed)) = parse(&args(&[
            "src",
            "-o",
            "out",
            "-Dflag",
            "--target=avr-none",
            "-O2",
        ])) else {
            panic!("expected compile command");
        };
        assert_eq!(parsed.definitions.len(), 1);
        assert_eq!(parsed.target, Target::parse("avr-none").expect("valid"));
        assert_eq!(parsed.opt_level, OptLevel::O2);
    }

    fn resolved(target: Target, values: &[&str]) -> Result<CompilerDefinitions, Vec<Diagnostic>> {
        resolve_definitions(target, &definitions(values))
    }

    fn resolve_errors(target: Target, values: &[&str]) -> Vec<String> {
        match resolved(target, values) {
            Ok(_) => panic!("{values:?} must be rejected"),
            Err(diagnostics) => diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.message)
                .collect(),
        }
    }

    fn resolve_error(values: &[&str]) -> String {
        let messages = resolve_errors(Target::DEFAULT, values);
        assert_eq!(messages.len(), 1, "{messages:?}");
        messages.into_iter().next().expect("one message")
    }

    #[test]
    fn a_bare_name_defines_a_truth_and_a_value_is_decoded() {
        let definitions = resolved(Target::DEFAULT, &["-Dflag", "-Dcount=12", "-Dlabel=\"x\""])
            .expect("valid options");
        assert_eq!(
            definitions.user(&Ident("flag".into())),
            Some(&DefinitionValue::Bool(true))
        );
        assert_eq!(
            definitions.user(&Ident("label".into())),
            Some(&DefinitionValue::Str("x".into()))
        );
        assert!(definitions.user(&Ident("count".into())).is_some());
        assert_eq!(definitions.user(&Ident("never_supplied".into())), None);
    }

    #[test]
    fn a_definition_name_must_be_one_plain_identifier() {
        for option in ["-D=1", "-D0abc", "-Dfoo-bar", "-Dif", "-Da::b"] {
            let message = resolve_error(&[option]);
            assert!(
                message.contains("definition name") && message.contains(option),
                "{option}: {message}"
            );
        }
    }

    #[test]
    fn a_repeated_name_is_reported_against_both_options() {
        let message = resolve_error(&["-Dflag=1", "-Dflag=2"]);
        assert!(message.contains("more than once"), "{message}");
        assert!(
            message.contains("-Dflag=1") && message.contains("-Dflag=2"),
            "both spellings must appear: {message}"
        );
        assert!(resolve_error(&["-Dflag", "-D", "flag=true"]).contains("more than once"));
    }

    #[test]
    fn a_malformed_value_names_the_option_that_carried_it() {
        for (option, expected) in [
            ("-Dlabel=release", "expected one literal value"),
            ("-Dcount=", "needs a value"),
            ("-Dcount=300u8", "does not fit"),
        ] {
            let message = resolve_error(&[option]);
            assert!(
                message.contains(expected) && message.contains(option),
                "{option}: {message}"
            );
        }
    }

    /// Every failure is collected, so one mistake does not hide the next.
    #[test]
    fn every_invalid_option_is_reported() {
        let messages = resolve_errors(Target::DEFAULT, &["-D0abc", "-Dgood=1", "-Dbad=release"]);
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].contains("0abc"), "{messages:?}");
        assert!(messages[1].contains("-Dbad=release"), "{messages:?}");
    }

    /// Options are decoded once the target is final, so an unused definition
    /// is checked against the target that was actually selected.
    #[test]
    fn a_definition_is_validated_against_the_final_target() {
        let narrow = Target::parse("avr-none").expect("valid target");
        assert!(resolve_errors(narrow, &["-Dunused=65536usize"])[0].contains("16-bit"));
        assert!(resolved(Target::DEFAULT, &["-Dunused=65536usize"]).is_ok());
    }

    #[test]
    fn a_bare_windows_drive_path_has_no_declared_name() {
        let (name, dir) = split_declared_root(r"C:\omega\core").unwrap();
        assert!(name.is_none());
        assert_eq!(dir, PathBuf::from(r"C:\omega\core"));

        let (name, dir) = split_declared_root(r"core=C:\omega\core").unwrap();
        assert_eq!(name.as_ref().map(Ident::as_ref), Some("core"));
        assert_eq!(dir, PathBuf::from(r"C:\omega\core"));
    }

    /// Only the first `=` separates the name, as in `-D`.
    #[test]
    fn a_declared_root_splits_at_the_first_equals() {
        let (name, dir) = split_declared_root("name=./a=b").unwrap();
        assert_eq!(name.as_ref().map(Ident::as_ref), Some("name"));
        assert_eq!(dir, PathBuf::from("./a=b"));
    }

    #[test]
    fn an_empty_declared_name_is_rejected() {
        assert!(split_declared_root("=src").is_err());
        let err = parse_error(&["src", "-o", "out", "--import==deps/core"]).message;
        assert!(err.contains("the name before '=' cannot be empty"), "{err}");
    }

    #[test]
    fn a_colon_is_part_of_the_path_not_a_name_separator() {
        let Ok(Command::Compile(parsed)) =
            parse(&args(&["pkg:src", "-o", "out", "--import=core:deps/core"]))
        else {
            panic!("expected compile command");
        };
        assert!(parsed.name.is_none());
        assert_eq!(parsed.entry_dir, PathBuf::from("pkg:src"));
        assert_eq!(parsed.externs[0].dir, PathBuf::from("core:deps/core"));
    }
}
