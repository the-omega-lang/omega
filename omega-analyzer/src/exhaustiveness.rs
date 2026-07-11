//! Interval-coverage checking for a numeric/`bool` `match` -- whether a set
//! of arm patterns (each reduced to a closed `i128` interval by
//! `Analyzer::analyze_match`) covers a scrutinee type's whole domain with no
//! gaps and no overlaps, so that a fully-covering match needs no `else`
//! (matching Rust's own interval-exhaustiveness checking) and an
//! overlapping one is a compile error rather than silent first-match-wins
//! (see `AnalysisErrorKind::OverlappingMatchArm`/`NonExhaustiveMatchValue`).
//! Pure interval math -- no notion of `ResolvedType`/`Ident` here at all;
//! the analyzer supplies the domain and formats the results.

use omega_parser::prelude::Span;

/// One arm's pattern, reduced to the closed integer interval it covers.
#[derive(Debug, Clone, Copy)]
pub struct Interval {
    pub lo: i128,
    pub hi: i128,
    pub span: Span,
}

pub struct CoverageReport {
    /// `(covering, redundant)` -- `covering` is whichever earlier-processed
    /// interval already reaches at least as far as `redundant`'s start;
    /// with overlaps disallowed (see this module's doc comment), all that
    /// matters for the diagnostic is naming *some* interval that already
    /// covers the value, not enumerating every interval that does.
    pub overlaps: Vec<(Interval, Interval)>,
    /// Each inclusive `[lo, hi]` sub-range of the domain left uncovered.
    pub gaps: Vec<(i128, i128)>,
}

/// Standard sweep-line interval algorithm: sort by `lo`, then walk once
/// tracking `cursor` (one past the highest value covered so far) and
/// `covering` (whichever processed interval achieved that reach) --
/// sufficient to detect every overlap (including one interval nested
/// entirely inside an earlier, wider one) and every gap in a single pass,
/// without an O(n^2) all-pairs scan.
pub fn check(domain: (i128, i128), mut intervals: Vec<Interval>) -> CoverageReport {
    intervals.sort_by_key(|iv| iv.lo);

    let mut overlaps = Vec::new();
    let mut gaps = Vec::new();
    let mut cursor = domain.0;
    let mut covering: Option<Interval> = None;

    for interval in intervals {
        if interval.lo < cursor {
            let prev = covering.expect("cursor only advances past domain.0 once an interval has set `covering`");
            overlaps.push((prev, interval));
        } else if interval.lo > cursor {
            gaps.push((cursor, interval.lo - 1));
        }

        if interval.hi + 1 > cursor {
            cursor = interval.hi + 1;
            covering = Some(interval);
        }
    }

    if cursor <= domain.1 {
        gaps.push((cursor, domain.1));
    }

    CoverageReport { overlaps, gaps }
}
