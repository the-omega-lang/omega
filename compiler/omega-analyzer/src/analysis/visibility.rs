use super::*;

#[derive(Default)]
pub(super) struct RevealState {
    frames: Vec<RevealFrame>,
}

struct RevealFrame {
    id: HirId,
    origin: Origin,
    used: bool,
}

impl RevealState {
    pub fn begin(&mut self, id: HirId, origin: Origin) {
        self.frames.push(RevealFrame {
            id,
            origin,
            used: false,
        });
    }

    /// Whether the `reveal` written as `id` is already active, because the
    /// position consuming its place activated it before resolving the place.
    fn is_active(&self, id: HirId) -> bool {
        self.frames.iter().any(|frame| frame.id == id)
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

impl crate::context::RevealAuthority for RevealState {
    fn allows(&self, origin: Origin) -> bool {
        RevealState::allows(self, origin)
    }

    fn mark_used(&mut self) {
        RevealState::mark_used(self)
    }
}

impl<'r> Analyzer<'r> {
    /// The module whose rights a piece of syntax carrying `origin` is
    /// checked with: the macro or alias definition module that authored it,
    /// or the module currently being analyzed for ordinary source syntax.
    pub(crate) fn origin_module(&self, origin: Origin) -> ModulePath {
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
        declaring_module: &ModulePath,
        origin: Origin,
    ) -> bool {
        if Self::visibility_allows(visibility, declaring_module, &self.origin_module(origin)) {
            return true;
        }
        self.revealed(origin)
    }

    pub(super) fn visibility_allows(
        visibility: Visibility,
        declaring_module: &ModulePath,
        accessor_module: &ModulePath,
    ) -> bool {
        match visibility {
            Visibility::Exposed => true,
            Visibility::Shared => declaring_module.root() == accessor_module.root(),
            Visibility::Hidden => declaring_module == accessor_module,
        }
    }

    /// The `reveal` prefixes folded into `expr` when it is a place.
    pub(super) fn place_reveals(expr: &HirExprNode) -> &[HirPlaceReveal] {
        match &expr.expr {
            HirExpr::Place(place) => &place.reveals,
            _ => &[],
        }
    }

    /// Runs `f` with `reveals` active, warning for each one nothing used. A
    /// prefix the consuming position already activated is left to it, so a
    /// place reached twice reports its `reveal` once.
    pub(super) fn within_reveals<T>(
        &mut self,
        reveals: &[HirPlaceReveal],
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let entered: Vec<HirPlaceReveal> = reveals
            .iter()
            .filter(|reveal| !self.reveals.is_active(reveal.id))
            .copied()
            .collect();
        for reveal in &entered {
            self.reveals.begin(reveal.id, reveal.origin);
        }
        let result = f(self);
        for reveal in entered.iter().rev() {
            if !self.reveals.finish() {
                self.warn(
                    reveal.id,
                    reveal.span,
                    AnalysisWarningKind::UnnecessaryReveal,
                );
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::RevealState;
    use omega_hir::{HirId, ModuleId};
    use omega_parser::prelude::{ExpansionId, Origin};

    fn id(local: u32) -> HirId {
        HirId {
            module: ModuleId(0),
            local,
        }
    }

    fn origin(id: u32) -> Origin {
        Origin(Some(ExpansionId(id)))
    }

    #[test]
    fn using_a_nested_reveal_marks_the_whole_chain_used() {
        let mut reveals = RevealState::default();
        reveals.begin(id(1), Origin::default());
        reveals.begin(id(2), Origin::default());
        reveals.mark_used();

        assert!(reveals.finish());
        assert!(reveals.finish());
    }

    #[test]
    fn only_the_exact_expansion_origin_of_an_active_frame_matches() {
        let mut reveals = RevealState::default();
        reveals.begin(id(1), origin(1));
        reveals.begin(id(2), origin(2));

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
        reveals.begin(id(1), Origin::default());

        assert!(reveals.allows(Origin::default()));
        assert!(!reveals.allows(origin(1)));
    }
}
