//! Bounded did-you-mean search (ISSUES P10).
//!
//! A suggestion compares a misspelled name with every candidate. Done naively
//! that is quadratic in a hostile sketch with many undefined names and many
//! rules. Here each comparison is a banded edit distance that gives up as soon
//! as the distance passes the limit (`O(len × limit)`), candidates whose length
//! alone puts them out of reach are skipped in `O(1)`, and the whole forge
//! shares one work budget: once it is spent, problems are still reported, only
//! without a suggestion.

use alloc::{format, string::String, vec, vec::Vec};

/// The edit-distance cells one forge may spend on suggestions.
pub(crate) const BUDGET: u64 = 1 << 24;

/// Names longer than this get no suggestion (identifiers in sketches are at
/// most 64 bytes; anything longer is not a typo of one).
const MAX_LEN: usize = 128;

/// "did you mean `x`?" for the closest candidate within `limit` edits, or
/// `None`. Ties go to the earliest candidate. `budget` is the shared work
/// budget; it is decremented by the cells computed.
pub(crate) fn suggest_with<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
    limit: usize,
    budget: &mut u64,
) -> Option<String> {
    let a: Vec<char> = name.chars().collect();
    if a.len() > MAX_LEN {
        return None;
    }
    let mut best: Option<(usize, &str)> = None;
    let mut row = Vec::new();
    for candidate in candidates {
        if *budget == 0 {
            break;
        }
        let len = candidate.chars().count();
        if len.abs_diff(a.len()) > limit || len > MAX_LEN {
            continue;
        }
        let bound = best.map_or(limit, |(d, _)| d.saturating_sub(1).min(limit));
        if let Some(d) = banded(&a, candidate, bound, &mut row, budget) {
            if best.is_none_or(|(b, _)| d < b) {
                best = Some((d, candidate));
                if d == 0 {
                    break;
                }
            }
        }
    }
    best.map(|(_, c)| format!("did you mean `{c}`?"))
}

/// A suggestion with a private budget, for one-off checks.
pub(crate) fn suggest<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
    limit: usize,
) -> Option<String> {
    let mut budget = BUDGET;
    suggest_with(name, candidates, limit, &mut budget)
}

/// The edit distance between `a` and `b` if it is at most `limit`: only the
/// diagonal band of width `2 × limit + 1` is computed, and the scan stops as
/// soon as a whole row exceeds the limit.
fn banded(
    a: &[char],
    b: &str,
    limit: usize,
    row: &mut Vec<usize>,
    budget: &mut u64,
) -> Option<usize> {
    const FAR: usize = usize::MAX / 2;
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > limit {
        return None;
    }
    row.clear();
    row.extend(0..=m);
    let mut prev = core::mem::take(row);
    let mut cur = vec![FAR; m + 1];
    for i in 1..=n {
        let lo = i.saturating_sub(limit).max(1);
        let hi = (i + limit).min(m);
        cur[lo - 1] = if lo == 1 { i } else { FAR };
        let mut row_min = cur[lo - 1];
        for j in lo..=hi {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let value = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
            cur[j] = value;
            row_min = row_min.min(value);
        }
        if hi < m {
            cur[hi + 1] = FAR;
        }
        *budget = budget.saturating_sub((hi + 1 - lo) as u64);
        if row_min > limit {
            *row = prev;
            return None;
        }
        core::mem::swap(&mut prev, &mut cur);
    }
    let d = prev[m];
    *row = prev;
    (d <= limit).then_some(d)
}

/// The plain edit distance, for tests.
#[cfg(test)]
fn edit_distance(a: &str, b: &str) -> usize {
    let mut budget = u64::MAX;
    let a: Vec<char> = a.chars().collect();
    banded(&a, b, usize::MAX / 4, &mut Vec::new(), &mut budget).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_distance() {
        assert_eq!(edit_distance("expr", "expr"), 0);
        assert_eq!(edit_distance("exprr", "expr"), 1);
        assert_eq!(edit_distance("stmt", "smt"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn test_suggest_picks_closest_within_limit() {
        let names = ["expr", "stmt", "program"];
        assert_eq!(
            suggest("exprr", names.iter().copied(), 1).as_deref(),
            Some("did you mean `expr`?")
        );
        assert_eq!(suggest("zzz", names.iter().copied(), 1), None);
        // Ties go to the first candidate.
        assert_eq!(
            suggest("ab", ["ax", "ay"].iter().copied(), 1).as_deref(),
            Some("did you mean `ax`?")
        );
        assert_eq!(
            suggest("idents", ["identifiers", "newlines"].iter().copied(), 2),
            None
        );
    }

    #[test]
    fn test_suggest_budget_bounds_work() {
        let names: Vec<String> = (0..200_000).map(|i| format!("rule{i:06}")).collect();
        let mut budget = 10_000;
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = suggest_with(
                "rulex00000",
                names.iter().map(String::as_str),
                3,
                &mut budget,
            );
        }
        assert_eq!(budget, 0);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
