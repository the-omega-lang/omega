//! The canonical structural identity of a generic function *declaration*.
//!
//! A generic instantiation's linker name is otherwise its path, its concrete
//! arguments, and its resulting signature. Explicit bound selection lets one
//! call reach either of two declarations at the *same* arguments, so those
//! three no longer identify which body a symbol refers to. This descriptor
//! supplies the missing component.
//!
//! It is taken before the declaration's own generic parameters are
//! substituted: `<M: A>` and `<M: B>` must differ even where both are
//! instantiated at one type, and a bound naming the declaration's own
//! parameter must differ from one fixed to that parameter's eventual
//! argument. Parameter *names*, source order of overloads, source location,
//! visibility, bodies, and the parameters' own defaults are deliberately not
//! part of it: they do not change which declaration a caller selected.

use crate::generics::pattern::{ArgumentPattern, SpecPattern, TypePattern};
use crate::resolved_type::{
    CallingConvention, CompIntType, CompScalar, ResolvedGenericArg, ResolvedType,
};
use crate::resolver::OverloadTemplate;
use omega_parser::prelude::Ident;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateDescriptor {
    /// The declaration's own generic parameters, in declared order.
    pub generics: Vec<TemplateParam>,
    pub params: Vec<TemplateType>,
    pub return_type: TemplateType,
    pub is_variadic: bool,
    pub convention: CallingConvention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateParam {
    /// A type parameter and the exact bound set it declares, canonically
    /// ordered so that `A + B` and `B + A` are one declaration.
    Type(Vec<TemplateBound>),
    /// A `comp` parameter and its declared value type.
    Comp(TemplateType),
}

/// One spec application, by the spec's full declared package/module path so
/// that two modules declaring the same short name stay distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateBound {
    pub module_path: Vec<Ident>,
    pub name: Ident,
    pub args: Vec<TemplateArg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateArg {
    Type(TemplateType),
    Value(CompScalar),
    /// A reference to the declaration's own `comp` parameter at this index.
    ValueParam(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateType {
    /// A reference to the declaration's own type parameter at this index.
    Param(usize),
    /// A type the declaration fixes, independent of its own parameters.
    Fixed(ResolvedType),
    Pointer(Box<Self>, bool),
    Slice(Box<Self>, bool),
    Array(Box<Self>, bool),
    SizedArray(Box<Self>, Box<TemplateArg>),
    SpecObject(Vec<TemplateBound>, bool),
    AnonymousEnum(Vec<Self>),
    Function(Vec<Self>, Box<Self>, CallingConvention, bool),
    /// An application of a nominal declaration, by its absolute item path.
    Nominal(Vec<Ident>, Vec<TemplateArg>),
}

impl TemplateDescriptor {
    /// The identity of the declaration `template` was built from.
    ///
    /// `template` must have been built in the declaration's own context --
    /// its module, and for a method the owner instantiation it belongs to --
    /// with its own generic parameters still unsubstituted.
    pub fn of(template: &OverloadTemplate) -> Option<Self> {
        let mut generics = Vec::with_capacity(template.generics.len());
        for (index, generic) in template.generics.iter().enumerate() {
            generics.push(if generic.is_comp() {
                TemplateParam::Comp(convert_type(template.comp_types[index].as_ref()?))
            } else {
                let mut bounds: Vec<TemplateBound> = Vec::new();
                for (parameter, spec) in &template.bounds {
                    if *parameter != index {
                        continue;
                    }
                    let bound = convert_bound(spec);
                    if !bounds.contains(&bound) {
                        bounds.push(bound);
                    }
                }
                canonicalize(&mut bounds);
                TemplateParam::Type(bounds)
            });
        }
        Some(Self {
            generics,
            params: template.params.iter().map(convert_type).collect(),
            return_type: convert_type(&template.return_type),
            is_variadic: template.is_variadic,
            convention: template.calling_convention,
        })
    }
}

/// A bound set is unordered, so it gets one order here. The key is
/// structural in exactly the sense [`crate::type_key`] requires: it observes
/// no `HirId`, allocation address, or discovery order, so two compilations
/// of one declaration agree.
fn canonicalize(bounds: &mut Vec<TemplateBound>) {
    bounds.sort_by_cached_key(bound_key);
    bounds.dedup();
}

fn bound_key(bound: &TemplateBound) -> String {
    let mut out = String::new();
    write_bound(&mut out, bound);
    out
}

fn write_bound(out: &mut String, bound: &TemplateBound) {
    for segment in &bound.module_path {
        out.push_str(segment.as_ref());
        out.push('.');
    }
    out.push_str(bound.name.as_ref());
    out.push('<');
    for arg in &bound.args {
        write_arg(out, arg);
        out.push(',');
    }
    out.push('>');
}

fn write_arg(out: &mut String, arg: &TemplateArg) {
    match arg {
        TemplateArg::Type(ty) => write_type(out, ty),
        TemplateArg::Value(value) => {
            out.push('=');
            out.push_str(&crate::type_key::structural_key(&value.resolved_type()));
            out.push(':');
            out.push_str(&value.to_string());
        }
        TemplateArg::ValueParam(index) => {
            out.push('$');
            out.push_str(&index.to_string());
        }
    }
}

fn write_type(out: &mut String, ty: &TemplateType) {
    match ty {
        TemplateType::Param(index) => {
            out.push('#');
            out.push_str(&index.to_string());
        }
        TemplateType::Fixed(fixed) => {
            out.push('!');
            out.push_str(&crate::type_key::structural_key(fixed));
        }
        TemplateType::Pointer(inner, mutable) => write_wrapped(out, 'p', *mutable, inner),
        TemplateType::Slice(inner, mutable) => write_wrapped(out, 's', *mutable, inner),
        TemplateType::Array(inner, mutable) => write_wrapped(out, 'a', *mutable, inner),
        TemplateType::SizedArray(inner, length) => {
            out.push('[');
            write_arg(out, length);
            out.push(']');
            write_type(out, inner);
        }
        TemplateType::SpecObject(members, mutable) => {
            write_mutability(out, 'o', *mutable);
            for member in members {
                write_bound(out, member);
                out.push(',');
            }
            out.push(')');
        }
        TemplateType::AnonymousEnum(members) => {
            out.push('e');
            for member in members {
                write_type(out, member);
                out.push('|');
            }
            out.push(')');
        }
        TemplateType::Function(params, return_type, convention, variadic) => {
            out.push('f');
            out.push(match convention {
                CallingConvention::Omega => '0',
                CallingConvention::C => '1',
                CallingConvention::SysV64 => '2',
            });
            out.push(if *variadic { 'v' } else { '-' });
            for param in params {
                write_type(out, param);
                out.push(',');
            }
            out.push(')');
            write_type(out, return_type);
        }
        TemplateType::Nominal(path, args) => {
            out.push('n');
            for segment in path {
                out.push_str(segment.as_ref());
                out.push('.');
            }
            out.push('<');
            for arg in args {
                write_arg(out, arg);
                out.push(',');
            }
            out.push('>');
        }
    }
}

fn write_wrapped(out: &mut String, tag: char, mutable: bool, inner: &TemplateType) {
    write_mutability(out, tag, mutable);
    write_type(out, inner);
}

fn write_mutability(out: &mut String, tag: char, mutable: bool) {
    out.push(tag);
    out.push(if mutable { 'm' } else { 'c' });
}

fn convert_bound(spec: &SpecPattern) -> TemplateBound {
    TemplateBound {
        module_path: spec.module_path.clone(),
        name: spec.name.clone(),
        args: spec.args.iter().map(convert_arg).collect(),
    }
}

fn convert_arg(arg: &ArgumentPattern) -> TemplateArg {
    match arg {
        ArgumentPattern::Type(pattern) => TemplateArg::Type(convert_type(pattern)),
        ArgumentPattern::Value(value) => TemplateArg::Value(*value),
        // Only a `comp` parameter ever reaches an argument slot as a bare
        // parameter reference; a type parameter arrives as `Type(Parameter)`.
        ArgumentPattern::Parameter(index, _) => TemplateArg::ValueParam(*index),
    }
}

fn convert_type(pattern: &TypePattern) -> TemplateType {
    match pattern {
        TypePattern::Fixed(ty) => convert_fixed(ty),
        TypePattern::Parameter(index) => TemplateType::Param(*index),
        TypePattern::Pointer(inner, mutable) => {
            TemplateType::Pointer(Box::new(convert_type(inner)), *mutable)
        }
        TypePattern::Slice(inner, mutable) => {
            TemplateType::Slice(Box::new(convert_type(inner)), *mutable)
        }
        TypePattern::Array(inner, mutable) => {
            TemplateType::Array(Box::new(convert_type(inner)), *mutable)
        }
        TypePattern::SizedArray(inner, length) => {
            TemplateType::SizedArray(Box::new(convert_type(inner)), Box::new(convert_arg(length)))
        }
        TypePattern::Nominal(path, args) => {
            TemplateType::Nominal(path.clone(), args.iter().map(convert_arg).collect())
        }
        TypePattern::Function(params, return_type, convention, variadic) => TemplateType::Function(
            params.iter().map(convert_type).collect(),
            Box::new(convert_type(return_type)),
            *convention,
            *variadic,
        ),
        TypePattern::AnonymousEnum(members) => canonical_enum(members.iter().map(convert_type)),
        TypePattern::SpecObject(members, mutable) => {
            let mut members: Vec<TemplateBound> = members.iter().map(convert_bound).collect();
            canonicalize(&mut members);
            TemplateType::SpecObject(members, *mutable)
        }
    }
}

fn canonical_enum(members: impl Iterator<Item = TemplateType>) -> TemplateType {
    let mut flattened = Vec::new();
    for member in members {
        match member {
            TemplateType::AnonymousEnum(inner) => flattened.extend(inner),
            TemplateType::Fixed(ResolvedType::AnonymousEnum { shape, .. }) => {
                flattened.extend(shape.members().iter().map(convert_fixed));
            }
            member => flattened.push(member),
        }
    }
    flattened.sort_by_cached_key(|member| {
        let mut key = String::new();
        write_type(&mut key, member);
        key
    });
    flattened.dedup();
    TemplateType::AnonymousEnum(flattened)
}

fn convert_resolved_arg(arg: &ResolvedGenericArg) -> TemplateArg {
    match arg {
        ResolvedGenericArg::Type(ty) => TemplateArg::Type(convert_fixed(ty)),
        ResolvedGenericArg::Comp(value) => TemplateArg::Value(*value),
    }
}

fn convert_nominal(module: &[Ident], name: &Ident, args: &[ResolvedGenericArg]) -> TemplateType {
    TemplateType::Nominal(
        module
            .iter()
            .cloned()
            .chain(std::iter::once(name.clone()))
            .collect(),
        args.iter().map(convert_resolved_arg).collect(),
    )
}

// A fixed leaf may come from an owner substitution or an omitted nominal
// default. Its structure must agree with the same type written explicitly.
fn convert_fixed(ty: &ResolvedType) -> TemplateType {
    match ty {
        ResolvedType::Pointer { pointee, mutable } => {
            TemplateType::Pointer(Box::new(convert_fixed(pointee)), *mutable)
        }
        ResolvedType::Slice { item, mutable } => {
            TemplateType::Slice(Box::new(convert_fixed(item)), *mutable)
        }
        ResolvedType::Array(item, mutable) => {
            TemplateType::Array(Box::new(convert_fixed(item)), *mutable)
        }
        ResolvedType::SizedArray(item, size) => TemplateType::SizedArray(
            Box::new(convert_fixed(item)),
            Box::new(TemplateArg::Value(CompScalar::Int {
                r#type: CompIntType::USize,
                value: i128::from(*size),
            })),
        ),
        ResolvedType::Function(function) if function.self_mode.is_none() => TemplateType::Function(
            function.param_types().map(convert_fixed).collect(),
            Box::new(convert_fixed(&function.return_type)),
            function.calling_convention,
            function.is_variadic,
        ),
        ResolvedType::Struct(cell) => {
            let cell = cell.borrow();
            convert_nominal(&cell.module_path, &cell.name, &cell.generic_args)
        }
        ResolvedType::Union(cell) => {
            let cell = cell.borrow();
            convert_nominal(&cell.module_path, &cell.name, &cell.generic_args)
        }
        ResolvedType::Enum {
            cell,
            variant: None,
        } => {
            let cell = cell.borrow();
            convert_nominal(&cell.module_path, &cell.name, &cell.generic_args)
        }
        ResolvedType::Spec(cell) => {
            let cell = cell.borrow();
            convert_nominal(&cell.module_path, &cell.name, &cell.generic_args)
        }
        ResolvedType::SpecObject { shape, mutable } => {
            let mut bounds = shape
                .members
                .iter()
                .map(|member| {
                    let spec = member.spec.borrow();
                    TemplateBound {
                        module_path: spec.module_path.clone(),
                        name: spec.name.clone(),
                        args: member.spec_args.iter().map(convert_resolved_arg).collect(),
                    }
                })
                .collect();
            canonicalize(&mut bounds);
            TemplateType::SpecObject(bounds, *mutable)
        }
        ResolvedType::AnonymousEnum {
            shape,
            variant: None,
        } => canonical_enum(shape.members().iter().map(convert_fixed)),
        _ => TemplateType::Fixed(ty.clone()),
    }
}
