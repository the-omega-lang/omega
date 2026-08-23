use super::*;

/// Whether a resolved type may currently be passed or returned **by value**
/// under a non-Omega calling convention.
///
/// Omega has no per-target platform ABI classifier yet (see the ABI entry in
/// `docs/issues/known-issues.md`), so the safe set is restricted to shapes
/// whose transported value is a single machine scalar under every supported
/// target. The `match` is deliberately exhaustive: a new `ResolvedType` must
/// force an explicit decision instead of silently becoming C-compatible.
fn supported_by_value_under_foreign_convention(r#type: &ResolvedType) -> bool {
    match r#type {
        ResolvedType::Void
        | ResolvedType::Never
        | ResolvedType::Bool
        | ResolvedType::Char
        | ResolvedType::I8
        | ResolvedType::I16
        | ResolvedType::I32
        | ResolvedType::I64
        | ResolvedType::ISize
        | ResolvedType::U8
        | ResolvedType::U16
        | ResolvedType::U32
        | ResolvedType::U64
        | ResolvedType::USize
        | ResolvedType::F32
        | ResolvedType::F64 => true,
        // A pointer transports the pointer itself, whatever it points at; an
        // unknown-size array is that same thin pointer. A function pointer is
        // safe to hand over even when invoking its pointee would not be --
        // that call is validated where it happens.
        ResolvedType::Pointer { .. } | ResolvedType::Array(_, _) | ResolvedType::Function(_) => {
            true
        }
        // Inline/composite or multi-leaf Omega representations. `Spec` is not
        // a value type at all and must never be mistaken for an FFI scalar.
        ResolvedType::SizedArray(_, _)
        | ResolvedType::Slice { .. }
        | ResolvedType::Str { .. }
        | ResolvedType::Struct(_)
        | ResolvedType::Union(_)
        | ResolvedType::Enum { .. }
        | ResolvedType::Spec(_)
        | ResolvedType::SpecObject { .. }
        | ResolvedType::AnonymousEnum { .. } => false,
    }
}

impl<'r> Analyzer<'r> {
    /// Validates that a function signature's by-value parameter/result shapes
    /// are supported by its own calling convention.
    ///
    /// ABI validity is a property of the resolved convention, never of
    /// `foreign` linkage: `CallingConvention::Omega` uses Omega's own
    /// `AbiSignature` contract and accepts any otherwise-valid value type,
    /// including across separately compiled objects.
    ///
    /// The variadic tail is not checked here; it has its own promotion rules.
    pub(crate) fn check_signature_abi(
        &mut self,
        id: HirId,
        span: Span,
        fn_type: &ResolvedFunctionType,
    ) -> bool {
        let convention = fn_type.calling_convention;
        if convention == CallingConvention::Omega {
            return true;
        }
        let unsupported = fn_type
            .params
            .iter()
            .map(|(_, ty)| ty)
            .chain(std::iter::once(&*fn_type.return_type))
            .find(|ty| !supported_by_value_under_foreign_convention(ty));
        match unsupported {
            Some(unsupported) => {
                self.error(
                    id,
                    span,
                    AnalysisErrorKind::UnsupportedConventionByValue {
                        r#type: unsupported.clone(),
                        convention,
                    },
                );
                false
            }
            None => true,
        }
    }
}

#[cfg(test)]
mod tests;
