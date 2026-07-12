//! A compilation target, in Omega's own vocabulary -- deliberately
//! decoupled from Cranelift's own `target_lexicon::Triple`, so a future
//! non-Cranelift backend could consume the same `Target` without knowing
//! anything Cranelift-specific. Only this module (and `Codegen::generate`'s
//! ISA-building step) ever names `target_lexicon` types; everything else
//! in this crate, and every caller outside it, deals only in `Target`.

use std::fmt;
use target_lexicon::{Architecture, Environment, OperatingSystem, Triple, Vendor};

/// `<arch>-<os>`, e.g. `x86_64-unknown-linux` -- structurally, not just a
/// string forwarded to whichever backend happens to be compiled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub arch: Arch,
    pub os: Os,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Target {
    /// `x86_64-unknown-linux` -- today's hardcoded target, preserved as
    /// the default when `--target` isn't given.
    pub const DEFAULT: Target = Target { arch: Arch::X86_64, os: Os::Linux };

    /// Parses `<arch>-<vendor>-<os>` or `<arch>-<os>` -- the vendor segment
    /// (when present) is accepted but ignored: Omega doesn't need it for
    /// anything the OS alone doesn't already decide (see `to_triple`'s
    /// per-OS vendor/environment/binary-format defaults). Both
    /// `x86_64-unknown-linux` (today's hardcoded string) and the bare
    /// `x86_64-linux` parse to `Target::DEFAULT`.
    pub fn parse(s: &str) -> Result<Target, TargetParseError> {
        let segments: Vec<&str> = s.split('-').collect();
        let (arch_str, os_str) = match segments.as_slice() {
            [arch, os] => (*arch, *os),
            [arch, _vendor, os] => (*arch, *os),
            _ => return Err(TargetParseError::Malformed(s.to_string())),
        };

        let arch = match arch_str {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            other => return Err(TargetParseError::UnknownArch(other.to_string())),
        };
        let os = match os_str {
            "linux" => Os::Linux,
            "macos" | "darwin" => Os::MacOs,
            "windows" => Os::Windows,
            other => return Err(TargetParseError::UnknownOs(other.to_string())),
        };
        Ok(Target { arch, os })
    }

    /// The Cranelift-specific translation, kept private to this module --
    /// nothing outside `omega-codegen` should ever need a
    /// `target_lexicon::Triple`. Each OS gets the vendor/environment/
    /// binary-format combination its own platform actually uses (e.g. ELF
    /// + GNU on Linux, Mach-O + Apple on macOS); the user-facing `Target`
    /// stays deliberately simpler than Cranelift's own 5-field `Triple`
    /// because Omega has no use for those extra axes today.
    pub(crate) fn to_triple(self) -> Triple {
        let architecture = match self.arch {
            Arch::X86_64 => Architecture::X86_64,
            Arch::Aarch64 => Architecture::Aarch64(target_lexicon::Aarch64Architecture::Aarch64),
        };
        let (vendor, operating_system, environment, binary_format) = match self.os {
            Os::Linux => {
                (Vendor::Unknown, OperatingSystem::Linux, Environment::Gnu, target_lexicon::BinaryFormat::Elf)
            }
            Os::MacOs => (
                Vendor::Apple,
                OperatingSystem::MacOSX(None),
                Environment::Unknown,
                target_lexicon::BinaryFormat::Macho,
            ),
            Os::Windows => {
                (Vendor::Pc, OperatingSystem::Windows, Environment::Msvc, target_lexicon::BinaryFormat::Coff)
            }
        };
        Triple { architecture, vendor, operating_system, environment, binary_format }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arch = match self.arch {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        };
        let os = match self.os {
            Os::Linux => "linux",
            Os::MacOs => "macos",
            Os::Windows => "windows",
        };
        write!(f, "{arch}-unknown-{os}")
    }
}

#[derive(Debug, Clone)]
pub enum TargetParseError {
    /// Not `<arch>-<os>` or `<arch>-<vendor>-<os>`.
    Malformed(String),
    UnknownArch(String),
    UnknownOs(String),
}

impl fmt::Display for TargetParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetParseError::Malformed(s) => {
                write!(f, "'{s}' is not a valid target triplet (expected `<arch>-<os>`, e.g. `x86_64-linux`)")
            }
            TargetParseError::UnknownArch(a) => write!(f, "unknown target architecture '{a}' (expected `x86_64` or `aarch64`)"),
            TargetParseError::UnknownOs(o) => write!(f, "unknown target OS '{o}' (expected `linux`, `macos`, or `windows`)"),
        }
    }
}
