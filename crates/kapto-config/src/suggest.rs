// SPDX-License-Identifier: Apache-2.0

//! "Did you mean …?" suggestions for misspelt names.

/// The candidate closest to `input`, if it is close enough to be a likely
/// typo: at most two single-character edits, and fewer than half the length.
pub fn closest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(input, candidate), candidate))
        .filter(|(distance, candidate)| *distance <= 2 && *distance * 2 < candidate.len())
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// Edit distance counting insertions, deletions, substitutions and swaps
/// of two neighbouring characters (optimal string alignment). Swaps count as
/// one edit because they are the most common typo (`prot` for `port`).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // distance[i][j]: edits to turn the first i chars of a into the first j of b.
    let mut distance = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in distance.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in distance[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let substitution = distance[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut best = substitution
                .min(distance[i - 1][j] + 1)
                .min(distance[i][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(distance[i - 2][j - 2] + 1);
            }
            distance[i][j] = best;
        }
    }
    distance[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_counts_single_character_edits() {
        assert_eq!(edit_distance("port", "port"), 0);
        assert_eq!(edit_distance("prot", "port"), 1);
        assert_eq!(edit_distance("prt", "port"), 1);
        assert_eq!(edit_distance("ports", "port"), 1);
        assert_eq!(edit_distance("", "port"), 4);
        assert_eq!(edit_distance("abc", "ca"), 3);
    }

    #[test]
    fn suggests_the_closest_likely_typo() {
        assert_eq!(closest("prot", ["bind", "port"]), Some("port"));
        assert_eq!(
            closest("serer", ["server", "viewer", "paths"]),
            Some("server")
        );
    }

    #[test]
    fn suggests_nothing_for_unrelated_names() {
        assert_eq!(closest("colour", ["bind", "port"]), None);
        assert_eq!(closest("ab", ["bind", "port"]), None);
    }
}
