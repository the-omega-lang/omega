//! Nominal type identity and the semantic facts filled in after it.
//!
//! A struct, union, enum or spec is named by a [`NominalRef`] from the moment
//! its declaration is first reached, so recursive references can point at it
//! before its definition is analyzed. The facts arrive later, in write-once
//! stages owned by the [`TypeStore`]; reading a stage that is not written yet
//! is the explicit [`Incomplete`] state rather than a default value.

use crate::annotations::Layout;
use crate::generics::GenericParam;
use crate::resolved_type::{
    RawSpecFunctionSig, ResolvedEnumVariant, ResolvedField, ResolvedGenericArg, ResolvedMethod,
    ResolvedType,
};
use crate::resolver::ResolveError;
use omega_hir::{HirId, ModulePath};
use omega_parser::prelude::{Ident, Visibility};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NominalId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NominalKind {
    Struct,
    Union,
    Enum,
    Spec,
}

/// Everything about a nominal type that is fixed when it is interned.
#[derive(Debug)]
pub struct NominalIdentity {
    /// The declaration, or the synthetic identity of one generic
    /// instantiation of it.
    pub decl_id: HirId,
    pub kind: NominalKind,
    pub name: Ident,
    pub module_path: ModulePath,
    pub generic_args: Vec<ResolvedGenericArg>,
    /// An enum's variant names in declaration order, taken from the
    /// declaration so a refined variant can be spelled without the store.
    /// Empty for every other kind.
    pub variant_names: Vec<Ident>,
}

/// A handle to one interned nominal type. Equality and hashing are by
/// [`NominalId`]; the identity travels with the handle so spelling and
/// mangling need no store.
#[derive(Debug, Clone)]
pub struct NominalRef {
    id: NominalId,
    identity: Rc<NominalIdentity>,
}

impl NominalRef {
    pub fn id(&self) -> NominalId {
        self.id
    }
}

impl std::ops::Deref for NominalRef {
    type Target = NominalIdentity;

    fn deref(&self) -> &NominalIdentity {
        &self.identity
    }
}

impl PartialEq for NominalRef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for NominalRef {}

impl Hash for NominalRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug)]
pub struct StructFacts {
    pub fields: Vec<ResolvedField>,
    pub layout: Layout,
    pub suppress: Vec<Ident>,
    pub is_marker: bool,
}

#[derive(Debug)]
pub struct UnionFacts {
    pub fields: Vec<ResolvedField>,
    pub suppress: Vec<Ident>,
}

#[derive(Debug)]
pub struct EnumFacts {
    pub tag_type: ResolvedType,
    pub header: Vec<ResolvedField>,
    pub dynamic_fields: Vec<ResolvedField>,
    pub variants: Vec<ResolvedEnumVariant>,
    pub layout: Layout,
    pub suppress: Vec<Ident>,
}

impl EnumFacts {
    pub fn variant(&self, name: &Ident) -> Option<(usize, &ResolvedEnumVariant)> {
        self.variants
            .iter()
            .enumerate()
            .find(|(_, v)| &v.name == name)
    }
}

#[derive(Debug)]
pub struct SpecFacts {
    pub visibility: Visibility,
    pub generics: Vec<GenericParam>,
    pub is_object_safe: bool,
    pub functions: Vec<(Ident, RawSpecFunctionSig)>,
    pub suppress: Vec<Ident>,
}

pub type Methods = Rc<Vec<(Ident, ResolvedMethod)>>;

/// The completion stages of a nominal type, in the order they are written.
/// A spec has only a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Definition,
    Functions,
}

/// A read of a stage that has not been written: the type's own definition is
/// still being analyzed, or failed.
#[derive(Debug, Clone)]
pub struct Incomplete {
    pub nominal: NominalRef,
    pub stage: Stage,
}

impl From<Incomplete> for ResolveError {
    fn from(incomplete: Incomplete) -> Self {
        let nominal = &incomplete.nominal;
        ResolveError::Cycle(vec![nominal.module_path.join(nominal.name.clone())])
    }
}

#[derive(Debug, Clone)]
enum Definition {
    Struct(Rc<StructFacts>),
    Union(Rc<UnionFacts>),
    Enum(Rc<EnumFacts>),
    Spec(Rc<SpecFacts>),
}

#[derive(Debug, Clone)]
struct Entry {
    nominal: NominalRef,
    definition: Option<Definition>,
    functions: Option<Methods>,
}

/// The owner of every nominal type's staged facts. Interning never
/// deduplicates: the caller keys instantiations and interns each once.
#[derive(Debug, Clone, Default)]
pub struct TypeStore {
    entries: Vec<Entry>,
}

impl TypeStore {
    pub fn intern(&mut self, identity: NominalIdentity) -> NominalRef {
        let id =
            NominalId(u32::try_from(self.entries.len()).expect("nominal type count fits in u32"));
        let nominal = NominalRef {
            id,
            identity: Rc::new(identity),
        };
        self.entries.push(Entry {
            nominal: nominal.clone(),
            definition: None,
            functions: None,
        });
        nominal
    }

    /// A spec is only interned once its declaration has resolved, so its
    /// facts are never incomplete.
    pub fn intern_spec(&mut self, identity: NominalIdentity, facts: SpecFacts) -> NominalRef {
        assert_eq!(identity.kind, NominalKind::Spec);
        let nominal = self.intern(identity);
        self.define(
            &nominal,
            NominalKind::Spec,
            Definition::Spec(Rc::new(facts)),
        );
        nominal
    }

    /// Every interned type, in intern order.
    pub fn nominals(&self) -> impl Iterator<Item = &NominalRef> {
        self.entries.iter().map(|entry| &entry.nominal)
    }

    fn entry(&self, nominal: &NominalRef) -> &Entry {
        let entry = &self.entries[nominal.id.0 as usize];
        debug_assert!(
            Rc::ptr_eq(&entry.nominal.identity, &nominal.identity),
            "nominal handle read from a store that did not intern it"
        );
        entry
    }

    fn entry_mut(&mut self, nominal: &NominalRef) -> &mut Entry {
        &mut self.entries[nominal.id.0 as usize]
    }

    fn define(&mut self, nominal: &NominalRef, kind: NominalKind, definition: Definition) {
        assert_eq!(nominal.kind, kind, "nominal definition of the wrong kind");
        let slot = &mut self.entry_mut(nominal).definition;
        assert!(
            slot.is_none(),
            "internal error: the definition of '{}' was written twice",
            nominal.name
        );
        *slot = Some(definition);
    }

    pub fn define_struct(&mut self, nominal: &NominalRef, facts: StructFacts) {
        self.define(
            nominal,
            NominalKind::Struct,
            Definition::Struct(Rc::new(facts)),
        );
    }

    pub fn define_union(&mut self, nominal: &NominalRef, facts: UnionFacts) {
        self.define(
            nominal,
            NominalKind::Union,
            Definition::Union(Rc::new(facts)),
        );
    }

    pub fn define_enum(&mut self, nominal: &NominalRef, facts: EnumFacts) {
        self.define(nominal, NominalKind::Enum, Definition::Enum(Rc::new(facts)));
    }

    pub fn define_functions(
        &mut self,
        nominal: &NominalRef,
        functions: Vec<(Ident, ResolvedMethod)>,
    ) {
        assert_ne!(
            nominal.kind,
            NominalKind::Spec,
            "a spec has no method stage"
        );
        let slot = &mut self.entry_mut(nominal).functions;
        assert!(
            slot.is_none(),
            "internal error: the functions of '{}' were written twice",
            nominal.name
        );
        *slot = Some(Rc::new(functions));
    }

    fn definition(&self, nominal: &NominalRef) -> Result<&Definition, Incomplete> {
        self.entry(nominal)
            .definition
            .as_ref()
            .ok_or_else(|| Incomplete {
                nominal: nominal.clone(),
                stage: Stage::Definition,
            })
    }

    pub fn struct_facts(&self, nominal: &NominalRef) -> Result<&Rc<StructFacts>, Incomplete> {
        match self.definition(nominal)? {
            Definition::Struct(facts) => Ok(facts),
            _ => panic!("'{}' is not a struct", nominal.name),
        }
    }

    pub fn union_facts(&self, nominal: &NominalRef) -> Result<&Rc<UnionFacts>, Incomplete> {
        match self.definition(nominal)? {
            Definition::Union(facts) => Ok(facts),
            _ => panic!("'{}' is not a union", nominal.name),
        }
    }

    pub fn enum_facts(&self, nominal: &NominalRef) -> Result<&Rc<EnumFacts>, Incomplete> {
        match self.definition(nominal)? {
            Definition::Enum(facts) => Ok(facts),
            _ => panic!("'{}' is not an enum", nominal.name),
        }
    }

    pub fn spec_facts(&self, nominal: &NominalRef) -> &Rc<SpecFacts> {
        match self.definition(nominal) {
            Ok(Definition::Spec(facts)) => facts,
            _ => panic!("'{}' is not an interned spec", nominal.name),
        }
    }

    /// The declared fields of a struct or union.
    pub fn fields(&self, nominal: &NominalRef) -> Result<&[ResolvedField], Incomplete> {
        match self.definition(nominal)? {
            Definition::Struct(facts) => Ok(&facts.fields),
            Definition::Union(facts) => Ok(&facts.fields),
            _ => panic!("'{}' has no field list", nominal.name),
        }
    }

    /// The `#[suppress]` names on a type's declaration.
    pub fn suppress(&self, nominal: &NominalRef) -> Result<&[Ident], Incomplete> {
        Ok(match self.definition(nominal)? {
            Definition::Struct(facts) => &facts.suppress,
            Definition::Union(facts) => &facts.suppress,
            Definition::Enum(facts) => &facts.suppress,
            Definition::Spec(facts) => &facts.suppress,
        })
    }

    pub fn functions(&self, nominal: &NominalRef) -> Result<&Methods, Incomplete> {
        self.entry(nominal)
            .functions
            .as_ref()
            .ok_or_else(|| Incomplete {
                nominal: nominal.clone(),
                stage: Stage::Functions,
            })
    }

    /// Reads every fact a frozen store must have. Only code that runs after
    /// a successful analysis may use these: an incomplete type there is an
    /// internal error, not a user-facing cycle.
    pub fn frozen(&self) -> Frozen<'_> {
        Frozen(self)
    }
}

/// [`TypeStore`] reads for MIR lowering, layout and code generation, where
/// analysis has completed every reachable type.
#[derive(Clone, Copy)]
pub struct Frozen<'a>(&'a TypeStore);

impl<'a> Frozen<'a> {
    pub fn struct_facts(self, nominal: &NominalRef) -> &'a StructFacts {
        expect_complete(self.0.struct_facts(nominal).map(|facts| &**facts))
    }

    pub fn union_facts(self, nominal: &NominalRef) -> &'a UnionFacts {
        expect_complete(self.0.union_facts(nominal).map(|facts| &**facts))
    }

    pub fn enum_facts(self, nominal: &NominalRef) -> &'a EnumFacts {
        expect_complete(self.0.enum_facts(nominal).map(|facts| &**facts))
    }

    pub fn fields(self, nominal: &NominalRef) -> &'a [ResolvedField] {
        expect_complete(self.0.fields(nominal))
    }

    pub fn functions(self, nominal: &NominalRef) -> &'a [(Ident, ResolvedMethod)] {
        expect_complete(
            self.0
                .functions(nominal)
                .map(|functions| functions.as_slice()),
        )
    }
}

fn expect_complete<T: ?Sized>(read: Result<&T, Incomplete>) -> &T {
    match read {
        Ok(facts) => facts,
        Err(incomplete) => panic!(
            "internal error: {:?} of '{}' read before it was written",
            incomplete.stage, incomplete.nominal.name
        ),
    }
}

/// A per-thread store for synthetic types in unit tests. The test harness
/// runs each test on its own thread, so tests never share interned types.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static STORE: RefCell<TypeStore> = RefCell::default();
    }

    pub(crate) fn with<T>(read: impl FnOnce(&TypeStore) -> T) -> T {
        STORE.with(|store| read(&store.borrow()))
    }

    pub(crate) fn with_mut<T>(write: impl FnOnce(&mut TypeStore) -> T) -> T {
        STORE.with(|store| write(&mut store.borrow_mut()))
    }

    /// A copy of this thread's store for a test resolver that must lend a
    /// reference to it. Leaked: it lives as long as the test process.
    pub(crate) fn snapshot() -> &'static TypeStore {
        Box::leak(Box::new(with(TypeStore::clone)))
    }

    pub(crate) fn identity(
        local: u32,
        kind: NominalKind,
        name: &str,
        module: &[&str],
        generic_args: Vec<ResolvedGenericArg>,
    ) -> NominalIdentity {
        NominalIdentity {
            decl_id: HirId {
                module: omega_hir::ModuleId(0),
                local,
            },
            kind,
            name: Ident(name.into()),
            module_path: ModulePath::from_names(module),
            generic_args,
            variant_names: vec![],
        }
    }

    pub(crate) fn define_struct(identity: NominalIdentity, facts: StructFacts) -> ResolvedType {
        with_mut(|store| {
            let nominal = store.intern(identity);
            store.define_struct(&nominal, facts);
            store.define_functions(&nominal, vec![]);
            ResolvedType::Struct(nominal)
        })
    }

    pub(crate) fn define_union(identity: NominalIdentity, facts: UnionFacts) -> ResolvedType {
        with_mut(|store| {
            let nominal = store.intern(identity);
            store.define_union(&nominal, facts);
            store.define_functions(&nominal, vec![]);
            ResolvedType::Union(nominal)
        })
    }

    /// The variant names come from `facts`, as a declaration would give them.
    pub(crate) fn define_enum(mut identity: NominalIdentity, facts: EnumFacts) -> ResolvedType {
        identity.variant_names = facts.variants.iter().map(|v| v.name.clone()).collect();
        with_mut(|store| {
            let nominal = store.intern(identity);
            store.define_enum(&nominal, facts);
            store.define_functions(&nominal, vec![]);
            ResolvedType::Enum {
                nominal,
                variant: None,
            }
        })
    }

    pub(crate) fn define_spec(identity: NominalIdentity, facts: SpecFacts) -> NominalRef {
        with_mut(|store| store.intern_spec(identity, facts))
    }
}

#[cfg(test)]
mod tests;
