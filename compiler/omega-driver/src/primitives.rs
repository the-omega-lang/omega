use crate::{Driver, ModulePath};
use omega_analyzer::analysis::AnalysisSite;
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolved_type::{ResolvedMethod, ResolvedType};
use omega_hir::{HirFunctionDef, HirId, HirItem, HirPrimitiveDef};
use omega_parser::prelude::{Ident, Type};

#[derive(Clone)]
pub(crate) struct PrimitiveEntry {
    pub module: ModulePath,
    pub span: omega_diagnostics::Span,
    pub target: ResolvedType,
    pub methods: Vec<(Ident, ResolvedMethod)>,
    pub method_ids: Vec<HirId>,
    pub functions: Vec<HirFunctionDef>,
    pub substitution: Vec<(Ident, ResolvedType)>,
    pub monomorphized: bool,
}

#[derive(Clone)]
struct PrimitiveTemplate {
    module: ModulePath,
    primitive: HirPrimitiveDef,
}

#[derive(Default)]
pub(crate) struct Primitives {
    pub entries: Vec<PrimitiveEntry>,
    templates: Vec<PrimitiveTemplate>,
    pub emitted: Vec<ResolvedType>,
}

impl Driver {
    pub(crate) fn collect_primitive_signatures(&mut self, paths: &[ModulePath]) {
        for module in paths {
            let declarations: Vec<_> = self
                .modules
                .parsed(module)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::Primitive(primitive) => Some(primitive.clone()),
                    _ => None,
                })
                .collect();

            for primitive in declarations {
                if module.first().map(Ident::as_ref) != Some("core") {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            primitive.id,
                            primitive.span,
                            AnalysisErrorKind::PrimitiveOutsideCore,
                        ),
                    );
                    continue;
                }

                if primitive.generics.is_empty() {
                    self.instantiate_primitive(module, &primitive, &[], None);
                } else {
                    self.primitives.templates.push(PrimitiveTemplate {
                        module: module.clone(),
                        primitive,
                    });
                }
            }
        }
    }

    fn instantiate_primitive(
        &mut self,
        module: &[Ident],
        primitive: &HirPrimitiveDef,
        substitution: &[(Ident, ResolvedType)],
        actual_target: Option<&ResolvedType>,
    ) -> Option<PrimitiveEntry> {
        let target = if let Some(actual) = actual_target {
            actual.clone()
        } else {
            let run = self.with_analyzer(
                module,
                substitution,
                AnalysisSite::new(primitive.id, primitive.span),
                |analyzer| {
                    analyzer.resolve_primitive_target(
                        primitive.id,
                        primitive.span,
                        &primitive.target,
                    )
                },
            );
            self.diagnostics.record_warnings(module, run.warnings);
            run.result?
        };

        if !Self::primitive_target_allowed(&target) {
            self.diagnostics.error(
                module,
                AnalysisError::new(
                    primitive.id,
                    primitive.span,
                    AnalysisErrorKind::PrimitiveTargetNotAllowed {
                        target: target.to_string(),
                    },
                ),
            );
            return None;
        }

        if let Some(previous) = self
            .primitives
            .entries
            .iter()
            .find(|entry| entry.target == target.lookup_key())
        {
            self.diagnostics.error(
                module,
                AnalysisError::new(
                    primitive.id,
                    primitive.span,
                    AnalysisErrorKind::DuplicatePrimitiveTarget {
                        target: target.to_string(),
                        previous: previous.span,
                    },
                ),
            );
            return None;
        }

        let mut method_substitution = substitution.to_vec();
        let self_type = match (&primitive.target, &target) {
            (Type::InferredArray(_), ResolvedType::Slice { item, mutable }) => {
                ResolvedType::Array(item.clone(), *mutable)
            }
            _ => target.clone(),
        };
        method_substitution.push((Ident("Self".to_string()), self_type));

        let method_ids = self.conformance_method_ids(
            module,
            primitive.id,
            &target,
            &primitive.functions,
        );
        let signatures = self.analyze(
            module,
            &method_substitution,
            AnalysisSite::new(primitive.id, primitive.span),
            |analyzer| {
                let mut resolved = Vec::with_capacity(primitive.functions.len());
                for function in &primitive.functions {
                    let (fn_type, annotations) =
                        analyzer.collect_function_signature(function)?;
                    resolved.push((fn_type, annotations));
                }
                analyzer.check_overload_duplicates(&primitive.functions, &resolved);
                Some(
                    primitive
                        .functions
                        .iter()
                        .zip(resolved)
                        .zip(&method_ids)
                        .map(|((function, (fn_type, annotations)), method_id)| {
                            (
                                function.name.clone(),
                                ResolvedMethod {
                                    decl_id: *method_id,
                                    fn_type,
                                    visibility: function.visibility,
                                    annotations,
                                    source: None,
                                },
                            )
                        })
                        .collect(),
                )
            },
        )?;

        let entry = PrimitiveEntry {
            module: module.to_vec(),
            span: primitive.span,
            target: target.lookup_key(),
            methods: signatures,
            method_ids,
            functions: primitive.functions.clone(),
            substitution: method_substitution,
            monomorphized: actual_target.is_some(),
        };
        self.primitives.entries.push(entry.clone());
        Some(entry)
    }

    fn primitive_target_allowed(target: &ResolvedType) -> bool {
        matches!(
            target,
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
                | ResolvedType::F64
                | ResolvedType::Str { .. }
                | ResolvedType::Slice { .. }
        )
    }

    pub(crate) fn primitive_methods(
        &mut self,
        target: &ResolvedType,
    ) -> Vec<(Ident, ResolvedMethod)> {
        if let Some(entry) = self
            .primitives
            .entries
            .iter()
            .find(|entry| entry.target == target.lookup_key())
        {
            return entry.methods.clone();
        }

        let templates = self.primitives.templates.clone();
        for template in templates {
            let Some(substitution) = Self::match_primitive_target(&template.primitive, target)
            else {
                continue;
            };
            if let Some(entry) = self.instantiate_primitive(
                &template.module,
                &template.primitive,
                &substitution,
                Some(target),
            ) {
                return entry.methods;
            }
        }
        Vec::new()
    }

    fn match_primitive_target(
        primitive: &HirPrimitiveDef,
        actual: &ResolvedType,
    ) -> Option<Vec<(Ident, ResolvedType)>> {
        let ResolvedType::Slice { item, .. } = actual else {
            return None;
        };
        let Type::InferredArray(raw_item) = &primitive.target else {
            return None;
        };
        let Type::Named(path) = raw_item.as_ref() else {
            return None;
        };
        if !path.is_unqualified()
            || !primitive
                .generics
                .iter()
                .any(|generic| generic.ident == path.head)
        {
            return None;
        }
        Some(vec![(path.head.clone(), (**item).clone())])
    }
}
