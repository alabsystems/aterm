// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The warm spare's hold FIFOs in `<control dir>/spare` ([`crate::SPARE_DIR`]): the name the host
//! gives one, `<pid>-<birth>-<sid>` (its owner's identity and the session's id, beside the relative
//! symlink `<sid>` the shell opens), and the sweep that releases the ones whose owner has ended.

use std::path::Path;
use std::sync::OnceLock;

use aterm_uds::process::{Birth, pid_alive, read_birth};

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

/// Release every FIFO in `dir` whose owner has ended (its pid no longer reads the birth the name
/// records), with its `<sid>` link while that still names it: what a launch does in its control
/// dir, so that a spare whose owner died waiting stops waiting.
pub fn sweep_dead(dir: &Path) {
    use std::os::unix::fs::FileTypeExt as _;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((pid, birth, sid)) = name.to_str().and_then(name_parts) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_fifo()) || owner_alive(pid, birth) {
            continue;
        }
        drop(release(&entry.path(), &dir.join(sid)));
    }
}

/// Unlink the FIFO at `path`, and `link` while it names it, holding a wake on the FIFO across both
/// unlinks; the wake is returned, and closes when the caller drops it. The wake opens the FIFO for
/// reading and writing, which no FIFO makes wait or refuses, reader or none. A shell waiting to open
/// it is let through; a shell that resolved `link` before its unlink and opens the FIFO while the
/// wake is held finds a writer. Either reads the end once the wake closes, finds its link gone,
/// and exits. A write-only, non-blocking wake is refused (`ENXIO`) while no reader holds the FIFO,
/// which is the instant a shell spends between reading the end and opening it again: that shell
/// would then wait for ever on a FIFO nobody can name.
fn release(path: &Path, link: &Path) -> Option<std::fs::File> {
    let wake = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok();
    if std::fs::read_link(link).is_ok_and(|to| Some(to.as_os_str()) == path.file_name()) {
        let _ = std::fs::remove_file(link);
    }
    let _ = std::fs::remove_file(path);
    wake
}

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

/// Whether the FIFO's owner `pid` still runs: its pid reads the birth the name records. A zombie,
/// a process that is ending, and another user's process read none, and none of them is the owner
/// (the folder is this user's). A name with no birth (0) asks only whether the pid answers.
fn owner_alive(pid: u32, birth: u64) -> bool {
    if birth == 0 {
        return pid_alive(pid);
    }
    i32::try_from(pid).ok().and_then(read_birth) == Some(Birth(birth))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "spare_fifo_tests.rs"]
mod tests;
