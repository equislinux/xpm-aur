//! Aligned phase progress for AUR builds.
//!
//! makepkg does not expose a byte-level progress stream, so xpm-aur reports
//! discrete phases with the same label/bar layout used by xpm. The label column
//! is fixed per transaction so lines stay aligned.

/// Column layout for AUR phase lines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhaseProgress {
    label_width: usize,
    bar_width: usize,
}

impl PhaseProgress {
    /// Build a layout for the package bases in one transaction.
    pub fn new<'a>(labels: impl IntoIterator<Item = &'a str>) -> Self {
        let longest = labels
            .into_iter()
            .map(|label| label.chars().count())
            .max()
            .unwrap_or(12);
        Self {
            label_width: longest.clamp(12, 32),
            bar_width: 20,
        }
    }

    /// Render `label [xxxx----]  50%  building`.
    pub fn render(&self, label: &str, step: u64, total: u64, phase: &str) -> String {
        let percent = if total == 0 {
            0
        } else {
            step.saturating_mul(100).div_euclid(total).min(100)
        };
        let filled = (percent as usize) * self.bar_width / 100;
        let bar = format!(
            "{}{}",
            "x".repeat(filled),
            "-".repeat(self.bar_width - filled)
        );
        let label = align_label(label, self.label_width);
        format!("{label} [{}] {percent:>3}%  {phase}", bar,)
    }

    pub fn print(&self, label: &str, step: u64, total: u64, phase: &str) {
        eprintln!("{}", self.render(label, step, total, phase));
    }
}

fn align_label(label: &str, width: usize) -> String {
    let chars: Vec<char> = label.chars().collect();
    if chars.len() > width {
        if width <= 2 {
            return ".".repeat(width);
        }
        let mut truncated: String = chars.into_iter().take(width - 2).collect();
        truncated.push_str("..");
        truncated
    } else {
        format!("{label:<width$}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_share_one_column() {
        let progress = PhaseProgress::new(["foo", "a-very-long-package"]);
        let short = progress.render("foo", 1, 3, "building");
        let long = progress.render("a-very-long-package", 1, 3, "building");
        assert_eq!(short.find('['), long.find('['));
    }

    #[test]
    fn long_labels_are_truncated() {
        let progress = PhaseProgress::new(["x"]);
        let line = progress.render("a-very-long-package-name", 1, 3, "building");
        assert!(line.starts_with("a-very-lon.."));
        assert_eq!(line.find('['), Some(13));
    }
}
