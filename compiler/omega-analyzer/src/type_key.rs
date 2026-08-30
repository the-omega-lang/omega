//! The one deterministic structural identity key for a `ResolvedType`.
//!
//! Anonymous-enum members and spec conjunctions are both unordered sets that
//! must nevertheless have a single canonical order, because that order decides
//! layout, tags, vtable slots, and mangled symbols. The key here provides it.
//!
//! Two properties are load-bearing and easy to lose:
//!
//! * it never observes `HirId`, pointer addresses, or discovery order, so two
//!   compilations -- and two separately compiled packages -- agree;
//! * it distinguishes every difference `ResolvedType`'s own `PartialEq`
//!   distinguishes, so sorting by key groups equal types adjacently and
//!   deduplication is exact.
//!
//! `Display` satisfies neither: it prints a bare nominal name with no module
//! path and no generic arguments, so `Vec<i32>` and `Vec<f64>` render alike.

use crate::resolved_type::{
    CallingConvention, ResolvedFunctionType, ResolvedGenericArg, ResolvedSpecApplication,
    ResolvedType,
};
use omega_parser::prelude::{Ident, SelfMode};

pub fn structural_key(ty: &ResolvedType) -> String {
    let mut out = String::new();
    write_type(&mut out, ty);
    out
}

pub fn spec_application_key(application: &ResolvedSpecApplication) -> String {
    let mut out = String::new();
    write_spec_application(&mut out, application);
    out
}

/// Identifiers cannot contain any of the delimiters used below, so nominal
/// names and paths need no escaping.
fn write_nominal(out: &mut String, tag: char, module_path: &[Ident], name: &Ident) {
    out.push(tag);
    for segment in module_path {
        out.push_str(segment.as_ref());
        out.push('.');
    }
    out.push_str(name.as_ref());
}

fn write_args(out: &mut String, args: &[ResolvedGenericArg]) {
    out.push('<');
    for arg in args {
        match arg {
            ResolvedGenericArg::Type(r#type) => write_type(out, r#type),
            // A distinct tag so a value can never collide with a type key,
            // and the declared type is part of the key so the same digits
            // under two different `comp` parameter types stay distinct.
            ResolvedGenericArg::Comp(value) => {
                out.push('=');
                write_type(out, &value.resolved_type());
                out.push(':');
                out.push_str(&value.to_string());
            }
        }
        out.push(',');
    }
    out.push('>');
}

fn write_mutability(out: &mut String, mutable: bool) {
    out.push(if mutable { 'm' } else { 'c' });
}

fn write_type(out: &mut String, ty: &ResolvedType) {
    match ty {
        ResolvedType::Void => out.push_str("v0"),
        ResolvedType::Never => out.push_str("v1"),
        ResolvedType::Bool => out.push_str("v2"),
        ResolvedType::Char => out.push_str("v3"),
        ResolvedType::I8 => out.push_str("i1"),
        ResolvedType::I16 => out.push_str("i2"),
        ResolvedType::I32 => out.push_str("i3"),
        ResolvedType::I64 => out.push_str("i4"),
        ResolvedType::ISize => out.push_str("i5"),
        ResolvedType::U8 => out.push_str("u1"),
        ResolvedType::U16 => out.push_str("u2"),
        ResolvedType::U32 => out.push_str("u3"),
        ResolvedType::U64 => out.push_str("u4"),
        ResolvedType::USize => out.push_str("u5"),
        ResolvedType::F32 => out.push_str("f1"),
        ResolvedType::F64 => out.push_str("f2"),
        ResolvedType::Pointer { pointee, mutable } => {
            out.push('p');
            write_mutability(out, *mutable);
            write_type(out, pointee);
        }
        ResolvedType::Array(item, mutable) => {
            out.push('a');
            write_mutability(out, *mutable);
            write_type(out, item);
        }
        ResolvedType::Slice { item, mutable } => {
            out.push('l');
            write_mutability(out, *mutable);
            write_type(out, item);
        }
        ResolvedType::Str { mutable } => {
            out.push('s');
            write_mutability(out, *mutable);
        }
        ResolvedType::SizedArray(item, size) => {
            out.push('r');
            out.push_str(&size.to_string());
            out.push(':');
            write_type(out, item);
        }
        ResolvedType::Function(fn_type) => write_function(out, fn_type),
        ResolvedType::Struct(cell) => {
            let cell = cell.borrow();
            write_nominal(out, 'S', &cell.module_path, &cell.name);
            write_args(out, &cell.generic_args);
        }
        ResolvedType::Union(cell) => {
            let cell = cell.borrow();
            write_nominal(out, 'U', &cell.module_path, &cell.name);
            write_args(out, &cell.generic_args);
        }
        ResolvedType::Enum { cell, variant } => {
            let cell = cell.borrow();
            write_nominal(out, 'E', &cell.module_path, &cell.name);
            write_args(out, &cell.generic_args);
            // The refined form is a distinct `ResolvedType` even though it
            // shares the parent's representation, so it must key distinctly.
            match variant {
                Some(index) => {
                    out.push('#');
                    out.push_str(&index.to_string());
                }
                None => out.push('.'),
            }
        }
        ResolvedType::Spec(cell) => {
            let cell = cell.borrow();
            write_nominal(out, 'P', &cell.module_path, &cell.name);
            write_args(out, &cell.generic_args);
        }
        ResolvedType::SpecObject { shape, mutable } => {
            out.push('O');
            write_mutability(out, *mutable);
            out.push('[');
            for member in &shape.members {
                write_spec_application(out, member);
                out.push(',');
            }
            out.push(']');
        }
        ResolvedType::AnonymousEnum { shape, variant } => {
            out.push('A');
            out.push('[');
            for member in shape.members() {
                write_type(out, member);
                out.push(',');
            }
            out.push(']');
            match variant {
                Some(index) => {
                    out.push('#');
                    out.push_str(&index.to_string());
                }
                None => out.push('.'),
            }
        }
    }
}

fn write_function(out: &mut String, fn_type: &ResolvedFunctionType) {
    out.push('F');
    out.push(match fn_type.calling_convention {
        CallingConvention::Omega => 'o',
        CallingConvention::C => 'c',
        CallingConvention::SysV64 => 's',
    });
    out.push(match fn_type.self_mode {
        None => 'n',
        Some(SelfMode::Value) => 'v',
        Some(SelfMode::MutValue) => 'V',
        Some(SelfMode::Pointer) => 'p',
        Some(SelfMode::MutPointer) => 'P',
    });
    out.push(if fn_type.is_variadic { '.' } else { '_' });
    out.push('(');
    // Parameter descriptors are not part of `ResolvedFunctionType`'s
    // equality, so encoding them here would split one type across two keys.
    for param in fn_type.param_types() {
        write_type(out, param);
        out.push(',');
    }
    out.push(')');
    write_type(out, &fn_type.return_type);
}

fn write_spec_application(out: &mut String, application: &ResolvedSpecApplication) {
    let spec = application.spec.borrow();
    write_nominal(out, 'P', &spec.module_path, &spec.name);
    write_args(out, &application.spec_args);
}
