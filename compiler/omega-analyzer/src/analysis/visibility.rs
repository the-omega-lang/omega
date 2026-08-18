use super::*;

impl<'r> Analyzer<'r> {
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

    pub(super) fn visibility_allows(visibility: Visibility, declaring_module: &[Ident], accessor_module: &[Ident]) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Internal => declaring_module.first() == accessor_module.first(),
            Visibility::Hidden => declaring_module == accessor_module,
        }
    }

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

    pub(super) fn strip_reveal(expr: &HirExprNode) -> (bool, &HirExprNode) {
        match &expr.expr {
            HirExpr::Reveal(inner) => (true, Self::strip_reveal(inner).1),
            _ => (false, expr),
        }
    }

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
