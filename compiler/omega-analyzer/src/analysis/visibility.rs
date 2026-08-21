use super::*;

#[derive(Default)]
pub(super) struct RevealState {
    frames: Vec<bool>,
}

impl RevealState {
    pub fn active(&self) -> bool {
        !self.frames.is_empty()
    }

    pub fn begin(&mut self) {
        self.frames.push(false);
    }

    pub fn mark_used(&mut self) {
        for frame in &mut self.frames {
            *frame = true;
        }
    }

    pub fn finish(&mut self) -> bool {
        self.frames
            .pop()
            .expect("finishing reveal analysis requires an active reveal")
    }
}

impl<'r> Analyzer<'r> {
    pub(crate) fn check_visibility(
        &mut self,
        visibility: Visibility,
        declaring_module: &[Ident],
    ) -> bool {
        if Self::visibility_allows(visibility, declaring_module, &self.module_path) {
            return true;
        }
        if self.reveals.active() {
            self.reveals.mark_used();
            return true;
        }
        false
    }

    pub(super) fn visibility_allows(
        visibility: Visibility,
        declaring_module: &[Ident],
        accessor_module: &[Ident],
    ) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Shared => declaring_module.first() == accessor_module.first(),
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
        if self.reveals.active() {
            self.reveals.mark_used();
            return true;
        }
        false
    }

    pub(super) fn strip_reveal(expr: &HirExprNode) -> (usize, &HirExprNode) {
        match &expr.expr {
            HirExpr::Reveal(inner) => {
                let (depth, inner) = Self::strip_reveal(inner);
                (depth + 1, inner)
            }
            _ => (0, expr),
        }
    }

    pub(super) fn with_reveal_bypass<T>(
        &mut self,
        reveal_depth: usize,
        node_id: HirId,
        span: Span,
        f: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        for _ in 0..reveal_depth {
            self.reveals.begin();
        }
        let result = f(self);
        let mut used = true;
        for _ in 0..reveal_depth {
            used &= self.reveals.finish();
        }
        if reveal_depth != 0 && !used {
            self.warn(node_id, span, AnalysisWarningKind::UnnecessaryReveal);
        }
        result
    }

    pub(super) fn with_reveal_operand<T>(
        &mut self,
        operand: &HirExprNode,
        f: impl FnOnce(&mut Self, &HirExprNode) -> Option<T>,
    ) -> Option<T> {
        let (reveal_depth, inner) = Self::strip_reveal(operand);
        self.with_reveal_bypass(reveal_depth, operand.id, operand.span, |this| {
            f(this, inner)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RevealState;

    #[test]
    fn using_a_nested_reveal_marks_the_whole_chain_used() {
        let mut reveals = RevealState::default();
        reveals.begin();
        reveals.begin();
        reveals.mark_used();

        assert!(reveals.finish());
        assert!(reveals.finish());
    }
}
