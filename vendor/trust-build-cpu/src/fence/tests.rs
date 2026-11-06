use std::io::{Read, Write};
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::*;

// A concurrent fork briefly inherits other tests' open descriptions before
// exec closes them. Serialize these lock-lifetime assertions against that fork.
static SERIAL: Mutex<()> = Mutex::new(());

struct ChildGuard(Child);
impl Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn fixture() -> (tempfile::TempDir, Vec<File>) {
    let directory = tempfile::tempdir().unwrap();
    let files = ["slot-0", "slot-1", "slot-7", "admission.lock"]
        .into_iter()
        .map(|name| {
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(directory.path().join(name))
                .unwrap();
            file.lock_shared().unwrap();
            file
        })
        .collect();
    (directory, files)
}

fn copies(files: &[File]) -> Vec<File> {
    files.iter().map(|file| file.try_clone().unwrap()).collect()
}

fn probe(directory: &Path, name: &str) -> File {
    OpenOptions::new().read(true).write(true).open(directory.join(name)).unwrap()
}

fn assert_released(directory: &Path, name: &str) {
    let file = probe(directory, name);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if file.try_lock().is_ok() {
            return;
        }
        // Other test modules may fork concurrently and briefly inherit a
        // description before their exec closes it. Persistent retention fails.
        assert!(Instant::now() < deadline, "fence remained held: {name}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn complete_shared_fence_including_historical_slots_is_retained() {
    let _serial = SERIAL.lock().unwrap();
    let (directory, files) = fixture();
    let fence = LegacyFence::from_inherited(directory.path(), 2, copies(&files)).unwrap();
    drop(files);
    assert_eq!(fence.files().len(), 4);
    for name in ["slot-0", "slot-1", "slot-7", "admission.lock"] {
        assert!(probe(directory.path(), name).try_lock().is_err());
    }
    drop(fence);
    for name in ["slot-0", "slot-1", "slot-7", "admission.lock"] {
        assert_released(directory.path(), name);
    }
}

#[test]
fn exclusive_original_is_rejected_without_downgrading_it() {
    let _serial = SERIAL.lock().unwrap();
    let (directory, files) = fixture();
    files[0].unlock().unwrap();
    files[0].lock().unwrap();
    assert!(LegacyFence::from_inherited(directory.path(), 2, copies(&files)).is_err());
    // A fresh shared description still conflicts: validation did not turn
    // the original exclusive reservation into a shared native fence.
    assert!(probe(directory.path(), "slot-0").try_lock_shared().is_err());
    files[0].unlock().unwrap();
    probe(directory.path(), "slot-0").try_lock_shared().unwrap();
}

#[test]
fn missing_duplicate_and_foreign_descriptors_are_rejected() {
    let _serial = SERIAL.lock().unwrap();
    let (directory, files) = fixture();
    let mut missing = copies(&files);
    missing.pop();
    assert!(LegacyFence::from_inherited(directory.path(), 2, missing).is_err());
    let mut duplicate = copies(&files);
    duplicate[0] = files[1].try_clone().unwrap();
    assert!(LegacyFence::from_inherited(directory.path(), 2, duplicate).is_err());
    let foreign = tempfile::tempfile().unwrap();
    let mut replaced = copies(&files);
    replaced[0] = foreign;
    assert!(LegacyFence::from_inherited(directory.path(), 2, replaced).is_err());
}

#[test]
fn child_retains_fence_after_all_parent_copies_close() {
    let _serial = SERIAL.lock().unwrap();
    let (directory, files) = fixture();
    let descriptors: Vec<_> = files.iter().map(AsRawFd::as_raw_fd).collect();
    let ready = directory.path().join("child-ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "fence::tests::inherited_fence_child", "--nocapture"])
        .env("TRUST_FENCE_TEST_DIRECTORY", directory.path())
        .env("TRUST_FENCE_TEST_READY", &ready)
        .env(
            "TRUST_FENCE_TEST_FDS",
            descriptors.iter().map(ToString::to_string).collect::<Vec<_>>().join(","),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    // SAFETY: after fork this closure only clears CLOEXEC on the exact live
    // fixture descriptors. It allocates nothing and performs no locking.
    unsafe {
        command.pre_exec(move || {
            for fd in &descriptors {
                if libc::fcntl(*fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = ChildGuard(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fence child did not validate its inherited descriptors");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(files);
    for name in ["slot-0", "slot-1", "slot-7", "admission.lock"] {
        assert!(probe(directory.path(), name).try_lock().is_err());
    }
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fence child did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(child.wait().unwrap().success());
    for name in ["slot-0", "slot-1", "slot-7", "admission.lock"] {
        assert_released(directory.path(), name);
    }
}

#[test]
fn inherited_fence_child() {
    let _serial = SERIAL.lock().unwrap();
    let Some(directory) = std::env::var_os("TRUST_FENCE_TEST_DIRECTORY") else {
        return;
    };
    let descriptors = std::env::var("TRUST_FENCE_TEST_FDS").unwrap();
    let files = descriptors
        .split(',')
        .map(|fd| {
            let fd = fd.parse().unwrap();
            // SAFETY: the parent explicitly passed these live fixture FDs
            // through exec; this child adopts each exactly once.
            unsafe { File::from_raw_fd(fd) }
        })
        .collect();
    let fence = LegacyFence::from_inherited(Path::new(&directory), 2, files).unwrap();
    std::fs::write(std::env::var_os("TRUST_FENCE_TEST_READY").unwrap(), b"ready").unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    drop(fence);
}
