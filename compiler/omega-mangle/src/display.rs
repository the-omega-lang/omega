use crate::decode::decode;
use crate::symbol::{
    MangleConvention, MangleGenericArg, ManglePath, MangleType, MangleValue, Symbol,
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
        ManglePath::Type(ty) => render_type(ty),
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
