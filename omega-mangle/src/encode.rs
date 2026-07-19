//! Turns a `Symbol` into a mangled string. Compression (RFC 2603's
//! byte-offset backref scheme) is built directly into the recursive
//! descent, following the RFC's own reference pseudocode: before
//! encoding a substitutable node, check whether it's already been
//! written; if so, emit a backref to where it started instead of
//! re-encoding it; otherwise encode it normally and record its start
//! position afterward. Because parents are always encoded before their
//! children, the longest available match is automatically preferred with
//! no extra bookkeeping.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::base62;
use crate::grammar::*;
use crate::symbol::{ManglePath, MangleType, Symbol};

struct Encoder {
    out: String,
    /// Byte offset (into `out`) where each already-encoded `ManglePath`
    /// began -- registered for *every* prefix, not just whole paths,
    /// since `encode_path` recurses into the parent before writing the
    /// child (see its own insert at the end of each call).
    path_subs: HashMap<ManglePath, usize>,
    /// Same idea for non-basic `MangleType`s.
    type_subs: HashMap<MangleType, usize>,
}

pub fn encode(symbol: &Symbol) -> String {
    let mut enc = Encoder { out: PREFIX.to_string(), path_subs: HashMap::new(), type_subs: HashMap::new() };
    enc.encode_path(&symbol.path);
    if let Some((params, ret)) = &symbol.signature {
        for p in params {
            enc.encode_type(p);
        }
        enc.out.push(TAG_LIST_END as char);
        enc.encode_type(ret);
    }
    if let Some(suffix) = &symbol.vendor_suffix {
        enc.out.push(VENDOR_SUFFIX_SEP as char);
        enc.out.push_str(suffix);
    }
    enc.out
}

impl Encoder {
    fn emit_backref(&mut self, pos: usize) {
        self.out.push(TAG_BACKREF as char);
        self.out.push_str(&base62::encode(pos as u64));
    }

    fn encode_ident(&mut self, name: &str) {
        write!(self.out, "{}", name.len()).expect("String write is infallible");
        if matches!(name.as_bytes().first(), Some(b) if b.is_ascii_digit() || *b == b'_') {
            self.out.push('_');
        }
        self.out.push_str(name);
    }

    fn encode_path(&mut self, path: &ManglePath) {
        if let Some(&pos) = self.path_subs.get(path) {
            self.emit_backref(pos);
            return;
        }
        let start = self.out.len();
        match path {
            ManglePath::Root(name) => {
                self.out.push(TAG_ROOT as char);
                self.encode_ident(name);
            }
            ManglePath::Nested(parent, ns, name) => {
                self.out.push(TAG_NESTED as char);
                self.out.push(ns.tag());
                self.encode_path(parent);
                self.encode_ident(name);
            }
            ManglePath::Generic(parent, args) => {
                self.out.push(TAG_GENERIC as char);
                self.encode_path(parent);
                for arg in args {
                    self.encode_type(arg);
                }
                self.out.push(TAG_LIST_END as char);
            }
        }
        self.path_subs.insert(path.clone(), start);
    }

    fn encode_type(&mut self, ty: &MangleType) {
        if let Some(letter) = basic_letter(ty) {
            self.out.push(letter as char);
            return;
        }
        if let Some(&pos) = self.type_subs.get(ty) {
            self.emit_backref(pos);
            return;
        }
        let start = self.out.len();
        match ty {
            MangleType::Pointer(inner, false) => {
                self.out.push(TAG_POINTER as char);
                self.encode_type(inner);
            }
            MangleType::Pointer(inner, true) => {
                self.out.push(TAG_POINTER_MUT as char);
                self.encode_type(inner);
            }
            MangleType::Slice(inner, false) => {
                self.out.push(TAG_SLICE as char);
                self.encode_type(inner);
            }
            MangleType::Slice(inner, true) => {
                self.out.push(TAG_SLICE_MUT as char);
                self.encode_type(inner);
            }
            MangleType::Array(inner) => {
                self.out.push(TAG_ARRAY as char);
                self.encode_type(inner);
            }
            MangleType::SizedArray(inner, len) => {
                self.out.push(TAG_SIZED_ARRAY as char);
                self.encode_type(inner);
                self.out.push_str(&base62::encode(*len));
            }
            MangleType::SpecObject(inner, false) => {
                self.out.push(TAG_SPEC_OBJECT as char);
                self.encode_type(inner);
            }
            MangleType::SpecObject(inner, true) => {
                self.out.push(TAG_SPEC_OBJECT_MUT as char);
                self.encode_type(inner);
            }
            MangleType::Function(params, ret, variadic) => {
                self.out.push(TAG_FUNCTION as char);
                if *variadic {
                    self.out.push(TAG_VARIADIC as char);
                }
                for p in params {
                    self.encode_type(p);
                }
                self.out.push(TAG_LIST_END as char);
                self.encode_type(ret);
            }
            MangleType::Named(path, None) => {
                self.encode_path(path);
            }
            MangleType::Named(path, Some(variant)) => {
                self.out.push(TAG_REFINED as char);
                self.encode_path(path);
                self.out.push_str(&base62::encode(*variant as u64));
            }
            _ => unreachable!("basic types are handled by the basic_letter early return above"),
        }
        self.type_subs.insert(ty.clone(), start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::Namespace;

    fn root(name: &str) -> ManglePath {
        ManglePath::Root(name.to_string())
    }

    fn nested(parent: ManglePath, ns: Namespace, name: &str) -> ManglePath {
        ManglePath::Nested(Box::new(parent), ns, name.to_string())
    }

    #[test]
    fn free_function_path() {
        let path = nested(root("mymod"), Namespace::Value, "foo");
        let sym = Symbol { path, signature: Some((vec![], MangleType::Void)), vendor_suffix: None };
        let out = encode(&sym);
        assert!(out.starts_with("_omg_"));
        assert!(out.contains("5mymod"));
        assert!(out.contains("3foo"));
    }

    #[test]
    fn identifier_starting_with_digit_gets_separator() {
        let path = nested(root("mymod"), Namespace::Value, "0foo");
        let sym = Symbol { path, signature: None, vendor_suffix: None };
        let out = encode(&sym);
        assert!(out.contains("4_0foo"));
    }
}
