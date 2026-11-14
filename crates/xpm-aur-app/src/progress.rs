//! Shared progress rendering.
//!
//! The bar implementation lives in the `xpm-progress` crate used by `xpm`, so
//! xpm-aur never drifts into a second format.

pub use xpm_progress::PhaseProgress;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_phases_use_cross_fill() {
        let line = PhaseProgress::new_network(["pkg"]).render("pkg", 1, 2, "sync");
        assert!(line.contains('x'), "expected cross fill: {line}");
        assert!(!line.contains('#'), "unexpected hash fill: {line}");
    }

    #[test]
    fn build_phases_use_slash_fill() {
        let line = PhaseProgress::new_build(["pkg"]).render("pkg", 1, 2, "building");
        assert!(line.contains('/'), "expected slash fill: {line}");
        assert!(!line.contains('#'), "unexpected hash fill: {line}");
    }
}
