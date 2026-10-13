use super::*;
use crate::core_contract::{self, CoreFeature, ReflectionFacts};

impl<'r> Analyzer<'r> {
    fn core_unavailable(&mut self, id: HirId, span: Span, feature: CoreFeature, detail: String) {
        self.error(
            id,
            span,
            AnalysisErrorKind::CoreSupportUnavailable { feature, detail },
        );
    }

    fn core_type(
        &mut self,
        id: HirId,
        span: Span,
        feature: CoreFeature,
        path: &ItemPath,
        generic_args: &[ResolvedGenericArg],
    ) -> Option<ResolvedType> {
        let options = ResolveItemOptions::DIRECT.bypassing_visibility(true);
        match self
            .resolver
            .resolve_item(&self.module_path, path, generic_args, options)
        {
            Ok(ResolvedItem::Type(r#type)) => Some(r#type),
            _ => {
                self.core_unavailable(
                    id,
                    span,
                    feature,
                    format!("'core' declares no type '{path}'"),
                );
                None
            }
        }
    }

    /// `core::range::Range<element>`, checked to hold exactly the fields a
    /// range literal fills.
    pub(super) fn core_range(
        &mut self,
        id: HirId,
        span: Span,
        element: ResolvedType,
    ) -> Option<NominalRef> {
        let feature = CoreFeature::Ranges;
        let path = core_contract::range_path();
        let generic_args = [ResolvedGenericArg::Type(element.clone())];
        let ResolvedType::Struct(nominal) =
            self.core_type(id, span, feature, &path, &generic_args)?
        else {
            self.core_unavailable(id, span, feature, format!("'{path}' must be a struct"));
            return None;
        };
        let fields = self.type_facts(id, span, |types| Ok(types.fields(&nominal)?.to_vec()))?;
        if let Err(detail) = core_contract::check_range(&fields, &element) {
            self.core_unavailable(id, span, feature, detail);
            return None;
        }
        Some(nominal)
    }

    /// A spec `core` declares at `path`, checked by `check`.
    pub(super) fn core_spec(
        &mut self,
        id: HirId,
        span: Span,
        feature: CoreFeature,
        path: &ItemPath,
        check: fn(&crate::type_store::SpecFacts) -> Result<(), String>,
    ) -> Option<NominalRef> {
        let Ok(Some(spec)) = self.resolver.spec_declaration(path) else {
            self.core_unavailable(
                id,
                span,
                feature,
                format!("'core' declares no spec '{path}'"),
            );
            return None;
        };
        if let Err(detail) = check(self.resolver.types().spec_facts(&spec)) {
            self.core_unavailable(id, span, feature, detail);
            return None;
        }
        Some(spec)
    }

    /// `r#type` as `core::option::Option`, in the shape `feature` reads it.
    pub(super) fn core_option(
        &mut self,
        id: HirId,
        span: Span,
        feature: CoreFeature,
        r#type: &ResolvedType,
    ) -> Option<NominalRef> {
        let nominal = match r#type {
            ResolvedType::Enum { nominal, .. }
                if core_contract::is_option(&nominal.module_path, &nominal.name) =>
            {
                nominal.clone()
            }
            other => {
                self.core_unavailable(
                    id,
                    span,
                    feature,
                    format!("expected 'core::option::Option', found '{other}'"),
                );
                return None;
            }
        };
        let facts = self.type_facts(id, span, |types| Ok(types.enum_facts(&nominal)?.clone()))?;
        if let Err(detail) = core_contract::check_option(&facts) {
            self.core_unavailable(id, span, feature, detail);
            return None;
        }
        Some(nominal)
    }

    /// The `core::reflection::TypeInfo` table type, with every declaration
    /// the reflection builder reads checked against the shape it fills.
    pub(super) fn core_type_info(&mut self, id: HirId, span: Span) -> Option<ResolvedType> {
        let feature = CoreFeature::Reflection;
        let mut resolved = Vec::new();
        for path in core_contract::reflection_paths() {
            resolved.push((
                path.name.clone(),
                self.core_type(id, span, feature, &path, &[])?,
            ));
        }
        let types = self.resolver.types();
        let checked = core_contract::check_reflection(|name| {
            let (_, r#type) = resolved
                .iter()
                .find(|(declared, _)| declared.as_ref() == name)?;
            match r#type {
                ResolvedType::Struct(nominal) => {
                    types.fields(nominal).ok().map(ReflectionFacts::Struct)
                }
                ResolvedType::Enum { nominal, .. } => types
                    .enum_facts(nominal)
                    .ok()
                    .map(|facts| ReflectionFacts::Enum(facts)),
                _ => None,
            }
        });
        if let Err(detail) = checked {
            self.core_unavailable(id, span, feature, detail);
            return None;
        }
        resolved
            .into_iter()
            .find(|(name, _)| name.as_ref() == "TypeInfo")
            .map(|(_, r#type)| r#type)
    }
}
