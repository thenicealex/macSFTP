//! Derive the visible file-list index space from stored entries.
//!
//! Stored `TabState` listings stay complete and sorted; the UI and keyboard
//! navigation map through these helpers so hiding dotfiles (and later
//! type-to-filter) never mutates the underlying snapshot.

use macsftp_core::{LocalEntry, RemoteEntry};

/// Dotfile rule for macSFTP: basename starts with `.`.
pub(crate) fn is_dotfile(name: &str) -> bool {
    name.starts_with('.')
}

/// Case-insensitive basename substring match; empty query matches everything.
pub(crate) fn name_matches(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

/// Indices into `entries` that should appear in the local file list.
pub(crate) fn visible_local_indices(
    entries: &[LocalEntry],
    show_hidden: bool,
    query: &str,
) -> Vec<usize> {
    visible_indices(
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
        entries.iter().map(|entry| entry.name.as_str()),
        show_hidden,
        query,
    )
}

fn visible_indices<'a>(
    names: impl Iterator<Item = &'a str>,
    show_hidden: bool,
    query: &str,
) -> Vec<usize> {
    names
        .enumerate()
        .filter(|(_, name)| show_hidden || !is_dotfile(name))
        .filter(|(_, name)| name_matches(name, query))
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use macsftp_core::{FileKind, LocalEntry, LocalPath, RemoteEntry, RemotePath};

    use super::{is_dotfile, name_matches, visible_local_indices, visible_remote_indices};

    #[test]
    fn filter_query_case_insensitive_substring() {
        assert!(name_matches("ReadMe.TXT", "me.t"));
        assert!(!name_matches("ReadMe.TXT", "xyz"));
        assert!(name_matches("ReadMe.TXT", ""));
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
}
