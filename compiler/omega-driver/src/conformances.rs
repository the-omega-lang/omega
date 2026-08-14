use crate::items::ItemKey;
use crate::{Driver, ModulePath};
use omega_analyzer::analysis::PendingSpecMethod;
use omega_analyzer::analysis::Analyzer;
use omega_analyzer::checked::ConformanceOwner;
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolved_type::{
    ResolvedBound, ResolvedMethod, ResolvedSpecType, ResolvedType,
};
use omega_diagnostics::Span;
use omega_hir::{HirConformDef, HirFunctionDef, HirId, HirItem, HirPrimitiveDef};
use omega_parser::prelude::{Ident, Type};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

/// The source form that produced a concrete conformance entry. Ordering is
/// specificity: a more concrete declaration supersedes a less concrete one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConformanceOrigin {
    Blanket,
    Generic,
    Concrete,
}

/// Whether an entry declares methods itself or merely exposes methods from a
/// spec dependency of a declared conformance.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConformanceRole {
    Derived,
    Declared,
}

/// What registration should do after it has compared a candidate with the
/// entry already owning the same `(target, spec, arguments)` key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RegistrationDecision {
    Insert,
    Replace(usize),
    Ignore,
}

#[derive(Clone)]
pub(crate) struct ConformanceEntry {
    pub module: ModulePath,
    pub id: HirId,
    pub span: Span,
    pub target: ResolvedType,
    pub spec: Rc<RefCell<ResolvedSpecType>>,
    pub spec_args: Vec<ResolvedType>,
    pub methods: Vec<(Ident, ResolvedMethod)>,
    pub method_ids: Vec<HirId>,
    pub functions: Vec<HirFunctionDef>,
    pub pending: Vec<PendingSpecMethod>,
    pub substitution: Vec<(Ident, ResolvedType)>,
    /// Bounds declared by this conform's own generic parameters. These are
    /// checked before registration, then seed the conform bodies alongside
    /// the spec this block itself implements.
    pub bounds: Vec<ResolvedBound>,
    pub origin: ConformanceOrigin,
    pub role: ConformanceRole,
}

impl ConformanceEntry {
    pub fn precedence(&self) -> (ConformanceOrigin, ConformanceRole) {
        (self.origin, self.role)
    }

    pub fn monomorphized(&self) -> bool {
        self.origin != ConformanceOrigin::Concrete
    }
}

impl ConformanceOrigin {
    /// Classifies the raw target shapes that `match_conform_target` can bind.
    /// Keep this beside that matcher: accepting a template here that the
    /// matcher cannot bind would silently drop a conform declaration.
    fn classify(target: &Type, generics: &[omega_hir::HirGenericParam]) -> Option<Self> {
        if generics.is_empty() {
            return Some(Self::Concrete);
        }
        match target {
            Type::Named(path)
                if path.is_unqualified()
                    && generics.iter().any(|generic| generic.ident == path.head) =>
            {
                Some(Self::Blanket)
            }
            Type::Generic(..) | Type::InferredArray(..) => Some(Self::Generic),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct ConformanceTemplate {
    module: ModulePath,
    conform: HirConformDef,
    origin: ConformanceOrigin,
}

#[derive(Clone)]
struct InProgressConformance {
    id: HirId,
    target: ResolvedType,
    spec: Rc<RefCell<ResolvedSpecType>>,
    span: Span,
}

#[derive(Default)]
pub(crate) struct Conformances {
    pub entries: Vec<ConformanceEntry>,
    templates: Vec<ConformanceTemplate>,
    /// Template instantiations whose own generic bounds were not satisfied,
    /// keyed the same way the success guard is. `conformances_for_type` and
    /// `conformance_for` re-walk every matching template on each call, and a
    /// failed instantiation registers no entry to be found the second time --
    /// so without this the same `SpecNotImplemented` was reported once per
    /// conformance lookup (twice for a target that is also coerced to
    /// `spec *T`). Correct anchor and wording; only the count was wrong.
    failed: Vec<(HirId, ResolvedType)>,
    /// Targets whose template set has already been considered. All templates
    /// are parked before any materialization, so this is a sound memo rather
    /// than an order-dependent cache.
    materialized: Vec<ResolvedType>,
    /// Active template checks, used to turn recursive blanket bounds into a
    /// diagnostic instead of unbounded query recursion.
    in_progress: Vec<InProgressConformance>,
    pub emitted: Vec<(ResolvedType, HirId, Vec<ResolvedType>)>,
}

#[derive(Clone)]
pub(crate) struct PrimitiveEntry {
    pub module: ModulePath,
    pub span: Span,
    pub target: ResolvedType,
    pub methods: Vec<(Ident, ResolvedMethod)>,
    pub method_ids: Vec<HirId>,
    pub functions: Vec<HirFunctionDef>,
    pub substitution: Vec<(Ident, ResolvedType)>,
}

impl PrimitiveEntry {
    /// Primitive templates have exactly one type substitution before their
    /// synthetic `Self` binding is added. Keep this legacy representation
    /// detail named at its boundary rather than leaking length tests into the
    /// compiler pipeline.
    pub fn monomorphized(&self) -> bool {
        self.substitution.len() > 1
    }
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
                (primitive.id, primitive.span),
                |analyzer| {
                    analyzer.resolve_conform_target(primitive.id, primitive.span, &primitive.target)
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
        let method_ids = self.conformance_method_ids(module, primitive.id, &target, &primitive.functions);
        let signatures = self.analyze(
            module,
            &method_substitution,
            (primitive.id, primitive.span),
            |analyzer| {
                let mut resolved = Vec::with_capacity(primitive.functions.len());
                for function in &primitive.functions {
                    let (fn_type, annotations) =
                        analyzer.collect_function_signature(function, None)?;
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
        };
        self.primitives.entries.push(entry.clone());
        Some(entry)
    }

    fn primitive_target_allowed(target: &ResolvedType) -> bool {
        matches!(
            target,
            ResolvedType::Bool
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

    pub(crate) fn collect_conformance_signatures(&mut self, paths: &[ModulePath]) {
        let mut concrete = Vec::new();
        for module in paths {
            let declarations: Vec<_> = self
                .modules
                .parsed(module)
                .hir
                .items
                .iter()
                .filter_map(|item| match item {
                    HirItem::Conform(conform) => Some(conform.clone()),
                    _ => None,
                })
                .collect();
            for conform in declarations {
                let Some(origin) = ConformanceOrigin::classify(&conform.target, &conform.generics) else {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::ConformTargetNotAType,
                        ),
                    );
                    continue;
                };
                if let Some(parameter) = Self::unconstrained_parameter(&conform) {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::UnconstrainedConformanceParameter { parameter },
                        ),
                    );
                    continue;
                }
                if origin == ConformanceOrigin::Blanket {
                    let spec_run = self.with_analyzer(
                        module,
                        &[],
                        (conform.id, conform.span),
                        |analyzer| analyzer.resolve_spec_reference(conform.id, conform.span, &conform.spec),
                    );
                    self.diagnostics.record_warnings(module, spec_run.warnings);
                    let Some((spec, _)) = spec_run.result else {
                        continue;
                    };
                    let spec_package = spec
                        .borrow()
                        .module_path
                        .first()
                        .cloned()
                        .unwrap_or_else(|| Ident(String::new()));
                    if module.first() != Some(&spec_package) {
                        self.diagnostics.error(
                            module,
                            AnalysisError::new(
                                conform.id,
                                conform.span,
                                AnalysisErrorKind::BlanketConformanceForeignSpec { spec_package },
                            ),
                        );
                        continue;
                    }
                }
                match origin {
                    ConformanceOrigin::Concrete => concrete.push((module.clone(), conform)),
                    ConformanceOrigin::Generic | ConformanceOrigin::Blanket => {
                    self.conformances.templates.push(ConformanceTemplate {
                        module: module.clone(),
                        conform,
                        origin,
                    });
                    }
                }
            }
        }
        // Every template is now visible before a concrete conform can cause
        // a bound lookup, removing module-order dependence from lazy
        // materialization.
        for (module, conform) in concrete {
            self.instantiate_conformance(&module, &conform, &[], ConformanceOrigin::Concrete);
        }
    }

    fn instantiate_conformance(
        &mut self,
        module: &[Ident],
        conform: &HirConformDef,
        substitution: &[(Ident, ResolvedType)],
        origin: ConformanceOrigin,
    ) -> Option<ConformanceEntry> {
        let target_run = self.with_analyzer(
            module,
            substitution,
            (conform.id, conform.span),
            |analyzer| analyzer.resolve_conform_target(conform.id, conform.span, &conform.target),
        );
        self.diagnostics
            .record_warnings(module, target_run.warnings);
        let target = target_run.result?;
        let spec_run = self.with_analyzer(
            module,
            substitution,
            (conform.id, conform.span),
            |analyzer| analyzer.resolve_spec_reference(conform.id, conform.span, &conform.spec),
        );
        self.diagnostics.record_warnings(module, spec_run.warnings);
        let spec_reference = spec_run.result?;
        let type_args: Vec<_> = conform
            .generics
            .iter()
            .map(|param| {
                substitution
                    .iter()
                    .find(|(ident, _)| ident == &param.ident)
                    .map(|(_, r#type)| r#type.clone())
                    .expect("a generic conform template pins every parameter")
            })
            .collect();
        // Checked before the success guard below, because a failure
        // registers no entry for that guard to find.
        if self
            .conformances
            .failed
            .iter()
            .any(|(id, failed)| *id == conform.id && *failed == target.lookup_key())
        {
            return None;
        }
        let in_progress = InProgressConformance {
            id: conform.id,
            target: target.lookup_key(),
            spec: spec_reference.0.clone(),
            span: conform.span,
        };
        if self.conformances.in_progress.iter().any(|active| {
            active.id == in_progress.id && active.target == in_progress.target
        }) {
            self.diagnostics.error(
                module,
                AnalysisError::new(
                    conform.id,
                    conform.span,
                    AnalysisErrorKind::ConformanceCycle {
                        target: target.to_string(),
                        spec: spec_reference.0.borrow().name.clone(),
                        declarations: self
                            .conformances
                            .in_progress
                            .iter()
                            .filter(|active| active.target == target.lookup_key())
                            .map(|active| active.span)
                            .collect(),
                    },
                ),
            );
            self.conformances.failed.push((conform.id, target.lookup_key()));
            return None;
        }
        self.conformances.in_progress.push(in_progress);
        let bounds = match self.check_generic_bounds(
            module,
            (conform.id, conform.span),
            &conform.generics,
            &type_args,
        ) {
            Some(Ok(bounds)) => bounds,
            Some(Err(error)) => {
                self.conformances.failed.push((conform.id, target.lookup_key()));
                self.conformances.in_progress.pop();
                // A blanket's bound is its applicability predicate: a
                // non-`Animal` type simply does not receive
                // `conform<T: Animal> T ...`. Generic constructor
                // templates keep their existing diagnostic behavior, where
                // a matched `List<NotBound>` is an attempted, invalid
                // instantiation.
                if origin != ConformanceOrigin::Blanket {
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::ModuleResolution(error),
                        ),
                    );
                }
                return None;
            }
            None => {
                self.conformances.failed.push((conform.id, target.lookup_key()));
                self.conformances.in_progress.pop();
                return None;
            }
        };
        self.conformances.in_progress.pop();
        // Instantiating one template twice at the same target is not a
        // duplicate conform -- `conformances_for_type` re-walks every matching
        // template on each call, so without this the *second* lookup for a
        // generic target would report `DuplicateConformance` against the entry
        // the first lookup registered. Keyed on the declaration's own id, so
        // two genuinely distinct `conform` blocks still collide below.
        if let Some(existing) = self
            .conformances
            .entries
            .iter()
            .find(|existing| existing.id == conform.id && existing.target == target.lookup_key())
        {
            return Some(existing.clone());
        }
        let mut method_substitution = substitution.to_vec();
        method_substitution.push((Ident("Self".to_string()), target.clone()));
        // Resolve precedence before checking the potentially expensive body.
        // In particular, a blanket superseded by an explicit conform must not
        // surface diagnostics from a body that can never be emitted.
        let header = ConformanceEntry {
            module: module.to_vec(),
            id: conform.id,
            span: conform.span,
            target: target.lookup_key(),
            spec: spec_reference.0.clone(),
            spec_args: spec_reference.1.clone(),
            methods: vec![],
            method_ids: vec![],
            functions: vec![],
            pending: vec![],
            substitution: method_substitution.clone(),
            bounds: bounds.clone(),
            origin,
            role: ConformanceRole::Declared,
        };
        // Keep the established diagnostic order: an illegal foreign conform
        // is rejected for violating the orphan rule, even if an imported
        // declaration happens to own the same conformance key.
        if !self.check_conformance_orphan(&header) {
            return None;
        }
        if self.registration_decision(&header) == RegistrationDecision::Ignore {
            return None;
        }
        let method_ids = self.conformance_method_ids(module, conform.id, &target, &conform.functions);
        let run = self.with_analyzer(
            module,
            &method_substitution,
            (conform.id, conform.span),
            |analyzer| {
                analyzer.check_conform_block(
                    conform.id,
                    conform.span,
                    &target,
                    &spec_reference,
                    &conform.functions,
                    &method_ids,
                )
            },
        );
        self.diagnostics.record_warnings(module, run.warnings);
        let (spec, spec_args, methods, pending) = run.result?;
        let entry = ConformanceEntry {
            module: module.to_vec(),
            id: conform.id,
            span: conform.span,
            target: target.lookup_key(),
            spec,
            spec_args,
            methods,
            method_ids,
            functions: conform.functions.clone(),
            pending,
            substitution: method_substitution,
            bounds,
            origin,
            role: ConformanceRole::Declared,
        };
        if !self.register_conformance(entry.clone()) {
            return None;
        }
        self.register_derived_conformances(&entry);
        Some(entry)
    }

    fn register_derived_conformances(&mut self, entry: &ConformanceEntry) {
        let run = self.with_analyzer(
            &entry.module,
            &entry.substitution,
            (entry.id, entry.span),
            |analyzer| {
                analyzer.transitive_spec_dependencies(
                    entry.id,
                    entry.span,
                    &entry.target,
                    &entry.spec,
                    &entry.spec_args,
                    &entry.methods,
                )
            },
        );
        self.diagnostics.record_warnings(&entry.module, run.warnings);
        let Some(transitive) = run.result else {
            return;
        };
        for (spec, spec_args, methods) in transitive {
            let derived = ConformanceEntry {
                module: entry.module.clone(),
                id: entry.id,
                span: entry.span,
                target: entry.target.clone(),
                spec,
                spec_args,
                methods,
                method_ids: vec![],
                functions: vec![],
                pending: vec![],
                substitution: entry.substitution.clone(),
                bounds: entry.bounds.clone(),
                origin: entry.origin,
                role: ConformanceRole::Derived,
            };
            self.register_conformance(derived);
        }
    }

    fn check_conformance_orphan(&mut self, entry: &ConformanceEntry) -> bool {
        let local = entry
            .module
            .first()
            .cloned()
            .unwrap_or_else(|| Ident(String::new()));
        let target_package = entry
            .target
            .declaring_owner()
            .and_then(|(path, _)| path.first().cloned())
            .unwrap_or_else(|| Ident("core".to_string()));
        let spec_package = entry
            .spec
            .borrow()
            .module_path
            .first()
            .cloned()
            .unwrap_or_else(|| Ident(String::new()));
        if local == target_package || local == spec_package {
            return true;
        }
        self.diagnostics.error(
            &entry.module,
            AnalysisError::new(
                entry.id,
                entry.span,
                AnalysisErrorKind::ConformanceOrphanViolation {
                    target_package,
                    spec_package,
                },
            ),
        );
        false
    }

    /// Registers exactly one winner for a `(target, spec, args)` key. A
    /// loser is deliberately not retained: every later resolver can query
    /// entries directly without re-implementing precedence.
    fn register_conformance(&mut self, entry: ConformanceEntry) -> bool {
        match self.registration_decision(&entry) {
            RegistrationDecision::Insert => {
                self.conformances.entries.push(entry);
                true
            }
            RegistrationDecision::Replace(index) => {
                self.conformances.entries.remove(index);
                self.conformances.entries.push(entry);
                true
            }
            RegistrationDecision::Ignore => false,
        }
    }

    /// Compare a declaration header with the current owner of its key. This
    /// is deliberately side-effect free except for duplicate/ambiguity
    /// diagnostics, so `instantiate_conformance` can make the selection
    /// before it type-checks a body and `register_conformance` can apply the
    /// same decision afterwards.
    fn registration_decision(&mut self, entry: &ConformanceEntry) -> RegistrationDecision {
        let incumbent = self.conformances.entries.iter().position(|existing| {
            existing.target == entry.target
                && existing.spec.borrow().id == entry.spec.borrow().id
                && existing.spec_args == entry.spec_args
        });
        let Some(index) = incumbent else {
            return RegistrationDecision::Insert;
        };
        let existing = self.conformances.entries[index].clone();
        match Self::compare_conformance_precedence(entry, &existing) {
            Some(Ordering::Greater) => RegistrationDecision::Replace(index),
            Some(Ordering::Less) => RegistrationDecision::Ignore,
            // A derived entry stands in for a spec dependency rather than
            // declaring anything, so an unordered clash with one is never the
            // author's mistake: two conforms sharing a dependency each derive
            // the same key. It still *competes* above, though -- a stand-in
            // from `conform Foo to Derived` is specific to `Foo` and must
            // displace a blanket that only matched `Foo` incidentally.
            _ if entry.role == ConformanceRole::Derived => RegistrationDecision::Ignore,
            Some(Ordering::Equal) => {
                self.diagnostics.error(
                    &entry.module,
                    AnalysisError::new(
                        entry.id,
                        entry.span,
                        AnalysisErrorKind::DuplicateConformance {
                            target: entry.target.to_string(),
                            spec: entry.spec.borrow().name.clone(),
                            previous: existing.span,
                        },
                    ),
                );
                RegistrationDecision::Ignore
            }
            None => {
                self.diagnostics.error(
                    &entry.module,
                    AnalysisError::new(
                        entry.id,
                        entry.span,
                        AnalysisErrorKind::AmbiguousConformance {
                            target: entry.target.to_string(),
                            spec: entry.spec.borrow().name.clone(),
                            first: existing.span,
                        },
                    ),
                );
                RegistrationDecision::Ignore
            }
        }
    }

    fn compare_conformance_precedence(
        candidate: &ConformanceEntry,
        incumbent: &ConformanceEntry,
    ) -> Option<Ordering> {
        if candidate.origin == ConformanceOrigin::Blanket
            && incumbent.origin == ConformanceOrigin::Blanket
            && candidate.role == ConformanceRole::Declared
            && incumbent.role == ConformanceRole::Declared
        {
            let candidate_bound = candidate.bounds.first().map(|(_, spec, _)| spec.clone());
            let incumbent_bound = incumbent.bounds.first().map(|(_, spec, _)| spec.clone());
            return match (candidate_bound, incumbent_bound) {
                (Some(candidate_bound), Some(incumbent_bound)) => {
                    let mut candidate_dependencies = Vec::new();
                    Self::transitive_dependency_ids(&candidate_bound, &mut candidate_dependencies);
                    let mut incumbent_dependencies = Vec::new();
                    Self::transitive_dependency_ids(&incumbent_bound, &mut incumbent_dependencies);
                    let candidate_contains_incumbent =
                        candidate_dependencies.contains(&incumbent_bound.borrow().id);
                    let incumbent_contains_candidate =
                        incumbent_dependencies.contains(&candidate_bound.borrow().id);
                    match (candidate_contains_incumbent, incumbent_contains_candidate) {
                        (true, false) => Some(Ordering::Greater),
                        (false, true) => Some(Ordering::Less),
                        (true, true) => Some(Ordering::Equal),
                        (false, false) => None,
                    }
                }
                // An *unbounded* blanket (`conform<T> T to Spec`) accepts every
                // type, so it is strictly less specific than one that filters by
                // a bound -- the empty bound set is a subset of every other.
                // Without this the two collided as `DuplicateConformance`.
                (Some(_), None) => Some(Ordering::Greater),
                (None, Some(_)) => Some(Ordering::Less),
                (None, None) => Some(Ordering::Equal),
            };
        }
        Some(candidate.precedence().cmp(&incumbent.precedence()))
    }

    pub(crate) fn conformance_for(
        &mut self,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Option<ConformanceEntry> {
        self.materialize(target);
        let matches = |entry: &&ConformanceEntry| {
            entry.target == target.lookup_key()
                && entry.spec.borrow().id == spec.borrow().id
                && entry.spec_args == spec_args
        };
        if let Some(entry) = self.conformances.entries.iter().find(matches) {
            return Some(entry.clone());
        }
        if let Some(active) = self.conformances.in_progress.iter().find(|active| {
            active.target == target.lookup_key() && active.spec.borrow().id == spec.borrow().id
        }) {
            self.diagnostics.error(
                &self
                    .conformances
                    .templates
                    .iter()
                    .find(|template| template.conform.id == active.id)
                    .map(|template| template.module.clone())
                    .unwrap_or_default(),
                AnalysisError::new(
                    active.id,
                    active.span,
                    AnalysisErrorKind::ConformanceCycle {
                        target: target.to_string(),
                        spec: spec.borrow().name.clone(),
                        declarations: self
                            .conformances
                            .in_progress
                            .iter()
                            .filter(|candidate| candidate.target == target.lookup_key())
                            .map(|candidate| candidate.span)
                            .collect(),
                    },
                ),
            );
        }
        None
    }

    /// Every spec reachable from `spec` through `dependencies`, by id,
    /// including `spec` itself. Ids only -- a membership test needs no
    /// per-dependency type arguments (which are raw at a declaration; see
    /// `ResolvedSpecType::dependencies`). Mirrors
    /// `Analyzer::transitive_dependency_ids`, which does the same walk for
    /// conformance; both must agree or a bound would admit a method that
    /// conformance rejects.
    fn transitive_dependency_ids(spec: &Rc<RefCell<ResolvedSpecType>>, out: &mut Vec<HirId>) {
        let (id, dependencies) = {
            let spec = spec.borrow();
            (spec.id, spec.dependencies.clone())
        };
        if out.contains(&id) {
            return;
        }
        out.push(id);
        for (dependency, _) in dependencies {
            Self::transitive_dependency_ids(&dependency, out);
        }
    }

    /// What a `T: Spec` bound puts into the analyzer's bound context: the
    /// declared bound itself, plus every conform already registered on
    /// `concrete` whose spec lies in `spec`'s transitive dependencies.
    ///
    /// The transitive part is what makes an **aggregate** bound work at all. A
    /// pure alias (`spec MySpec = Dummy | Mammal`) is never itself conformed to,
    /// so `(concrete, MySpec)` has no entry and a receiver call under
    /// `T: MySpec` would find nothing -- even though `conform Wolf to Mammal`
    /// already registered every method the alias names.
    ///
    /// Restricted to those, never "every conform on the type": seeding
    /// the latter is what previously let `x.secret()` resolve under
    /// `T: Speak` merely because someone wrote `conform Dog to Secret`, which
    /// voids the guarantee `conform` exists to provide.
    pub(crate) fn bound_context_for(
        &mut self,
        concrete: &ResolvedType,
        spec: Rc<RefCell<ResolvedSpecType>>,
        spec_args: Vec<ResolvedType>,
    ) -> Vec<(ResolvedType, Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)> {
        let mut permitted = Vec::new();
        Self::transitive_dependency_ids(&spec, &mut permitted);
        let mut seeds = vec![(concrete.clone(), spec, spec_args)];
        for entry in self.conformances_for_type(concrete) {
            if permitted.contains(&entry.spec.borrow().id) {
                seeds.push((entry.target, entry.spec, entry.spec_args));
            }
        }
        seeds
    }

    pub(crate) fn conformances_for_type(&mut self, target: &ResolvedType) -> Vec<ConformanceEntry> {
        self.materialize(target);
        self.conformances
            .entries
            .iter()
            .filter(|entry| entry.target == target.lookup_key())
            .cloned()
            .collect()
    }

    /// Instantiates each matching template at most once for one concrete
    /// target. Templates are all parked before this can run, so a cached
    /// target cannot miss a declaration from a later module.
    fn materialize(&mut self, target: &ResolvedType) {
        let key = target.lookup_key();
        if self.conformances.materialized.contains(&key) {
            return;
        }
        let templates = self.conformances.templates.clone();
        for template in templates {
            if self.conformances.in_progress.iter().any(|active| {
                active.id == template.conform.id && active.target == target.lookup_key()
            }) {
                continue;
            }
            if let Some(substitution) = Self::match_conform_target(&template.conform, target) {
                self.instantiate_conformance(
                    &template.module,
                    &template.conform,
                    &substitution,
                    template.origin,
                );
            }
        }
        if !self.conformances.materialized.contains(&key) {
            self.conformances.materialized.push(key);
        }
    }

    /// The first of `conform`'s own generic parameters that appears nowhere in
    /// its target, so nothing can ever bind it -- `conform<T, U: Foo> List<T>
    /// to Bar`, whose `U` no concrete `List<...>` determines. Materializing
    /// such a template is impossible, so it is rejected at its declaration.
    ///
    /// A *blanket* (`conform<T: Numeric> T to Sum`) is deliberately not this:
    /// its target **is** the parameter, so matching binds `T` to whatever type
    /// is being checked. `ConformanceOrigin::classify` sorts the two apart.
    fn unconstrained_parameter(conform: &HirConformDef) -> Option<Ident> {
        let mut mentioned = Vec::new();
        Self::collect_type_idents(&conform.target, &mut mentioned);
        conform
            .generics
            .iter()
            .find(|generic| !mentioned.contains(&generic.ident))
            .map(|generic| generic.ident.clone())
    }

    /// Every unqualified identifier a raw `Type` mentions, in source order.
    /// Only used to ask whether a generic parameter occurs in a conform
    /// target, so a qualified path (which can never *be* a parameter) is
    /// deliberately not contributed.
    fn collect_type_idents(r#type: &Type, out: &mut Vec<Ident>) {
        match r#type {
            Type::Named(path) => {
                if path.is_unqualified() {
                    out.push(path.head.clone());
                }
            }
            Type::Pointer(inner, _)
            | Type::InferredArray(inner)
            | Type::UnknownSizeArray(inner)
            | Type::SizedArray(inner, _) => Self::collect_type_idents(inner, out),
            Type::Generic(_, args) => {
                for arg in args {
                    Self::collect_type_idents(arg, out);
                }
            }
            _ => {}
        }
    }

    fn match_conform_target(
        conform: &HirConformDef,
        actual: &ResolvedType,
    ) -> Option<Vec<(Ident, ResolvedType)>> {
        if let Type::Named(path) = &conform.target
            && path.is_unqualified()
            && conform.generics.iter().any(|generic| generic.ident == path.head)
        {
            return Analyzer::is_conformable_target(actual)
                .then(|| vec![(path.head.clone(), actual.clone())]);
        }
        if let (Type::InferredArray(raw_item), ResolvedType::Slice { item, .. }) =
            (&conform.target, actual)
        {
            let Type::Named(path) = raw_item.as_ref() else {
                return None;
            };
            if !path.is_unqualified()
                || !conform
                    .generics
                    .iter()
                    .any(|generic| generic.ident == path.head)
            {
                return None;
            }
            return Some(vec![(path.head.clone(), (**item).clone())]);
        }
        let (actual_name, actual_args) = match actual {
            ResolvedType::Struct(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            ResolvedType::Enum { cell, .. } => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            ResolvedType::Union(cell) => {
                let cell = cell.borrow();
                (cell.name.clone(), cell.type_args.clone())
            }
            _ => return None,
        };
        let (path, args) = match &conform.target {
            Type::Generic(path, args) => (path, args),
            _ => return None,
        };
        if path.segments().last() != Some(&actual_name) || args.len() != actual_args.len() {
            return None;
        }
        let mut substitution = Vec::new();
        for (raw, concrete) in args.iter().zip(actual_args) {
            let Type::Named(path) = raw else { return None };
            if !path.is_unqualified()
                || !conform
                    .generics
                    .iter()
                    .any(|generic| generic.ident == path.head)
            {
                return None;
            }
            substitution.push((path.head.clone(), concrete));
        }
        Some(substitution)
    }

    /// Conform and primitive blocks have no named `ItemKey` of their own,
    /// but every concrete target instantiation still needs a distinct method
    /// identity. Reuse the normal item identity allocator with an internal
    /// key made from the declaration and canonical target.
    fn conformance_method_ids(
        &mut self,
        module: &[Ident],
        declaration: HirId,
        target: &ResolvedType,
        functions: &[HirFunctionDef],
    ) -> Vec<HirId> {
        let key = ItemKey::new(
            module,
            &Ident(format!("__conform_{}", declaration.local)),
            &[target.lookup_key()],
        );
        self.items
            .method_identities(&key, functions.iter().map(|function| function.id))
    }

    pub(crate) fn conformance_owner(entry: &ConformanceEntry) -> ConformanceOwner {
        let spec = entry.spec.borrow();
        ConformanceOwner {
            target: entry.target.clone(),
            spec_module_path: spec.module_path.clone(),
            spec_name: spec.name.clone(),
            spec_args: entry.spec_args.clone(),
            monomorphized: entry.monomorphized(),
        }
    }
}
