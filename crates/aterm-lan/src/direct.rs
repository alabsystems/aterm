// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The allowed direct path's server (`aterm_uds::lan::direct`,
//! `docs/DESIGN-lan-broker-2026-11-05.md` §5): the broker's own [`connection`] served inside this
//! process over a socketpair, to this process alone, while no broker is installed and macOS's own
//! record says it allowed this code on the local network ([`crate::grant`]).
//!
//! A Mac updated from a build with no broker (0.158.0 and earlier) holds that allowance for aterm
//! and no broker until a person types the Mac password the broker's install needs; this keeps
//! its LAN meanwhile, raising no alert, since macOS asks only code it has no answer for. The
//! process makes its own LAN sockets and DNS-SD calls as it did before the broker. Each session
//! reads both when it opens, so once the broker answers, or macOS's answer for this account
//! changes, the next session goes to the broker or finds no path; a session already open (a
//! browse, an advert) runs until its client ends it.
//!
//! macOS judges a LAN use by the process's responsible app, which is not always aterm: an `aterm`
//! command typed in Terminal is Terminal's to answer for. So the path serves only a process whose
//! responsible app runs this same executable (the app, a tab's command, a helper it started) or
//! that is its own: any other would ask its app's question, which the broker never does.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use aterm_uds::CtlStream;

use crate::grant;
use crate::server::{Admission, Shared, connection};

/// This process's direct path: its code (read once) and its server (made once).
struct Direct {
    code: OnceLock<Result<grant::Code, String>>,
    shared: OnceLock<Arc<Shared>>,
    /// Where macOS's answers are read ([`grant::NEHELPER_PLIST`]; a test's fixture).
    plist: &'static str,
    /// Whether macOS answers this process's LAN use as `code`'s own
    /// ([`responsible_is_this_code`]; a test's stand-in).
    responsible: fn(&grant::Code) -> Result<(), String>,
    /// Where the server's connection lines go ([`quiet`]; a test's recorder).
    log: fn(std::fmt::Arguments<'_>),
}

/// The direct path's server log: nothing. Its one client is this process, which hears every
/// failure as the broker's frame and says it in its own words; the broker's log lines would
/// otherwise land on this process's stderr (a person's terminal, or `ssh`'s from `lan fdpass`).
fn quiet(_: std::fmt::Arguments<'_>) {}

/// Whether the app macOS holds responsible for this process (the one whose Local Network answer
/// it applies) is `code`'s executable, or this process itself.
///
/// # Errors
/// Another app's, in words, or a responsible app macOS did not name.
fn responsible_is_this_code(code: &grant::Code) -> Result<(), String> {
    let me = i32::try_from(std::process::id()).map_err(|_| "a pid past i32".to_owned())?;
    let responsible = aterm_containment::responsible_pid(me)
        .ok_or_else(|| "macOS did not name the app responsible for this process".to_owned())?;
    if responsible == me {
        return Ok(());
    }
    let path = pid_path(responsible).ok_or_else(|| {
        format!("the app responsible for this process (pid {responsible}) is gone")
    })?;
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    if path.to_string_lossy() == code.executable {
        Ok(())
    } else {
        Err(format!(
            "macOS answers this process's LAN use as {}'s (pid {responsible}), not aterm's",
            path.display()
        ))
    }
}

/// `pid`'s executable, from `proc_pidpath`.
fn pid_path(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut buf = [0u8; 4 * 1024];
    let len = u32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is writable for `len` bytes, all `proc_pidpath` may write.
    let wrote = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), len) };
    let wrote = usize::try_from(wrote).ok().filter(|n| *n > 0)?;
    Some(std::ffi::OsStr::from_bytes(&buf[..wrote.min(buf.len())]).into())
}

impl Direct {
    fn code(&self) -> Result<&grant::Code, String> {
        self.code
            .get_or_init(grant::this_code)
            .as_ref()
            .map_err(Clone::clone)
    }

    fn shared(&self) -> &Arc<Shared> {
        // Admitted by uid: the one peer is this process, at the socketpair's other end.
        self.shared.get_or_init(|| {
            Arc::new(Shared::new(
                Admission::SameUid,
                crate::bonjour::system(),
                self.log,
            ))
        })
    }
}

impl aterm_uds::lan::direct::Direct for Direct {
    fn allowed(&self) -> Result<String, String> {
        let code = self.code()?;
        let bytes = grant::read_bounded(Path::new(self.plist))?;
        let allowed = grant::allowed(&bytes, code)?;
        (self.responsible)(code)?;
        Ok(allowed)
    }

    fn record(&self) -> Option<std::path::PathBuf> {
        Some(Path::new(self.plist).to_path_buf())
    }

    fn serve(&self, stream: CtlStream) {
        connection(self.shared(), &stream);
    }
}

static DIRECT: Direct = Direct {
    code: OnceLock::new(),
    shared: OnceLock::new(),
    plist: grant::NEHELPER_PLIST,
    responsible: responsible_is_this_code,
    log: quiet,
};

/// Give this process the allowed direct path (`aterm`'s start does, once). It reads nothing until
/// a LAN use finds no broker.
pub fn provide_direct() {
    // A second provision is the first's, already in place.
    let _ = aterm_uds::lan::direct::provide(&DIRECT);
}

#[cfg(test)]
#[path = "direct_tests.rs"]
mod tests;
