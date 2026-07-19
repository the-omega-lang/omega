//! Parses a mangled string back into a `Symbol` (`decode`), and renders
//! that into a readable form (`demangle`). Backrefs are resolved exactly
//! per RFC 2603's own reference pseudocode: hitting a `B<offset>` token
//! re-invokes parsing at that byte offset in the *mangled* string (not
//! the output), using a fresh cursor, and the outer cursor only advances
//! past the backref token itself -- no separate decoded-position cache
//! needed. A backref's offset is required to be strictly less than where
//! the backref token itself starts, which is always true of anything an
//! honest encoder produces (a substitution can only ever point at a
//! position already fully written) and rejects malformed/adversarial
//! input (e.g. a self-referential or forward-pointing backref) instead
//! of looping or overflowing the stack.
//!
//! Rendering deliberately doesn't try to reconstruct Rust-style
//! `<Owner>::method(...)` bracketing -- reliably telling "this path
//! segment is a type" from "this path segment is just a module" apart
//! would need a third pseudo-namespace with no real Omega meaning behind
//! it, purely to serve cosmetics. A flat, fully `::`-qualified path
//! followed by `(params) -> return` is simpler, unambiguous, and just as
//! readable (RFC 2603 itself leaves the demangled *form* entirely up to
//! each demangler).

use crate::base62;
use crate::grammar::*;
use crate::symbol::{ManglePath, MangleType, Namespace, Symbol};

pub fn decode(mangled: &str) -> Option<Symbol> {
    let bytes = mangled.as_bytes();
    if !mangled.starts_with(PREFIX) {
        return None;
    }
    let mut pos = PREFIX.len();
    let path = parse_path(bytes, &mut pos)?;

    let signature = if bytes.get(pos) == Some(&VENDOR_SUFFIX_SEP) {
        None
    } else if pos < bytes.len() {
        let mut params = Vec::new();
        while bytes.get(pos) != Some(&TAG_LIST_END) {
            params.push(parse_type(bytes, &mut pos)?);
        }
        pos += 1; // consume 'E'
        let ret = parse_type(bytes, &mut pos)?;
        Some((params, ret))
    } else {
        None
    };

    let vendor_suffix = if bytes.get(pos) == Some(&VENDOR_SUFFIX_SEP) {
        Some(String::from_utf8(bytes[pos + 1..].to_vec()).ok()?)
    } else if pos == bytes.len() {
        None
    } else {
        return None;
    };

    Some(Symbol { path, signature, vendor_suffix })
}

pub fn demangle(mangled: &str) -> Option<String> {
    let symbol = decode(mangled)?;
    Some(render(&symbol))
}

fn parse_ident(bytes: &[u8], pos: &mut usize) -> Option<String> {
    let start = *pos;
    let mut len: usize = 0;
    while let Some(&b) = bytes.get(*pos) {
        if !b.is_ascii_digit() {
            break;
        }
        len = len.checked_mul(10)?.checked_add((b - b'0') as usize)?;
        *pos += 1;
    }
    if *pos == start {
        return None;
    }
    if bytes.get(*pos) == Some(&b'_') {
        *pos += 1;
    }
    let end = pos.checked_add(len)?;
    let slice = bytes.get(*pos..end)?;
    *pos = end;
    String::from_utf8(slice.to_vec()).ok()
}

fn parse_path(bytes: &[u8], pos: &mut usize) -> Option<ManglePath> {
    let backref_start = *pos;
    match *bytes.get(*pos)? {
        TAG_BACKREF => {
            *pos += 1;
            let offset = base62::decode(bytes, pos)? as usize;
            if offset >= backref_start {
                return None;
            }
            let mut sub_pos = offset;
            parse_path(bytes, &mut sub_pos)
        }
        TAG_ROOT => {
            *pos += 1;
            Some(ManglePath::Root(parse_ident(bytes, pos)?))
        }
        TAG_NESTED => {
            *pos += 1;
            let ns = Namespace::from_tag(*bytes.get(*pos)? as char)?;
            *pos += 1;
            let parent = parse_path(bytes, pos)?;
            let name = parse_ident(bytes, pos)?;
            Some(ManglePath::Nested(Box::new(parent), ns, name))
        }
        TAG_GENERIC => {
            *pos += 1;
            let parent = parse_path(bytes, pos)?;
            let mut args = Vec::new();
            while bytes.get(*pos) != Some(&TAG_LIST_END) {
                args.push(parse_type(bytes, pos)?);
            }
            *pos += 1;
            Some(ManglePath::Generic(Box::new(parent), args))
        }
        _ => None,
    }
}

fn parse_type(bytes: &[u8], pos: &mut usize) -> Option<MangleType> {
    let backref_start = *pos;
    let tag = *bytes.get(*pos)?;

    if let Some(basic) = basic_from_letter(tag) {
        *pos += 1;
        return Some(basic);
    }

    match tag {
        TAG_BACKREF => {
            *pos += 1;
            let offset = base62::decode(bytes, pos)? as usize;
            if offset >= backref_start {
                return None;
            }
            let mut sub_pos = offset;
            parse_type(bytes, &mut sub_pos)
        }
        TAG_POINTER => {
            *pos += 1;
            Some(MangleType::Pointer(Box::new(parse_type(bytes, pos)?), false))
        }
        TAG_POINTER_MUT => {
            *pos += 1;
            Some(MangleType::Pointer(Box::new(parse_type(bytes, pos)?), true))
        }
        TAG_SLICE => {
            *pos += 1;
            Some(MangleType::Slice(Box::new(parse_type(bytes, pos)?), false))
        }
        TAG_SLICE_MUT => {
            *pos += 1;
            Some(MangleType::Slice(Box::new(parse_type(bytes, pos)?), true))
        }
        TAG_ARRAY => {
            *pos += 1;
            Some(MangleType::Array(Box::new(parse_type(bytes, pos)?)))
        }
        TAG_STR => {
            *pos += 1;
            Some(MangleType::Str(false))
        }
        TAG_STR_MUT => {
            *pos += 1;
            Some(MangleType::Str(true))
        }
        TAG_SIZED_ARRAY => {
            *pos += 1;
            let inner = parse_type(bytes, pos)?;
            let len = base62::decode(bytes, pos)?;
            Some(MangleType::SizedArray(Box::new(inner), len))
        }
        TAG_SPEC_OBJECT => {
            *pos += 1;
            Some(MangleType::SpecObject(Box::new(parse_type(bytes, pos)?), false))
        }
        TAG_SPEC_OBJECT_MUT => {
            *pos += 1;
            Some(MangleType::SpecObject(Box::new(parse_type(bytes, pos)?), true))
        }
        TAG_FUNCTION => {
            *pos += 1;
            let variadic = if bytes.get(*pos) == Some(&TAG_VARIADIC) {
                *pos += 1;
                true
            } else {
                false
            };
            let mut params = Vec::new();
            while bytes.get(*pos) != Some(&TAG_LIST_END) {
                params.push(parse_type(bytes, pos)?);
            }
            *pos += 1;
            let ret = parse_type(bytes, pos)?;
            Some(MangleType::Function(params, Box::new(ret), variadic))
        }
        TAG_REFINED => {
            *pos += 1;
            let path = parse_path(bytes, pos)?;
            let variant = base62::decode(bytes, pos)? as u32;
            Some(MangleType::Named(path, Some(variant)))
        }
        TAG_ROOT | TAG_NESTED | TAG_GENERIC => Some(MangleType::Named(parse_path(bytes, pos)?, None)),
        _ => None,
    }
}

fn render(symbol: &Symbol) -> String {
    let mut s = render_path(&symbol.path);
    if let Some((params, ret)) = &symbol.signature {
        s.push('(');
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&render_type(p));
        }
        s.push_str(") -> ");
        s.push_str(&render_type(ret));
    }
    if let Some(suffix) = &symbol.vendor_suffix {
        s.push('.');
        s.push_str(suffix);
    }
    s
}

fn render_path(path: &ManglePath) -> String {
    match path {
        ManglePath::Root(name) => name.clone(),
        ManglePath::Nested(parent, _ns, name) => format!("{}::{}", render_path(parent), name),
        ManglePath::Generic(parent, args) => {
            let rendered_args: Vec<String> = args.iter().map(render_type).collect();
            format!("{}<{}>", render_path(parent), rendered_args.join(", "))
        }
    }
}

fn render_type(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".to_string(),
        MangleType::Bool => "bool".to_string(),
        MangleType::Char => "char".to_string(),
        MangleType::I8 => "i8".to_string(),
        MangleType::I16 => "i16".to_string(),
        MangleType::I32 => "i32".to_string(),
        MangleType::I64 => "i64".to_string(),
        MangleType::ISize => "isize".to_string(),
        MangleType::U8 => "u8".to_string(),
        MangleType::U16 => "u16".to_string(),
        MangleType::U32 => "u32".to_string(),
        MangleType::U64 => "u64".to_string(),
        MangleType::USize => "usize".to_string(),
        MangleType::F32 => "f32".to_string(),
        MangleType::F64 => "f64".to_string(),
        MangleType::Pointer(inner, false) => format!("*{}", render_type(inner)),
        MangleType::Pointer(inner, true) => format!("*mut {}", render_type(inner)),
        MangleType::Slice(inner, false) => format!("*[{}]", render_type(inner)),
        MangleType::Slice(inner, true) => format!("*mut [{}]", render_type(inner)),
        MangleType::Array(inner) => format!("[{}]", render_type(inner)),
        MangleType::Str(false) => "*str".to_string(),
        MangleType::Str(true) => "*mut str".to_string(),
        MangleType::SizedArray(inner, len) => format!("[{}; {len}]", render_type(inner)),
        MangleType::SpecObject(inner, false) => format!("spec *{}", render_type(inner)),
        MangleType::SpecObject(inner, true) => format!("spec *mut {}", render_type(inner)),
        MangleType::Function(params, ret, variadic) => {
            let mut rendered: Vec<String> = params.iter().map(render_type).collect();
            if *variadic {
                rendered.push("...".to_string());
            }
            format!("({}) => {}", rendered.join(", "), render_type(ret))
        }
        MangleType::Named(path, None) => render_path(path),
        MangleType::Named(path, Some(variant)) => format!("{}[#{variant}]", render_path(path)),
    }
}
