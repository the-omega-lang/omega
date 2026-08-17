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

/// The source form that produced a concrete conformance entry. Ordering is
/// specificity: a more concrete declaration supersedes a less concrete one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConformanceOrigin {
    Blanket,
    Generic,
    Concrete,
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
    /// The conform's own declared bounds, exactly as written -- one
    /// `(concrete, spec, spec_args)` per member, before any bound-context
    /// seeding. The fully seeded analyzer context is deliberately *not*
    /// stored: it is body-checking information, computed at body-check time
    /// (see `check_generic_bounds`'s doc comment for why computing it during
    /// signature resolution made conformance queries re-entrant).
    pub declared_bounds: Vec<ResolvedBound>,
    /// `declared_bounds`' alias-expanded identity -- every `(spec id,
    /// resolved args)` in the declared set, transitively expanded through
    /// every alias, computed once where an analyzer is already in hand (see
    /// `Analyzer::expand_bound_set`). This is what blanket precedence
    /// compares and what `bound_context_for` uses to admit derived
    /// conformances: an alias bound and its inline spelling must be
    /// interchangeable everywhere.
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

/// What one sweep through a target's matching templates accomplished.
pub(crate) struct SweepOutcome {
    /// At least one template was skipped because its goal was already on
    /// the goal stack -- see `conformances_for_type` for what that means
    /// for the `materialized` memo.
    skipped_goal: bool,
}

/// One conformance proof currently in flight: "this template, instantiated
/// at `target`, is being asked to produce `spec`". The stack of these is the
/// recursion guard -- a goal re-entered while its own instantiation is still
/// running is a genuine cycle, and only that (see `Driver::solve`).
/// `spec_name` is `spec`'s display name, carried so the cycle chain can be
/// rendered without re-resolving anything mid-error.
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
    /// Active conformance goals, used to turn recursive blanket bounds into
    /// a diagnostic instead of unbounded query recursion.
    goals: Vec<ConformanceGoal>,
    /// `(target, spec)` pairs already reported as cyclic. The same closure
    /// can be re-proved through a different door -- a bound check's alias
    /// fallback re-asking a member spec while its own proof is still in
    /// flight rediscovers the identical cycle -- and one diagnostic is
    /// enough: the pair *is* the cycle.
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
    /// Marks every import alias a raw bound `Type` references -- a bound's
    /// spec names only ever resolve at *instantiation* time, which the
    /// declaring package's own standalone build may never do (a blanket
    /// template nobody in that package materializes). Without this, that
    /// package's own build reports `UnusedImport` on the very import that
    /// binds the bound's name -- a false positive by construction, now that
    /// a bound is the primary spelling for a spec reference (`T: Successor +
    /// Ord`). Purely a lint bookkeeping fix: nothing resolves, nothing
    /// errors; an alias that isn't actually an import (a local type, a
    /// module) is simply not marked.
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

    /// Every built-in type gets a `primitive` block as its *declaration
    /// site* in `core`, whether or not it has any methods to attach. That is
    /// what the one-block-per-target rule is for: reading `core` should
    /// answer "which types does this language have" without consulting the
    /// compiler's own source.
    ///
    /// `Void` and `Never` are included even though neither can have a
    /// callable `*self` method -- neither has a value to call one on. Their
    /// blocks exist to be declarations, and to give their semantics somewhere
    /// to be documented in Omega rather than only in `docs/`. Type
    /// *constructors* (`*T`, `[N]T`, `[?]T`) are still excluded: they are not
    /// single types, and `[]T` is already covered by the generic slice form.
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
                // Bound-position spec references only ever resolve when the
                // template is instantiated -- which a package's own
                // standalone build may never do -- so mark their import
                // aliases as used right here, at the declaration, or the
                // package's own build reports `UnusedImport` on an import
                // that genuinely binds the bound's name.
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
        // A spec alias is a name for a conjunction, never a contract of its
        // own: `conform T to Alias` is rejected outright rather than
        // flattening its members into one block (which was never its
        // semantics -- an alias is satisfied by conforming each member
        // separately). Nothing downstream ever needs to wonder about an
        // alias-named entry again.
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
        // The recursion guard itself lives in `solve`: it pushes this
        // instantiation's `(target, spec)` goal before calling in and skips
        // any template whose goal is already in flight, so re-entry is
        // impossible here -- only `conformance_for` can report a cycle,
        // from outside, once the goal stack tells it the proof closed on
        // itself.
        let declared_bounds = match self.check_generic_bounds(
            module,
            (conform.id, conform.span),
            &conform.generics,
            &type_args,
        ) {
            Some(Ok(bounds)) => bounds,
            Some(Err(error)) => {
                // At the outermost goal (`solve`'s stack holds only this
                // one) nothing else in flight could have caused the
                // failure, so it is genuine and permanent -- worth memoizing
                // in `failed`. A nested failure is *not*: the in-flight
                // proof above it may itself fail and unwind, and the same
                // template may be re-asked later from a clean stack. (This
                // is what keeps the already-fixed duplicate-
                // `SpecNotImplemented` behaviour intact while making a
                // nested failure retryable.)
                if self.conformances.goals.len() == 1 {
                    self.conformances.failed.push((conform.id, target.lookup_key()));
                }
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
                if self.conformances.goals.len() == 1 {
                    self.conformances.failed.push((conform.id, target.lookup_key()));
                }
                return None;
            }
        };
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
        // The declared set's alias-expanded identity, computed once here
        // where an analyzer is already in hand -- both blanket precedence
        // and derived-conformance admission compare on this, so an alias
        // bound and its inline spelling are interchangeable everywhere.
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

    /// Compares two declarations competing for the same `(target, spec,
    /// args)` key. Outside the blanket-vs-blanket arm this is the plain
    /// origin ordering (a concrete conform beats a generic template
    /// instantiation, which beats a blanket) -- the author's explicit
    /// declaration is always more specific than a derivation.
    ///
    /// Two *blankets* are ordered by their declared bound sets instead: a
    /// strict superset of bounds is more specific (its applicability
    /// predicate is narrower), a strict subset less so, and incomparable
    /// sets (`{A, B}` vs `{A, C}`) are genuinely ambiguous -- reported by the
    /// caller as `AmbiguousConformance`, never silently picked. Equal sets
    /// fall through to the ordinary `DuplicateConformance` handling. An
    /// *unbounded* blanket (`conform<T> T to Spec`) accepts every type, so
    /// it is strictly less specific than one that filters by any bound -- the
    /// empty bound set is a subset of every other, the degenerate case of
    /// the same rule. Without this the two collided as
    /// `DuplicateConformance`.
    fn compare_conformance_precedence(
        candidate: &ConformanceEntry,
        incumbent: &ConformanceEntry,
    ) -> Option<Ordering> {
        if candidate.origin == ConformanceOrigin::Blanket
            && incumbent.origin == ConformanceOrigin::Blanket
        {
            // Both sides compare their *alias-expanded* key sets
            // (`declared_bound_keys`), so `T: AB` and `T: A + B` describe
            // the same set and compare as equal.
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
        // Goal-directed proving: instantiate only the templates that can
        // produce *this* spec, then look again. A goal still on the stack
        // after that means this query re-entered its own proof -- a genuine
        // cycle, reported with the chain that closes it. (An unsatisfied
        // but acyclic query leaves nothing on the stack and reports nothing
        // here; its own diagnostic came from wherever the failure was.)
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

    /// What a `T: Spec` bound puts into the analyzer's bound context: the
    /// declared bound itself, plus -- for an alias bound -- every conform
    /// already registered on `concrete` whose spec is one of the alias's
    /// members, plus every *template-derived* conform on `concrete` whose
    /// own declared bound set is a subset of the item's declared bounds.
    ///
    /// The alias part is what makes an aggregate bound work at all: a pure
    /// alias (`spec AB = A + B`) is never itself conformed to, so
    /// `(concrete, AB)` has no entry and a receiver call under `T: AB`
    /// would find nothing -- even though `conform T to A` and `conform T to
    /// B` already registered every method the alias names.
    ///
    /// The derived part is what makes a blanket's methods reachable under a
    /// bound: `conform<T: Ord> T to Eq` means every `T: Ord` type is also
    /// `Eq`, so a body bounded on `Ord` alone may call `equals` -- the
    /// conform is *entailed* by the bound set, exactly like the alias
    /// members are. Restricted to template-derived entries (a blanket or
    /// generic-template instantiation, never a hand-written concrete
    /// `conform`), and to those whose declared bounds the current item's
    /// own bounds already guarantee: seeding anything wider is what
    /// previously let `x.secret()` resolve under `T: Speak` merely because
    /// someone wrote `conform Dog to Secret`, which voids the guarantee
    /// `conform` exists to provide.
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

    /// `bound_context_for` over a whole declared set -- the body-checking
    /// sites' shared fold (a generic function's own body, an aggregate
    /// instantiation's inherent methods, a conform body).
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
            // skipped because its goal was already in flight, every
            // template has *not* been considered, so the memo must not
            // claim it has -- the next query re-sweeps and picks up
            // whatever the interrupted proof left behind.
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

    /// The single place a template is ever instantiated for a target.
    /// `Some(spec)` restricts the sweep to templates that can produce that
    /// spec -- the demand path, which makes proving goal-directed: each
    /// goal pulls in precisely the templates it needs, instead of
    /// instantiating every template on the type (which is what let one
    /// blanket's bound check fire mid-sweep and start a *second* template
    /// while the first was still in flight -- the false-cycle bug).
    /// `None` sweeps every matching template -- the "all conformances of
    /// this type" path, kept as-is for the queries that genuinely want it.
    ///
    /// A template whose `(target, spec)` goal is already on the stack is
    /// skipped **silently** -- only `conformance_for` reports, and only
    /// when the goal it asked for is still in flight once `solve` returns.
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
            // A template whose own bound check already failed at this
            // target is permanently non-applicable here; skip it without
            // re-running any analysis.
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
                // silently (it cannot match a spec it cannot resolve); the
                // full sweep still instantiates and reports -- see
                // `template_spec`'s doc comment.
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

    /// Resolves a matched template's spec reference with its generics
    /// already bound. The substitution is required because a generic
    /// template's spec arguments may reference its own generics
    /// (`conform<K, V> HashMap<K, V> to ToIterator<KeyValue<K, V>>`): at
    /// park time there is nothing to bind them to, so the spec cannot be
    /// resolved then -- once the target has *matched*, the substitution
    /// pins every generic and the spec resolves exactly.
    ///
    /// Failures are silent here on purpose: `solve`'s demand path uses this
    /// as a pure filter, and the real path (`instantiate_conformance`)
    /// re-resolves the same reference and reports whatever went wrong. A
    /// template whose spec name does not resolve is consequently skipped
    /// without a diagnostic by the demand path alone; any full sweep for
    /// the type still instantiates it and reports (see
    /// `a_template_whose_spec_does_not_resolve_reports` in
    /// `tests/conform.rs`).
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
