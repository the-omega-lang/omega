//! Single source of truth for every tag byte the grammar uses, shared by
//! `encode` and `demangle` so the two can never drift apart. See the
//! crate-level docs for the full grammar in EBNF form.
//!
//! Tag bytes are partitioned by case: lowercase is basic (primitive)
//! types plus the two namespace tags (`t`/`v`, only ever read right
//! after `N`); uppercase is path/type structural tags. No byte is ever
//! reused as both a leading tag and a trailing marker on another
//! production, so parsing is never positionally ambiguous.

use crate::symbol::MangleType;

pub const PREFIX: &str = "_omg_";

pub const TAG_ROOT: u8 = b'C';
pub const TAG_NESTED: u8 = b'N';
pub const TAG_GENERIC: u8 = b'I';
pub const TAG_BACKREF: u8 = b'B';
pub const TAG_LIST_END: u8 = b'E';

pub const TAG_POINTER: u8 = b'P';
pub const TAG_POINTER_MUT: u8 = b'Q';
pub const TAG_SLICE: u8 = b'S';
pub const TAG_SLICE_MUT: u8 = b'W';
pub const TAG_ARRAY: u8 = b'G';
pub const TAG_ARRAY_MUT: u8 = b'H';
pub const TAG_SIZED_ARRAY: u8 = b'A';
pub const TAG_SPEC_OBJECT: u8 = b'D';
pub const TAG_SPEC_OBJECT_MUT: u8 = b'K';
pub const TAG_FUNCTION: u8 = b'F';
pub const TAG_VARIADIC: u8 = b'V';
pub const TAG_REFINED: u8 = b'R';
pub const TAG_STR: u8 = b'T';
pub const TAG_STR_MUT: u8 = b'U';
/// A structural type promoted to a conformance-owner path.
pub const TAG_TYPE_PATH: u8 = b'X';

pub const VENDOR_SUFFIX_SEP: u8 = b'.';

/// `None` for a compound type (handled structurally by the caller), and
/// for anything represented as a `<path>` (structs/enums/unions/specs) --
/// those aren't "basic" in the grammar's sense at all.
pub fn basic_letter(ty: &MangleType) -> Option<u8> {
    Some(match ty {
        MangleType::Void => b'v',
        MangleType::Never => b'n',
        MangleType::Bool => b'b',
        MangleType::Char => b'c',
        MangleType::I8 => b'a',
        MangleType::I16 => b's',
        MangleType::I32 => b'l',
        MangleType::I64 => b'x',
        MangleType::ISize => b'z',
        MangleType::U8 => b'h',
        MangleType::U16 => b't',
        MangleType::U32 => b'm',
        MangleType::U64 => b'y',
        MangleType::USize => b'j',
        MangleType::F32 => b'f',
        MangleType::F64 => b'd',
        _ => return None,
    })
}

pub fn basic_from_letter(letter: u8) -> Option<MangleType> {
    Some(match letter {
        b'v' => MangleType::Void,
        b'n' => MangleType::Never,
        b'b' => MangleType::Bool,
        b'c' => MangleType::Char,
        b'a' => MangleType::I8,
        b's' => MangleType::I16,
        b'l' => MangleType::I32,
        b'x' => MangleType::I64,
        b'z' => MangleType::ISize,
        b'h' => MangleType::U8,
        b't' => MangleType::U16,
        b'm' => MangleType::U32,
        b'y' => MangleType::U64,
        b'j' => MangleType::USize,
        b'f' => MangleType::F32,
        b'd' => MangleType::F64,
        _ => return None,
    })
}
