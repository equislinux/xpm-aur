//! Parsing of ALPM dependency and provides specifications.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::version::VersionConstraint;

/// A dependency such as `glibc>=2.39` or `java-runtime`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub constraint: Option<VersionConstraint>,
}

impl Dependency {
    /// Parse `name[op version]`.
    pub fn parse(input: &str) -> CoreResult<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(CoreError::InvalidDependency("empty specification".into()));
        }

        let split = input
            .char_indices()
            .find(|(_, c)| matches!(c, '<' | '>' | '='))
            .map(|(index, _)| index)
            .unwrap_or(input.len());

        let name = input[..split].trim();
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err(CoreError::InvalidDependency(input.into()));
        }

        let constraint = VersionConstraint::parse(input[split..].trim())?;
        Ok(Self {
            name: name.to_string(),
            constraint,
        })
    }

    /// True for an unversioned dependency.
    pub fn is_unversioned(&self) -> bool {
        self.constraint.is_none()
    }
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.constraint {
            Some(constraint) => write!(f, "{}{constraint}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Evr;

    #[test]
    fn parses_plain_and_versioned_dependencies() {
        let plain = Dependency::parse("java-runtime").unwrap();
        assert_eq!(plain.name, "java-runtime");
        assert!(plain.constraint.is_none());

        let versioned = Dependency::parse("glibc>=2.39-1").unwrap();
        assert_eq!(versioned.name, "glibc");
        let constraint = versioned.constraint.unwrap();
        assert!(Evr::parse("2.39-1").unwrap().matches(&constraint));
        assert!(!Evr::parse("2.38-1").unwrap().matches(&constraint));
    }

    #[test]
    fn rejects_invalid_specs() {
        assert!(Dependency::parse("").is_err());
        assert!(Dependency::parse(">=1.0").is_err());
        assert!(Dependency::parse("foo>=").is_err());
    }
}
