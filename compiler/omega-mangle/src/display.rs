use crate::decode::decode;
use crate::symbol::{
    MangleConvention, MangleGenericArg, ManglePath, MangleTemplate, MangleTemplateArg,
    MangleTemplateBound, MangleTemplateParam, MangleTemplateType, MangleType, MangleValue, Symbol,
};

pub fn demangle(mangled: &str) -> Option<String> {
    decode(mangled).map(|symbol| render(&symbol))
}

fn render_convention_prefix(convention: MangleConvention) -> &'static str {
    match convention {
        MangleConvention::Omega => "",
        MangleConvention::C => "foreign(c) ",
        MangleConvention::SysV64 => "foreign(sysv64) ",
    }
}

fn render(symbol: &Symbol) -> String {
    let mut rendered = render_path(&symbol.path);
    if let Some(signature) = &symbol.signature {
        rendered.push_str(render_convention_prefix(signature.convention));
        rendered.push('(');
        let mut params = signature.params.iter().map(render_type).collect::<Vec<_>>();
        if signature.is_variadic {
            params.push("...".to_string());
        }
        rendered.push_str(&params.join(", "));
        rendered.push_str(") -> ");
        rendered.push_str(&render_type(&signature.return_type));
    }
    if let Some(suffix) = &symbol.vendor_suffix {
        rendered.push('.');
        rendered.push_str(suffix);
    }
    rendered
}

fn render_path(path: &ManglePath) -> String {
    match path {
        ManglePath::Root(name) => name.clone(),
        ManglePath::Nested(parent, _namespace, name) => {
            format!("{}::{name}", render_path(parent))
        }
        ManglePath::Generic(parent, args) => {
            format!("{}<{}>", render_path(parent), render_types(args))
        }
        ManglePath::MixedGeneric(parent, args) => {
            let args: Vec<String> = args
                .iter()
                .map(|arg| match arg {
                    MangleGenericArg::Type(ty) => render_type(ty),
                    MangleGenericArg::Value(value) => render_value(value),
                })
                .collect();
            format!("{}<{}>", render_path(parent), args.join(", "))
        }
        ManglePath::Template(parent, template) => {
            format!("{}{{{}}}", render_path(parent), render_template(template))
        }
        ManglePath::Type(ty) => render_type(ty),
    }
}

/// The declaration a template-qualified symbol names, rendered so that two
/// overloads reachable at the same concrete arguments read differently.
///
/// Braces delimit it because nothing else in a rendered symbol uses them, so
/// a reader -- or a tool that only wants the instantiated path -- can find
/// and skip the whole descriptor without parsing it.
fn render_template(template: &MangleTemplate) -> String {
    let generics: Vec<String> = template
        .generics
        .iter()
        .enumerate()
        .map(|(index, generic)| match generic {
            MangleTemplateParam::Type(bounds) if bounds.is_empty() => format!("#{index}"),
            MangleTemplateParam::Type(bounds) => format!(
                "#{index}: {}",
                bounds
                    .iter()
                    .map(render_bound)
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
            MangleTemplateParam::Comp(value_type) => {
                format!("comp #{index}: {}", render_template_type(value_type))
            }
        })
        .collect();
    let mut params: Vec<String> = template.params.iter().map(render_template_type).collect();
    if template.is_variadic {
        params.push("...".to_string());
    }
    format!(
        "<{}>{}({}) => {}",
        generics.join(", "),
        render_convention_prefix(template.convention),
        params.join(", "),
        render_template_type(&template.return_type)
    )
}

fn render_bound(bound: &MangleTemplateBound) -> String {
    let path = render_path(&bound.spec);
    if bound.args.is_empty() {
        return path;
    }
    format!("{path}<{}>", render_template_args(&bound.args))
}

fn render_template_args(args: &[MangleTemplateArg]) -> String {
    args.iter()
        .map(|arg| match arg {
            MangleTemplateArg::Type(ty) => render_template_type(ty),
            MangleTemplateArg::Value(value) => render_value(value),
            MangleTemplateArg::Param(index) => format!("#{index}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_template_type(ty: &MangleTemplateType) -> String {
    match ty {
        MangleTemplateType::Param(index) => format!("#{index}"),
        MangleTemplateType::Fixed(fixed) => render_type(fixed),
        MangleTemplateType::Pointer(inner, false) => format!("*{}", render_template_type(inner)),
        MangleTemplateType::Pointer(inner, true) => {
            format!("*mut {}", render_template_type(inner))
        }
        MangleTemplateType::Slice(inner, false) => format!("*[]{}", render_template_type(inner)),
        MangleTemplateType::Slice(inner, true) => {
            format!("*mut []{}", render_template_type(inner))
        }
        MangleTemplateType::Array(inner, false) => format!("*[?]{}", render_template_type(inner)),
        MangleTemplateType::Array(inner, true) => {
            format!("*mut [?]{}", render_template_type(inner))
        }
        MangleTemplateType::SizedArray(inner, length) => format!(
            "[{}]{}",
            render_template_args(std::slice::from_ref(length.as_ref())),
            render_template_type(inner)
        ),
        MangleTemplateType::SpecObject(members, mutable) => format!(
            "*{}spec {}",
            if *mutable { "mut " } else { "" },
            members
                .iter()
                .map(render_bound)
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        MangleTemplateType::AnonymousEnum(members) => format!(
            "enum {}",
            members
                .iter()
                .map(render_template_type)
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        MangleTemplateType::Function(params, return_type, variadic, convention) => {
            let mut params: Vec<String> = params.iter().map(render_template_type).collect();
            if *variadic {
                params.push("...".to_string());
            }
            format!(
                "{}({}) => {}",
                render_convention_prefix(*convention),
                params.join(", "),
                render_template_type(return_type)
            )
        }
        MangleTemplateType::Nominal(path, args) if args.is_empty() => render_path(path),
        MangleTemplateType::Nominal(path, args) => {
            format!("{}<{}>", render_path(path), render_template_args(args))
        }
    }
}

fn render_value(value: &MangleValue) -> String {
    match value {
        MangleValue::Int { value, .. } => value.to_string(),
        MangleValue::Bool(value) => value.to_string(),
        MangleValue::Char(value) => format!("'{value}'"),
    }
}

fn render_types(types: &[MangleType]) -> String {
    types.iter().map(render_type).collect::<Vec<_>>().join(", ")
}

fn render_type(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".to_string(),
        MangleType::Never => "never".to_string(),
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
        MangleType::Slice(inner, false) => format!("*[]{}", render_type(inner)),
        MangleType::Slice(inner, true) => format!("*mut []{}", render_type(inner)),
        MangleType::Array(inner, false) => format!("*[?]{}", render_type(inner)),
        MangleType::Array(inner, true) => format!("*mut [?]{}", render_type(inner)),
        MangleType::Str(false) => "*str".to_string(),
        MangleType::Str(true) => "*mut str".to_string(),
        MangleType::SizedArray(inner, len) => format!("[{len}]{}", render_type(inner)),
        MangleType::SpecObject(members, mutable) => {
            let members: Vec<String> = members.iter().map(render_type).collect();
            format!(
                "*{}spec {}",
                if *mutable { "mut " } else { "" },
                members.join(" + ")
            )
        }
        MangleType::AnonymousEnum(members, refinement) => {
            let members: Vec<String> = members.iter().map(render_type).collect();
            let rendered = format!("enum {}", members.join(" | "));
            match refinement {
                Some(index) => format!("{rendered}[#{index}]"),
                None => rendered,
            }
        }
        MangleType::Function(params, return_type, variadic, convention) => {
            let mut rendered_params = params.iter().map(render_type).collect::<Vec<_>>();
            if *variadic {
                rendered_params.push("...".to_string());
            }
            format!(
                "{}({}) => {}",
                render_convention_prefix(*convention),
                rendered_params.join(", "),
                render_type(return_type)
            )
        }
        MangleType::Named(path, None) => render_path(path),
        MangleType::Named(path, Some(variant)) => {
            format!("{}[#{variant}]", render_path(path))
        }
    }
}
