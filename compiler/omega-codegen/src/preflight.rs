use crate::CodegenRequest;

/// Historically rejected non-function `extern` data declarations here; the
/// backend now emits them as external-global declarations from the shared
/// catalog, so there is nothing left to preflight-reject for foreign items
/// specifically.
pub(crate) fn preflight(_request: &CodegenRequest) -> Result<(), String> {
    Ok(())
}
