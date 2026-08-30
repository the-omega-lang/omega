use crate::items::ItemKey;
use crate::{Driver, ModulePath};
use omega_analyzer::analysis::AnalysisSite;
use omega_analyzer::analysis::Analyzer;
use omega_analyzer::analysis::PendingSpecMethod;
use omega_analyzer::checked::ConformanceOwner;
use omega_analyzer::error::{AnalysisError, AnalysisErrorKind};
use omega_analyzer::generics::GenericSubstitution;
use omega_analyzer::resolved_type::{
    ResolvedBound, ResolvedGenericArg, ResolvedMethod, ResolvedSpecType, ResolvedType,
};
use omega_diagnostics::Span;
use omega_hir::{AliasTarget, HirConformDef, HirFunctionDef, HirGenericParam, HirId, HirItem};
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
    pub spec_args: Vec<ResolvedGenericArg>,
    pub methods: Vec<(Ident, ResolvedMethod)>,
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
}

mod registration;
mod solver;

impl Driver {
    pub(crate) fn conformance_method_ids(
        &mut self,
        module: &[Ident],
        declaration: HirId,
        target: &ResolvedType,
        functions: &[HirFunctionDef],
    ) -> Vec<HirId> {
        let key = ItemKey::new(
            module,
            &Ident(format!("__conform_{}", declaration.local)),
            &[ResolvedGenericArg::Type(target.lookup_key())],
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
