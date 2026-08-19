use omega_analyzer::Target;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    #[cfg(feature = "cranelift")]
    Cranelift,
    #[cfg(feature = "llvm")]
    Llvm,
}

impl BackendKind {
    pub const ALL: &'static [BackendKind] = &[
        #[cfg(feature = "cranelift")]
        BackendKind::Cranelift,
        #[cfg(feature = "llvm")]
        BackendKind::Llvm,
    ];

    /// Compatibility helper; new callers can use `name.parse::<BackendKind>()`.
    pub fn parse(name: &str) -> Result<Self, String> {
        name.parse()
    }

    pub fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Cranelift => "cranelift",
            #[cfg(feature = "llvm")]
            Self::Llvm => "llvm",
        }
    }

    pub fn supports(self, target: Target) -> bool {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Cranelift => crate::cranelift::supports(target),
            #[cfg(feature = "llvm")]
            Self::Llvm => crate::llvm::supports(target),
        }
    }

    pub fn supported_targets(self) -> &'static str {
        match self {
            #[cfg(feature = "cranelift")]
            Self::Cranelift => "x86_64, aarch64",
            #[cfg(feature = "llvm")]
            Self::Llvm => "x86_64, x86, armv7, thumbv7em, aarch64, riscv32, riscv64",
        }
    }
}

impl FromStr for BackendKind {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|backend| backend.name() == name)
            .ok_or_else(|| {
                let available = Self::ALL
                    .iter()
                    .map(|backend| backend.name())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown backend '{name}' (available: {available})")
            })
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(feature = "cranelift")]
impl Default for BackendKind {
    fn default() -> Self {
        Self::Cranelift
    }
}

#[cfg(all(not(feature = "cranelift"), feature = "llvm"))]
impl Default for BackendKind {
    fn default() -> Self {
        Self::Llvm
    }
}
