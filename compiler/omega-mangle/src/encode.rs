use std::collections::HashMap;
use std::fmt::Write as _;

use crate::base62;
use crate::grammar::*;
use crate::symbol::{ManglePath, MangleType, Symbol};

struct Encoder {
    out: String,
    path_subs: HashMap<ManglePath, usize>,
    type_subs: HashMap<MangleType, usize>,
}

pub fn encode(symbol: &Symbol) -> String {
    let mut encoder = Encoder::new();
    encoder.encode_path(&symbol.path);

    if let Some((params, return_type)) = &symbol.signature {
        encoder.encode_types(params);
        encoder.push_tag(TAG_LIST_END);
        encoder.encode_type(return_type);
    }

    if let Some(suffix) = &symbol.vendor_suffix {
        encoder.push_tag(VENDOR_SUFFIX_SEP);
        encoder.out.push_str(suffix);
    }

    encoder.out
}

impl Encoder {
    fn new() -> Self {
        Self {
            out: PREFIX.to_owned(),
            path_subs: HashMap::new(),
            type_subs: HashMap::new(),
        }
    }

    fn push_tag(&mut self, tag: u8) {
        self.out.push(char::from(tag));
    }

    fn emit_backref(&mut self, position: usize) {
        self.push_tag(TAG_BACKREF);
        let offset = u64::try_from(position)
            .expect("mangled symbol offsets must fit in the u64 backreference grammar");
        self.out.push_str(&base62::encode(offset));
    }

    fn encode_ident(&mut self, name: &str) {
        write!(self.out, "{}", name.len()).expect("writing to String cannot fail");
        if matches!(name.as_bytes().first(), Some(byte) if byte.is_ascii_digit() || *byte == b'_') {
            self.out.push('_');
        }
        self.out.push_str(name);
    }

    fn encode_path(&mut self, path: &ManglePath) {
        if let Some(&position) = self.path_subs.get(path) {
            self.emit_backref(position);
            return;
        }

        let start = self.out.len();
        match path {
            ManglePath::Root(name) => {
                self.push_tag(TAG_ROOT);
                self.encode_ident(name);
            }
            ManglePath::Nested(parent, namespace, name) => {
                self.push_tag(TAG_NESTED);
                self.push_tag(namespace_tag(*namespace));
                self.encode_path(parent);
                self.encode_ident(name);
            }
            ManglePath::Generic(parent, args) => {
                self.push_tag(TAG_GENERIC);
                self.encode_path(parent);
                self.encode_types(args);
                self.push_tag(TAG_LIST_END);
            }
            ManglePath::Type(ty) => {
                self.push_tag(TAG_TYPE_PATH);
                self.encode_type(ty);
            }
        }
        self.path_subs.insert(path.clone(), start);
    }

    fn encode_types(&mut self, types: &[MangleType]) {
        for ty in types {
            self.encode_type(ty);
        }
    }

    fn encode_type(&mut self, ty: &MangleType) {
        if let Some(tag) = basic_letter(ty) {
            self.push_tag(tag);
            return;
        }

        // A one-byte string tag is always shorter than a backreference.
        if let MangleType::Str(mutable) = ty {
            self.push_tag(if *mutable { TAG_STR_MUT } else { TAG_STR });
            return;
        }

        if let Some(&position) = self.type_subs.get(ty) {
            self.emit_backref(position);
            return;
        }

        let start = self.out.len();
        match ty {
            MangleType::Pointer(inner, mutable) => self.encode_wrapped_type(
                if *mutable {
                    TAG_POINTER_MUT
                } else {
                    TAG_POINTER
                },
                inner,
            ),
            MangleType::Slice(inner, mutable) => {
                self.encode_wrapped_type(if *mutable { TAG_SLICE_MUT } else { TAG_SLICE }, inner)
            }
            MangleType::Array(inner, mutable) => {
                self.encode_wrapped_type(if *mutable { TAG_ARRAY_MUT } else { TAG_ARRAY }, inner)
            }
            MangleType::SizedArray(inner, len) => {
                self.push_tag(TAG_SIZED_ARRAY);
                self.encode_type(inner);
                self.out.push_str(&base62::encode(*len));
            }
            MangleType::SpecObject(inner, mutable) => self.encode_wrapped_type(
                if *mutable {
                    TAG_SPEC_OBJECT_MUT
                } else {
                    TAG_SPEC_OBJECT
                },
                inner,
            ),
            MangleType::Function(params, return_type, variadic) => {
                self.push_tag(TAG_FUNCTION);
                if *variadic {
                    self.push_tag(TAG_VARIADIC);
                }
                self.encode_types(params);
                self.push_tag(TAG_LIST_END);
                self.encode_type(return_type);
            }
            MangleType::Named(path, None) => self.encode_path(path),
            MangleType::Named(path, Some(variant)) => {
                self.push_tag(TAG_REFINED);
                self.encode_path(path);
                self.out.push_str(&base62::encode(u64::from(*variant)));
            }
            _ => unreachable!("basic and string types return before substitution encoding"),
        }
        self.type_subs.insert(ty.clone(), start);
    }

    fn encode_wrapped_type(&mut self, tag: u8, inner: &MangleType) {
        self.push_tag(tag);
        self.encode_type(inner);
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
        let sym = Symbol {
            path,
            signature: Some((vec![], MangleType::Void)),
            vendor_suffix: None,
        };
        let out = encode(&sym);
        assert!(out.starts_with("_omg_"));
        assert!(out.contains("5mymod"));
        assert!(out.contains("3foo"));
    }

    #[test]
    fn identifier_starting_with_digit_gets_separator() {
        let path = nested(root("mymod"), Namespace::Value, "0foo");
        let sym = Symbol {
            path,
            signature: None,
            vendor_suffix: None,
        };
        let out = encode(&sym);
        assert!(out.contains("4_0foo"));
    }
}
