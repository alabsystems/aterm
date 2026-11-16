// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land --tab`: an optional program-worker tab for a landing.
//!
//! Run bare `verify land` with native shell/background command tools by default. Use this mode
//! only on the person's explicit request or for a concrete persistence need those tools cannot
//! meet, such as an actual restart or upgrade that will end the launching process. State that
//! reason before spawning the tab.
//!
//! The landing runs as a program worker of the aterm session that asks for it
//! (`aterm ctl spawn program=`, `aterm help workers`), in a tab of its own, so it outlives that
//! session's agent (its restart, its usage limit, its upgrade), whose shell would otherwise own
//! it. The worker runs `targo --unverified verify land` with the landing's other words and
//! `--at <commit>`, the HEAD the ask saw, at the checkout's root, so it lands that commit or
//! nothing. Its exit status concludes its task (`aterm ctl await task @<sid> since=0`), and its
//! output tail, which ends with the landing's verdict, is its result (`aterm ctl @<sid> result`).
//! Where no aterm harness would run it (a shell in no aterm session, no `aterm` to ask, or an
//! aterm that answers `ERR unsupervised:`), nothing starts and the landing runs in place, as
//! without the flag. Any other answer (another refusal, none, or one naming no sid) lands
//! nothing here, since a worker may run that this one would race.
//!
//! This file reads only `std` and `aterm_types`: aterm's live suite mounts it to drive a real
//! instance (`crates/aterm/tests/worker_spawn_live_headless.rs`), and a change here selects that
//! suite through the mount.

use std::path::Path;

/// The flag that asks for the worker.
pub const FLAG: &str = "--tab";

/// The word that names the commit a landing lands, or else lands nothing: `--at <commit>`, which
/// the ask gives its worker.
pub const AT: &str = "--at";

/// What asking for the landing's worker came to ([`start`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// The worker runs the landing: its sid.
    Worker(String),
    /// Nothing was started, for this reason: the landing runs here.
    Here(String),
    /// Any other answer, said here: a worker may have started, so nothing lands here.
    Unknown(String),
}

/// The worker's landing words: `words` (the landing's own, [`FLAG`] taken out) and
/// `--at <head>`.
fn landing(words: &[String], head: &str) -> impl Iterator<Item = String> {
    words
        .iter()
        .cloned()
        .chain([AT.to_owned(), head.to_owned()])
}

/// The words after `aterm ctl` that start the landing of `head` (the landing's `words` with
/// `--at <head>`) as a program worker in a background tab at `root`; `None` where `root` is no
/// UTF-8 path, which no request can name.
#[must_use]
pub fn spawn_words(root: &Path, words: &[String], head: &str) -> Option<Vec<String>> {
    let root = root.to_str()?;
    let mut spawn: Vec<String> = ["@self", "spawn", "program=targo"]
        .map(String::from)
        .to_vec();
    spawn.extend(
        ["--unverified", "verify", "land"]
            .into_iter()
            .map(String::from)
            .chain(landing(words, head))
            .map(|word| format!("arg={word}")),
    );
    spawn.extend([
        format!("cwd={root}"),
        "raise=false".to_owned(),
        "-".to_owned(),
    ]);
    Some(spawn)
}

/// The worker's task: its program's standard input, which the landing does not read, and the
/// first line of the request the spawn opens.
#[must_use]
pub fn task(root: &Path, words: &[String], head: &str) -> String {
    let words: String = landing(words, head)
        .map(|word| format!(" {word}"))
        .collect();
    format!(
        "targo --unverified verify land{words} at {}, started by verify land {FLAG}\n",
        root.display()
    )
}

/// Ask for the worker that lands `head`, the checkout's HEAD. `env` reads this process's
/// environment, whose `ATERM_PARENT_SESSION_ID` names the aterm session this shell runs in, the
/// one `aterm ctl @self` addresses. `ctl` runs `aterm ctl` with its words and its standard input
/// and returns what it wrote to stdout and stderr; a `NotFound` error is no `aterm` to run, so
/// nothing was asked.
pub fn start(
    root: &Path,
    words: &[String],
    head: &str,
    env: &dyn Fn(&str) -> Option<String>,
    ctl: &mut dyn FnMut(&[String], &[u8]) -> std::io::Result<(Vec<u8>, Vec<u8>)>,
) -> Start {
    if env(aterm_types::domain::ENV_PARENT_SESSION_ID).is_none_or(|sid| sid.is_empty()) {
        return Start::Here("this shell runs in no aterm session".to_owned());
    }
    let Some(args) = spawn_words(root, words, head) else {
        return Start::Here(format!(
            "{} is no UTF-8 path, which no worker's request can name",
            root.display()
        ));
    };
    let (out, err) = match ctl(&args, task(root, words, head).as_bytes()) {
        Ok(said) => said,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Start::Here(format!("no `aterm` to ask for a worker ({error})"));
        }
        Err(error) => return Start::Unknown(format!("`aterm ctl` did not answer: {error}")),
    };
    let (out, err) = (String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
    // The client says the reply's status line on stdout, or on stderr behind its `aterm-ctl: `
    // prefix (aterm 0.201.0 answers a spawn there): read both.
    let said = || {
        err.lines()
            .chain(out.lines())
            .map(|line| line.strip_prefix("aterm-ctl: ").unwrap_or(line).trim())
    };
    let sid = said()
        .find_map(|line| line.strip_prefix("OK "))
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|sid| {
            // A tab this Mac's aterm opened: the `t` form, or the legacy `s-` form an aterm from
            // before it (0.201.0) still mints.
            aterm_types::tab_id::is_tab_id(sid) || aterm_types::tab_id::is_legacy(sid)
        });
    if let Some(sid) = sid {
        return Start::Worker(sid.to_owned());
    }
    if let Some(refusal) = said().find(|line| line.starts_with("ERR unsupervised:")) {
        return Start::Here(format!("aterm answered `{refusal}`"));
    }
    Start::Unknown(said().rfind(|line| !line.is_empty()).map_or_else(
        || "`aterm ctl` said nothing".to_owned(),
        |line| format!("`aterm ctl` answered `{line}`"),
    ))
}

/// What the ask prints once its worker runs: that nothing has landed yet and the checkout is
/// the worker's until it ends, the line that waits for the landing's end however long it takes,
/// and the one that reads its verdict.
#[must_use]
pub fn said(sid: &str, root: &Path) -> [String; 3] {
    [
        format!(
            "verify land: NOT LANDED YET — the landing runs as program worker @{sid} at {}, \
             apart from this session's agent; leave that checkout's HEAD and files as they are \
             until the worker ends, and do other work in another worktree",
            root.display()
        ),
        format!("aterm ctl --timeout 0 await task @{sid} since=0"),
        format!("aterm ctl @{sid} result"),
    ]
}
