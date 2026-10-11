//! The pattern space `match` checking works in.
//!
//! A [`Pat`] is one rectangle of values: a choice per leaf (an integral
//! interval, an enum variant, an anonymous-enum member) and a subpattern per
//! named component. Every node carries the facts about its own position that
//! subtraction needs to enumerate the rest of that position, so the engine
//! never consults types.

use std::rc::Rc;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafKind {
    Integer,
    Bool,
    Char,
}

/// The full domain of an integral position.
#[derive(Debug)]
pub struct Leaf {
    pub lo: i128,
    pub hi: i128,
    pub kind: LeafKind,
}

/// The alternatives of an enum or anonymous-enum position, as they render.
#[derive(Debug)]
pub struct Sum {
    pub names: Vec<String>,
}

/// One component of a product. `position` is the declaration order the
/// product is kept sorted by; only named fields exist so far.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ComponentKey {
    pub position: usize,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum Pat {
    Any,
    Range {
        lo: i128,
        hi: i128,
        leaf: Rc<Leaf>,
    },
    Variant {
        sum: Rc<Sum>,
        index: usize,
        fields: Product,
    },
    Member {
        sum: Rc<Sum>,
        index: usize,
        inner: Box<Pat>,
    },
    Struct {
        name: Rc<str>,
        fields: Product,
    },
}

impl Pat {
    fn is_empty(&self) -> bool {
        match self {
            Self::Any => false,
            Self::Range { lo, hi, .. } => lo > hi,
            Self::Variant { fields, .. } | Self::Struct { fields, .. } => {
                fields.components().iter().any(|(_, pat)| pat.is_empty())
            }
            Self::Member { inner, .. } => inner.is_empty(),
        }
    }
}

/// Components sorted by key; a missing component is [`Pat::Any`].
#[derive(Debug, Clone, Default)]
pub struct Product(Vec<(ComponentKey, Pat)>);

impl Product {
    pub fn new(mut components: Vec<(ComponentKey, Pat)>) -> Self {
        components.retain(|(_, pat)| !matches!(pat, Pat::Any));
        components.sort_by(|a, b| a.0.cmp(&b.0));
        Self(components)
    }

    pub fn components(&self) -> &[(ComponentKey, Pat)] {
        &self.0
    }

    fn get(&self, key: &ComponentKey) -> &Pat {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&Pat::Any, |(_, pat)| pat)
    }

    fn with(&self, key: &ComponentKey, pat: Pat) -> Self {
        let mut components: Vec<_> = self.0.iter().filter(|(k, _)| k != key).cloned().collect();
        components.push((key.clone(), pat));
        Self::new(components)
    }

    fn keys<'a>(&'a self, other: &'a Self) -> Vec<&'a ComponentKey> {
        let mut keys: Vec<_> = self.0.iter().chain(&other.0).map(|(k, _)| k).collect();
        keys.sort();
        keys.dedup();
        keys
    }
}

/// One `match` arm: the alternatives any of which selects it. Exactly one
/// today; checks and subtraction already treat it as a union.
#[derive(Debug, Clone)]
pub struct Arm {
    pub alternatives: Vec<Pat>,
}

/// How pattern `a` relates to pattern `b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relation {
    Disjoint,
    Equal,
    /// `a` lies strictly inside `b`.
    Sub,
    /// `b` lies strictly inside `a`.
    Super,
    /// Inside in opposite directions on different components.
    SameWeight,
    /// Two leaves overlap without either containing the other; the path names
    /// the component the leaf sits at.
    Partial(Vec<String>),
}

impl Relation {
    fn combine(relations: impl IntoIterator<Item = Relation>) -> Relation {
        let (mut sub, mut sup, mut partial) = (false, false, None);
        let mut any = false;
        for relation in relations {
            any = true;
            match relation {
                Relation::Disjoint => return Relation::Disjoint,
                Relation::Partial(path) => {
                    partial.get_or_insert(path);
                }
                Relation::Equal => {}
                Relation::Sub => sub = true,
                Relation::Super => sup = true,
                Relation::SameWeight => (sub, sup) = (true, true),
            }
        }
        if !any {
            return Relation::Equal;
        }
        if let Some(path) = partial {
            return Relation::Partial(path);
        }
        match (sub, sup) {
            (false, false) => Relation::Equal,
            (true, false) => Relation::Sub,
            (false, true) => Relation::Super,
            (true, true) => Relation::SameWeight,
        }
    }
}

pub fn relate_arms(a: &Arm, b: &Arm) -> Relation {
    let relations: Vec<_> = a
        .alternatives
        .iter()
        .flat_map(|x| b.alternatives.iter().map(move |y| relate(x, y)))
        .filter(|relation| *relation != Relation::Disjoint)
        .collect();
    if relations.is_empty() {
        return Relation::Disjoint;
    }
    Relation::combine(relations)
}

pub fn relate(a: &Pat, b: &Pat) -> Relation {
    if a.is_empty() || b.is_empty() {
        return Relation::Disjoint;
    }
    match (a, b) {
        (Pat::Any, Pat::Any) => Relation::Equal,
        (
            Pat::Range { lo, hi, .. },
            Pat::Range {
                lo: blo, hi: bhi, ..
            },
        ) => relate_intervals((*lo, *hi), (*blo, *bhi)),
        (Pat::Any, Pat::Range { lo, hi, leaf }) => relate_intervals((leaf.lo, leaf.hi), (*lo, *hi)),
        (Pat::Range { lo, hi, leaf }, Pat::Any) => relate_intervals((*lo, *hi), (leaf.lo, leaf.hi)),
        (
            Pat::Variant { index, fields, .. },
            Pat::Variant {
                index: b_index,
                fields: b_fields,
                ..
            },
        ) => {
            if index != b_index {
                Relation::Disjoint
            } else {
                relate_products(fields, b_fields)
            }
        }
        (Pat::Any, Pat::Variant { sum, fields, .. }) if sum.names.len() == 1 => {
            relate_products(&Product::default(), fields)
        }
        (Pat::Variant { sum, fields, .. }, Pat::Any) if sum.names.len() == 1 => {
            relate_products(fields, &Product::default())
        }
        (
            Pat::Member { index, inner, .. },
            Pat::Member {
                index: b_index,
                inner: b_inner,
                ..
            },
        ) => {
            if index != b_index {
                Relation::Disjoint
            } else {
                relate(inner, b_inner)
            }
        }
        (Pat::Any, Pat::Member { sum, inner, .. }) if sum.names.len() == 1 => {
            relate(&Pat::Any, inner)
        }
        (Pat::Member { sum, inner, .. }, Pat::Any) if sum.names.len() == 1 => {
            relate(inner, &Pat::Any)
        }
        (Pat::Any, Pat::Variant { .. } | Pat::Member { .. }) => Relation::Super,
        (Pat::Variant { .. } | Pat::Member { .. }, Pat::Any) => Relation::Sub,
        (
            Pat::Struct { fields, .. },
            Pat::Struct {
                fields: b_fields, ..
            },
        ) => relate_products(fields, b_fields),
        (Pat::Any, Pat::Struct { fields, .. }) => relate_products(&Product::default(), fields),
        (Pat::Struct { fields, .. }, Pat::Any) => relate_products(fields, &Product::default()),
        _ => unreachable!("patterns at one position always share that position's kind"),
    }
}

fn relate_intervals(a: (i128, i128), b: (i128, i128)) -> Relation {
    if a.1 < b.0 || b.1 < a.0 {
        Relation::Disjoint
    } else if a == b {
        Relation::Equal
    } else if b.0 <= a.0 && a.1 <= b.1 {
        Relation::Sub
    } else if a.0 <= b.0 && b.1 <= a.1 {
        Relation::Super
    } else {
        Relation::Partial(Vec::new())
    }
}

fn relate_products(a: &Product, b: &Product) -> Relation {
    Relation::combine(
        a.keys(b)
            .into_iter()
            .map(|key| match relate(a.get(key), b.get(key)) {
                Relation::Partial(mut path) => {
                    path.insert(0, key.name.clone());
                    Relation::Partial(path)
                }
                other => other,
            }),
    )
}

pub fn intersect(a: &Pat, b: &Pat) -> Option<Pat> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    match (a, b) {
        (Pat::Any, other) | (other, Pat::Any) => Some(other.clone()),
        (
            Pat::Range { lo, hi, leaf },
            Pat::Range {
                lo: blo, hi: bhi, ..
            },
        ) => {
            let (lo, hi) = ((*lo).max(*blo), (*hi).min(*bhi));
            (lo <= hi).then(|| Pat::Range {
                lo,
                hi,
                leaf: leaf.clone(),
            })
        }
        (
            Pat::Variant { sum, index, fields },
            Pat::Variant {
                index: b_index,
                fields: b_fields,
                ..
            },
        ) => {
            if index != b_index {
                return None;
            }
            Some(Pat::Variant {
                sum: sum.clone(),
                index: *index,
                fields: intersect_products(fields, b_fields)?,
            })
        }
        (
            Pat::Member { sum, index, inner },
            Pat::Member {
                index: b_index,
                inner: b_inner,
                ..
            },
        ) => {
            if index != b_index {
                return None;
            }
            Some(Pat::Member {
                sum: sum.clone(),
                index: *index,
                inner: Box::new(intersect(inner, b_inner)?),
            })
        }
        (
            Pat::Struct { name, fields },
            Pat::Struct {
                fields: b_fields, ..
            },
        ) => Some(Pat::Struct {
            name: name.clone(),
            fields: intersect_products(fields, b_fields)?,
        }),
        _ => unreachable!("patterns at one position always share that position's kind"),
    }
}

fn intersect_products(a: &Product, b: &Product) -> Option<Product> {
    let components = a
        .keys(b)
        .into_iter()
        .map(|key| Some((key.clone(), intersect(a.get(key), b.get(key))?)))
        .collect::<Option<Vec<_>>>()?;
    Some(Product::new(components))
}

pub fn arm_intersects(space: &[Pat], arm: &Arm) -> bool {
    space.iter().any(|cell| {
        arm.alternatives
            .iter()
            .any(|alternative| intersect(cell, alternative).is_some())
    })
}

/// `space` minus `arm`, as disjoint cells.
pub fn subtract(space: Vec<Pat>, arm: &Arm) -> Vec<Pat> {
    arm.alternatives.iter().fold(space, |space, alternative| {
        space
            .iter()
            .flat_map(|cell| difference(cell, alternative))
            .collect()
    })
}

fn difference(cell: &Pat, pat: &Pat) -> Vec<Pat> {
    if intersect(cell, pat).is_none() {
        return vec![cell.clone()];
    }
    match (cell, pat) {
        (_, Pat::Any) => Vec::new(),
        (Pat::Any, _) => expand(pat)
            .iter()
            .flat_map(|cell| difference(cell, pat))
            .collect(),
        (
            Pat::Range { lo, hi, leaf },
            Pat::Range {
                lo: plo, hi: phi, ..
            },
        ) => {
            let mut pieces = Vec::new();
            if lo < plo {
                pieces.push(Pat::Range {
                    lo: *lo,
                    hi: plo - 1,
                    leaf: leaf.clone(),
                });
            }
            if phi < hi {
                pieces.push(Pat::Range {
                    lo: phi + 1,
                    hi: *hi,
                    leaf: leaf.clone(),
                });
            }
            pieces
        }
        (
            Pat::Variant { sum, index, fields },
            Pat::Variant {
                fields: p_fields, ..
            },
        ) => product_difference(fields, p_fields)
            .into_iter()
            .map(|fields| Pat::Variant {
                sum: sum.clone(),
                index: *index,
                fields,
            })
            .collect(),
        (Pat::Member { sum, index, inner }, Pat::Member { inner: p_inner, .. }) => {
            difference(inner, p_inner)
                .into_iter()
                .map(|inner| Pat::Member {
                    sum: sum.clone(),
                    index: *index,
                    inner: Box::new(inner),
                })
                .collect()
        }
        (
            Pat::Struct { name, fields },
            Pat::Struct {
                fields: p_fields, ..
            },
        ) => product_difference(fields, p_fields)
            .into_iter()
            .map(|fields| Pat::Struct {
                name: name.clone(),
                fields,
            })
            .collect(),
        _ => unreachable!("patterns at one position always share that position's kind"),
    }
}

/// `Any` at `pat`'s position, split into that position's constructors.
fn expand(pat: &Pat) -> Vec<Pat> {
    match pat {
        Pat::Any => vec![Pat::Any],
        Pat::Range { leaf, .. } => vec![Pat::Range {
            lo: leaf.lo,
            hi: leaf.hi,
            leaf: leaf.clone(),
        }],
        Pat::Variant { sum, .. } => (0..sum.names.len())
            .map(|index| Pat::Variant {
                sum: sum.clone(),
                index,
                fields: Product::default(),
            })
            .collect(),
        Pat::Member { sum, .. } => (0..sum.names.len())
            .map(|index| Pat::Member {
                sum: sum.clone(),
                index,
                inner: Box::new(Pat::Any),
            })
            .collect(),
        Pat::Struct { name, .. } => vec![Pat::Struct {
            name: name.clone(),
            fields: Product::default(),
        }],
    }
}

/// Splits `cell \ pat` component by component: the i-th piece agrees with
/// `pat` on every earlier component and misses it on the i-th, so the pieces
/// are disjoint.
fn product_difference(cell: &Product, pat: &Product) -> Vec<Product> {
    let mut agreed = cell.clone();
    let mut pieces = Vec::new();
    for (key, p) in pat.components() {
        let c = agreed.get(key).clone();
        for piece in difference(&c, p) {
            pieces.push(agreed.with(key, piece));
        }
        let Some(both) = intersect(&c, p) else {
            break;
        };
        agreed = agreed.with(key, both);
    }
    pieces
}

/// A cell rendered as the pattern that would cover it.
pub fn render(pat: &Pat) -> String {
    match pat {
        Pat::Any => "..".to_string(),
        Pat::Range { lo, hi, leaf } => render_interval(leaf.kind, *lo, *hi),
        Pat::Variant { sum, index, fields } => {
            let name = &sum.names[*index];
            if fields.components().is_empty() {
                name.clone()
            } else {
                format!("{name} {}", render_fields(fields))
            }
        }
        Pat::Member { sum, index, inner } => match **inner {
            Pat::Any => sum.names[*index].clone(),
            ref inner => render(inner),
        },
        Pat::Struct { name, fields } => format!("{name} {}", render_fields(fields)),
    }
}

fn render_fields(fields: &Product) -> String {
    let mut text = String::from("{ ");
    for (key, pat) in fields.components() {
        text.push_str(&format!("{} = {}; ", key.name, render(pat)));
    }
    text.push_str(".. }");
    text
}

pub fn render_interval(kind: LeafKind, lo: i128, hi: i128) -> String {
    let render = |n: i128| match kind {
        LeafKind::Bool => (n != 0).to_string(),
        LeafKind::Char => match char::from_u32(n as u32) {
            Some(c) if c.is_ascii_graphic() || c == ' ' => format!("'{c}'"),
            _ => format!("U+{n:04X}"),
        },
        LeafKind::Integer => n.to_string(),
    };
    if lo == hi {
        render(lo)
    } else {
        format!("{}..={}", render(lo), render(hi))
    }
}
