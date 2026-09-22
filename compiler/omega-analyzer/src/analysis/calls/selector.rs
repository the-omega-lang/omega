use super::*;
use crate::generics::GenericSubstitution;
use crate::resolver::{DeclaredBound, GenericCallTarget};
use crate::resolved_type::ResolvedSpecType;
use omega_parser::prelude::ExprGenericArg;
use std::cell::RefCell;
use std::rc::Rc;

/// The exact bound set one written position requires of the declaration it
/// selects, resolved in the caller's own visibility context.
#[derive(Debug, Clone)]
pub(crate) struct BoundSelector {
    pub(crate) span: Span,
    /// Canonical keys, deduplicated. Order carries no meaning: two selectors
    /// naming the same specs select identically however they were written.
    pub(crate) required: Vec<DeclaredBound>,
    /// What was written, for diagnostics.
    pub(crate) description: String,
}

impl BoundSelector {
    /// Whether a declaration whose parameter declares exactly `declared`
    /// is the one this selector names. Both sides are canonical keys, so
    /// this is set equality.
    pub(crate) fn matches(&self, declared: &[DeclaredBound]) -> bool {
        self.required.len() == declared.len()
            && self.required.iter().all(|bound| declared.contains(bound))
    }
}

/// A written generic-argument list after caller validation. See
/// [`Analyzer::validate_written_generics`].
#[derive(Debug, Clone, Default)]
pub(crate) struct ValidatedGenerics {
    entries: Vec<ValidatedEntry>,
}

impl ValidatedGenerics {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone)]
struct ValidatedEntry {
    /// Kept so a plain argument can still be read against the parameter kind
    /// of whichever declaration it reaches.
    written: ExprGenericArg,
    selector: Option<BoundSelector>,
    /// The type a typed selector fixes, resolved in the caller's context.
    fixed: Option<ResolvedGenericArg>,
}

/// A written generic-argument list, split into what it binds and what it
/// demands of the declaration it selects.
#[derive(Debug, Clone, Default)]
pub(crate) struct WrittenGenerics {
    /// Positional bindings. A `spec ...` selector occupies its position
    /// without binding it, so its slot stays `None` for ordinary inference.
    pub(crate) bindings: Vec<Option<ResolvedGenericArg>>,
    /// The selector written at each position, where one was.
    pub(crate) selectors: Vec<Option<BoundSelector>>,
    /// Positions written as a plain type argument, which is what the
    /// unbounded preference ranks. A `comp` position is excluded here: it
    /// declares no bounds either way, so it can never distinguish
    /// declarations.
    plain_type_positions: Vec<usize>,
}

impl WrittenGenerics {
    pub(crate) fn has_selectors(&self) -> bool {
        self.selectors.iter().any(Option::is_some)
    }

    /// The written plain-type positions at which a candidate declares no
    /// bounds. A candidate whose set strictly contains another's is the more
    /// specific answer to what the caller wrote; see `dominates`.
    pub(crate) fn unbounded_positions(&self, unbounded: impl Fn(usize) -> bool) -> Vec<usize> {
        self.plain_type_positions
            .iter()
            .copied()
            .filter(|position| unbounded(*position))
            .collect()
    }

    /// Whether the unbounded-position sets rank `left` above `right`.
    /// Strict inclusion only: incomparable sets leave both candidates
    /// standing, which is an ordinary ambiguity rather than a winner.
    pub(crate) fn dominates(left: &[usize], right: &[usize]) -> bool {
        right.iter().all(|position| left.contains(position)) && left.len() > right.len()
    }
}

impl<'r> Analyzer<'r> {
    /// A written list as an ordinary generic application reads it. Every
    /// other consumer of generic-argument syntax applies arguments to
    /// something with no declarations to choose between, so a selector there
    /// is rejected rather than silently dropped.
    pub(crate) fn ordinary_generic_args(
        &mut self,
        node_id: HirId,
        span: Span,
        written: &[ExprGenericArg],
        applied_to: &str,
    ) -> Option<Vec<GenericArg>> {
        let mut args = Vec::with_capacity(written.len());
        for entry in written {
            let Some(arg) = entry.plain() else {
                self.error(
                    node_id,
                    entry.selector_span().unwrap_or(span),
                    AnalysisErrorKind::BoundSelectorNotAllowed {
                        applied_to: applied_to.to_string(),
                    },
                );
                return None;
            };
            args.push(arg.clone());
        }
        Some(args)
    }

    /// The same reading, without a diagnostic. Used where a selector simply
    /// means "this is not the path shape being tried", and the reading that
    /// does apply reports what is wrong.
    pub(crate) fn plain_generic_args(written: &[ExprGenericArg]) -> Option<Vec<GenericArg>> {
        written.iter().map(|entry| entry.plain().cloned()).collect()
    }

    /// Settles everything a written list means to the *caller*: every
    /// selector's names, their visibility, the obligations the aliases in
    /// them carry, and the concrete type a typed selector fixes.
    ///
    /// None of that depends on which declarations happen to be in scope, so
    /// it is decided once, here, rather than per candidate under suppressed
    /// diagnostics. Adding, hiding, or reordering overloads therefore cannot
    /// make an invalid selector acceptable, nor report it twice. `None`
    /// means the list is wrong and the reason was reported: the call or
    /// value reference must stop rather than resolve some other way.
    pub(crate) fn validate_written_generics(
        &mut self,
        node_id: HirId,
        span: Span,
        written: &[ExprGenericArg],
    ) -> Option<ValidatedGenerics> {
        let mut entries = Vec::with_capacity(written.len());
        let mut ok = true;
        for entry in written {
            let selector_span = entry.selector_span().unwrap_or(span);
            let selector = match entry.selector() {
                None => None,
                Some(bounds) => match self.resolve_bound_selector(node_id, selector_span, bounds) {
                    Some(selector) => Some(selector),
                    None => {
                        ok = false;
                        continue;
                    }
                },
            };
            // A type written beside a selector is caller-owned: a selector
            // never lands on a `comp` parameter, so that type means the same
            // whatever declaration the entry reaches. A plain argument's
            // kind, and a value written where only a selector could put one,
            // are still the candidate's to judge.
            let fixed = match (&selector, entry.arg()) {
                (Some(_), Some(arg @ GenericArg::Type(_))) => {
                    match self.resolve_generic_arg_validated(node_id, span, arg, None) {
                        (true, Some(resolved)) => Some(resolved),
                        _ => {
                            ok = false;
                            continue;
                        }
                    }
                }
                _ => None,
            };
            entries.push(ValidatedEntry {
                written: entry.clone(),
                selector,
                fixed,
            });
        }
        ok.then_some(ValidatedGenerics { entries })
    }

    /// Resolves a written list against the generic parameters it applies to,
    /// reporting what does not fit. `owner` names the declaration only so an
    /// excess argument can be reported against it.
    pub(crate) fn resolve_written_generics(
        &mut self,
        node_id: HirId,
        span: Span,
        written: &[ExprGenericArg],
        owner: &[Ident],
        params: &[HirGenericParam],
    ) -> Option<WrittenGenerics> {
        self.check_generic_arity(node_id, span, owner, params, written.len())?;
        let validated = self.validate_written_generics(node_id, span, written)?;
        self.bind_written_generics(node_id, span, &validated, params, true)
    }

    /// Applies an already-validated list to one declaration's parameters.
    ///
    /// Only what depends on *this* declaration is decided here: parameter
    /// kind, arity, and how a plain argument reads against the slot it
    /// lands on. Nothing resolves a selector name or rechecks an alias
    /// constraint, so a candidate can be rejected without its rejection
    /// changing what the caller wrote. With `report`, a list that does not
    /// fit is diagnosed against this declaration; without it, not fitting
    /// simply means this is not the declaration meant.
    pub(crate) fn bind_written_generics(
        &mut self,
        node_id: HirId,
        span: Span,
        validated: &ValidatedGenerics,
        params: &[HirGenericParam],
        report: bool,
    ) -> Option<WrittenGenerics> {
        let written = &validated.entries;
        let mut result = WrittenGenerics {
            bindings: vec![None; written.len()],
            selectors: vec![None; written.len()],
            plain_type_positions: Vec::new(),
        };
        let mut ok = true;
        for (position, entry) in written.iter().enumerate() {
            let param = params.get(position);
            if entry.selector.is_some() && param.is_some_and(HirGenericParam::is_comp) {
                if report {
                    self.error(
                        node_id,
                        entry.written.selector_span().unwrap_or(span),
                        AnalysisErrorKind::BoundSelectorOnCompParam {
                            parameter: param.unwrap().ident.clone(),
                        },
                    );
                }
                ok = false;
                continue;
            }
            match (&entry.fixed, entry.written.arg()) {
                (Some(fixed), _) => result.bindings[position] = Some(fixed.clone()),
                (None, Some(arg)) => {
                    let resolved = if report {
                        self.resolve_generic_arg_or_error(node_id, span, arg, param)
                    } else {
                        self.without_diagnostics(|this| {
                            this.resolve_generic_arg_or_error(node_id, span, arg, param)
                        })
                    };
                    match resolved {
                        Some(resolved) => result.bindings[position] = Some(resolved),
                        None => ok = false,
                    }
                }
                (None, None) => {}
            }
            match &entry.selector {
                None => {
                    if !param.is_some_and(HirGenericParam::is_comp) {
                        result.plain_type_positions.push(position);
                    }
                }
                Some(selector) => result.selectors[position] = Some(selector.clone()),
            }
        }
        ok.then_some(result)
    }

    /// The canonical bound set a written selector names. Names resolve in the
    /// caller's ordinary visibility context; conjunction aliases expand, so
    /// `A + B`, `B + A` and an alias of either produce the same key set.
    fn resolve_bound_selector(
        &mut self,
        node_id: HirId,
        span: Span,
        written: &[Type],
    ) -> Option<BoundSelector> {
        let description = written
            .iter()
            .map(crate::error::raw_type_display)
            .collect::<Vec<_>>()
            .join(" + ");
        let module = self.module_path.clone();
        // An alias carries its own bounds, and expanding a conjunction alias
        // below erases the wrapper that declares them, so they are checked
        // at the spelling that wrote them -- once, before expansion.
        let mut met = true;
        for bound in written {
            met &= self.check_alias_generic_bounds(node_id, span, bound, &module);
        }
        if !met {
            return None;
        }
        let expanded = match crate::aliases::expand_bounds(&mut *self.resolver, &module, written) {
            Ok(expanded) => expanded,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                return None;
            }
        };
        let mut required: Vec<DeclaredBound> = Vec::with_capacity(expanded.len());
        for bound in &expanded {
            let (_, key) = self.expanded_bound_key(node_id, span, bound)?;
            if !required.contains(&key) {
                required.push(key);
            }
        }
        Some(BoundSelector {
            span,
            required,
            description,
        })
    }

    /// One bound as a canonical key, with the spec it names. The bounds the
    /// aliases in it declare are checked first, and an unmet one makes this
    /// no key at all: a resolved spec comes back either way, so returning it
    /// would let a caller proceed on an obligation that failed.
    pub fn bound_key(
        &mut self,
        node_id: HirId,
        span: Span,
        bound: &Type,
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, DeclaredBound)> {
        let module = self.module_path.clone();
        if !self.check_alias_generic_bounds(node_id, span, bound, &module) {
            return None;
        }
        self.expanded_bound_key(node_id, span, bound)
    }

    /// The same, for a bound whose alias obligations the caller already
    /// checked at the spelling that wrote them. Aliases are expanded before
    /// extracting arguments; declared defaults are then applied so that the
    /// same bound compares equal whether or not a defaulted argument was
    /// written out.
    fn expanded_bound_key(
        &mut self,
        node_id: HirId,
        span: Span,
        bound: &Type,
    ) -> Option<(Rc<RefCell<ResolvedSpecType>>, DeclaredBound)> {
        let module = self.module_path.clone();
        let expanded = match crate::aliases::expand_type_alias(self.resolver, &module, bound.clone()) {
            Ok(expanded) => expanded,
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                return None;
            }
        };
        let (spec, mut args) = self.resolve_spec_reference(node_id, span, &expanded)?;
        let (id, name, params, module) = {
            let cell = spec.borrow();
            (
                cell.id,
                cell.name.clone(),
                cell.generics.clone(),
                cell.module_path.clone(),
            )
        };
        // A spec's defaults belong to the spec's own module, not to whichever
        // site is naming the bound.
        let saved = std::mem::replace(&mut self.module_path, module);
        for param in &params[args.len().min(params.len())..] {
            let Some(default) = param.default.clone() else {
                break;
            };
            let substitution = GenericSubstitution::zip(params.iter().map(|p| &p.ident), &args);
            let Some(resolved) =
                self.resolve_default_generic_arg(node_id, span, param, &default, &substitution)
            else {
                self.module_path = saved;
                return None;
            };
            args.push(resolved);
        }
        self.module_path = saved;
        Some((
            spec,
            DeclaredBound {
                spec: id,
                args,
                name,
            },
        ))
    }

    /// What a written list binds, as a substitution. A `spec ...` selector
    /// occupies its position without binding it, so the name it stands for
    /// stays open to ordinary inference rather than shifting the arguments
    /// after it.
    pub(crate) fn written_substitution(
        params: &[HirGenericParam],
        written: &WrittenGenerics,
    ) -> GenericSubstitution {
        let mut substitution = GenericSubstitution::new();
        for (param, binding) in params.iter().zip(&written.bindings) {
            if let Some(arg) = binding {
                substitution.push(param.ident.clone(), arg.clone());
            }
        }
        substitution
    }

    /// Checks the selectors a written list applied to a lone generic
    /// declaration. With nothing to choose between, a selector that does not
    /// name this declaration's own bounds is an error rather than a reason to
    /// look elsewhere.
    pub(crate) fn check_written_selectors(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        target: GenericCallTarget<'_>,
        written: &WrittenGenerics,
        generic_args: &[ResolvedGenericArg],
    ) -> Option<()> {
        if !written.has_selectors() {
            return Some(());
        }
        let arguments: Vec<Option<ResolvedGenericArg>> =
            generic_args.iter().cloned().map(Some).collect();
        let prepared = match self.resolver.prepare_generic_call(target, &arguments) {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return Some(()),
            Err(error) => {
                self.error(node_id, span, AnalysisErrorKind::ModuleResolution(error));
                return None;
            }
        };
        for (position, selector) in written.selectors.iter().enumerate() {
            let Some(selector) = selector else { continue };
            let declared = prepared.bounds.get(position).map_or(&[][..], Vec::as_slice);
            if selector.matches(declared) {
                continue;
            }
            self.error(
                node_id,
                selector.span,
                AnalysisErrorKind::NoMatchingBoundSelector {
                    name: name.clone(),
                    selector: selector.description.clone(),
                    candidates: vec![Self::describe_declared_bounds(declared)],
                },
            );
            return None;
        }
        Some(())
    }

    /// The bound set a declaration actually declares at one position, named
    /// the way a selector would have had to write it.
    fn describe_declared_bounds(declared: &[DeclaredBound]) -> String {
        if declared.is_empty() {
            return "declares no bounds".to_string();
        }
        format!(
            "declares {}",
            declared
                .iter()
                .map(DeclaredBound::to_string)
                .collect::<Vec<_>>()
                .join(" + ")
        )
    }

    /// Reports a generic parameter inference could not determine. A `spec ...`
    /// entry looks like a written argument but binds nothing, so the position
    /// it occupies gets its own diagnostic rather than reading as a parameter
    /// the caller simply never mentioned.
    pub(crate) fn undetermined_generic_param(
        &mut self,
        node_id: HirId,
        span: Span,
        name: &Ident,
        params: &[HirGenericParam],
        written: &WrittenGenerics,
        parameter: Ident,
    ) {
        let hole = params
            .iter()
            .position(|param| param.ident == parameter)
            .is_some_and(|position| written.selectors.get(position).is_some_and(Option::is_some));
        let kind = if hole {
            AnalysisErrorKind::UndeterminedBoundSelector {
                name: name.clone(),
                parameter,
            }
        } else {
            AnalysisErrorKind::UnresolvedGenericParam(parameter)
        };
        self.error(node_id, span, kind);
    }
}
