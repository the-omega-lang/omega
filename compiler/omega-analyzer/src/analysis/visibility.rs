use super::*;

#[derive(Default)]
pub(super) struct RevealState {
    frames: Vec<RevealFrame>,
}

struct RevealFrame {
    origin: Origin,
    used: bool,
}

impl RevealState {
    pub fn active(&self) -> bool {
        !self.frames.is_empty()
    }

    pub fn begin(&mut self, origin: Origin) {
        self.frames.push(RevealFrame {
            origin,
            used: false,
        });
    }

    /// Whether some active `reveal` was written by the same macro expansion
    /// that produced `origin`. A caller-written `reveal` around a macro
    /// invocation never matches, so it cannot authorize what the macro
    /// definition itself was not allowed to expose.
    pub fn has_origin(&self, origin: Origin) -> bool {
        origin.0.is_some() && self.frames.iter().any(|frame| frame.origin == origin)
    }

    pub fn mark_used(&mut self) {
        for frame in &mut self.frames {
            frame.used = true;
        }
    }

    pub fn finish(&mut self) -> bool {
        self.frames
            .pop()
            .expect("finishing reveal analysis requires an active reveal")
            .used
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

    /// The origins of the `reveal` prefixes wrapping `expr`, outermost
    /// first, and the expression underneath all of them.
    pub(super) fn strip_reveal(expr: &HirExprNode) -> (Vec<Origin>, &HirExprNode) {
        match &expr.expr {
            HirExpr::Reveal(reveal) => {
                let (mut origins, inner) = Self::strip_reveal(&reveal.base);
                origins.insert(0, reveal.origin);
                (origins, inner)
            }
            _ => (Vec::new(), expr),
        }
    }

    pub(super) fn with_reveal_bypass<T>(
        &mut self,
        reveal_origins: &[Origin],
        node_id: HirId,
        span: Span,
        f: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        for origin in reveal_origins {
            self.reveals.begin(*origin);
        }
        let result = f(self);
        let mut used = true;
        for _ in reveal_origins {
            used &= self.reveals.finish();
        }
        if !reveal_origins.is_empty() && !used {
            self.warn(node_id, span, AnalysisWarningKind::UnnecessaryReveal);
        }
        result
    }

    pub(super) fn with_reveal_operand<T>(
        &mut self,
        operand: &HirExprNode,
        f: impl FnOnce(&mut Self, &HirExprNode) -> Option<T>,
    ) -> Option<T> {
        let (reveal_origins, inner) = Self::strip_reveal(operand);
        self.with_reveal_bypass(&reveal_origins, operand.id, operand.span, |this| {
            f(this, inner)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RevealState;
    use omega_parser::prelude::{ExpansionId, Origin};

    fn origin(id: u32) -> Origin {
        Origin(Some(ExpansionId(id)))
    }

    #[test]
    fn using_a_nested_reveal_marks_the_whole_chain_used() {
        let mut reveals = RevealState::default();
        reveals.begin(Origin::default());
        reveals.begin(Origin::default());
        reveals.mark_used();

        assert!(reveals.finish());
        assert!(reveals.finish());
    }

    #[test]
    fn only_the_exact_expansion_origin_of_an_active_frame_matches() {
        let mut reveals = RevealState::default();
        reveals.begin(origin(1));
        reveals.begin(origin(2));

        assert!(reveals.has_origin(origin(1)));
        assert!(reveals.has_origin(origin(2)));
        assert!(!reveals.has_origin(origin(3)));

        reveals.finish();
        assert!(!reveals.has_origin(origin(2)));
        assert!(reveals.has_origin(origin(1)));
    }

    #[test]
    fn a_source_written_reveal_never_matches_a_macro_dependency() {
        let mut reveals = RevealState::default();
        reveals.begin(Origin::default());

        assert!(reveals.active());
        assert!(!reveals.has_origin(Origin::default()));
    }
}
