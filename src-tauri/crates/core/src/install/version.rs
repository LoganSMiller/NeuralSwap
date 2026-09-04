//! Comparing runtime versions.
//!
//! Version strings arrive from three places that do not agree on a format: a
//! PE `VS_FIXEDFILEINFO` block gives four numbers, a PE string table often
//! gives `"310, 8, 0, 0"` with spaces, and a package's own name might carry
//! `3.1.13`. All three have to order correctly against each other, because the
//! answer decides whether an install is an upgrade or a downgrade - and a
//! silent downgrade is the change a user is least likely to expect and most
//! likely to blame on us.
//!
//! Deliberately not semver: these are not semver, and pretending otherwise
//! would mean rejecting `310.8.0.0` as invalid.

use serde::{Deserialize, Serialize};

/// Numeric components, most significant first.
pub type Version = Vec<u64>;

/// Parse whatever a PE or a package name offered. `None` when there is no
/// usable number in the string, which is a normal outcome rather than an
/// error - plenty of DLLs carry no version resource at all.
pub fn parse_version(raw: Option<&str>) -> Option<Version> {
    let raw = raw?;
    let mut parts: Version = Vec::new();
    for piece in raw.split(['.', ',', ' ', '\t', '_', '-']) {
        if piece.is_empty() {
            continue;
        }
        // A component that is not a plain number makes the whole string
        // unusable rather than partly usable: "310.8.beta" must not silently
        // compare as 310.8.
        parts.push(piece.parse::<u64>().ok()?);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

/// Component-wise ordering, short versions padded with zeroes so that `310.8`
/// and `310.8.0.0` compare equal rather than the shorter one sorting first.
pub fn compare_versions(left: &[u64], right: &[u64]) -> std::cmp::Ordering {
    let width = left.len().max(right.len());
    for index in 0..width {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        let order = a.cmp(&b);
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
    std::cmp::Ordering::Equal
}

/// Canonical dotted form, for display.
pub fn format_version(version: &[u64]) -> String {
    version
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// How a package version relates to what is already on disk.
///
/// `Unknown` when either side has no parsable version. The caller must treat
/// that as "cannot claim this is an upgrade" rather than as equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VersionRelation {
    Newer,
    Older,
    Same,
    Unknown,
}

pub fn relate(package_version: Option<&str>, present_version: Option<&str>) -> VersionRelation {
    let (Some(a), Some(b)) = (
        parse_version(package_version),
        parse_version(present_version),
    ) else {
        return VersionRelation::Unknown;
    };
    match compare_versions(&a, &b) {
        std::cmp::Ordering::Greater => VersionRelation::Newer,
        std::cmp::Ordering::Less => VersionRelation::Older,
        std::cmp::Ordering::Equal => VersionRelation::Same,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_format_a_pe_offers() {
        assert_eq!(
            parse_version(Some("310, 8, 0, 0")),
            Some(vec![310, 8, 0, 0])
        );
        assert_eq!(parse_version(Some("310.8.0.0")), Some(vec![310, 8, 0, 0]));
        assert_eq!(parse_version(Some("3.1.13")), Some(vec![3, 1, 13]));
        assert_eq!(parse_version(None), None);
        assert_eq!(parse_version(Some("")), None);
        assert_eq!(parse_version(Some("not a version")), None);
        // Partly numeric is not partly usable.
        assert_eq!(parse_version(Some("310.8.beta")), None);
    }

    #[test]
    fn short_versions_are_padded_not_sorted_first() {
        use std::cmp::Ordering;
        assert_eq!(
            compare_versions(&[310, 8], &[310, 8, 0, 0]),
            Ordering::Equal
        );
        assert_eq!(
            compare_versions(&[310, 8, 1], &[310, 8, 0, 0]),
            Ordering::Greater
        );
        // Numeric, not lexical: 310 is not less than 9.
        assert_eq!(compare_versions(&[310, 0], &[9, 0]), Ordering::Greater);
    }

    #[test]
    fn an_unknown_version_is_never_equal() {
        assert_eq!(
            relate(Some("310.8.0.0"), Some("310.1.0.0")),
            VersionRelation::Newer
        );
        assert_eq!(
            relate(Some("310.1.0.0"), Some("310.8.0.0")),
            VersionRelation::Older
        );
        assert_eq!(
            relate(Some("310.8.0.0"), Some("310.8")),
            VersionRelation::Same
        );
        assert_eq!(relate(Some("310.8.0.0"), None), VersionRelation::Unknown);
        assert_eq!(relate(None, None), VersionRelation::Unknown);
    }

    #[test]
    fn formats_back_to_a_dotted_string() {
        assert_eq!(format_version(&[310, 8, 0, 0]), "310.8.0.0");
    }
}
