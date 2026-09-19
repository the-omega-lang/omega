use crate::{Driver, ModulePath};
use indexmap::IndexMap;
use omega_analyzer::analysis::{AnalysisSite, Analyzer, item_site, item_visibility};
use omega_analyzer::annotations::ResolvedAnnotations;
use omega_analyzer::checked::{CheckedItem, Storage};
use omega_analyzer::error::AnalysisWarning;
use omega_analyzer::resolved_type::{
    ResolvedBound, ResolvedEnumType, ResolvedFunctionType, ResolvedGenericArg, ResolvedMethod,
    ResolvedSpecType, ResolvedStructType, ResolvedType, ResolvedUnionType,
};
use omega_analyzer::resolver::{ResolveError, ResolveItemOptions, ResolvedItem};
use omega_diagnostics::Span;
use omega_hir::{HirFunctionDef, HirGenericParam, HirId, HirItem, SYNTHETIC_MODULE};
use omega_parser::prelude::{Ident, Type, Visibility};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ItemKey {
    pub scope: ItemScope,
    pub disambiguator: usize,
    pub name: Ident,
    pub generic_args: Vec<ResolvedGenericArg>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ItemScope {
    Module(ModulePath),
    Owner(Box<ItemKey>),
}

impl ItemKey {
    pub fn new(
        module: &[Ident],
        name: &Ident,
        disambiguator: usize,
        generic_args: &[ResolvedGenericArg],
    ) -> Self {
        Self {
            scope: ItemScope::Module(module.to_vec()),
            disambiguator,
            name: name.clone(),
            generic_args: generic_args.to_vec(),
        }
    }

    pub fn member(owner: ItemKey, name: Ident, disambiguator: usize) -> Self {
        Self {
            scope: ItemScope::Owner(Box::new(owner)),
            name,
            disambiguator,
            generic_args: Vec::new(),
        }
    }

    pub fn module(&self) -> &ModulePath {
        match &self.scope {
            ItemScope::Module(module) => module,
            ItemScope::Owner(owner) => owner.module(),
        }
    }

    pub fn owner(&self) -> Option<&ItemKey> {
        match &self.scope {
            ItemScope::Module(_) => None,
            ItemScope::Owner(owner) => Some(owner),
        }
    }

    fn path(&self) -> ModulePath {
        let mut path = match &self.scope {
            ItemScope::Module(module) => module.clone(),
            ItemScope::Owner(owner) => owner.path(),
        };
        path.push(self.name.clone());
        path
    }

    pub fn is_instantiation(&self) -> bool {
        !self.generic_args.is_empty()
    }

    pub(crate) fn failed(&self) -> ResolveError {
        ResolveError::ItemFailed {
            module: self.module().clone(),
            item: self.name.clone(),
        }
    }
}

type SpecKey = (ModulePath, Ident);

enum ItemQueryState {
    InProgress,
    Resolved(ResolvedEntry),
    Failed(QueryFailure),
}

/// Why a query failed. A failed state always retains one of these, so no path
/// can produce an "already failed" result whose primary reason was never
/// surfaced anywhere.
enum QueryFailure {
    /// The query's own reason, handed back to the caller that started it.
    Cause(ResolveError),
    /// An analyzer run recorded the reason into the diagnostics sink; there is
    /// no `ResolveError` carrying it.
    Reported,
}

struct ResolvedEntry {
    visibility: Visibility,
    item: ResolvedItem,
}

enum SpecQueryState {
    InProgress,
    Resolved(Rc<RefCell<ResolvedSpecType>>),
    Failed(QueryFailure),
}

pub(crate) struct CheckedBody {
    pub item: CheckedItem,
    pub warnings: Vec<AnalysisWarning>,
}

pub(crate) struct GlueSignature {
    pub module: ModulePath,
    pub span: Span,
    pub gap: Rc<omega_analyzer::resolved_type::ResolvedGap>,
    pub functions: Vec<(HirFunctionDef, ResolvedFunctionType)>,
}

impl CheckedBody {
    pub fn clone_of(&self) -> Self {
        Self {
            item: self.item.clone(),
            warnings: self.warnings.clone(),
        }
    }
}

#[derive(Default)]
pub(crate) struct TypeCells {
    structs: IndexMap<ItemKey, Rc<RefCell<ResolvedStructType>>>,
    enums: IndexMap<ItemKey, Rc<RefCell<ResolvedEnumType>>>,
    unions: IndexMap<ItemKey, Rc<RefCell<ResolvedUnionType>>>,
}

impl TypeCells {
    pub fn struct_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedStructType>> {
        self.structs
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedStructType {
                    id,
                    name: key.name.clone(),
                    module_path: key.module().clone(),
                    generic_args: key.generic_args.clone(),
                    fields: vec![],
                    functions: vec![],
                    layout: Default::default(),
                    suppress: vec![],
                    is_marker: false,
                }))
            })
            .clone()
    }

    pub fn enum_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedEnumType>> {
        self.enums
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedEnumType {
                    id,
                    name: key.name.clone(),
                    module_path: key.module().clone(),
                    generic_args: key.generic_args.clone(),
                    tag_type: ResolvedType::U16,
                    header: vec![],
                    dynamic_fields: vec![],
                    variants: vec![],
                    functions: vec![],
                    layout: Default::default(),
                    suppress: vec![],
                }))
            })
            .clone()
    }

    pub fn union_cell(&mut self, key: &ItemKey, id: HirId) -> Rc<RefCell<ResolvedUnionType>> {
        self.unions
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(ResolvedUnionType {
                    id,
                    name: key.name.clone(),
                    module_path: key.module().clone(),
                    generic_args: key.generic_args.clone(),
                    fields: vec![],
                    functions: vec![],
                    suppress: vec![],
                }))
            })
            .clone()
    }

    pub fn expect_struct(&self, key: &ItemKey) -> Rc<RefCell<ResolvedStructType>> {
        self.structs
            .get(key)
            .expect("cell was created when the signature resolved")
            .clone()
    }

    pub fn expect_enum(&self, key: &ItemKey) -> Rc<RefCell<ResolvedEnumType>> {
        self.enums
            .get(key)
            .expect("cell was created when the signature resolved")
            .clone()
    }

    pub fn expect_union(&self, key: &ItemKey) -> Rc<RefCell<ResolvedUnionType>> {
        self.unions
            .get(key)
            .expect("cell was created when the signature resolved")
            .clone()
    }

    pub fn resolved_type(&self, key: &ItemKey) -> Option<ResolvedType> {
        if let Some(cell) = self.structs.get(key) {
            return Some(ResolvedType::Struct(cell.clone()));
        }
        if let Some(cell) = self.enums.get(key) {
            return Some(ResolvedType::Enum {
                cell: cell.clone(),
                variant: None,
            });
        }
        self.unions
            .get(key)
            .map(|cell| ResolvedType::Union(cell.clone()))
    }

    pub fn structs(&self) -> impl Iterator<Item = (&ItemKey, &Rc<RefCell<ResolvedStructType>>)> {
        self.structs.iter()
    }

    pub fn enums(&self) -> impl Iterator<Item = (&ItemKey, &Rc<RefCell<ResolvedEnumType>>)> {
        self.enums.iter()
    }

    pub fn unions(&self) -> impl Iterator<Item = (&ItemKey, &Rc<RefCell<ResolvedUnionType>>)> {
        self.unions.iter()
    }

    pub fn all_methods(&self) -> impl Iterator<Item = (&ItemKey, Vec<(Ident, ResolvedMethod)>)> {
        self.structs
            .iter()
            .map(|(key, cell)| (key, cell.borrow().functions.clone()))
            .chain(
                self.enums
                    .iter()
                    .map(|(key, cell)| (key, cell.borrow().functions.clone())),
            )
            .chain(
                self.unions
                    .iter()
                    .map(|(key, cell)| (key, cell.borrow().functions.clone())),
            )
    }
}

#[derive(Default)]
pub(crate) struct ItemQueries {
    item_states: IndexMap<ItemKey, ItemQueryState>,
    resolution_stack: Vec<ItemKey>,
    pub cells: TypeCells,
    pub gaps: HashMap<ItemKey, Rc<omega_analyzer::resolved_type::ResolvedGap>>,
    pub glues: Vec<GlueSignature>,
    spec_states: HashMap<SpecKey, SpecQueryState>,
    pub function_annotations: HashMap<HirId, ResolvedAnnotations>,
    pub generic_instantiations: IndexMap<ItemKey, CheckedBody>,
    pub declared_bounds: HashMap<ItemKey, Vec<ResolvedBound>>,
    next_synthetic_id: u32,
    checked_bodies: HashMap<ItemKey, CheckedBody>,
    pub decl_id_owner: HashMap<HirId, ItemKey>,
    pub comp_values: HashMap<HirId, omega_analyzer::resolved_type::ConstValue>,
    /// Every ordinary global's checked declaration, kept from signature
    /// analysis so body materialization reuses the resolved policy and
    /// initializer instead of rebuilding a declaration without them.
    checked_globals: HashMap<HirId, omega_analyzer::checked::CheckedDeclaration>,
    body_in_progress: std::collections::HashSet<ItemKey>,
}

impl ItemQueries {
    pub fn cache_checked_global(
        &mut self,
        declaration: omega_analyzer::checked::CheckedDeclaration,
    ) {
        self.checked_globals.insert(declaration.id, declaration);
    }

    pub fn expect_checked_global(&self, id: HirId) -> &omega_analyzer::checked::CheckedDeclaration {
        match self.checked_globals.get(&id) {
            Some(declaration) => declaration,
            None => panic!("every global is checked before its body is materialized"),
        }
    }

    pub fn fresh_synthetic_id(&mut self) -> HirId {
        let id = HirId {
            module: SYNTHETIC_MODULE,
            local: self.next_synthetic_id,
        };
        self.next_synthetic_id += 1;
        id
    }

    pub fn identity_for(&mut self, key: &ItemKey, declared: HirId) -> HirId {
        let id = if key.is_instantiation() {
            self.fresh_synthetic_id()
        } else {
            declared
        };
        self.decl_id_owner.insert(id, key.clone());
        id
    }

    pub fn method_identities(
        &mut self,
        key: &ItemKey,
        declared: impl IntoIterator<Item = HirId>,
    ) -> Vec<HirId> {
        declared
            .into_iter()
            .map(|id| self.identity_for(key, id))
            .collect()
    }

    fn state(&self, key: &ItemKey) -> Option<&ItemQueryState> {
        self.item_states.get(key)
    }

    fn begin(&mut self, key: &ItemKey) {
        self.item_states
            .insert(key.clone(), ItemQueryState::InProgress);
        self.resolution_stack.push(key.clone());
    }

    fn finish(
        &mut self,
        key: &ItemKey,
        visibility: Visibility,
        result: Result<&ResolvedItem, &ResolveError>,
    ) {
        let active = self
            .resolution_stack
            .pop()
            .expect("finishing item resolution requires an active query");
        assert_eq!(&active, key, "query stack must unwind in LIFO order");

        let state = match result {
            Ok(item) => ItemQueryState::Resolved(ResolvedEntry {
                visibility,
                item: item.clone(),
            }),
            Err(cause) => ItemQueryState::Failed(Self::failure(key, cause)),
        };
        self.item_states.insert(key.clone(), state);
    }

    /// A query whose own reason is the "already failed" marker for itself has
    /// no reason of its own: the analyzer reported it, and repeating the
    /// marker as a cause would make it rootless.
    fn failure(key: &ItemKey, cause: &ResolveError) -> QueryFailure {
        match cause {
            ResolveError::ItemFailed { module, item }
                if *module == *key.module() && *item == key.name =>
            {
                QueryFailure::Reported
            }
            other => QueryFailure::Cause(other.clone()),
        }
    }

    fn cycle_path(&self, key: &ItemKey) -> Vec<ModulePath> {
        let start = self
            .resolution_stack
            .iter()
            .position(|active| active == key)
            .expect("an in-progress query must be present in the resolution stack");
        self.resolution_stack[start..]
            .iter()
            .chain(std::iter::once(key))
            .map(ItemKey::path)
            .collect()
    }

    fn begin_spec(&mut self, key: &SpecKey) {
        self.spec_states
            .insert(key.clone(), SpecQueryState::InProgress);
    }

    fn finish_spec(
        &mut self,
        key: &SpecKey,
        result: Result<Rc<RefCell<ResolvedSpecType>>, QueryFailure>,
    ) {
        let state = match result {
            Ok(cell) => SpecQueryState::Resolved(cell),
            Err(failure) => SpecQueryState::Failed(failure),
        };
        self.spec_states.insert(key.clone(), state);
    }

    pub fn cached_body(&self, key: &ItemKey) -> Option<&CheckedBody> {
        if key.is_instantiation() {
            self.generic_instantiations.get(key)
        } else {
            self.checked_bodies.get(key)
        }
    }

    pub fn begin_body(&mut self, key: &ItemKey) -> bool {
        self.body_in_progress.insert(key.clone())
    }

    pub fn finish_body(&mut self, key: &ItemKey) {
        let removed = self.body_in_progress.remove(key);
        assert!(removed, "body query must be active when it finishes");
    }

    pub fn cache_checked_body(&mut self, key: &ItemKey, body: CheckedBody) {
        if key.is_instantiation() {
            self.generic_instantiations.insert(key.clone(), body);
        } else {
            self.checked_bodies.insert(key.clone(), body);
        }
    }

    /// Whether every recorded failure kept a reason. Component tests assert
    /// it so a new query path cannot silently create a rootless failure.
    pub fn failures_retain_a_cause(&self) -> bool {
        let rooted = |failure: &QueryFailure| match failure {
            QueryFailure::Cause(cause) => !matches!(cause, ResolveError::ItemFailed { .. }),
            QueryFailure::Reported => true,
        };
        self.item_states.values().all(|state| match state {
            ItemQueryState::Failed(failure) => rooted(failure),
            _ => true,
        }) && self.spec_states.values().all(|state| match state {
            SpecQueryState::Failed(failure) => rooted(failure),
            _ => true,
        })
    }

    pub fn is_resolved(&self, key: &ItemKey) -> bool {
        matches!(self.item_states.get(key), Some(ItemQueryState::Resolved(_)))
    }

    pub fn expect_resolved(&self, key: &ItemKey) -> &ResolvedItem {
        match self.item_states.get(key) {
            Some(ItemQueryState::Resolved(entry)) => &entry.item,
            _ => panic!("every signature is resolved before its body is checked"),
        }
    }

    pub fn resolved_items(&self) -> impl Iterator<Item = (&ItemKey, &ResolvedItem)> {
        self.item_states
            .iter()
            .filter_map(|(key, state)| match state {
                ItemQueryState::Resolved(entry) => Some((key, &entry.item)),
                ItemQueryState::InProgress | ItemQueryState::Failed(_) => None,
            })
    }
}

mod methods;
mod resolution;

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> Ident {
        Ident(name.to_string())
    }

    #[test]
    fn a_failed_query_never_keeps_its_own_already_failed_marker_as_the_cause() {
        let key = ItemKey::new(&[ident("a")], &ident("Broken"), 0, &[]);
        let mut queries = ItemQueries::default();

        queries.begin(&key);
        queries.finish(
            &key,
            Visibility::Exposed,
            Err(&ResolveError::ItemFailed {
                module: key.module().clone(),
                item: key.name.clone(),
            }),
        );

        assert!(!queries.is_resolved(&key));
        assert!(
            queries.failures_retain_a_cause(),
            "an analyzer-reported failure must record `Reported`, not a self-referential cause"
        );
    }

    #[test]
    fn a_failed_query_retains_the_reason_it_failed() {
        let key = ItemKey::new(&[ident("a")], &ident("Broken"), 0, &[]);
        let mut queries = ItemQueries::default();

        queries.begin(&key);
        queries.finish(
            &key,
            Visibility::Exposed,
            Err(&ResolveError::UnknownModule(vec![ident("missing")])),
        );

        assert!(queries.failures_retain_a_cause());
        assert!(matches!(
            queries.state(&key),
            Some(ItemQueryState::Failed(QueryFailure::Cause(
                ResolveError::UnknownModule(_)
            )))
        ));
    }

    #[test]
    fn item_cycle_path_preserves_resolution_order() {
        let first = ItemKey::new(&[ident("a")], &ident("First"), 0, &[]);
        let second = ItemKey::new(&[ident("b")], &ident("Second"), 0, &[]);
        let mut queries = ItemQueries::default();
        queries.begin(&first);
        queries.begin(&second);

        assert_eq!(
            queries.cycle_path(&first),
            vec![
                vec![ident("a"), ident("First")],
                vec![ident("b"), ident("Second")],
                vec![ident("a"), ident("First")],
            ]
        );
    }
    #[test]
    fn owner_scoped_queries_share_cycle_and_failure_states() {
        let mut driver = Driver::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/t10c_generic_member_functions"),
            None,
            Vec::new(),
            omega_analyzer::Target::DEFAULT,
        )
        .unwrap();
        let owner = ItemKey::new(&[ident("a")], &ident("Holder"), 0, &[]);
        let first = ItemKey::member(owner.clone(), ident("first"), 0);
        let second = ItemKey::member(owner, ident("second"), 1);
        let error = driver
            .ensure_item_query(
                &first,
                Visibility::Exposed,
                ResolveItemOptions::INDIRECT,
                |driver| {
                    driver.ensure_item_query(
                        &second,
                        Visibility::Exposed,
                        ResolveItemOptions::INDIRECT,
                        |driver| {
                            driver.ensure_item_query(
                                &first,
                                Visibility::Exposed,
                                ResolveItemOptions::INDIRECT,
                                |_| panic!("an in-progress query must not be recomputed"),
                            )
                        },
                    )
                },
            )
            .unwrap_err();
        let ResolveError::Cycle(chain) = error else {
            panic!("expected a cycle")
        };
        assert_eq!(chain, vec![first.path(), second.path(), first.path()]);
        assert!(driver.items.resolution_stack.is_empty());
        assert!(driver.items.failures_retain_a_cause());
        assert!(matches!(
            driver.ensure_item_query(
                &first,
                Visibility::Exposed,
                ResolveItemOptions::INDIRECT,
                |_| { panic!("a failed query must not be recomputed") }
            ),
            Err(ResolveError::ItemFailed { .. })
        ));
    }
}
