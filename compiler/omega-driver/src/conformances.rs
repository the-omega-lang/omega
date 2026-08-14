use crate::items::ItemKey;
use crate::{Driver, ModulePath};
use omega_analyzer::analysis::PendingSpecMethod;
use omega_analyzer::checked::ConformanceOwner;
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::resolved_type::{
    ResolvedBound, ResolvedMethod, ResolvedSpecType, ResolvedType,
};
use omega_diagnostics::Span;
use omega_hir::{HirConformDef, HirFunctionDef, HirId, HirItem, HirPrimitiveDef};
use omega_parser::prelude::{Ident, Type};
use std::cell::RefCell;
use std::rc::Rc;

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
    /// Derived entries model a dependency of a directly declared conform;
    /// they make conformance discoverable but never own a second body.
    pub derived: bool,
    /// Whether this entry came from matching a *generic* conform template
    /// against a concrete target, rather than from a directly-written
    /// concrete `conform`. Decided by whether `instantiate_conformance` was
    /// handed a substitution: only a template match produces one. Reaches
    /// codegen through `ConformanceOwner::from_template`, where it decides weak
    /// vs. strong linkage -- see that field's own doc comment.
    pub from_template: bool,
}

#[derive(Clone)]
struct ConformanceTemplate {
    module: ModulePath,
    conform: HirConformDef,
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
                if conform.generics.is_empty() {
                    self.instantiate_conformance(module, &conform, &[]);
                } else if let Some(parameter) = Self::blanket_parameter(&conform) {
                    // Registering this as a template would be worse than
                    // useless: `match_conform_target` can never bind it, so
                    // the conform would be silently dropped and the only
                    // diagnostic anyone ever saw would be a
                    // `SpecNotImplemented` at some unrelated use site.
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::BlanketConformanceNotYetSupported { parameter },
                        ),
                    );
                } else if !Self::template_target_is_matchable(&conform.target) {
                    // Same reasoning as the blanket case just above, for a
                    // different shape: a *generic* conform never reaches
                    // `resolve_conform_target` (only concrete instantiations
                    // do), so a target `match_conform_target` cannot bind --
                    // `conform<T> *T to Spec`, `conform<T> [4]T to Spec` --
                    // would register as a template that nothing can ever
                    // match and vanish without a word. Rejected here, with
                    // the same diagnostic the concrete path already gives.
                    self.diagnostics.error(
                        module,
                        AnalysisError::new(
                            conform.id,
                            conform.span,
                            AnalysisErrorKind::ConformTargetNotAType,
                        ),
                    );
                } else {
                    self.conformances.templates.push(ConformanceTemplate {
                        module: module.clone(),
                        conform,
                    });
                }
            }
        }
    }

    /// Whether a *generic* conform's raw target is a shape
    /// `match_conform_target` can actually bind: a named generic type
    /// (`List<T>`) or a slice pattern (`[?]T`). Anything else registers a
    /// template no concrete type will ever match, so it is rejected at
    /// declaration instead. Kept deliberately in step with
    /// `match_conform_target`'s own arms — the two must agree or a legal
    /// target gets refused, or an unmatchable one slips through again.
    fn template_target_is_matchable(target: &Type) -> bool {
        matches!(target, Type::Generic(..) | Type::InferredArray(_))
    }

    fn instantiate_conformance(
        &mut self,
        module: &[Ident],
        conform: &HirConformDef,
        substitution: &[(Ident, ResolvedType)],
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
        let bounds = match self.check_generic_bounds(
            module,
            (conform.id, conform.span),
            &conform.generics,
            &type_args,
        ) {
            Some(Ok(bounds)) => bounds,
            Some(Err(error)) => {
                self.diagnostics.error(
                    module,
                    AnalysisError::new(
                        conform.id,
                        conform.span,
                        AnalysisErrorKind::ModuleResolution(error),
                    ),
                );
                self.conformances.failed.push((conform.id, target.lookup_key()));
                return None;
            }
            None => {
                self.conformances.failed.push((conform.id, target.lookup_key()));
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
                    &conform.spec,
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
            derived: false,
            from_template: !substitution.is_empty(),
        };
        if !self.check_conformance_orphan(&entry) || self.reject_duplicate_conformance(&entry) {
            return None;
        }
        // A directly-written conform supersedes any derived entry standing in
        // for the same `(target, spec, args)`. Without this, declaring
        // `conform Foo to Derived` before `conform Foo to Base` left the derived
        // stand-in ahead of the explicit block in `entries`, so `Base::b(&f)`
        // silently called *Derived*'s body while the hand-written one was
        // emitted and never reached -- wrong code, no diagnostic. This is not
        // a `DuplicateConformance`: the author wrote exactly one `Base` block, and
        // it is the one that must win.
        let spec_id = entry.spec.borrow().id;
        self.conformances.entries.retain(|existing| {
            !(existing.derived
                && existing.target == entry.target
                && existing.spec.borrow().id == spec_id
                && existing.spec_args == entry.spec_args)
        });
        self.conformances.entries.push(entry.clone());
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
            if self.conformances.entries.iter().any(|existing| {
                existing.target == entry.target
                    && existing.spec.borrow().id == spec.borrow().id
                    && existing.spec_args == spec_args
            }) {
                continue;
            }
            self.conformances.entries.push(ConformanceEntry {
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
                derived: true,
                from_template: entry.from_template,
            });
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

    fn reject_duplicate_conformance(&mut self, entry: &ConformanceEntry) -> bool {
        let Some(previous) = self.conformances.entries.iter().find(|existing| {
            !existing.derived
                && existing.target == entry.target.lookup_key()
                && existing.spec.borrow().id == entry.spec.borrow().id
                && existing.spec_args == entry.spec_args
        }) else {
            return false;
        };
        self.diagnostics.error(
            &entry.module,
            AnalysisError::new(
                entry.id,
                entry.span,
                AnalysisErrorKind::DuplicateConformance {
                    target: entry.target.to_string(),
                    spec: entry.spec.borrow().name.clone(),
                    previous: previous.span,
                },
            ),
        );
        true
    }

    pub(crate) fn conformance_for(
        &mut self,
        target: &ResolvedType,
        spec: &Rc<RefCell<ResolvedSpecType>>,
        spec_args: &[ResolvedType],
    ) -> Option<ConformanceEntry> {
        // Directly-written blocks first, derived stand-ins only as a fallback:
        // registration order must never decide which body a call reaches (see
        // `instantiate_conformance`'s supersede step). Belt-and-braces alongside
        // that removal, since a derived entry can also be registered *after* a
        // direct one by a later conform's own dependency walk.
        let matches = |entry: &&ConformanceEntry| {
            entry.target == target.lookup_key()
                && entry.spec.borrow().id == spec.borrow().id
                && entry.spec_args == spec_args
        };
        if let Some(entry) = self
            .conformances
            .entries
            .iter()
            .find(|entry| !entry.derived && matches(entry))
            .or_else(|| self.conformances.entries.iter().find(matches))
        {
            return Some(entry.clone());
        }
        let templates = self.conformances.templates.clone();
        for template in templates {
            let Some(substitution) = Self::match_conform_target(&template.conform, target) else {
                continue;
            };
            if let Some(entry) =
                self.instantiate_conformance(&template.module, &template.conform, &substitution)
                && entry.spec.borrow().id == spec.borrow().id
                && entry.spec_args == spec_args
            {
                return Some(entry);
            }
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
        let templates = self.conformances.templates.clone();
        for template in templates {
            if let Some(substitution) = Self::match_conform_target(&template.conform, target) {
                self.instantiate_conformance(&template.module, &template.conform, &substitution);
            }
        }
        self.conformances
            .entries
            .iter()
            .filter(|entry| entry.target == target.lookup_key())
            .cloned()
            .collect()
    }

    /// The first of `conform`'s own generic parameters that its *target*
    /// does not pin down -- either because the target is that parameter
    /// (`conform<T: Numeric> T to Sum`, which would apply to every type) or
    /// because the parameter appears nowhere in the target at all
    /// (`conform<T, U: Foo> List<T> to Bar`, whose `U` nothing can ever
    /// bind). Both are blanket conformances, deliberately out of scope for now;
    /// a target that uses every parameter (`conform<T> List<T> to
    /// ToIterator<T>`) is not one, and is fully supported.
    fn blanket_parameter(conform: &HirConformDef) -> Option<Ident> {
        if let Type::Named(path) = &conform.target
            && path.is_unqualified()
            && let Some(generic) = conform
                .generics
                .iter()
                .find(|generic| generic.ident == path.head)
        {
            return Some(generic.ident.clone());
        }
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
            from_template: entry.from_template,
        }
    }
}
