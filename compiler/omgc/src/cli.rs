use omega_analyzer::Target;
use omega_codegen::{BackendKind, EmitKind, OptLevel};
use omega_diagnostics::{BOLD, CYAN, paint};
use omega_driver::{ExternRoot, basename};
use omega_parser::prelude::Ident;
use std::io::IsTerminal;
use std::path::PathBuf;

pub(crate) enum Command {
    Help,
    Compile(Args),
}

pub(crate) struct Args {
    pub(crate) entry_dir: PathBuf,
    pub(crate) output_file: PathBuf,
    pub(crate) externs: Vec<ExternRoot>,
    pub(crate) name: Option<Ident>,
    pub(crate) opt_level: OptLevel,
    pub(crate) target: Target,
    pub(crate) emit: EmitKind,
    pub(crate) backend: BackendKind,
    pub(crate) verbose: bool,
}

pub(crate) fn parse(args: &[String]) -> Result<Command, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(Command::Help);
    }

    parse_compile(args).map(Command::Compile)
}

fn parse_compile(args: &[String]) -> Result<Args, String> {
    let mut entry_dir = None;
    let mut output_file = None;
    let mut externs = Vec::new();
    let mut name = None;
    let mut opt_level = OptLevel::default();
    let mut target = Target::DEFAULT;
    let mut emit = EmitKind::default();
    let mut backend = BackendKind::default();
    let mut verbose = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--extern=") {
            externs.push(parse_extern(arg, value)?);
        } else if let Some(value) = arg.strip_prefix("--name=") {
            if value.is_empty() {
                return Err(format!(
                    "invalid --name flag '{arg}': the name cannot be empty"
                ));
            }
            name = Some(Ident(value.to_string()));
        } else if arg == "-o" {
            let file = iter
                .next()
                .ok_or_else(|| "expected a file path after '-o'".to_string())?;
            output_file = Some(PathBuf::from(file));
        } else if let Some(value) = arg.strip_prefix("-O") {
            opt_level = value.parse()?;
        } else if let Some(value) = arg.strip_prefix("--target=") {
            target = Target::parse(value).map_err(|error| error.to_string())?;
        } else if let Some(value) = arg.strip_prefix("--emit=") {
            emit = value.parse()?;
        } else if let Some(value) = arg.strip_prefix("--backend=") {
            backend = value.parse()?;
        } else if arg == "-v" || arg == "--verbose" {
            verbose = true;
        } else if arg.starts_with('-') {
            return Err(format!("unknown flag '{arg}'"));
        } else if entry_dir.is_some() {
            return Err(format!(
                "unexpected extra argument '{arg}' (the entry directory was already given)"
            ));
        } else {
            entry_dir = Some(PathBuf::from(arg));
        }
    }

    let entry_dir = entry_dir.ok_or_else(|| {
        "usage: omgc <entry-dir> -o <output-file> [OPTIONS] (see --help)".to_string()
    })?;
    let output_file = output_file.ok_or_else(|| "the -o <file> flag is required".to_string())?;

    Ok(Args {
        entry_dir,
        output_file,
        externs,
        name,
        opt_level,
        target,
        emit,
        backend,
        verbose,
    })
}

fn parse_extern(flag: &str, value: &str) -> Result<ExternRoot, String> {
    let (explicit_name, dir) = split_extern(value)
        .map_err(|reason| format!("invalid --extern flag '{flag}': {reason}"))?;
    let Some(physical_name) = basename(&dir) else {
        return Err(format!(
            "invalid --extern flag '{flag}': '{}' has no usable directory name",
            dir.display()
        ));
    };

    Ok(ExternRoot {
        name: explicit_name.unwrap_or(physical_name),
        dir,
    })
}

fn split_extern(value: &str) -> Result<(Option<Ident>, PathBuf), String> {
    // A drive letter is part of a bare Windows path, not an explicit module
    // identity (`--extern=C:\\...`). Explicit identities still work with
    // Windows paths: `--extern=core:C:\\...` splits at the first colon.
    if is_windows_absolute_path(value) {
        return Ok((None, PathBuf::from(value)));
    }

    match value.split_once(':') {
        Some(("", _)) => Err("the name before ':' cannot be empty".to_string()),
        Some((name, dir)) => Ok((Some(Ident(name.to_string())), PathBuf::from(dir))),
        None => Ok((None, PathBuf::from(value))),
    }
}

fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn help_option(colors: bool, flag: &str, desc: &str) {
    let padded = format!("{flag:<26}");
    println!("    {} {desc}", paint(colors, CYAN, &padded));
}

pub(crate) fn print_help() {
    let colors = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    println!("{}", paint(colors, BOLD, "omgc"));
    println!("The Omega compiler\n");
    println!("{}", paint(colors, BOLD, "USAGE:"));
    println!("    omgc <entry-dir> -o <output-file> [OPTIONS]\n");
    println!("{}", paint(colors, BOLD, "OPTIONS:"));
    help_option(colors, "-o <file>", "Output file path (required)");
    help_option(colors, "-O<0-3>", "Optimization level (default: 0)");
    help_option(
        colors,
        "--target=<triplet>",
        &format!(
            "Target triplet, e.g. x86_64-unknown-linux (default: {})",
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
        "--backend=<name>",
        &format!(
            "Codegen backend to use (default: {}; available: {})",
            BackendKind::default(),
            BackendKind::ALL
                .iter()
                .map(BackendKind::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        ),
    );
    help_option(
        colors,
        "--extern=[<name>:]<dir>",
        "Register an external module root (repeatable; name defaults to the directory basename)",
    );
    help_option(
        colors,
        "--name=<name>",
        "Override the local project's declared identity (default: entry directory basename)",
    );
    help_option(colors, "-v, --verbose", "Print progress information");
    help_option(colors, "-h, --help", "Print this help message");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn help_does_not_require_compile_arguments() {
        assert!(matches!(parse(&args(&["--help"])), Ok(Command::Help)));
    }

    #[test]
    fn parses_minimal_compile_command() {
        let Ok(Command::Compile(parsed)) = parse(&args(&["src", "-o", "out.o"])) else {
            panic!("expected compile command");
        };
        assert_eq!(parsed.entry_dir, PathBuf::from("src"));
        assert_eq!(parsed.output_file, PathBuf::from("out.o"));
        assert_eq!(parsed.opt_level, OptLevel::O0);
        assert_eq!(parsed.emit, EmitKind::Obj);
    }

    #[test]
    fn parses_codegen_options_and_explicit_extern_identity() {
        let Ok(Command::Compile(parsed)) = parse(&args(&[
            "src",
            "-o",
            "out.s",
            "-O3",
            "--emit=asm",
            "--backend=cranelift",
            "--extern=core:deps/core",
            "--verbose",
        ])) else {
            panic!("expected compile command");
        };
        assert_eq!(parsed.opt_level, OptLevel::O3);
        assert_eq!(parsed.emit, EmitKind::Asm);
        assert_eq!(parsed.backend, BackendKind::Cranelift);
        assert!(parsed.verbose);
        assert_eq!(parsed.externs.len(), 1);
        assert_eq!(parsed.externs[0].name.as_ref(), "core");
        assert_eq!(parsed.externs[0].dir, PathBuf::from("deps/core"));
    }

    #[test]
    fn rejects_invalid_codegen_options() {
        for invalid in ["-Ofast", "--emit=wat", "--backend=missing"] {
            assert!(parse(&args(&["src", "-o", "out.o", invalid])).is_err());
        }
    }

    #[test]
    fn rejects_missing_output_and_extra_positionals() {
        assert!(parse(&args(&["src"])).is_err());
        assert!(parse(&args(&["src", "other", "-o", "out.o"])).is_err());
    }

    #[test]
    fn windows_drive_letter_is_not_parsed_as_an_extern_name() {
        let (name, dir) = split_extern(r"C:\omega\core").unwrap();
        assert!(name.is_none());
        assert_eq!(dir, PathBuf::from(r"C:\omega\core"));

        let (name, dir) = split_extern(r"core:C:\omega\core").unwrap();
        assert_eq!(name.as_ref().map(Ident::as_ref), Some("core"));
        assert_eq!(dir, PathBuf::from(r"C:\omega\core"));
    }
}
