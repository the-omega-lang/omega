/// A byte range into one source file -- deliberately *not* tagged with
/// which file: the driver already threads a module's file identity
/// alongside every span it touches (see `omega_driver`), so embedding file
/// identity here would ripple through every `Span` field in
/// `omega-hir`/`omega-analyzer` for no benefit -- nothing there ever
/// compares spans *across* files.
///
/// Composite spans (covering more than one token, e.g. a whole
/// `BinaryOpExpr`) are built as `(min of every constituent token's start,
/// max of every constituent token's end)`, not "first token's start, last
/// token's end" -- see `omega_parser::macros`, where a node built from
/// tokens spliced in from two different source locations (a macro's
/// definition site and its invocation site) could otherwise produce a
/// non-contiguous or even inverted (`start > end`) span. `min`/`max`
/// construction is always well-formed regardless of where the constituent
/// tokens originated, even though it may not describe a single contiguous
/// range in that case -- callers must not assume a `Span` is always one
/// contiguous highlighted region, only that `start <= end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "Span::new: start ({start}) > end ({end})");
        Self { start, end }
    }

    /// The smallest span covering both `self` and `other` -- the `min`
    /// start/`max` end construction described above.
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}
