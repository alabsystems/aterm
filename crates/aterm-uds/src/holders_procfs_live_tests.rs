// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::{Duration, Instant};

use super::{PROC, Table, table};

/// The hang detector for a child's exec.
const HANG: Duration = Duration::from_secs(60);

/// A running copy of `/bin/sleep`, killed and reaped when it drops, so a failed assertion
/// or a hang detector leaves nothing running.
struct Sleeper(crate::spawnfd::Child);

impl Sleeper {
    /// A copy of `/bin/sleep` at `copy` with `mode` and `held` as its stdin, returned once
    /// its exec is done: the kernel names the task after the copy (`comm`) only after it has
    /// settled the task's dumpability (`begin_new_exec`).
    fn start(copy: &Path, mode: u32, held: &Path) -> Self {
        // A child writes the copy (`copy_regular_fresh`), so no fork another test makes holds
        // it open for writing past the spawn's `ETXTBSY` retries.
        crate::deadline::copy_regular_fresh(Path::new("/bin/sleep"), copy)
            .expect("copy /bin/sleep");
        std::fs::set_permissions(copy, std::fs::Permissions::from_mode(mode)).expect("chmod");
        let file = std::fs::File::open(held).expect("open the held file");
        let sleeper = Self(
            crate::spawnfd::command(copy)
                .arg("600")
                .stdin(crate::spawnfd::Stdio::from(file))
                .stdout(crate::spawnfd::Stdio::null())
                .stderr(crate::spawnfd::Stdio::null())
                .spawn()
                .expect("spawn the copy"),
        );
        let name = copy.file_name().expect("a file name").as_encoded_bytes();
        let comm = format!("{PROC}/{}/comm", sleeper.id());
        let started = Instant::now();
        while !std::fs::read(&comm).is_ok_and(|c| c.trim_ascii_end() == name) {
            assert!(
                started.elapsed() < HANG,
                "{} was never exec'd",
                copy.display()
            );
            crate::deadline::poll_yield();
        }
        sleeper
    }

    fn id(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A child holding a file is its holder and names it. A copy this uid may run but not read
/// is non-dumpable (as `ssh-agent` makes itself), so its table is root's: sealed, not
/// unreadable, and still this uid's by its directory's owner. COULD NOT RUN where
/// `fs.suid_dumpable = 1` keeps such an exec dumpable. Negative control: the readable copy
/// of the same program on the same file is listed and holds it.
#[test]
fn the_kernels_tables_name_a_holder_and_seal_a_non_dumpable_one() {
    crate::host::could_not_run_as_root("a program this uid may run but not read");
    let tmp = aterm_tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let (held, unheld) = (dir.join("held"), dir.join("unheld"));
    std::fs::write(&held, b"held").unwrap();
    std::fs::write(&unheld, b"").unwrap();
    let me = std::process::id();

    let open = Sleeper::start(&dir.join("readable-sleep"), 0o755, &held);
    let sealed = Sleeper::start(&dir.join("sealed-sleep"), 0o111, &held);
    let (o, s) = (open.id(), sealed.id());
    let owner = |p: String| std::fs::metadata(p).map(|m| m.uid()).ok();
    if owner(format!("{PROC}/{s}/fd")) != Some(0) {
        let setting = std::fs::read_to_string("/proc/sys/fs/suid_dumpable")
            .map_or_else(|e| format!("unread: {e}"), |v| v.trim().to_owned());
        crate::host::could_not_run(format!(
            "this kernel kept a program this uid may run but not read dumpable, so its table \
             stays this uid's and nothing is sealed (fs.suid_dumpable = {setting}; 1 keeps \
             such an exec dumpable, 0 and 2 do not)"
        ));
    }

    let scan = super::super::scan_file_holders(&held, &[me]).expect("scan");
    assert!(scan.holds(o), "the readable copy: {scan:?}");
    assert!(
        !scan.sealed.contains(&o) && !scan.unreadable.contains(&o),
        "{scan:?}"
    );
    assert!(scan.sealed.contains(&s), "the non-dumpable copy: {scan:?}");
    assert!(!scan.holds(s) && !scan.unreadable.contains(&s), "{scan:?}");
    let other = super::super::scan_file_holders(&unheld, &[me]).expect("scan");
    assert!(!other.holds(o), "a file it does not hold: {other:?}");
    assert!(matches!(table(Path::new(PROC), o), Table::Listed(_)));
    assert!(matches!(table(Path::new(PROC), s), Table::Sealed));
    let files = super::super::open_files(o).expect("its open files");
    assert!(files.contains(&held), "{files:?}");
    assert_eq!(
        super::super::open_files(s).map_err(|e| e.kind()),
        Err(ErrorKind::PermissionDenied)
    );
    assert_eq!(
        owner(format!("{PROC}/{s}")),
        Some(crate::peer::our_uid()),
        "a non-dumpable process's directory stays its effective uid's"
    );
    let ours = super::super::uid_pids().expect("listing");
    assert!(ours.contains(&o) && ours.contains(&s), "{ours:?}");
}

/// A running program this test holds stdin of, killed and reaped when it drops, so a failed
/// assertion or a hang detector leaves nothing running.
struct Held(crate::spawnfd::Child);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A process of this uid's that execs a set-user-ID program (`su`, left waiting for a
/// password on a pipe this test holds) runs as root: the kernel makes its directory and its
/// table root's, so the table alone is unknown, not sealed, and [`super::read_of`] counts
/// it outside a scan of this uid, whose listing no longer names it. `su` is killed while it
/// waits, before it reads a password. COULD NOT RUN without a set-user-ID root
/// `/usr/bin/su`. Negative control: [`table`] alone answers `Unreadable`.
#[test]
fn a_process_that_became_a_set_user_id_program_reads_left_not_unknown() {
    crate::host::could_not_run_as_root("a set-user-ID program that runs as another user");
    let su = Path::new("/usr/bin/su");
    if !std::fs::metadata(su).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o4000 != 0) {
        crate::host::could_not_run("this host has no set-user-ID root /usr/bin/su");
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let held = tmp.path().join("held");
    std::fs::write(&held, b"").unwrap();
    let meta = std::fs::metadata(&held).unwrap();
    let key = (meta.dev(), meta.ino());
    let me = crate::peer::our_uid();

    let su = Held(
        crate::spawnfd::command(su)
            .args(["root", "-c", "true"])
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn su"),
    );
    let pid = su.0.id();
    let dir = format!("{PROC}/{pid}");
    let started = Instant::now();
    while std::fs::metadata(&dir).map(|m| m.uid()).ok() != Some(0) {
        assert!(started.elapsed() < HANG, "su never ran as root");
        crate::deadline::poll_yield();
    }

    let read = super::read_of(Path::new(PROC), pid, me, |table| super::holds(table, key));
    let alone = table(Path::new(PROC), pid);
    let listed = super::uid_pids_in(Path::new(PROC), me).expect("listing");
    drop(su);
    assert_eq!(read, super::Read::Left, "outside this uid's scan");
    assert!(matches!(alone, Table::Unreadable(_)), "{alone:?}");
    assert!(!listed.contains(&pid), "no longer listed as this uid's");
}
