use std::collections::HashMap;
use std::str;

use crate::base62;
use crate::grammar::*;
use crate::symbol::{
    FunctionSignature, MangleConvention, MangleGenericArg, MangleIntType, ManglePath, MangleType,
    MangleValue, Symbol,
};

pub fn decode(mangled: &str) -> Option<Symbol> {
    let bytes = mangled.as_bytes();
    if !bytes.starts_with(PREFIX.as_bytes()) {
        return None;
    }

    Decoder {
        bytes,
        pos: PREFIX.len(),
        path_substitutions: HashMap::new(),
        type_substitutions: HashMap::new(),
    }
    .parse_symbol()
}

struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    path_substitutions: HashMap<usize, ManglePath>,
    type_substitutions: HashMap<usize, MangleType>,
}

impl Decoder<'_> {
    fn parse_symbol(mut self) -> Option<Symbol> {
        let path = self.parse_path()?;
        let signature = match self.peek() {
            None | Some(VENDOR_SUFFIX_SEP) => None,
            Some(_) => Some(self.parse_signature()?),
        };
        let vendor_suffix = if self.consume_if(VENDOR_SUFFIX_SEP) {
            Some(str::from_utf8(self.remaining()).ok()?.to_owned())
        } else {
            None
        };

        (self.pos == self.bytes.len()).then_some(Symbol {
            path,
            signature,
            vendor_suffix,
        })
    }

    fn parse_signature(&mut self) -> Option<FunctionSignature> {
        let convention = self.parse_convention();
        let is_variadic = self.consume_if(TAG_VARIADIC);
        let params = self.parse_type_list()?;
        let return_type = self.parse_type()?;
        Some(FunctionSignature {
            params,
            return_type,
            is_variadic,
            convention,
        })
    }

    fn parse_convention(&mut self) -> MangleConvention {
        match self.peek().and_then(convention_from_tag) {
            Some(convention) => {
                self.pos += 1;
                convention
            }
            None => MangleConvention::Omega,
        }
    }

    fn parse_ident(&mut self) -> Option<String> {
        let length_start = self.pos;
        let mut length = 0usize;
        while let Some(byte @ b'0'..=b'9') = self.peek() {
            length = length
                .checked_mul(10)?
                .checked_add(usize::from(byte - b'0'))?;
            self.pos += 1;
        }
        if self.pos == length_start {
            return None;
        }

        self.consume_if(b'_');
        let ident = str::from_utf8(self.take(length)?).ok()?;
        Some(ident.to_owned())
    }

    fn parse_path(&mut self) -> Option<ManglePath> {
        let start = self.pos;
        if self.consume_if(TAG_BACKREF) {
            let offset = self.parse_backref_offset(start)?;
            return self.path_substitutions.get(&offset).cloned();
        }

        let path = match self.peek()? {
            TAG_ROOT => {
                self.pos += 1;
                ManglePath::Root(self.parse_ident()?)
            }
            TAG_NESTED => {
                self.pos += 1;
                let namespace = namespace_from_tag(self.next()?)?;
                let parent = self.parse_path()?;
                let name = self.parse_ident()?;
                ManglePath::Nested(Box::new(parent), namespace, name)
            }
            TAG_GENERIC => {
                self.pos += 1;
                let parent = self.parse_path()?;
                let args = self.parse_type_list()?;
                ManglePath::Generic(Box::new(parent), args)
            }
            TAG_GENERIC_MIXED => {
                self.pos += 1;
                let parent = self.parse_path()?;
                ManglePath::MixedGeneric(Box::new(parent), self.parse_generic_arg_list()?)
            }
            TAG_TYPE_PATH => {
                self.pos += 1;
                ManglePath::Type(Box::new(self.parse_type()?))
            }
            _ => return None,
        };

        self.path_substitutions.insert(start, path.clone());
        Some(path)
    }

    fn parse_type(&mut self) -> Option<MangleType> {
        let start = self.pos;
        let tag = self.peek()?;
        if let Some(basic) = basic_from_letter(tag) {
            self.pos += 1;
            return Some(basic);
        }

        if tag == TAG_STR || tag == TAG_STR_MUT {
            self.pos += 1;
            return Some(MangleType::Str(tag == TAG_STR_MUT));
        }

        if self.consume_if(TAG_BACKREF) {
            let offset = self.parse_backref_offset(start)?;
            if let Some(ty) = self.type_substitutions.get(&offset) {
                return Some(ty.clone());
            }

            // Named types are encoded directly as paths. If that path was already substituted,
            // the type therefore starts with a path backreference rather than a type backreference.
            let path = self.path_substitutions.get(&offset)?.clone();
            let ty = MangleType::Named(path, None);
            self.type_substitutions.insert(start, ty.clone());
            return Some(ty);
        }

        let ty = match tag {
            TAG_POINTER => self.parse_wrapped_type(false, MangleType::Pointer)?,
            TAG_POINTER_MUT => self.parse_wrapped_type(true, MangleType::Pointer)?,
            TAG_SLICE => self.parse_wrapped_type(false, MangleType::Slice)?,
            TAG_SLICE_MUT => self.parse_wrapped_type(true, MangleType::Slice)?,
            TAG_ARRAY => self.parse_wrapped_type(false, MangleType::Array)?,
            TAG_ARRAY_MUT => self.parse_wrapped_type(true, MangleType::Array)?,
            TAG_SIZED_ARRAY => {
                self.pos += 1;
                let item = self.parse_type()?;
                let length = base62::decode(self.bytes, &mut self.pos)?;
                MangleType::SizedArray(Box::new(item), length)
            }
            TAG_SPEC_OBJECT => {
                self.pos += 1;
                MangleType::SpecObject(vec![self.parse_type()?], false)
            }
            TAG_SPEC_OBJECT_MUT => {
                self.pos += 1;
                MangleType::SpecObject(vec![self.parse_type()?], true)
            }
            TAG_SPEC_OBJECT_SHAPE => {
                self.pos += 1;
                MangleType::SpecObject(self.parse_type_list()?, false)
            }
            TAG_SPEC_OBJECT_SHAPE_MUT => {
                self.pos += 1;
                MangleType::SpecObject(self.parse_type_list()?, true)
            }
            TAG_ANONYMOUS_ENUM => {
                self.pos += 1;
                MangleType::AnonymousEnum(self.parse_type_list()?, None)
            }
            TAG_ANONYMOUS_ENUM_REFINED => {
                self.pos += 1;
                let members = self.parse_type_list()?;
                let index = u32::try_from(base62::decode(self.bytes, &mut self.pos)?).ok()?;
                MangleType::AnonymousEnum(members, Some(index))
            }
            TAG_FUNCTION => {
                self.pos += 1;
                let convention = self.parse_convention();
                let variadic = self.consume_if(TAG_VARIADIC);
                let params = self.parse_type_list()?;
                let return_type = self.parse_type()?;
                MangleType::Function(params, Box::new(return_type), variadic, convention)
            }
            TAG_REFINED => {
                self.pos += 1;
                let path = self.parse_path()?;
                let variant = u32::try_from(base62::decode(self.bytes, &mut self.pos)?).ok()?;
                MangleType::Named(path, Some(variant))
            }
            TAG_ROOT | TAG_NESTED | TAG_GENERIC | TAG_GENERIC_MIXED | TAG_TYPE_PATH => {
                MangleType::Named(self.parse_path()?, None)
            }
            _ => return None,
        };

        self.type_substitutions.insert(start, ty.clone());
        Some(ty)
    }

    fn parse_wrapped_type(
        &mut self,
        mutable: bool,
        wrap: fn(Box<MangleType>, bool) -> MangleType,
    ) -> Option<MangleType> {
        self.pos += 1;
        Some(wrap(Box::new(self.parse_type()?), mutable))
    }

    fn parse_generic_arg_list(&mut self) -> Option<Vec<MangleGenericArg>> {
        let mut args = Vec::new();
        loop {
            if self.consume_if(TAG_LIST_END) {
                return Some(args);
            }
            args.push(match self.next()? {
                TAG_ARG_TYPE => MangleGenericArg::Type(self.parse_type()?),
                TAG_ARG_VALUE => MangleGenericArg::Value(self.parse_value()?),
                _ => return None,
            });
        }
    }

    fn parse_value(&mut self) -> Option<MangleValue> {
        let ty = basic_from_letter(self.next()?)?;
        let negative = self.consume_if(TAG_VALUE_NEGATIVE);
        let magnitude = base62::decode(self.bytes, &mut self.pos)?;
        Some(match ty {
            MangleType::Bool if !negative => MangleValue::Bool(match magnitude {
                0 => false,
                1 => true,
                _ => return None,
            }),
            MangleType::Char if !negative => {
                MangleValue::Char(char::from_u32(u32::try_from(magnitude).ok()?)?)
            }
            other => {
                let r#type = MangleIntType::from_mangle_type(&other)?;
                let magnitude = i128::from(magnitude);
                MangleValue::Int {
                    r#type,
                    value: if negative { -magnitude } else { magnitude },
                }
            }
        })
    }

    fn parse_type_list(&mut self) -> Option<Vec<MangleType>> {
        let mut types = Vec::new();
        loop {
            if self.consume_if(TAG_LIST_END) {
                return Some(types);
            }
            self.peek()?;
            types.push(self.parse_type()?);
        }
    }

    fn parse_backref_offset(&mut self, reference_start: usize) -> Option<usize> {
        let offset = usize::try_from(base62::decode(self.bytes, &mut self.pos)?).ok()?;
        (offset < reference_start).then_some(offset)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn consume_if(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn take(&mut self, length: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(length)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn remaining(&mut self) -> &[u8] {
        let remaining = &self.bytes[self.pos..];
        self.pos = self.bytes.len();
        remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_refined_variant_that_does_not_fit_u32() {
        let too_large = base62::encode(u64::from(u32::MAX) + 1);
        let symbol = format!("{PREFIX}C1xRC1x{too_large}Ev");
        assert!(decode(&symbol).is_none());
    }

    #[test]
    fn rejects_cyclic_backreference_expansion() {
        // The nested path starts at byte 5 and its parent backreferences byte 5. Resolving
        // raw offsets by reparsing would recurse forever; completed substitutions reject it.
        assert!(decode("_omg_NtB4_1x").is_none());
    }

    #[test]
    fn named_structural_path_round_trips_through_decoder() {
        let symbol = Symbol {
            path: ManglePath::Root("pkg".to_string()),
            signature: Some(FunctionSignature {
                params: vec![MangleType::Named(
                    ManglePath::Type(Box::new(MangleType::I32)),
                    None,
                )],
                return_type: MangleType::Void,
                is_variadic: false,
                convention: MangleConvention::Omega,
            }),
            vendor_suffix: None,
        };
        let mangled = crate::encode::encode(&symbol);
        assert_eq!(decode(&mangled), Some(symbol));
    }
}
