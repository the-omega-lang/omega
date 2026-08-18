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
use omega_hir::{HirConformDef, HirFunctionDef, HirGenericParam, HirId, HirItem, HirPrimitiveDef};
use omega_parser::prelude::{Ident, Type};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConformanceOrigin {
    Blanket,
    Generic,
    Concrete,
}

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
    pub declared_bounds: Vec<ResolvedBound>,
    pub declared_bound_keys: Vec<(HirId, Vec<ResolvedType>)>,
    pub origin: ConformanceOrigin,
}

impl ConformanceEntry {
    pub fn precedence(&self) -> ConformanceOrigin {
        self.origin
    }

    pub fn monomorphized(&self) -> bool {
        self.origin != ConformanceOrigin::Concrete
    }
}

impl ConformanceOrigin {
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

pub(crate) struct SweepOutcome {
    skipped_goal: bool,
}

#[derive(Clone)]
struct ConformanceGoal {
    id: HirId,
    target: ResolvedType,
    spec: HirId,
    spec_name: Ident,
    span: Span,
}

#[derive(Default)]
pub(crate) struct Conformances {
    pub entries: Vec<ConformanceEntry>,
    templates: Vec<ConformanceTemplate>,
    failed: Vec<(HirId, ResolvedType)>,
    materialized: Vec<ResolvedType>,
    goals: Vec<ConformanceGoal>,
    reported_cycles: Vec<(ResolvedType, HirId)>,
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
    pub(crate) fn mark_bound_type_imports(&mut self, module: &[Ident], generics: &[HirGenericParam]) {
        fn walk(this: &mut Driver, module: &[Ident], ty: &Type) {
            match ty {
                Type::Named(path) => {
                    if path.is_unqualified() {
                        let _ = this.import_entry(module, &path.head);
                    }
                }
                Type::Generic(path, args) => {
                    if path.is_unqualified() {
                        let _ = this.import_entry(module, &path.head);
                    }
                    for arg in args {
                        walk(this, module, arg);
                    }
                }
                Type::Pointer(inner, _)
                | Type::InferredArray(inner)
                | Type::UnknownSizeArray(inner)
                | Type::SizedArray(inner, _)
                | Type::SpecObject(inner, _) => walk(this, module, inner),
                Type::Function(f) => {
                    for param in &f.params {
                        walk(this, module, &param.r#type);
                    }
                    walk(this, module, &f.return_type);
                }
                Type::SpecStatic(_) => {}
            }
        }
        for param in generics {
            for bound in &param.bounds {
                walk(self, module, bound);
            }
        }
    }

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
        let method_ids = self.conformance_method_ids(module, primitive.id, &target, &primitive.functions);
        let signatures = self.analyze(
            module,
            &method_substitution,
            (primitive.id, primitive.span),
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
        };        let Type::InferredArray(raw_item) = &primitive.target else {
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
                self.mark_bound_type_imports(module, &conform.generics);
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
        // Every template is visible before a concrete conform can cause a
        // bound lookup, removing module-order dependence.
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
        if spec_reference.0.borrow().is_alias {
            self.diagnostics.error(
                module,
                AnalysisError::new(
                    conform.id,
                    conform.span,
                    AnalysisErrorKind::ConformToAliasSpec {
                        alias: spec_reference.0.borrow().name.clone(),
                    },
                ),
            );
            return None;
        }
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
        // The recursion guard lives in `solve`: it pushes this
        // instantiation's `(target, spec)` goal and skips any template
        // already in flight, so re-entry is impossible here -- only
        // `conformance_for` reports a cycle, once the goal stack shows the
        // proof closed on itself.
        let declared_bounds = match self.check_generic_bounds(
            module,
            (conform.id, conform.span),
            &conform.generics,
            &type_args,
        ) {
            Some(Ok(bounds)) => bounds,
            Some(Err(error)) => {
                // At the outermost goal, the failure is genuine and
                // permanent, worth memoizing in `failed`. A nested failure
                // is not: the in-flight proof above it may itself fail and
                // unwind, and the same template may be re-asked later from a
                // clean stack.
                if self.conformances.goals.len() == 1 {
                    self.conformances.failed.push((conform.id, target.lookup_key()));
                }
                // A blanket's bound is its applicability predicate: a
                // non-`Animal` type simply does not receive
                // `conform<T: Animal> T ...`. Generic constructor templates
                // still diagnose a matched `List<NotBound>` as an invalid
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
                if self.conformances.goals.len() == 1 {
                    self.conformances.failed.push((conform.id, target.lookup_key()));
                }
                return None;
            }
        };
        // Instantiating one template twice at the same target is not a
        // duplicate conform -- `conformances_for_type` re-walks every
        // matching template on each call, so without this the second lookup
        // would report `DuplicateConformance` against its own first entry.
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
        // The declared set's alias-expanded identity -- both blanket
        // precedence and derived-conformance admission compare on this, so
        // an alias bound and its inline spelling are interchangeable.
        let keys_run = self.with_analyzer(module, &substitution, (conform.id, conform.span), |a| {
            a.expand_bound_set(conform.id, conform.span, &declared_bounds)
        });
        self.diagnostics.record_warnings(module, keys_run.warnings);
        let declared_bound_keys = keys_run.result;
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
            declared_bounds: declared_bounds.clone(),
            declared_bound_keys: declared_bound_keys.clone(),
            origin,
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
            declared_bounds,
            declared_bound_keys,
            origin,
        };
        if !self.register_conformance(entry.clone()) {
            return None;
        }
        Some(entry)
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
        {
            // Both sides compare alias-expanded key sets, so `T: AB` and
            // `T: A + B` compare as equal.
            let candidate_subset_of_incumbent = candidate
                .declared_bound_keys
                .iter()
                .all(|bound| incumbent.declared_bound_keys.contains(bound));
            let incumbent_subset_of_candidate = incumbent
                .declared_bound_keys
                .iter()
                .all(|bound| candidate.declared_bound_keys.contains(bound));
            return match (candidate_subset_of_incumbent, incumbent_subset_of_candidate) {
                (true, false) => Some(Ordering::Less),
                (false, true) => Some(Ordering::Greater),
                (true, true) => Some(Ordering::Equal),
                (false, false) => None,
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
        let matches = |entry: &&ConformanceEntry| {
            entry.target == target.lookup_key()
                && entry.spec.borrow().id == spec.borrow().id
                && entry.spec_args == spec_args
        };
        if let Some(entry) = self.conformances.entries.iter().find(matches) {
            return Some(entry.clone());
        }
        // Goal-directed: instantiate only the templates that can produce
        // this spec, then look again. A goal still on the stack after that
        // is a genuine cycle, reported with the chain that closes it.
        self.solve(target, Some(&spec.borrow().id));
        if let Some(entry) = self.conformances.entries.iter().find(matches) {
            return Some(entry.clone());
        }
        if let Some(active) = self
            .conformances
            .goals
            .iter()
            .find(|goal| goal.target == target.lookup_key() && goal.spec == spec.borrow().id)
        {
            if self
                .conformances
                .reported_cycles
                .iter()
                .any(|(t, id)| *t == target.lookup_key() && *id == spec.borrow().id)
            {
                return None;
            }
            self.conformances
                .reported_cycles
                .push((target.lookup_key(), spec.borrow().id));
            let mut chain: Vec<(String, Ident, Span)> = self
                .conformances
                .goals
                .iter()
                .map(|goal| {
                    (
                        goal.target.to_string(),
                        goal.spec_name.clone(),
                        goal.span,
                    )
                })
                .collect();
            chain.push((target.to_string(), spec.borrow().name.clone(), active.span));
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
                        chain,
                    },
                ),
            );
        }
        None
    }

    pub(crate) fn bound_context_for(
        &mut self,
        concrete: &ResolvedType,
        spec: Rc<RefCell<ResolvedSpecType>>,
        spec_args: Vec<ResolvedType>,
        declared_keys: &[(HirId, Vec<ResolvedType>)],
    ) -> Vec<(ResolvedType, Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)> {
        let mut permitted = HashSet::new();
        Analyzer::alias_member_ids(&spec, &mut permitted);
        let mut seeds = vec![(concrete.clone(), spec, spec_args)];
        for entry in self.conformances_for_type(concrete) {
            if permitted.contains(&entry.spec.borrow().id) {
                seeds.push((entry.target.clone(), entry.spec.clone(), entry.spec_args.clone()));
                continue;
            }
            if entry.origin == ConformanceOrigin::Concrete {
                continue;
            }
            // Both sides compare alias-expanded key sets: the candidate's
            // stored `declared_bound_keys` against the item's own expanded
            // `declared_keys`.
            let entailed = entry
                .declared_bound_keys
                .iter()
                .all(|bound| declared_keys.contains(bound));
            if entailed {
                seeds.push((entry.target.clone(), entry.spec.clone(), entry.spec_args.clone()));
            }
        }
        seeds
    }

    pub(crate) fn bound_context_over(
        &mut self,
        declared: &[ResolvedBound],
        declared_keys: &[(HirId, Vec<ResolvedType>)],
    ) -> Vec<(ResolvedType, Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)> {
        let mut context = Vec::new();
        for (concrete, spec, spec_args) in declared {
            context.extend(self.bound_context_for(
                concrete,
                spec.clone(),
                spec_args.clone(),
                declared_keys,
            ));
        }
        context
    }

    pub(crate) fn conformances_for_type(&mut self, target: &ResolvedType) -> Vec<ConformanceEntry> {
        let key = target.lookup_key();
        if !self.conformances.materialized.contains(&key) {
            let outcome = self.solve(target, None);
            // A partial sweep is not a complete one: if any template was
            // skipped because its goal was already in flight, the memo must
            // not claim completeness -- the next query re-sweeps.
            if !outcome.skipped_goal {
                self.conformances.materialized.push(key);
            }
        }
        self.conformances
            .entries
            .iter()
            .filter(|entry| entry.target == target.lookup_key())
            .cloned()
            .collect()
    }

    pub(crate) fn solve(
        &mut self,
        target: &ResolvedType,
        spec: Option<&HirId>,
    ) -> SweepOutcome {
        let templates = self.conformances.templates.clone();
        let mut skipped_goal = false;
        for template in templates {
            let Some(substitution) = Self::match_conform_target(&template.conform, target) else {
                continue;
            };
            if self
                .conformances
                .failed
                .iter()
                .any(|(id, failed)| *id == template.conform.id && *failed == target.lookup_key())
            {
                continue;
            }
            let Some((produced, _)) = self.template_spec(&template, &substitution) else {
                // The produced spec does not resolve. The demand path skips
                // silently; the full sweep still instantiates and reports.
                if spec.is_some() {
                    continue;
                }
                self.instantiate_conformance(
                    &template.module,
                    &template.conform,
                    &substitution,
                    template.origin,
                );
                continue;
            };
            let spec_id = produced.borrow().id;
            let spec_name = produced.borrow().name.clone();
            if let Some(wanted) = spec
                && spec_id != *wanted
            {
                continue;
            }
            if self
                .conformances
                .goals
                .iter()
                .any(|goal| goal.target == target.lookup_key() && goal.spec == spec_id)
            {
                skipped_goal = true;
                continue;
            }
            self.conformances.goals.push(ConformanceGoal {
                id: template.conform.id,
                target: target.lookup_key(),
                spec: spec_id,
                spec_name,
                span: template.conform.span,
            });
            self.instantiate_conformance(
                &template.module,
                &template.conform,
                &substitution,
                template.origin,
            );
            self.conformances.goals.pop();
        }
        SweepOutcome { skipped_goal }
    }

    fn template_spec(
        &mut self,
        template: &ConformanceTemplate,
        substitution: &[(Ident, ResolvedType)],
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, Vec<ResolvedType>)> {
        self.with_analyzer(
            &template.module,
            substitution,
            (template.conform.id, template.conform.span),
            |analyzer| {
                analyzer.probe(|a| {
                    a.resolve_spec_reference(
                        template.conform.id,
                        template.conform.span,
                        &template.conform.spec,
                    )
                })
            },
        )
        .result
    }

    fn unconstrained_parameter(conform: &HirConformDef) -> Option<Ident> {
        let mut mentioned = Vec::new();
        Self::collect_type_idents(&conform.target, &mut mentioned);
        conform
            .generics
            .iter()
            .find(|generic| !mentioned.contains(&generic.ident))
            .map(|generic| generic.ident.clone())
    }

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
