// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aterm_agent::harness::footer;

use super::{Asks, Identity, LaunchFacts, LogWake, NO_TAP, RosterMemo};
use crate::session_log::transcript::Applied;
use crate::session_timeline::SessionTimeline;

/// Default session UUID written into test registry fixtures.
pub(crate) const SESSION_ID: &str = "00000000-0000-4000-8000-000000000a01";

/// Returns a hermetic test home directory under `test_root`.
pub(crate) fn scratch_home() -> Option<PathBuf> {
    aterm_types::dirs::test_root().map(|root| root.join("home"))
}

/// Returns a short temporary directory path for test fixture `name`.
pub(crate) fn home_root(name: &str) -> PathBuf {
    crate::test_scratch::short_process_root("aterm-cf-").join(name)
}

/// Creates a scratch Claude home directory and session registry file for `pid`.
pub(crate) fn claude_home(name: &str, pid: i32, started_at: u64) -> (PathBuf, PathBuf) {
    let root = home_root(name);
    let _ = std::fs::remove_dir_all(&root);
    claude_home_at(root, pid, started_at)
}

/// Creates a Claude registry fixture within a caller-owned root.
pub(crate) fn claude_home_at(root: PathBuf, pid: i32, started_at: u64) -> (PathBuf, PathBuf) {
    let (cwd, claude) = (root.join("work"), root.join("claude"));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    std::fs::write(
        claude.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION_ID}","cwd":{},"startedAt":{},"version":"2.1.287"}}"#,
            aterm_json::to_string(&cwd.display().to_string()).unwrap(),
            started_at * 1000 + 684
        ),
    )
    .unwrap();
    (root, claude)
}

/// Returns the transcript JSONL path for a [`claude_home`] fixture.
pub(crate) fn transcript_of(root: &Path, claude: &Path) -> PathBuf {
    let dir = claude
        .join("projects")
        .join(footer::project_slug(&root.join("work")));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{SESSION_ID}.jsonl"))
}

/// Executes a single synchronous footer-fact read for `pid` into `timeline`.
pub(crate) fn read(
    timeline: &Arc<Mutex<SessionTimeline>>,
    pid: i32,
    claude: &Path,
    fold: &mut footer::FooterCache,
    tail: &mut footer::TailCache,
    session: u64,
    wake: &LogWake,
) -> Applied {
    let mut identity = Identity {
        pid,
        started: None,
        dir: Some(claude.to_path_buf()),
        launch: LaunchFacts::default(),
        launch_for: None,
        login: None,
        argv: Vec::new(),
        task: None,
        login_checked_for: None,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        unoffered: None,
        env_stamp: None,
        first_seen: None,
    };
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| {
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec!["claude".into()],
                env: vec!["HOME=/home/someone".into()],
                ..Default::default()
            })
        },
        roster: || None,
        home: scratch_home,
        route: |_, _, _| aterm_agent::harness::accounts::AuthRoute::Stored,
    };
    let (_, _, applied) = super::resolve_into(
        timeline,
        &mut identity,
        tail,
        fold,
        &mut RosterMemo::default(),
        asks,
    );
    wake.after(session, &applied);
    applied
}
