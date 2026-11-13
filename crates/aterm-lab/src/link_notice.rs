// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab's record of a link it refused (`docs/DESIGN-leftover-processes-2026-10-16.md` §5.6):
//! the lab opens what its runs left in their scratch with `O_NOFOLLOW_ANY` ([`crate::sys`]), so a
//! link a process it ran planted there is refused, never followed, and a listing of a scratch
//! folder leaves a link in it out. When the lab runs in an aterm session, each such path is told
//! once to the instance hosting that session, as one `appnotice proc` line its message center
//! keeps as a `proc.lab-link` record. Nothing is told from a test: only the lab's own verbs arm
//! it ([`arm`]), and not in a lab a test binary started, which inherits the session that test
//! ran in.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// How long the instance has to take the line.
const TELL_BOUND: Duration = Duration::from_secs(2);

/// The most bytes of a path a line carries: the line stays within the 512 the instance takes.
const PATH_BYTES: usize = 360;

/// Where a refused path is told.
enum Teller {
    /// The instance hosting this lab's session, by its control socket.
    Instance(String),
    /// A test's: each line kept.
    #[cfg(test)]
    Kept(Mutex<Vec<String>>),
}

/// Where refused paths are told, when the lab's own verbs run in a session.
static TELLER: OnceLock<Option<Teller>> = OnceLock::new();

/// The paths told this run.
static TOLD: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Finds the instance hosting the session this lab runs in (`ATERM_PARENT_SESSION_ID`, by its
/// discovery graph entry: [`aterm_ctl::hosting_sock`]); with none, or under a test
/// ([`started_by_a_test`]), nothing is ever told.
pub(crate) fn arm() {
    let _ = TELLER.set(
        (!started_by_a_test(std::process::id()))
            .then(|| std::env::var(aterm_types::domain::ENV_PARENT_SESSION_ID).ok())
            .flatten()
            .and_then(|sid| {
                let dir = aterm_uds::control_socket_dir()?;
                let sid = aterm_types::control_request::Sid::parse(&sid)?;
                aterm_ctl::hosting_sock(&dir, sid)
            })
            .map(Teller::Instance),
    );
}

/// Whether `pid`, or a process its parent chain reaches (at most 64 up), runs a Cargo test
/// binary ([`aterm_uds::test_binary::is_test_binary`]), by each one's executable path and lineage.
fn started_by_a_test(pid: u32) -> bool {
    let mut at = pid;
    for _ in 0..64 {
        let Ok(p) = i32::try_from(at) else {
            return false;
        };
        if aterm_uds::process::read_process_path(p)
            .is_some_and(|exe| aterm_uds::test_binary::is_test_binary(&exe))
        {
            return true;
        }
        match aterm_uds::process::read_lineage(p) {
            Some(lin) if lin.parent > 1 && lin.parent != at => at = lin.parent,
            _ => return false,
        }
    }
    false
}

/// The line that tells of `path`, the first time `told` meets it: `None` for a path told before.
pub(crate) fn line_once(told: &mut BTreeSet<PathBuf>, path: &Path) -> Option<String> {
    if !told.insert(path.to_path_buf()) {
        return None;
    }
    let mut shown = String::new();
    for c in path.display().to_string().chars() {
        let c = if c.is_control() { ' ' } else { c };
        if shown.len() + c.len_utf8() > PATH_BYTES {
            shown.push('…');
            break;
        }
        shown.push(c);
    }
    Some(format!(
        "lab refused {shown}: a link in its path, not followed"
    ))
}

/// Tells the instance hosting this lab's session, once per path, that the lab refused `path`
/// for a link in it; on a thread of its own, waited for at most [`TELL_BOUND`], so an instance
/// that does not answer holds nothing. Unarmed (a test, or no session), it tells nothing.
pub(crate) fn refused(path: &Path) {
    let Some(Some(teller)) = TELLER.get() else {
        return;
    };
    let Some(line) = line_once(
        &mut TOLD.lock().unwrap_or_else(PoisonError::into_inner),
        path,
    ) else {
        return;
    };
    let sock = match teller {
        Teller::Instance(sock) => sock.clone(),
        #[cfg(test)]
        Teller::Kept(lines) => {
            lines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(line);
            return;
        }
    };
    let (done, finished) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("lab-link-notice".to_owned())
        .spawn(move || {
            let _ = aterm_ctl::front_door_send_within(
                &sock,
                &format!("appnotice proc {line}\n"),
                TELL_BOUND,
                TELL_BOUND,
            );
            let _ = done.send(());
        });
    if started.is_ok() {
        let _ = finished.recv_timeout(TELL_BOUND);
    }
}

#[cfg(test)]
#[path = "link_notice_tests.rs"]
mod tests;
