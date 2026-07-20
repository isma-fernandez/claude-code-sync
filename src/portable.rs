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

/// Placeholder for the home directory in encoded project directory names.
///
/// Encoded paths always start with `-` (the leading `/` of an absolute path), so a name
/// starting with `HOME` cannot collide with a real encoded path.
pub const HOME_DIR_TOKEN: &str = "HOME";

/// Local home directory, honoring the same test override as the rest of the crate.
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

/// Replace the local home prefix with [`HOME_TOKEN`]. No-op when the home is unknown.
pub fn to_portable(content: &str) -> String {
    match local_home_str() {
        Some(home) => content.replace(&home, HOME_TOKEN),
        None => content.to_string(),
    }
}

/// Expand [`HOME_TOKEN`] back to the local home. No-op when the home is unknown.
pub fn from_portable(content: &str) -> String {
    match local_home_str() {
        Some(home) => content.replace(HOME_TOKEN, &home),
        None => content.to_string(),
    }
}

/// True when the content carries a placeholder that needs expanding.
pub fn has_token(content: &str) -> bool {
    content.contains(HOME_TOKEN)
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
    fn encodes_and_decodes_project_dir_round_trip() {
        let _g = with_home("/Users/alice");
        assert_eq!(encode_project_dir("-Users-alice-work"), "HOME-work");
        assert_eq!(decode_project_dir("HOME-work"), "-Users-alice-work");
    }

    #[test]
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
    fn leaves_paths_outside_home_untouched() {
        let _g = with_home("/Users/alice");
        assert_eq!(encode_project_dir("-srv-shared-proj"), "-srv-shared-proj");
        assert_eq!(decode_project_dir("-srv-shared-proj"), "-srv-shared-proj");
    }

    #[test]
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
    fn encodes_the_home_root_itself() {
        let _g = with_home("/Users/alice");
        assert_eq!(encode_project_dir("-Users-alice"), "HOME");
        assert_eq!(decode_project_dir("HOME"), "-Users-alice");
    }

    #[test]
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
    fn expanding_content_without_a_token_is_a_no_op() {
        let _g = with_home("/Users/alice");
        let plain = r#"{"cwd":"/Users/alice/work"}"#;
        assert_eq!(from_portable(plain), plain);
    }

    #[test]
    fn maps_only_the_first_path_component() {
        let _g = with_home("/Users/alice");
        let encoded = encode_relative_path(Path::new("-Users-alice-work/session.jsonl"));
        assert_eq!(encoded, Path::new("HOME-work/session.jsonl"));
        let decoded = decode_relative_path(&encoded);
        assert_eq!(decoded, Path::new("-Users-alice-work/session.jsonl"));
    }
}
