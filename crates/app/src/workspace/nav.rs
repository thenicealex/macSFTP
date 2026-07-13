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
    /// Change path without touching history (refresh-adjacent or rare replace).
    #[allow(dead_code)] // available for callers; Push covers normal navigation
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

/// Split an absolute POSIX-style path into breadcrumb segments.
///
/// Each item is `(label, absolute_path)`. Root is always `("/", "/")`.
/// Empty and `"/"` both yield a single root segment.
pub fn breadcrumb_segments(path: &str) -> Vec<(String, String)> {
    if path.is_empty() || path == "/" {
        return vec![("/".into(), "/".into())];
    }
    let mut out = vec![("/".into(), "/".into())];
    let mut acc = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        acc.push('/');
        acc.push_str(part);
        out.push((part.to_string(), acc.clone()));
    }
    out
}

/// Segments to render when the full trail is long: first, ellipsis, last two.
///
/// Returns indices into the full `segments` slice, or `None` for a non-clickable `…`.
pub fn breadcrumb_display_indices(segment_count: usize) -> Vec<Option<usize>> {
    if segment_count <= 5 {
        return (0..segment_count).map(Some).collect();
    }
    vec![
        Some(0),
        None,
        Some(segment_count - 2),
        Some(segment_count - 1),
    ]
}

#[cfg(test)]
mod tests {
    use super::{PaneNavHistory, breadcrumb_display_indices, breadcrumb_segments};

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

    #[test]
    fn breadcrumb_segments_root_and_nested() {
        assert_eq!(
            breadcrumb_segments("/"),
            vec![("/".into(), "/".into())]
        );
        assert_eq!(
            breadcrumb_segments(""),
            vec![("/".into(), "/".into())]
        );
        let segs = breadcrumb_segments("/Users/alex/Projects");
        assert_eq!(segs[0].0, "/");
        assert_eq!(segs[0].1, "/");
        assert_eq!(segs[1], ("Users".into(), "/Users".into()));
        assert_eq!(segs[2], ("alex".into(), "/Users/alex".into()));
        assert_eq!(segs.last().map(|s| s.1.as_str()), Some("/Users/alex/Projects"));
        assert_eq!(segs.len(), 4);
    }

    #[test]
    fn breadcrumb_display_collapses_when_more_than_five() {
        assert_eq!(
            breadcrumb_display_indices(5),
            vec![Some(0), Some(1), Some(2), Some(3), Some(4)]
        );
        assert_eq!(
            breadcrumb_display_indices(6),
            vec![Some(0), None, Some(4), Some(5)]
        );
        assert_eq!(
            breadcrumb_display_indices(1),
            vec![Some(0)]
        );
    }
}
