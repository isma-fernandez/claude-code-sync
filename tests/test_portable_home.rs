//! End-to-end check for `portable_home`: a project pushed from a machine whose home is
//! `/Users/alice` must land on a machine whose home is `/root` as the same project, with
//! the paths inside the transcript rewritten to the second machine's home.
//!
//! Without this, syncing between machines requires replicating one machine's home layout
//! on the other, and resuming a pulled session points at a directory that does not exist.

use std::fs;
use std::io::Write;
use std::path::Path;

use claude_code_sync::filter::FilterConfig;
use claude_code_sync::parser::ConversationSession;
use claude_code_sync::portable;
use claude_code_sync::sync::discovery::discover_sessions;
use claude_code_sync::sync::push::plan_push;
use serial_test::serial;
use tempfile::TempDir;

const SESSION_ID: &str = "3f1c9a2e-0000-4000-8000-000000000001";

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

fn portable_filter() -> FilterConfig {
    FilterConfig {
        portable_home: true,
        ..FilterConfig::default()
    }
}

/// Seed one session whose project lives at `<home>/portalhero/wt-feature`.
fn seed_session(claude_projects: &Path, home: &str) -> String {
    let cwd = format!("{home}/portalhero/wt-feature");
    let encoded = cwd.replace('/', "-");
    let project = claude_projects.join(&encoded);
    fs::create_dir_all(&project).unwrap();

    let mut file = fs::File::create(project.join(format!("{SESSION_ID}.jsonl"))).unwrap();
    writeln!(
        file,
        r#"{{"type":"user","sessionId":"{SESSION_ID}","uuid":"u-1","timestamp":"2025-01-01T00:00:00Z","cwd":"{cwd}","message":{{"text":"open {cwd}/src/main.rs"}}}}"#
    )
    .unwrap();
    encoded
}

/// Push from `home`, returning the repo-relative paths written.
fn push_to_repo(
    claude_projects: &Path,
    repo_projects: &Path,
    filter: &FilterConfig,
) -> Vec<String> {
    let sessions = discover_sessions(claude_projects, filter).unwrap();
    let plan = plan_push(&sessions, claude_projects, repo_projects, filter).unwrap();
    let mut written = Vec::new();
    for entry in &plan.entries {
        let dest = repo_projects.join(&entry.relative_path);
        sessions[entry.session_index]
            .write_to_file_portable(&dest)
            .unwrap();
        written.push(entry.relative_path.to_string_lossy().to_string());
    }
    written
}

#[test]
#[serial]
fn repo_stores_the_home_as_a_placeholder() {
    let _g = with_home("/Users/alice");
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_session(claude.path(), "/Users/alice");

    let written = push_to_repo(claude.path(), repo.path(), &portable_filter());

    assert_eq!(
        written,
        vec!["HOME-portalhero-wt-feature/3f1c9a2e-0000-4000-8000-000000000001.jsonl"]
    );

    let stored = fs::read_to_string(repo.path().join(&written[0])).unwrap();
    assert!(
        !stored.contains("/Users/alice"),
        "the pusher's home leaked into the repo: {stored}"
    );
    assert!(stored.contains(portable::HOME_TOKEN));
    // Everything below the home survives, so sibling worktrees stay distinct.
    assert!(stored.contains("/portalhero/wt-feature/src/main.rs"));
}

#[test]
#[serial]
fn a_second_machine_pulls_it_under_its_own_home() {
    let claude_a = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();

    {
        let _g = with_home("/Users/alice");
        seed_session(claude_a.path(), "/Users/alice");
        push_to_repo(claude_a.path(), repo.path(), &portable_filter());
    }

    // Machine B: different home, no /Users/alice anywhere.
    let _g = with_home("/root");
    let claude_b = TempDir::new().unwrap();
    let filter = portable_filter();

    let remote_sessions = discover_sessions(repo.path(), &filter).unwrap();
    assert_eq!(remote_sessions.len(), 1);

    let relative = Path::new(&remote_sessions[0].file_path)
        .strip_prefix(repo.path())
        .unwrap();
    let local_relative = portable::decode_relative_path(relative);
    assert_eq!(
        local_relative,
        Path::new("-root-portalhero-wt-feature/3f1c9a2e-0000-4000-8000-000000000001.jsonl")
    );

    let dest = claude_b.path().join(&local_relative);
    remote_sessions[0].write_to_file(&dest).unwrap();

    let landed = fs::read_to_string(&dest).unwrap();
    assert!(landed.contains(r#""cwd":"/root/portalhero/wt-feature""#));
    assert!(landed.contains("/root/portalhero/wt-feature/src/main.rs"));
    assert!(!landed.contains("/Users/alice"));
    assert!(!landed.contains(portable::HOME_TOKEN));
}

#[test]
#[serial]
fn pushing_back_from_the_second_machine_reports_no_changes() {
    let claude_a = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();

    {
        let _g = with_home("/Users/alice");
        seed_session(claude_a.path(), "/Users/alice");
        push_to_repo(claude_a.path(), repo.path(), &portable_filter());
    }

    // Machine B receives it, then pushes without touching anything. If the placeholder
    // were compared against the expanded local copy, every session would look modified
    // on every sync and the two machines would fight forever.
    let _g = with_home("/root");
    let claude_b = TempDir::new().unwrap();
    seed_session(claude_b.path(), "/root");

    let filter = portable_filter();
    let sessions = discover_sessions(claude_b.path(), &filter).unwrap();
    let plan = plan_push(&sessions, claude_b.path(), repo.path(), &filter).unwrap();

    assert_eq!(plan.unchanged, 1, "expected no diff, got {plan:?}");
    assert_eq!(plan.added, 0);
    assert_eq!(plan.modified, 0);
}

#[test]
#[serial]
fn reading_a_stored_session_expands_the_placeholder() {
    let _g = with_home("/root");
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("s.jsonl");
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        r#"{{"type":"user","sessionId":"{SESSION_ID}","uuid":"u-1","cwd":"{}/work"}}"#,
        portable::HOME_TOKEN
    )
    .unwrap();

    let session = ConversationSession::from_file(&path).unwrap();
    assert_eq!(session.entries[0].cwd.as_deref(), Some("/root/work"));
}

#[test]
#[serial]
fn disabled_by_default_so_existing_repos_are_untouched() {
    let _g = with_home("/Users/alice");
    let claude = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    seed_session(claude.path(), "/Users/alice");

    let filter = FilterConfig::default();
    assert!(!filter.portable_home);

    let sessions = discover_sessions(claude.path(), &filter).unwrap();
    let plan = plan_push(&sessions, claude.path(), repo.path(), &filter).unwrap();
    let relative = plan.entries[0].relative_path.to_string_lossy().to_string();
    assert!(
        relative.starts_with("-Users-alice-portalhero-wt-feature"),
        "default mode must keep absolute encoding, got {relative}"
    );
}
