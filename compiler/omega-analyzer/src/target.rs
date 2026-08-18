
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub arch: Arch,
    pub os: Os,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    X86,
    Armv7,
    Thumbv7em,
    Aarch64,
    Riscv32,
    Riscv64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    None,
    Linux,
    MacOs,
    Windows,
}

impl Target {
    pub const DEFAULT: Target = Target { arch: Arch::X86_64, os: Os::Linux };

    pub fn pointer_bytes(self) -> u32 {
        match self.arch {
            Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 => 8,
            Arch::X86 | Arch::Armv7 | Arch::Thumbv7em | Arch::Riscv32 => 4,
        }
    }

    pub fn pointer_bits(self) -> u32 {
        self.pointer_bytes() * 8
    }

    pub fn parse(s: &str) -> Result<Target, TargetParseError> {
        let segments: Vec<&str> = s.split('-').collect();
        let (arch_str, os_str) = match segments.as_slice() {
            [arch, os] => (*arch, *os),
            [arch, _vendor, os] => (*arch, *os),
            _ => return Err(TargetParseError::Malformed(s.to_string())),
        };

        let arch = match arch_str {
            "x86_64" => Arch::X86_64,
            "x86" | "i386" | "i686" => Arch::X86,
            "armv7" | "arm" => Arch::Armv7,
            "thumbv7em" | "thumbv7" => Arch::Thumbv7em,
            "aarch64" => Arch::Aarch64,
            "riscv32" => Arch::Riscv32,
            "riscv64" => Arch::Riscv64,
            other => return Err(TargetParseError::UnknownArch(other.to_string())),
        };
        let os = match os_str {
            "none" | "freestanding" => Os::None,
            "linux" => Os::Linux,
            "macos" | "darwin" => Os::MacOs,
            "windows" => Os::Windows,
            other => return Err(TargetParseError::UnknownOs(other.to_string())),
        };
        Ok(Target { arch, os })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arch = match self.arch {
            Arch::X86_64 => "x86_64",
            Arch::X86 => "x86",
            Arch::Armv7 => "armv7",
            Arch::Thumbv7em => "thumbv7em",
            Arch::Aarch64 => "aarch64",
            Arch::Riscv32 => "riscv32",
            Arch::Riscv64 => "riscv64",
        };
        let os = match self.os {
            Os::None => "none",
            Os::Linux => "linux",
            Os::MacOs => "macos",
            Os::Windows => "windows",
        };
        write!(f, "{arch}-unknown-{os}")
    }
}

#[derive(Debug, Clone)]
pub enum TargetParseError {
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
            TargetParseError::UnknownArch(a) => write!(
                f,
                "unknown target architecture '{a}' (expected `x86_64`, `x86`, `armv7`, `thumbv7em`, `aarch64`, `riscv32`, or `riscv64`)"
            ),
            TargetParseError::UnknownOs(o) => write!(
                f,
                "unknown target OS '{o}' (expected `none`/`freestanding`, `linux`, `macos`, or `windows`)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(arch: Arch, os: Os) -> String {
        Target { arch, os }.to_string().replace("-unknown-", "-")
    }

    fn round_trips(s: &str, arch: Arch, os: Os) {
        let parsed = Target::parse(s).unwrap_or_else(|e| panic!("{s} should parse: {e}"));
        assert_eq!(parsed.arch, arch);
        assert_eq!(parsed.os, os);
        assert_eq!(parsed.to_string().replace("-unknown-", "-"), canonical(arch, os));
    }

    #[test]
    fn every_arch_and_os_parses_and_displays() {
        round_trips("x86_64-linux", Arch::X86_64, Os::Linux);
        round_trips("x86_64-macos", Arch::X86_64, Os::MacOs);
        round_trips("x86_64-windows", Arch::X86_64, Os::Windows);
        round_trips("x86-linux", Arch::X86, Os::Linux);
        round_trips("armv7-none", Arch::Armv7, Os::None);
        round_trips("thumbv7em-none", Arch::Thumbv7em, Os::None);
        round_trips("aarch64-linux", Arch::Aarch64, Os::Linux);
        round_trips("riscv32-none", Arch::Riscv32, Os::None);
        round_trips("riscv64-linux", Arch::Riscv64, Os::Linux);
        round_trips("riscv32-freestanding", Arch::Riscv32, Os::None);
        round_trips("x86_64-unknown-linux", Arch::X86_64, Os::Linux);
        round_trips("x86_64-unknown-none", Arch::X86_64, Os::None);
        round_trips("i386-unknown-linux", Arch::X86, Os::Linux);
        round_trips("arm-unknown-none", Arch::Armv7, Os::None);
        round_trips("thumbv7-unknown-none", Arch::Thumbv7em, Os::None);
        round_trips("x86_64-unknown-darwin", Arch::X86_64, Os::MacOs);
    }

    #[test]
    fn pointer_widths_are_per_arch() {
        for (arch, bytes) in [
            (Arch::X86_64, 8),
            (Arch::Aarch64, 8),
            (Arch::Riscv64, 8),
            (Arch::X86, 4),
            (Arch::Armv7, 4),
            (Arch::Thumbv7em, 4),
            (Arch::Riscv32, 4),
        ] {
            assert_eq!(Target { arch, os: Os::Linux }.pointer_bytes(), bytes, "{arch:?}");
            assert_eq!(Target { arch, os: Os::Linux }.pointer_bits(), bytes * 8, "{arch:?}");
        }
    }

    #[test]
    fn unknown_names_list_the_supported_set() {
        let arch = Target::parse("sparc-linux").unwrap_err().to_string();
        assert!(arch.contains("x86_64") && arch.contains("riscv32"), "{arch}");
        let os = Target::parse("x86_64-vxworks").unwrap_err().to_string();
        assert!(os.contains("none") && os.contains("windows"), "{os}");
        assert!(Target::parse("just")
            .unwrap_err()
            .to_string()
            .contains("not a valid target triplet"));
    }
}
