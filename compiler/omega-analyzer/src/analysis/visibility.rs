use super::*;

impl<'r> Analyzer<'r> {
    /// The choke point every in-analyzer visibility check goes through --
    /// field access, field initializers, and method resolution. Cross-module
    /// item lookups instead go through `ModuleResolver::resolve_item`'s own
    /// `accessor_module_path`/`bypass` parameters, since that check happens
    /// across a trait boundary this `reveal_stack` can't reach directly.
    ///
    /// Returns whether the access is allowed -- if the ordinary rule would
    /// deny it but a `reveal` frame is active, this allows it anyway and
    /// marks the innermost frame as load-bearing.
    pub(crate) fn check_visibility(&mut self, visibility: Visibility, declaring_module: &[Ident]) -> bool {
        if Self::visibility_allows(visibility, declaring_module, &self.module_path) {
            return true;
        }
        if let Some(top) = self.reveal_stack.last_mut() {
            *top = true;
            return true;
        }
        false
    }

    /// The raw, `reveal`-blind visibility decision -- factored out for
    /// callers that must not consult the ambient `reveal_stack` (currently
    /// only `resolve_bare_overload_candidates`'s candidate-set filtering,
    /// where membership has to be a fixed, resolution-time fact).
    pub(super) fn visibility_allows(visibility: Visibility, declaring_module: &[Ident], accessor_module: &[Ident]) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Internal => declaring_module.first() == accessor_module.first(),
            Visibility::Hidden => declaring_module == accessor_module,
        }
    }

    /// The choke point every *member* (field, field initializer, method)
    /// visibility check goes through -- separate from `check_visibility`
    /// since a hidden member's scope is narrower than a hidden item's: "not
    /// accessible outside the struct definition," not merely outside its
    /// declaring module. `owner_id` is the declaring type's stable identity;
    /// `Hidden` is allowed only when `self.current_owner` (the type whose
    /// method bodies are currently being checked) is that exact type. A
    /// top-level function has no `current_owner`, so it can never touch a
    /// hidden field/method without `reveal`, even in its own module.
    /// `Exposed`/`Internal` fall back to the ordinary module-path rule.
    pub(crate) fn check_member_visibility(
        &mut self,
        visibility: Visibility,
        declaring_module: &[Ident],
        owner_id: HirId,
    ) -> bool {
        let allowed = match visibility {
            Visibility::Hidden => self.current_owner == Some(owner_id),
            _ => Self::visibility_allows(visibility, declaring_module, &self.module_path),
        };
        if allowed {
            return true;
        }
        if let Some(top) = self.reveal_stack.last_mut() {
            *top = true;
            return true;
        }
        false
    }

    /// Peels `HirExpr::Reveal` wrappers off `expr`, returning whether at
    /// least one was present and the first non-`Reveal` node reached. Needed
    /// before pattern-matching `HirExpr::Place`, since `reveal` is a generic
    /// wrapper node, not folded into `HirPlace` itself, so a `reveal`-wrapped
    /// place would otherwise look like "not a place at all".
    ///
    /// `reveal` here wraps only the sub-position (e.g. `reveal a.b = c;`
    /// parses as `Assignment { target: Reveal(FieldAccess(a, b)), .. }`), so
    /// `analyze_expr`'s own `HirExpr::Reveal` arm never fires for it and
    /// never pushes a bypass frame. Callers that go on to perform a
    /// visibility-gated lookup must wrap it in `with_reveal_bypass` using
    /// the bool returned here, or the bypass silently never activates.
    pub(super) fn strip_reveal(expr: &HirExprNode) -> (bool, &HirExprNode) {
        match &expr.expr {
            HirExpr::Reveal(inner) => (true, Self::strip_reveal(inner).1),
            _ => (false, expr),
        }
    }

    /// Runs `f` with a `reveal_stack` frame active iff `was_reveal` (the
    /// bool `strip_reveal` returned) -- the sub-position counterpart to
    /// `analyze_expr`'s own `HirExpr::Reveal` arm. Pops the frame and emits
    /// `UnnecessaryReveal` (anchored at the enclosing expression's own
    /// `node_id`/`span`, since the `Reveal` node has none of its own here)
    /// if the bypass never proved necessary.
    pub(super) fn with_reveal_bypass<T>(
        &mut self,
        was_reveal: bool,
        node_id: HirId,
        span: Span,
        f: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        if was_reveal {
            self.reveal_stack.push(false);
        }
        let result = f(self);
        if was_reveal {
            let load_bearing = self.reveal_stack.pop().expect("just pushed above");
            if !load_bearing {
                self.warn(node_id, span, AnalysisWarningKind::UnnecessaryReveal);
            }
        }
        result
    }
}
