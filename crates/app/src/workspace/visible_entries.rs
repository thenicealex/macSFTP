//! Derive the visible file-list index space from stored entries.
//!
//! Stored `TabState` listings stay complete and sorted; the UI and keyboard
//! navigation map through these helpers so hiding dotfiles, the explicit
//! filter query, and type-select prefix matching never mutate the underlying
//! snapshot.

use macsftp_core::{LocalEntry, RemoteEntry};

/// Dotfile rule for macSFTP: basename starts with `.`.
pub(crate) fn is_dotfile(name: &str) -> bool {
    name.starts_with('.')
}

/// Case-insensitive query matcher for file names with an ASCII fast path
/// to avoid heap allocations when both the query and file name are ASCII.
pub(crate) struct QueryMatcher<'a> {
    query: &'a str,
    needle: &'a [u8],
    is_ascii: bool,
    lower_query: String,
}

impl<'a> QueryMatcher<'a> {
    pub(crate) fn new(query: &'a str) -> Self {
        Self {
            query,
            needle: query.as_bytes(),
            is_ascii: query.is_ascii(),
            lower_query: query.to_lowercase(),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    #[inline]
    pub(crate) fn matches(&self, name: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        if self.is_ascii && name.is_ascii() {
            if name.len() < self.needle.len() {
                return false;
            }
            return name
                .as_bytes()
                .windows(self.needle.len())
                .any(|w| w.eq_ignore_ascii_case(self.needle));
        }
        name.to_lowercase().contains(&self.lower_query)
    }
}

/// Case-insensitive basename substring match; empty query matches everything.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn name_matches(name: &str, query: &str) -> bool {
    QueryMatcher::new(query).matches(name)
}

/// Case-insensitive prefix matcher for type-select jumps with an ASCII fast path.
pub(crate) struct PrefixMatcher<'a> {
    prefix: &'a str,
    needle: &'a [u8],
    lower_prefix: String,
}

impl<'a> PrefixMatcher<'a> {
    pub(crate) fn new(prefix: &'a str) -> Self {
        Self {
            prefix,
            needle: prefix.as_bytes(),
            lower_prefix: prefix.to_lowercase(),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.prefix.is_empty()
    }

    #[inline]
    pub(crate) fn matches(&self, name: &str) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.prefix.is_ascii() && name.is_ascii() {
            if name.len() < self.needle.len() {
                return false;
            }
            return name.as_bytes()[..self.needle.len()].eq_ignore_ascii_case(self.needle);
        }
        name.to_lowercase().starts_with(&self.lower_prefix)
    }
}

/// Case-insensitive starts_with with the same ASCII fast path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn name_has_prefix(name: &str, prefix: &str) -> bool {
    PrefixMatcher::new(prefix).matches(name)
}

/// Visible index of the first name starting with `prefix`, scanning from
/// `from` and wrapping to the top; `None` when nothing matches.
pub(crate) fn first_prefix_match(names: &[&str], from: usize, prefix: &str) -> Option<usize> {
    if names.is_empty() || prefix.is_empty() {
        return None;
    }
    let matcher = PrefixMatcher::new(prefix);
    let start = from % names.len();
    (0..names.len())
        .map(|offset| (start + offset) % names.len())
        .find(|&index| matcher.matches(names[index]))
}

/// Indices into `entries` that should appear in the local file list.
pub(crate) fn visible_local_indices(
    entries: &[LocalEntry],
    show_hidden: bool,
    query: &str,
) -> Vec<usize> {
    visible_indices(
        entries.len(),
        entries.iter().map(|entry| entry.name.as_str()),
        show_hidden,
        query,
    )
}

/// Indices into `entries` that should appear in the remote file list.
pub(crate) fn visible_remote_indices(
    entries: &[RemoteEntry],
    show_hidden: bool,
    query: &str,
) -> Vec<usize> {
    visible_indices(
        entries.len(),
        entries.iter().map(|entry| entry.name.as_str()),
        show_hidden,
        query,
    )
}

/// Count of entries that should appear in the local file list without allocating.
pub(crate) fn count_visible_local(
    entries: &[LocalEntry],
    show_hidden: bool,
    query: &str,
) -> usize {
    count_visible(
        entries.len(),
        entries.iter().map(|entry| entry.name.as_str()),
        show_hidden,
        query,
    )
}

/// Count of entries that should appear in the remote file list without allocating.
pub(crate) fn count_visible_remote(
    entries: &[RemoteEntry],
    show_hidden: bool,
    query: &str,
) -> usize {
    count_visible(
        entries.len(),
        entries.iter().map(|entry| entry.name.as_str()),
        show_hidden,
        query,
    )
}

fn count_visible<'a>(
    total_count: usize,
    names: impl Iterator<Item = &'a str>,
    show_hidden: bool,
    query: &str,
) -> usize {
    let matcher = QueryMatcher::new(query);
    if matcher.is_empty() {
        if show_hidden {
            return total_count;
        }
        return names.filter(|name| !is_dotfile(name)).count();
    }
    names
        .filter(|name| (show_hidden || !is_dotfile(name)) && matcher.matches(name))
        .count()
}

/// Stored entry index at `visible_index` in the local list without allocating a vector.
pub(crate) fn real_local_index_at(
    entries: &[LocalEntry],
    visible_index: usize,
    show_hidden: bool,
    query: &str,
) -> Option<usize> {
    real_index_at(
        entries.len(),
        entries.iter().map(|entry| entry.name.as_str()),
        visible_index,
        show_hidden,
        query,
    )
}

/// Stored entry index at `visible_index` in the remote list without allocating a vector.
pub(crate) fn real_remote_index_at(
    entries: &[RemoteEntry],
    visible_index: usize,
    show_hidden: bool,
    query: &str,
) -> Option<usize> {
    real_index_at(
        entries.len(),
        entries.iter().map(|entry| entry.name.as_str()),
        visible_index,
        show_hidden,
        query,
    )
}

fn real_index_at<'a>(
    total_count: usize,
    names: impl Iterator<Item = &'a str>,
    visible_index: usize,
    show_hidden: bool,
    query: &str,
) -> Option<usize> {
    let matcher = QueryMatcher::new(query);
    if matcher.is_empty() {
        if show_hidden {
            return (visible_index < total_count).then_some(visible_index);
        }
        return names
            .enumerate()
            .filter(|(_, name)| !is_dotfile(name))
            .map(|(index, _)| index)
            .nth(visible_index);
    }
    names
        .enumerate()
        .filter(|(_, name)| (show_hidden || !is_dotfile(name)) && matcher.matches(name))
        .map(|(index, _)| index)
        .nth(visible_index)
}

fn visible_indices<'a>(
    total_count: usize,
    names: impl Iterator<Item = &'a str>,
    show_hidden: bool,
    query: &str,
) -> Vec<usize> {
    let matcher = QueryMatcher::new(query);
    if matcher.is_empty() {
        if show_hidden {
            return (0..total_count).collect();
        }
        let mut result = Vec::with_capacity(total_count);
        for (index, name) in names.enumerate() {
            if !is_dotfile(name) {
                result.push(index);
            }
        }
        return result;
    }

    let mut result = Vec::new();
    for (index, name) in names.enumerate() {
        if !show_hidden && is_dotfile(name) {
            continue;
        }
        if matcher.matches(name) {
            result.push(index);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use macsftp_core::{FileKind, LocalEntry, LocalPath, RemoteEntry, RemotePath};

    use super::{
        first_prefix_match, is_dotfile, name_has_prefix, name_matches, visible_local_indices,
        visible_remote_indices,
    };

    #[test]
    fn filter_query_case_insensitive_substring() {
        assert!(name_matches("ReadMe.TXT", "me.t"));
        assert!(!name_matches("ReadMe.TXT", "xyz"));
        assert!(name_matches("ReadMe.TXT", ""));
    }

    #[test]
    fn prefix_matching_is_case_insensitive_and_needs_a_prefix() {
        assert!(name_has_prefix("ReadMe.md", "read"));
        assert!(name_has_prefix("readme.md", "R"));
        assert!(!name_has_prefix("readme.md", "e"));
        assert!(!name_has_prefix("readme.md", ""));
    }

    #[test]
    fn first_prefix_match_scans_forward_and_wraps() {
        let names = ["alpha", "beta", "brand", "gamma"];
        assert_eq!(first_prefix_match(&names, 0, "b"), Some(1));
        assert_eq!(
            first_prefix_match(&names, 2, "b"),
            Some(2),
            "starts scanning at the current offset"
        );
        assert_eq!(
            first_prefix_match(&names, 3, "a"),
            Some(0),
            "wraps back to the top"
        );
        assert_eq!(first_prefix_match(&names, 0, "BR"), Some(2));
        assert_eq!(first_prefix_match(&names, 0, "zzz"), None);
        assert_eq!(first_prefix_match(&[], 0, "a"), None);
        assert_eq!(first_prefix_match(&names, 0, ""), None);
    }

    fn local(name: &str) -> LocalEntry {
        LocalEntry {
            name: name.to_string(),
            path: LocalPath::new(format!("/tmp/{name}")),
            kind: FileKind::File,
            size: None,
            permissions: None,
            modified_at: None,
            link_target: None,
        }
    }

    fn remote(name: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.to_string(),
            path: RemotePath::new(format!("/home/{name}")),
            kind: FileKind::File,
            size: None,
            permissions: None,
            modified_at: None,
            link_target: None,
        }
    }

    #[test]
    fn is_dotfile_matches_leading_dot_only() {
        assert!(is_dotfile(".secret"));
        assert!(is_dotfile("."));
        assert!(is_dotfile(".."));
        assert!(!is_dotfile("visible.txt"));
        assert!(!is_dotfile("a.b"));
    }

    #[test]
    fn hides_dotfiles_when_show_hidden_is_false() {
        let entries = vec![local(".secret"), local("visible.txt"), local(".config")];
        assert_eq!(
            visible_local_indices(&entries, false, ""),
            vec![1],
            "only non-dotfile remains"
        );
        assert_eq!(
            visible_local_indices(&entries, true, ""),
            vec![0, 1, 2],
            "all entries when show_hidden"
        );
    }

    #[test]
    fn empty_query_does_not_filter_by_name() {
        let entries = vec![remote("alpha"), remote("beta")];
        assert_eq!(visible_remote_indices(&entries, true, ""), vec![0, 1]);
    }

    #[test]
    fn non_empty_query_is_case_insensitive_substring() {
        let entries = vec![local(".hidden"), local("ReadMe.md"), local("notes.txt")];
        assert_eq!(
            visible_local_indices(&entries, false, "read"),
            vec![1],
            "hidden still excluded; case-insensitive match"
        );
        assert_eq!(
            visible_local_indices(&entries, true, "E"),
            vec![0, 1, 2],
            "substring hits every name that contains e/E"
        );
        assert_eq!(
            visible_local_indices(&entries, true, "zzz"),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn visible_indices_handle_ten_thousand_entries() {
        let entries: Vec<LocalEntry> = (0..10_000)
            .map(|i| LocalEntry {
                name: format!("file-{i:05}.txt"),
                path: LocalPath::new(format!("/tmp/bulk/file-{i:05}.txt")),
                kind: FileKind::File,
                size: Some(i as u64),
                permissions: None,
                modified_at: None,
                link_target: None,
            })
            .collect();
        let all = visible_local_indices(&entries, true, "");
        assert_eq!(all.len(), 10_000);

        let filtered = visible_local_indices(&entries, true, "file-099");
        assert!(
            !filtered.is_empty() && filtered.len() < 10_000,
            "substring filter must reduce 10k set"
        );
        // Correctness only — do not assert elapsed time (flaky on CI)
    }

    #[test]
    fn visible_remote_indices_handle_ten_thousand_with_hidden() {
        let entries: Vec<RemoteEntry> = (0..10_000)
            .map(|i| RemoteEntry {
                name: if i % 50 == 0 {
                    format!(".hidden-{i}")
                } else {
                    format!("entry-{i}")
                },
                path: RemotePath::new(format!("/data/{i}")),
                kind: FileKind::File,
                size: None,
                permissions: None,
                modified_at: None,
                link_target: None,
            })
            .collect();
        let shown = visible_remote_indices(&entries, false, "");
        assert_eq!(shown.len(), 10_000 - (10_000 / 50));
        let _ = visible_remote_indices(&entries, true, "entry-1");
    }

    #[test]
    fn count_and_real_index_at_match_visible_indices() {
        use super::{
            count_visible_local, count_visible_remote, real_local_index_at, real_remote_index_at,
        };

        let entries = vec![
            local(".secret"),
            local("alpha.txt"),
            local("ReadMe.md"),
            local(".config"),
            local("zebra.txt"),
        ];

        for show_hidden in [false, true] {
            for query in ["", "read", "z", "e", "nonexistent"] {
                let full = visible_local_indices(&entries, show_hidden, query);
                let count = count_visible_local(&entries, show_hidden, query);
                assert_eq!(full.len(), count, "count mismatch for query '{query}', show_hidden {show_hidden}");

                for (visible_idx, &expected_real) in full.iter().enumerate() {
                    let real = real_local_index_at(&entries, visible_idx, show_hidden, query);
                    assert_eq!(
                        real,
                        Some(expected_real),
                        "real index mismatch at visible {visible_idx} for query '{query}'"
                    );
                }
                assert_eq!(
                    real_local_index_at(&entries, full.len(), show_hidden, query),
                    None
                );
            }
        }

        let remote_entries = vec![
            remote(".hidden"),
            remote("file1.dat"),
            remote("file2.dat"),
        ];
        assert_eq!(count_visible_remote(&remote_entries, false, ""), 2);
        assert_eq!(count_visible_remote(&remote_entries, true, ""), 3);
        assert_eq!(real_remote_index_at(&remote_entries, 0, false, ""), Some(1));
        assert_eq!(real_remote_index_at(&remote_entries, 1, false, ""), Some(2));
        assert_eq!(real_remote_index_at(&remote_entries, 2, false, ""), None);
    }
}
