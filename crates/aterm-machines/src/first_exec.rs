// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A started program's folder stays until the program showed it ran: macOS 27.0.1's
//! syspolicyd crashes when a file it is assessing at its first exec goes, and every exec
//! then waiting on it is stranded. This module reads and writes
//! [`aterm_uds::deadline`]'s stamp and marker through the [`System`] port.

use crate::port::{Cmd, Ran, System};
use aterm_uds::deadline::{EXEC_STAMP, FIRST_EXEC_HANG, RAN};
use std::path::Path;
use std::time::Duration;

/// The mode of [`EXEC_STAMP`] and [`RAN`]: an awake-clock reading is no secret, and a plan
/// run without root ages the stamp too.
const MODE: u32 = 0o644;

/// Record in `home`, just before a program under it starts, that it has not shown it ran:
/// [`EXEC_STAMP`] with the awake-clock reading, and no [`RAN`]. An earlier uninstall's mark
/// ([`crate::agent::reclaim::KEPT`]) is cancelled before its reclaim claim is checked; the job
/// claims that mark atomically before it empties the folder, so either cancellation wins or the
/// claim refuses this start until the job finishes.
///
/// # Errors
/// The reclaim claim is present or cannot be checked, the stamp cannot be written, or a
/// proof or mark cannot be removed; the program must not start.
pub fn stamp(sys: &dyn System, home: &Path) -> std::io::Result<()> {
    match sys.remove(&home.join(crate::agent::reclaim::KEPT)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let claim = home.join(crate::agent::reclaim::CLAIM);
    match sys.stat(&claim) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("{} is held by an uninstall's reclaim job", claim.display()),
            ));
        }
    }
    match sys.remove(&home.join(RAN)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let at = sys.awake_ms().map(|ms| ms.to_string()).unwrap_or_default();
    sys.write_file(&home.join(EXEC_STAMP), at.as_bytes(), MODE)
}

/// Write [`RAN`] in `home`: a program under it showed it ran.
pub fn mark_ran(sys: &dyn System, home: &Path) {
    let _ = sys.write_file(&home.join(RAN), b"", MODE);
}

/// Whether `ran` shows its program ran: a line it wrote, or an exit of its own (a signal
/// is no proof).
#[must_use]
pub fn showed_it_ran(ran: &Ran) -> bool {
    ran.status.is_some() || !ran.stdout.is_empty()
}

/// This Mac's clocks as the enrollment root script reads them
/// ([`crate::controller::enroll::stage_guard`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootClocks {
    /// This boot's id (`kern.bootsessionuuid`).
    pub session: String,
    /// The boot's Unix seconds (`kern.boottime`).
    pub boot: u64,
    /// The last wake's Unix seconds (`kern.waketime`), 0 before any.
    pub woke: u64,
    /// The wall clock less `boot`: the seconds since boot, which no step of the wall clock
    /// moves.
    pub up: u64,
}

/// Read [`BootClocks`] with the root script's commands: `None` when a reading has another
/// form, or when the boot time read again after the wall clock moved (a step between the
/// reads would be counted in `up`).
#[must_use]
pub fn boot_clocks(sys: &dyn System) -> Option<BootClocks> {
    let sysctl = |names: &[&str]| {
        sys.run(&Cmd::new("/usr/sbin/sysctl").arg("-n").args(names))
            .ok()
            .filter(Ran::success)
            .map(|ran| ran.out())
    };
    let read = sysctl(&["kern.bootsessionuuid", "kern.boottime", "kern.waketime"])?;
    let now = sys.now();
    let again = sysctl(&["kern.boottime"]).and_then(|t| crate::facts::parse_boottime(&t))?;
    let mut lines = read.lines();
    let session = lines.next().filter(|s| is_boot_id(s))?.to_owned();
    let boot = lines.next().and_then(crate::facts::parse_boottime)?;
    let woke = lines.next().and_then(crate::facts::parse_boottime)?;
    if again != boot {
        return None;
    }
    Some(BootClocks {
        session,
        boot,
        woke,
        up: now.checked_sub(boot)?,
    })
}

/// Whether `word` has `kern.bootsessionuuid`'s form.
fn is_boot_id(word: &str) -> bool {
    word.len() == 36
        && word.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The stamp the enrollment root script writes just before the staged agent's first exec:
/// `<boot id> <up> <boot> <woke>`, as [`BootClocks`] read then.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RootStamp(BootClocks);

impl RootStamp {
    /// `text` as a root script's stamp, `None` when it has another form.
    fn parse(text: &str) -> Option<Self> {
        let seconds =
            |word: &str| (1..=12).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_digit());
        match text.split_ascii_whitespace().collect::<Vec<_>>()[..] {
            [session, up, boot, woke]
                if is_boot_id(session) && [up, boot, woke].into_iter().all(seconds) =>
            {
                Some(Self(BootClocks {
                    session: session.to_owned(),
                    boot: boot.parse().ok()?,
                    woke: woke.parse().ok()?,
                    up: up.parse().ok()?,
                }))
            }
            _ => None,
        }
    }

    /// How much longer its folder stays, judged as the stage guard judges it: `None` after a
    /// restart (another boot id), else [`FIRST_EXEC_HANG`] of `up` from the later of the
    /// stamp and the last wake. Clocks that cannot be read, and a wake that cannot be placed
    /// on `up` (the wall clock stepped since the stamp too), hold the whole of it.
    fn left(&self, now: Option<&BootClocks>) -> Option<Duration> {
        let (was, Some(now)) = (&self.0, now) else {
            return Some(FIRST_EXEC_HANG);
        };
        if was.session != now.session {
            return None;
        }
        let from = if was.woke == now.woke {
            was.up
        } else if was.boot == now.boot {
            now.woke.saturating_sub(now.boot).max(was.up)
        } else {
            return Some(FIRST_EXEC_HANG);
        };
        let Some(spent) = now.up.checked_sub(from) else {
            return Some(FIRST_EXEC_HANG);
        };
        let left = FIRST_EXEC_HANG.saturating_sub(Duration::from_secs(spent));
        (!left.is_zero()).then_some(left)
    }
}

/// [`hold_left`] of a folder whose stamp is `text`, read against `now`, when `text` is the
/// enrollment root script's stamp; `None` when it is not.
#[cfg(test)]
pub(crate) fn root_stamp_left(text: &str, now: Option<&BootClocks>) -> Option<Option<Duration>> {
    RootStamp::parse(text).map(|stamp| stamp.left(now))
}

/// How much longer `home` stays: `None` when no program was started under it (no
/// [`EXEC_STAMP`], or `home` is a file), once one showed it ran ([`RAN`]), once the stamp
/// is [`FIRST_EXEC_HANG`] of awake time old, or after a restart (an awake clock below the
/// stamp). The enrollment root script's stamp is aged by [`boot_clocks`], as its stage guard
/// ages it. A stamp with no reading, or one that cannot be read, holds the whole
/// [`FIRST_EXEC_HANG`].
#[must_use]
pub fn hold_left(sys: &dyn System, home: &Path) -> Option<Duration> {
    if sys.stat(&home.join(RAN)).is_ok() {
        return None;
    }
    let text = match sys.read_file(&home.join(EXEC_STAMP)) {
        Ok(text) => text,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return None;
        }
        Err(_) => return Some(FIRST_EXEC_HANG),
    };
    let text = String::from_utf8_lossy(&text);
    if let Some(stamp) = RootStamp::parse(&text) {
        return stamp.left(boot_clocks(sys).as_ref());
    }
    let at = crate::parse_uint::<u64>(text.trim());
    let age = match (at, sys.awake_ms()) {
        (Some(at), Some(now)) => Duration::from_millis(now.checked_sub(at)?),
        _ => Duration::ZERO,
    };
    let left = FIRST_EXEC_HANG.saturating_sub(age);
    (!left.is_zero()).then_some(left)
}

/// `left` as whole minutes, rounded up, for a line a person reads.
#[must_use]
pub fn minutes(left: Duration) -> u64 {
    left.as_secs().div_ceil(60).max(1)
}

#[cfg(test)]
#[path = "first_exec_tests.rs"]
mod tests;
