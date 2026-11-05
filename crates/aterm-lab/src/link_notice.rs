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
mod tests {
    use super::*;

    /// The lines a test's teller kept that name `under`.
    fn kept_under(under: &Path) -> Vec<String> {
        let teller = TELLER.get_or_init(|| Some(Teller::Kept(Mutex::new(Vec::new()))));
        let Some(Teller::Kept(lines)) = teller else {
            panic!("a test's teller");
        };
        let under = under.display().to_string();
        lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|l| l.contains(&under))
            .cloned()
            .collect()
    }

    /// THE LAB'S SCRATCH READS TELL EACH LINK THEY REFUSE ONCE: a file read and a listing whose
    /// path runs through a planted link, a removal under it and a listing that meets a link in a
    /// folder each tell its path, and a second refusal of the same path tells nothing.
    /// NEGATIVE CONTROL: drop the scratch file read's report of its refusal, and its path is not
    /// told.
    #[test]
    fn the_scratch_reads_tell_each_link_they_refuse_once() {
        use std::os::unix::fs::symlink;
        let _ = kept_under(Path::new("/"));
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        let out = root.join("out");
        std::fs::create_dir(&out).expect("out");
        std::fs::write(out.join("result.json"), b"{}").expect("a result");
        symlink(&out, root.join("planted")).expect("a planted folder link");
        symlink("/etc/hosts", out.join("hosts")).expect("a planted file link");
        let through = root.join("planted/result.json");
        assert!(crate::sys::scratch_file(&through).is_err(), "refused");
        assert!(crate::sys::scratch_file(&through).is_err(), "refused again");
        assert!(crate::sys::scratch_dir(&root.join("planted")).is_err());
        assert!(crate::sys::scratch_remove(&root.join("planted/gone")).is_err());
        let listed = crate::sys::scratch_dir(&out)
            .expect("listed")
            .expect("there");
        assert_eq!(listed.len(), 1, "the link is left out: {listed:?}");
        let told = |p: &Path| {
            format!(
                "lab refused {}: a link in its path, not followed",
                p.display()
            )
        };
        assert_eq!(
            kept_under(&root),
            vec![
                told(&through),
                told(&root.join("planted")),
                told(&root.join("planted/gone")),
                told(&out.join("hosts")),
            ]
        );
    }

    /// A LAB A TEST STARTED TELLS NOTHING: this test binary is a Cargo test's, so the lab's
    /// verbs it runs arm no teller, whatever session it inherits. NEGATIVE CONTROL: read only
    /// the parent chain above this process, and this process reads as no test's.
    #[test]
    fn a_lab_a_test_started_tells_nothing() {
        assert!(started_by_a_test(std::process::id()));
        assert!(!started_by_a_test(1), "launchd");
    }

    /// A REFUSED LINK IS TOLD ONCE: the first refusal of a path makes its line (the path, and
    /// that it was not followed), a second makes none, and another path its own; a path's control
    /// characters are spaced and a long one is cut, so the line stays one line the instance takes.
    #[test]
    fn a_refused_link_is_told_once() {
        let mut told = BTreeSet::new();
        let path = Path::new("/w/runs/lab-1/out/result.json");
        assert_eq!(
            line_once(&mut told, path).as_deref(),
            Some("lab refused /w/runs/lab-1/out/result.json: a link in its path, not followed")
        );
        assert_eq!(line_once(&mut told, path), None, "told once");
        let odd = PathBuf::from(format!("/w/a\nb/{}", "x".repeat(600)));
        let line = line_once(&mut told, &odd).expect("another path");
        assert!(!line.contains('\n') && line.len() < 512, "{line}");
        assert!(line.starts_with("lab refused /w/a b/xxx"), "{line}");
    }
}
