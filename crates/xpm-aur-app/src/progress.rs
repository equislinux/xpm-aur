//! Shared progress rendering.
//!
//! The bar implementation lives in the `xpm-progress` crate used by `xpm`, so
//! xpm-aur never drifts into a second format.

pub use xpm_progress::PhaseProgress;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_bars_use_slash_fill() {
        let line = PhaseProgress::new(["pkg"]).render("pkg", 1, 2, "build");
        assert!(line.contains('/'), "expected slash fill: {line}");
        assert!(!line.contains('#'), "unexpected hash fill: {line}");
        assert!(!line.contains('x'), "unexpected x fill: {line}");
    }
}
