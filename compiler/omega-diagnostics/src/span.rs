#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        assert!(start <= end, "Span::new: start ({start}) > end ({end})");
        Self { start, end }
    }

    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "Span::new")]
    fn rejects_reversed_ranges_in_all_build_modes() {
        Span::new(2, 1);
    }
}
