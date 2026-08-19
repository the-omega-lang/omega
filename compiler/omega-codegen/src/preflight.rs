use crate::CodegenRequest;
use omega_analyzer::resolved_type::ResolvedType;
use omega_mir::MirItem;

pub(crate) fn preflight(request: &CodegenRequest) -> Result<(), String> {
    for (_, module) in &request.modules {
        for item in &module.items {
            if let MirItem::ExternDeclaration(declaration) = item
                && !matches!(&declaration.r#type, ResolvedType::Function(_))
            {
                return Err(format!(
                    "extern data declarations (a non-function `extern`) are not implemented yet: '{}'",
                    declaration.ident.as_ref()
                ));
            }
        }
    }
    Ok(())
}
