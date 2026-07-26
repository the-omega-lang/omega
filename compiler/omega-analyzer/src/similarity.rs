//! Name-similarity matching behind every "did you mean" suggestion --
//! shared by `Context`'s scope-level searches and by `ModuleResolver`
//! implementations (`omega-driver` suggests across a module's top-level
//! item index, which only it can see).

use omega_parser::prelude::Ident;

/// The candidate most similar to `target`, if its edit distance is small
/// enough relative to the name's length (a third of it, minimum 1 -- "one
/// or two typos, not a different word", the same intuition rustc's
/// suggestions follow). Ties go to the first-seen candidate; with hash-map
/// iteration order behind most call sites that's effectively arbitrary,
/// which is fine -- any close match is a useful suggestion.
pub fn best_match<'a>(target: &Ident, candidates: impl Iterator<Item = &'a Ident>) -> Option<Ident> {
    let target = target.as_ref();
    let max_distance = (target.chars().count() / 3).max(1);
    candidates
        .map(|candidate| (levenshtein(target, candidate.as_ref()), candidate))
        .filter(|&(distance, _)| distance > 0 && distance <= max_distance)
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, candidate)| candidate.clone())
}

/// Plain single-row Levenshtein -- names are short, and this only ever runs
/// on the error path, so the simplest correct implementation wins.
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
