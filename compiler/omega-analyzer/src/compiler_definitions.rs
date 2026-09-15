//! Compiler definitions: the immutable configuration one compiler invocation
//! selects declarations with. A definition is a value, never a name in any
//! Omega namespace: nothing here resolves, allocates, or reaches the program
//! being compiled.

use crate::resolved_type::NumericKind;
use crate::target::Target;
use omega_parser::prelude::{AnnotationLiteral, Ident, NumberBase, NumberExpr};
use std::collections::HashMap;
use std::fmt;

/// A decoded definition value. It keeps the kind and declared numeric width
/// it was written with, because those decide which comparisons are meaningful
/// and which literals fit.
#[derive(Debug, Clone, PartialEq)]
pub enum DefinitionValue {
    Bool(bool),
    /// The mathematical value, so widths and signedness compare without
    /// wrapping or converting through a float.
    Int {
        value: i128,
        r#type: IntType,
    },
    /// Already rounded to `width`; an `f32` is stored as the `f64` it
    /// promotes to exactly.
    Float {
        value: f64,
        width: u32,
    },
    Char(char),
    Str(String),
    ByteStr(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntType {
    pub signed: bool,
    pub width: u32,
}

impl DefinitionValue {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "a boolean",
            Self::Int { .. } => "an integer",
            Self::Float { .. } => "a float",
            Self::Char(_) => "a character",
            Self::Str(_) => "a string",
            Self::ByteStr(_) => "a byte string",
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The numeric kind an already-decoded value was established with, which
    /// is what an unsuffixed literal compared against it adopts.
    pub fn numeric_kind(&self) -> Option<NumericKind> {
        match self {
            Self::Int { r#type, .. } if r#type.signed => Some(NumericKind::Signed(r#type.width)),
            Self::Int { r#type, .. } => Some(NumericKind::Unsigned(r#type.width)),
            Self::Float { width, .. } => Some(NumericKind::Float(*width)),
            _ => None,
        }
    }
}

impl fmt::Display for DefinitionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Int { value, .. } => write!(f, "{value}"),
            Self::Float { value, .. } => write!(f, "{value}"),
            Self::Char(value) => write!(f, "'{value}'"),
            Self::Str(value) => write!(f, "\"{value}\""),
            Self::ByteStr(value) => write!(f, "b\"{value}\""),
        }
    }
}

/// The configuration every source read by one invocation is selected with.
#[derive(Debug, Clone)]
pub struct CompilerDefinitions {
    target: Target,
    user: HashMap<Ident, DefinitionValue>,
}

/// The builtin definitions, which describe the selected target rather than
/// the host. They live in their own namespace: a supplied definition of the
/// same spelling defines `def::target_os`, and never replaces `target_os`.
pub const BUILTIN_NAMES: [&str; 4] = [
    "target_os",
    "target_arch",
    "target_pointer_width",
    "target_freestanding",
];

impl CompilerDefinitions {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            user: HashMap::new(),
        }
    }

    /// Adds one definition. A configuration has exactly one value per name,
    /// so a repeated name is refused rather than overwritten; which input
    /// supplied each one, and how to say so, belongs to whoever collected
    /// them.
    #[must_use = "a refused definition is a configuration conflict the caller must report"]
    pub fn define(&mut self, name: Ident, value: DefinitionValue) -> bool {
        match self.user.entry(name) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(value);
                true
            }
        }
    }

    pub fn target(&self) -> Target {
        self.target
    }

    /// A user definition, or `None` when none was supplied. Absence is not
    /// falsehood: only a boolean-expected position turns it into `false`.
    pub fn user(&self, name: &Ident) -> Option<&DefinitionValue> {
        self.user.get(name)
    }

    pub fn builtin(&self, name: &Ident) -> Option<DefinitionValue> {
        Some(match name.as_ref() {
            "target_os" => DefinitionValue::Str(self.target.os.name().to_string()),
            "target_arch" => DefinitionValue::Str(self.target.arch.name().to_string()),
            "target_pointer_width" => DefinitionValue::Int {
                value: i128::from(self.target.pointer_bits()),
                r#type: IntType {
                    signed: false,
                    width: 32,
                },
            },
            "target_freestanding" => {
                DefinitionValue::Bool(self.target.os == crate::target::Os::None)
            }
            _ => return None,
        })
    }
}

/// Decodes a written literal into a value. Only the numeric forms depend on
/// the target, through `usize`/`isize`.
pub fn decode_literal(
    literal: &AnnotationLiteral,
    pointer_bits: u32,
) -> Result<DefinitionValue, LiteralValueError> {
    match literal {
        AnnotationLiteral::Bool(value) => Ok(DefinitionValue::Bool(*value)),
        AnnotationLiteral::Char(value) => Ok(DefinitionValue::Char(*value)),
        AnnotationLiteral::Str(value) => Ok(DefinitionValue::Str(value.clone())),
        AnnotationLiteral::ByteStr(value) => Ok(DefinitionValue::ByteStr(value.clone())),
        AnnotationLiteral::Number { negative, value } => {
            let kind = declared_numeric_kind(value, pointer_bits)?
                .unwrap_or_else(|| default_numeric_kind(value));
            decode_number(value, *negative, kind)
        }
    }
}

/// The numeric kind an unsuffixed literal takes when nothing else establishes
/// one, matching ordinary Omega literal defaults.
pub fn default_numeric_kind(value: &NumberExpr) -> NumericKind {
    if value.fractional_part.is_some() {
        NumericKind::Float(32)
    } else {
        NumericKind::Signed(32)
    }
}

/// The kind a written suffix establishes, or `None` when the literal carries
/// no suffix. Unlike ordinary analysis this cannot resolve a type name, so
/// only the primitive numeric spellings are accepted.
pub fn declared_numeric_kind(
    value: &NumberExpr,
    pointer_bits: u32,
) -> Result<Option<NumericKind>, LiteralValueError> {
    let Some(suffix) = &value.explicit_type else {
        return Ok(None);
    };
    let kind = match suffix.as_ref() {
        "i8" => NumericKind::Signed(8),
        "i16" => NumericKind::Signed(16),
        "i32" => NumericKind::Signed(32),
        "i64" => NumericKind::Signed(64),
        "isize" => NumericKind::Signed(pointer_bits),
        "u8" => NumericKind::Unsigned(8),
        "u16" => NumericKind::Unsigned(16),
        "u32" => NumericKind::Unsigned(32),
        "u64" => NumericKind::Unsigned(64),
        "usize" => NumericKind::Unsigned(pointer_bits),
        "f32" => NumericKind::Float(32),
        "f64" => NumericKind::Float(64),
        other => return Err(LiteralValueError::UnknownSuffix(other.to_string())),
    };
    Ok(Some(kind))
}

/// Decodes a number at a known kind. The sign is kept apart from the
/// magnitude so a signed minimum decodes without ever being representable as
/// its own positive counterpart.
pub fn decode_number(
    value: &NumberExpr,
    negative: bool,
    kind: NumericKind,
) -> Result<DefinitionValue, LiteralValueError> {
    let is_float = matches!(kind, NumericKind::Float(_));
    if value.fractional_part.is_some() && !is_float {
        return Err(LiteralValueError::FractionalInteger);
    }
    if is_float && value.base != NumberBase::Decimal {
        return Err(LiteralValueError::NonDecimalFloat);
    }

    let text = || match &value.fractional_part {
        Some(fraction) => format!("{}.{}", value.integer_part, fraction),
        None => value.integer_part.clone(),
    };

    match kind {
        NumericKind::Float(width) => {
            let literal = text();
            // Parsing through f64 can double-round decimals near an f32 midpoint.
            let rounded = if width == 32 {
                literal.parse::<f32>().map(f64::from)
            } else {
                literal.parse::<f64>()
            }
            .map_err(|_| LiteralValueError::Malformed(literal.clone()))?;
            if !rounded.is_finite() {
                return Err(LiteralValueError::NotFinite {
                    literal: text(),
                    width,
                });
            }
            Ok(DefinitionValue::Float {
                value: if negative { -rounded } else { rounded },
                width,
            })
        }
        NumericKind::Signed(width) | NumericKind::Unsigned(width) => {
            let signed = matches!(kind, NumericKind::Signed(_));
            let magnitude = u128::from_str_radix(&value.integer_part, value.base.radix())
                .map_err(|_| LiteralValueError::Malformed(text()))?;
            let out_of_range = || LiteralValueError::OutOfRange {
                literal: if negative {
                    format!("-{}", text())
                } else {
                    text()
                },
                width,
                signed,
            };
            if negative && !signed {
                return Err(LiteralValueError::NegativeUnsigned(text()));
            }
            let limit: u128 = match (signed, negative) {
                (false, _) => (1u128 << width) - 1,
                (true, false) => (1u128 << (width - 1)) - 1,
                (true, true) => 1u128 << (width - 1),
            };
            if magnitude > limit {
                return Err(out_of_range());
            }
            let value = if negative {
                -(magnitude as i128)
            } else {
                magnitude as i128
            };
            Ok(DefinitionValue::Int {
                value,
                r#type: IntType { signed, width },
            })
        }
    }
}

#[derive(Debug, Clone)]
pub enum LiteralValueError {
    UnknownSuffix(String),
    FractionalInteger,
    NonDecimalFloat,
    NegativeUnsigned(String),
    Malformed(String),
    NotFinite {
        literal: String,
        width: u32,
    },
    OutOfRange {
        literal: String,
        width: u32,
        signed: bool,
    },
}

impl fmt::Display for LiteralValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSuffix(suffix) => write!(
                f,
                "'{suffix}' is not a numeric type -- expected one of i8, i16, i32, i64, isize, u8, \
                 u16, u32, u64, usize, f32, or f64"
            ),
            Self::FractionalInteger => {
                f.write_str("a fractional number needs a float type, not an integer one")
            }
            Self::NonDecimalFloat => {
                f.write_str("a float must be written in decimal, not in another base")
            }
            Self::NegativeUnsigned(literal) => {
                write!(
                    f,
                    "'-{literal}' is negative, so it is not an unsigned value"
                )
            }
            Self::Malformed(literal) => write!(f, "'{literal}' is not a valid number"),
            Self::NotFinite { literal, width } => {
                write!(f, "'{literal}' is not a finite f{width} value")
            }
            Self::OutOfRange {
                literal,
                width,
                signed,
            } => write!(
                f,
                "'{literal}' does not fit in {} {width}-bit value",
                if *signed { "a signed" } else { "an unsigned" }
            ),
        }
    }
}

#[cfg(test)]
mod tests;
