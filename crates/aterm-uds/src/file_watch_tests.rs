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

#[test]
fn debounce_matches_independent_scan_through_updates_forgets_and_drains() {
    let start = Instant::now();
    let mut debounce = Debounce::default();
    let mut reference = BTreeMap::<u64, (Instant, Instant)>::new();
    let mut seed = 0x5368_f173_u64;
    for step in 0..8192 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = (seed >> 32) % 129;
        // Deliberately include supplied instants that go backwards as well as
        // long bursts; the public operation's result must retain scan semantics.
        let now = start + Duration::from_millis((seed >> 16) % 1800);
        match seed % 8 {
            0 => {
                debounce.forget(key);
                reference.remove(&key);
            }
            1 => {
                let due: Vec<_> = reference
                    .iter()
                    .filter(|(_, (first, latest))| {
                        now.saturating_duration_since(*latest) >= QUIET
                            || now.saturating_duration_since(*first) >= SETTLE_CAP
                    })
                    .map(|(key, _)| *key)
                    .collect();
                assert_eq!(debounce.due(now), due, "drain at step {step}");
                for key in due {
                    reference.remove(&key);
                }
            }
            _ => {
                debounce.note(key, now);
                reference
                    .entry(key)
                    .and_modify(|(_, latest)| *latest = now)
                    .or_insert((now, now));
            }
        }
        let deadline = reference
            .values()
            .map(|(first, latest)| (*first + SETTLE_CAP).min(*latest + QUIET))
            .min();
        assert_eq!(debounce.deadline(), deadline, "deadline at step {step}");
        assert_eq!(debounce.pending, reference);
        assert_eq!(
            debounce.deadlines.len(),
            if reference.len() > 1 {
                reference.len()
            } else {
                0
            },
            "no stale deadlines"
        );
    }
    let expected: Vec<_> = reference.keys().copied().collect();
    assert_eq!(debounce.due(start + Duration::from_secs(4)), expected);
    assert_eq!(debounce.deadline(), None);
}

#[test]
fn debounce_returns_to_single_file_scheduling_after_forget_and_partial_drain() {
    let start = Instant::now();
    let mut debounce = Debounce::default();
    for round in 0..4 {
        let at = start + Duration::from_secs(round);
        debounce.note(7, at);
        debounce.note(9, at + QUIET / 2);
        assert_eq!(debounce.deadline(), Some(at + QUIET));
        debounce.forget(7);
        assert_eq!(debounce.deadline(), Some(at + QUIET + QUIET / 2));
        debounce.note(3, at + QUIET / 4);
        assert_eq!(debounce.due(at + QUIET + QUIET / 4), [3]);
        assert_eq!(debounce.deadline(), Some(at + QUIET + QUIET / 2));
        debounce.note(9, at + QUIET);
        assert_eq!(debounce.deadline(), Some(at + QUIET * 2));
        debounce.note(1, at);
        assert_eq!(debounce.due(at + QUIET), [1]);
        assert_eq!(debounce.due(at + QUIET * 2), [9]);
        assert_eq!(debounce.deadline(), None);
    }
}

#[test]
fn readable_input_yields_without_losing_queued_bytes() {
    fn exercise<const BYTES: usize>() {
        use std::io::Read as _;
        let input: Vec<u8> = (0..49 * BYTES).map(|byte| byte as u8).collect();
        let mut source = std::io::Cursor::new(input.as_slice());
        let mut output = Vec::new();
        let mut turns = Vec::new();
        for _ in 0..4 {
            let mut reads = 0;
            read_batch::<BYTES>(
                |buffer| {
                    reads += 1;
                    source.read(buffer).ok()
                },
                |buffer| output.extend_from_slice(buffer),
            );
            turns.push(reads);
        }
        eprintln!("buffer={BYTES} read turns={turns:?}");
        assert!(
            turns.iter().all(|reads| *reads <= 16),
            "each input yields to the scheduler"
        );
        assert_eq!(output, input, "later turns retain every unread byte");
    }
    exercise::<64>();
    exercise::<4096>();
}

#[test]
fn read_batches_stop_on_end_or_read_failure() {
    for end in [Some(0), None] {
        let mut reads = 0;
        read_batch::<64>(
            |_| {
                reads += 1;
                end
            },
            |_| panic!("no initialized bytes to forward"),
        );
        assert_eq!(reads, 1);
    }
}

#[cfg(unix)]
#[test]
fn a_partial_read_batch_leaves_native_input_ready_for_another_turn() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    let (mut read, mut write) = crate::pipe().unwrap();
    let input: Vec<u8> = (0..49).collect();
    write.write_all(&input).unwrap();
    drop(write);
    let mut output = Vec::new();
    read_batch::<1>(
        |buffer| read.read(buffer).ok(),
        |buffer| output.extend_from_slice(buffer),
    );
    assert_eq!(output, input[..16]);
    assert_eq!(
        crate::ready::any_interests_now(&[crate::ready::Watch::new(
            read.as_fd(),
            crate::ready::Interest::Read,
        )])
        .unwrap(),
        [0],
        "unread bytes remain ready without another write"
    );
    let mut rest = [0; 33];
    read.read_exact(&mut rest).unwrap();
    output.extend_from_slice(&rest);
    assert_eq!(output, input);
}

/// An arm a test scripts: each `wait` reports what the test queued. Every arm the watch asks for
/// is in `armed` once (a directory arm on a folder not there falls back to a file arm, which is
/// the one recorded), a directory arm that took in `entries` as well, and each wait's timeout in
/// `waits`. Where `files_only`, it hears no folder's entries, as the stat poll does not; a key in
/// `elsewhere` is armed on a volume that does not report every writer. Where `pins`, its
/// listeners pin the folders above them, as the Windows arm's do, and it listens for no path
/// under `outside`.
#[derive(Clone, Default)]
struct Scripted {
    files_only: bool,
    pins: bool,
    outside: Option<PathBuf>,
    elsewhere: Arc<Mutex<BTreeSet<u64>>>,
    queued: Arc<Mutex<Vec<u64>>>,
    armed: Arc<Mutex<Vec<(u64, PathBuf)>>>,
    entries: Arc<Mutex<Vec<u64>>>,
    waits: Arc<Mutex<Vec<Option<Duration>>>>,
    inert: InertEvents,
}

impl Scripted {
    /// Queue a burst on `keys` and wake the watch.
    fn burst(&self, keys: &[u64]) {
        self.queued.lock().unwrap().extend_from_slice(keys);
        (self.inert.waker())();
    }

    /// Wait until the watch has armed `arms` times in all and then waited again: whatever the
    /// turn that armed it last was going to tell, it has told.
    fn settled(&self, arms: usize) {
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the watch arms", || {
            (self.armed.lock().unwrap().len() >= arms).then_some(())
        });
        let waits = self.waits.lock().unwrap().len();
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the watch waits", || {
            (self.waits.lock().unwrap().len() > waits).then_some(())
        });
    }
}

impl FileEvents for Scripted {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        self.armed.lock().unwrap().push((key, path.to_path_buf()));
        Ok(())
    }

    fn arm_directory(&mut self, key: u64, path: &Path) -> io::Result<()> {
        if self.files_only {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        match std::fs::metadata(path) {
            Err(error) => return Err(error),
            Ok(there) if !there.is_dir() => {
                return Err(io::Error::from(io::ErrorKind::NotADirectory));
            }
            Ok(_) => {}
        }
        self.armed.lock().unwrap().push((key, path.to_path_buf()));
        self.entries.lock().unwrap().push(key);
        Ok(())
    }

    fn disarm(&mut self, _key: u64) {}

    fn hears_every_writer(&self, key: u64) -> bool {
        !self.elsewhere.lock().unwrap().contains(&key)
    }

    fn pins_folders(&self) -> bool {
        self.pins
    }

    fn listens_for(&self, path: &Path) -> bool {
        !self
            .outside
            .as_ref()
            .is_some_and(|outside| path.starts_with(outside))
    }

    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> io::Result<()> {
        self.waits.lock().unwrap().push(timeout);
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
    let end = name.floor_char_boundary(24);
    let prefix = format!("aterm-watch-{}-", &name[..end]);
    aterm_tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".noindex")
        .tempdir()
        .expect("create watch scratch directory")
}

/// Any arm, counting its arms so a test knows the watch took its baseline.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
struct Counted {
    inner: Box<dyn FileEvents>,
    arms: Arc<Mutex<usize>>,
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
impl FileEvents for Counted {
    fn arm(&mut self, key: u64, path: &Path) -> io::Result<()> {
        let armed = self.inner.arm(key, path);
        *self.arms.lock().unwrap() += 1;
        armed
    }

    fn disarm(&mut self, key: u64) {
        self.inner.disarm(key);
    }

    fn hears_every_writer(&self, key: u64) -> bool {
        self.inner.hears_every_writer(key)
    }

    fn pins_folders(&self) -> bool {
        self.inner.pins_folders()
    }

    fn listens_for(&self, path: &Path) -> bool {
        self.inner.listens_for(path)
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
        "aterm-document-watch",
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
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
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
        "aterm-document-watch",
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
/// folder deleted too moves the listen to the folder above, which hears it
/// back, so the file made again in it is heard as well; a file
/// in another folder, whose arm never failed, hears its writes throughout (the
/// control).
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
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
        "aterm-document-watch",
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
    // The look after that delete armed on the folder above.
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

/// A folder renamed above the file's own is heard, though the file and its folder move with it and
/// neither's listener hears anything happen to them: the file is gone from its path. The folders
/// made again at the path and the file written in them are heard, and so is the next write, which
/// only a listener on the new file hears.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_folder_above_renamed_and_the_path_made_again() {
    let dir = scratch("amacs-watch-above");
    let top = dir.path().join("a");
    let folder = top.join("b");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        platform_events().unwrap(),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(7, path.clone());
    // Each look arms before it tells, so a told look's listeners are on.
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the first look"
    );

    std::fs::rename(&top, dir.path().join("a.old")).unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the folder above renamed"
    );
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(&path, "two\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the file made again in its folders made again"
    );
    std::fs::write(&path, "three, a longer line\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the next write"
    );
    drop(watch);
}

/// A folder behind a link on the path renamed away is heard, and then the link names nothing; the
/// folders made again behind it (new ones, which only a new arm listens on: the watch arms the
/// file again once the link names a folder) and the file written there are heard, and so is the
/// next write.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_folder_behind_a_link_renamed_away_and_made_again() {
    let dir = scratch("amacs-watch-behind-link-again");
    let real = dir.path().join("real");
    std::fs::create_dir_all(real.join("sub")).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let path = link.join("sub").join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        platform_events().unwrap(),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(7, path.clone());
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the first look"
    );

    std::fs::rename(&real, dir.path().join("real.away")).unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the folder behind the link renamed away"
    );
    std::fs::create_dir_all(real.join("sub")).unwrap();
    std::fs::write(&path, "two\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the file made again in new folders behind the link"
    );
    std::fs::write(&path, "three, a longer line\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(7),
        "the next write"
    );
    drop(watch);
}

/// Two files whose paths run through the same folders share one listener on each folder above
/// their own folders. A shared one renamed touches both, and each listener stays while either
/// file holds it and goes with the last.
#[cfg(target_os = "macos")]
#[test]
fn folders_above_two_files_share_one_listener_each() {
    use std::os::unix::fs::MetadataExt as _;
    let identity = |path: &Path| {
        let there = std::fs::metadata(path).unwrap();
        (there.dev(), there.ino())
    };
    let dir = scratch("amacs-watch-above-shared");
    let top = dir.path().join("a");
    let one = top.join("one").join("doc.txt");
    let two = top.join("two").join("doc.txt");
    for path in [&one, &two] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let mut events = kqueue::KqueueEvents::new().unwrap();
    let held_by = |events: &kqueue::KqueueEvents, keys: &[u64]| {
        events
            .above()
            .iter()
            .all(|(_, holders)| holders.as_slice() == keys)
    };

    events.arm(1, &one).unwrap();
    let alone = events.above();
    let folders: Vec<_> = alone.iter().map(|(id, _)| *id).collect();
    assert!(
        folders.contains(&identity(&top)),
        "the nearest: {folders:?}"
    );
    assert!(
        folders.contains(&identity(dir.path())),
        "the next: {folders:?}"
    );
    assert!(held_by(&events, &[1]));

    events.arm(2, &two).unwrap();
    assert_eq!(events.above().len(), alone.len(), "no second listener");
    assert!(held_by(&events, &[1, 2]));

    let touched = heard_for(&mut events, || {
        std::fs::rename(&top, dir.path().join("a.old")).unwrap()
    });
    assert_eq!(touched, [1, 2]);

    events.disarm(1);
    assert_eq!(events.above().len(), alone.len());
    assert!(held_by(&events, &[2]));
    events.disarm(2);
    assert!(events.above().is_empty());
}

/// A folder above the file's own renamed and made again as the watch arms, after the file's own
/// listens and before the walk above, leaves those listens on the folders it moved: the arm sees
/// its folder is no longer what its path names and arms again, so the file there now is heard.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_above_replaced_as_the_watch_arms_hears_the_path_made_again() {
    let dir = scratch("amacs-watch-above-replaced");
    let top = dir.path().join("a");
    let folder = top.join("b");
    let path = folder.join("doc.txt");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(&path, "").unwrap();
    let mut moved = false;
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events
        .arm_around(1, &path, |step, at| {
            if step == kqueue::Listen::Answered
                && at == path
                && !std::mem::replace(&mut moved, true)
            {
                std::fs::rename(&top, dir.path().join("a.old")).unwrap();
                std::fs::create_dir_all(&folder).unwrap();
                std::fs::write(&path, "").unwrap();
            }
            Ok(())
        })
        .unwrap();

    let touched = heard_for(&mut events, || std::fs::write(&path, "written").unwrap());

    assert_eq!(touched, [1]);
}

/// The same race on Linux, between the file's own folder's watch and the watches above it: the arm
/// asks the kernel for its folder's watch again, finds another than the one it holds, and arms
/// again, so the file there now is heard.
#[cfg(target_os = "linux")]
#[test]
fn an_inotify_folder_above_replaced_as_the_watch_arms_hears_the_path_made_again() {
    let dir = scratch("amacs-watch-above-replaced-inotify");
    let top = dir.path().join("a");
    let folder = top.join("b");
    let path = folder.join("doc.txt");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(&path, "").unwrap();
    let mut moved = false;
    let mut events = inotify::InotifyEvents::new().unwrap();
    events
        .arm_then(1, &path, |_| {
            if !std::mem::replace(&mut moved, true) {
                std::fs::rename(&top, dir.path().join("a.old")).unwrap();
                std::fs::create_dir_all(&folder).unwrap();
                std::fs::write(&path, "").unwrap();
            }
        })
        .unwrap();

    let touched = heard_for(&mut events, || std::fs::write(&path, "written").unwrap());

    assert_eq!(touched, [1]);
}

/// A file whose own folder is a link, which no folder listener opens, holds only its file's
/// listener there; the folders above the link still listen, so the folder holding the link
/// renamed, which moves the path off that file, is heard.
#[cfg(target_os = "macos")]
#[test]
fn a_file_in_a_linked_folder_hears_the_folder_above_the_link_renamed() {
    let dir = scratch("amacs-watch-above-link");
    let real = dir.path().join("real");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(real.join("doc.txt"), "").unwrap();
    std::os::unix::fs::symlink(&real, project.join("link")).unwrap();
    let path = project.join("link").join("doc.txt");
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events.arm(1, &path).unwrap();

    let touched = heard_for(&mut events, || {
        std::fs::rename(&project, dir.path().join("project.old")).unwrap()
    });

    assert_eq!(touched, [1]);
}

/// The folders and links looking `path` up passes through below `real`, each by its path there
/// and whether it is a link.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn passed_below(path: &Path, real: &Path) -> Vec<(String, bool)> {
    passed_through(path)
        .unwrap()
        .into_iter()
        .filter_map(|passed| {
            let below = passed.path.strip_prefix(real).ok()?.to_str()?.to_owned();
            (!below.is_empty()).then_some((below, passed.link))
        })
        .collect()
}

/// Looking a path up passes through each link on it and then through the folders its target
/// names, from the root where the target is absolute and from the link's folder otherwise, and a
/// `..` goes back from where the lookup is: after a link, the folder above the target's, which is
/// not the link's own folder (where a `..` read off the path as written would land).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_lookup_passes_through_each_link_and_the_folders_behind_it() {
    let dir = scratch("amacs-watch-lookup");
    let real = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(real.join("x/a/b")).unwrap();
    std::fs::write(real.join("x/a/b/doc.txt"), "").unwrap();
    std::os::unix::fs::symlink(dir.path().join("x/a"), real.join("absolute")).unwrap();
    std::os::unix::fs::symlink("..", real.join("x/a/back")).unwrap();
    std::os::unix::fs::symlink("x/a", real.join("relative")).unwrap();

    let expected = |rows: &[(&str, bool)]| -> Vec<(String, bool)> {
        rows.iter()
            .map(|(below, link)| ((*below).to_owned(), *link))
            .collect()
    };
    assert_eq!(
        passed_below(&dir.path().join("absolute/back/a/b"), &real),
        expected(&[
            ("absolute", true),
            ("x", false),
            ("x/a", false),
            ("x/a/back", true),
            ("x/a", false),
            ("x/a/b", false),
        ]),
    );
    assert_eq!(
        passed_below(&dir.path().join("relative/b"), &real),
        expected(&[
            ("relative", true),
            ("x", false),
            ("x/a", false),
            ("x/a/b", false)
        ]),
    );
    assert_eq!(
        passed_through(&dir.path().join("relative/missing"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
        "a lookup that does not get there moved as it walked",
    );
}

/// A link this user cannot read (one made under a strict umask) is still gone through by a lookup,
/// so the folders behind it are still passed through and listened on, even where the folder it
/// names can be searched but not listed (another user's, say), which only a search opens.
#[cfg(target_os = "macos")]
#[test]
fn a_lookup_goes_through_a_link_its_user_cannot_read() {
    use std::os::unix::fs::PermissionsExt as _;
    /// Gives `release` back its listing on the way out, so the scratch folder can go.
    struct Listable(PathBuf);
    impl Drop for Listable {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let dir = scratch("amacs-watch-unreadable-link");
    let real = std::fs::canonicalize(dir.path()).unwrap();
    let release = real.join("release");
    std::fs::create_dir_all(release.join("mid/sub")).unwrap();
    std::fs::write(release.join("mid/sub/doc.txt"), "").unwrap();
    let current = real.join("current");
    std::os::unix::fs::symlink("release", &current).unwrap();
    let named = std::ffi::CString::new(current.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a NUL-terminated path; the call changes the link's own mode, not its target's.
    let changed =
        unsafe { libc::fchmodat(libc::AT_FDCWD, named.as_ptr(), 0, libc::AT_SYMLINK_NOFOLLOW) };
    assert_eq!(changed, 0, "{}", io::Error::last_os_error());
    let _listable = Listable(release.clone());
    std::fs::set_permissions(&release, std::fs::Permissions::from_mode(0o311)).unwrap();
    assert_eq!(
        std::fs::read_link(&current).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied,
        "the link refuses its reader",
    );

    assert_eq!(
        passed_below(&current.join("mid/sub"), &real),
        [
            ("current".to_owned(), true),
            ("release".to_owned(), false),
            ("release/mid".to_owned(), false),
            ("release/mid/sub".to_owned(), false),
        ],
    );
    let path = current.join("mid/sub/doc.txt");
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events.arm(1, &path).unwrap();
    let touched = heard_for(&mut events, || {
        std::fs::rename(release.join("mid"), release.join("mid.old")).unwrap()
    });
    assert_eq!(touched, [1]);
}

/// A `/proc` link names a place its text does not (here an open folder that is gone, whose text
/// ends ` (deleted)`): the kernel's lookup follows it there, and the walk, which cannot, ends at
/// that entry rather than calling the path moved.
#[cfg(target_os = "linux")]
#[test]
fn a_lookup_ends_where_a_proc_link_goes_where_its_text_does_not() {
    use std::os::fd::AsRawFd as _;
    let dir = scratch("amacs-watch-proc-link");
    let gone = dir.path().join("gone");
    std::fs::create_dir(&gone).unwrap();
    let held = std::fs::File::open(&gone).unwrap();
    std::fs::remove_dir(&gone).unwrap();
    let path = PathBuf::from(format!("/proc/self/fd/{}", held.as_raw_fd()));
    assert!(
        std::fs::metadata(&path).unwrap().is_dir(),
        "the kernel goes through it"
    );

    let passed = passed_through(&path).unwrap();

    let fd = std::fs::canonicalize("/proc/self")
        .unwrap()
        .join("fd")
        .join(held.as_raw_fd().to_string());
    assert!(
        passed.iter().any(|passed| passed.link && passed.path == fd),
        "{passed:?}"
    );
}

/// A folder the lookup passed that is a link when its listener opens (moved, and a link left in
/// its place) moved as the walk ran: the arm looks again rather than passing it over.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_found_a_link_as_it_is_listened_on_moved() {
    let dir = scratch("amacs-watch-folder-now-link");
    let folder = dir.path().join("project");
    std::fs::create_dir_all(dir.path().join("project.real")).unwrap();
    std::os::unix::fs::symlink("project.real", &folder).unwrap();
    let mut events = kqueue::KqueueEvents::new().unwrap();

    assert_eq!(
        events
            .listen_one(1, &folder, false, libc::NOTE_RENAME)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
    );
    assert!(
        events
            .listen_one(1, &folder, true, libc::NOTE_RENAME)
            .is_ok()
    );
}

/// A link on a file's path made to name another folder (renamed over by a new link, as an atomic
/// switch of a `current` release link is) moves the path onto another file, which the link's own
/// listener hears.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_link_on_the_path_made_to_name_another_folder() {
    let dir = scratch("amacs-watch-link-switched");
    for release in ["one", "two"] {
        let folder = dir.path().join(release).join("sub");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("doc.txt"), release).unwrap();
    }
    let current = dir.path().join("current");
    std::os::unix::fs::symlink(dir.path().join("one"), &current).unwrap();
    let path = current.join("sub").join("doc.txt");
    let mut events = platform_events().unwrap();
    events.arm(1, &path).unwrap();

    let touched = heard_for(&mut *events, || {
        let next = dir.path().join("current.next");
        std::os::unix::fs::symlink(dir.path().join("two"), &next).unwrap();
        std::fs::rename(&next, &current).unwrap();
    });

    assert_eq!(touched, [1]);
}

/// A folder that a link on a file's path names, renamed, takes the file away from its path, which
/// that folder's own listener hears though no folder written on the path moved.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_folder_behind_a_link_on_the_path_renamed() {
    let dir = scratch("amacs-watch-behind-link");
    let real = dir.path().join("real");
    std::fs::create_dir_all(real.join("sub")).unwrap();
    std::fs::write(real.join("sub").join("doc.txt"), "").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let path = link.join("sub").join("doc.txt");
    let mut events = platform_events().unwrap();
    events.arm(1, &path).unwrap();

    let touched = heard_for(&mut *events, || {
        std::fs::rename(&real, dir.path().join("real.old")).unwrap()
    });

    assert_eq!(touched, [1]);
}

/// A path under a regular file (a records layout's data file, while the records entry is still a
/// plain file) is a file not there yet: it arms on the nearest folder above, with no re-arm
/// rounds, and that folder hears the file replaced by a folder, after which the arm goes lower
/// and hears the path's own file written.
///
/// NEGATIVE CONTROL (2026-11-11): with `listen_folder` answering the file's `ENOTDIR` as it did a
/// refusal, the arm fails, [`Took::of`] answers [`Took::No`] and the key is armed again every
/// [`REARM_EVERY`] for as long as the file stands.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_path_under_a_regular_file_arms_on_the_folder_above_it() {
    let dir = scratch("watch-under-a-file");
    let entry = dir.path().join("records");
    std::fs::write(&entry, "plain\n").unwrap();
    let path = entry.join("current");
    let mut events = platform_events().unwrap();
    assert_eq!(
        Took::of(&mut *events, 1, &path, Shape::File),
        Took::File,
        "armed on the folder above the file"
    );
    let mut pending = Unarmed::default();
    assert_eq!(
        pending.arm(&mut *events, 1, &path, Shape::File, Instant::now()),
        Took::File
    );
    assert_eq!(pending.wait(Instant::now()), None, "no re-arm round is due");

    let touched = heard_for(&mut *events, || {
        let next = dir.path().join("records.next");
        std::fs::create_dir(&next).unwrap();
        std::fs::write(next.join("current"), "one\n").unwrap();
        std::fs::remove_file(&entry).unwrap();
        std::fs::rename(&next, &entry).unwrap();
    });
    assert_eq!(touched, [1], "the file replaced by a folder");

    events.arm(1, &path).unwrap();
    let touched = heard_for(&mut *events, || {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"two\n").unwrap();
    });
    assert_eq!(touched, [1], "a write on the end of the path's own file");
}

/// A file not yet made in a folder that is a link to one (a dotfiles `~/.config/aterm`) arms on
/// the folder the link names, so its making is heard there with no re-arm rounds; and the link
/// made to name another folder is heard by the link's own listener.
#[cfg(target_os = "macos")]
#[test]
fn a_missing_file_in_a_linked_folder_arms_on_the_folder_the_link_names() {
    let dir = scratch("amacs-watch-linked-folder");
    let (real, other) = (dir.path().join("real"), dir.path().join("other"));
    std::fs::create_dir(&real).unwrap();
    std::fs::create_dir(&other).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let path = link.join("absent.txt");
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events
        .arm(1, &path)
        .expect("armed on the folder the link names");

    let touched = heard_for(&mut events, || {
        std::fs::write(real.join("absent.txt"), "made\n").unwrap();
    });
    assert_eq!(touched, [1], "made where the link points");

    events.arm(1, &path).unwrap();
    let touched = heard_for(&mut events, || {
        let next = dir.path().join("link.next");
        std::os::unix::fs::symlink(&other, &next).unwrap();
        std::fs::rename(&next, &link).unwrap();
    });
    assert_eq!(touched, [1], "the link made to name another folder");

    // The negative control: a link that names no folder is refused, as it was.
    let dangling = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("gone"), &dangling).unwrap();
    assert!(events.arm(2, &dangling.join("absent.txt")).is_err());
}

/// An arm a test refuses while `refuse` holds (the file's folder is gone, or a folder cannot be
/// read), counting the arms it was asked for and keeping each wait's timeout.
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

    /// Refused as a folder this user cannot read is, which no file arm stands in for.
    fn arm_directory(&mut self, _key: u64, _path: &Path) -> io::Result<()> {
        *self.arms.lock().unwrap() += 1;
        if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
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

#[test]
fn failed_rearms_do_not_postpone_other_pending_files() {
    let start = Instant::now();
    let path = Path::new("fixture/unavailable");
    let mut events = Refusing::default();
    events
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut pending = Unarmed::default();
    assert_eq!(
        pending.arm(&mut events, 7, path, Shape::File, start),
        Took::No
    );
    for part in 1..4 {
        assert_eq!(
            pending.arm(
                &mut events,
                9,
                path,
                Shape::File,
                start + REARM_EVERY / 4 * part
            ),
            Took::No
        );
    }
    let now = start + REARM_EVERY;
    let due = pending.due(now);
    assert_eq!(
        due,
        [7, 9],
        "new failures cannot postpone the existing retry batch"
    );
    events
        .refuse
        .store(false, std::sync::atomic::Ordering::SeqCst);
    for key in due {
        assert_eq!(
            pending.arm(&mut events, key, path, Shape::File, now),
            Took::File
        );
    }
    assert_eq!(pending.wait(now), None, "recovery leaves no retry timer");
}

#[test]
fn retry_batches_advance_and_restart_after_the_pending_set_empties() {
    let start = Instant::now();
    let path = Path::new("fixture/unavailable");
    let mut events = Refusing::default();
    events
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut pending = Unarmed::default();
    assert_eq!(
        pending.arm(&mut events, 7, path, Shape::File, start),
        Took::No
    );
    let first = start + REARM_EVERY;
    assert_eq!(pending.due(first), [7]);
    assert_eq!(
        pending.arm(&mut events, 7, path, Shape::File, first + REARM_EVERY / 4),
        Took::No
    );
    assert_eq!(pending.wait(first + REARM_EVERY / 2), Some(REARM_EVERY / 2));
    assert_eq!(pending.due(first + REARM_EVERY), [7]);
    pending.forget(7);
    assert_eq!(pending.wait(first + REARM_EVERY), None);
    let later = first + REARM_EVERY * 4;
    assert_eq!(
        pending.arm(&mut events, 9, path, Shape::File, later),
        Took::No
    );
    assert_eq!(pending.wait(later), Some(REARM_EVERY));
    assert!(pending.due(later).is_empty());
    assert_eq!(pending.due(later + REARM_EVERY), [9]);
}

/// The files the watch only looks at are looked at `LOOK_EVERY` apart while a round finds one
/// changed or one more comes to be watched, twice as far apart after each round that finds none
/// up to `LOOK_SLOWEST`, and on no clock at all once none is left.
#[test]
fn looks_come_close_while_a_file_changes_and_far_apart_while_every_one_rests() {
    let start = Instant::now();
    let mut pending = Unarmed::default();
    assert_eq!(pending.rest(start), None, "nothing to look at, no clock");
    pending.look(7, start);
    assert_eq!(pending.rest(start), Some(LOOK_EVERY));
    assert_eq!(pending.looks.due(start + LOOK_EVERY / 2), None, "not yet");
    assert_eq!(pending.wait(start), None, "it is in no round of arms");

    // Rounds that find nothing changed: each rest is twice the last, up to the slowest.
    let mut now = start + LOOK_EVERY;
    let mut rests = Vec::new();
    for _ in 0..5 {
        assert_eq!(pending.looks.due(now), Some(vec![7]));
        pending.looks.rest(false);
        let rest = pending.rest(now).unwrap();
        rests.push(rest);
        now += rest;
    }
    let mut doubling = LOOK_EVERY;
    let expected: Vec<Duration> = (0..5)
        .map(|_| {
            doubling = (doubling * 2).min(LOOK_SLOWEST);
            doubling
        })
        .collect();
    assert_eq!(rests, expected);
    assert_eq!(rests.last(), Some(&LOOK_SLOWEST), "and no slower");
    assert!(LOOK_EVERY < LOOK_SLOWEST);

    // A round that finds a change brings the next one close again.
    assert_eq!(pending.looks.due(now), Some(vec![7]));
    pending.looks.rest(true);
    assert_eq!(pending.rest(now), Some(LOOK_EVERY));

    // So does one more file to look at, and the same file watched again does not.
    now += LOOK_EVERY;
    assert_eq!(pending.looks.due(now), Some(vec![7]));
    pending.looks.rest(false);
    assert_eq!(pending.rest(now), Some(LOOK_EVERY * 2));
    pending.look(7, now);
    assert_eq!(pending.rest(now), Some(LOOK_EVERY * 2));
    pending.look(9, now);
    assert_eq!(pending.rest(now), Some(LOOK_EVERY));
    assert_eq!(pending.looks.due(now + LOOK_EVERY), Some(vec![7, 9]));

    // A file no arm took is armed again on its own clock, and the watch rests for the sooner.
    let path = Path::new("fixture/unavailable");
    let mut events = Refusing::default();
    events
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let later = now + LOOK_EVERY + LOOK_EVERY / 4;
    assert_eq!(
        pending.arm(&mut events, 11, path, Shape::File, later),
        Took::No
    );
    assert_eq!(pending.wait(later), Some(REARM_EVERY));
    assert_eq!(
        pending.rest(later),
        Some(REARM_EVERY.min(LOOK_EVERY - LOOK_EVERY / 4))
    );

    pending.forget(7);
    pending.forget(9);
    pending.forget(11);
    assert_eq!(
        pending.rest(later + LOOK_SLOWEST),
        None,
        "none left, no clock"
    );
}

/// A file its host asks only to be looked at is never armed where the arm's listeners pin the
/// folders above them, nor is any file the arm does not listen for, and each is still told its
/// first look and its changes, by the watch's looks. The controls: the same arm arms a file
/// watched the plain way, and an arm whose listeners pin nothing (macOS, Linux) arms a
/// looked-at file as any other.
#[test]
fn a_looked_at_file_is_never_armed_where_listeners_pin_and_its_changes_are_told() {
    let dir = scratch("looked");
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let (document, feed, config) = (
        dir.path().join("doc.txt"),
        project.join("feed.toml"),
        dir.path().join("aterm.toml"),
    );
    for file in [&document, &feed, &config] {
        std::fs::write(file, "one\n").unwrap();
    }
    let events = Scripted {
        pins: true,
        outside: Some(project.clone()),
        ..Scripted::default()
    };
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    let next = || heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    watch.watch_looked(1, document.clone());
    assert_eq!(next(), Ok(1), "the looked-at file's first look");
    watch.watch(2, feed.clone());
    assert_eq!(next(), Ok(2), "the unlistened file's first look");
    std::fs::write(&document, "two, longer\n").unwrap();
    assert_eq!(next(), Ok(1), "the looked-at file's change");
    std::fs::write(&feed, "two, longer\n").unwrap();
    assert_eq!(next(), Ok(2), "the unlistened file's change");
    std::fs::remove_file(&document).unwrap();
    assert_eq!(next(), Ok(1), "the looked-at file gone");
    assert!(
        events.armed.lock().unwrap().is_empty(),
        "neither was ever armed"
    );

    // The control: the same arm is asked for a file watched the plain way.
    watch.watch(3, config.clone());
    assert_eq!(next(), Ok(3), "the first look");
    assert!(
        events
            .armed
            .lock()
            .unwrap()
            .iter()
            .all(|(key, path)| *key == 3 && *path == config),
        "only the plainly watched file is armed"
    );
    assert!(!events.armed.lock().unwrap().is_empty());
    // A key watched again the other way changes hands: looked at, it is told with no event.
    watch.watch_looked(3, config.clone());
    assert_eq!(next(), Ok(3), "the first look again");
    let arms = events.armed.lock().unwrap().len();
    std::fs::write(&config, "two, longer\n").unwrap();
    assert_eq!(next(), Ok(3), "its change, looked at");
    assert_eq!(
        events.armed.lock().unwrap().len(),
        arms,
        "and not armed again"
    );
    drop(watch);

    // The control: where listeners pin nothing, a looked-at file is armed as any other.
    let events = Scripted::default();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch_looked(1, config.clone());
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(1),
        "the first look"
    );
    assert_eq!(
        events.armed.lock().unwrap().first(),
        Some(&(1, config.clone()))
    );
    drop(watch);
}

type CommandTurn = (usize, Option<Duration>);

struct CommandTurns {
    stop: Arc<std::sync::atomic::AtomicBool>,
    seen: Arc<Mutex<BTreeSet<u64>>>,
    turns: Arc<Mutex<Vec<CommandTurn>>>,
    stop_after: Option<usize>,
    inert: InertEvents,
}

impl FileEvents for CommandTurns {
    fn arm(&mut self, _key: u64, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn disarm(&mut self, key: u64) {
        let count = {
            let mut seen = self.seen.lock().unwrap();
            seen.insert(key);
            seen.len()
        };
        if self.stop_after == Some(count) {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    fn wait(&mut self, timeout: Option<Duration>, _touched: &mut Vec<u64>) -> io::Result<()> {
        let count = self.seen.lock().unwrap().len();
        let mut turns = self.turns.lock().unwrap();
        turns.push((count, timeout));
        if count == 257 || turns.len() >= 8 {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    fn waker(&self) -> Waker {
        self.inert.waker()
    }
}

fn command_turns(stop_after: Option<usize>) -> (usize, Vec<CommandTurn>) {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let turns = Arc::new(Mutex::new(Vec::new()));
    let (send, receive) = mpsc::channel();
    for key in 0..257 {
        send.send(Command::Unwatch(key)).unwrap();
    }
    watch_loop(
        Box::new(CommandTurns {
            stop: Arc::clone(&stop),
            seen: Arc::clone(&seen),
            turns: Arc::clone(&turns),
            stop_after,
            inert: InertEvents::default(),
        }),
        &receive,
        &stop,
        Box::new(|_| {}),
    );
    drop(send);
    let count = seen.lock().unwrap().len();
    let turns = turns.lock().unwrap().clone();
    (count, turns)
}

#[test]
fn command_backlog_services_events_between_bounded_batches() {
    let (count, turns) = command_turns(None);
    eprintln!("command/event turns: {turns:?}");
    assert_eq!(
        count, 257,
        "queued commands must not be stranded after the wake drains"
    );
    assert!(
        turns[0].0 <= 128,
        "event service cannot wait for the whole backlog"
    );
    assert_eq!(turns[0].1, Some(Duration::ZERO));
    assert_eq!(turns[1].1, Some(Duration::ZERO));
    assert_eq!(turns.last().unwrap().1, None, "idle waits remain untimed");
}

#[test]
fn command_backlog_is_abandoned_when_stop_is_observed() {
    let (count, _) = command_turns(Some(1));
    eprintln!("commands applied after stop in first command: {count}");
    assert_eq!(count, 1, "shutdown must abandon the remaining backlog");
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
        "aterm-document-watch",
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
    // A wait before the watch has nothing to bound.
    let first_look = events.waits.lock().unwrap().len();
    // Refused at the watch and at its first look, then armed again unasked.
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "armed again", || {
        (*events.arms.lock().unwrap() >= 4).then_some(())
    });
    let refused_waits = events.waits.lock().unwrap()[first_look..].to_vec();
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

/// A file no arm takes hears no event, so each round that arms it again in
/// vain looks at it: its change is told all the same.
#[test]
fn a_file_no_arm_takes_is_looked_at_each_round_and_its_change_told() {
    let dir = scratch("amacs-watch-unarmed");
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let events = Refusing::default();
    events
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
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
    std::fs::write(&path, "two, longer\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(5),
        "a round's look told the change no event could"
    );
    assert!(
        *events.arms.lock().unwrap() >= 3,
        "still refused when it told"
    );
    drop(watch);
}

/// A folder's entries tell at every settled burst, whatever the folder's stamp says (its times
/// need not tell two entries made inside one tick apart), while a file tells only when its stamp
/// moved: one burst on both, with nothing changed, tells the folder alone.
#[test]
fn a_directory_key_tells_each_settled_burst_and_a_file_key_only_a_moved_stamp() {
    let dir = scratch("watch-directory-burst");
    let (file, folder) = (dir.path().join("aterm.toml"), dir.path().join("themes"));
    std::fs::write(&file, "one\n").unwrap();
    std::fs::create_dir(&folder).unwrap();
    let events = Scripted::default();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(1, file.clone());
    watch.watch_directory(2, folder);
    // Each armed at its watch and again at its first look, which tells.
    events.settled(4);
    let mut first: Vec<u64> = heard.try_iter().collect();
    first.sort_unstable();
    assert_eq!(first, [1, 2], "the first looks");
    assert_eq!(
        *events.entries.lock().unwrap(),
        [2, 2],
        "armed on its entries"
    );

    events.burst(&[1, 2]);
    events.settled(6);
    assert_eq!(
        heard.try_iter().collect::<Vec<_>>(),
        [2],
        "a burst with nothing changed tells the folder and not the file"
    );

    // The control: the file's change is told.
    std::fs::write(&file, "two, longer\n").unwrap();
    events.burst(&[1]);
    events.settled(7);
    assert_eq!(
        heard.try_iter().collect::<Vec<_>>(),
        [1],
        "the file changed"
    );
    drop(watch);
}

/// A folder not there yet is armed as a file is (the watch then waits on events alone, with no
/// timeout), so a burst above it that did not make it tells nothing; its making is told, and from
/// then it is armed on its entries, each burst of which tells, until it goes again.
#[test]
fn a_directory_key_on_a_missing_folder_is_heard_made_and_then_on_its_entries() {
    let dir = scratch("watch-directory-missing");
    let folder = dir.path().join("themes");
    let events = Scripted::default();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch_directory(2, folder.clone());
    events.settled(2);
    assert_eq!(heard.try_iter().collect::<Vec<_>>(), [2], "the first look");
    let untimed = |why: &str| {
        assert_eq!(
            events.waits.lock().unwrap().last(),
            Some(&None),
            "{why}: the watch waits on events alone"
        );
    };
    untimed("armed on the folder's folder");
    assert!(events.entries.lock().unwrap().is_empty());

    events.burst(&[2]);
    events.settled(3);
    assert!(
        heard.try_iter().next().is_none(),
        "a burst that made no folder tells nothing"
    );

    std::fs::create_dir(&folder).unwrap();
    events.burst(&[2]);
    events.settled(4);
    assert_eq!(heard.try_iter().collect::<Vec<_>>(), [2], "the folder made");
    assert_eq!(*events.entries.lock().unwrap(), [2], "armed on its entries");
    untimed("armed on its entries");

    events.burst(&[2]);
    events.settled(5);
    assert_eq!(
        heard.try_iter().collect::<Vec<_>>(),
        [2],
        "a burst on its entries"
    );

    std::fs::remove_dir(&folder).unwrap();
    events.burst(&[2]);
    events.settled(6);
    assert_eq!(heard.try_iter().collect::<Vec<_>>(), [2], "the folder gone");
    events.burst(&[2]);
    events.settled(7);
    assert!(
        heard.try_iter().next().is_none(),
        "armed as a file again, a burst that made no folder tells nothing"
    );

    // A file where the folder was is watched as a file too, and its making told.
    std::fs::write(&folder, "not a folder\n").unwrap();
    events.burst(&[2]);
    events.settled(8);
    assert_eq!(
        heard.try_iter().collect::<Vec<_>>(),
        [2],
        "a file made there"
    );
    assert_eq!(
        *events.entries.lock().unwrap(),
        [2, 2],
        "no arm on entries since the folder went"
    );
    untimed("armed on a file where the folder was");
    drop(watch);
}

/// Where no arm hears a folder's entries (the stat poll), a directory key is armed as a file:
/// the watch waits on events alone, a burst that left the folder's stamp alone tells nothing,
/// and one that moved it tells.
#[test]
fn a_directory_key_with_no_directory_events_is_armed_as_a_file() {
    let dir = scratch("watch-directory-files-only");
    let folder = dir.path().join("themes");
    std::fs::create_dir(&folder).unwrap();
    let events = Scripted {
        files_only: true,
        ..Scripted::default()
    };
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch_directory(3, folder.clone());
    events.settled(2);
    assert_eq!(heard.try_iter().collect::<Vec<_>>(), [3], "the first look");
    assert_eq!(
        events.waits.lock().unwrap().last(),
        Some(&None),
        "armed: the watch waits on events alone"
    );
    events.burst(&[3]);
    events.settled(3);
    assert!(
        heard.try_iter().next().is_none(),
        "a burst that left its stamp alone tells nothing"
    );
    std::fs::rename(&folder, dir.path().join("retired")).unwrap();
    events.burst(&[3]);
    events.settled(4);
    assert_eq!(heard.try_iter().collect::<Vec<_>>(), [3], "the folder gone");
    assert!(events.entries.lock().unwrap().is_empty());
    drop(watch);
}

/// A folder no arm took is looked at in rounds (so a change its stamp shows is told); the round
/// whose arm takes looks at it as a first look does, and that look on its entries tells: what
/// changed while nothing listened need not have moved the folder's stamp.
#[test]
fn a_directory_key_whose_arm_takes_in_a_round_is_told_as_at_a_first_look() {
    let dir = scratch("watch-directory-round");
    let folder = dir.path().join("themes");
    std::fs::create_dir(&folder).unwrap();
    let events = Refusing::default();
    events
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch_directory(5, folder);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(5),
        "the first look"
    );
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "armed in a round", || {
        (*events.arms.lock().unwrap() >= 3).then_some(())
    });
    // The wait after that round's arm comes after its look.
    let waits = events.waits.lock().unwrap().len();
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the watch waits", || {
        (events.waits.lock().unwrap().len() > waits).then_some(())
    });
    assert!(
        heard.try_recv().is_err(),
        "a round's look at an unchanged stamp tells nothing"
    );
    events
        .refuse
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(5),
        "the look after the arm took"
    );
    drop(watch);
}

/// A directory key on the platform's own events, telling each step it is driven through.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn platform_directory_watch(key: u64, path: PathBuf) -> (FileWatch, mpsc::Receiver<u64>) {
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::platform(
        "aterm-document-watch",
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch_directory(key, path);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(key),
        "the first look"
    );
    (watch, heard)
}

/// The platform's own events, through a folder's whole life under a directory key: made where it
/// was missing, then an entry made, replaced and removed, each told. Each step is one call, so
/// one burst: a tell left over from a step that came as two would answer the next step's.
/// Each tell comes after the arm its burst took, so the arm hears the step after it.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn the_platform_tells_a_directory_key_its_folder_made_and_its_entries_changed() {
    let dir = scratch("watch-directory-platform");
    let folder = dir.path().join("themes");
    let (watch, heard) = platform_directory_watch(4, folder.clone());
    let step = |why: &str, act: &dyn Fn()| {
        act();
        assert_eq!(
            heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
            Ok(4),
            "{why}"
        );
    };
    let (one, two) = (dir.path().join("one.conf"), dir.path().join("two.conf"));
    std::fs::write(&one, "one\n").unwrap();
    std::fs::write(&two, "two\n").unwrap();
    let entry = folder.join("dusk.conf");
    step("the folder made", &|| std::fs::create_dir(&folder).unwrap());
    step("an entry made", &|| std::fs::rename(&one, &entry).unwrap());
    step("an entry replaced", &|| {
        std::fs::rename(&two, &entry).unwrap()
    });
    step("an entry removed", &|| {
        std::fs::remove_file(&entry).unwrap()
    });
    drop(watch);
}

/// A directory key on a link that names no folder (a dotfiles link before its checkout, or
/// while one is made again) is looked at in rounds, which follow the link: the folder made where
/// it points is told, though no listener of the link's own folder could hear it, and from then
/// its entries are. The folder it names moved away and made again is told the same way.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_directory_key_on_a_link_to_no_folder_is_told_the_folder_made_and_then_its_entries() {
    let dir = scratch("watch-directory-dangling");
    let (conf, dotfiles) = (dir.path().join("conf"), dir.path().join("dotfiles"));
    std::fs::create_dir(&conf).unwrap();
    std::fs::create_dir(&dotfiles).unwrap();
    let target = dotfiles.join("themes");
    let link = conf.join("themes");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let (watch, heard) = platform_directory_watch(6, link);
    let step = |why: &str, act: &dyn Fn()| {
        act();
        assert_eq!(
            heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
            Ok(6),
            "{why}"
        );
    };
    let entry = dir.path().join("dusk.conf");
    std::fs::write(&entry, "one\n").unwrap();
    step("the folder made where the link points", &|| {
        std::fs::create_dir(&target).unwrap();
    });
    step("an entry made in it", &|| {
        std::fs::rename(&entry, target.join("dusk.conf")).unwrap();
    });
    step("the folder moved away", &|| {
        std::fs::rename(&target, dotfiles.join("retired")).unwrap();
    });
    step("the folder made again", &|| {
        std::fs::create_dir(&target).unwrap();
    });
    drop(watch);
}

/// A directory arm on a link to a folder (a dotfiles `~/.config/aterm/themes`) listens on the
/// folder the link names, and on the link itself, which hears it made to name another folder.
#[cfg(target_os = "macos")]
#[test]
fn a_directory_arm_on_a_link_hears_the_folder_it_names_and_the_link_replaced() {
    let dir = scratch("watch-directory-link");
    let (real, other) = (dir.path().join("real"), dir.path().join("other"));
    std::fs::create_dir(&real).unwrap();
    std::fs::create_dir(&other).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events
        .arm_directory(1, &link)
        .expect("armed on the folder the link names");
    let touched = heard_for(&mut events, || {
        std::fs::write(real.join("row"), "made\n").unwrap();
    });
    assert_eq!(touched, [1], "an entry made where the link points");

    let touched = heard_for(&mut events, || {
        let next = dir.path().join("link.next");
        std::os::unix::fs::symlink(&other, &next).unwrap();
        std::fs::rename(&next, &link).unwrap();
    });
    assert_eq!(touched, [1], "the link made to name another folder");
    events.arm_directory(1, &link).unwrap();
    let touched = heard_for(&mut events, || {
        std::fs::write(other.join("row"), "made\n").unwrap();
    });
    assert_eq!(touched, [1], "an entry made where it points now");

    // The negative controls: a link to a file, or to nothing, is no folder.
    let to_file = dir.path().join("to-file");
    std::os::unix::fs::symlink(real.join("row"), &to_file).unwrap();
    assert!(events.arm_directory(2, &to_file).is_err());
    let dangling = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("gone"), &dangling).unwrap();
    assert!(events.arm_directory(2, &dangling).is_err());
}

/// Make the link `link` name `folder` instead, in one rename.
#[cfg(target_os = "macos")]
fn retarget(link: &Path, folder: &Path) {
    let next = link.with_extension("next");
    std::os::unix::fs::symlink(folder, &next).unwrap();
    std::fs::rename(&next, link).unwrap();
}

/// A link made to name another folder after a directory arm listened on the folder it named,
/// and before the walk above listened on the link, is caught by the arm, which arms again on the
/// folder the link names now.
#[cfg(target_os = "macos")]
#[test]
fn a_directory_arm_through_a_link_made_to_name_another_folder_as_it_arms_arms_again() {
    let dir = scratch("watch-directory-link-moved");
    let (real, other) = (dir.path().join("real"), dir.path().join("other"));
    std::fs::create_dir(&real).unwrap();
    std::fs::create_dir(&other).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut events = kqueue::KqueueEvents::new().unwrap();
    let mut turns = 0;
    events
        .arm_directory_then(1, &link, |_| {
            turns += 1;
            if turns == 1 {
                retarget(&link, &other);
            }
        })
        .unwrap();
    assert_eq!(turns, 2, "armed again once the link named another folder");
    let touched = heard_for(&mut events, || {
        std::fs::write(other.join("row"), "made\n").unwrap();
    });
    assert_eq!(touched, [1], "an entry made where the link points now");
}

/// The same for a file in a linked folder: the link made to name another folder after its
/// folder's listener took, and before the walk above listened on the link, arms it again there.
#[cfg(target_os = "macos")]
#[test]
fn a_file_arm_through_a_link_made_to_name_another_folder_as_it_arms_arms_again() {
    let dir = scratch("watch-linked-folder-moved");
    let (real, other) = (dir.path().join("real"), dir.path().join("other"));
    std::fs::create_dir(&real).unwrap();
    std::fs::create_dir(&other).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let named = std::fs::canonicalize(&real).unwrap();
    let mut events = kqueue::KqueueEvents::new().unwrap();
    let mut moved = false;
    events
        .arm_around(1, &link.join("absent.txt"), |listen, at| {
            if listen == kqueue::Listen::Answered && at == named && !moved {
                moved = true;
                retarget(&link, &other);
            }
            Ok(())
        })
        .unwrap();
    assert!(moved);
    let touched = heard_for(&mut events, || {
        std::fs::write(other.join("absent.txt"), "made\n").unwrap();
    });
    assert_eq!(touched, [1], "made where the link points now");
}

/// A folder made unsearchable for the length of a test, searchable again when it ends however
/// it ends, so its scratch can be removed.
#[cfg(unix)]
struct Locked<'a>(&'a Path);

#[cfg(unix)]
impl<'a> Locked<'a> {
    fn new(folder: &'a Path) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        crate::host::could_not_run_as_root("a folder of mode 000");
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o000)).unwrap();
        Self(folder)
    }
}

#[cfg(unix)]
impl Drop for Locked<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// A path that goes from missing to one this user cannot look at is told (a stamp of neither
/// cannot tell them apart); the key is then looked at in rounds, though its arm took, so the
/// end of the refusal is told with no event to tell of it, and then the watch waits on events
/// alone again.
#[cfg(unix)]
#[test]
fn a_path_missing_then_refused_is_told_and_looked_at_until_it_reads() {
    let dir = scratch("watch-refused");
    let folder = dir.path().join("locked");
    let path = folder.join("aterm.toml");
    let events = Scripted::default();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(1, path.clone());
    events.settled(2);
    assert_eq!(heard.try_iter().collect::<Vec<_>>(), [1], "the first look");

    std::fs::create_dir(&folder).unwrap();
    std::fs::write(&path, "one\n").unwrap();
    let locked = Locked::new(&folder);
    events.burst(&[1]);
    events.settled(3);
    assert_eq!(
        heard.try_iter().collect::<Vec<_>>(),
        [1],
        "missing, then refused"
    );
    assert!(
        events
            .waits
            .lock()
            .unwrap()
            .last()
            .is_some_and(Option::is_some),
        "a refused key is in the rounds"
    );

    drop(locked);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(1),
        "readable again, which no event told"
    );
    let arms = events.armed.lock().unwrap().len();
    events.settled(arms);
    assert_eq!(
        events.waits.lock().unwrap().last(),
        Some(&None),
        "out of the rounds"
    );
    drop(watch);
}

/// The stat poll, where no file events exist, touches a key whose look moved: a path gone from
/// missing to refused (no stamp of either) among them, and nothing where no look moved.
#[cfg(unix)]
#[test]
fn the_stat_poll_touches_a_path_gone_from_missing_to_refused() {
    let dir = scratch("watch-poll-refused");
    let folder = dir.path().join("locked");
    let path = folder.join("aterm.toml");
    let mut events = PollEvents::default();
    events.arm(1, &path).unwrap();
    let mut touched = Vec::new();
    events.wait(Some(Duration::ZERO), &mut touched).unwrap();
    assert!(touched.is_empty(), "still missing");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(&path, "one\n").unwrap();
    let _locked = Locked::new(&folder);
    events.wait(Some(Duration::ZERO), &mut touched).unwrap();
    assert_eq!(touched, [1], "missing, then refused");
}

/// A link made again to name the same file is a new link, told though the file it names is
/// unchanged.
#[cfg(unix)]
#[test]
fn a_link_made_again_to_name_the_same_file_is_told() {
    let dir = scratch("watch-link-remade");
    let real = dir.path().join("real.toml");
    std::fs::write(&real, "one\n").unwrap();
    let link = dir.path().join("aterm.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let events = Scripted::default();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(1, link.clone());
    events.settled(2);
    assert_eq!(heard.try_iter().collect::<Vec<_>>(), [1], "the first look");

    // The negative control: a burst with nothing changed tells nothing.
    events.burst(&[1]);
    events.settled(3);
    assert!(heard.try_iter().next().is_none());

    let next = dir.path().join("aterm.toml.next");
    std::os::unix::fs::symlink(&real, &next).unwrap();
    std::fs::rename(&next, &link).unwrap();
    events.burst(&[1]);
    events.settled(4);
    assert_eq!(
        heard.try_iter().collect::<Vec<_>>(),
        [1],
        "the link made again"
    );
    drop(watch);
}

/// A relative path is watched where it names now, so the folders above it can be walked.
#[test]
fn a_relative_path_is_watched_where_it_names_now() {
    let events = Scripted::default();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
        Box::new(events.clone()),
        Box::new(|_| {}),
    )
    .unwrap();
    let (file, folder) = (
        PathBuf::from("aterm-watch-relative.toml"),
        PathBuf::from("aterm-watch-relative-themes"),
    );
    watch.watch(1, file.clone());
    watch.watch_directory(2, folder.clone());
    events.settled(4);
    let armed = events.armed.lock().unwrap().clone();
    for (key, path) in [(1, &file), (2, &folder)] {
        let at = std::path::absolute(path).unwrap();
        assert!(
            armed
                .iter()
                .filter(|(armed, _)| *armed == key)
                .all(|(_, armed)| *armed == at),
            "key {key} armed at {at:?}: {armed:?}"
        );
    }
    drop(watch);
}

/// A folder's mode changed, its own or one's above it, is heard: it can stop the lookup to the
/// path, or let it through.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_folders_mode_changed_on_the_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("watch-folder-mode");
    let above = dir.path().join("a");
    let own = above.join("b");
    std::fs::create_dir_all(&own).unwrap();
    let path = own.join("aterm.toml");
    std::fs::write(&path, "one\n").unwrap();
    let mode = |folder: &Path, mode: u32| {
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let mut events = platform_events().unwrap();
    events.arm(1, &path).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || mode(&own, 0o700)),
        [1],
        "its own folder's mode"
    );
    assert_eq!(
        heard_for(events.as_mut(), || mode(&above, 0o700)),
        [1],
        "the mode of a folder above"
    );
    events.arm_directory(2, &own).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || mode(&own, 0o755)),
        [1, 2],
        "a watched folder's own mode"
    );
}

/// A file whose folder is made unsearchable is told, and told again once it can be read: the
/// folder's own listener hears its mode change, and the key no arm takes while it is refused is
/// looked at in rounds.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_tells_a_file_its_folder_refused_and_then_let_through() {
    let dir = scratch("watch-folder-refused");
    let folder = dir.path().join("conf");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("aterm.toml");
    std::fs::write(&path, "one\n").unwrap();
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::platform(
        "aterm-document-watch",
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(1, path);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(1),
        "the first look"
    );
    let locked = Locked::new(&folder);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(1),
        "its folder refused"
    );
    drop(locked);
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(1),
        "its folder let through"
    );
    drop(watch);
}

/// An entry's mode changed in a folder above a file's own (an atomic write's `fchmod` beside the
/// state folder) reaches inotify's wait with the folder's own `IN_ATTRIB`, touches no key, and
/// ends no wait: the file's write after it is what the wait answers.
#[cfg(target_os = "linux")]
#[test]
fn an_inotify_wait_goes_on_past_an_event_that_touches_no_key() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("watch-inotify-quiet");
    let above = dir.path().join("state");
    let own = above.join("board");
    std::fs::create_dir_all(&own).unwrap();
    let sibling = above.join("vitals.json");
    std::fs::write(&sibling, "{}").unwrap();
    let path = own.join("row");
    std::fs::write(&path, "one").unwrap();
    let mut events = inotify::InotifyEvents::new().unwrap();
    events.arm(1, &path).unwrap();
    let mut touched = Vec::new();
    events.wait(Some(Duration::ZERO), &mut touched).unwrap();
    touched.clear();
    std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o600)).unwrap();
    // The write comes after the wait has read the sibling's event, unless this thread is slow
    // to wait, when both come in one read and the law holds unexercised.
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&path, "two").unwrap();
    });
    events
        .wait(Some(aterm_spec::wait::MUST_HAPPEN), &mut touched)
        .unwrap();
    writer.join().unwrap();
    // One write is a modify and a close, often read together.
    touched.dedup();
    assert_eq!(touched, [1], "the sibling's mode ended no wait");
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

/// Which keys `events` hears touched by `write`, past anything arming left to hear. The wait is a
/// hang detector for an event that must come.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn heard_for<E: FileEvents + ?Sized>(events: &mut E, write: impl FnOnce()) -> Vec<u64> {
    let mut touched = Vec::new();
    events.wait(Some(Duration::ZERO), &mut touched).unwrap();
    touched.clear();
    write();
    events
        .wait(Some(Duration::from_secs(60)), &mut touched)
        .unwrap();
    touched.sort_unstable();
    touched.dedup();
    touched
}

/// Directory listeners hear every entry, including replacing an existing file, and follow a
/// moved lookup ancestry after rearming. No lock file or sentinel participates in these writes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn directory_events_hear_entry_changes_and_rearm_after_an_ancestor_moves() {
    let dir = scratch("aterm-directory-events");
    let top = dir.path().join("top");
    let folder = top.join("rows");
    std::fs::create_dir_all(&folder).unwrap();
    let row = folder.join("one");
    std::fs::write(&row, "initial").unwrap();
    let mut events = platform_events().unwrap();
    events.arm_directory(7, &folder).unwrap();
    let next = dir.path().join("next");
    std::fs::write(&next, "replacement").unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::rename(&next, &row).unwrap()),
        [7]
    );
    let sibling = folder.join("two");
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&sibling, "new").unwrap()),
        [7]
    );
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::remove_file(&row).unwrap()),
        [7]
    );
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::rename(
            &top,
            dir.path().join("retired")
        )
        .unwrap()),
        [7]
    );
    std::fs::create_dir_all(&folder).unwrap();
    events.arm_directory(7, &folder).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&row, "current").unwrap()),
        [7]
    );
    events.disarm(7);
}

/// A file arm stays selective while directory arms share its kernel watch. Reusing a key in
/// either direction removes its former routing; releasing the last directory owner preserves
/// the named listener. Every check waits for an operation with a known positive recipient.
#[cfg(target_os = "linux")]
#[test]
fn directory_and_named_events_share_watches_without_broadening_file_arms() {
    let dir = scratch("aterm-directory-and-file-events");
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    std::fs::write(&one, "initial").unwrap();
    std::fs::write(&two, "initial").unwrap();
    let mut events = platform_events().unwrap();
    events.arm(1, &one).unwrap();
    events.arm_directory(2, dir.path()).unwrap();
    events.arm_directory(3, dir.path()).unwrap();
    events.arm_directory(3, dir.path()).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&two, "sibling").unwrap()),
        [2, 3]
    );
    events.arm(3, &one).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&two, "again").unwrap()),
        [2]
    );
    events.arm_directory(1, dir.path()).unwrap();
    events.disarm(2);
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::remove_file(&two).unwrap()),
        [1]
    );
    events.disarm(1);
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&one, "last owner")
            .unwrap()),
        [3]
    );
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn directory_events_refuse_a_missing_directory_and_a_regular_file() {
    let dir = scratch("aterm-directory-events-refused");
    let path = dir.path().join("entry");
    let mut events = platform_events().unwrap();
    assert!(events.arm_directory(1, &path).is_err());
    std::fs::write(&path, "a file is not a directory").unwrap();
    assert!(events.arm_directory(1, &path).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    events.arm_directory(1, &path).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(path.join("row"), "made")
            .unwrap()),
        [1]
    );
}

/// Replacing an ancestor after the directory's own watch takes must not leave that watch on
/// the retired inode. Key 2 supplies a known recipient on the current directory, so accepting
/// the stale registration fails by delivering only 2. A peer on the old inode must survive
/// key 1's retry and receive its own publication, without touching the new directory's keys.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn directory_arm_retries_replacement_before_ancestor_registration() {
    let dir = scratch("aterm-directory-arm-replaced");
    let top = dir.path().join("top");
    let folder = top.join("rows");
    let retired = dir.path().join("retired");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("row"), "old").unwrap();
    #[cfg(target_os = "macos")]
    let mut events = kqueue::KqueueEvents::new().unwrap();
    #[cfg(target_os = "linux")]
    let mut events = inotify::InotifyEvents::new().unwrap();
    events.arm(3, &folder.join("row")).unwrap();
    let mut replaced = false;
    events
        .arm_directory_then(1, &folder, |_| {
            if !std::mem::replace(&mut replaced, true) {
                std::fs::rename(&top, &retired).unwrap();
                std::fs::create_dir_all(&folder).unwrap();
            }
        })
        .unwrap();
    events.arm_directory(2, &folder).unwrap();
    assert_eq!(
        heard_for(&mut events, || std::fs::write(
            folder.join("row"),
            "current"
        )
        .unwrap()),
        [1, 2],
        "the retried arm hears the current directory with the positive control"
    );

    let next = dir.path().join("next");
    std::fs::write(&next, "retired peer").unwrap();
    assert_eq!(
        heard_for(&mut events, || std::fs::rename(
            &next,
            retired.join("rows/row")
        )
        .unwrap()),
        [3],
        "retry releases only its own retired-directory registration"
    );
}

/// An ancestor replaced at every registration boundary cannot be accepted as a stable arm.
/// The bounded refusal must leave the backend reusable once the path stops moving. No retry
/// count is part of this contract; the operations and their later native events are observed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn directory_arm_refuses_endless_replacement_and_can_be_reused() {
    let dir = scratch("aterm-directory-arm-unstable");
    let top = dir.path().join("top");
    let folder = top.join("rows");
    std::fs::create_dir_all(&folder).unwrap();
    #[cfg(target_os = "macos")]
    let mut events = kqueue::KqueueEvents::new().unwrap();
    #[cfg(target_os = "linux")]
    let mut events = inotify::InotifyEvents::new().unwrap();
    let mut replaced = 0;
    let answer = events.arm_directory_then(1, &folder, |_| {
        std::fs::rename(&top, dir.path().join(format!("retired-{replaced}"))).unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        replaced += 1;
    });
    assert_eq!(answer.unwrap_err().kind(), io::ErrorKind::WouldBlock);
    events.arm_directory(1, &folder).unwrap();
    events.arm_directory(2, &folder).unwrap();
    assert_eq!(
        heard_for(&mut events, || std::fs::write(folder.join("row"), "stable")
            .unwrap()),
        [1, 2],
        "a later stable arm works after the bounded refusal"
    );
}

/// A file made while its watch arms is heard being written. A folder hears a file made but no
/// write into one, so a file made right after the first listen must still get a listener of its
/// own, or every later write goes unheard.
#[cfg(target_os = "macos")]
#[test]
fn a_file_made_while_its_watch_arms_hears_its_next_write() {
    let dir = scratch("amacs-watch-arming");
    let path = dir.path().join("doc.txt");
    let mut events = kqueue::KqueueEvents::new().unwrap();
    let mut first = true;
    events
        .arm_around(1, &path, |step, _| {
            if step == kqueue::Listen::Answered && std::mem::take(&mut first) {
                std::fs::write(&path, "").unwrap();
            }
            Ok(())
        })
        .unwrap();

    let touched = heard_for(&mut events, || std::fs::write(&path, "written").unwrap());

    assert_eq!(touched, [1]);
}

/// A file and its folder that are both there take one listen each: there is nothing to go lower
/// for.
#[cfg(target_os = "macos")]
#[test]
fn a_file_already_there_arms_in_one_pass() {
    let dir = scratch("amacs-watch-arming-once");
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "").unwrap();
    let mut events = kqueue::KqueueEvents::new().unwrap();
    let mut listens = 0;

    events
        .arm_around(1, &path, |step, _| {
            listens += usize::from(step == kqueue::Listen::Answered);
            Ok(())
        })
        .unwrap();

    assert_eq!(listens, 2);
}

/// A file out of descriptors has only its folder, which hears no write into it, so the arm says
/// so and the watch arms it again. A file refused for good, as a link is, answers the same at every
/// look, so its folder is what it gets and the arm stands.
#[cfg(target_os = "macos")]
#[test]
fn a_file_out_of_descriptors_is_armed_again_and_a_link_is_not() {
    let dir = scratch("amacs-watch-arming-out");
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "").unwrap();
    let mut events = kqueue::KqueueEvents::new().unwrap();

    for out in [libc::EMFILE, libc::ENFILE] {
        let refused = events.arm_around(1, &path, |step, at| {
            if step == kqueue::Listen::Opened && at == path {
                Err(io::Error::from_raw_os_error(out))
            } else {
                Ok(())
            }
        });

        assert_eq!(refused.unwrap_err().raw_os_error(), Some(out));
    }

    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(events.arm_around(1, &link, |_, _| Ok(())).is_ok());
}

/// A folder deleted between its open and its filter never fires that filter, so the listen goes
/// to the folder above, which hears the folder made again.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_deleted_as_its_listen_takes_is_heard_made_again() {
    let dir = scratch("amacs-watch-arming-gone");
    let folder = dir.path().join("secrets");
    let path = folder.join("doc.txt");
    std::fs::create_dir(&folder).unwrap();
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events
        .arm_around(1, &path, |step, at| {
            if step == kqueue::Listen::Opened && at == folder && folder.exists() {
                std::fs::remove_dir(&folder).unwrap();
            }
            Ok(())
        })
        .unwrap();

    let touched = heard_for(&mut events, || std::fs::create_dir(&folder).unwrap());

    assert_eq!(touched, [1]);
}

/// A folder replaced between its open and its filter leaves that listen on the old one, so the arm
/// goes on to the folder the path names now.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_replaced_as_its_listen_takes_hears_the_new_one() {
    let dir = scratch("amacs-watch-arming-replaced");
    let folder = dir.path().join("secrets");
    let path = folder.join("doc.txt");
    std::fs::create_dir(&folder).unwrap();
    let mut replaced = false;
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events
        .arm_around(1, &path, |step, at| {
            if step == kqueue::Listen::Opened
                && at == folder
                && !std::mem::replace(&mut replaced, true)
            {
                std::fs::rename(&folder, dir.path().join("old")).unwrap();
                std::fs::create_dir(&folder).unwrap();
            }
            Ok(())
        })
        .unwrap();

    let touched = heard_for(&mut events, || std::fs::write(&path, "written").unwrap());

    assert_eq!(touched, [1]);
}

/// Folders still being made after every look leave the arm on a folder that may hear none of what
/// is below it, so the arm says it did not take and the watch arms it again.
#[cfg(target_os = "macos")]
#[test]
fn folders_still_being_made_after_every_look_fail_the_arm() {
    let dir = scratch("amacs-watch-arming-deep");
    let mut folder = dir.path().to_path_buf();
    let levels: Vec<PathBuf> = (0..=ARM_AGAIN + 2)
        .map(|level| {
            folder.push(level.to_string());
            folder.clone()
        })
        .collect();
    let path = folder.join("doc.txt");
    let mut events = kqueue::KqueueEvents::new().unwrap();

    let answer = events.arm_around(1, &path, |step, at| {
        // One more level at each look, the topmost still missing.
        if step == kqueue::Listen::Answered
            && levels
                .iter()
                .find(|level| !level.exists())
                .is_some_and(|next| next == at)
        {
            std::fs::create_dir(at).unwrap();
        }
        Ok(())
    });

    assert_eq!(answer.unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

/// A folder made just after its failed listen is one the listen above it never hears made, so the
/// arm has to see that folder and go lower, or nothing that happens in it is ever heard.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_made_while_its_watch_arms_hears_the_file_made_in_it() {
    let dir = scratch("amacs-watch-arming-folder");
    let folder = dir.path().join("secrets");
    let path = folder.join("doc.txt");
    let mut events = kqueue::KqueueEvents::new().unwrap();
    events
        .arm_around(1, &path, |step, at| {
            if step == kqueue::Listen::Answered && at == folder && !folder.exists() {
                std::fs::create_dir(&folder).unwrap();
            }
            Ok(())
        })
        .unwrap();

    let touched = heard_for(&mut events, || std::fs::write(&path, "written").unwrap());

    assert_eq!(touched, [1]);
}

/// A file whose folder, and that folder's folder, are not there yet is heard
/// once all three are made: the watch listens on the nearest folder that is
/// there and arms again lower as each one below it is made.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
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
        "aterm-document-watch",
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

/// One `struct inotify_event` for watch `wd` with `mask`, naming `name` (none
/// for an event on the watched folder itself).
fn inotify_event(wd: i32, mask: u32, name: &[u8]) -> Vec<u8> {
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

fn inotify_routes(
    keys: &BTreeMap<u64, (i32, InotifyEntry)>,
    above: &BTreeMap<u64, Vec<i32>>,
) -> InotifyRoutes {
    let mut routes = InotifyRoutes::default();
    for (key, (wd, name)) in keys {
        routes.add_own(*key, *wd, name);
    }
    for (key, watches) in above {
        routes.replace_above(*key, &[], watches);
    }
    routes
}

#[test]
fn inotify_routes_match_registration_scans_through_replacement_and_release() {
    let mut keys = BTreeMap::new();
    let mut above = BTreeMap::new();
    let mut files: BTreeMap<u64, i32> = BTreeMap::new();
    let mut routes = InotifyRoutes::default();
    let mut seed = 0x785f_190a_u64;
    for step in 0..1024 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = (seed >> 32) % 17;
        let wd = ((seed >> 16) % 9) as i32;
        match seed % 4 {
            0 => {
                let name = match (seed >> 8) % 3 {
                    0 => InotifyEntry::Named(b"x".to_vec()),
                    1 => InotifyEntry::Named(b"y".to_vec()),
                    _ => InotifyEntry::Directory,
                };
                if let Some((old_wd, old_name)) = keys.insert(key, (wd, name.clone())) {
                    routes.remove_own(key, old_wd, &old_name);
                }
                routes.add_own(key, wd, &name);
            }
            1 => {
                // Duplicate and overlapping watches must still hold the key once.
                let new = vec![wd, (wd + 1) % 9, wd];
                let old = above.insert(key, new.clone()).unwrap_or_default();
                routes.replace_above(key, &old, &new);
            }
            2 => {
                // A file watch made again on the same file, on another, or let go.
                let new = (!(seed >> 8).is_multiple_of(4)).then_some(wd);
                let old = match new {
                    Some(wd) => files.insert(key, wd),
                    None => files.remove(&key),
                };
                routes.replace_file(key, old, new);
            }
            _ => {
                if let Some((wd, name)) = keys.remove(&key) {
                    routes.remove_own(key, wd, &name);
                }
                let old = above.remove(&key).unwrap_or_default();
                routes.replace_above(key, &old, &[]);
                routes.replace_file(key, files.remove(&key), None);
            }
        }
        for wd in 0..10 {
            assert_eq!(
                routes.held(wd),
                keys.values().any(|(own, _)| *own == wd)
                    || above.values().any(|watches| watches.contains(&wd))
                    || files.values().any(|file| *file == wd),
                "watch ownership at step {step}"
            );
            for name in [b"".as_slice(), b"x", b"unwatched"] {
                for mask in [2, 4, INOTIFY_SELF, IN_Q_OVERFLOW] {
                    let mut expected = Vec::new();
                    if mask == IN_Q_OVERFLOW {
                        expected.extend(keys.keys().copied());
                    } else {
                        expected.extend(keys.iter().filter_map(|(key, (own, entry))| {
                            (*own == wd
                                && (name.is_empty()
                                    || match entry {
                                        InotifyEntry::Named(entry) => name == entry.as_slice(),
                                        InotifyEntry::Directory => true,
                                    }))
                            .then_some(*key)
                        }));
                        // A file's own watch carries no name.
                        if name.is_empty() {
                            expected.extend(
                                files
                                    .iter()
                                    .filter_map(|(key, file)| (*file == wd).then_some(*key)),
                            );
                            expected.sort_unstable();
                        }
                        // A folder's own move, end or attributes, not an entry's, reach above.
                        if name.is_empty() && mask & (INOTIFY_SELF | 4) != 0 {
                            expected.extend(above.iter().filter_map(|(key, watches)| {
                                watches.contains(&wd).then_some(*key)
                            }));
                        }
                    }
                    let mut touched = Vec::new();
                    inotify_touched(&inotify_event(wd, mask, name), &keys, &routes, &mut touched);
                    assert_eq!(touched, expected, "dispatch at step {step}, watch {wd}");
                }
            }
        }
    }
    for (key, (wd, name)) in keys {
        routes.remove_own(key, wd, &name);
    }
    for (key, watches) in above {
        routes.replace_above(key, &watches, &[]);
    }
    for (key, file) in files {
        routes.replace_file(key, Some(file), None);
    }
    assert!(
        routes.own.is_empty()
            && routes.directories.is_empty()
            && routes.above.is_empty()
            && routes.files.is_empty(),
        "no empty routes or dead registrations retained"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn shared_inotify_watches_survive_rearming_and_releasing_one_document() {
    let dir = scratch("aterm-inotify-shared-routes");
    let top = dir.path().join("top");
    std::fs::create_dir(&top).unwrap();
    let one = top.join("one");
    let two = top.join("two");
    for path in [&one, &two] {
        std::fs::write(path, "initial").unwrap();
    }
    let mut events = platform_events().unwrap();
    events.arm(1, &one).unwrap();
    events.arm(2, &two).unwrap();
    events.arm(3, &two).unwrap();
    events.arm(2, &two).unwrap();
    events.disarm(1);
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&two, "rewrite").unwrap()),
        [2, 3]
    );
    events.arm(3, &one).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&two, "again").unwrap()),
        [2]
    );
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::rename(
            &top,
            dir.path().join("moved")
        )
        .unwrap()),
        [2, 3]
    );
    events.disarm(2);
    events.disarm(3);
}

/// Manual work comparison for watched and unwatched entries across folder layouts.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual retired-instruction measurement, not a timing gate"]
fn inotify_dispatch_cost() {
    fn instructions() -> u64 {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
        // SAFETY: this flavor writes the complete buffer, read only on success.
        let result = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(result, 0);
        // SAFETY: success initialized the selected rusage buffer.
        unsafe { info.assume_init() }.ri_instructions
    }
    for count in [1u64, 64, 1024, 16384] {
        for shared in [false, true] {
            let keys: BTreeMap<_, _> = (0..count)
                .map(|key| {
                    (
                        key,
                        (
                            if shared { 1 } else { key as i32 + 1 },
                            InotifyEntry::Named(format!("file-{key}").into_bytes()),
                        ),
                    )
                })
                .collect();
            let above = BTreeMap::new();
            let routes = inotify_routes(&keys, &above);
            for hit in [false, true] {
                let event = inotify_event(1, 2, if hit { b"file-0" } else { b"unwatched" });
                let mut touched = Vec::with_capacity(count as usize);
                let before = instructions();
                let started = Instant::now();
                for _ in 0..1024 {
                    touched.clear();
                    inotify_touched(std::hint::black_box(&event), &keys, &routes, &mut touched);
                    assert_eq!(touched.len(), usize::from(hit));
                    std::hint::black_box(&touched);
                }
                let elapsed = started.elapsed();
                let retired = instructions() - before;
                eprintln!(
                    "keys={count} shared={shared} hit={hit} rounds=1024 ns={} instructions={retired}",
                    elapsed.as_nanos()
                );
            }
        }
    }
}

/// Linux's inotify events: one names its watch and entry, so it touches only
/// the key watching that file in that directory (the negative control), and
/// a queue overflow (watch -1, the kernel dropped events) touches every key,
/// whose looks then find which files really changed.
#[test]
fn an_inotify_event_touches_its_file_and_an_overflow_touches_every_file() {
    let event = inotify_event;
    let keys = BTreeMap::from([
        (1, (5, InotifyEntry::Named(b"doc.txt".to_vec()))),
        (2, (5, InotifyEntry::Named(b"other.txt".to_vec()))),
        (3, (6, InotifyEntry::Named(b"doc.txt".to_vec()))),
    ]);
    let routes = inotify_routes(&keys, &BTreeMap::new());
    let modify = event(5, 0x2, b"doc.txt");
    let mut touched = Vec::new();
    inotify_touched(&modify, &keys, &routes, &mut touched);
    assert_eq!(touched, [1], "only the file the event names");

    let mut touched = Vec::new();
    let mut buffer = modify;
    buffer.extend(event(-1, IN_Q_OVERFLOW, b""));
    inotify_touched(&buffer, &keys, &routes, &mut touched);
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
    inotify_touched(&huge_len, &keys, &routes, &mut touched);
    assert_eq!(touched, [1]);
}

/// A watch on a folder above a key's own hears that folder renamed or gone, or its attributes
/// changed (a `chmod` can stop the lookup through it), which touches the key. The same watch can
/// be another key's own folder, whose entry events name an entry: those touch only the key
/// watching that entry (the negative control), an entry's attributes included.
#[test]
fn an_inotify_folder_above_a_file_touches_it_only_when_it_moves_goes_or_changes_mode() {
    // Key 2's own folder (watch 7) is above key 1's (watch 5); watch 8 is above both.
    let keys = BTreeMap::from([
        (1, (5, InotifyEntry::Named(b"doc.txt".to_vec()))),
        (2, (7, InotifyEntry::Named(b"notes.txt".to_vec()))),
    ]);
    let above = BTreeMap::from([(1, vec![7, 8]), (2, vec![8])]);
    let routes = inotify_routes(&keys, &above);
    let heard = |buffer: Vec<u8>| {
        let mut touched = Vec::new();
        inotify_touched(&buffer, &keys, &routes, &mut touched);
        touched.sort_unstable();
        touched
    };

    assert_eq!(
        heard(inotify_event(7, 0x2, b"notes.txt")),
        [2],
        "an entry's write"
    );
    assert_eq!(
        heard(inotify_event(7, 0x100, b"doc")),
        Vec::<u64>::new(),
        "another entry made"
    );
    assert_eq!(
        heard(inotify_event(8, 0x800, b"")),
        [1, 2],
        "the folder above both renamed"
    );
    assert_eq!(
        heard(inotify_event(7, 0x400, b"")),
        [1, 2],
        "key 2's own folder gone"
    );
    assert_eq!(
        heard(inotify_event(7, 0x4, b"")),
        [1, 2],
        "key 2's own folder's attributes, on key 1's way"
    );
    assert_eq!(
        heard(inotify_event(8, 0x4, b"")),
        [1, 2],
        "the folder above both, its attributes"
    );
    assert_eq!(
        heard(inotify_event(7, 0x4, b"notes.txt")),
        [2],
        "an entry's attributes"
    );
    assert_eq!(
        heard(inotify_event(8, 0x4, b"other")),
        Vec::<u64>::new(),
        "another entry's attributes, in the folder above both"
    );
    assert_eq!(
        heard(inotify_event(8, 0x8000, b"")),
        Vec::<u64>::new(),
        "the folder above both let go"
    );
}

/// A file key hears a write and a change of mode made through another of the file's names (a
/// hard link in another folder), which its folder, which names only its own entries, does not.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_platform_hears_a_file_written_through_another_link() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("watch-hard-link");
    let own = dir.path().join("conf");
    let other = dir.path().join("other");
    std::fs::create_dir(&own).unwrap();
    std::fs::create_dir(&other).unwrap();
    let path = own.join("aterm.toml");
    std::fs::write(&path, "one\n").unwrap();
    let linked = other.join("linked.toml");
    std::fs::hard_link(&path, &linked).unwrap();
    let mut events = platform_events().unwrap();
    events.arm(1, &path).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&linked)
                .unwrap();
            file.write_all(b"two\n").unwrap();
        }),
        [1],
        "a write through the other link"
    );
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::set_permissions(
            &linked,
            std::fs::Permissions::from_mode(0o600)
        )
        .unwrap()),
        [1],
        "a change of mode through the other link"
    );
    events.disarm(1);
}

/// A file key armed again once its path names another file no longer hears the file it named:
/// a write through a link to the old one touches nothing, while a key on the folder the link is
/// in hears its own file written after it, in the same wait.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_file_key_armed_on_a_replaced_file_lets_the_old_one_go() {
    let dir = scratch("watch-hard-link-gone");
    let own = dir.path().join("conf");
    let other = dir.path().join("other");
    std::fs::create_dir(&own).unwrap();
    std::fs::create_dir(&other).unwrap();
    let path = own.join("aterm.toml");
    std::fs::write(&path, "one\n").unwrap();
    let linked = other.join("linked.toml");
    std::fs::hard_link(&path, &linked).unwrap();
    let sentinel = other.join("sentinel");
    std::fs::write(&sentinel, "one\n").unwrap();
    let mut events = platform_events().unwrap();
    events.arm(1, &path).unwrap();
    events.arm(2, &sentinel).unwrap();
    let temporary = own.join(".aterm.toml.new");
    std::fs::write(&temporary, "replaced\n").unwrap();
    std::fs::rename(&temporary, &path).unwrap();
    events.arm(1, &path).unwrap();
    assert_eq!(
        heard_for(events.as_mut(), || {
            std::fs::write(&linked, "the old file, written\n").unwrap();
            std::fs::write(&sentinel, "two\n").unwrap();
        }),
        [2],
        "the old file's write touched no key"
    );
    assert_eq!(
        heard_for(events.as_mut(), || std::fs::write(&path, "the new one\n")
            .unwrap()),
        [1],
        "the file the path names now"
    );
    events.disarm(1);
    events.disarm(2);
}

/// A key armed on a volume that does not report every writer's change (a network share, written
/// from another machine) is looked at in each round, so a change no event tells is told; armed
/// again on one that does, it leaves the rounds and the watch waits untimed.
#[test]
fn a_key_on_a_volume_that_misses_writers_is_looked_at_each_round() {
    let dir = scratch("watch-elsewhere");
    let path = dir.path().join("aterm.toml");
    std::fs::write(&path, "one\n").unwrap();
    let events = Scripted::default();
    events.elsewhere.lock().unwrap().insert(5);
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-document-watch",
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
    std::fs::write(&path, "two, written from another machine\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(5),
        "a round's look told what no event did"
    );
    let arms = events.armed.lock().unwrap().len();
    events.elsewhere.lock().unwrap().clear();
    events.burst(&[5]);
    events.settled(arms + 1);
    assert_eq!(
        events.waits.lock().unwrap().last(),
        Some(&None),
        "armed where every writer is heard, no round is due"
    );
    assert_eq!(heard.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(watch);
}

/// A Mac volume reports every writer's change when it is local and not a user-space one; the
/// type name is matched whatever its case.
#[test]
fn a_mac_volume_reports_every_writer_when_local_and_in_the_kernel() {
    const MNT_LOCAL: u32 = 0x1000;
    for (flags, fstype, every) in [
        (MNT_LOCAL, "apfs", true),
        (MNT_LOCAL | 0x1, "hfs", true),
        (MNT_LOCAL, "msdos", true),
        (0, "apfs", false),
        (0, "smbfs", false),
        (MNT_LOCAL, "nfs", false),
        (MNT_LOCAL, "macfuse", false),
        (MNT_LOCAL, "SMBFS", false),
        (MNT_LOCAL, "virtiofs", false),
    ] {
        assert_eq!(
            volume_reports_every_writer(flags, fstype),
            every,
            "{fstype} with flags {flags:#x}"
        );
    }
}

/// A Linux volume reports every writer's change unless it is a network one, a virtual machine's
/// share, a cluster's shared disk or a user-space one; an overlay does.
#[cfg(unix)]
#[test]
fn a_linux_volume_reports_every_writer_unless_written_elsewhere() {
    for (magic, every) in [
        (0xEF53, true),      // ext4
        (0x9123_683E, true), // btrfs
        (0x0102_1994, true), // tmpfs
        (0x794C_7630, true), // overlay
        (0x5846_5342, true), // xfs
        (0x2FC1_2FC1, true), // zfs
    ] {
        assert_eq!(volume_type_reports_every_writer(magic), every, "{magic:#x}");
    }
    for magic in crate::shared_write::REMOTE_FILESYSTEMS.into_iter().chain([
        0x517B,
        0x786F_4256,
        0x7C7C_6673,
        0xBACB_ACBC,
        0x7461_636F,
        0x0116_1970,
        0x7375_7245,
        0x2003_0528,
    ]) {
        assert!(!volume_type_reports_every_writer(magic), "{magic:#x}");
    }
}

/// The platform says a key in scratch, on this machine's own disk, hears every writer.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn the_platform_hears_every_writer_on_a_local_volume() {
    let dir = scratch("watch-local-volume");
    let path = dir.path().join("aterm.toml");
    std::fs::write(&path, "one\n").unwrap();
    let mut events = platform_events().unwrap();
    events.arm(1, &path).unwrap();
    events.arm_directory(2, dir.path()).unwrap();
    assert!(events.hears_every_writer(1), "a file key");
    assert!(events.hears_every_writer(2), "a directory key");
    events.disarm(1);
    events.disarm(2);
}

/// inotify's answer for a key matches what the kernel's mount table says the volume is: a key in
/// this crate's own folder, which a virtual machine's share holds in the Linux guest lane, is
/// looked at in rounds there, and heard from every writer on a local disk.
#[cfg(target_os = "linux")]
#[test]
fn an_inotify_key_hears_every_writer_as_its_mount_says() {
    use std::os::unix::ffi::OsStrExt as _;
    // The kernel writes a space, tab, newline or backslash in a mount point as `\NNN` in octal.
    fn unescaped(field: &str) -> PathBuf {
        let bytes = field.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            let octal = bytes.get(at + 1..at + 4).and_then(|digits| {
                std::str::from_utf8(digits)
                    .ok()
                    .and_then(|digits| u8::from_str_radix(digits, 8).ok())
            });
            match (bytes[at], octal) {
                (b'\\', Some(byte)) => {
                    out.push(byte);
                    at += 4;
                }
                (byte, _) => {
                    out.push(byte);
                    at += 1;
                }
            }
        }
        PathBuf::from(std::ffi::OsStr::from_bytes(&out))
    }
    let folder = std::fs::canonicalize(aterm_tempfile::manifest_dir()).unwrap();
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap();
    let fstype = mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let _source = fields.next()?;
            let at = unescaped(fields.next()?);
            let fstype = fields.next()?;
            folder
                .starts_with(&at)
                .then(|| (at.components().count(), fstype.to_owned()))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, fstype)| fstype)
        .unwrap();
    // A FUSE mount's type names its program after a dot (`fuse.sshfs`, `fuseblk.ntfs-3g`).
    let elsewhere = matches!(
        fstype.split('.').next().unwrap_or_default(),
        "nfs"
            | "nfs4"
            | "cifs"
            | "smb3"
            | "smbfs"
            | "ceph"
            | "afs"
            | "9p"
            | "lustre"
            | "gpfs"
            | "fuse"
            | "fuseblk"
            | "virtiofs"
            | "vboxsf"
            | "prl_fs"
            | "vmhgfs"
            | "ocfs2"
            | "gfs2"
            | "coda"
            | "pvfs2"
    );
    let mut events = platform_events().unwrap();
    events.arm(1, &folder.join("Cargo.toml")).unwrap();
    assert_eq!(events.hears_every_writer(1), !elsewhere, "{fstype}");
    events.disarm(1);
}

/// A lookup's chain of names: a path that is no link is its own; a link to a link to a file
/// names each in turn, a relative target from its link's folder and an absolute one as it
/// stands; a dangling link ends at the path it names; a loop ends after the kernel's count; a
/// relative path is made absolute.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_link_chain_names_each_hop_a_lookup_takes() {
    let dir = scratch("watch-link-chain");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let conf = root.join("conf");
    std::fs::create_dir(&conf).unwrap();
    let file = conf.join("aterm.toml");
    std::fs::write(&file, "one\n").unwrap();
    assert_eq!(link_chain(&file), std::slice::from_ref(&file), "no link");

    let middle = root.join("middle.toml");
    std::os::unix::fs::symlink("conf/aterm.toml", &middle).unwrap();
    let first = conf.join("first.toml");
    std::os::unix::fs::symlink(&middle, &first).unwrap();
    assert_eq!(
        link_chain(&first),
        [first.clone(), middle.clone(), root.join("conf/aterm.toml")],
        "an absolute hop, then a relative one"
    );

    let dangling = root.join("dangling.toml");
    std::os::unix::fs::symlink("gone/aterm.toml", &dangling).unwrap();
    assert_eq!(
        link_chain(&dangling),
        [dangling.clone(), root.join("gone/aterm.toml")],
        "a link to nothing ends where it points"
    );

    let a = root.join("a");
    let b = root.join("b");
    std::os::unix::fs::symlink("b", &a).unwrap();
    std::os::unix::fs::symlink("a", &b).unwrap();
    let chain = link_chain(&a);
    assert_eq!(chain.len(), LOOKUP_LINKS + 1, "a loop ends");
    assert!(chain.iter().all(|hop| *hop == a || *hop == b));

    let relative = Path::new("relative-link-chain.toml");
    assert_eq!(
        link_chain(relative),
        [std::env::current_dir().unwrap().join(relative)],
        "made absolute where it names now"
    );
}

/// A file key named through a link its reader cannot read (a link made under a strict umask,
/// which a lookup still goes through) is left to the rounds: no listener can be put on what it
/// names, in another folder here, and a round's look, which follows the link, tells a write
/// made there in place.
/// NEGATIVE CONTROL: arming it as a file (dropping `Took::of`'s unread-link arm) listens only on
/// the link's own folder, and the write is never told.
#[cfg(target_os = "macos")]
#[test]
fn a_file_key_through_a_link_it_cannot_read_is_looked_at_each_round() {
    let dir = scratch("watch-unread-link");
    let real = std::fs::canonicalize(dir.path()).unwrap();
    let other = real.join("other");
    std::fs::create_dir(&other).unwrap();
    let target = other.join("fleet.md");
    std::fs::write(&target, "one\n").unwrap();
    let link = real.join("fleet.md");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let named = std::ffi::CString::new(link.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a NUL-terminated path; the call changes the link's own mode, not its target's.
    let changed =
        unsafe { libc::fchmodat(libc::AT_FDCWD, named.as_ptr(), 0, libc::AT_SYMLINK_NOFOLLOW) };
    assert_eq!(changed, 0, "{}", io::Error::last_os_error());
    assert_eq!(
        std::fs::read_link(&link).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied,
        "the link refuses its reader",
    );
    assert_eq!(
        std::fs::read_to_string(&link).unwrap(),
        "one\n",
        "a lookup goes through"
    );
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::platform(
        "aterm-document-watch",
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    watch.watch(3, link.clone());
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(3),
        "the first look"
    );
    std::fs::write(&target, "two, written in place\n").unwrap();
    assert_eq!(
        heard.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(3),
        "a round's look through the link told the write"
    );
    drop(watch);
}

/// A volume mounted or unmounted is a change of every key's path; a low disk or a server that
/// stopped answering moves none.
#[cfg(target_os = "macos")]
#[test]
fn a_mount_or_unmount_is_heard_and_no_other_volume_event() {
    for (fflags, moves) in [
        (0x0008, true),
        (0x0010, true),
        (0x0018, true),
        (0x0004, false),
        (0x0001, false),
        (0x0100, false),
        (0, false),
    ] {
        assert_eq!(kqueue::remounted(fflags), moves, "{fflags:#x}");
    }
}
