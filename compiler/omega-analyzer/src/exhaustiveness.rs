use omega_parser::prelude::Span;

#[derive(Debug, Clone, Copy)]
pub struct Interval {
    pub lo: i128,
    pub hi: i128,
    pub span: Span,
}

pub struct CoverageReport {
    pub overlaps: Vec<(Interval, Interval)>,
    pub gaps: Vec<(i128, i128)>,
}

pub fn check(domain: (i128, i128), mut intervals: Vec<Interval>) -> CoverageReport {
    intervals.sort_by_key(|iv| iv.lo);

    let mut overlaps = Vec::new();
    let mut gaps = Vec::new();
    let mut cursor = domain.0;
    let mut covering: Option<Interval> = None;

    for interval in intervals {
        if interval.lo < cursor {
            let prev = covering
                .expect("cursor only advances past domain.0 once an interval has set `covering`");
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
