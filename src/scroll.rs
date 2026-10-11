// One scroll model for every content-bearing pane. Offsets count lines from the top of the
// pane's content. The renderer clamps them and records the bounds in `Cell`s, so input
// handlers (which only hold `&App`) can clamp against what was last drawn.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;

/// Every pane that can scroll. Keyboard focus and mouse hit-testing both name panes with this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Project,
    Objective,
    Recent,
    BrowserPath,
    Folders,
    IssuesHeader,
    Issues,
    Preview,
    Totals,
    Lead,
    Advisor,
    Worker0,
    Worker1,
    Worker2,
    Review,
    Timeline,
    Terminal,
    Attention,
    Footer,
    Help,
}
impl Pane {
    pub const ALL: [Pane; 20] = [
        Pane::Project,
        Pane::Objective,
        Pane::Recent,
        Pane::BrowserPath,
        Pane::Folders,
        Pane::IssuesHeader,
        Pane::Issues,
        Pane::Preview,
        Pane::Totals,
        Pane::Lead,
        Pane::Advisor,
        Pane::Worker0,
        Pane::Worker1,
        Pane::Worker2,
        Pane::Review,
        Pane::Timeline,
        Pane::Terminal,
        Pane::Attention,
        Pane::Footer,
        Pane::Help,
    ];
    /// Dashboard panes in the order F5 cycles through them.
    pub const DASHBOARD: [Pane; 8] = [
        Pane::Totals,
        Pane::Lead,
        Pane::Advisor,
        Pane::Worker0,
        Pane::Worker1,
        Pane::Worker2,
        Pane::Review,
        Pane::Timeline,
    ];
}

/// A requested change of position. Keys and the mouse wheel both reduce to these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Lines(isize),
    Pages(isize),
    Top,
    End,
}

/// Lines per mouse-wheel notch.
pub const WHEEL_LINES: isize = 3;

/// Ctrl+Up/Down, Ctrl+PageUp/PageDown, Ctrl+Home/End scroll the focused pane. Plain keys keep
/// their navigation or Claude meaning. Ctrl-chords are never forwarded to Claude.
pub fn key_move(key: KeyEvent) -> Option<Move> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(Move::Lines(-1)),
        KeyCode::Down => Some(Move::Lines(1)),
        KeyCode::PageUp => Some(Move::Pages(-1)),
        KeyCode::PageDown => Some(Move::Pages(1)),
        KeyCode::Home => Some(Move::Top),
        KeyCode::End => Some(Move::End),
        _ => None,
    }
}

/// Where the view should start, given the current top line and the content bounds.
/// Pure so the terminal (which scrolls inside vt100) and the panes share one rule.
pub fn target(m: Move, top: usize, max: usize, viewport: usize) -> usize {
    let page = viewport.saturating_sub(1).max(1) as isize;
    let next = match m {
        Move::Lines(n) => top as isize + n,
        Move::Pages(n) => top as isize + n * page,
        Move::Top => 0,
        Move::End => max as isize,
    };
    next.clamp(0, max as isize) as usize
}

/// Viewport state for one pane.
///
/// `tail` panes (the session log) anchor to the newest line: the stored offset is the distance
/// from the end, so dropping old lines does not shift what the reader is looking at.
/// `follow` pins the view to the end; it is re-enabled when the reader scrolls back to the end.
#[derive(Debug)]
pub struct View {
    stored: Cell<usize>,
    max: Cell<usize>,
    viewport: Cell<usize>,
    total: Cell<usize>,
    follow: Cell<bool>,
    tail: bool,
}
impl View {
    fn new(tail: bool) -> Self {
        Self {
            stored: Cell::new(0),
            max: Cell::new(0),
            viewport: Cell::new(0),
            total: Cell::new(0),
            follow: Cell::new(tail),
            tail,
        }
    }
    /// The top line implied by the stored state and the bounds recorded by the last render.
    pub fn top(&self) -> usize {
        let max = self.max.get();
        if self.follow.get() {
            max
        } else if self.tail {
            max.saturating_sub(self.stored.get())
        } else {
            self.stored.get().min(max)
        }
    }
    fn store(&self, top: usize) {
        let max = self.max.get();
        self.stored.set(if self.tail {
            max.saturating_sub(top)
        } else {
            top
        });
    }
    /// Called by the renderer: records the content size and returns the top line to draw from.
    pub fn sync(&self, total: usize, viewport: usize) -> usize {
        self.total.set(total);
        self.viewport.set(viewport);
        self.max.set(total.saturating_sub(viewport));
        let top = self.top();
        self.store(top);
        top
    }
    /// Applies a move. Scrolling back to the end re-enables follow.
    pub fn apply(&self, m: Move) {
        let top = target(m, self.top(), self.max.get(), self.viewport.get());
        self.follow.set(top >= self.max.get());
        self.store(top);
    }
    /// Keeps a list's selected row on screen after keyboard navigation.
    pub fn reveal(&self, index: usize) {
        let viewport = self.viewport.get().max(1);
        let top = self.top();
        let next = if index < top {
            index
        } else if index >= top + viewport {
            index + 1 - viewport
        } else {
            top
        };
        self.follow.set(false);
        self.store(next.min(self.max.get()));
    }
    /// Returns to the default position: the top, or the newest line for a tail pane.
    pub fn reset(&self) {
        self.follow.set(self.tail);
        self.stored.set(0);
    }
    /// Keeps a text editor's end visible after an edit.
    pub fn pin_end(&self) {
        self.follow.set(true);
    }
    /// A short position note for the pane title, shown only when the pane overflows.
    pub fn status(&self) -> Option<String> {
        let max = self.max.get();
        if max == 0 {
            return None;
        }
        let top = self.top();
        let first = top + 1;
        let last = (top + self.viewport.get()).min(self.total.get());
        let arrows = match (top > 0, top < max) {
            (true, true) => "↑↓",
            (true, false) => "↑",
            (false, true) => "↓",
            (false, false) => "",
        };
        Some(format!("{arrows} {first}-{last}/{}", self.total.get()))
    }
}

/// The scroll state of every pane, indexed by `Pane`.
pub struct Scrolls {
    views: [View; Pane::ALL.len()],
}
impl Default for Scrolls {
    fn default() -> Self {
        Self {
            views: std::array::from_fn(|i| View::new(Pane::ALL[i] == Pane::Timeline)),
        }
    }
}
impl Scrolls {
    pub fn get(&self, pane: Pane) -> &View {
        &self.views[pane as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_chords_scroll_and_plain_keys_do_not() {
        let ctrl = |code| KeyEvent::new(code, KeyModifiers::CONTROL);
        assert_eq!(key_move(ctrl(KeyCode::Up)), Some(Move::Lines(-1)));
        assert_eq!(key_move(ctrl(KeyCode::PageDown)), Some(Move::Pages(1)));
        assert_eq!(key_move(ctrl(KeyCode::Home)), Some(Move::Top));
        assert_eq!(key_move(ctrl(KeyCode::End)), Some(Move::End));
        // Plain navigation and Claude's own keys are untouched.
        for code in [KeyCode::Up, KeyCode::PageUp, KeyCode::Home, KeyCode::End] {
            assert_eq!(key_move(KeyEvent::new(code, KeyModifiers::NONE)), None);
        }
        assert_eq!(key_move(ctrl(KeyCode::Char('l'))), None);
    }

    #[test]
    fn target_clamps_and_pages_by_viewport() {
        assert_eq!(target(Move::Lines(-1), 0, 10, 5), 0);
        assert_eq!(target(Move::Lines(20), 0, 10, 5), 10);
        assert_eq!(target(Move::Pages(1), 0, 100, 10), 9);
        assert_eq!(target(Move::Pages(1), 0, 100, 1), 1);
        assert_eq!(target(Move::End, 3, 10, 5), 10);
        assert_eq!(target(Move::Top, 7, 10, 5), 0);
    }

    #[test]
    fn pane_table_matches_enum_order() {
        // Scrolls indexes its views by discriminant, so ALL must list the variants in declaration order.
        for (i, pane) in Pane::ALL.iter().enumerate() {
            assert_eq!(*pane as usize, i, "{pane:?}");
        }
    }
    #[test]
    fn panes_keep_independent_offsets() {
        let s = Scrolls::default();
        s.get(Pane::Preview).sync(100, 10);
        s.get(Pane::Issues).sync(50, 10);
        s.get(Pane::Preview).apply(Move::Lines(30));
        assert_eq!(s.get(Pane::Preview).sync(100, 10), 30);
        assert_eq!(s.get(Pane::Issues).sync(50, 10), 0);
    }

    #[test]
    fn offsets_clamp_when_content_or_viewport_shrinks() {
        let v = View::new(false);
        v.sync(100, 10);
        v.apply(Move::End);
        assert_eq!(v.sync(100, 10), 90);
        // Content shrinks: the stored offset must not point past the new end.
        assert_eq!(v.sync(40, 10), 30);
        // Terminal grows: the view is no longer past the end.
        assert_eq!(v.sync(40, 40), 0);
    }

    #[test]
    fn scrolling_up_leaves_follow_and_returning_to_end_restores_it() {
        let v = View::new(false);
        v.sync(100, 10);
        v.pin_end();
        assert_eq!(v.sync(100, 10), 90);
        v.apply(Move::Lines(-1));
        assert_eq!(v.sync(100, 10), 89);
        // New lines arriving while reading history must not move the view.
        assert_eq!(v.sync(120, 10), 89);
        v.apply(Move::End);
        assert_eq!(v.sync(120, 10), 110);
    }

    #[test]
    fn tail_panes_follow_new_lines_and_survive_front_eviction() {
        let v = View::new(true);
        assert_eq!(v.sync(36, 10), 26, "starts at the newest line");
        v.apply(Move::Lines(-5));
        assert_eq!(v.sync(36, 10), 21);
        // Old lines dropped from the front: the reader stays on the same lines.
        assert_eq!(v.sync(35, 10), 20);
        v.apply(Move::End);
        assert_eq!(v.sync(40, 10), 30);
        v.reset();
        assert_eq!(v.sync(40, 10), 30);
    }

    #[test]
    fn reveal_keeps_selection_visible_without_fighting_a_manual_scroll() {
        let v = View::new(false);
        v.sync(50, 10);
        v.reveal(25);
        assert_eq!(v.sync(50, 10), 16);
        v.apply(Move::Lines(-10));
        assert_eq!(v.sync(50, 10), 6);
        v.reveal(0);
        assert_eq!(v.sync(50, 10), 0);
    }

    #[test]
    fn status_reports_position_only_when_overflowing() {
        let v = View::new(false);
        v.sync(5, 10);
        assert_eq!(v.status(), None);
        v.sync(40, 10);
        assert_eq!(v.status().as_deref(), Some("↓ 1-10/40"));
        v.apply(Move::End);
        v.sync(40, 10);
        assert_eq!(v.status().as_deref(), Some("↑ 31-40/40"));
    }
}
