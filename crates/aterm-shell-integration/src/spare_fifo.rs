// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The warm spare's hold FIFOs in `<control dir>/spare` ([`crate::SPARE_DIR`]): the name the host
//! gives one, `<pid>-<birth>-<sid>` (its owner's identity and the session's id, beside the relative
//! symlink `<sid>` the shell opens), and the sweep that releases the ones whose owner has ended.

use std::path::Path;
use std::sync::OnceLock;

use aterm_uds::process::{Birth, ProcessPresence, process_presence, read_birth};

/// `<pid>-<birth>-<sid>`: the name of session `sid`'s hold FIFO, owned by this process. Its birth
/// ([`read_birth`], 0 where none reads) tells this process from a later one given its pid.
#[must_use]
pub fn name(sid: &str) -> String {
    static OWNER: OnceLock<String> = OnceLock::new();
    let owner = OWNER.get_or_init(|| {
        let pid = std::process::id();
        let birth = i32::try_from(pid)
            .ok()
            .and_then(read_birth)
            .map_or(0, |b| b.0);
        format!("{pid}-{birth}")
    });
    format!("{owner}-{sid}")
}

/// Release every FIFO in `dir` whose owner has ended or whose pid now has a different birth,
/// with its `<sid>` link while that still names it. An unread identity alone releases nothing.
/// What a launch does in its control dir, so a spare whose owner died waiting stops waiting.
pub fn sweep_dead(dir: &Path) {
    sweep_dead_with(dir, read_birth, process_presence);
}

fn sweep_dead_with(
    dir: &Path,
    birth_of: impl Fn(i32) -> Option<Birth>,
    presence: impl Fn(i32) -> ProcessPresence,
) {
    use std::os::unix::fs::FileTypeExt as _;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((pid, birth, sid)) = name.to_str().and_then(name_parts) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_fifo())
            || !owner_ended(pid, birth, &birth_of, &presence)
        {
            continue;
        }
        drop(release(&entry.path(), &dir.join(sid)));
    }
}

/// Unlink the FIFO at `path`, and `link` while it names it, holding a wake on the FIFO across both
/// unlinks; the caller drops the returned wake to close it. The wake opens the FIFO read-write,
/// which never waits or fails for want of a reader. A shell waiting to open the FIFO, or one that
/// resolved `link` before its unlink, gets through, reads the end once the wake closes, finds its
/// link gone, and exits. A write-only, non-blocking wake fails (`ENXIO`) while no reader holds the
/// FIFO, as in the instant a shell spends between reading the end and opening it again, which
/// would leave that shell waiting for ever on a FIFO nobody can name. On Darwin the wake is
/// close-on-fork: a child this process forks while the wake is held would otherwise keep a writer
/// open until it execs, and the shell would read no end until then. If the wake cannot open, both
/// names stay for a later sweep.
fn release(path: &Path, link: &Path) -> Option<std::fs::File> {
    let mut wake = std::fs::OpenOptions::new();
    wake.read(true).write(true);
    #[cfg(target_vendor = "apple")]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut wake, O_CLOFORK);
    let wake = wake.open(path).ok()?;
    if std::fs::read_link(link).is_ok_and(|to| Some(to.as_os_str()) == path.file_name()) {
        let _ = std::fs::remove_file(link);
    }
    let _ = std::fs::remove_file(path);
    Some(wake)
}

/// Darwin's `O_CLOFORK` (`<sys/fcntl.h>`): a child this process forks gets no copy of the
/// descriptor.
#[cfg(target_vendor = "apple")]
const O_CLOFORK: i32 = 0x0800_0000;

/// `(pid, birth, sid)` of a FIFO's name, its sid `s-<hex>` of at most 64 characters.
fn name_parts(name: &str) -> Option<(u32, u64, &str)> {
    let mut parts = name.splitn(3, '-');
    let pid = parts.next()?.parse().ok()?;
    let birth = parts.next()?.parse().ok()?;
    let sid = parts.next()?;
    let hex = sid.strip_prefix("s-")?;
    (sid.len() <= 64 && !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some((pid, birth, sid))
}

/// Positive evidence that the owner ended or its pid was reused. Identity reads can fail for a
/// live owner (on Linux, even descriptor exhaustion): absence requires a separate kernel answer.
fn owner_ended(
    pid: u32,
    birth: u64,
    birth_of: &impl Fn(i32) -> Option<Birth>,
    presence: &impl Fn(i32) -> ProcessPresence,
) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    if birth != 0
        && let Some(now) = birth_of(pid)
    {
        return now != Birth(birth);
    }
    matches!(
        presence(pid),
        ProcessPresence::Exited | ProcessPresence::Absent
    )
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "spare_fifo_tests.rs"]
mod tests;
