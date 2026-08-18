//! Interval-coverage checking for a numeric/`bool` `match` -- whether arm
//! patterns (each reduced to a closed `i128` interval by
//! `Analyzer::analyze_match`) cover a scrutinee type's domain with no gaps
//! and no overlaps (see `AnalysisErrorKind::OverlappingMatchArm`/
//! `NonExhaustiveMatchValue`). Pure interval math; the analyzer supplies the
//! domain and formats the results.

use omega_parser::prelude::Span;

/// One arm's pattern, reduced to the closed integer interval it covers.
#[derive(Debug, Clone, Copy)]
pub struct Interval {
    pub lo: i128,
    pub hi: i128,
    pub span: Span,
}

pub struct CoverageReport {
    /// `(covering, redundant)` -- `covering` is the earlier interval that
    /// already reaches `redundant`'s start; only one covering interval is
    /// named per overlap, not every one that applies.
    pub overlaps: Vec<(Interval, Interval)>,
    /// Each inclusive `[lo, hi]` sub-range of the domain left uncovered.
    pub gaps: Vec<(i128, i128)>,
}

/// Sweep-line: sort by `lo`, then walk once tracking `cursor` (one past the
/// highest value covered so far) and `covering` (the interval that achieved
/// that reach) -- detects every overlap and gap in a single pass.
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
