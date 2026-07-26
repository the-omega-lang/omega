use super::*;

impl<'r> Analyzer<'r> {
    /// The single choke point every *in-analyzer* visibility check goes
    /// through -- field access (`resolve_field_projection`), struct/enum-
    /// variant/union literal field initializers (`check_field_initializers`
    /// and the union-literal inline check), and method resolution
    /// (`resolve_callee`, static member access). Cross-module *item*
    /// lookups go through `ModuleResolver::resolve_item`'s own
    /// `accessor_module_path`/`bypass` parameters instead (see
    /// `omega_driver::Driver::ensure_item`) -- that check happens inside the
    /// resolver, across a trait boundary this `hidden_stack` can't reach
    /// directly, so it threads the same bypass decision (`!self.hidden_stack.
    /// is_empty()`) down as an explicit argument and separately reports back
    /// via `ModuleResolver::is_item_visible` whether the bypass was load-
    /// bearing, for the identical `UnnecessaryHidden` warning.
    ///
    /// `declaring_module` is `visibility`'s own declaring module; `self.
    /// module_path` is always the *accessing* site's module, stable for
    /// this whole `Analyzer` instance's lifetime (one throwaway `Analyzer`
    /// checks exactly one top-level item). Returns whether the access is
    /// allowed -- if the ordinary rule would deny it but a `hidden` frame is
    /// active, this allows it anyway and marks the innermost frame as
    /// load-bearing.
    pub(crate) fn check_visibility(&mut self, visibility: Visibility, declaring_module: &[Ident]) -> bool {
        if Self::visibility_allows(visibility, declaring_module, &self.module_path) {
            return true;
        }
        if let Some(top) = self.hidden_stack.last_mut() {
            *top = true;
            return true;
        }
        false
    }

    /// The raw, `hidden`-blind visibility decision -- `check_visibility`'s
    /// own "allowed" computation, factored out so a caller that must *not*
    /// consult the ambient `hidden_stack` at all (currently only
    /// `resolve_bare_overload_candidates`'s candidate-set filtering, where
    /// membership in the set has to be a fixed, resolution-time fact, not
    /// something a call-site `hidden` can expand) can reuse the exact same
    /// rule without its bypass fallback.
    pub(super) fn visibility_allows(visibility: Visibility, declaring_module: &[Ident], accessor_module: &[Ident]) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Internal => declaring_module.first() == accessor_module.first(),
            Visibility::Private => declaring_module == accessor_module,
        }
    }

    /// The choke point every *member* (struct/union/enum-header/enum-
    /// dynamic/enum-variant field; a struct/enum-variant/union literal's
    /// own field initializers; and a struct/union/enum's own instance/
    /// static methods, both plain and overloaded) visibility check goes
    /// through -- deliberately separate from `check_visibility`, since a
    /// private *member*'s scope is narrower than a private *item*'s: "a
    /// private field/method cannot be accessed outside of the struct
    /// definition," not merely outside its declaring module. `owner_id` is
    /// the declaring struct/union/enum's own stable identity
    /// (`ResolvedStructType::id` and friends); `Private` is allowed only
    /// when `self.current_owner` (the type whose own method bodies -- or,
    /// for a spec-satisfying default method, whose own *identity* -- are
    /// currently being checked) is that exact same type. A top-level
    /// function has no `current_owner` at all (`None`), so it can never
    /// touch a private field or call a private method of any type,
    /// including one declared in its own module, without `hidden` --
    /// unlike `Exposed`/`Internal`, which behave identically to an
    /// ordinary item (module-path-based, via the same `visibility_allows`)
    /// regardless of whether the accessor happens to be a method of the
    /// same type or not.
    pub(crate) fn check_member_visibility(
        &mut self,
        visibility: Visibility,
        declaring_module: &[Ident],
        owner_id: HirId,
    ) -> bool {
        let allowed = match visibility {
            Visibility::Private => self.current_owner == Some(owner_id),
            _ => Self::visibility_allows(visibility, declaring_module, &self.module_path),
        };
        if allowed {
            return true;
        }
        if let Some(top) = self.hidden_stack.last_mut() {
            *top = true;
            return true;
        }
        false
    }

    /// Peels `HirExpr::Hidden` wrappers off `expr`, returning whether at
    /// least one was present and the first non-`Hidden` node reached -- a
    /// no-op (`(false, expr)`) when there's no `Hidden` wrapper at all, the
    /// overwhelmingly common case. Every raw-HIR "is this syntactically a
    /// place" check (an assignment/compound-assign target, `++`/`--`'s
    /// operand, `&`'s operand, a call callee, ...) needs this before
    /// pattern-matching `HirExpr::Place`, since `hidden` is a real, generic
    /// wrapper node -- never folded into `HirPlace` itself (see
    /// `HirExpr::Hidden`'s doc comment) -- so a `hidden`-wrapped place would
    /// otherwise look like "not a place at all" to these checks.
    ///
    /// Critically, `hidden` here wraps only the *sub-position*
    /// (`assignment.target`, `addr.base`, ...), not the enclosing expression
    /// (`hidden a.b = c;` parses as `Assignment { target: Hidden(FieldAccess(a,
    /// b)), value: c }` -- `parse_assignment`'s own target is parsed via the
    /// same precedence descent that bottoms out at `parse_unary`, where
    /// `hidden` is recognized, so it binds to `a.b` alone, never spanning
    /// the `=` and beyond). That means `analyze_expr`'s own `HirExpr::Hidden`
    /// arm -- which only ever sees a `Hidden` node when it's the *outermost*
    /// shape of whatever's being analyzed -- never runs for this position at
    /// all, so it never pushes a bypass frame here. Every caller of this
    /// function that goes on to perform a visibility-gated lookup (as
    /// opposed to a purely structural probe like `narrowable_scrutinee`/
    /// `resolve_variant_pattern`, which call this only to recognize a place
    /// shape, never to gate anything) must wrap that lookup in
    /// `with_hidden_bypass(was_hidden, ...)` using the `bool` returned here,
    /// or the bypass silently never activates.
    pub(super) fn strip_hidden(expr: &HirExprNode) -> (bool, &HirExprNode) {
        match &expr.expr {
            HirExpr::Hidden(inner) => (true, Self::strip_hidden(inner).1),
            _ => (false, expr),
        }
    }

    /// Runs `f` with a `hidden_stack` frame active iff `was_hidden` (the
    /// bool `strip_hidden` returned) -- the sub-position counterpart to
    /// `analyze_expr`'s own `HirExpr::Hidden` arm (see `strip_hidden`'s doc
    /// comment for why that arm doesn't fire on its own here). Pops the
    /// frame and emits `UnnecessaryHidden` (anchored at `node_id`/`span`,
    /// the *enclosing* expression's own -- there is no narrower span to
    /// anchor at, since the `Hidden` node itself was never assigned one of
    /// its own by this call path) if the bypass never proved necessary,
    /// regardless of how `f` returns (including every `?`-early-return
    /// inside it) -- a plain closure boundary, not a `Drop` guard, precisely
    /// so early returns inside `f` still run this function's own
    /// after-`f` cleanup correctly.
    pub(super) fn with_hidden_bypass<T>(
        &mut self,
        was_hidden: bool,
        node_id: HirId,
        span: Span,
        f: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        if was_hidden {
            self.hidden_stack.push(false);
        }
        let result = f(self);
        if was_hidden {
            let load_bearing = self.hidden_stack.pop().expect("just pushed above");
            if !load_bearing {
                self.warn(node_id, span, AnalysisWarningKind::UnnecessaryHidden);
            }
        }
        result
    }
}
