/// A byte range into one source file -- deliberately *not* tagged with
/// which file: the driver already threads a module's file identity
/// alongside every span it touches (see `omega_driver`).
///
/// Composite spans (covering more than one token) are built as `(min of
/// every constituent token's start, max of every constituent token's end)`
/// -- callers must not assume a `Span` is always one contiguous region,
/// only that `start <= end`.
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

    /// The smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}
