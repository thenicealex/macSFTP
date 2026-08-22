//! Breadcrumb helpers for the path bar.
//!
//! Per-pane back/forward navigation history lives in
//! `macsftp_core::TabNavState` (part of `TabState`), not in this module.

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
    use super::{breadcrumb_display_indices, breadcrumb_segments};

    #[test]
    fn breadcrumb_segments_root_and_nested() {
        assert_eq!(breadcrumb_segments("/"), vec![("/".into(), "/".into())]);
        assert_eq!(breadcrumb_segments(""), vec![("/".into(), "/".into())]);
        let segs = breadcrumb_segments("/Users/alex/Projects");
        assert_eq!(segs[0].0, "/");
        assert_eq!(segs[0].1, "/");
        assert_eq!(segs[1], ("Users".into(), "/Users".into()));
        assert_eq!(segs[2], ("alex".into(), "/Users/alex".into()));
        assert_eq!(
            segs.last().map(|s| s.1.as_str()),
            Some("/Users/alex/Projects")
        );
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
        assert_eq!(breadcrumb_display_indices(1), vec![Some(0)]);
    }
}
