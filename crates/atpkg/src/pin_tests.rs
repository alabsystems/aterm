// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-pin-{label}-"))
}

/// What answers this thread's pin-file reads in place of the file, by their count from 0
/// ([`instead_of_read`]).
type Reads = Box<dyn FnMut(usize) -> Option<io::Error>>;

thread_local! {
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_SET_ASIDE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static READS: std::cell::RefCell<Option<(usize, Reads)>> =
        const { std::cell::RefCell::new(None) };
}

/// A read of a pin-format file ([`read_of`]): `read`, or the error [`with_reads`] answers it
/// with on this thread.
pub(super) fn instead_of_read(read: io::Result<String>) -> io::Result<String> {
    let instead = READS.with(|slot| {
        slot.borrow_mut().as_mut().and_then(|(count, reads)| {
            let instead = reads(*count);
            *count += 1;
            instead
        })
    });
    instead.map_or(read, Err)
}

/// Run `f` with each pin-file read this thread makes answered by `reads` (given each read's
/// count from 0) where it gives an error, and by the file where it gives `None`.
fn with_reads<T>(
    reads: impl FnMut(usize) -> Option<io::Error> + 'static,
    f: impl FnOnce() -> T,
) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            READS.with(|slot| slot.take());
        }
    }
    READS.with(|slot| slot.replace(Some((0, Box::new(reads)))));
    let _clear = Clear;
    f()
}

pub(super) fn before_publish() {
    let callback = BEFORE_PUBLISH.with(|slot| slot.take());
    if let Some(callback) = callback {
        callback();
    }
}

/// Run between an unattended read that failed and the pin lock it then takes ([`unattended`]).
pub(super) fn before_set_aside() {
    let callback = BEFORE_SET_ASIDE.with(|slot| slot.take());
    if let Some(callback) = callback {
        callback();
    }
}

#[test]
fn concurrent_pin_edits_preserve_both_changes() {
    use std::sync::mpsc;
    use std::time::Duration;

    for pinning in [true, false] {
        let l = layout("concurrent-edits");
        set_pinned(&l, "keep", true).unwrap();
        if !pinning {
            set_pinned(&l, "ay", true).unwrap();
            set_pinned(&l, "trust", true).unwrap();
        }
        let lock_path = l.prefix.join("pins.lock");
        let open_lock = || {
            fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .unwrap()
        };
        drop(open_lock());
        let (ready, first_read) = mpsc::channel();
        let (go, release) = mpsc::channel();
        let held = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                BEFORE_PUBLISH.with(|slot| {
                    slot.replace(Some(Box::new(move || {
                        ready.send(()).unwrap();
                        release.recv_timeout(Duration::from_secs(60)).unwrap();
                    })));
                });
                set_pinned(&l, "ay", pinning).unwrap()
            });
            first_read.recv_timeout(Duration::from_secs(60)).unwrap();
            let held = match crate::lock::Flock::try_lock(open_lock()) {
                Err(std::fs::TryLockError::WouldBlock) => true,
                Ok(guard) => {
                    drop(guard);
                    false
                }
                Err(error) => panic!("probe the pin writer lock: {error}"),
            };
            let second = scope.spawn(|| set_pinned(&l, "trust", pinning).unwrap());
            // Without serialization, force the second publication before the first stale
            // snapshot. With serialization, release the first so the second can read it.
            if held {
                go.send(()).unwrap();
                assert!(first.join().unwrap());
                assert!(second.join().unwrap());
            } else {
                assert!(second.join().unwrap());
                go.send(()).unwrap();
                assert!(first.join().unwrap());
            }
            held
        });
        let expected = if pinning {
            BTreeSet::from(["ay".to_string(), "keep".to_string(), "trust".to_string()])
        } else {
            BTreeSet::from(["keep".to_string()])
        };
        assert_eq!(pinned_set(&l).unwrap(), expected, "pinning={pinning}");
        assert!(
            held,
            "the first snapshot must remain locked through publication"
        );
    }
}

#[test]
fn an_unreadable_pin_file_cannot_be_replaced_by_a_pin_edit() {
    let l = layout("unreadable-edit");
    let original = b"ay\n\xff";
    fs::write(pins_path(&l), original).unwrap();
    assert!(pinned_set(&l).is_err());
    assert!(is_pinned(&l, "ay").is_err());
    assert!(set_pinned(&l, "trust", true).is_err());
    assert_eq!(fs::read(pins_path(&l)).unwrap(), original);
    assert!(set_pinned(&l, "ay", false).is_err());
    assert_eq!(fs::read(pins_path(&l)).unwrap(), original);
}

#[test]
fn set_is_pinned_roundtrip_and_0600() {
    let l = layout("roundtrip");
    assert!(!is_pinned(&l, "ay").unwrap());
    assert!(set_pinned(&l, "ay", true).unwrap(), "first pin is a change");
    assert!(is_pinned(&l, "ay").unwrap());
    #[cfg(unix)]
    {
        let mode = fs::metadata(pins_path(&l)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "pin file is 0600");
    }
    assert!(
        !set_pinned(&l, "ay", true).unwrap(),
        "re-pinning is a no-op change"
    );
    assert!(set_pinned(&l, "ay", false).unwrap(), "unpin is a change");
    assert!(!is_pinned(&l, "ay").unwrap());
    let _ = fs::remove_dir_all(&l.prefix);
}

#[test]
fn rejects_unsafe_program_names() {
    let l = layout("unsafe");
    for bad in [
        "", ".", "..", "a/b", "a\\b", "x\ny", "x\ry", "x\0y", " ay", "ay ", "\tay", "   ",
    ] {
        let err = set_pinned(&l, bad, true).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidInput,
            "{bad:?} must be rejected"
        );
    }
    assert!(!pins_path(&l).exists(), "a rejected name writes no file");
    let _ = fs::remove_dir_all(&l.prefix);
}

#[test]
fn pinned_set_reads_all() {
    let l = layout("reads-all");
    set_pinned(&l, "ay", true).unwrap();
    set_pinned(&l, "trust", true).unwrap();
    assert_eq!(
        pinned_set(&l).unwrap(),
        BTreeSet::from(["ay".to_string(), "trust".to_string()])
    );
    let _ = fs::remove_dir_all(&l.prefix);
}

#[test]
fn invalid_stored_names_preserve_the_entire_pin_file() {
    let l = layout("invalid-stored");
    let original = "ay\n../trust\n";
    fs::write(pins_path(&l), original).unwrap();
    assert_eq!(
        pinned_set(&l).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(set_pinned(&l, "ay", false).is_err());
    assert_eq!(fs::read_to_string(pins_path(&l)).unwrap(), original);
}

#[test]
fn oversized_sparse_and_overcount_pin_files_are_errors() {
    let l = layout("bounded");
    let path = pins_path(&l);
    let file = fs::File::create(&path).unwrap();
    file.set_len((MAX_PIN_FILE_BYTES + 1) as u64).unwrap();
    assert!(pinned_set(&l).is_err());
    assert!(set_pinned(&l, "ay", true).is_err());
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        (MAX_PIN_FILE_BYTES + 1) as u64
    );

    let exact_with_dup: String = (0..MAX_PINNED_PROGRAMS)
        .map(|index| format!("p{index}\n"))
        .chain(std::iter::once("p0\n".to_string()))
        .collect();
    fs::write(&path, exact_with_dup).unwrap();
    assert_eq!(
        pinned_set(&l).unwrap().len(),
        MAX_PINNED_PROGRAMS,
        "duplicate lines do not push a 2048-member set over the cap"
    );
    let err = set_pinned(&l, "p2048", true).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        pinned_set(&l).unwrap().len(),
        MAX_PINNED_PROGRAMS,
        "refusing the 2049th pin preserves all 2048 existing pins"
    );

    let body: String = (0..=MAX_PINNED_PROGRAMS)
        .map(|index| format!("p{index}\n"))
        .collect();
    fs::write(&path, &body).unwrap();
    assert!(pinned_set(&l).is_err(), "over-count is never partial");
    assert!(set_pinned(&l, "ay", true).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), body);
    let _ = fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn fifo_and_symlink_pin_files_return_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let l = layout("special");
    let path = pins_path(&l);
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    assert!(pinned_set(&l).is_err());
    assert!(set_pinned(&l, "ay", true).is_err());
    fs::remove_file(&path).unwrap();
    let target = l.prefix.join("target-pins");
    fs::write(&target, "ay\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(pinned_set(&l).is_err(), "pin links are not followed");
    assert!(set_pinned(&l, "trust", true).is_err());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "ay\n");
    let _ = fs::remove_dir_all(&l.prefix);
}

/// A scratch store whose packages log this thread binds to a scratch directory beside it.
struct Logged {
    layout: crate::testdir::ScratchLayout,
    logs: crate::testdir::Dir,
    _bound: crate::packages_log::test_bind::Guard,
}

fn logged(label: &str) -> Logged {
    let layout = layout(label);
    let logs = crate::testdir::dir(&format!("atpkg-pin-{label}-logs-"));
    let _bound = crate::packages_log::test_bind::bind(logs.to_path_buf(), layout.prefix.clone());
    Logged {
        layout,
        logs,
        _bound,
    }
}

/// The packages log's set-aside lines under `logs`.
fn set_aside_lines(logs: &Path) -> Vec<crate::packages_log::Entry> {
    crate::packages_log::read_tail(logs, 100)
        .into_iter()
        .filter(|e| e.kind == crate::packages_log::kind::SET_ASIDE)
        .collect()
}

/// The entries under `prefix` that `file` was set aside as, in name order.
fn kept_as(prefix: &Path, file: PinFile) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = fs::read_dir(prefix)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{}{UNREAD}", file.name())))
        })
        .collect();
    kept.sort();
    kept
}

/// One line says `file` was set aside as `kept` and why it did not read (`why`).
fn assert_one_set_aside_line(logs: &Path, file: &Path, kept: &Path, why: &str) {
    let lines = set_aside_lines(logs);
    assert!(
        lines.len() == 1
            && lines[0].get("file") == Some(file.to_str().unwrap())
            && lines[0].get("kept") == Some(kept.to_str().unwrap())
            && lines[0].get("error").is_some_and(|e| e.contains(why))
            && lines[0].get("unmoved").is_none(),
        "{lines:?}"
    );
}

/// A PINS FILE NAMING AN INVALID PROGRAM HOLDS NOTHING AND IS SET ASIDE WITH ITS BYTES: every
/// program updates, the file is renamed beside itself to `pins.unread-<unix seconds>`
/// byte for byte, one packages-log line names it and why, and later reads find no pins file
/// and write nothing more. Before, every program read as pinned for good.
/// NEGATIVE CONTROL: with `holds_update` back to `is_pinned(..).unwrap_or(true)` every program
/// holds and nothing is set aside.
#[test]
fn a_pins_file_with_an_invalid_name_holds_nothing_and_is_set_aside_byte_for_byte() {
    let fx = logged("invalid-set-aside");
    let l = &fx.layout;
    let original = b"ay\n../trust\nclaude\n";
    fs::write(pins_path(l), original).unwrap();
    let before = crate::flow::now_unix();
    for program in ["ay", "trust", "claude", "codex"] {
        assert!(!holds_update(l, program), "{program} must not be held");
    }
    let after = crate::flow::now_unix();
    let kept = kept_as(&l.prefix, PinFile::Pins);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let stamp: i64 = kept[0]
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("pins.unread-"))
        .and_then(|n| n.parse().ok())
        .unwrap();
    assert!(
        (before..=after).contains(&stamp),
        "{stamp} in {before}..={after}"
    );
    assert_eq!(fs::read(&kept[0]).unwrap(), original);
    assert!(
        fs::symlink_metadata(pins_path(l)).is_err(),
        "no pins file is left"
    );
    assert_one_set_aside_line(&fx.logs, &pins_path(l), &kept[0], "invalid program name");
    assert!(
        pinned_set(l).unwrap().is_empty(),
        "a later read finds no pins"
    );
    assert!(
        set_pinned(l, "claude", true).unwrap(),
        "a pin can be made again"
    );
    assert!(holds_update(l, "claude"));
    assert_eq!(
        set_aside_lines(&fx.logs).len(),
        1,
        "later reads write nothing"
    );
    assert_eq!(fs::read(&kept[0]).unwrap(), original, "the kept file stays");
}

/// A PINS FILE THAT IS A LINK, A FIFO OR OVER THE SIZE BOUND HOLDS NOTHING AND IS SET ASIDE AS
/// IT IS: the link itself is moved (its target is not read, followed or changed), the FIFO is
/// moved without blocking, and the oversized file keeps every byte; each writes one line.
/// NEGATIVE CONTROL: with `holds_update` back to `is_pinned(..).unwrap_or(true)` each holds and
/// stays where it was.
#[cfg(unix)]
#[test]
fn a_linked_fifo_or_oversized_pins_file_holds_nothing_and_is_set_aside_as_it_is() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::FileTypeExt as _;

    let link = logged("link-set-aside");
    let target = link.logs.join("target-pins");
    fs::write(&target, "ay\n").unwrap();
    std::os::unix::fs::symlink(&target, pins_path(&link.layout)).unwrap();
    assert!(!holds_update(&link.layout, "ay"));
    let kept = kept_as(&link.layout.prefix, PinFile::Pins);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(fs::symlink_metadata(&kept[0]).unwrap().is_symlink());
    assert_eq!(fs::read_link(&kept[0]).unwrap(), target);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "ay\n",
        "the target is untouched"
    );
    assert!(fs::symlink_metadata(pins_path(&link.layout)).is_err());
    assert_one_set_aside_line(&link.logs, &pins_path(&link.layout), &kept[0], "local pins");

    let fifo = logged("fifo-set-aside");
    let path_c = std::ffi::CString::new(pins_path(&fifo.layout).as_os_str().as_bytes()).unwrap();
    // SAFETY: `path_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    assert!(!holds_update(&fifo.layout, "ay"));
    let kept = kept_as(&fifo.layout.prefix, PinFile::Pins);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        fs::symlink_metadata(&kept[0])
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert_one_set_aside_line(&fifo.logs, &pins_path(&fifo.layout), &kept[0], "local pins");

    let big = logged("oversized-set-aside");
    let file = fs::File::create(pins_path(&big.layout)).unwrap();
    file.set_len((MAX_PIN_FILE_BYTES + 1) as u64).unwrap();
    drop(file);
    assert!(!holds_update(&big.layout, "ay"));
    let kept = kept_as(&big.layout.prefix, PinFile::Pins);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        fs::read(&kept[0]).unwrap(),
        vec![0u8; MAX_PIN_FILE_BYTES + 1]
    );
    assert!(fs::symlink_metadata(pins_path(&big.layout)).is_err());
    assert_one_set_aside_line(&big.logs, &pins_path(&big.layout), &kept[0], "local pins");
}

/// A PIN THAT WAS READ HOLDS AS IT ALWAYS DID: `claude` named in a readable pins file holds,
/// another program does not, and nothing is renamed or logged.
/// NEGATIVE CONTROL: with `holds_update` answering `false` for every program `claude` is not
/// held.
#[test]
fn a_readable_pin_holds_and_nothing_is_set_aside() {
    let fx = logged("readable-holds");
    let l = &fx.layout;
    fs::write(pins_path(l), "claude\n").unwrap();
    assert!(holds_update(l, "claude"));
    assert!(!holds_update(l, "codex"));
    assert_eq!(fs::read_to_string(pins_path(l)).unwrap(), "claude\n");
    assert!(kept_as(&l.prefix, PinFile::Pins).is_empty());
    assert!(set_aside_lines(&fx.logs).is_empty());
}

/// NO PINS FILE HOLDS NOTHING AND WRITES NOTHING: no line, no kept file, no lock file.
/// NEGATIVE CONTROL: with `set_of`'s confirmed-missing arm removed, the missing file is an
/// error and a line says it could not be moved.
#[test]
fn no_pins_file_holds_nothing_and_writes_nothing() {
    let fx = logged("no-pins");
    let l = &fx.layout;
    assert!(!holds_update(l, "claude"));
    assert!(!is_held(l, "claude"));
    assert!(set_aside_lines(&fx.logs).is_empty());
    assert_eq!(
        fs::read_dir(&l.prefix).unwrap().count(),
        0,
        "nothing written"
    );
}

/// A HOLDS FILE THAT DOES NOT READ MARKS NO HOLD AND IS SET ASIDE: `is_held`, a named install's
/// read, is false, the file is kept byte for byte as `harness-holds.unread-<n>`, one line names
/// it, the pins file is not touched, and a hold can be recorded again. Before, `aterm pkg install
/// <harness> --build <n>` failed for good on it.
/// NEGATIVE CONTROL: with `is_held` reading through `set_of(..).unwrap_or_default()` the file
/// stays and `set_held` keeps refusing.
#[test]
fn a_holds_file_that_does_not_read_marks_no_hold_and_is_set_aside() {
    let fx = logged("holds-set-aside");
    let l = &fx.layout;
    set_pinned(l, "hx-alab", true).unwrap();
    let pins = fs::read(pins_path(l)).unwrap();
    let holds = l.prefix.join(PinFile::Holds.name());
    let original = b"hx-alab\n\xff";
    fs::write(&holds, original).unwrap();
    assert!(!is_held(l, "hx-alab"));
    let kept = kept_as(&l.prefix, PinFile::Holds);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(fs::read(&kept[0]).unwrap(), original);
    assert!(fs::symlink_metadata(&holds).is_err());
    assert_one_set_aside_line(&fx.logs, &holds, &kept[0], "not UTF-8");
    assert_eq!(
        fs::read(pins_path(l)).unwrap(),
        pins,
        "the pins are not touched"
    );
    assert!(kept_as(&l.prefix, PinFile::Pins).is_empty());
    assert!(set_held(l, "hx-alab", true).unwrap());
    assert!(is_held(l, "hx-alab"));
}

/// TWO FILES SET ASIDE IN ONE SECOND ARE BOTH KEPT: the second takes `-2`, never the first's name.
/// NEGATIVE CONTROL: with `move_aside` renaming by `fs::rename` the second replaces the first.
#[test]
fn two_files_set_aside_in_one_second_are_both_kept() {
    let fx = logged("same-second");
    let l = &fx.layout;
    let now = 1_763_337_600;
    let why = io::Error::from(io::ErrorKind::InvalidData);
    fs::write(pins_path(l), b"first\n\xff").unwrap();
    assert_eq!(
        set_aside(l, PinFile::Pins, &why, now),
        Some(BTreeSet::new())
    );
    fs::write(pins_path(l), b"second\n\xff").unwrap();
    assert_eq!(
        set_aside(l, PinFile::Pins, &why, now),
        Some(BTreeSet::new())
    );
    let first = l.prefix.join(format!("pins.unread-{now}"));
    let second = l.prefix.join(format!("pins.unread-{now}-2"));
    assert_eq!(
        kept_as(&l.prefix, PinFile::Pins),
        vec![first.clone(), second.clone()]
    );
    assert_eq!(fs::read(&first).unwrap(), b"first\n\xff");
    assert_eq!(fs::read(&second).unwrap(), b"second\n\xff");
    assert_eq!(set_aside_lines(&fx.logs).len(), 2);
}

/// A PINS FILE THAT CANNOT BE MOVED HOLDS NOTHING AND IS LOGGED ONCE: in a read-only prefix every
/// read answers no pin, the file stays as it was, and one line per process says it did not read
/// and why it stayed.
/// NEGATIVE CONTROL: with `note_unmoved` logging on every call, the second read writes a second
/// line.
#[cfg(unix)]
#[test]
fn a_pins_file_that_cannot_be_moved_holds_nothing_and_is_logged_once() {
    let fx = logged("read-only");
    let l = &fx.layout;
    let original = b"ay\n\xff";
    fs::write(pins_path(l), original).unwrap();
    // The lock opens; the rename is what the prefix refuses.
    fs::write(l.prefix.join(LOCK), b"").unwrap();
    let mode = fs::metadata(&l.prefix).unwrap().permissions().mode();
    fs::set_permissions(&l.prefix, fs::Permissions::from_mode(0o555)).unwrap();
    let answers = [holds_update(l, "ay"), holds_update(l, "ay")];
    fs::set_permissions(&l.prefix, fs::Permissions::from_mode(mode)).unwrap();
    assert_eq!(answers, [false, false]);
    assert_eq!(fs::read(pins_path(l)).unwrap(), original, "the file stays");
    assert!(kept_as(&l.prefix, PinFile::Pins).is_empty());
    let lines = set_aside_lines(&fx.logs);
    assert!(
        lines.len() == 1
            && lines[0].get("file") == Some(pins_path(l).to_str().unwrap())
            && lines[0].get("kept").is_none()
            && lines[0].get("unmoved").is_some_and(|u| !u.is_empty()),
        "{lines:?}"
    );
}

/// Set, the test binary is [`standin_meets_a_damaged_pins_file`] over the scratch directory it
/// names (`prefix/` the store, `logs/` its packages log).
#[cfg(unix)]
const RACE: &str = "ATPKG_PIN_RACE_DIR";

/// Run by [`two_processes_meeting_a_damaged_pins_file_set_it_aside_once`] in a process of its own:
/// it reads the store's damaged pins file, writes `met-<pid>` once that read failed, waits for
/// its stdin to close before it takes the pin lock, and writes its answer to `answered-<pid>`.
#[cfg(unix)]
#[test]
fn standin_meets_a_damaged_pins_file() {
    let Some(dir) = std::env::var_os(RACE).map(PathBuf::from) else {
        return;
    };
    let layout = Layout {
        prefix: dir.join("prefix"),
    };
    let _bound = crate::packages_log::test_bind::bind(dir.join("logs"), layout.prefix.clone());
    let pid = std::process::id();
    let met = dir.join(format!("met-{pid}"));
    BEFORE_SET_ASIDE.with(|slot| {
        slot.replace(Some(Box::new(move || {
            fs::write(&met, "met").expect("say met");
            let mut rest = String::new();
            let _ = io::Read::read_to_string(&mut io::stdin(), &mut rest);
        })));
    });
    let held = holds_update(&layout, "ay");
    fs::write(dir.join(format!("answered-{pid}")), held.to_string()).expect("say the answer");
}

/// A stand-in process, killed if the test ends before it does.
#[cfg(unix)]
struct Standin(aterm_uds::spawnfd::Child);

#[cfg(unix)]
impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// TWO PROCESSES MEETING A DAMAGED PINS FILE AT ONCE SET IT ASIDE ONCE AND LOSE NOTHING: both
/// fail their read before either takes the pin lock; one renames the file, the other reads again
/// under the lock, finds no pins file and moves nothing. Both answer no pin, one file is kept
/// byte for byte, and one line is written.
/// NEGATIVE CONTROL: with `set_aside`'s read under the lock removed, the second process tries to
/// move a file that is gone and writes a second line saying it could not.
#[cfg(unix)]
#[test]
fn two_processes_meeting_a_damaged_pins_file_set_it_aside_once() {
    let dir = crate::testdir::dir("atpkg-pin-race-");
    let prefix = dir.join("prefix");
    fs::create_dir(&prefix).unwrap();
    let original = b"ay\n../trust\n";
    fs::write(prefix.join("pins"), original).unwrap();
    let spawn = || {
        Standin(
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "pin::tests::standin_meets_a_damaged_pins_file",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(RACE, dir.as_os_str())
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .expect("the stand-in"),
        )
    };
    let mut standins = [spawn(), spawn()];
    let count = |what: &str| {
        fs::read_dir(&*dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(what))
            .count()
    };
    aterm_spec::wait::until_while_descendants_work("both stand-ins met the damaged file", || {
        (count("met-") == 2).then_some(())
    });
    for standin in &mut standins {
        drop(standin.0.stdin.take());
    }
    for standin in &mut standins {
        let pid = standin.0.id();
        let status = aterm_spec::wait::until_while_working(pid, "the stand-in's end", || {
            standin.0.try_wait().ok().flatten()
        });
        assert!(status.success(), "{status}");
        assert_eq!(
            fs::read_to_string(dir.join(format!("answered-{pid}"))).unwrap(),
            "false"
        );
    }
    let kept = kept_as(&prefix, PinFile::Pins);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(fs::read(&kept[0]).unwrap(), original);
    assert!(fs::symlink_metadata(prefix.join("pins")).is_err());
    assert_one_set_aside_line(&dir.join("logs"), &prefix.join("pins"), &kept[0], "invalid");
}

/// The errors that say this process could not read just now, each as a read can return it.
fn passing_errors() -> Vec<io::Error> {
    vec![
        #[cfg(unix)]
        io::Error::from_raw_os_error(libc::EMFILE),
        #[cfg(unix)]
        io::Error::from_raw_os_error(libc::ENFILE),
        #[cfg(unix)]
        io::Error::from_raw_os_error(libc::ENOMEM),
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            "package metadata allocation refused",
        ),
    ]
}

/// A READ THIS PROCESS COULD NOT MAKE JUST NOW MOVES NOTHING AND HOLDS FOR THAT CALL: with every
/// read of a valid pins file (naming `claude`) and holds file (naming `hx-alab`) failing out of
/// descriptors (`EMFILE`, `ENFILE`) or memory, as in a long-lived window near its descriptor
/// limit, `holds_update` holds every program, `is_held` marks no hold and the read-only
/// `would_hold_update` holds too; neither file is renamed, no lock file is made and nothing is
/// logged. Once reads succeed the file answers as it always did. Before, such an error counted as
/// a file that does not read: the valid file was renamed aside and the person's pins stopped
/// holding for good.
/// NEGATIVE CONTROL: with `passing` answering `false` for every error (each error the file's)
/// the valid pins file is renamed to `pins.unread-<n>` and `claude` updates.
#[test]
fn a_read_this_process_could_not_make_now_moves_nothing_and_holds_for_that_call() {
    for (index, error) in passing_errors().into_iter().enumerate() {
        let fx = logged(&format!("passing-{index}"));
        let l = &fx.layout;
        fs::write(pins_path(l), "claude\n").unwrap();
        let holds = l.prefix.join(PinFile::Holds.name());
        fs::write(&holds, "hx-alab\n").unwrap();
        let (code, kind) = (error.raw_os_error(), error.kind());
        let again = move || match code {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(kind, "package metadata allocation refused"),
        };
        let answers = with_reads(
            move |_| Some(again()),
            || {
                [
                    holds_update(l, "claude"),
                    holds_update(l, "codex"),
                    would_hold_update(l, "codex"),
                    !is_held(l, "hx-alab"),
                ]
            },
        );
        assert_eq!(answers, [true; 4], "{error}");
        assert_eq!(
            fs::read_to_string(pins_path(l)).unwrap(),
            "claude\n",
            "{error}"
        );
        assert_eq!(fs::read_to_string(&holds).unwrap(), "hx-alab\n", "{error}");
        assert!(kept_as(&l.prefix, PinFile::Pins).is_empty(), "{error}");
        assert!(kept_as(&l.prefix, PinFile::Holds).is_empty(), "{error}");
        assert!(!l.prefix.join(LOCK).exists(), "{error}: no lock was taken");
        assert!(set_aside_lines(&fx.logs).is_empty(), "{error}");
        assert!(passing(&again()), "{error}");
        assert!(holds_update(l, "claude") && !holds_update(l, "codex"));
        assert!(is_held(l, "hx-alab"));
    }
}

/// A DAMAGED FILE WHOSE READ UNDER THE LOCK COULD NOT BE MADE STAYS: the first read finds a name
/// no pin can have, but the read made again under the pin lock fails out of descriptors, so it
/// says nothing of what the file holds now (another process may have replaced it); nothing is
/// renamed or logged and every program holds for that call. A later call sets it aside.
/// NEGATIVE CONTROL: with `set_aside` renaming the file on any error of its read under the lock,
/// the file is renamed in that call and `claude` updates.
#[cfg(unix)]
#[test]
fn a_damaged_file_whose_read_under_the_lock_could_not_be_made_stays() {
    let fx = logged("passing-under-lock");
    let l = &fx.layout;
    let original = b"claude\n../trust\n";
    fs::write(pins_path(l), original).unwrap();
    let held = with_reads(
        |count| (count == 1).then(|| io::Error::from_raw_os_error(libc::EMFILE)),
        || holds_update(l, "claude"),
    );
    assert!(held, "a read under the lock that failed holds for the call");
    assert_eq!(fs::read(pins_path(l)).unwrap(), original);
    assert!(kept_as(&l.prefix, PinFile::Pins).is_empty());
    assert!(set_aside_lines(&fx.logs).is_empty());
    assert!(!holds_update(l, "claude"));
    let kept = kept_as(&l.prefix, PinFile::Pins);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(fs::read(&kept[0]).unwrap(), original);
}

/// A DIAGNOSTIC'S READ OF A DAMAGED PINS FILE CHANGES NOTHING: `would_hold_update` answers what
/// unattended work will (the file holds nothing; a readable pin holds) and never renames the file,
/// takes the pin lock or writes the log.
/// NEGATIVE CONTROL: with `would_hold_update` reading through `holds_update` the file is renamed
/// and a line is written.
#[test]
fn a_diagnostics_read_of_a_damaged_pins_file_changes_nothing() {
    let fx = logged("would-hold");
    let l = &fx.layout;
    let original = b"claude\n\xff";
    fs::write(pins_path(l), original).unwrap();
    assert!(!would_hold_update(l, "claude"));
    assert_eq!(fs::read(pins_path(l)).unwrap(), original);
    assert_eq!(
        fs::read_dir(&l.prefix).unwrap().count(),
        1,
        "nothing beside it"
    );
    assert!(set_aside_lines(&fx.logs).is_empty());
    fs::write(pins_path(l), "claude\n").unwrap();
    assert!(would_hold_update(l, "claude") && !would_hold_update(l, "codex"));
}

/// A PERSON'S READ OF A DAMAGED HOLDS FILE IS AN ERROR THAT NAMES IT AND MOVES NOTHING:
/// `marks_hold`, what `aterm pkg update <harness>` reads, answers the error with the file's path
/// and leaves the file where it is, so no hold is taken for the person's own pin.
/// NEGATIVE CONTROL: with `marks_hold` reading through `is_held` it answers `Ok(false)` and the
/// file is renamed.
#[test]
fn a_persons_read_of_a_damaged_holds_file_is_an_error_that_names_it() {
    let fx = logged("marks-hold");
    let l = &fx.layout;
    let holds = l.prefix.join(PinFile::Holds.name());
    let original = b"hx-alab\n\xff";
    fs::write(&holds, original).unwrap();
    let error = marks_hold(l, "hx-alab").unwrap_err();
    assert!(
        error.to_string().contains(&holds.display().to_string())
            && error.to_string().contains("harness holds"),
        "{error}"
    );
    assert_eq!(fs::read(&holds).unwrap(), original);
    assert!(kept_as(&l.prefix, PinFile::Holds).is_empty());
    assert!(set_aside_lines(&fx.logs).is_empty());
    fs::write(&holds, "hx-alab\n").unwrap();
    assert!(marks_hold(l, "hx-alab").unwrap() && !marks_hold(l, "other").unwrap());
}
