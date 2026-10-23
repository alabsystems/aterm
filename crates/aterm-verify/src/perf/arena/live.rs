// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What an arena run must not leave behind when it is interrupted, and the one thread that retires
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError, mpsc};
use std::time::Duration;

use aterm_uds::spawnfd::{Child, Command, Stdio, command};

/// Everything live, under one lock. Holding it is what keeps a launch and an
/// interrupt from interleaving: an interrupt that takes it first exits before
/// the launch starts, and a launch that takes it first is registered before
/// the interrupt reads the table.
struct Live {
    /// Each launched group by pgid, with its leader's handle (`None` while a
    /// retire holds the handle).
    groups: BTreeMap<u32, Option<Child>>,
    dirs: BTreeSet<PathBuf>,
    /// Those of `dirs` holding a program started from a copy there that has not
    /// yet shown it ran ([`starting_from`]): kept, never removed.
    unproven: BTreeSet<PathBuf>,
    /// Ghostty copies opened through `open -na`: launchd's children, not ours.
    apps: BTreeSet<PathBuf>,
}

static LIVE: Mutex<Live> = Mutex::new(Live {
    groups: BTreeMap::new(),
    dirs: BTreeSet::new(),
    unproven: BTreeSet::new(),
    apps: BTreeSet::new(),
});

fn live() -> MutexGuard<'static, Live> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long a retired group gets after SIGTERM before SIGKILL, and how long
/// after SIGKILL before the wait is called hung.
const TERM_GRACE: Duration = Duration::from_secs(3);
const KILL_CEILING: Duration = Duration::from_secs(60);

fn killpg(pgid: u32, sig: i32) {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return;
    };
    // SAFETY: killpg takes two integers and touches no memory of this process.
    unsafe { libc::killpg(pgid, sig) };
}

/// A launched process that leads its own process group, retired as a group.
pub(crate) struct Group {
    pgid: u32,
}

impl Group {
    pub(crate) fn spawn(cmd: &mut Command) -> Result<Group, String> {
        let mut live = live();
        let child = cmd
            .process_group(0)
            .spawn()
            .map_err(|e| format!("could not launch {:?}: {e}", cmd.get_program()))?;
        let pgid = child.id();
        live.groups.insert(pgid, Some(child));
        Ok(Group { pgid })
    }

    /// The exit status, if the leader has exited.
    pub(crate) fn exited(&mut self) -> Option<ExitStatus> {
        live()
            .groups
            .get_mut(&self.pgid)
            .and_then(Option::as_mut)
            .and_then(|c| c.try_wait().ok().flatten())
    }

    /// SIGTERM the group, wait for the leader's exit, SIGKILL on the grace running out.
    pub(crate) fn retire(mut self) -> Result<(), String> {
        self.retire_inner()
    }

    fn retire_inner(&mut self) -> Result<(), String> {
        let taken = live().groups.get_mut(&self.pgid).and_then(Option::take);
        let Some(mut child) = taken else {
            return Ok(());
        };
        let result = if child.try_wait().ok().flatten().is_some() {
            killpg(self.pgid, libc::SIGKILL);
            Ok(())
        } else {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(child.wait());
            });
            killpg(self.pgid, libc::SIGTERM);
            if rx.recv_timeout(TERM_GRACE).is_ok() {
                killpg(self.pgid, libc::SIGKILL);
                Ok(())
            } else {
                killpg(self.pgid, libc::SIGKILL);
                rx.recv_timeout(KILL_CEILING)
                    .map(|_| ())
                    .map_err(|_| format!("process group {} outlived SIGKILL", self.pgid))
            }
        };
        live().groups.remove(&self.pgid);
        result
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let _ = self.retire_inner();
    }
}

/// A fresh private directory `<base>/<prefix><pid>.<n>`, registered for removal on interrupt.
pub(crate) fn make_dir(base: &Path, prefix: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt as _;
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let mut live = live();
    loop {
        let dir = base.join(format!(
            "{prefix}{}.{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("could not chmod {}: {e}", dir.display()))?;
                live.dirs.insert(dir.clone());
                return Ok(dir);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("could not create {}: {e}", dir.display())),
        }
    }
}

/// Remove a directory [`make_dir`] made, and forget it.
pub(crate) fn remove_dir(dir: &Path) {
    let mut live = live();
    if live.unproven.remove(dir) {
        eprintln!("{}", kept(dir));
    } else {
        let _ = std::fs::remove_dir_all(dir);
    }
    live.dirs.remove(dir);
}

/// A program copied into `dir` (a [`make_dir`] directory) is about to start.
pub(crate) fn starting_from(dir: &Path) {
    live().unproven.insert(dir.to_path_buf());
}

/// The program [`starting_from`] noted in `dir` has shown it ran: `dir` may go.
pub(crate) fn ran_from(dir: &Path) {
    live().unproven.remove(dir);
}

/// The words for a scratch directory kept because a program copied there never
/// showed it ran.
fn kept(dir: &Path) -> String {
    format!(
        "verify perf: kept {}: a program copied there was started and never showed it ran, so \
         macOS may still be assessing its first exec; remove it once no process runs from it",
        dir.display()
    )
}

/// Note a ghostty copy about to be opened, so an interrupt stops it too.
pub(crate) fn opening(app: &Path) {
    live().apps.insert(app.to_path_buf());
}

/// Stop every ghostty the arena opened from `app`.
pub(crate) fn stop_app(app: &Path) {
    let _ = command("pkill")
        .arg("-f")
        .arg(app.join("Contents/MacOS"))
        .stderr(Stdio::null())
        .status();
    live().apps.remove(app);
}

fn interrupts() -> libc::sigset_t {
    let mut set = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: sigemptyset initialises the set it is handed; sigaddset adds a valid signal number to
    // that initialised set.
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(set.as_mut_ptr(), sig);
        }
        set.assume_init()
    }
}

/// Install the interrupt guard, once.
pub(crate) fn guard() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let set = interrupts();
        // SAFETY: changes only the calling thread's mask, from a live set.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
        let spawned = std::thread::Builder::new()
            .name("arena-interrupt".into())
            .spawn(|| {
                let set = interrupts();
                let mut sig = 0;
                // SAFETY: waits on a live, initialised set and writes one int.
                while unsafe { libc::sigwait(&set, &mut sig) } != 0 {}
                interrupted(sig)
            });
        if spawned.is_err() {
            // SAFETY: as above.
            unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) };
        }
    });
}

/// Retire everything live and exit, holding the table so nothing launches
/// after it was read.
fn interrupted(sig: i32) -> ! {
    let mut live = live();
    let mut children = Vec::new();
    for (pgid, child) in &mut live.groups {
        killpg(*pgid, libc::SIGKILL);
        children.extend(child.take());
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for mut child in children {
            let _ = child.wait();
        }
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(KILL_CEILING);
    for app in &live.apps {
        let _ = command("pkill")
            .arg("-f")
            .arg(app.join("Contents/MacOS"))
            .stderr(Stdio::null())
            .status();
    }
    let mut removed = 0;
    for dir in &live.dirs {
        if live.unproven.contains(dir) {
            eprintln!("{}", kept(dir));
        } else {
            let _ = std::fs::remove_dir_all(dir);
            removed += 1;
        }
    }
    eprintln!(
        "verify perf: interrupted by signal {sig}; retired {} terminal group(s) and removed \
         {removed} scratch director(ies)",
        live.groups.len(),
    );
    std::process::exit(128 + sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retired group is gone: the leader's exit is what the retire waits on,
    /// so the test needs no sleep and no poll.
    #[test]
    fn retiring_a_group_ends_its_leader() {
        let mut g = Group::spawn(command("/bin/sleep").arg("600")).expect("spawn");
        assert!(g.exited().is_none());
        let pid = g.pgid;
        g.retire().expect("retired");
        assert!(
            !aterm_uds::process::pid_alive(pid),
            "{pid} outlived its retire"
        );
        assert!(!live().groups.contains_key(&pid), "the table forgets it");
    }

    /// A scratch directory a started copy has not shown it ran from is kept;
    /// once it ran, the same directory goes. NEGATIVE CONTROL: the second half.
    #[test]
    fn a_directory_a_started_copy_never_ran_from_is_kept() {
        let unproven = make_dir(&std::env::temp_dir(), "atv-unproven-").expect("scratch");
        std::fs::write(unproven.join("aterm"), b"copied").expect("the copy");
        starting_from(&unproven);
        remove_dir(&unproven);
        let kept = unproven.join("aterm").is_file();
        let forgotten = !live().dirs.contains(&unproven);
        let unmarked = !live().unproven.contains(&unproven);
        let _ = std::fs::remove_dir_all(&unproven);
        assert!(kept, "kept while unproven");
        assert!(forgotten && unmarked, "and forgotten, both marks");

        let proven = make_dir(&std::env::temp_dir(), "atv-proven-").expect("scratch");
        std::fs::write(proven.join("aterm"), b"copied").expect("the copy");
        starting_from(&proven);
        ran_from(&proven);
        remove_dir(&proven);
        assert!(!proven.exists(), "it ran: removed");
    }
}
