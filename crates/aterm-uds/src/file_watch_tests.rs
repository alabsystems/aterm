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
    let (mut read, mut write) = crate::kept_out(std::io::pipe).unwrap();
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
    let end = name.floor_char_boundary(24);
    let prefix = format!("aterm-watch-{}-", &name[..end]);
    aterm_tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".noindex")
        .tempdir()
        .expect("create watch scratch directory")
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

#[test]
fn failed_rearms_do_not_postpone_other_pending_files() {
    let start = Instant::now();
    let path = Path::new("fixture/unavailable");
    let mut events = Refusing::default();
    events
        .refuse
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut pending = Unarmed::default();
    assert!(!pending.arm(&mut events, 7, path, start));
    for part in 1..4 {
        assert!(!pending.arm(&mut events, 9, path, start + REARM_EVERY / 4 * part));
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
        assert!(pending.arm(&mut events, key, path, now));
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
    assert!(!pending.arm(&mut events, 7, path, start));
    let first = start + REARM_EVERY;
    assert_eq!(pending.due(first), [7]);
    assert!(!pending.arm(&mut events, 7, path, first + REARM_EVERY / 4));
    assert_eq!(pending.wait(first + REARM_EVERY / 2), Some(REARM_EVERY / 2));
    assert_eq!(pending.due(first + REARM_EVERY), [7]);
    pending.forget(7);
    assert_eq!(pending.wait(first + REARM_EVERY), None);
    let later = first + REARM_EVERY * 4;
    assert!(!pending.arm(&mut events, 9, path, later));
    assert_eq!(pending.wait(later), Some(REARM_EVERY));
    assert!(pending.due(later).is_empty());
    assert_eq!(pending.due(later + REARM_EVERY), [9]);
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
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
    let mut routes = InotifyRoutes::default();
    let mut seed = 0x785f_190a_u64;
    for step in 0..1024 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = (seed >> 32) % 17;
        let wd = ((seed >> 16) % 9) as i32;
        match seed % 3 {
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
            _ => {
                if let Some((wd, name)) = keys.remove(&key) {
                    routes.remove_own(key, wd, &name);
                }
                let old = above.remove(&key).unwrap_or_default();
                routes.replace_above(key, &old, &[]);
            }
        }
        for wd in 0..10 {
            assert_eq!(
                routes.held(wd),
                keys.values().any(|(own, _)| *own == wd)
                    || above.values().any(|watches| watches.contains(&wd)),
                "watch ownership at step {step}"
            );
            for name in [b"".as_slice(), b"x", b"unwatched"] {
                for mask in [2, INOTIFY_SELF, IN_Q_OVERFLOW] {
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
                        if name.is_empty() && mask & INOTIFY_SELF != 0 {
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
    assert!(
        routes.own.is_empty() && routes.directories.is_empty() && routes.above.is_empty(),
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

/// A watch on a folder above a key's own hears that folder renamed or gone, which touches the key.
/// The same watch can be another key's own folder, whose entry events name an entry: those touch
/// only the key watching that entry (the negative control).
#[test]
fn an_inotify_folder_above_a_file_touches_it_only_when_it_moves_or_goes() {
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
        [2],
        "key 2's own folder changed in place"
    );
    assert_eq!(
        heard(inotify_event(8, 0x8000, b"")),
        Vec::<u64>::new(),
        "the folder above both let go"
    );
}
