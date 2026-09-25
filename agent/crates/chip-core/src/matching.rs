//! Which on-screen chip is the task? The UI never shows task_id, so match by title
//! (exact, then whitespace-normalized) and break ties with tldr.

/// A chip as read from the UIA tree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChipInfo {
    pub title: String,
    pub tldr: String,
    /// Names of the chip's buttons (dismiss, pager, start, more options).
    pub buttons: Vec<String>,
    /// Left x of the chip in screen coordinates; identifies the pane in split view.
    pub pane_x: i32,
}

impl ChipInfo {
    pub fn has_button(&self, name: &str) -> bool {
        self.buttons.iter().any(|b| b == name)
    }
}

/// Collapses runs of whitespace (incl. newlines, NBSP, full-width space) to one ASCII
/// space and trims.
pub fn normalize(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c == '\u{3000}')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !(c.is_whitespace() || *c == '\u{3000}'))
        .collect()
}

/// Index of the chip for (title, tldr), or None.
pub fn select_chip(chips: &[ChipInfo], title: &str, tldr: Option<&str>) -> Option<usize> {
    let mut cands: Vec<usize> = (0..chips.len())
        .filter(|&i| chips[i].title == title)
        .collect();
    if cands.is_empty() {
        let nt = normalize(title);
        cands = (0..chips.len())
            .filter(|&i| normalize(&chips[i].title) == nt)
            .collect();
    }
    match cands.len() {
        0 => None,
        1 => Some(cands[0]),
        _ => {
            if let Some(t) = tldr.filter(|t| !t.is_empty()) {
                if let Some(&i) = cands.iter().find(|&&i| chips[i].tldr == t) {
                    return Some(i);
                }
                // The tldr group may be split into several text runs.
                let st = squash(t);
                if let Some(&i) = cands.iter().find(|&&i| squash(&chips[i].tldr) == st) {
                    return Some(i);
                }
            }
            Some(cands[0])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip(title: &str, tldr: &str) -> ChipInfo {
        ChipInfo {
            title: title.into(),
            tldr: tldr.into(),
            ..Default::default()
        }
    }

    #[test]
    fn exact_then_normalized() {
        let chips = vec![chip("a  b", ""), chip("c", "")];
        assert_eq!(select_chip(&chips, "c", None), Some(1));
        assert_eq!(select_chip(&chips, "a b", None), Some(0));
        assert_eq!(select_chip(&chips, "a\u{3000}b\n", None), Some(0));
        assert_eq!(select_chip(&chips, "zzz", None), None);
    }

    #[test]
    fn tldr_breaks_ties() {
        let chips = vec![chip("t", "one"), chip("t", "two three")];
        assert_eq!(select_chip(&chips, "t", Some("two three")), Some(1));
        assert_eq!(select_chip(&chips, "t", Some("twothree")), Some(1));
        assert_eq!(select_chip(&chips, "t", Some("nomatch")), Some(0));
        assert_eq!(select_chip(&chips, "t", None), Some(0));
    }

    #[test]
    fn has_button() {
        let c = ChipInfo {
            buttons: vec!["x".into(), "y".into()],
            ..Default::default()
        };
        assert!(c.has_button("y"));
        assert!(!c.has_button("z"));
    }
}
