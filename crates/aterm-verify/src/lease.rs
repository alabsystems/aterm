// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RUN'S LEASE on the toolchain it resolved (gaps #12 and #31, 2026-09-26).
//!
//! A run resolves ONE physical toolchain directory and hands it to every child as
//! `$TRUST_STAGE2_BIN` ([`crate::Ctx::with_pinned_child_facts`]); stages then run from it
//! for 33 minutes to 14 hours. Between two stages no process runs from that directory, so
//! nothing the package manager could see said it was in use: an update that superseded the
//! toolchain twice inside one run let atpkg's gc reclaim the store build the run was still
//! going to use (the run ended COULD NOT RUN), and an unattended trust update could re-lay
//! the rustup view — the directory `cargo +trust` resolves into — between two stages. The
//! lease is the run saying so, for exactly as long as it runs: gc keeps a leased build,
//! the seam never re-lays a leased view, and the unattended trust flip waits while the
//! live toolchain is leased (four hours at the most).
//!
//! THIS IS A MIRROR. The protocol belongs to `atpkg::lease` (crates/atpkg/src/lease.rs),
//! which gc, the seam and the flip gate read. This crate has no dependencies — Cargo.toml
//! says why, and it is load-bearing — so it cannot call that module; it writes the same
//! files with std alone, and each side's tests pin the spelling:
//!
//! * `<prefix>/leases/<subject>.gate`, taken SHARED (polled, bounded) while the lease is
//!   laid, so it never lands inside a reclaim, which holds the gate exclusive;
//! * `<prefix>/leases/<subject>@<pid>-<nonce>.lease`, locked exclusive by this process for
//!   the whole run and carrying one "who" line — laid as `….taking`, locked, written and
//!   renamed, so a reader never finds an unlocked lease of a live run;
//! * `<subject>` is `store-<program>-<build>` for a store build (or its `compat/` exec
//!   root) and `rustup-<name>` for the seam's view.
//!
//! `File::try_lock` is `flock(2)`: the kernel drops the lock with the process, so a killed
//! run pins nothing — atpkg reaps its file at the next reading. Dropped, the lease removes
//! its file and unlocks with `LOCK_UN`, not the close: a child another stage's thread is
//! spawning holds a copy of every descriptor until it execs (atpkg's `lock::Flock`).
//!
//! A toolchain outside the atpkg prefix (a checkout's stage2, a hand-made rustup link)
//! takes no lease — nothing of atpkg's reclaims it — and a lease that cannot be taken is
//! said and never stops the run: the run is exactly as exposed as it was before leases.

use std::fs::File;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// `<prefix>/leases`.
const DIR: &str = "leases";

/// How long the run waits for a reclaim in progress to let the gate go: a reclaim holds it
/// across one build's delete — seconds, never minutes.
pub const WAIT: Duration = Duration::from_secs(120);

/// How often the gate is re-tried while a reclaim holds it.
const POLL: Duration = Duration::from_millis(100);

/// A word a subject may carry (atpkg's `lease::is_word`).
fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The subject a toolchain directory belongs to under `prefix`, as `(stem, words)`:
/// `("store-trust-9192", "trust build 9192")`,
/// `("rustup-trust", "the rustup trust toolchain")` — or `None` for a directory atpkg does
/// not manage. Both spellings of each path are tried, as written and resolved (atpkg's
/// `Subject::of_dir`).
#[must_use]
pub fn subject_of(prefix: &Path, dir: &Path) -> Option<(String, String)> {
    let prefixes = [
        Some(prefix.to_path_buf()),
        std::fs::canonicalize(prefix).ok(),
    ];
    let dirs = [Some(dir.to_path_buf()), std::fs::canonicalize(dir).ok()];
    for p in prefixes.iter().flatten() {
        for d in dirs.iter().flatten() {
            let Ok(rel) = d.strip_prefix(p) else {
                continue;
            };
            let mut parts = rel.components().filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            });
            match (parts.next(), parts.next(), parts.next()) {
                (Some("store" | "compat"), Some(program), Some(build))
                    if is_word(program)
                        && !build.is_empty()
                        && build.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    let build: u64 = build.parse().ok()?;
                    return Some((
                        format!("store-{program}-{build}"),
                        format!("{program} build {build}"),
                    ));
                }
                (Some("rustup"), Some(name), _) if is_word(name) => {
                    return Some((
                        format!("rustup-{name}"),
                        format!("the rustup {name} toolchain"),
                    ));
                }
                _ => {}
            }
        }
    }
    None
}

/// A held lease; dropping it releases it.
#[derive(Debug)]
pub struct Lease {
    file: File,
    path: PathBuf,
    /// `trust build 9192`, `the rustup trust toolchain`.
    pub what: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = self.file.unlock();
    }
}

/// What taking the run's lease came to.
#[derive(Debug)]
pub enum Taken {
    /// Held for the run.
    Held(Lease),
    /// The toolchain is not atpkg's: nothing of atpkg's can reclaim it.
    NotManaged,
    /// It could not be taken — the run goes on, exactly as exposed as before leases.
    Failed(String),
}

impl Taken {
    /// The `verify:` header line that says it, under the toolchain line —
    /// nothing for a toolchain atpkg does not manage, since nothing happened.
    #[must_use]
    pub fn header_line(&self) -> String {
        match self {
            Self::Held(lease) => format!(
                "verify: {} held for this run; an unattended trust update waits for it \
                 (4 h at most)\n",
                lease.what
            ),
            Self::NotManaged => String::new(),
            Self::Failed(why) => format!(
                "verify: toolchain not held ({why}); an update during this run can replace it\n"
            ),
        }
    }
}

/// A process-unique `<pid>-<nonce>`.
fn tag() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{}-{nanos:x}{n:x}", std::process::id())
}

/// `who` cut to one line under atpkg's 512-byte read, on a character boundary.
fn who_line(who: &str) -> String {
    let first = who.lines().next().unwrap_or_default();
    let mut end = first.len().min(511);
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    first[..end].to_string()
}

/// Take the lease on `toolchain_dir` under `prefix`, saying `who` holds it, waiting up to
/// `wait` for a reclaim in progress. [`Taken::NotManaged`] for a directory outside the
/// prefix's store, compat roots and views — and nothing is created then.
#[must_use]
pub fn take(prefix: &Path, toolchain_dir: &Path, who: &str, wait: Duration) -> Taken {
    let Some((stem, what)) = subject_of(prefix, toolchain_dir) else {
        return Taken::NotManaged;
    };
    match lay(prefix, &stem, who, wait) {
        Ok((file, path)) => Taken::Held(Lease { file, path, what }),
        Err(why) => Taken::Failed(why),
    }
}

/// The protocol itself (the module header).
fn lay(prefix: &Path, stem: &str, who: &str, wait: Duration) -> Result<(File, PathBuf), String> {
    let dir = prefix.join(DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("{} is not a directory", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("{}: {e}", dir.display())),
            }
        }
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    let open = |path: &Path, create_new: bool| {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        if create_new {
            options.create_new(true);
        } else {
            options.create(true).truncate(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(path)
    };
    let gate_path = dir.join(format!("{stem}.gate"));
    let gate: std::fs::File =
        open(&gate_path, false).map_err(|e| format!("{}: {e}", gate_path.display()))?;
    let deadline = Instant::now() + wait;
    loop {
        match gate.try_lock_shared() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(POLL);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "atpkg was reclaiming it and did not finish within {} s",
                    wait.as_secs()
                ));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("{}: {e}", gate_path.display()));
            }
        }
    }
    let tag = tag();
    let taking = dir.join(format!("{stem}@{tag}.taking"));
    let path = dir.join(format!("{stem}@{tag}.lease"));
    let laid = (|| -> std::io::Result<File> {
        let mut file: std::fs::File = open(&taking, true)?;
        file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::Error(e) => e,
            std::fs::TryLockError::WouldBlock => std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "a lease file this run just created was already locked",
            ),
        })?;
        let mut line = who_line(who);
        line.push('\n');
        file.write_all(line.as_bytes())?;
        std::fs::rename(&taking, &path)?;
        Ok(file)
    })();
    let _ = gate.unlock();
    match laid {
        Ok(file) => Ok((file, path)),
        Err(e) => {
            let _ = std::fs::remove_file(&taking);
            Err(format!("{}: {e}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aterm-verify-lease-{label}-{}", tag()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The spelling atpkg's `lease::Subject` reads back (its `subjects_spell_and_read_back`
    /// pins the same stems from the other side).
    #[test]
    fn the_subject_is_spelled_as_atpkg_reads_it() {
        let p = prefix("subject");
        let bin = p.join("store/trust/9192/bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(
            subject_of(&p, &bin),
            Some(("store-trust-9192".into(), "trust build 9192".into()))
        );
        assert_eq!(
            subject_of(&p, &p.join("compat/trust/9192/bin")).map(|s| s.0),
            Some("store-trust-9192".into())
        );
        assert_eq!(
            subject_of(&p, &p.join("rustup/trust/bin")),
            Some(("rustup-trust".into(), "the rustup trust toolchain".into()))
        );
        assert_eq!(
            subject_of(&p, &p.join("store/trust-mc/7/bin")).map(|s| s.0),
            Some("store-trust-mc-7".into())
        );
        assert_eq!(subject_of(&p, Path::new("/opt/stage2/bin")), None);
        assert_eq!(subject_of(&p, &p.join("store/trust/current/bin")), None);
        let _ = std::fs::remove_dir_all(&p);
    }

    /// Held: one `<stem>@<pid>-<nonce>.lease` file, locked, carrying the who line; dropped:
    /// gone. Outside the prefix: nothing taken and nothing created.
    #[test]
    fn a_lease_is_one_locked_file_for_the_life_of_the_run() {
        let p = prefix("held");
        let bin = p.join("store/trust/9192/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let Taken::Held(lease) = take(&p, &bin, "aterm-verify (pid 7) in /w", WAIT) else {
            panic!("a store toolchain is leased");
        };
        let names: Vec<String> = std::fs::read_dir(p.join(DIR))
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        let held: Vec<&String> = names.iter().filter(|n| n.ends_with(".lease")).collect();
        assert_eq!(held.len(), 1, "{names:?}");
        assert!(held[0].starts_with("store-trust-9192@"), "{names:?}");
        assert!(names.contains(&"store-trust-9192.gate".to_string()));
        let file = p.join(DIR).join(held[0]);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "aterm-verify (pid 7) in /w\n"
        );
        // Another open of it cannot lock it: this is what atpkg reads as a live holder.
        let other: std::fs::File = File::open(&file).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        assert!(
            Taken::Held(lease)
                .header_line()
                .contains("verify: trust build 9192 held for this run")
        );
        assert!(!file.exists(), "dropped with its run");
        let elsewhere = prefix("elsewhere");
        let other = take(&p, &elsewhere, "x", WAIT);
        assert!(matches!(other, Taken::NotManaged));
        assert_eq!(other.header_line(), "");
        let _ = std::fs::remove_dir_all(&p);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    /// A reclaim holding the gate (atpkg takes it exclusive across its delete) makes the
    /// run wait, and say so at its bound — never hang, never stop the run.
    #[test]
    fn a_reclaim_in_progress_is_waited_out_and_said() {
        let p = prefix("gate");
        let bin = p.join("rustup/trust/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(p.join(DIR)).unwrap();
        let gate: std::fs::File = File::create(p.join(DIR).join("rustup-trust.gate")).unwrap();
        gate.try_lock().unwrap();
        let taken = take(&p, &bin, "x", Duration::from_millis(250));
        let Taken::Failed(why) = &taken else {
            panic!("a held gate is waited out: {taken:?}");
        };
        assert!(why.contains("reclaiming"), "{why}");
        assert!(taken.header_line().contains("not held"));
        gate.unlock().unwrap();
        assert!(matches!(take(&p, &bin, "x", WAIT), Taken::Held(_)));
        let _ = std::fs::remove_dir_all(&p);
    }
}
