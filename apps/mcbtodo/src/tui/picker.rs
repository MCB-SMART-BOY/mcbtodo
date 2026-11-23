use anyhow::Result;
use mcbtodo_domain::Id;
use mcbtodo_vault::Vault;

use super::daemon_views::{DaemonViews, outline_rows};

pub(super) struct Match {
    pub(super) id: Id,
    pub(super) title: String,
    pub(super) depth: usize,
}

pub(super) fn matches(
    vault: &Vault,
    service: Option<&DaemonViews>,
    query: &str,
) -> Result<Vec<Match>> {
    let query_lower = query.to_lowercase();
    let mut matches: Vec<_> = outline_rows(vault, service)?
        .iter()
        .filter_map(|row| {
            let score = score(query, &row.title).or_else(|| {
                row.source_path
                    .to_lowercase()
                    .contains(&query_lower)
                    .then_some(usize::MAX / 2)
            })?;
            Some((
                score,
                Match {
                    id: row.id,
                    title: row.title.clone(),
                    depth: row.depth,
                },
            ))
        })
        .collect();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        left_score
            .cmp(right_score)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(matches.into_iter().map(|(_, row)| row).collect())
}

fn score(query: &str, text: &str) -> Option<usize> {
    let query = query
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    if query.is_empty() {
        return Some(0);
    }
    let text = text
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    let mut cursor = 0;
    let mut first = None;
    let mut previous = None;
    let mut gaps = 0;
    for wanted in query {
        let found = text[cursor..].iter().position(|&actual| actual == wanted)? + cursor;
        if let Some(previous) = previous {
            gaps += found - previous - 1;
        }
        first.get_or_insert(found);
        previous = Some(found);
        cursor = found + 1;
    }
    Some(first.unwrap_or_default() * 8 + gaps)
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn fuzzy_score_requires_a_subsequence_and_prefers_tighter_prefix_matches() {
        assert!(score("rsv", "Read Sv39").is_some());
        assert!(score("rsv", "Read Sv39").unwrap() < score("rsv", "Rust OS Sv39").unwrap());
        assert!(score("xyz", "Read Sv39").is_none());
    }
}
