use crate::items::ItemKey;
use crate::{Driver, ModulePath};
use omega_analyzer::analysis::AnalysisSite;
use omega_analyzer::analysis::Analyzer;
use omega_analyzer::analysis::PendingSpecMethod;
use omega_analyzer::checked::{CheckedFunctionDef, ConformanceOwner};
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind, AnalysisWarning};
use omega_analyzer::generics::GenericSubstitution;
use omega_analyzer::resolved_type::{
    ResolvedBound, ResolvedGenericArg, ResolvedMethod, ResolvedSpecType, ResolvedType,
};
use omega_analyzer::resolver::ResolveError;
use omega_diagnostics::Span;
use omega_hir::{AliasTarget, HirConformDef, HirFunctionDef, HirGenericParam, HirId, HirItem};
use omega_parser::prelude::{Ident, Type};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
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
    pub spec_args: Vec<ResolvedGenericArg>,
    pub methods: Vec<(Ident, ResolvedMethod)>,
    /// Declarations selected to satisfy generic spec requirements. They are
    /// instantiated lazily and deliberately have no concrete method slot.
    pub templates: Vec<HirId>,
    pub method_ids: Vec<HirId>,
    pub functions: Vec<HirFunctionDef>,
    pub pending: Vec<PendingSpecMethod>,
    pub substitution: GenericSubstitution,
    pub declared_bounds: Vec<ResolvedBound>,
    pub declared_bound_keys: Vec<(HirId, Vec<ResolvedGenericArg>)>,
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
    module: ModulePath,
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
    pub emitted: Vec<(ResolvedType, HirId, Vec<ResolvedGenericArg>)>,
    /// Checked method bodies by method id; `None` records a failed check.
    bodies: HashMap<HirId, Option<ConformanceMethodBody>>,
    bodies_in_progress: HashSet<HirId>,
}

#[derive(Clone)]
pub(crate) struct ConformanceMethodBody {
    pub function: CheckedFunctionDef,
    pub warnings: Vec<(ModulePath, AnalysisWarning)>,
}

mod registration;
mod solver;

impl Driver {
    pub(crate) fn conformance_method_key(&self, entry: &ConformanceEntry) -> ItemKey {
        self.implementation_owner_key(&entry.module, entry.id, &entry.target)
    }

    fn implementation_owner_key(
        &self,
        module: &[Ident],
        declaration: HirId,
        target: &ResolvedType,
    ) -> ItemKey {
        let index = self
            .modules
            .parsed(module)
            .hir
            .items
            .iter()
            .position(|item| match item {
                HirItem::Conform(conform) => conform.id == declaration,
                HirItem::Primitive(primitive) => primitive.id == declaration,
                _ => false,
            })
            .expect("an implementation owner belongs to its declaring module");
        ItemKey::new(
            module,
            &Ident(format!("__conform_{}", declaration.local)),
            index,
            &[ResolvedGenericArg::Type(target.lookup_key())],
        )
    }

    pub(crate) fn conformance_method_ids(
        &mut self,
        module: &[Ident],
        declaration: HirId,
        target: &ResolvedType,
        functions: &[HirFunctionDef],
    ) -> Vec<HirId> {
        let key = self.implementation_owner_key(module, declaration, target);
        self.items
            .method_identities(&key, functions.iter().map(|function| function.id))
    }

    /// The bound context every method body of `entry` is checked in, and the
    /// warnings expanding it raised.
    pub(crate) fn conformance_bounds(
        &mut self,
        entry: &ConformanceEntry,
    ) -> (Vec<ResolvedBound>, Vec<AnalysisWarning>) {
        let mut bounds = vec![ResolvedBound::new(
            entry.target.clone(),
            entry.spec.clone(),
            entry.spec_args.clone(),
        )];
        let keys_run = self.with_analyzer(
            &entry.module,
            &entry.substitution,
            AnalysisSite::new(entry.id, entry.span),
            |a| a.expand_bound_set(entry.id, entry.span, &entry.declared_bounds),
        );
        bounds.extend(self.bound_context_over(&entry.declared_bounds, &keys_run.result));
        (bounds, keys_run.warnings)
    }

    /// One method of `entry`, a concrete method or a spec default, checked
    /// once whether the body sweep or a `comp` evaluation asks first. `None`
    /// is a failed check, or a request made while the body is itself being
    /// checked.
    pub(crate) fn conformance_method_body(
        &mut self,
        entry: &ConformanceEntry,
        bounds: &[ResolvedBound],
        method_id: HirId,
    ) -> Option<ConformanceMethodBody> {
        if let Some(body) = self.conformances.bodies.get(&method_id) {
            return body.clone();
        }
        if !self.conformances.bodies_in_progress.insert(method_id) {
            return None;
        }
        let body = self.check_conformance_method(entry, bounds, method_id);
        self.conformances.bodies_in_progress.remove(&method_id);
        self.conformances.bodies.insert(method_id, body.clone());
        body
    }

    fn check_conformance_method(
        &mut self,
        entry: &ConformanceEntry,
        bounds: &[ResolvedBound],
        method_id: HirId,
    ) -> Option<ConformanceMethodBody> {
        let (module, run) =
            if let Some(index) = entry.method_ids.iter().position(|id| *id == method_id) {
                let function = &entry.functions[index];
                let (_, method) = entry
                    .methods
                    .iter()
                    .find(|(_, method)| method.decl_id == method_id)?;
                let run = self.with_analyzer_in(
                    &entry.module,
                    &entry.substitution,
                    bounds,
                    AnalysisSite::new(function.id, function.span),
                    |analyzer| {
                        analyzer.check_function_body(
                            function,
                            &method.fn_type,
                            method.decl_id,
                            &method.annotations,
                        )
                    },
                );
                (entry.module.clone(), run)
            } else {
                let pending = entry
                    .pending
                    .iter()
                    .find(|pending| pending.id == method_id)?;
                let spec_module = entry.spec.borrow().module_path.clone();
                let run = self.with_analyzer_in(
                    &spec_module,
                    &pending.substitution,
                    bounds,
                    AnalysisSite::new(pending.id, pending.raw.span),
                    |analyzer| analyzer.check_pending_spec_method(pending),
                );
                (spec_module, run)
            };
        let mut function = run.result?;
        function.conformance_owner = Some(Self::conformance_owner(entry));
        Some(ConformanceMethodBody {
            function,
            warnings: run
                .warnings
                .into_iter()
                .map(|warning| (module.clone(), warning))
                .collect(),
        })
    }

    /// The body of `method_id` for `comp` evaluation, or `None` when no
    /// conformance declares it.
    pub(crate) fn conformance_method_query(
        &mut self,
        method_id: HirId,
    ) -> Option<Result<CheckedFunctionDef, ResolveError>> {
        if let Some(Some(body)) = self.conformances.bodies.get(&method_id) {
            return Some(Ok(body.function.clone()));
        }
        let entry = self
            .conformances
            .entries
            .iter()
            .find(|entry| {
                entry.method_ids.contains(&method_id)
                    || entry.pending.iter().any(|pending| pending.id == method_id)
            })?
            .clone();
        // The sweep records the bound context's warnings with the bodies.
        let (bounds, _) = self.conformance_bounds(&entry);
        Some(
            match self.conformance_method_body(&entry, &bounds, method_id) {
                Some(body) => Ok(body.function),
                None => {
                    let key = self.conformance_method_key(&entry);
                    Err(ResolveError::ItemFailed {
                        module: key.module().clone(),
                        item: key.name,
                    })
                }
            },
        )
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
