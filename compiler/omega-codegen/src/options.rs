use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    #[default]
    O0,
    O1,
    O2,
    O3,
}

impl FromStr for OptLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "0" => Ok(Self::O0),
            "1" => Ok(Self::O1),
            "2" => Ok(Self::O2),
            "3" => Ok(Self::O3),
            other => Err(format!(
                "invalid optimization level '-O{other}': expected -O0, -O1, -O2, or -O3"
            )),
        }
    }
}

impl fmt::Display for OptLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::O0 => "0",
            Self::O1 => "1",
            Self::O2 => "2",
            Self::O3 => "3",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitKind {
    #[default]
    Obj,
    Ir,
    Asm,
}

impl FromStr for EmitKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "obj" => Ok(Self::Obj),
            "ir" => Ok(Self::Ir),
            "asm" => Ok(Self::Asm),
            other => Err(format!(
                "invalid --emit value '{other}': expected obj, ir, or asm"
            )),
        }
    }
}

impl fmt::Display for EmitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Obj => "obj",
            Self::Ir => "ir",
            Self::Asm => "asm",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_optimization_levels() {
        for (source, level) in [
            ("0", OptLevel::O0),
            ("1", OptLevel::O1),
            ("2", OptLevel::O2),
            ("3", OptLevel::O3),
        ] {
            assert_eq!(source.parse(), Ok(level));
            assert_eq!(level.to_string(), source);
        }
        assert!("fast".parse::<OptLevel>().is_err());
    }

    #[test]
    fn parses_and_displays_emit_kinds() {
        for (source, kind) in [
            ("obj", EmitKind::Obj),
            ("ir", EmitKind::Ir),
            ("asm", EmitKind::Asm),
        ] {
            assert_eq!(source.parse(), Ok(kind));
            assert_eq!(kind.to_string(), source);
        }
        assert!("binary".parse::<EmitKind>().is_err());
    }
}
