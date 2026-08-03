//! Reducing a registry's full tag list to the handful worth putting in a
//! dropdown.
//!
//! Registries publish everything forever — mariadb lists 916 tags, postgres
//! 1385 — most of which are patch pins (`11.4.5`), OS variants
//! (`11.4.5-noble`), or pre-releases (`0.15.0.rc25`). What someone choosing "a
//! version" wants is the release line: the moving major (`12`) and its minors
//! (`12.3`). Anything with a third component or a non-digit is noise here, and
//! dropping it is what keeps the list honest without a hand-maintained table.

use std::cmp::Ordering;

/// How many release lines to offer. Enough to reach back a major or two
/// without turning the dropdown into the tag list it replaced.
const LIMIT: usize = 12;

/// `12.3` → `[12, 3]`; [`None`] for anything that is not one or two numeric
/// components.
pub fn version_sort_key(tag: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = tag.split('.').collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    parts
        .iter()
        .map(|p| {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                p.parse().ok()
            }
        })
        .collect()
}

/// Newest first. Where one version is a prefix of another the shorter — the
/// moving tag — leads: `12` sits above `12.3`, because picking `12` is a
/// choice to track the line rather than a lower version of it.
fn compare_desc(a: &[u64], b: &[u64]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        match y.cmp(x) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    a.len().cmp(&b.len())
}

/// Release lines from a raw tag list, newest first, capped at [`LIMIT`].
///
/// Empty means the repo publishes nothing version-shaped — mailpit ships only
/// `latest` — and the caller should offer no choice at all rather than invent
/// one.
pub fn select_versions(tags: &[String]) -> Vec<String> {
    let mut keyed: Vec<(Vec<u64>, &String)> =
        tags.iter().filter_map(|t| version_sort_key(t).map(|k| (k, t))).collect();
    keyed.sort_by(|(a, _), (b, _)| compare_desc(a, b));
    keyed.truncate(LIMIT);
    keyed.into_iter().map(|(_, t)| t.clone()).collect()
}

/// The list actually offered: `selected` plus any tag that must appear
/// whatever the registry says.
///
/// Two tags earn that guarantee. The tag a service *currently runs* — without
/// it the picker opens on a value absent from its own options and renders
/// blank. And the tag Mast's catalog *installs* (`redis:alpine`), which is
/// deliberately not version-shaped and so never survives [`select_versions`].
///
/// Version-shaped guarantees sort into place; the rest (`alpine`) follow, in
/// the order given.
pub fn offered_versions(selected: &[String], must_include: &[&str]) -> Vec<String> {
    let mut numeric: Vec<(Vec<u64>, String)> = Vec::new();
    let mut literal: Vec<String> = Vec::new();
    let push = |tag: &str, numeric: &mut Vec<(Vec<u64>, String)>, literal: &mut Vec<String>| {
        if numeric.iter().any(|(_, t)| t == tag) || literal.iter().any(|t| t == tag) {
            return;
        }
        match version_sort_key(tag) {
            Some(key) => numeric.push((key, tag.to_string())),
            None => literal.push(tag.to_string()),
        }
    };
    for tag in selected {
        push(tag, &mut numeric, &mut literal);
    }
    for tag in must_include.iter().filter(|t| !t.is_empty()) {
        push(tag, &mut numeric, &mut literal);
    }
    numeric.sort_by(|(a, _), (b, _)| compare_desc(a, b));
    numeric.into_iter().map(|(_, t)| t).chain(literal).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(raw: &str) -> Vec<String> {
        raw.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
    }

    #[test]
    fn parses_only_one_or_two_numeric_components() {
        assert_eq!(version_sort_key("12"), Some(vec![12]));
        assert_eq!(version_sort_key("11.4"), Some(vec![11, 4]));
        assert_eq!(version_sort_key("11.4.5"), None);
        assert_eq!(version_sort_key("11.4.5-noble"), None);
        assert_eq!(version_sort_key("alpine"), None);
        assert_eq!(version_sort_key("8-alpine"), None);
        assert_eq!(version_sort_key("0.15.0.rc25"), None);
        assert_eq!(version_sort_key(""), None);
        assert_eq!(version_sort_key("11."), None);
    }

    #[test]
    fn the_moving_major_leads_its_own_minors() {
        let tags = fixture("11.4\n12\n11\n12.3\n11.8");
        assert_eq!(select_versions(&tags), ["12", "12.3", "11", "11.8", "11.4"]);
    }

    /// The regression this whole module exists for: a project running
    /// `mariadb:11.4` was offered `12, 11, 10.11` by the old static table, so
    /// the picker rendered blank.
    #[test]
    fn mariadb_offers_the_lts_minors_the_static_table_missed() {
        let versions = select_versions(&fixture(include_str!("../tests/tags/mariadb.txt")));
        assert!(versions.contains(&"11.4".to_string()), "{versions:?}");
        assert_eq!(versions[0], "12");
        assert!(versions.len() <= LIMIT);
        assert!(!versions.iter().any(|v| v.contains('-')), "no OS variants: {versions:?}");
    }

    /// MySQL moved to calendar versioning; a hand-written list does not
    /// notice, a fetched one does.
    #[test]
    fn mysql_picks_up_the_calendar_versions() {
        let versions = select_versions(&fixture(include_str!("../tests/tags/mysql.txt")));
        assert_eq!(versions[0], "26", "{versions:?}");
    }

    #[test]
    fn every_offered_tag_is_a_plain_release_line() {
        for fixture_text in [
            include_str!("../tests/tags/mariadb.txt"),
            include_str!("../tests/tags/mysql.txt"),
            include_str!("../tests/tags/postgres.txt"),
            include_str!("../tests/tags/redis.txt"),
            include_str!("../tests/tags/typesense.txt"),
        ] {
            let all = fixture(fixture_text);
            let versions = select_versions(&all);
            assert!(!versions.is_empty());
            for v in &versions {
                assert!(version_sort_key(v).is_some(), "{v} is not a release line");
                assert!(all.contains(v), "{v} was not in the registry list");
            }
        }
    }

    /// A repo that publishes only `latest` must produce no dropdown, not a
    /// fabricated one.
    #[test]
    fn a_latest_only_repo_offers_nothing() {
        assert!(select_versions(&fixture("latest\nv1.27\nlatest-arm64")).is_empty());
    }

    #[test]
    fn the_running_tag_is_always_offered_even_when_the_registry_moved_on() {
        // 10.11 has aged out of the top of the list, but the service runs it.
        let selected = fixture("12\n12.3\n11\n11.8");
        let offered = offered_versions(&selected, &["10.11"]);
        assert_eq!(offered, ["12", "12.3", "11", "11.8", "10.11"]);
    }

    #[test]
    fn non_numeric_guarantees_follow_the_versions() {
        let offered = offered_versions(&fixture("8\n8.10\n7"), &["alpine"]);
        assert_eq!(offered, ["8", "8.10", "7", "alpine"]);
    }

    #[test]
    fn guarantees_never_duplicate_what_the_registry_already_offers() {
        let offered = offered_versions(&fixture("12\n11.4"), &["11.4", "11.4", "alpine", "alpine"]);
        assert_eq!(offered, ["12", "11.4", "alpine"]);
    }

    #[test]
    fn with_no_registry_data_the_guarantees_stand_alone() {
        assert_eq!(offered_versions(&[], &["11.4", "alpine"]), ["11.4", "alpine"]);
        assert!(offered_versions(&[], &[]).is_empty());
    }
}
