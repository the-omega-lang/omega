use omega_parser::prelude::Ident;
use std::borrow::Borrow;
use std::fmt;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId {
    pub module: ModuleId,
    pub local: u32,
}

pub const SYNTHETIC_MODULE: ModuleId = ModuleId(u32::MAX);

pub(crate) struct HirIdGen {
    module: ModuleId,
    next: u32,
}

impl HirIdGen {
    pub fn new(module: ModuleId) -> Self {
        Self { module, next: 0 }
    }

    pub fn next(&mut self) -> HirId {
        let local = self.next;
        self.next += 1;
        HirId {
            module: self.module,
            local,
        }
    }
}

/// The absolute identity of a module: its package-qualified segments.
///
/// Equality, hashing and ordering are by content, so two paths spelled
/// independently denote the same module. Cloning shares the segments.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModulePath(Rc<[Ident]>);

impl ModulePath {
    pub fn new(segments: impl Into<Rc<[Ident]>>) -> Self {
        Self(segments.into())
    }

    /// A path spelled by the compiler itself, such as a `core` module it
    /// relies on.
    pub fn from_names(names: &[&str]) -> Self {
        names.iter().map(|name| Ident(name.to_string())).collect()
    }

    /// Whether this is the path spelled by `names`.
    pub fn is(&self, names: &[&str]) -> bool {
        self.0.iter().map(Ident::as_ref).eq(names.iter().copied())
    }

    pub fn segments(&self) -> &[Ident] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The package this module belongs to.
    pub fn root(&self) -> Option<&Ident> {
        self.0.first()
    }

    pub fn parent(&self) -> Option<ModulePath> {
        let (_, parent) = self.0.split_last()?;
        Some(Self::new(parent))
    }

    /// The submodule `name` of this module.
    pub fn child(&self, name: Ident) -> ModulePath {
        self.0
            .iter()
            .cloned()
            .chain(std::iter::once(name))
            .collect()
    }

    /// The item `name` declared in this module.
    pub fn join(&self, name: Ident) -> ItemPath {
        ItemPath {
            module: self.clone(),
            name,
        }
    }

    /// The item `rest` names below this module, where every segment of
    /// `rest` but the last is a submodule. `None` when `rest` is empty.
    pub fn join_segments(&self, rest: &[Ident]) -> Option<ItemPath> {
        let (name, submodules) = rest.split_last()?;
        let module = self.0.iter().chain(submodules).cloned().collect();
        Some(ItemPath::new(module, name.clone()))
    }

    /// This module read as an item of its parent, for a path that may
    /// denote either. `None` for the empty path.
    pub fn as_item(&self) -> Option<ItemPath> {
        ItemPath::from_segments(&self.0)
    }

    pub fn starts_with(&self, prefix: &ModulePath) -> bool {
        self.0.starts_with(&prefix.0)
    }
}

impl FromIterator<Ident> for ModulePath {
    fn from_iter<I: IntoIterator<Item = Ident>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl From<Vec<Ident>> for ModulePath {
    fn from(segments: Vec<Ident>) -> Self {
        Self::new(segments)
    }
}

impl From<&[Ident]> for ModulePath {
    fn from(segments: &[Ident]) -> Self {
        Self::new(segments)
    }
}

impl Borrow<[Ident]> for ModulePath {
    fn borrow(&self) -> &[Ident] {
        &self.0
    }
}

impl fmt::Display for ModulePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str("::")?;
            }
            f.write_str(segment.as_ref())?;
        }
        Ok(())
    }
}

impl fmt::Debug for ModulePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModulePath({self})")
    }
}

/// The absolute identity of a top-level item: its declaring module and name.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemPath {
    pub module: ModulePath,
    pub name: Ident,
}

impl ItemPath {
    pub fn new(module: ModulePath, name: Ident) -> Self {
        Self { module, name }
    }

    /// Splits written absolute segments into module and item. `None` for an
    /// empty path.
    pub fn from_segments(segments: &[Ident]) -> Option<Self> {
        let (name, module) = segments.split_last()?;
        Some(Self::new(ModulePath::from(module), name.clone()))
    }

    pub fn segments(&self) -> impl Iterator<Item = &Ident> {
        self.module
            .segments()
            .iter()
            .chain(std::iter::once(&self.name))
    }

    pub fn to_segments(&self) -> Vec<Ident> {
        self.segments().cloned().collect()
    }

    /// This path read as the module it would name, for a path that may
    /// denote either a module or an item.
    pub fn as_module(&self) -> ModulePath {
        self.module.child(self.name.clone())
    }
}

impl fmt::Display for ItemPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.module.is_empty() {
            write!(f, "{}::", self.module)?;
        }
        f.write_str(self.name.as_ref())
    }
}

impl fmt::Debug for ItemPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ItemPath({self})")
    }
}

#[cfg(test)]
mod tests;
