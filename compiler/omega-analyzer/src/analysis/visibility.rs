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
    pub fn begin(&mut self, origin: Origin) {
        self.frames.push(RevealFrame {
            origin,
            used: false,
        });
    }

    /// Whether an active `reveal` authorizes syntax carrying `origin`. A
    /// `reveal` only ever speaks for the environment it was written in, so a
    /// caller-written `reveal` around a macro invocation cannot authorize the
    /// macro's own definition-origin references, and a macro-written `reveal`
    /// cannot authorize the caller's substituted syntax.
    pub fn allows(&self, origin: Origin) -> bool {
        self.frames.iter().any(|frame| frame.origin == origin)
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
    /// The module whose rights a piece of syntax carrying `origin` is
    /// checked with: the macro or alias definition module that authored it,
    /// or the module currently being analyzed for ordinary source syntax.
    pub(crate) fn origin_module(&self, origin: Origin) -> Vec<Ident> {
        self.resolver
            .macro_origin_module(origin)
            .unwrap_or_else(|| self.module_path.clone())
    }

    /// Whether a `reveal` may bypass a failed check on syntax carrying
    /// `origin`, recording the bypass so an unused `reveal` still warns.
    pub(crate) fn revealed(&mut self, origin: Origin) -> bool {
        if self.reveals.allows(origin) {
            self.reveals.mark_used();
            return true;
        }
        false
    }

    pub(crate) fn check_visibility(
        &mut self,
        visibility: Visibility,
        declaring_module: &[Ident],
        origin: Origin,
    ) -> bool {
        if Self::visibility_allows(visibility, declaring_module, &self.origin_module(origin)) {
            return true;
        }
        self.revealed(origin)
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

    /// Member access uses the rights of the member token's own origin. The
    /// owner-only rule for a `hidden` member therefore does not lend the
    /// analyzed declaration's privilege to a member name a macro authored
    /// elsewhere; such a name is only ever authorized by a `reveal` the macro
    /// body wrote itself.
    pub(crate) fn check_member_visibility(
        &mut self,
        visibility: Visibility,
        declaring_module: &[Ident],
        owner_id: HirId,
        origin: Origin,
    ) -> bool {
        let allowed = match visibility {
            Visibility::Hidden => self.current_owner == Some(owner_id) && origin.0.is_none(),
            _ => Self::visibility_allows(visibility, declaring_module, &self.origin_module(origin)),
        };
        if allowed {
            return true;
        }
        self.revealed(origin)
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

        assert!(reveals.allows(origin(1)));
        assert!(reveals.allows(origin(2)));
        assert!(!reveals.allows(origin(3)));

        reveals.finish();
        assert!(!reveals.allows(origin(2)));
        assert!(reveals.allows(origin(1)));
    }

    #[test]
    fn a_source_written_reveal_authorizes_source_syntax_only() {
        let mut reveals = RevealState::default();
        reveals.begin(Origin::default());

        assert!(reveals.allows(Origin::default()));
        assert!(!reveals.allows(origin(1)));
    }
}
