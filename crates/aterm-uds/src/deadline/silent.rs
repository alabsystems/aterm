// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support (test and debug builds, never release ones): whether a macOS service a test needs answers, and the end of a test that needs one that does not.

use std::sync::OnceLock;
use std::time::Duration;

use super::{Refused, TRUST};
use crate::host::could_not_run;

/// How long the probe waits for `trustd`: about a hundred times its honest answer, and half a test's one-minute hang detector.
pub const PROBE_DEADLINE: Duration = Duration::from_secs(30);

/// Whether macOS's `trustd` does not answer (asked once per process).
#[must_use]
pub fn trust_service_silent() -> bool {
    static SILENT: OnceLock<bool> = OnceLock::new();
    *SILENT.get_or_init(|| {
        cfg!(target_vendor = "apple")
            && TRUST.run(PROBE_DEADLINE, PROBE_DEADLINE, probe) == Err(Refused::Unanswered)
    })
}

/// End the calling test COULD NOT RUN when macOS's `trustd` does not answer ([`trust_service_silent`]): every code-signing and TLS check waits on it, so the silence is the machine's, not the code's.
#[track_caller]
pub fn require_trust_service() {
    if trust_service_silent() {
        could_not_run(
            "macOS's trust service (trustd) did not answer, so no code-signing or TLS check \
             ran here; rebooting the Mac clears a wedged trustd",
        );
    }
}

/// End the calling test COULD NOT RUN when `out` is an `hdiutil` run that failed with `Operation timed out` (a wedged disk-image service, after about 121 s).
#[track_caller]
pub fn require_disk_image_service(out: &std::process::Output) {
    if timed_out(out) {
        could_not_run(
            "macOS's disk-image service (hdiutil) did not answer, so no disk-image check ran \
             here; rebooting the Mac clears a wedged disk-image service",
        );
    }
}

/// Whether `out` is a failed `hdiutil` run that timed out.
fn timed_out(out: &std::process::Output) -> bool {
    !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("Operation timed out")
}

/// How long a test's own `codesign` may run: a hang detector, never a budget.
pub const CODESIGN_HANG: Duration = Duration::from_secs(60);

/// Sign `path` ad hoc (`codesign --force --sign -`), as a test does to a copy of an Apple binary so the kernel lets it run off the system volume.
#[track_caller]
pub fn sign_ad_hoc(path: &std::path::Path) {
    let out = super::output_until(
        crate::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(path),
        std::time::Instant::now() + CODESIGN_HANG,
    )
    .unwrap_or_else(|e| panic!("codesign --force --sign - {}: {e}", path.display()));
    assert!(
        out.status.success(),
        "codesign --force --sign - {}: {}: {}",
        path.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// The kept stand-in of the Apple binary `from`, signed ad hoc ([`sign_ad_hoc`]), for a test to exec IN PLACE: it runs off the system volume under its own identity (the kernel kills a copy that keeps Apple's signature, and hides a platform binary's environment from other processes).
#[must_use]
#[track_caller]
pub fn stand_in(from: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_vendor = "apple")]
    {
        signed_stand_in(&stand_in_cache(), from, &mut |fresh| {
            require_trust_service();
            sign_ad_hoc(fresh);
        })
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        from.to_path_buf()
    }
}

/// Copy the Apple binary `from` to `to`, signed ad hoc: a plain copy of its [`stand_in`], for a test that needs the executable at a path of its own (in a bundle, renamed over).
#[track_caller]
pub fn ad_hoc_copy(from: &std::path::Path, to: &std::path::Path) {
    let from = stand_in(from);
    std::fs::copy(&from, to)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", from.display(), to.display()));
}

/// A directory holding programs a test copied and runs ([`ad_hoc_copy`], a copy of the test binary, a bundle around one), removed when dropped only once [`KeptUntilRan::ran`] said every child started from a file under it has shown it ran ([`prints`], [`cat_echoes`], a file the copy wrote).
#[derive(Debug)]
pub struct KeptUntilRan {
    dir: std::path::PathBuf,
    ran: bool,
}

impl KeptUntilRan {
    /// Take over removing `dir`: a directory the test made, or an `aterm_tempfile::TempDir`'s `keep()`.
    #[must_use]
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            ran: false,
        }
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.dir
    }

    /// Every child started from a file under the directory has shown it ran: it goes when dropped.
    pub const fn ran(&mut self) {
        self.ran = true;
    }
}

impl Drop for KeptUntilRan {
    fn drop(&mut self) {
        if self.ran {
            let _ = std::fs::remove_dir_all(&self.dir);
            return;
        }
        let kept = format!(
            "KEPT {}: a program copied there was started and never showed it ran, so macOS may \
             still be assessing its first exec; remove it once no process runs from it",
            self.dir.display()
        );
        if std::thread::panicking() {
            eprintln!("{kept}");
        } else {
            panic!("{kept} (the test ended without calling `KeptUntilRan::ran`)");
        }
    }
}

/// How long [`cat_echoes`] waits for the echo: the one-minute hang detector, never a budget.
pub const ECHO_HANG: Duration = Duration::from_secs(60);

/// The proof [`ad_hoc_copy`] asks for before a copied executable goes: `child`, spawned from the copy with piped stdout, writes the line `line` (lines before it are skipped) within `hang` ([`ECHO_HANG`] for a copy of a small Apple binary, [`FIRST_EXEC_HANG`](super::FIRST_EXEC_HANG) for a copy of the test binary), so its exec's assessment is done and it runs from its file.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
pub fn prints(child: &mut crate::spawnfd::Child, line: &str, hang: Duration) {
    use std::io::BufRead as _;
    use std::sync::mpsc::RecvTimeoutError;
    let Some(from) = child.stdout.take() else {
        refused(child, line, "spawn it with piped stdout");
    };
    let (tx, seen) = std::sync::mpsc::channel();
    let wanted = line.as_bytes().to_vec();
    std::thread::spawn(move || {
        let mut from = std::io::BufReader::new(from);
        let mut tx = Some(tx);
        let mut got = Vec::new();
        loop {
            got.clear();
            match from.read_until(b'\n', &mut got) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if got.strip_suffix(b"\n") == Some(wanted.as_slice())
                        && let Some(tx) = tx.take()
                    {
                        let _ = tx.send(());
                    }
                }
            }
        }
    });
    match seen.recv_timeout(hang) {
        Ok(()) => {}
        Err(RecvTimeoutError::Disconnected) => refused(child, line, "its output ended first"),
        Err(RecvTimeoutError::Timeout) => refused(child, line, &format!("not within {hang:?}")),
    }
}

/// [`prints`]' and [`cat_echoes`]' end without their proof: `child` killed and reaped, its file
/// left, the panic naming the line and why.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
fn refused(child: &mut crate::spawnfd::Child, line: &str, why: &str) -> ! {
    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "the copied program (pid {}) never wrote {line:?}: {why}",
        child.id()
    );
}

/// [`prints`] for a copy of `cat`: `child`, spawned with piped stdin and stdout, echoes a line written to it within [`ECHO_HANG`].
#[cfg(not(target_arch = "wasm32"))]
#[cfg(unix)]
#[track_caller]
pub fn cat_echoes(child: &mut crate::spawnfd::Child) -> std::process::ChildStdin {
    use std::io::Write as _;
    let (Some(mut to), true) = (child.stdin.take(), child.stdout.is_some()) else {
        refused(child, "ran", "spawn it with piped stdin and stdout");
    };
    if let Err(e) = to.write_all(b"ran\n") {
        refused(child, "ran", &format!("write to it: {e}"));
    }
    prints(child, "ran", ECHO_HANG);
    to
}

/// Where [`stand_in`] keeps its signed stand-ins: the per-user temporary directory, which
/// no other user can write.
#[cfg(target_vendor = "apple")]
fn stand_in_cache() -> std::path::PathBuf {
    std::env::temp_dir().join("aterm-ad-hoc-stand-ins")
}

/// Where the stand-in of this version of `from` is kept in `cache`: its name holds `from`'s
/// size, modification time and inode, which an OS update that replaces the binary changes.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
#[track_caller]
fn kept_at(cache: &std::path::Path, from: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
    let name = from
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("stand-in");
    cache.join(format!(
        "{name}-{}-{}.{}-{}",
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ino()
    ))
}

/// The copy of `from` in `cache` that `sign` signed ([`kept_at`]), made the first time this
/// version of `from` is asked for.
#[cfg(all(unix, any(target_vendor = "apple", test)))]
#[track_caller]
fn signed_stand_in(
    cache: &std::path::Path,
    from: &std::path::Path,
    sign: &mut dyn FnMut(&std::path::Path),
) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static FRESH: AtomicU64 = AtomicU64::new(0);
    let signed = kept_at(cache, from);
    if signed.is_file() {
        return signed;
    }
    std::fs::create_dir_all(cache).unwrap_or_else(|e| panic!("create {}: {e}", cache.display()));
    let name = from
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("stand-in");
    let fresh = cache.join(format!(
        ".{name}.{}.{}",
        std::process::id(),
        FRESH.fetch_add(1, Ordering::Relaxed)
    ));
    struct Fresh<'a>(&'a std::path::Path);
    impl Drop for Fresh<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _fresh = Fresh(&fresh);
    std::fs::copy(from, &fresh)
        .unwrap_or_else(|e| panic!("copy {} to {}: {e}", from.display(), fresh.display()));
    sign(&fresh);
    match std::fs::hard_link(&fresh, &signed) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => panic!("place {} at {}: {e}", fresh.display(), signed.display()),
    }
    signed
}

/// One certificate-chained code check; its verdict is discarded.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn probe() {
    use core::ffi::c_void;
    type CFTypeRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFURLCreateFromFileSystemRepresentation(
            alloc: CFTypeRef,
            buffer: *const u8,
            len: isize,
            is_directory: u8,
        ) -> CFTypeRef;
    }
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecStaticCodeCreateWithPath(path: CFTypeRef, flags: u32, out: *mut CFTypeRef) -> i32;
        fn SecStaticCodeCheckValidity(code: CFTypeRef, flags: u32, req: CFTypeRef) -> i32;
    }

    const PATH: &[u8] = b"/usr/bin/true";
    // SAFETY: `PATH` is `len` live bytes, copied by the call.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            PATH.as_ptr(),
            PATH.len() as isize,
            0,
        )
    };
    if url.is_null() {
        return;
    }
    let mut code: CFTypeRef = std::ptr::null();
    // SAFETY: `url` is a live CFURL; an out-parameter for one new reference.
    let created = unsafe { SecStaticCodeCreateWithPath(url, 0, &mut code) };
    if created == 0 && !code.is_null() {
        // SAFETY: `code` is a live static code object; no requirement.
        let _ = unsafe { SecStaticCodeCheckValidity(code, 0, std::ptr::null()) };
        // SAFETY: we own exactly one reference.
        unsafe { CFRelease(code) };
    }
    // SAFETY: we own exactly one reference.
    unsafe { CFRelease(url) };
}

#[cfg(not(target_vendor = "apple"))]
fn probe() {}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{ExitStatus, Output};

    fn run(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// Only a failed run that timed out is the service's silence.
    #[test]
    fn only_a_timed_out_failure_is_a_silent_disk_image_service() {
        use super::timed_out as silent;
        assert!(silent(&run(
            1,
            "hdiutil: create failed - Operation timed out\n"
        )));
        assert!(!silent(&run(
            1,
            "hdiutil: create failed - No space left on device\n"
        )));
        assert!(!silent(&run(1, "")));
        assert!(!silent(&run(0, "Operation timed out\n")));
    }

    /// A stand-in is signed once per version of its binary: a second ask for the same
    /// binary copies the signed one, and a changed binary is signed again.
    #[test]
    fn a_stand_in_is_signed_once_per_version_of_its_binary() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let cache = dir.path().join("cache");
        let binary = dir.path().join("tool");
        std::fs::write(&binary, b"version one").expect("the binary");
        let mut signed = Vec::new();
        let mut sign = |fresh: &std::path::Path| {
            signed.push(fresh.to_path_buf());
            let mut bytes = std::fs::read(fresh).expect("the fresh copy");
            bytes.extend_from_slice(b" signed");
            std::fs::write(fresh, bytes).expect("sign the fresh copy");
        };
        let first = super::signed_stand_in(&cache, &binary, &mut sign);
        let again = super::signed_stand_in(&cache, &binary, &mut sign);
        assert_eq!(first, again, "one stand-in per version");
        assert_eq!(std::fs::read(&first).unwrap(), b"version one signed");
        std::fs::write(&binary, b"version two, longer").expect("the binary changes");
        let changed = super::signed_stand_in(&cache, &binary, &mut sign);
        assert_ne!(changed, first, "a new version is a new stand-in");
        assert_eq!(
            std::fs::read(&changed).unwrap(),
            b"version two, longer signed"
        );
        assert_eq!(signed.len(), 2, "signed once per version: {signed:?}");
        let left: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with('.'))
            .collect();
        assert!(left.is_empty(), "no unplaced copy left: {left:?}");
    }

    /// A stand-in once placed is never replaced: a caller that finds one placed while it
    /// signed its own (another test process's, which may be running it while macOS assesses
    /// it by path) keeps the placed one, file and all, and leaves no copy of its own behind.
    #[test]
    fn a_placed_stand_in_is_never_replaced() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let cache = dir.path().join("cache");
        let binary = dir.path().join("tool");
        std::fs::write(&binary, b"bytes").expect("the binary");
        let kept = super::kept_at(&cache, &binary);
        let mut placed = None;
        let got = super::signed_stand_in(&cache, &binary, &mut |fresh| {
            std::fs::write(fresh, b"ours").expect("sign ours");
            std::fs::write(&kept, b"theirs").expect("another process places its own");
            placed = Some(std::fs::metadata(&kept).expect("theirs").ino());
        });
        assert_eq!(got, kept);
        assert_eq!(
            std::fs::read(&kept).unwrap(),
            b"theirs",
            "the placed one stays"
        );
        assert_eq!(
            Some(std::fs::metadata(&kept).unwrap().ino()),
            placed,
            "the same file, not a replacement"
        );
        assert_eq!(
            std::fs::read_dir(&cache).unwrap().count(),
            1,
            "no copy of our own left"
        );
        let other = dir.path().join("other");
        std::fs::write(&other, b"other bytes").expect("another binary");
        let own = super::signed_stand_in(&cache, &other, &mut |fresh| {
            std::fs::write(fresh, b"ours").expect("sign ours");
        });
        assert_eq!(
            std::fs::read(&own).unwrap(),
            b"ours",
            "nothing placed: ours is"
        );
    }

    /// Only an echo of its line is a `cat`'s proof that it runs: `cat` itself passes and runs
    /// on until its stdin closes.
    #[test]
    fn only_an_echo_proves_a_cat_runs() {
        use crate::spawnfd::{Stdio, command};
        let piped = |program: &str, args: &[&str]| {
            command(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn")
        };
        let mut cat = piped("/bin/cat", &[]);
        let stdin = super::cat_echoes(&mut cat);
        assert_eq!(cat.try_wait().expect("poll"), None, "it runs on");
        drop(stdin);
        assert!(cat.wait().expect("reap").success(), "and ends at its EOF");
        for (program, args) in [
            ("/bin/sh", &["-c", "read line; echo other"][..]),
            ("/usr/bin/true", &[][..]),
        ] {
            let mut child = piped(program, args);
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::cat_echoes(&mut child)
            }));
            assert!(refused.is_err(), "{program} {args:?} is no proof");
            assert!(
                child.try_wait().expect("poll").is_some(),
                "{program}: killed and reaped"
            );
        }
        let mut unpiped = command("/bin/cat")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn");
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::cat_echoes(&mut unpiped)
        }));
        assert!(refused.is_err(), "no pipes, no proof");
        assert!(
            unpiped.try_wait().expect("poll").is_some(),
            "killed and reaped"
        );
    }

    /// Only the line itself is a copied program's proof that it runs: a child that writes other
    /// lines first and then the line passes, and its output after it is read on, so a child
    /// that writes a megabyte more still ends.
    #[test]
    fn only_its_line_proves_a_copied_program_runs() {
        use crate::spawnfd::{Stdio, command};
        let piped = |script: &str| {
            command("/bin/sh")
                .args(["-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn")
        };
        let mut child = piped("echo running 1 test; echo parked; head -c 1048576 /dev/zero");
        super::prints(&mut child, "parked", super::ECHO_HANG);
        let hang = std::time::Instant::now() + super::ECHO_HANG;
        while child.try_wait().expect("poll").is_none() {
            assert!(
                std::time::Instant::now() < hang,
                "its output after the line was never read"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for (script, hang) in [
            ("echo parked-not; echo other", super::ECHO_HANG),
            ("exec sleep 60", std::time::Duration::from_millis(200)),
        ] {
            let mut child = piped(script);
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::prints(&mut child, "parked", hang);
            }));
            assert!(refused.is_err(), "{script} is no proof");
            assert!(
                child.try_wait().expect("poll").is_some(),
                "{script}: killed and reaped"
            );
        }
        let mut unpiped = command("/bin/sh")
            .args(["-c", "echo parked"])
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn");
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::prints(&mut unpiped, "parked", super::ECHO_HANG);
        }));
        assert!(refused.is_err(), "no pipe, no proof");
        assert!(
            unpiped.try_wait().expect("poll").is_some(),
            "killed and reaped"
        );
    }

    /// A directory of copies goes when dropped only once they ran: dropped by a failing test's
    /// unwind it stays, and dropped by a test that never said so it stays and fails that test.
    #[test]
    fn a_directory_of_copies_goes_only_once_they_ran() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let laid = |name: &str| {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(dir.join("bin")).expect("the directory");
            std::fs::write(dir.join("bin").join("copy"), b"copied").expect("the copy");
            dir
        };
        let proven = laid("proven");
        let mut kept = super::KeptUntilRan::new(&proven);
        assert_eq!(kept.path(), proven);
        kept.ran();
        drop(kept);
        assert!(!proven.exists(), "its copies ran: it goes");

        let failing = laid("failing");
        let unwound = std::panic::catch_unwind(|| {
            let _kept = super::KeptUntilRan::new(&failing);
            panic!("a hang detector gave up");
        });
        assert!(unwound.is_err());
        assert!(
            failing.join("bin").join("copy").is_file(),
            "a failing test's unwind keeps it"
        );

        let forgot = laid("forgot");
        let refused = std::panic::catch_unwind(|| drop(super::KeptUntilRan::new(&forgot)))
            .expect_err("a test that never said its copies ran fails");
        let why = refused
            .downcast_ref::<String>()
            .expect("the guard panics with its words");
        assert!(why.contains(&forgot.display().to_string()), "{why}");
        assert!(
            forgot.join("bin").join("copy").is_file(),
            "and keeps the directory"
        );
    }

    /// A signing that panics leaves no stand-in, so the next ask signs again.
    #[test]
    fn a_refused_signing_leaves_no_stand_in() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let cache = dir.path().join("cache");
        let binary = dir.path().join("tool");
        std::fs::write(&binary, b"bytes").expect("the binary");
        let refused = std::panic::catch_unwind(|| {
            super::signed_stand_in(&cache, &binary, &mut |_| panic!("codesign refused"));
        });
        assert!(refused.is_err());
        assert_eq!(
            std::fs::read_dir(&cache).unwrap().count(),
            0,
            "nothing left"
        );
        let mut asked = 0;
        super::signed_stand_in(&cache, &binary, &mut |_| asked += 1);
        assert_eq!(asked, 1, "the next ask signs");
    }

    /// The silence ends the test COULD NOT RUN; an answer returns.
    #[test]
    fn a_silent_disk_image_service_ends_the_test_could_not_run() {
        super::require_disk_image_service(&run(1, "hdiutil: No space left on device\n"));
        let refused = std::panic::catch_unwind(|| {
            super::require_disk_image_service(&run(1, "hdiutil: Operation timed out\n"));
        })
        .expect_err("a timed-out run ends the test");
        let why = refused
            .downcast_ref::<String>()
            .expect("could_not_run panics with its words");
        assert!(why.starts_with(crate::host::COULD_NOT_RUN), "{why}");
    }
}
