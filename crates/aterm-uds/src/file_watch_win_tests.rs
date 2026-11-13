// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Laws of the Windows file-event arm.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use super::super::{FileEvents, FileWatch, Waker};
use super::{Hear, Role, WinEvents, folded, millis, notified};

fn scratch(name: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-watch-win-{name}-"))
        .tempdir()
        .expect("create watch scratch directory")
}

/// The arm, listening only inside `own`: a scratch folder standing in for the account's
/// application-data folders, wherever this machine keeps its temporary files.
fn inside(own: &Path) -> WinEvents {
    WinEvents::inside(vec![std::fs::canonicalize(own).unwrap()]).unwrap()
}

/// Which keys `events` hears touched by `act`, past anything arming left to hear. The wait is a
/// hang detector for an event that must come.
fn heard_for(events: &mut dyn FileEvents, act: impl FnOnce()) -> Vec<u64> {
    let mut touched = Vec::new();
    events.wait(Some(Duration::ZERO), &mut touched).unwrap();
    touched.clear();
    act();
    events.wait(Some(MUST_HAPPEN), &mut touched).unwrap();
    touched.dedup();
    touched
}

/// Any arm, counting the arms the watch asked of it, the waits the watch began, whether the last
/// it began had no timeout and how many had none, and the waits that returned.
struct Waits {
    inner: Box<dyn FileEvents>,
    arms: Arc<AtomicUsize>,
    began: Arc<AtomicUsize>,
    untimed: Arc<AtomicBool>,
    untimed_waits: Arc<AtomicUsize>,
    returned: Arc<AtomicUsize>,
}

impl FileEvents for Waits {
    fn arm(&mut self, key: u64, path: &Path) -> std::io::Result<()> {
        self.arms.fetch_add(1, Ordering::SeqCst);
        self.inner.arm(key, path)
    }

    fn arm_directory(&mut self, key: u64, path: &Path) -> std::io::Result<()> {
        self.arms.fetch_add(1, Ordering::SeqCst);
        self.inner.arm_directory(key, path)
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

    fn wait(&mut self, timeout: Option<Duration>, touched: &mut Vec<u64>) -> std::io::Result<()> {
        self.untimed.store(timeout.is_none(), Ordering::SeqCst);
        if timeout.is_none() {
            self.untimed_waits.fetch_add(1, Ordering::SeqCst);
        }
        self.began.fetch_add(1, Ordering::SeqCst);
        let waited = self.inner.wait(timeout, touched);
        self.returned.fetch_add(1, Ordering::SeqCst);
        waited
    }

    fn waker(&self) -> Waker {
        self.inner.waker()
    }
}

struct Counts {
    arms: Arc<AtomicUsize>,
    began: Arc<AtomicUsize>,
    untimed: Arc<AtomicBool>,
    untimed_waits: Arc<AtomicUsize>,
    returned: Arc<AtomicUsize>,
}

impl Counts {
    /// Wait until the watch is parked in a wait it began with no timeout and nothing more to do:
    /// the count of waits that returned by then.
    fn parked(&self) -> usize {
        until_within(MUST_HAPPEN, "the watch parks untimed", || {
            let returned = self.returned.load(Ordering::SeqCst);
            (self.untimed.load(Ordering::SeqCst)
                && self.began.load(Ordering::SeqCst) == returned + 1
                && self.returned.load(Ordering::SeqCst) == returned)
                .then_some(returned)
        })
    }
}

/// How a test watches its path.
#[derive(Clone, Copy)]
enum As {
    /// [`FileWatch::watch`].
    Watched,
    /// [`FileWatch::watch_looked`].
    Looked,
}

/// A watch of `path` as key 1 over `inner`, past its first look.
fn counted_watch(
    inner: WinEvents,
    path: &Path,
    how: As,
) -> (FileWatch, mpsc::Receiver<u64>, Counts) {
    let counts = Counts {
        arms: Arc::new(AtomicUsize::new(0)),
        began: Arc::new(AtomicUsize::new(0)),
        untimed: Arc::new(AtomicBool::new(false)),
        untimed_waits: Arc::new(AtomicUsize::new(0)),
        returned: Arc::new(AtomicUsize::new(0)),
    };
    let events = Waits {
        inner: Box::new(inner),
        arms: Arc::clone(&counts.arms),
        began: Arc::clone(&counts.began),
        untimed: Arc::clone(&counts.untimed),
        untimed_waits: Arc::clone(&counts.untimed_waits),
        returned: Arc::clone(&counts.returned),
    };
    let (told, heard) = mpsc::channel();
    let watch = FileWatch::spawn(
        "aterm-watch-win-test",
        Box::new(events),
        Box::new(move |key| {
            let _ = told.send(key);
        }),
    )
    .unwrap();
    match how {
        As::Watched => watch.watch(1, path.to_path_buf()),
        As::Looked => watch.watch_looked(1, path.to_path_buf()),
    }
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the first look");
    (watch, heard, counts)
}

/// A watch at rest over the event arm is on no clock: the watch hands the arm a wait with no
/// timeout (which the arm gives the port as `INFINITE`,
/// `a_due_wait_polls_at_once_and_a_short_one_never_spins`) and has none other outstanding. The
/// next write ends that wait and is told, so the parked watch was alive. The control is a file
/// outside the arm's own folders, which the watch never parks untimed on: it owes it a look.
#[test]
fn a_watch_at_rest_parks_in_an_untimed_wait_and_hears_the_next_write() {
    let dir = scratch("quiet");
    let path = dir.path().join("aterm.toml");
    std::fs::write(&path, "one\n").unwrap();

    let (watch, heard, counts) = counted_watch(inside(dir.path()), &path, As::Watched);
    let at_rest = counts.parked();
    std::fs::write(&path, "two, longer\n").unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the write");
    assert!(
        counts.returned.load(Ordering::SeqCst) > at_rest,
        "the write ended the untimed wait"
    );
    drop(watch);

    // The control: every wait the watch begins over a file it only looks at has a timeout. Only
    // a wait begun before the watch took the file in can have none, and that one ended before
    // the first look was told, so it is in the count taken here.
    let own = dir.path().join("own");
    std::fs::create_dir(&own).unwrap();
    let (watch, _heard, counts) = counted_watch(inside(&own), &path, As::Watched);
    let untimed = counts.untimed_waits.load(Ordering::SeqCst);
    let returned = counts.returned.load(Ordering::SeqCst);
    until_within(MUST_HAPPEN, "the watch waits on its clock", || {
        (counts.returned.load(Ordering::SeqCst) >= returned + 2).then_some(())
    });
    assert_eq!(
        counts.untimed_waits.load(Ordering::SeqCst),
        untimed,
        "a looked-at file owes a look, so no wait is untimed"
    );
    drop(watch);
}

/// A file outside the arm's own folders (a document in a project) is never armed and nothing is
/// held open for it: its folder and every folder above it can be renamed while it is watched,
/// and its changes are still told, by the watch's looks. The control is the same file with its
/// folders the arm's own, where a listener is held and the folder above it cannot be renamed.
#[test]
fn a_file_outside_the_arms_own_folders_is_looked_at_and_its_folders_stay_free_to_rename() {
    let dir = scratch("project");
    let own = dir.path().join("appdata");
    std::fs::create_dir(&own).unwrap();
    let top = dir.path().join("work");
    let repo = top.join("repo");
    let folder = repo.join("src");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("lib.rs");
    std::fs::write(&path, "one\n").unwrap();

    let mut events = inside(&own);
    assert!(
        !events.listens_for(&path),
        "a project file is not listened for"
    );
    assert!(events.listens_for(&own.join("aterm").join("aterm.toml")));
    assert_eq!(
        events.arm(1, &path).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported,
        "and an arm asked for anyway is refused"
    );
    assert_eq!(
        events.arm_directory(1, &folder).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(events.held(), (0, 0), "nothing is held open");

    let (watch, heard, counts) = counted_watch(events, &path, As::Watched);
    // The file's own folder, the one above it and the one above that each move and move back.
    for moved in [&folder, &repo, &top] {
        let away = moved.with_extension("moved");
        std::fs::rename(moved, &away).unwrap();
        assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the file went");
        std::fs::rename(&away, moved).unwrap();
        assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the file came back");
    }
    std::fs::write(&path, "two, longer\n").unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the write");
    assert_eq!(
        counts.arms.load(Ordering::SeqCst),
        0,
        "the watch never asked the arm for it"
    );
    drop(watch);

    // The control: with the folders the arm's own, the watch holds a listener, and the folder
    // above the listened one cannot be renamed.
    let (watch, _heard, counts) = counted_watch(inside(dir.path()), &path, As::Watched);
    counts.parked();
    assert!(counts.arms.load(Ordering::SeqCst) > 0);
    assert!(
        std::fs::rename(&repo, repo.with_extension("moved")).is_err(),
        "renamed above a listened folder"
    );
    drop(watch);
    std::fs::rename(&repo, repo.with_extension("moved")).unwrap();
}

/// A file its host asked only to be looked at (a document) is never armed though it is inside
/// the arm's own folders, so the folders above it stay free to rename; and a writer that keeps
/// it open is followed with no flush and no close, since a look reads the file's size as it is.
#[test]
fn a_looked_at_file_holds_nothing_open_and_follows_a_writer_that_keeps_it_open() {
    use std::io::Write as _;
    let dir = scratch("looked");
    let top = dir.path().join("a");
    let folder = top.join("b");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("log.txt");
    std::fs::write(&path, "one\n").unwrap();
    let (watch, heard, counts) = counted_watch(inside(dir.path()), &path, As::Looked);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"appended and left in the cache\n").unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the append");
    file.write_all(b"and again\n").unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the next append");
    drop(file);

    let away = dir.path().join("a.moved");
    std::fs::rename(&top, &away).unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the file went");
    assert_eq!(
        counts.arms.load(Ordering::SeqCst),
        0,
        "the watch never asked the arm for it"
    );
    drop(watch);
}

/// An application-data folder that is a junction into a project is not the arm's own: where
/// the folder really is decides, so no listener pins the project's folders.
#[test]
fn a_junction_out_of_the_arms_own_folders_is_not_listened_through() {
    crate::host::could_not_run_under_redirection_guard(
        "the test makes a junction as this user and goes through it",
    );
    let dir = scratch("junction");
    let own = dir.path().join("appdata");
    let dotfiles = dir.path().join("dotfiles");
    let real = dotfiles.join("aterm");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir(&own).unwrap();
    std::fs::write(real.join("aterm.toml"), "one\n").unwrap();
    let link = own.join("aterm");
    let status = crate::spawnfd::command("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&real)
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .status()
        .expect("run mklink");
    assert!(status.success(), "mklink /J {link:?} {real:?}");
    let path = link.join("aterm.toml");

    let mut events = inside(&own);
    assert!(!events.listens_for(&path));
    assert_eq!(
        events.arm(1, &path).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert_eq!(events.held(), (0, 0));
    std::fs::rename(&dotfiles, dir.path().join("dotfiles.moved")).unwrap();
}

/// A key whose folder's own name has no listener (the kernel refused one on the folder above)
/// is armed on its folder's entries and says it does not hear every change, so the watch looks
/// at it in its rounds: its folder renamed away, which no listener hears, is told. The control
/// is the same key with the listener above, which hears every change.
#[test]
fn a_key_whose_folder_above_refuses_a_listener_is_looked_at_and_hears_its_folder_renamed() {
    let dir = scratch("deaf-above");
    let folder = dir.path().join("f");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();

    let mut control = inside(dir.path());
    control.arm(1, &path).unwrap();
    assert_eq!(control.held().0, 2, "the entries and the name above");
    assert!(control.hears_every_writer(1));
    control.arm_directory(2, &folder).unwrap();
    assert!(control.hears_every_writer(2));
    drop(control);

    let mut events = inside(dir.path());
    events.deaf_above = true;
    events.arm(1, &path).unwrap();
    assert_eq!(events.held().0, 1, "the entries alone");
    assert!(
        !events.hears_every_writer(1),
        "its folder's rename is unheard"
    );
    events.arm_directory(2, &folder).unwrap();
    assert!(!events.hears_every_writer(2));
    events.disarm(2);
    events.disarm(1);
    assert!(events.hears_every_writer(1), "a let-go key is in no round");

    let (watch, heard, _counts) = counted_watch(events, &path, As::Watched);
    std::fs::write(&path, "two, longer\n").unwrap();
    assert_eq!(
        heard.recv_timeout(MUST_HAPPEN),
        Ok(1),
        "a write is still heard"
    );
    std::fs::rename(&folder, dir.path().join("f.old")).unwrap();
    assert_eq!(
        heard.recv_timeout(MUST_HAPPEN),
        Ok(1),
        "the folder renamed away"
    );
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(&path, "three, in the folder made again\n").unwrap();
    assert_eq!(
        heard.recv_timeout(MUST_HAPPEN),
        Ok(1),
        "the file made again at its path"
    );
    drop(watch);
}

/// What a writer that keeps the file open is heard at over the event arm: NTFS tells a folder
/// of a write only once the file's record there is brought up to date, which a flush to disk
/// (`FlushFileBuffers`) and the writer's close each do. A file whose writes must be followed
/// as they are made is watched looked at
/// (`a_looked_at_file_holds_nothing_open_and_follows_a_writer_that_keeps_it_open`).
#[test]
fn a_writer_that_keeps_the_file_open_is_heard_at_its_flush_to_disk_and_at_its_close() {
    use std::io::Write as _;
    let dir = scratch("kept-open");
    let path = dir.path().join("log.txt");
    std::fs::write(&path, "one\n").unwrap();
    let (watch, heard, counts) = counted_watch(inside(dir.path()), &path, As::Watched);
    counts.parked();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"flushed to disk\n").unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the law is what a bare FlushFileBuffers tells the folder, not a durable write"
    )]
    file.sync_all().unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the flush to disk");
    counts.parked();
    file.write_all(b"closed\n").unwrap();
    drop(file);
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the close");
    drop(watch);
}

/// The watched file's own folder renamed away is heard (the file is gone from its path), though
/// the folder's listener goes with it and hears nothing; so are the folder made again with the
/// file in it, and the next write, which only a listener on the new folder hears.
#[test]
fn the_files_folder_renamed_away_and_the_path_made_again_are_heard() {
    let dir = scratch("renamed");
    let folder = dir.path().join("a");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let (watch, heard, _counts) = counted_watch(inside(dir.path()), &path, As::Watched);

    std::fs::rename(&folder, dir.path().join("a.old")).unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the folder renamed");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(&path, "two\n").unwrap();
    assert_eq!(
        heard.recv_timeout(MUST_HAPPEN),
        Ok(1),
        "the file made again in its folder made again"
    );
    std::fs::write(&path, "three, a longer line\n").unwrap();
    assert_eq!(heard.recv_timeout(MUST_HAPPEN), Ok(1), "the next write");
    drop(watch);
}

/// Why nothing listens above the folder over a key's own, and why the arm listens only inside
/// its own folders: Windows refuses to rename a folder while a handle is open below it, so a
/// folder above a listened one cannot move unheard, nor be moved by anyone. The rename goes
/// through once the key is disarmed. Were this to start passing while armed, the arm would need
/// a listener on every folder up to the root, as the other platforms' arms have, and could
/// listen anywhere.
#[test]
fn a_folder_above_a_listened_one_cannot_be_renamed_until_the_key_is_disarmed() {
    let dir = scratch("above");
    let top = dir.path().join("a");
    let folder = top.join("b");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("doc.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut events = inside(dir.path());
    events.arm(1, &path).unwrap();
    let moved = dir.path().join("a.moved");
    assert!(
        std::fs::rename(&top, &moved).is_err(),
        "renamed above a listened folder"
    );
    events.disarm(1);
    std::fs::rename(&top, &moved).unwrap();
}

/// Two keys in one folder share its listeners, each hears only its own entry (whatever the
/// case it was named in), and a neighbour's write touches neither. A let-go listener's buffer
/// is freed once its cancelled read's completion is taken.
#[test]
fn keys_in_one_folder_share_its_listeners_and_hear_only_their_own_entries() {
    let dir = scratch("shared");
    let (one, two, other) = (
        dir.path().join("one.txt"),
        dir.path().join("two.txt"),
        dir.path().join("other.txt"),
    );
    for file in [&one, &two, &other] {
        std::fs::write(file, "initial").unwrap();
    }
    let mut events = inside(dir.path());
    events.arm(1, &dir.path().join("ONE.TXT")).unwrap();
    events.arm(2, &two).unwrap();
    assert_eq!(
        events.held().0,
        2,
        "the folder's entries and the name above"
    );
    assert_eq!(
        heard_for(&mut events, || std::fs::write(&one, "changed").unwrap()),
        [1]
    );
    // The neighbour's write is queued first and touches no key; the wait goes on to key 2's.
    assert_eq!(
        heard_for(&mut events, || {
            std::fs::write(&other, "a neighbour").unwrap();
            std::fs::write(&two, "changed").unwrap();
        }),
        [2]
    );
    // Arming again on the same paths keeps the listeners.
    events.arm(1, &one).unwrap();
    assert_eq!(events.held(), (2, 0));

    events.disarm(1);
    assert_eq!(events.held().0, 2, "key 2 still holds them");
    events.disarm(2);
    assert_eq!(events.held().0, 0);
    until_within(MUST_HAPPEN, "the cancelled reads complete", || {
        let mut touched = Vec::new();
        events.wait(Some(Duration::ZERO), &mut touched).unwrap();
        assert!(touched.is_empty(), "a let-go listener touches no key");
        (events.held().1 == 0).then_some(())
    });
}

/// A directory key hears every entry made, replaced, written and removed, and its folder
/// renamed away; armed again on the folder made again, it hears that one.
#[test]
fn a_directory_key_hears_its_entries_and_its_folder_renamed_away() {
    let dir = scratch("directory");
    let folder = dir.path().join("rows");
    std::fs::create_dir(&folder).unwrap();
    let row = folder.join("one");
    std::fs::write(&row, "initial").unwrap();
    let mut events = inside(dir.path());
    events.arm_directory(7, &folder).unwrap();
    let next = dir.path().join("next");
    std::fs::write(&next, "replacement").unwrap();
    assert_eq!(
        heard_for(&mut events, || std::fs::rename(&next, &row).unwrap()),
        [7],
        "an entry replaced by rename"
    );
    assert_eq!(
        heard_for(&mut events, || std::fs::write(folder.join("two"), "new")
            .unwrap()),
        [7],
        "an entry made"
    );
    assert_eq!(
        heard_for(&mut events, || std::fs::remove_file(&row).unwrap()),
        [7],
        "an entry removed"
    );
    assert_eq!(
        heard_for(&mut events, || std::fs::rename(
            &folder,
            dir.path().join("retired")
        )
        .unwrap()),
        [7],
        "the folder renamed away"
    );
    assert_eq!(
        events.held().0,
        1,
        "the renamed folder is let go before the key arms again"
    );
    std::fs::create_dir(&folder).unwrap();
    events.arm_directory(7, &folder).unwrap();
    assert_eq!(
        heard_for(&mut events, || std::fs::write(&row, "current").unwrap()),
        [7],
        "an entry of the folder made again"
    );
    events.disarm(7);
}

/// A listener whose folder is deleted under it touches its keys, and the key armed again
/// listens on the folder above, which hears the folder made again.
#[test]
fn a_listened_folder_deleted_touches_its_keys_and_is_heard_made_again() {
    let dir = scratch("deleted");
    let folder = dir.path().join("gone");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("doc.txt");
    let mut events = inside(dir.path());
    events.arm(3, &path).unwrap();
    assert_eq!(
        heard_for(&mut events, || std::fs::remove_dir(&folder).unwrap()),
        [3],
        "the folder deleted"
    );
    assert_eq!(
        events.held().0,
        1,
        "the deleted folder is let go before the key arms again"
    );
    events.arm(3, &path).unwrap();
    assert_eq!(
        heard_for(&mut events, || std::fs::create_dir(&folder).unwrap()),
        [3],
        "the folder made again"
    );
}

/// One `FILE_NOTIFY_INFORMATION` record naming `name`, followed by another where `more`.
fn record(name: &str, more: bool) -> Vec<u32> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut words = vec![0, 1, (units.len() * 2) as u32];
    for pair in units.chunks(2) {
        words.push(u32::from(pair[0]) | (u32::from(*pair.get(1).unwrap_or(&0)) << 16));
    }
    if more {
        words[0] = (words.len() * 4) as u32;
    }
    words
}

fn named(name: &str) -> Hear {
    Hear::Named(folded(name.encode_utf16()))
}

/// Records touch the keys they name, whatever the case; a short alias touches every named key
/// of an entries listener and none of a names listener; a directory key hears every record;
/// and a record cut short by the buffer's end touches nothing it does not name.
#[test]
fn notification_records_touch_the_keys_they_name() {
    let subs = [
        (1, named("Aterm.toml")),
        (2, named("dusk.conf")),
        (3, Hear::Any),
    ];
    let mut buffer = record("other.txt", true);
    buffer.extend(record("ATERM.TOML", false));
    let mut touched = Vec::new();
    notified(&buffer, Role::Entries, &subs, &mut touched);
    assert_eq!(touched, [3, 1, 3]);

    touched.clear();
    notified(
        &record("ATERM~1.TOM", false),
        Role::Entries,
        &subs,
        &mut touched,
    );
    assert_eq!(touched, [1, 2, 3], "a short alias may be any entry");

    touched.clear();
    notified(
        &record("ATERM~1", false),
        Role::Names,
        &subs[..2],
        &mut touched,
    );
    assert!(touched.is_empty(), "a folder's name is its long one");

    touched.clear();
    let whole = record("dusk.conf", false);
    notified(
        &whole[..whole.len() - 1],
        Role::Entries,
        &subs[..2],
        &mut touched,
    );
    assert!(touched.is_empty(), "a cut name is not the key's");

    touched.clear();
    notified(&[], Role::Entries, &subs, &mut touched);
    assert!(touched.is_empty());
}

/// The port's timeout: none waits for a completion, a wait already due polls without sleeping,
/// and a rest under a millisecond is one, never a spin at zero.
#[test]
fn a_due_wait_polls_at_once_and_a_short_one_never_spins() {
    assert_eq!(millis(None), u32::MAX);
    assert_eq!(millis(Some(Duration::ZERO)), 0);
    assert_eq!(millis(Some(Duration::from_micros(1))), 1);
    assert_eq!(millis(Some(Duration::from_millis(80))), 80);
    assert_eq!(millis(Some(Duration::from_micros(80_001))), 81);
    assert_eq!(millis(Some(Duration::MAX)), u32::MAX - 1);
}
