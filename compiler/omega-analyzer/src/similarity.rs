
use omega_parser::prelude::Ident;

pub fn best_match<'a>(target: &Ident, candidates: impl Iterator<Item = &'a Ident>) -> Option<Ident> {
    let target = target.as_ref();
    let max_distance = (target.chars().count() / 3).max(1);
    candidates
        .map(|candidate| (levenshtein(target, candidate.as_ref()), candidate))
        .filter(|&(distance, _)| distance > 0 && distance <= max_distance)
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, candidate)| candidate.clone())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b_chars.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev_diag = row[0];
        row[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let substitution = prev_diag + usize::from(ca != cb);
            prev_diag = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b_chars.len()]
}
