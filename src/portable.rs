//! Home-directory portability for multi-machine sync.
//!
//! Claude Code stores conversations under `~/.claude/projects/<encoded-cwd>/`, where the
//! encoded name is the project's absolute path with `/` replaced by `-`. The absolute path
//! also appears inside the session files themselves (`cwd`, tool inputs, prose).
//!
//! That makes the sync repository machine-specific: a project at `/Users/alice/work` on one
//! machine and `/root/work` on another are stored as two unrelated projects, and resuming a
//! pulled session points at a directory that does not exist.
//!
//! This module replaces the *home prefix* with a placeholder on the way into the repository
//! and expands it back to the local home on the way out. Only the home prefix is touched, so
//! everything below it — including nested worktrees — keeps its full structure and stays
//! unambiguous, unlike collapsing a project to its last path segment.
//!
//! The local home is read at runtime, so moving to a machine with a different home needs no
//! code change and no configuration.

use std::path::PathBuf;

/// Placeholder for the home directory inside session and artifact content.
pub const HOME_TOKEN: &str = "{{CLAUDE_SYNC_HOME}}";

/// Placeholder for the home in its *encoded* form, as Claude Code names project directories.
///
/// Transcripts quote those names whenever they reference `~/.claude/projects/...`, and on
/// another machine the quoted directory genuinely has a different name, so leaving it alone
/// would point at a path that does not exist there.
pub const HOME_ENC_TOKEN: &str = "{{CLAUDE_SYNC_HOME_ENC}}";

/// Placeholder for the home directory in encoded project directory names.
///
/// Encoded paths always start with `-` (the leading `/` of an absolute path), so a name
/// starting with `HOME` cannot collide with a real encoded path.
pub const HOME_DIR_TOKEN: &str = "HOME";

/// Local home directory.
///
/// `CLAUDE_CODE_SYNC_HOME` overrides it for tests, mirroring `CLAUDE_CODE_SYNC_CLAUDE_DIR`:
/// faking `HOME` cannot redirect `dirs::home_dir()` on Windows.
///
/// Note that this feature assumes POSIX-style absolute paths. On Windows a home like
/// `C:\\Users\\alice` neither matches the backslash-escaped form serde writes into the
/// JSONL nor encodes to Claude's directory name, so `portable_home` should stay off there.
pub fn local_home() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CODE_SYNC_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    dirs::home_dir()
}

fn local_home_str() -> Option<String> {
    let home = local_home()?;
    let s = home.to_string_lossy().trim_end_matches('/').to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// The local home as Claude Code would encode it: `/Users/alice` -> `-Users-alice`.
fn encoded_local_home() -> Option<String> {
    Some(local_home_str()?.replace(['/', '\\'], "-"))
}

/// True when `next` cannot continue a path segment, so an occurrence of the home ends here.
///
/// Without this, a home of `/Users/alice` would also match `/Users/alice-backup/notes.md`
/// and turn it into `<home>-backup/notes.md`, which expands to a different directory on the
/// receiving machine — silent path corruption that round-trips cleanly on the origin.
fn ends_segment(next: Option<char>) -> bool {
    match next {
        None => true,
        Some(c) => !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'),
    }
}

/// True when `prev` cannot be the tail of a longer path, so an occurrence of the home may
/// begin here.
///
/// This matters most for short homes. With `/root`, the path `/var/lib/docker/root` and the
/// CLI flag `--root` both contain the needle; without a leading boundary they would be
/// tokenized and expand on the other machine to `/var/lib/docker/<other-home>` and
/// `-<other-encoded-home>`. A preceding `/` is rejected for the plain form (`…docker//root`
/// is never the home) but accepted for the encoded form, which legitimately follows one in
/// `projects/-Users-alice-work`. A preceding `~` is rejected in both: `~/root` is a path
/// *relative* to whatever home the reader has, so it is already portable as written.
fn starts_segment(prev: Option<char>, allow_slash: bool) -> bool {
    match prev {
        None => true,
        Some('/') => allow_slash,
        Some('~') => false,
        Some(c) => !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.'),
    }
}

/// Replace every whole-segment occurrence of `needle` with `token`.
fn replace_bounded(content: &str, needle: &str, token: &str, encoded_form: bool) -> String {
    let mut out = String::with_capacity(content.len());
    // Indices into the ORIGINAL string, so `prev` is the true preceding character even when
    // one occurrence directly follows another. Re-slicing on each iteration would make a
    // second `/root` in `/root/root/work` look like the start of the string and tokenize it,
    // expanding on the other machine to `<home><home>/work`.
    let mut pos = 0;
    while let Some(rel) = content[pos..].find(needle) {
        let start = pos + rel;
        let end = start + needle.len();
        let mut before = content[..start].chars().rev();
        let mut prev = before.next();
        // Inside JSON-serialized strings a newline is the two-character escape `\n`, so a
        // path at the start of an embedded line is preceded by the letter `n`. Treat the
        // escapes `\n`, `\t` and `\r` as the whitespace they represent.
        if matches!(prev, Some('n' | 't' | 'r')) && before.next() == Some('\\') {
            prev = Some('\n');
        }
        let next = content[end..].chars().next();
        out.push_str(&content[pos..start]);
        // In the encoded form `-` is the separator, so it also terminates an occurrence.
        let ends = ends_segment(next) || (encoded_form && next == Some('-'));
        if ends && starts_segment(prev, encoded_form) {
            out.push_str(token);
        } else {
            out.push_str(needle);
        }
        pos = end;
    }
    out.push_str(&content[pos..]);
    out
}

/// Replace the local home with a placeholder, in both the forms it appears in content:
/// the plain path (`/Users/alice/work`) and Claude's encoded directory name
/// (`-Users-alice-work`, which transcripts quote whenever they reference
/// `~/.claude/projects/`). No-op when the home is unknown.
pub fn to_portable(content: &str) -> String {
    let Some(home) = local_home_str() else {
        return content.to_string();
    };
    let out = replace_bounded(content, &home, HOME_TOKEN, false);

    match encoded_local_home() {
        Some(encoded) => replace_bounded(&out, &encoded, HOME_ENC_TOKEN, true),
        None => out,
    }
}

/// Expand both placeholders back to the local home. No-op when the home is unknown.
pub fn from_portable(content: &str) -> String {
    let Some(home) = local_home_str() else {
        return content.to_string();
    };
    let out = content.replace(HOME_TOKEN, &home);
    match encoded_local_home() {
        Some(encoded) => out.replace(HOME_ENC_TOKEN, &encoded),
        None => out,
    }
}

/// True when the content carries a placeholder that needs expanding.
pub fn has_token(content: &str) -> bool {
    content.contains(HOME_TOKEN) || content.contains(HOME_ENC_TOKEN)
}

/// Marker at the root of a sync repository whose contents are stored in portable form.
///
/// The mode has to be a property of the repository, not of each machine's config: a machine
/// that has not enabled `portable_home` would otherwise push absolute paths into a portable
/// repository and pull placeholder-named directories out of it, and the two machines would
/// rewrite each other's copies on every sync. With the marker, whoever finds it follows it.
pub const MARKER_FILE: &str = ".portable-home";

/// True when this repository stores paths in portable form.
pub fn repo_is_portable(repo_root: &std::path::Path) -> bool {
    repo_root.join(MARKER_FILE).is_file()
}

/// Record that this repository stores paths in portable form. Idempotent.
pub fn mark_repo_portable(repo_root: &std::path::Path) -> std::io::Result<()> {
    let marker = repo_root.join(MARKER_FILE);
    if marker.is_file() {
        return Ok(());
    }
    std::fs::write(
        marker,
        format!(
            "Home directories in this repository are stored as {HOME_TOKEN}.\n\
             Managed by claude-code-sync; do not delete while any machine still syncs here.\n"
        ),
    )
}

/// Walk up from `path` looking for a repository marker.
///
/// Lets any writer that lands inside a portable repository canonicalize automatically,
/// instead of relying on every call site remembering to pick the portable variant.
pub fn is_inside_portable_repo(path: &std::path::Path) -> bool {
    path.ancestors().skip(1).any(repo_is_portable)
}

fn replace_prefix(name: &str, prefix: &str, replacement: &str) -> Option<String> {
    if name == prefix {
        return Some(replacement.to_string());
    }
    let rest = name.strip_prefix(prefix)?;
    if rest.starts_with('-') {
        Some(format!("{replacement}{rest}"))
    } else {
        None
    }
}

/// `-Users-alice-work` -> `HOME-work`. Names outside the home are left untouched.
pub fn encode_project_dir(name: &str) -> String {
    encoded_local_home()
        .and_then(|home| replace_prefix(name, &home, HOME_DIR_TOKEN))
        .unwrap_or_else(|| name.to_string())
}

/// `HOME-work` -> `-Users-alice-work`. Names without the placeholder are left untouched.
pub fn decode_project_dir(name: &str) -> String {
    encoded_local_home()
        .and_then(|home| replace_prefix(name, HOME_DIR_TOKEN, &home))
        .unwrap_or_else(|| name.to_string())
}

/// Apply [`encode_project_dir`] to the first component of a repo-relative session path.
pub fn encode_relative_path(relative: &std::path::Path) -> PathBuf {
    map_first_component(relative, encode_project_dir)
}

/// Apply [`decode_project_dir`] to the first component of a repo-relative session path.
pub fn decode_relative_path(relative: &std::path::Path) -> PathBuf {
    map_first_component(relative, decode_project_dir)
}

fn map_first_component(relative: &std::path::Path, f: fn(&str) -> String) -> PathBuf {
    let mut components = relative.components();
    let first = match components.next() {
        Some(c) => c,
        None => return relative.to_path_buf(),
    };
    let first_str = match first.as_os_str().to_str() {
        Some(s) => s,
        None => return relative.to_path_buf(),
    };
    let mut out = PathBuf::from(f(first_str));
    out.extend(components);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::Path;

    struct HomeGuard;

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            std::env::remove_var("CLAUDE_CODE_SYNC_HOME");
        }
    }

    fn with_home(home: &str) -> HomeGuard {
        std::env::set_var("CLAUDE_CODE_SYNC_HOME", home);
        HomeGuard
    }

    #[test]
    #[serial]
    fn encodes_and_decodes_project_dir_round_trip() {
        let _g = with_home("/Users/alice");
        assert_eq!(encode_project_dir("-Users-alice-work"), "HOME-work");
        assert_eq!(decode_project_dir("HOME-work"), "-Users-alice-work");
    }

    #[test]
    #[serial]
    fn round_trip_is_stable_across_different_homes() {
        let encoded = {
            let _g = with_home("/Users/alice");
            encode_project_dir("-Users-alice-code-proj-wt-feature")
        };
        assert_eq!(encoded, "HOME-code-proj-wt-feature");

        let _g = with_home("/root");
        assert_eq!(decode_project_dir(&encoded), "-root-code-proj-wt-feature");
    }

    #[test]
    #[serial]
    fn leaves_paths_outside_home_untouched() {
        let _g = with_home("/Users/alice");
        assert_eq!(encode_project_dir("-srv-shared-proj"), "-srv-shared-proj");
        assert_eq!(decode_project_dir("-srv-shared-proj"), "-srv-shared-proj");
    }

    #[test]
    #[serial]
    fn does_not_split_a_longer_sibling_name() {
        // /Users/alice-backup must not be mistaken for a path under /Users/alice.
        let _g = with_home("/Users/alice");
        assert_eq!(
            encode_project_dir("-Users-alice-backup-proj"),
            "HOME-backup-proj"
        );
        assert_eq!(
            encode_project_dir("-Users-alicexbackup"),
            "-Users-alicexbackup"
        );
    }

    #[test]
    #[serial]
    fn encodes_the_home_root_itself() {
        let _g = with_home("/Users/alice");
        assert_eq!(encode_project_dir("-Users-alice"), "HOME");
        assert_eq!(decode_project_dir("HOME"), "-Users-alice");
    }

    #[test]
    #[serial]
    fn content_round_trips_between_machines() {
        let portable = {
            let _g = with_home("/Users/alice");
            to_portable(r#"{"cwd":"/Users/alice/work","x":"/Users/alice/work/f.rs"}"#)
        };
        assert!(has_token(&portable));
        assert!(!portable.contains("/Users/alice"));

        let _g = with_home("/root");
        assert_eq!(
            from_portable(&portable),
            r#"{"cwd":"/root/work","x":"/root/work/f.rs"}"#
        );
    }

    #[test]
    #[serial]
    fn does_not_tokenize_a_sibling_home_in_content() {
        let _g = with_home("/Users/alice");
        // /Users/alice-backup is a different directory, not a path under /Users/alice.
        let text = r#"{"a":"/Users/alice/work","b":"/Users/alice-backup/notes.md"}"#;
        let portable = to_portable(text);
        assert!(portable.contains("/Users/alice-backup/notes.md"));
        assert_eq!(
            portable,
            format!(r#"{{"a":"{HOME_TOKEN}/work","b":"/Users/alice-backup/notes.md"}}"#)
        );
    }

    #[test]
    #[serial]
    fn tokenizes_the_encoded_project_directory_name_too() {
        let portable = {
            let _g = with_home("/Users/alice");
            to_portable("see /Users/alice/.claude/projects/-Users-alice-work/s.jsonl")
        };
        assert!(!portable.contains("/Users/alice"));
        assert!(!portable.contains("-Users-alice"));

        // On the receiving machine both forms name the directory that actually exists there.
        let _g = with_home("/root");
        assert_eq!(
            from_portable(&portable),
            "see /root/.claude/projects/-root-work/s.jsonl"
        );
    }

    #[test]
    #[serial]
    fn short_home_needs_a_leading_boundary() {
        // The server case: with home `/root`, unrelated content contains the needle.
        let _g = with_home("/root");
        assert_eq!(to_portable("/var/lib/docker/root"), "/var/lib/docker/root");
        assert_eq!(
            to_portable("cargo run --root file"),
            "cargo run --root file"
        );
        assert_eq!(to_portable("the web-root dir"), "the web-root dir");
        // While genuine occurrences still tokenize.
        assert_eq!(
            to_portable(r#"{"cwd":"/root/portalhero"}"#),
            format!(r#"{{"cwd":"{HOME_TOKEN}/portalhero"}}"#)
        );
        assert_eq!(
            to_portable("projects/-root-portalhero/s.jsonl"),
            format!("projects/{HOME_ENC_TOKEN}-portalhero/s.jsonl")
        );
    }

    #[test]
    #[serial]
    fn adjacent_occurrences_keep_their_true_predecessor() {
        let _g = with_home("/root");
        // /root/root/work is the home plus a subdirectory named root: only the first
        // occurrence is the home.
        assert_eq!(
            to_portable("/root/root/work"),
            format!("{HOME_TOKEN}/root/work")
        );
        // Neither occurrence here is the home.
        assert_eq!(
            to_portable("/var/lib/docker/root/root"),
            "/var/lib/docker/root/root"
        );
        assert_eq!(to_portable("x-root-root"), "x-root-root");
    }

    #[test]
    #[serial]
    fn json_escaped_newlines_count_as_boundaries() {
        let _g = with_home("/Users/alice");
        // Inside a JSON string a newline is the escape `\n`; the path after it must still
        // tokenize even though the raw predecessor is the letter `n`.
        assert_eq!(
            to_portable(r#"{"text":"---\n/Users/alice/work/f.vue"}"#),
            format!(r#"{{"text":"---\n{HOME_TOKEN}/work/f.vue"}}"#)
        );
        // A word genuinely ending in `n` is still no boundary.
        let _g2 = with_home("/root");
        assert_eq!(to_portable("chaperon/root"), "chaperon/root");
    }

    #[test]
    #[serial]
    fn tilde_prefixed_paths_are_already_portable() {
        let _g = with_home("/root");
        assert_eq!(to_portable("see ~/root/notes.md"), "see ~/root/notes.md");
    }

    #[test]
    #[serial]
    fn does_not_tokenize_a_longer_encoded_sibling() {
        let _g = with_home("/Users/alice");
        assert_eq!(to_portable("-Users-alicexyz-work"), "-Users-alicexyz-work");
    }

    #[test]
    #[serial]
    fn tokenizes_the_home_at_the_end_of_a_value() {
        let _g = with_home("/Users/alice");
        assert_eq!(
            to_portable(r#"{"cwd":"/Users/alice"}"#),
            format!(r#"{{"cwd":"{HOME_TOKEN}"}}"#)
        );
    }

    #[test]
    #[serial]
    fn marker_makes_the_repository_authoritative() {
        let repo = tempfile::tempdir().unwrap();
        assert!(!repo_is_portable(repo.path()));
        mark_repo_portable(repo.path()).unwrap();
        assert!(repo_is_portable(repo.path()));
        mark_repo_portable(repo.path()).unwrap();

        let nested = repo.path().join("projects").join("HOME-work");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(is_inside_portable_repo(&nested.join("s.jsonl")));

        let outside = tempfile::tempdir().unwrap();
        assert!(!is_inside_portable_repo(&outside.path().join("s.jsonl")));
    }

    #[test]
    #[serial]
    fn expanding_content_without_a_token_is_a_no_op() {
        let _g = with_home("/Users/alice");
        let plain = r#"{"cwd":"/Users/alice/work"}"#;
        assert_eq!(from_portable(plain), plain);
    }

    #[test]
    #[serial]
    fn maps_only_the_first_path_component() {
        let _g = with_home("/Users/alice");
        let encoded = encode_relative_path(Path::new("-Users-alice-work/session.jsonl"));
        assert_eq!(encoded, Path::new("HOME-work/session.jsonl"));
        let decoded = decode_relative_path(&encoded);
        assert_eq!(decoded, Path::new("-Users-alice-work/session.jsonl"));
    }
}
