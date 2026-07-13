//! Per-pane session navigation history (back/forward stacks).
//!
//! History is view-only and not persisted. Each side of each tab has its own
//! stack; path changes that should record history go through
//! [`HistoryOp::Push`] via `navigate_pane_*`.

/// How a path change interacts with [`PaneNavHistory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOp {
    /// Record the previous path on the back stack and clear forward.
    Push,
    /// Change path without touching history (e.g. initial load, go-to replace).
    #[allow(dead_code)] // wired for breadcrumbs / Go to Path (later tasks)
    Replace,
    /// Pop back → go there; push current onto forward.
    Back,
    /// Pop forward → go there; push current onto back.
    Forward,
}

/// Back/forward stacks for one pane side.
#[derive(Debug, Clone, Default)]
pub struct PaneNavHistory {
    pub back: Vec<String>,
    pub forward: Vec<String>,
}

impl PaneNavHistory {
    pub const MAX: usize = 50;

    /// Record a user navigation from `from` to `to`.
    ///
    /// No-op when `from` is missing or equal to `to`. Otherwise pushes `from`
    /// onto back, clears forward, and trims back to [`Self::MAX`].
    pub fn push_navigating_from(&mut self, from: Option<&str>, to: &str) {
        let Some(from) = from else {
            return;
        };
        if from == to {
            return;
        }
        self.back.push(from.to_string());
        if self.back.len() > Self::MAX {
            let excess = self.back.len() - Self::MAX;
            self.back.drain(0..excess);
        }
        self.forward.clear();
    }

    /// Pop the previous path; push `current` onto forward.
    pub fn go_back(&mut self, current: &str) -> Option<String> {
        let target = self.back.pop()?;
        self.forward.push(current.to_string());
        Some(target)
    }

    /// Pop the next path; push `current` onto back.
    pub fn go_forward(&mut self, current: &str) -> Option<String> {
        let target = self.forward.pop()?;
        self.back.push(current.to_string());
        Some(target)
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

/// Local + remote history for one connection tab.
#[derive(Debug, Clone, Default)]
pub struct TabNavState {
    pub local: PaneNavHistory,
    pub remote: PaneNavHistory,
}

#[cfg(test)]
mod tests {
    use super::PaneNavHistory;

    #[test]
    fn push_clears_forward() {
        let mut history = PaneNavHistory::default();
        history.push_navigating_from(Some("/a"), "/b");
        history.forward.push("/x".into()); // simulate leftover forward
        history.push_navigating_from(Some("/b"), "/c");
        assert!(history.forward.is_empty());
        assert_eq!(
            history.back,
            vec!["/a".to_string(), "/b".to_string()]
        );
    }

    #[test]
    fn back_and_forward_round_trip() {
        let mut history = PaneNavHistory::default();
        history.push_navigating_from(Some("/a"), "/b");
        history.push_navigating_from(Some("/b"), "/c");
        let to = history.go_back("/c").expect("back from /c");
        assert_eq!(to, "/b");
        let to = history.go_forward("/b").expect("forward from /b");
        assert_eq!(to, "/c");
    }

    #[test]
    fn push_same_path_is_noop() {
        let mut history = PaneNavHistory::default();
        history.push_navigating_from(Some("/a"), "/a");
        assert!(history.back.is_empty());
    }

    #[test]
    fn push_none_from_is_noop() {
        let mut history = PaneNavHistory::default();
        history.push_navigating_from(None, "/b");
        assert!(history.back.is_empty());
        assert!(history.forward.is_empty());
    }

    #[test]
    fn push_trims_to_max() {
        let mut history = PaneNavHistory::default();
        for index in 0..PaneNavHistory::MAX + 5 {
            let from = format!("/{index}");
            let to = format!("/{}", index + 1);
            history.push_navigating_from(Some(&from), &to);
        }
        assert_eq!(history.back.len(), PaneNavHistory::MAX);
        assert_eq!(history.back.first().map(String::as_str), Some("/5"));
    }

    #[test]
    fn go_back_empty_returns_none() {
        let mut history = PaneNavHistory::default();
        assert!(history.go_back("/here").is_none());
        assert!(history.forward.is_empty());
    }
}
