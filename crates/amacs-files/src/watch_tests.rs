// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests of the file watch: the debounce, the stamp filter over a scripted
//! event arm, and the platform's own events on a real file.

use super::*;
use std::sync::Mutex;

#[test]
fn a_burst_falls_due_once_it_goes_quiet_or_hits_the_cap() {
    let start = Instant::now();
    let mut debounce = Debounce::default();
    debounce.note(7, start);
    debounce.note(7, start + QUIET / 2);
    assert!(debounce.due(start + QUIET / 2).is_empty(), "still noisy");
    assert_eq!(debounce.deadline(), Some(start + QUIET / 2 + QUIET));
    assert_eq!(debounce.due(start + QUIET / 2 + QUIET), [7]);
    assert!(debounce.due(start + SETTLE_CAP * 2).is_empty(), "due once");

    // A file that never stops changing is still looked at by the cap.
    let mut noisy = Debounce::default();
    let mut at = start;
    while at < start + SETTLE_CAP {
        noisy.note(9, at);
        assert!(noisy.due(at).is_empty() || at >= start + SETTLE_CAP);
        at += QUIET / 4;
    }
    noisy.note(9, at);
    assert_eq!(noisy.due(at), [9]);
}

/// An arm a test scripts: each `wait` reports what the test queued.
#[derive(Clone, Default)]
struct Scripted {
    queued: Arc<Mutex<Vec<u64>>>,
    armed: Arc<Mutex<Vec<(u64, PathBuf)>>>,
    inert: InertEvents,
}

impl FileEvents for Scripted {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        self.armed.lock().unwrap().push((key, path.to_path_buf()));
        Ok(())
    }

    fn disarm(&mut self, _key: u64) {}

    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
        let queued = std::mem::take(&mut *self.queued.lock().unwrap());
        if queued.is_empty() {
            self.inert.wait(timeout.or(Some(QUIET)), touched)?;
        }
        touched.extend(queued);
        Ok(())
    }

    fn waker(&self) -> Waker {
        self.inert.waker()
    }
}

fn scratch(name: &str) -> aterm_tempfile::TempDir {
    crate::test_scratch_dir(name)
}

/// Any arm, counting its arms so a test knows the watch took its baseline.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Counted {
    inner: Box<dyn FileEvents>,
    arms: Arc<Mutex<usize>>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl FileEvents for Counted {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        let armed = self.inner.arm(key, path);
        *self.arms.lock().unwrap() += 1;
        armed
    }

    fn disarm(&mut self, key: u64) {
        self.inner.disarm(key);
    }

    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
        self.inner.wait(timeout, touched)
    }

    fn waker(&self) -> Waker {
        self.inner.waker()
    }
}

/// The first look at a newly watched file tells, whatever its stamp (the
/// host read the file before the watch armed, and a change in between would
/// be in any baseline the watch took); after it, an event tells only when
/// the file's stamp moved: an event on a file that did not change (a
/// neighbour in its directory) tells nothing, and the watch arms again on
/// the path after each burst.
#[test]
fn the_first_look_tells_then_an_event_tells_only_when_the_file_really_changed() {
    let dir = scratch("amacs-watch-stamp");
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let events = Scripted::default();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(3, path.clone());
    let arms = |at_least: usize| {
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the watch arms", || {
            (events.armed.lock().unwrap().len() >= at_least).then_some(())
        });
    };
    // Armed, then looked at once: that look tells, though the file did not
    // change since the watch armed.
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(3),
        "the first look"
    );
    arms(2);

    // The negative control: an event with no change tells nothing, and the
    // change after it is the only thing told.
    events.queued.lock().unwrap().push(3);
    (events.inert.waker())();
    arms(3);
    std::fs::write(&path, "two, longer\n").unwrap();
    events.queued.lock().unwrap().push(3);
    (events.inert.waker())();
    assert_eq!(heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN), Ok(3));
    arms(4);
    assert!(
        heard.try_recv().is_err(),
        "the unchanged bursts told nothing"
    );
    drop(watch);
}

/// The platform's own events: a rewrite in place and a replace by rename (a
/// new inode, as editors, formatters and `git checkout` save) are each heard,
/// the second proving the watch armed again on the new file.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_rewrite_and_a_replace_by_rename() {
    let dir = scratch("amacs-watch-platform");
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let (told, heard) = mpsc::channel();
    let arms = Arc::new(Mutex::new(0));
    let events = Counted {
        inner: platform_events().unwrap(),
        arms: Arc::clone(&arms),
    };
    let watch = FileWatch::spawn(
        Box::new(events),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(11, path.clone());
    // The first look tells and takes the stamp later changes are told by.
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(11),
        "the first look"
    );
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the watch arms", || {
        (*arms.lock().unwrap() >= 2).then_some(())
    });

    std::fs::write(&path, "two, rewritten in place\n").unwrap();
    assert_eq!(heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN), Ok(11));

    for round in 0..2 {
        let temporary = dir.path().join(format!(".doc.txt.{round}"));
        std::fs::write(&temporary, format!("replaced by rename {round}\n")).unwrap();
        std::fs::rename(&temporary, &path).unwrap();
        assert_eq!(
            heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
            Ok(11),
            "replace {round}"
        );
    }
    drop(watch);
}

/// A file deleted under the watch is heard, and so is the file made again at
/// its path, which only the directory's events can tell (the deleted file's
/// own events end with it): a buffer whose file went hears it come back. Its
/// folder deleted too leaves nothing to arm on, and the watch arms again until
/// the folder is back, so the file made again in it is heard as well; a file
/// in another folder, whose arm never failed, hears its writes throughout (the
/// control).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_delete_and_the_file_made_again() {
    let dir = scratch("amacs-watch-recreate");
    let folder = dir.path().join("nested");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let (told, heard) = mpsc::channel();
    let arms = Arc::new(Mutex::new(0));
    let events = Counted {
        inner: platform_events().unwrap(),
        arms: Arc::clone(&arms),
    };
    let watch = FileWatch::spawn(
        Box::new(events),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(9, path.clone());
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(9),
        "the first look"
    );
    let armed = |at_least: usize| {
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the watch arms", || {
            (*arms.lock().unwrap() >= at_least).then_some(())
        });
    };
    armed(2);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(9),
        "the delete"
    );
    // The look after the delete armed again, on the directory alone.
    armed(3);
    std::fs::write(&path, "two\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(9),
        "the file made again"
    );
    armed(4);

    // In a folder of its own: a look of it that the folders made and deleted
    // below touched could race the control's own write.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let beside = elsewhere.join("beside.txt");
    std::fs::write(&beside, "one\n").unwrap();
    watch.watch(10, beside.clone());
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(10),
        "the first look beside"
    );
    armed(6);
    std::fs::remove_dir_all(&folder).unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(9),
        "the folder deleted with the file"
    );
    // The look after that delete found nothing to arm on.
    armed(7);
    std::fs::write(&beside, "two, beside\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(10),
        "the control's write"
    );
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(&path, "three\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(9),
        "the file made again in its folder made again"
    );
    drop(watch);
}

/// An arm a test refuses while `refuse` holds (the file's folder is gone),
/// counting the arms it was asked for and keeping each wait's timeout.
#[derive(Clone, Default)]
struct Refusing {
    refuse: Arc<std::sync::atomic::AtomicBool>,
    arms: Arc<Mutex<usize>>,
    waits: Arc<Mutex<Vec<Option<Duration>>>>,
    inert: InertEvents,
}

impl FileEvents for Refusing {
    fn arm(&mut self, _key: u64, _path: &Path) -> io::Result<()> {
        *self.arms.lock().unwrap() += 1;
        if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
            Err(io::Error::from(io::ErrorKind::NotFound))
        } else {
            Ok(())
        }
    }

    fn disarm(&mut self, _key: u64) {}

    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
        self.waits.lock().unwrap().push(timeout);
        self.inert.wait(timeout, touched)
    }

    fn waker(&self) -> Waker {
        self.inert.waker()
    }
}

/// A file no arm took is armed again, every wait bounded meanwhile, until an
/// arm takes; from then on the watch waits on events alone, with no timeout:
/// it arms again only while a file is unarmed.
#[test]
fn a_file_no_arm_took_is_armed_again_until_one_takes_and_then_waits_untimed() {
    let dir = scratch("amacs-watch-rearm");
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let events = Refusing::default();
    events
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(5, path.clone());
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(5),
        "the first look"
    );
    // Refused at the watch and at its first look, then armed again unasked.
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "armed again", || {
        (*events.arms.lock().unwrap() >= 4).then_some(())
    });
    let refused_waits = events.waits.lock().unwrap().clone();
    assert!(
        refused_waits.iter().all(Option::is_some),
        "an unarmed file bounds every wait: {refused_waits:?}"
    );
    events
        .refuse
        .store(false, std::sync::atomic::Ordering::SeqCst);
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "an untimed wait", || {
        events
            .waits
            .lock()
            .unwrap()
            .last()
            .is_some_and(Option::is_none)
            .then_some(())
    });
    assert!(
        heard.try_recv().is_err(),
        "the look after the arm took found the file unchanged"
    );
    drop(watch);
}

/// inotify's `poll` timeout: none waits for an event, a wait already due
/// polls without sleeping (it slept a millisecond), and a rest under a
/// millisecond is one, never a spin at zero.
#[test]
fn a_due_wait_polls_at_once_and_a_short_one_never_spins() {
    assert_eq!(poll_millis(None), -1);
    assert_eq!(poll_millis(Some(Duration::ZERO)), 0);
    assert_eq!(poll_millis(Some(Duration::from_micros(1))), 1);
    assert_eq!(poll_millis(Some(QUIET)), 80);
    assert_eq!(poll_millis(Some(Duration::MAX)), i32::MAX);
}

/// A file whose folder, and that folder's folder, are not there yet is heard
/// once all three are made: the watch listens on the nearest folder that is
/// there and arms again lower as each one below it is made.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_file_made_in_folders_made_after_the_watch() {
    let dir = scratch("amacs-watch-folders");
    let outer = dir.path().join("outer");
    let path = outer.join("inner").join("doc.txt");
    let (told, heard) = mpsc::channel();
    let arms = Arc::new(Mutex::new(0));
    let events = Counted {
        inner: platform_events().unwrap(),
        arms: Arc::clone(&arms),
    };
    let watch = FileWatch::spawn(
        Box::new(events),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(5, path.clone());
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(5),
        "the first look"
    );
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the watch arms", || {
        (*arms.lock().unwrap() >= 2).then_some(())
    });
    std::fs::create_dir(&outer).unwrap();
    // The look the outer folder's making brings arms on it.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the watch arms lower",
        || (*arms.lock().unwrap() >= 3).then_some(()),
    );
    std::fs::create_dir(outer.join("inner")).unwrap();
    std::fs::write(&path, "made\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(5),
        "the file made in its new folders"
    );
    drop(watch);
}

/// Linux's inotify events: one names its watch and entry, so it touches only
/// the key watching that file in that directory (the negative control), and
/// a queue overflow (watch -1, the kernel dropped events) touches every key,
/// whose looks then find which files really changed.
#[test]
fn an_inotify_event_touches_its_file_and_an_overflow_touches_every_file() {
    fn event(wd: i32, mask: u32, name: &[u8]) -> Vec<u8> {
        let padded = if name.is_empty() { 0 } else { 16 };
        let mut bytes = Vec::new();
        bytes.extend(wd.to_ne_bytes());
        bytes.extend(mask.to_ne_bytes());
        bytes.extend(0u32.to_ne_bytes());
        bytes.extend(u32::try_from(padded).unwrap().to_ne_bytes());
        let mut entry = name.to_vec();
        entry.resize(padded, 0);
        bytes.extend(entry);
        bytes
    }
    let keys = BTreeMap::from([
        (1, (5, b"doc.txt".to_vec())),
        (2, (5, b"other.txt".to_vec())),
        (3, (6, b"doc.txt".to_vec())),
    ]);
    let modify = event(5, 0x2, b"doc.txt");
    let mut touched = Vec::new();
    inotify_touched(&modify, &keys, &mut touched);
    assert_eq!(touched, [1], "only the file the event names");

    let mut touched = Vec::new();
    let mut buffer = modify;
    buffer.extend(event(-1, IN_Q_OVERFLOW, b""));
    inotify_touched(&buffer, &keys, &mut touched);
    touched.sort_unstable();
    touched.dedup();
    assert_eq!(touched, [1, 2, 3], "an overflow touches every file");

    let mut huge_len = Vec::new();
    huge_len.extend(5i32.to_ne_bytes());
    huge_len.extend(0x2u32.to_ne_bytes());
    huge_len.extend(0u32.to_ne_bytes());
    huge_len.extend(u32::MAX.to_ne_bytes());
    huge_len.extend_from_slice(b"doc.txt\0");
    let mut touched = Vec::new();
    inotify_touched(&huge_len, &keys, &mut touched);
    assert_eq!(touched, [1]);
}
