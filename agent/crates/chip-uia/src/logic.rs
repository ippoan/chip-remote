//! UIA-independent decisions, split out so they can be unit-tested without a live
//! Claude window. The Windows backend feeds these with (control type, name) data read
//! from the UIA tree.
//!
//! Measured chip layout (Claude desktop 2.7032, docs/PROTOCOL.md "UIA での chip の見え方"):
//!
//! ```text
//! StatusBar                        <- the chip body
//!   Text   <marker>                (labels.marker)
//!   Text   <title>
//!   Group
//!     Text <tldr>
//! Button <dismiss>                 <- the buttons are SIBLINGS of the StatusBar
//! Group '' > Button <prev>         pager: only when the session has several chips;
//! Text   "N of M"                  then only the current one is rendered and
//! Group '' > Button <next>         labels.next pages to the others
//! Group  <start>
//!   Button <start>                 (InvokePattern)
//!   Button <more options>
//! Group  "チャットメッセージ"         <- end of the chip
//! ```

use chip_core::{normalize, ChipInfo, Labels};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// The UIA control types the chip logic distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    StatusBar,
    Text,
    Group,
    Button,
    Image,
    Other,
}

/// A direct child of a StatusBar: control type and Name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub kind: Kind,
    pub name: String,
}

impl Child {
    pub fn new(kind: Kind, name: &str) -> Child {
        Child {
            kind,
            name: name.to_string(),
        }
    }
}

/// What to do with the next following sibling of a chip's StatusBar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiblingStep {
    /// The sibling itself is one of the chip's buttons (dismiss).
    TakeButton,
    /// Unnamed pager group or the "start" split-button group: its descendant buttons
    /// belong to the chip.
    TakeGroupButtons,
    /// Pager position text ("2件中1番目"); keep walking.
    Skip,
    /// Next StatusBar (another chip), a named Group other than "start" (the chat
    /// message list), or anything unexpected: the chip ends here.
    Stop,
}

/// Is `name` the chip's start button (or the group that wraps it)? The configured label,
/// or any name ending with `labels.start_suffix` except the "more options" button
/// (SSH sessions read "mini-ryzen-claudeでworktreeを使って開始").
pub fn is_start_label(name: &str, labels: &Labels) -> bool {
    if name == labels.start {
        return true;
    }
    let suffix = labels.start_suffix.as_str();
    !suffix.is_empty() && name.ends_with(suffix) && !name.contains(MORE_OPTIONS_MARK)
}

/// "その他の開始オプション" ends with オプション, but guard anyway in case the UI changes.
const MORE_OPTIONS_MARK: &str = "その他";

pub fn sibling_step(kind: Kind, name: &str, labels: &Labels) -> SiblingStep {
    match kind {
        Kind::Button => SiblingStep::TakeButton,
        Kind::Group if name.is_empty() || is_start_label(name, labels) => {
            SiblingStep::TakeGroupButtons
        }
        Kind::Text => SiblingStep::Skip,
        _ => SiblingStep::Stop,
    }
}

/// Parses a StatusBar's children into (title, tldr), or None when it is not a chip
/// (no marker text). `group_tldr(i)` is asked for Group children after the marker until
/// a tldr is found; it returns Some(text) for the text-only tldr group and None for a
/// group that holds buttons (a future layout might nest the start group here).
pub fn parse_chip<F>(
    children: &[Child],
    labels: &Labels,
    mut group_tldr: F,
) -> Option<(String, String)>
where
    F: FnMut(usize) -> Option<String>,
{
    let mut seen_marker = false;
    let mut title: Option<String> = None;
    let mut tldr: Option<String> = None;
    for (i, c) in children.iter().enumerate() {
        match c.kind {
            Kind::Text => {
                if !seen_marker {
                    if c.name == labels.marker {
                        seen_marker = true;
                    }
                } else if title.is_none() && !c.name.is_empty() {
                    title = Some(c.name.clone());
                }
            }
            Kind::Group if seen_marker && tldr.is_none() => {
                tldr = group_tldr(i);
            }
            _ => {}
        }
    }
    if !seen_marker {
        return None;
    }
    Some((title.unwrap_or_default(), tldr.unwrap_or_default()))
}

/// The tldr group may be split into several text runs; concatenate them. Falls back to
/// the group's own Name when it has no Text descendants.
pub fn join_tldr(texts: &[String], group_name: &str) -> String {
    if texts.is_empty() {
        group_name.to_string()
    } else {
        texts.concat()
    }
}

/// Chromium builds its a11y tree lazily: the first query after a period without UIA
/// clients returns only the title-bar buttons. A window is "cold" when it was never
/// queried or not within `idle`.
pub fn is_cold(last_query: Option<Instant>, now: Instant, idle: Duration) -> bool {
    match last_query {
        None => true,
        Some(t) => now.saturating_duration_since(t) > idle,
    }
}

/// Pager bookkeeping for `act`: per pane (chip left x), the titles already shown.
/// A pane whose current title was already seen has cycled through all its chips.
#[derive(Debug, Default)]
pub struct PagerState {
    seen: HashMap<i32, HashSet<String>>,
}

impl PagerState {
    /// Indices of the chips whose "next" button should be pressed this round: chips
    /// that have `next_label` and whose title has not been shown in that pane yet.
    /// Records every such title as seen (even if the press later fails, so a broken
    /// pane cannot loop forever).
    pub fn plan(&mut self, chips: &[ChipInfo], next_label: &str) -> Vec<usize> {
        let mut out = Vec::new();
        for (i, c) in chips.iter().enumerate() {
            if !c.has_button(next_label) {
                continue;
            }
            let titles = self.seen.entry(c.pane_x).or_default();
            if titles.insert(c.title.clone()) {
                out.push(i);
            }
        }
        out
    }
}

/// After pressing "next", the a11y tree lags ~300 ms+. True while some pressed pane
/// (pane -> title shown before the press) still shows the old title; the caller keeps
/// waiting, otherwise the next round would misdetect that title as a cycle.
pub fn any_stale(pressed: &HashMap<i32, String>, chips: &[ChipInfo]) -> bool {
    chips
        .iter()
        .any(|c| pressed.get(&c.pane_x).is_some_and(|t| *t == c.title))
}

/// Is this process image (full path) the Claude executable? The Claude Code CLI is
/// also claude.exe, but has no top-level window, so the caller filters windows first.
pub fn is_claude_image(path: &str) -> bool {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    file.eq_ignore_ascii_case("claude.exe")
}

/// Picks Claude's main window among the visible, unowned top-level windows of
/// claude.exe: the one titled "Claude", else the first with a title, else the first.
pub fn pick_main_window(titles: &[String]) -> Option<usize> {
    titles
        .iter()
        .position(|t| t == "Claude")
        .or_else(|| titles.iter().position(|t| !t.is_empty()))
        .or(if titles.is_empty() { None } else { Some(0) })
}

// ------------------------------------------------------------------ sessions
//
// Measured sidebar / pane-header layout (Claude desktop 2.7032, Japanese UI, 2026-09-25):
//
// ```text
// Button "<status> <title>"                <- sidebar session entry; Invoke opens it in
//   StatusBar|Image "<status>"                the primary pane. status is e.g. "実行中",
//   Group ''                                  "未読の返答", "アイドル" or a PR prefix
// Button "<title>のその他のオプション"       "#21, #584 · マージ済み" (then an Image)
// ...
// Button "<title>、セッション名を変更"        <- header of each SHOWN chat pane
// Button "<title>のその他のオプション"
// ```
//
// The account button ("<name> <name> Max": Image + Text + Text) and the chat-pane task
// buttons ("実行中 <agent task>", no children) must not be taken for sessions: an entry
// needs exactly the [StatusBar|Image, Group] child structure.

/// Suffix of a shown pane's header button: "<title>、セッション名を変更".
pub const RENAME_SUFFIX: &str = "、セッション名を変更";
/// Suffix of the per-session menu buttons (sidebar and header), never the one to press.
pub const MORE_OPTIONS_SUFFIX: &str = "のその他のオプション";

/// Session title of a pane header button, or None when `name` is not a header.
pub fn header_session_title(name: &str) -> Option<&str> {
    name.strip_suffix(RENAME_SUFFIX).filter(|t| !t.is_empty())
}

/// The status text of a sidebar session entry, whose children are exactly
/// [StatusBar|Image "<status>", Group] (measured). None for any other button.
pub fn sidebar_status(children: &[Child]) -> Option<&str> {
    match children {
        [s, g]
            if matches!(s.kind, Kind::StatusBar | Kind::Image)
                && !s.name.is_empty()
                && g.kind == Kind::Group =>
        {
            Some(&s.name)
        }
        _ => None,
    }
}

/// Session title of a sidebar entry named "<status> <title>". None when there is no
/// status or the name does not start with "<status> ".
pub fn sidebar_session_title<'a>(name: &'a str, status: Option<&str>) -> Option<&'a str> {
    let status = status.filter(|s| !s.is_empty())?;
    name.strip_prefix(status)?
        .strip_prefix(' ')
        .filter(|t| !t.is_empty())
}

/// How well a sidebar entry matches the wanted session title (higher is better).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SidebarMatch {
    /// The name ends with " " + title, but the status prefix does not split it off as
    /// exactly that title.
    Suffix,
    /// The title split off the name equals the wanted one after whitespace
    /// normalization (the session file and the UI may differ in spacing).
    Normalized,
    /// The name is exactly "<status> <title>" (or exactly the title).
    Exact,
}

/// Does the sidebar button `name` (with `status` from [`sidebar_status`]) open the
/// session titled `title`? The "…のその他のオプション" menu buttons never match.
pub fn match_sidebar(name: &str, status: Option<&str>, title: &str) -> Option<SidebarMatch> {
    if title.is_empty() || name.is_empty() {
        return None;
    }
    if name.ends_with(MORE_OPTIONS_SUFFIX) && !title.ends_with(MORE_OPTIONS_SUFFIX) {
        return None;
    }
    if name == title {
        return Some(SidebarMatch::Exact);
    }
    if let Some(t) = sidebar_session_title(name, status) {
        if t == title {
            return Some(SidebarMatch::Exact);
        }
        if normalize(t) == normalize(title) {
            return Some(SidebarMatch::Normalized);
        }
    }
    if normalize(name).ends_with(&format!(" {}", normalize(title))) {
        return Some(SidebarMatch::Suffix);
    }
    None
}

/// Index of the best sidebar entry for `title` among (name, status) pairs: the highest
/// [`SidebarMatch`]; on a tie the first one (top of the sidebar = most recent).
pub fn pick_sidebar(entries: &[(String, Option<String>)], title: &str) -> Option<usize> {
    let mut best: Option<(SidebarMatch, usize)> = None;
    for (i, (name, status)) in entries.iter().enumerate() {
        if let Some(m) = match_sidebar(name, status.as_deref(), title) {
            if best.is_none_or(|(b, _)| m > b) {
                best = Some((m, i));
            }
        }
    }
    best.map(|(_, i)| i)
}

/// Is the pane-header title `shown` the session `title` (exact or whitespace-normalized)?
pub fn same_session(shown: &str, title: &str) -> bool {
    shown == title || normalize(shown) == normalize(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Labels {
        Labels::default()
    }

    fn chip(title: &str, pane_x: i32, buttons: &[&str]) -> ChipInfo {
        ChipInfo {
            title: title.into(),
            tldr: String::new(),
            buttons: buttons.iter().map(|s| s.to_string()).collect(),
            pane_x,
        }
    }

    #[test]
    fn sibling_walk_matches_measured_layout() {
        let l = labels();
        // Following siblings of the StatusBar, in tree order (2026-09-25 measurement).
        let seq = [
            (Kind::Button, "提案を非表示", SiblingStep::TakeButton),
            (Kind::Group, "", SiblingStep::TakeGroupButtons),
            (Kind::Text, "2件中1番目", SiblingStep::Skip),
            (Kind::Group, "", SiblingStep::TakeGroupButtons),
            (
                Kind::Group,
                "ワークツリーで開始",
                SiblingStep::TakeGroupButtons,
            ),
            (Kind::Group, "チャットメッセージ", SiblingStep::Stop),
        ];
        for (kind, name, want) in seq {
            assert_eq!(sibling_step(kind, name, &l), want, "{kind:?} {name}");
        }
        assert_eq!(sibling_step(Kind::StatusBar, "", &l), SiblingStep::Stop);
        assert_eq!(sibling_step(Kind::Other, "x", &l), SiblingStep::Stop);
    }

    #[test]
    fn sibling_walk_uses_configured_start_label() {
        let l = Labels {
            start: "Start in worktree".into(),
            start_suffix: String::new(),
            ..Labels::default()
        };
        assert_eq!(
            sibling_step(Kind::Group, "Start in worktree", &l),
            SiblingStep::TakeGroupButtons
        );
        assert_eq!(
            sibling_step(Kind::Group, "ワークツリーで開始", &l),
            SiblingStep::Stop
        );
    }

    #[test]
    fn start_label_matches_local_and_ssh_wording() {
        let l = labels();
        assert!(is_start_label("ワークツリーで開始", &l));
        assert!(is_start_label(
            "mini-ryzen-claudeでworktreeを使って開始",
            &l
        ));
        assert!(!is_start_label("その他の開始オプション", &l));
        assert!(!is_start_label("提案を非表示", &l));
        assert!(!is_start_label("開始済み 3セッション", &l));
        // the SSH start group is walked into, not treated as the chat list
        assert_eq!(
            sibling_step(Kind::Group, "mini-ryzen-claudeでworktreeを使って開始", &l),
            SiblingStep::TakeGroupButtons
        );
        let no_suffix = Labels {
            start_suffix: String::new(),
            ..Labels::default()
        };
        assert!(!is_start_label(
            "mini-ryzen-claudeでworktreeを使って開始",
            &no_suffix
        ));
    }

    #[test]
    fn parses_chip_children() {
        let l = labels();
        let kids = [
            Child::new(Kind::Text, "推奨タスク"),
            Child::new(Kind::Text, "Fix the thing"),
            Child::new(Kind::Group, ""),
        ];
        let mut asked = vec![];
        let r = parse_chip(&kids, &l, |i| {
            asked.push(i);
            Some("tl;dr".into())
        });
        assert_eq!(r, Some(("Fix the thing".into(), "tl;dr".into())));
        assert_eq!(asked, vec![2]);
    }

    #[test]
    fn non_chip_status_bar_is_ignored() {
        let l = labels();
        let kids = [Child::new(Kind::Text, "Ready"), Child::new(Kind::Group, "")];
        assert_eq!(parse_chip(&kids, &l, |_| panic!("not asked")), None);
        assert_eq!(parse_chip(&[], &l, |_| None), None);
    }

    #[test]
    fn button_group_is_skipped_for_tldr_and_empty_title_is_skipped() {
        let l = labels();
        let kids = [
            Child::new(Kind::Group, ""), // before marker: never asked
            Child::new(Kind::Text, "推奨タスク"),
            Child::new(Kind::Text, ""),
            Child::new(Kind::Text, "T"),
            Child::new(Kind::Text, "second text is not the title"),
            Child::new(Kind::Group, "start"), // holds buttons -> None
            Child::new(Kind::Group, ""),      // text only -> tldr
            Child::new(Kind::Group, ""),      // tldr already found: not asked
        ];
        let mut asked = vec![];
        let r = parse_chip(&kids, &l, |i| {
            asked.push(i);
            if i == 5 {
                None
            } else {
                Some(format!("g{i}"))
            }
        });
        assert_eq!(r, Some(("T".into(), "g6".into())));
        assert_eq!(asked, vec![5, 6]);
    }

    #[test]
    fn chip_without_title_or_tldr() {
        let l = labels();
        let r = parse_chip(&[Child::new(Kind::Text, "推奨タスク")], &l, |_| None);
        assert_eq!(r, Some((String::new(), String::new())));
    }

    #[test]
    fn tldr_joins_runs_or_falls_back_to_group_name() {
        assert_eq!(join_tldr(&["ab ".into(), "cd".into()], "g"), "ab cd");
        assert_eq!(join_tldr(&[], "group name"), "group name");
        assert_eq!(join_tldr(&[], ""), "");
    }

    #[test]
    fn cold_after_idle() {
        let now = Instant::now();
        let idle = Duration::from_secs(30);
        assert!(is_cold(None, now, idle));
        assert!(!is_cold(Some(now), now, idle));
        let later = now + Duration::from_secs(31);
        assert!(is_cold(Some(now), later, idle));
        assert!(!is_cold(Some(now), now + Duration::from_secs(29), idle));
    }

    #[test]
    fn pager_presses_each_pane_until_it_cycles() {
        let next = Labels::default().next;
        let mut p = PagerState::default();
        // Two panes with pagers, one single-chip pane without.
        let round1 = vec![
            chip("a1", 0, &["提案を非表示", &next]),
            chip("b1", 800, &[&next]),
            chip("c1", 1600, &["提案を非表示"]),
        ];
        assert_eq!(p.plan(&round1, &next), vec![0, 1]);
        let round2 = vec![chip("a2", 0, &[&next]), chip("b1", 800, &[&next])];
        // Pane 800 still shows b1 (seen) -> not pressed again.
        assert_eq!(p.plan(&round2, &next), vec![0]);
        // Pane 0 wraps back to a1 -> cycled; nothing left to press.
        let round3 = vec![chip("a1", 0, &[&next]), chip("b1", 800, &[&next])];
        assert!(p.plan(&round3, &next).is_empty());
    }

    #[test]
    fn stale_until_pressed_pane_changes_title() {
        let mut pressed = HashMap::new();
        pressed.insert(0, "a1".to_string());
        assert!(any_stale(&pressed, &[chip("a1", 0, &[])]));
        assert!(!any_stale(&pressed, &[chip("a2", 0, &[])]));
        // Same title in another pane does not count.
        assert!(!any_stale(&pressed, &[chip("a1", 800, &[])]));
        // Pane vanished: nothing stale.
        assert!(!any_stale(&pressed, &[]));
    }

    #[test]
    fn claude_image_names() {
        assert!(is_claude_image(
            r"C:\Users\x\AppData\Local\AnthropicClaude\app-1.0\claude.exe"
        ));
        assert!(is_claude_image(
            r"C:\Program Files\WindowsApps\Claude_1.0\app\Claude.exe"
        ));
        assert!(is_claude_image("claude.exe"));
        assert!(!is_claude_image(r"C:\x\claude-code.exe"));
        assert!(!is_claude_image(r"C:\claude.exe\other.exe"));
    }

    #[test]
    fn main_window_prefers_title_claude() {
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            pick_main_window(&t(&["", "Quick entry", "Claude"])),
            Some(2)
        );
        assert_eq!(pick_main_window(&t(&["", "Other"])), Some(1));
        assert_eq!(pick_main_window(&t(&["", ""])), Some(0));
        assert_eq!(pick_main_window(&[]), None);
    }

    fn entry_kids(status_kind: Kind, status: &str) -> Vec<Child> {
        vec![Child::new(status_kind, status), Child::new(Kind::Group, "")]
    }

    #[test]
    fn header_title_is_the_rename_button_prefix() {
        assert_eq!(
            header_session_title("#p1133 訴訟用の準備ページの監督、セッション名を変更"),
            Some("#p1133 訴訟用の準備ページの監督")
        );
        assert_eq!(
            header_session_title("Claude Desktop Android 拡張、セッション名を変更"),
            Some("Claude Desktop Android 拡張")
        );
        assert_eq!(header_session_title("、セッション名を変更"), None);
        assert_eq!(
            header_session_title("Claude Desktop Android 拡張のその他のオプション"),
            None
        );
        assert_eq!(header_session_title("分割ビューを閉じる"), None);
    }

    #[test]
    fn sidebar_status_needs_measured_structure() {
        assert_eq!(
            sidebar_status(&entry_kids(Kind::StatusBar, "実行中")),
            Some("実行中")
        );
        assert_eq!(
            sidebar_status(&entry_kids(Kind::Image, "#21, #584 · マージ済み")),
            Some("#21, #584 · マージ済み")
        );
        // Account button: Image + Text + Text.
        let account = [
            Child::new(Kind::Image, "user"),
            Child::new(Kind::Text, "user"),
            Child::new(Kind::Text, "Max"),
        ];
        assert_eq!(sidebar_status(&account), None);
        // Chat-pane task buttons and menu buttons have no children.
        assert_eq!(sidebar_status(&[]), None);
        assert_eq!(sidebar_status(&entry_kids(Kind::StatusBar, "")), None);
        assert_eq!(sidebar_status(&entry_kids(Kind::Text, "実行中")), None);
        let mut three = entry_kids(Kind::StatusBar, "実行中");
        three.push(Child::new(Kind::Group, ""));
        assert_eq!(sidebar_status(&three), None);
    }

    #[test]
    fn sidebar_title_strips_status_and_pr_prefixes() {
        let cases = [
            (
                "実行中 Claude Desktop Android 拡張",
                "実行中",
                "Claude Desktop Android 拡張",
            ),
            (
                "入力待ち #p1133 訴訟用の準備ページの監督",
                "入力待ち",
                "#p1133 訴訟用の準備ページの監督",
            ),
            ("アイドル 仕入れ対応", "アイドル", "仕入れ対応"),
            (
                "未読の返答 chip-remote prod probe 2",
                "未読の返答",
                "chip-remote prod probe 2",
            ),
            (
                "#21, #584 · マージ済み #p20 指静脈の配線の監督",
                "#21, #584 · マージ済み",
                "#p20 指静脈の配線の監督",
            ),
            (
                "#273 · マージ済み [O] VoiceS3R で FC-1200 を RS232 (G7/G8) につなぐ",
                "#273 · マージ済み",
                "[O] VoiceS3R で FC-1200 を RS232 (G7/G8) につなぐ",
            ),
        ];
        for (name, status, want) in cases {
            assert_eq!(
                sidebar_session_title(name, Some(status)),
                Some(want),
                "{name}"
            );
        }
        assert_eq!(sidebar_session_title("実行中 x", None), None);
        assert_eq!(sidebar_session_title("実行中 x", Some("")), None);
        assert_eq!(sidebar_session_title("実行中x", Some("実行中")), None);
        assert_eq!(sidebar_session_title("実行中 ", Some("実行中")), None);
        assert_eq!(sidebar_session_title("アイドル x", Some("実行中")), None);
    }

    #[test]
    fn match_sidebar_ranks_exact_over_suffix() {
        let t = "Claude Desktop Android 拡張";
        let name = "実行中 Claude Desktop Android 拡張";
        assert_eq!(
            match_sidebar(name, Some("実行中"), t),
            Some(SidebarMatch::Exact)
        );
        // Status unknown: only the suffix rule applies.
        assert_eq!(match_sidebar(name, None, t), Some(SidebarMatch::Suffix));
        // A title that is a word-suffix of another title (titles contain spaces) is
        // only a suffix match, never exact.
        assert_eq!(
            match_sidebar(name, Some("実行中"), "Android 拡張"),
            Some(SidebarMatch::Suffix)
        );
        // Not at a word boundary.
        assert_eq!(match_sidebar(name, Some("実行中"), "張"), None);
        assert_eq!(match_sidebar(name, Some("実行中"), "id 拡張"), None);
        // PR prefix.
        assert_eq!(
            match_sidebar(
                "#21, #584 · マージ済み #p20 指静脈の配線の監督",
                Some("#21, #584 · マージ済み"),
                "#p20 指静脈の配線の監督"
            ),
            Some(SidebarMatch::Exact)
        );
        // Exact name (no prefix at all).
        assert_eq!(match_sidebar(t, None, t), Some(SidebarMatch::Exact));
        // Spacing differs between the session file and the UI.
        assert_eq!(
            match_sidebar(
                "アイドル 本社ネット接続 syslog確認",
                Some("アイドル"),
                "本社ネット接続  syslog確認"
            ),
            Some(SidebarMatch::Normalized)
        );
        assert_eq!(match_sidebar("", None, t), None);
        assert_eq!(match_sidebar(name, None, ""), None);
    }

    #[test]
    fn match_sidebar_ignores_more_options_buttons() {
        assert_eq!(
            match_sidebar("仕入れ対応のその他のオプション", None, "仕入れ対応"),
            None
        );
        assert_eq!(
            match_sidebar("x 仕入れ対応のその他のオプション", None, "x 仕入れ対応"),
            None
        );
        // Only a title that itself ends with the suffix could match such a name.
        assert_eq!(
            match_sidebar(
                "アイドル aのその他のオプション",
                Some("アイドル"),
                "aのその他のオプション"
            ),
            Some(SidebarMatch::Exact)
        );
    }

    #[test]
    fn pick_sidebar_prefers_exact_then_first() {
        let e = |v: &[(&str, Option<&str>)]| {
            v.iter()
                .map(|(n, s)| (n.to_string(), s.map(str::to_string)))
                .collect::<Vec<_>>()
        };
        let entries = e(&[
            ("アイドル x foo bar", Some("アイドル")), // suffix for "foo bar"
            ("foo barのその他のオプション", None),    // menu: ignored
            ("入力待ち foo bar", Some("入力待ち")),   // exact
            ("#1 · マージ済み foo bar", Some("#1 · マージ済み")), // exact, later
        ]);
        assert_eq!(pick_sidebar(&entries, "foo bar"), Some(2));
        assert_eq!(pick_sidebar(&entries, "x foo bar"), Some(0));
        assert_eq!(pick_sidebar(&entries, "bar"), Some(0));
        assert_eq!(pick_sidebar(&entries, "nothing"), None);
        assert_eq!(pick_sidebar(&[], "foo"), None);
    }

    #[test]
    fn same_session_normalizes_whitespace() {
        assert!(same_session("a b", "a b"));
        assert!(same_session("a  b ", "a b"));
        assert!(same_session("a\u{3000}b", "a b"));
        assert!(!same_session("a b", "a bc"));
    }
}
