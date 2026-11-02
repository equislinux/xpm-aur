//! Arch Linux version handling.
//!
//! The comparison algorithm mirrors `alpm_pkg_vercmp` / `vercmp(8)`:
//! compare epochs first, then `pkgver` using alternating numeric and alpha
//! segments, and finally `pkgrel`. Numeric segments compare numerically,
//! alpha segments lexicographically, and a numeric segment is newer than an
//! alpha segment. Separators are ignored.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

/// An Arch Linux epoch, version and optional pkgrel.
///
/// Examples: `1.2.3-1`, `2:1.0-4`, `1.0`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Evr {
    pub epoch: u64,
    pub pkgver: String,
    pub pkgrel: Option<String>,
}

impl Evr {
    /// Parse `[epoch:]pkgver[-pkgrel]`.
    pub fn parse(input: &str) -> CoreResult<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(CoreError::InvalidVersion("empty version".into()));
        }

        let (epoch, rest) = match input.split_once(':') {
            Some((epoch, rest)) => {
                if epoch.is_empty() || !epoch.chars().all(|c| c.is_ascii_digit()) {
                    return Err(CoreError::InvalidVersion(input.into()));
                }
                let epoch = epoch
                    .parse::<u64>()
                    .map_err(|_| CoreError::InvalidVersion(input.into()))?;
                (epoch, rest)
            }
            None => (0, input),
        };

        if rest.is_empty() {
            return Err(CoreError::InvalidVersion(input.into()));
        }

        // pkgver cannot contain '-' in Arch, so the last '-' separates pkgrel.
        let (pkgver, pkgrel) = match rest.rsplit_once('-') {
            Some((pkgver, pkgrel)) if !pkgver.is_empty() && !pkgrel.is_empty() => {
                (pkgver.to_string(), Some(pkgrel.to_string()))
            }
            _ => (rest.to_string(), None),
        };

        Ok(Self {
            epoch,
            pkgver,
            pkgrel,
        })
    }

    /// Compare two versions using pacman's ordering.
    pub fn cmp_version(&self, other: &Self) -> Ordering {
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| rpmvercmp(&self.pkgver, &other.pkgver))
            .then_with(|| match (&self.pkgrel, &other.pkgrel) {
                (Some(a), Some(b)) => rpmvercmp(a, b),
                // A version without pkgrel is treated as the lowest pkgrel.
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
    }

    /// True when this version satisfies a version constraint.
    pub fn matches(&self, constraint: &VersionConstraint) -> bool {
        let ordering = self.cmp_version(&constraint.version);
        match constraint.operator {
            VersionOperator::Equal => ordering == Ordering::Equal,
            VersionOperator::Greater => ordering == Ordering::Greater,
            VersionOperator::GreaterEqual => ordering != Ordering::Less,
            VersionOperator::Less => ordering == Ordering::Less,
            VersionOperator::LessEqual => ordering != Ordering::Greater,
        }
    }
}

impl Ord for Evr {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_version(other)
    }
}

impl PartialOrd for Evr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Evr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.epoch != 0 {
            write!(f, "{}:", self.epoch)?;
        }
        write!(f, "{}", self.pkgver)?;
        if let Some(pkgrel) = &self.pkgrel {
            write!(f, "-{pkgrel}")?;
        }
        Ok(())
    }
}

/// Compare two version strings. Invalid input falls back to a stable string
/// comparison so callers that only need a best effort never panic.
pub fn vercmp(a: &str, b: &str) -> Ordering {
    match (Evr::parse(a), Evr::parse(b)) {
        (Ok(a), Ok(b)) => a.cmp_version(&b),
        _ => a.cmp(b),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum VersionOperator {
    Equal,
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
}

impl fmt::Display for VersionOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self {
            Self::Equal => "=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::Less => "<",
            Self::LessEqual => "<=",
        };
        f.write_str(op)
    }
}

/// An `op version` suffix on a dependency or provides entry.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct VersionConstraint {
    pub operator: VersionOperator,
    pub version: Evr,
}

impl VersionConstraint {
    /// Parse the suffix of e.g. `foo>=1.2-3`.
    pub fn parse(input: &str) -> CoreResult<Option<Self>> {
        let input = input.trim();
        if input.is_empty() {
            return Ok(None);
        }

        let (operator, rest) = if let Some(rest) = input.strip_prefix(">=") {
            (VersionOperator::GreaterEqual, rest)
        } else if let Some(rest) = input.strip_prefix("<=") {
            (VersionOperator::LessEqual, rest)
        } else if let Some(rest) = input.strip_prefix('=') {
            (VersionOperator::Equal, rest)
        } else if let Some(rest) = input.strip_prefix('>') {
            (VersionOperator::Greater, rest)
        } else if let Some(rest) = input.strip_prefix('<') {
            (VersionOperator::Less, rest)
        } else {
            return Ok(None);
        };

        if rest.is_empty() {
            return Err(CoreError::InvalidVersion(input.into()));
        }

        Ok(Some(Self {
            operator,
            version: Evr::parse(rest)?,
        }))
    }
}

impl fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.operator, self.version)
    }
}

/// pacman's `rpmvercmp` segment comparison.
fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }

    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut ia = 0;
    let mut ib = 0;

    while ia < a.len() || ib < b.len() {
        while ia < a.len() && !a[ia].is_ascii_alphanumeric() {
            ia += 1;
        }
        while ib < b.len() && !b[ib].is_ascii_alphanumeric() {
            ib += 1;
        }

        if ia >= a.len() || ib >= b.len() {
            break;
        }

        let a_digit = a[ia].is_ascii_digit();
        let b_digit = b[ib].is_ascii_digit();

        if a_digit && b_digit {
            let a_start = ia;
            let b_start = ib;

            while ia < a.len() && a[ia].is_ascii_digit() {
                ia += 1;
            }
            while ib < b.len() && b[ib].is_ascii_digit() {
                ib += 1;
            }

            let a_num = strip_leading_zeroes(&a[a_start..ia]);
            let b_num = strip_leading_zeroes(&b[b_start..ib]);

            match a_num.len().cmp(&b_num.len()) {
                Ordering::Equal => match a_num.cmp(b_num) {
                    Ordering::Equal => {}
                    ordering => return ordering,
                },
                ordering => return ordering,
            }
        } else if !a_digit && !b_digit {
            let a_start = ia;
            let b_start = ib;

            while ia < a.len() && a[ia].is_ascii_alphabetic() {
                ia += 1;
            }
            while ib < b.len() && b[ib].is_ascii_alphabetic() {
                ib += 1;
            }

            match a[a_start..ia].cmp(&b[b_start..ib]) {
                Ordering::Equal => {}
                ordering => return ordering,
            }
        } else if a_digit {
            return Ordering::Greater;
        } else {
            return Ordering::Less;
        }
    }

    // Whatever is left wins only when it starts with a digit; alpha suffixes
    // (rc1, beta, a, ...) sort before the release without a suffix.
    let a_left = a.get(ia).copied();
    let b_left = b.get(ib).copied();
    match (a_left, b_left) {
        (Some(c), None) if c.is_ascii_digit() => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (None, Some(c)) if c.is_ascii_digit() => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

fn strip_leading_zeroes(mut input: &[u8]) -> &[u8] {
    while input.first() == Some(&b'0') {
        input = &input[1..];
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_and_pkgrel() {
        let evr = Evr::parse("2:1.2.3-4").unwrap();
        assert_eq!(evr.epoch, 2);
        assert_eq!(evr.pkgver, "1.2.3");
        assert_eq!(evr.pkgrel.as_deref(), Some("4"));
        assert_eq!(evr.to_string(), "2:1.2.3-4");
    }

    #[test]
    fn compares_like_pacman() {
        let cases = [
            ("1.0", "1.0", Ordering::Equal),
            ("1.0", "1.0.1", Ordering::Less),
            ("1.0.1", "1.0", Ordering::Greater),
            ("1.0-1", "1.0-2", Ordering::Less),
            ("1.0rc1", "1.0", Ordering::Less),
            ("1.0a", "1.0", Ordering::Less),
            ("1.0a", "1.0b", Ordering::Less),
            ("1.0", "1.0a", Ordering::Greater),
            ("1:1.0", "2.0", Ordering::Greater),
            ("1.0.0", "1.0", Ordering::Greater),
            ("1.0", "1.0.0", Ordering::Less),
            ("20240101", "20231231", Ordering::Greater),
            ("1.0_1", "1.0.1", Ordering::Equal),
        ];
        for (a, b, expected) in cases {
            assert_eq!(vercmp(a, b), expected, "{a} vs {b}");
        }
    }

    #[test]
    fn comparisons_are_reversible() {
        let versions = ["1.0", "1.0rc1", "1.0-1", "2:0.1", "1.0.1", "1.0a"];
        for a in versions {
            for b in versions {
                assert_eq!(vercmp(a, b), vercmp(b, a).reverse());
            }
        }
    }

    #[test]
    fn constraints_match() {
        let c = VersionConstraint::parse(">=1.2-1").unwrap().unwrap();
        assert!(Evr::parse("1.2-1").unwrap().matches(&c));
        assert!(Evr::parse("1.3-1").unwrap().matches(&c));
        assert!(!Evr::parse("1.1-9").unwrap().matches(&c));
    }
}
