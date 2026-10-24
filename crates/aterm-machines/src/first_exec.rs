// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A started program's folder stays until the program showed it ran: macOS 27.0.1's
//! syspolicyd crashes when a file it is assessing at its first exec goes, and every exec
//! then waiting on it is stranded until a restart. [`aterm_uds::deadline`]'s stamp and
//! marker, read and written through the [`System`] port.

use crate::port::{Cmd, Ran, System};
use aterm_uds::deadline::{EXEC_STAMP, FIRST_EXEC_HANG, RAN};
use std::path::Path;
use std::time::Duration;

/// The mode of [`EXEC_STAMP`] and [`RAN`]: an awake-clock reading is no secret, and a plan
/// run without root ages the stamp too.
const MODE: u32 = 0o644;

/// Record in `home`, just before a program under it starts, that it has not shown it ran:
/// [`EXEC_STAMP`] with the awake-clock reading, and no [`RAN`]. Both are readable by all.
/// An earlier uninstall's mark ([`crate::agent::reclaim::KEPT`]) goes too: the folder is in
/// use again, so its reclaim job leaves it, unless the job read the mark just before and is
/// emptying the folder now.
///
/// # Errors
/// The stamp cannot be written, or a proof or mark cannot be removed; the program should not
/// start.
pub fn stamp(sys: &dyn System, home: &Path) -> std::io::Result<()> {
    for stale in [RAN, crate::agent::reclaim::KEPT] {
        match sys.remove(&home.join(stale)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
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
    let at = text.trim().parse::<u64>().ok();
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
mod tests {
    use super::*;
    use crate::port::Fake;

    const HANG_MS: u64 = 600_000;

    #[test]
    fn a_stamped_folder_stays_until_its_program_ran_its_hang_passed_or_the_mac_restarted() {
        let sys = Fake::root();
        let home = Path::new("/A/stage/upgrade-1-5");
        assert_eq!(hold_left(&sys, home), None, "nothing was started under it");
        sys.awake_at(5_000_000);
        stamp(&sys, home).unwrap();
        assert_eq!(hold_left(&sys, home), Some(FIRST_EXEC_HANG));
        sys.awake_at(5_000_000 + HANG_MS - 60_000);
        assert_eq!(hold_left(&sys, home), Some(Duration::from_secs(60)));
        sys.awake_at(5_000_000 + HANG_MS);
        assert_eq!(hold_left(&sys, home), None, "its hang has passed");
        sys.awake_at(4_000_000);
        assert_eq!(
            hold_left(&sys, home),
            None,
            "a clock below the stamp: a restart"
        );

        sys.awake_at(5_000_000);
        mark_ran(&sys, home);
        assert_eq!(hold_left(&sys, home), None, "it showed it ran");
        stamp(&sys, home).unwrap();
        assert_eq!(
            hold_left(&sys, home),
            Some(FIRST_EXEC_HANG),
            "a new start takes back the old proof"
        );
    }

    #[test]
    fn a_new_start_takes_its_folder_back_from_an_earlier_uninstalls_reclaim_job() {
        let sys = Fake::root();
        let home = Path::new("/A/state/aterm.app-first-run");
        let mark = home.join(crate::agent::reclaim::KEPT);
        sys.file(&mark.to_string_lossy(), b"1-9");
        sys.awake_at(5_000_000);
        stamp(&sys, home).unwrap();
        assert!(sys.written(&mark.to_string_lossy()).is_none());
        assert_eq!(hold_left(&sys, home), Some(FIRST_EXEC_HANG));
    }

    #[test]
    fn a_stamp_with_no_reading_holds_and_only_an_exit_or_a_line_is_proof() {
        let sys = Fake::root();
        sys.file("/A/.staging/exec", b"");
        sys.awake_at(u64::MAX);
        assert_eq!(
            hold_left(&sys, Path::new("/A/.staging")),
            Some(FIRST_EXEC_HANG)
        );
        assert!(showed_it_ran(&Ran::exit(1, "refused")));
        assert!(showed_it_ran(&Ran {
            status: None,
            stdout: b"0.1.0\n".to_vec(),
            stderr: Vec::new(),
        }));
        assert!(!showed_it_ran(&Ran {
            status: None,
            stdout: Vec::new(),
            stderr: b"Killed: 9".to_vec(),
        }));
        assert_eq!(minutes(Duration::from_secs(61)), 2);
        assert_eq!(minutes(Duration::from_millis(1)), 1);
    }

    /// The hold left on `/A/.staging` stamped `stamp` by the root script, on a Mac whose
    /// clocks read the boot id `session`, the boot and the last wake at `boot` and `woke`,
    /// the wall clock at `now`, and the boot again at `again` (Unix seconds).
    fn staged_hold(
        stamp: &str,
        session: &str,
        (boot, woke, now, again): Clock,
    ) -> Option<Duration> {
        let mut sys = Fake::root();
        sys.now = now;
        let clock = |sec: u64| format!("{{ sec = {sec}, usec = 0 }} Thu Oct 22 09:00:00 2026\n");
        sys.on(
            "/usr/sbin/sysctl -n kern.bootsessionuuid kern.boottime kern.waketime",
            Ran::ok(&format!("{session}\n{}{}", clock(boot), clock(woke))),
        )
        .on("/usr/sbin/sysctl -n kern.boottime", Ran::ok(&clock(again)));
        sys.file("/A/.staging/exec", stamp.as_bytes());
        hold_left(&sys, Path::new("/A/.staging"))
    }

    /// `(boot, woke, now, again)`, as [`staged_hold`] takes them.
    type Clock = (u64, u64, u64, u64);

    #[test]
    fn the_root_scripts_stamp_is_aged_by_the_clocks_it_was_read_on() {
        const BOOT: u64 = 1_792_000_000;
        const UP: u64 = 5_000;
        let id = "55C317A3-F793-4F0D-BE04-262D2C6ED332";
        let stamp = format!("{id} {UP} {BOOT} 0\n");
        let at = |up: u64| (BOOT, 0, BOOT + up, BOOT);
        assert_eq!(
            staged_hold(&stamp, id, at(UP + 7_200)),
            None,
            "the negative control: its hang passed two hours ago"
        );
        assert_eq!(staged_hold(&stamp, id, at(UP)), Some(FIRST_EXEC_HANG));
        assert_eq!(
            staged_hold(&stamp, id, at(UP + 9 * 60)),
            Some(Duration::from_secs(60))
        );
        let other = "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0";
        assert_eq!(
            staged_hold(&stamp, other, (BOOT + 9_000, 0, BOOT + 9_030, BOOT + 9_000)),
            None,
            "a restart since the stamp"
        );
        let stepped = BOOT + 1_200;
        assert_eq!(
            staged_hold(&stamp, id, (stepped, 0, stepped + UP + 120, stepped)),
            Some(Duration::from_secs(480)),
            "a step of the wall clock moves the boot time with it"
        );
        assert_eq!(
            staged_hold(&stamp, id, (BOOT, 0, BOOT + UP + 7_200, stepped)),
            Some(FIRST_EXEC_HANG),
            "a step between the reads of the boot time and the wall clock"
        );
        assert_eq!(
            staged_hold(&format!("{id} {UP} {BOOT} 0 more\n"), id, at(UP + 7_200)),
            Some(FIRST_EXEC_HANG),
            "another form, read as no reading"
        );
    }

    #[test]
    fn a_file_is_no_folder_a_program_was_started_from() {
        let live = crate::port::Live::new();
        let tmp = aterm_tempfile::tempdir().unwrap();
        let file = tmp.path().join("aterm-open-1.sig");
        std::fs::write(&file, b"sig").unwrap();
        assert_eq!(hold_left(&live, &file), None);
        let started = tmp.path().join("upgrade-1-5");
        std::fs::create_dir(&started).unwrap();
        stamp(&live, &started).unwrap();
        assert!(hold_left(&live, &started).is_some(), "the negative control");
    }
}
