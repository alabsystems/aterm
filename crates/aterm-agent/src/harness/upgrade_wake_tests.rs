// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A WAIT A SIGNAL INTERRUPTS WAITS AGAIN, for what is left of it, and the event that comes
/// next wakes it: an interrupted wait is no failure that sleeps the whole wait out. CONTROLS:
/// any other errno is the wait's error at once, and a call that times out is `None`.
#[test]
fn an_interrupted_kqueue_wait_waits_again_for_the_event() {
    let wait = Duration::from_secs(60);
    let mut asked: Vec<Duration> = Vec::new();
    let woke = kq_wait_by(wait, |ts, event| {
        asked.push(Duration::new(
            u64::try_from(ts.tv_sec).unwrap(),
            u32::try_from(ts.tv_nsec).unwrap(),
        ));
        if asked.len() == 1 {
            return Err(std::io::Error::from_raw_os_error(libc::EINTR));
        }
        event.filter = libc::EVFILT_VNODE;
        Ok(1)
    });
    assert_eq!(woke.unwrap(), Some(libc::EVFILT_VNODE));
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1] <= asked[0] && asked[0] <= wait, "{asked:?}");

    let mut calls = 0;
    let failed = kq_wait_by(wait, |_, _| {
        calls += 1;
        Err(std::io::Error::from_raw_os_error(libc::EBADF))
    });
    assert_eq!(
        failed.unwrap_err().raw_os_error(),
        Some(libc::EBADF),
        "CONTROL"
    );
    assert_eq!(calls, 1, "CONTROL: another errno is not waited again");
    assert_eq!(kq_wait_by(wait, |_, _| Ok(0)).unwrap(), None, "CONTROL");
}

const HANG: Duration = Duration::from_secs(60);

const FALLBACK: Duration = Duration::from_secs(120);

fn fixed(path: PathBuf) -> Box<dyn FnMut() -> Option<PathBuf> + Send> {
    Box::new(move || Some(path.clone()))
}

fn scratch(prefix: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap()
}

fn wait_parked(parks: &std::sync::atomic::AtomicUsize, at_least: usize) {
    let now = Instant::now();
    let deadline = now
        .checked_add(HANG)
        .unwrap_or_else(|| now + Duration::from_secs(60));
    while parks.load(std::sync::atomic::Ordering::SeqCst) < at_least {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {at_least} park(s)"
        );
        aterm_uds::deadline::poll_yield();
    }
}

#[test]
fn a_managed_claude_twin_move_is_seen_without_its_notice() {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-claude-twin-wake-")
        .tempdir()
        .unwrap();
    let layout = atpkg::store::Layout {
        prefix: root.path().join("pkg"),
    };
    let twin = twin_under(&layout, "claude").unwrap();
    let notice = atpkg::activation_notice::marker_path(&layout);
    std::fs::create_dir_all(layout.agents_dir()).unwrap();
    let old = root.path().join("old");
    let new = root.path().join("new");
    std::fs::write(&old, b"old captured build").unwrap();
    std::fs::write(&new, b"new captured build").unwrap();
    std::os::unix::fs::symlink(&old, &twin).unwrap();
    let mut wake =
        ActivationWake::with_claude_sources(None, fixed(notice.clone()), fixed(twin.clone()));
    wake.deaf = true;
    assert!(
        !wake.wait(Duration::ZERO),
        "the installed build is the baseline"
    );
    std::fs::write(layout.agents_dir().join("unrelated"), b"x").unwrap();
    assert!(!wake.wait(Duration::ZERO), "a neighbour is no activation");
    let replacement = layout.agents_dir().join("next");
    std::os::unix::fs::symlink(&new, &replacement).unwrap();
    std::fs::rename(&replacement, &twin).unwrap();
    assert!(!notice.exists(), "no activation notice was published");
    assert!(wake.wait(Duration::ZERO), "the managed entrypoint moved");
    assert!(!wake.wait(Duration::ZERO), "each change is told once");

    std::fs::write(&replacement, b"captured regular twin").unwrap();
    std::fs::rename(&replacement, &twin).unwrap();
    assert!(
        wake.wait(Duration::ZERO),
        "a regular twin replacement is news"
    );
    assert!(!wake.wait(Duration::ZERO));
}

#[test]
fn marker_wakes_a_parked_host_and_unrelated_writes_do_not() {
    let dir = scratch("aterm-claude-upgrade-wake-");
    let root = dir.path();
    let marker = root
        .join("pkg")
        .join(atpkg::activation_notice::NOTICE_DIR)
        .join(atpkg::activation_notice::CLAUDE_MARKER);
    let mut wake = ActivationWake::with_marks(vec![Mark::new(fixed(marker.clone()))]);
    std::fs::write(root.join("unrelated"), b"x").unwrap();
    assert!(
        !wake.wait(Duration::ZERO),
        "an unrelated parent write is not a Claude upgrade"
    );
    std::fs::create_dir(root.join("pkg")).unwrap();
    assert!(!wake.wait(Duration::ZERO));
    std::fs::create_dir(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR)).unwrap();
    assert_eq!(
        watch_dir(&marker),
        Some(root.join("pkg").join(atpkg::activation_notice::NOTICE_DIR))
    );
    std::fs::write(root.join("pkg/progress.json"), b"busy").unwrap();
    assert!(
        !wake.wait(Duration::ZERO),
        "an active download's progress writes wake nobody"
    );
    let parks = std::sync::Arc::clone(&wake.parks);
    let parks_before = parks.load(std::sync::atomic::Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let woke = wake.wait(FALLBACK);
        tx.send((woke, wake.last_event)).unwrap();
    });
    wait_parked(&parks, parks_before + 1);
    let tmp = root
        .join("pkg")
        .join(atpkg::activation_notice::NOTICE_DIR)
        .join(".marker.tmp");
    std::fs::write(&tmp, b"1\n").unwrap();
    std::fs::rename(&tmp, &marker).unwrap();
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some((true, Some(libc::EVFILT_VNODE))),
        "the marker replacement wakes the parked host on EVFILT_VNODE, and says so"
    );
    waiter.join().unwrap();
    let mut quiet = ActivationWake::with_marks(vec![Mark::new(fixed(marker))]);
    assert!(!quiet.wait(Duration::ZERO));
}

#[test]
fn the_owners_word_wakes_a_parked_host() {
    let scratch_dir = scratch("aterm-claude-upgrade-word-");
    let root = scratch_dir.path();
    let marker = super::super::upgrade_drive::word_marker(root);
    let mut wake = ActivationWake::with_marks(Vec::new()).with_word(Some(marker.clone()));
    let dir = marker.parent().expect("a directory").to_path_buf();
    assert!(dir.is_dir(), "made up front");
    assert_eq!(watch_dir(&marker), Some(dir.clone()));
    std::fs::write(dir.join("unrelated"), b"x").unwrap();
    assert!(
        !wake.wait(Duration::ZERO),
        "a write beside the marker is no word"
    );
    let parks = std::sync::Arc::clone(&wake.parks);
    let parks_before = parks.load(std::sync::atomic::Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let woke = wake.wait(FALLBACK);
        tx.send((woke, wake.last_event)).unwrap();
    });
    wait_parked(&parks, parks_before + 1);
    let tmp = marker.with_extension("tmp");
    std::fs::write(&tmp, b"1\n").unwrap();
    std::fs::rename(&tmp, &marker).unwrap();
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some((true, Some(libc::EVFILT_VNODE))),
        "the word wakes the parked host on EVFILT_VNODE"
    );
    waiter.join().unwrap();
}

#[test]
fn a_side_mark_tells_its_listener_and_ends_no_wait() {
    let scratch_dir = scratch("aterm-accounts-side-wake-");
    let root = scratch_dir.path();
    let word = root.join("word");
    let store = root.join("accounts");
    let marker = store.join("wake");
    let replace = |path: &Path| {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, b"").unwrap();
        std::fs::rename(&tmp, path).unwrap();
    };
    let told = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = std::sync::Arc::clone(&told);
    let heard = move || told.load(std::sync::atomic::Ordering::SeqCst);
    let mut wake = ActivationWake::with_marks(vec![Mark::new(fixed(word.clone()))]).with_side(
        Some(marker.clone()),
        Box::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    assert!(!store.exists(), "the store's directory is not made for it");
    let now = Duration::ZERO;
    assert!(!wake.wait(now));
    assert_eq!(heard(), 0);
    std::fs::create_dir_all(&store).unwrap();
    replace(&marker);
    assert!(!wake.wait(now), "a side mark ends no wait");
    assert_eq!(heard(), 1);
    replace(&marker);
    assert!(!wake.wait(now));
    assert_eq!(heard(), 2);
    std::fs::write(store.join("state.json"), b"{}").unwrap();
    assert!(!wake.wait(now));
    assert_eq!(heard(), 2, "a write beside the marker is not it");
    replace(&word);
    assert!(wake.wait(now), "the word is a wake");
    assert_eq!(heard(), 2, "and no news of the side mark");

    let _ = std::fs::remove_dir_all(&store);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut parked = ActivationWake::with_marks(vec![Mark::new(fixed(word.clone()))]).with_side(
        Some(marker.clone()),
        Box::new(move || {
            let _ = tx.send(());
        }),
    );
    let parks = std::sync::Arc::clone(&parked.parks);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = done_tx.send(parked.wait(FALLBACK));
    });
    wait_parked(&parks, 1);
    std::fs::create_dir_all(&store).unwrap();
    replace(&marker);
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some(()),
        "the marker made under a directory made after the wait parked is told"
    );
    replace(&word);
    assert_eq!(
        done_rx.recv_timeout(HANG).ok(),
        Some(true),
        "the side mark ended no wait: the word did"
    );
    waiter.join().unwrap();
}

#[test]
fn an_agents_own_marker_tells_its_listener_and_wakes_no_other_agent() {
    let scratch_dir = scratch("aterm-agent-marker-wake-");
    let root = scratch_dir.path();
    let notices = root.join(atpkg::activation_notice::NOTICE_DIR);
    std::fs::create_dir_all(&notices).unwrap();
    let first = notices.join(atpkg::activation_notice::CLAUDE_MARKER);
    let own = notices.join(atpkg::activation_notice::marker_name("muse"));
    assert_ne!(first, own, "Muse Code's marker is its own");
    let replace = |path: &Path| {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, b"1\n").unwrap();
        std::fs::rename(&tmp, path).unwrap();
    };
    let told = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = std::sync::Arc::clone(&told);
    let heard = move || told.load(std::sync::atomic::Ordering::SeqCst);
    let mut wake = ActivationWake::with_marks(vec![Mark::new(fixed(first.clone()))])
        .with_agent_mark(
            fixed(own.clone()),
            Box::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
    let now = Duration::ZERO;
    assert!(!wake.wait(now));
    assert_eq!(heard(), 0, "nothing changed");
    replace(&own);
    assert!(
        !wake.wait(now),
        "a Muse landing wakes no other agent's worker"
    );
    assert_eq!(heard(), 1, "Muse Code's workers are told");
    replace(&own);
    assert!(!wake.wait(now));
    assert_eq!(heard(), 2);
    std::fs::write(notices.join("progress.json"), b"{}").unwrap();
    assert!(!wake.wait(now));
    assert_eq!(heard(), 2, "a write beside the markers is neither");
    replace(&first);
    assert!(wake.wait(now), "the first marker is every worker's wake");
    assert_eq!(heard(), 2, "and no news of Muse Code's own");
}

#[test]
fn a_repointed_native_link_wakes_the_host_and_its_neighbours_do_not() {
    let scratch_dir = scratch("aterm-native-wake-");
    let home = scratch_dir.path();
    let versions = home.join(".local/share/claude/versions");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&versions).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(versions.join("2.1.281"), b"old").unwrap();
    let link = bin.join("claude");
    std::os::unix::fs::symlink(versions.join("2.1.281"), &link).unwrap();
    let mut wake = ActivationWake::new(Some(home));
    wake.marks
        .retain(|m| m.path.as_deref() == Some(link.as_path()));
    std::fs::write(versions.join("2.1.282"), b"new").unwrap();
    std::fs::write(bin.join("other-tool"), b"x").unwrap();
    assert!(
        !wake.wait(Duration::ZERO),
        "a download and a neighbour are not the new build in place"
    );
    let parks = std::sync::Arc::clone(&wake.parks);
    let parks_before = parks.load(std::sync::atomic::Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let woke = wake.wait(FALLBACK);
        tx.send((woke, wake.last_event)).unwrap();
    });
    wait_parked(&parks, parks_before + 1);
    let next = bin.join(".claude.next");
    std::os::unix::fs::symlink(versions.join("2.1.282"), &next).unwrap();
    std::fs::rename(&next, &link).unwrap();
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some((true, Some(libc::EVFILT_VNODE))),
        "the repointed link wakes the host on EVFILT_VNODE"
    );
    waiter.join().unwrap();
}

#[test]
fn the_trigger_ends_a_parked_wait_at_once() {
    let mut wake = ActivationWake::with_marks(Vec::new());
    let trigger = wake.trigger();
    let parks = std::sync::Arc::clone(&wake.parks);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let woke = wake.wait(FALLBACK);
        tx.send((woke, wake.last_event)).unwrap();
    });
    wait_parked(&parks, 1);
    trigger.pull();
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some((false, Some(libc::EVFILT_USER))),
        "the pulled trigger wakes kq_wait via EVFILT_USER, reporting no activation"
    );
    waiter.join().unwrap();
}

#[test]
fn first_install_wakes_even_when_the_package_parent_did_not_exist() {
    let scratch_dir = scratch("aterm-claude-upgrade-first-install-");
    let root = scratch_dir.path();
    let marker = root
        .join("new-parent")
        .join("pkg")
        .join(atpkg::activation_notice::NOTICE_DIR)
        .join(atpkg::activation_notice::CLAUDE_MARKER);
    assert_eq!(watch_dir(&marker), Some(root.to_path_buf()));
    let mut wake = ActivationWake::with_marks(vec![
        Mark::new(fixed(marker.clone())).rechecked_before_its_dir(),
    ]);
    let parks = std::sync::Arc::clone(&wake.parks);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        tx.send(wake.wait(FALLBACK)).unwrap();
    });
    wait_parked(&parks, 1);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, b"1\n").unwrap();
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some(true),
        "the new marker is observed once its ancestor tree appears"
    );
    waiter.join().unwrap();
}

#[test]
fn first_install_rechecks_a_missing_path_after_a_lost_ancestor_notification() {
    let scratch_dir = scratch("aterm-claude-upgrade-lost-ancestor-");
    let root = scratch_dir.path();
    let marker = root
        .join("new-parent")
        .join("pkg")
        .join(atpkg::activation_notice::NOTICE_DIR)
        .join(atpkg::activation_notice::CLAUDE_MARKER);
    let mut wake = ActivationWake::with_marks(vec![
        Mark::new(fixed(marker.clone())).rechecked_before_its_dir(),
    ]);
    wake.deaf = true;
    let parks = std::sync::Arc::clone(&wake.parks);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        tx.send((wake.wait(FALLBACK), wake.last_event)).unwrap();
    });
    wait_parked(&parks, 1);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, b"1\n").unwrap();
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some((true, None)),
        "ancestor recheck observes the new marker while deaf (without kqueue events)"
    );
    waiter.join().unwrap();
}

#[test]
fn an_exit_is_seen_as_it_happens() {
    let mut child = aterm_uds::spawnfd::command("/bin/cat")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let pid = child.id();
    assert_eq!(
        wait_exit_inner(pid, None, Duration::ZERO, None),
        (false, None)
    );
    assert!(!wait_exit(pid, Duration::ZERO));

    let parks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let parks_waiter = std::sync::Arc::clone(&parks);
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        tx.send(wait_exit_inner(pid, None, FALLBACK, Some(&parks_waiter)))
            .unwrap();
    });
    wait_parked(&parks, 1);
    drop(stdin);
    child.wait().unwrap();
    assert_eq!(
        rx.recv_timeout(HANG).ok(),
        Some((true, Some(libc::EVFILT_PROC))),
        "EVFILT_PROC reports the exit to kq_wait"
    );
    waiter.join().unwrap();
    assert!(wait_exit(pid, FALLBACK), "already gone");
}

#[test]
fn a_zombie_has_exited() {
    let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
        .spawn()
        .unwrap();
    let pid = child.id();
    loop {
        // SAFETY: a zeroed out-parameter `waitid` fills, for our own child.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above; `WNOWAIT` leaves the child to `child.wait`.
        let rc =
            unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        let err = std::io::Error::last_os_error();
        if rc == 0 {
            break;
        }
        assert_eq!(err.raw_os_error(), Some(libc::EINTR), "waitid: {err}");
    }
    assert_eq!(
        // SAFETY: signal 0 queries existence without delivering a signal.
        unsafe { libc::kill(pid as libc::pid_t, 0) },
        0,
        "kill(pid, 0) still finds a zombie"
    );
    assert!(
        !super::super::upgrade_drive::alive(pid),
        "an unreaped zombie is not live"
    );
    let parks = std::sync::atomic::AtomicUsize::new(0);
    assert_eq!(
        wait_exit_inner(pid, None, FALLBACK, Some(&parks)),
        (true, None),
        "a zombie returns immediately as exited without parking"
    );
    assert_eq!(parks.load(std::sync::atomic::Ordering::SeqCst), 0);
    child.wait().unwrap();
}

#[test]
fn an_existing_prefix_gets_its_notice_dir_and_no_recheck() {
    let scratch_dir = scratch("aterm-wake-prepare-");
    let root = scratch_dir.path();
    let prefix = root.join("pkg");
    std::fs::create_dir(&prefix).expect("prefix");
    let layout = atpkg::store::Layout {
        prefix: prefix.clone(),
    };
    let marker = marker_under(&layout, "claude");
    assert!(marker.parent().is_some_and(Path::is_dir), "{marker:?}");
    let kq = kqueue().expect("kqueue");
    let mut mark = Mark::new(Box::new(move || Some(marker.clone()))).rechecked_before_its_dir();
    mark.ensure_watch(&kq);
    assert!(
        !mark.watching_an_ancestor(),
        "the watch is on the dir itself"
    );
    let absent = atpkg::store::Layout {
        prefix: root.join("none"),
    };
    let marker = marker_under(&absent, "claude");
    assert!(!root.join("none").exists());
    let mut mark = Mark::new(Box::new(move || Some(marker.clone()))).rechecked_before_its_dir();
    mark.ensure_watch(&kq);
    assert!(mark.watching_an_ancestor());
}
