// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Write as _;

fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut v = b"\x1b_G".to_vec();
    v.extend_from_slice(control.as_bytes());
    v.push(b';');
    v.extend_from_slice(
        aterm_codec::base64::encode(payload)
            .expect("encode")
            .as_bytes(),
    );
    v.extend_from_slice(b"\x1b\\");
    v
}

#[test]
fn alternate_scroll_survives_a_foreground_handback() {
    let mut t = new_live_terminal(10, 40, None, None);
    t.process(b"\x1b[?25l");
    let h = t
        .foreground_handback()
        .expect("a hidden cursor is evidence");
    assert!(
        !h.reverted.contains(&"alternate-scroll"),
        "{:?}",
        h.reverted
    );
    assert!(t.modes().alternate_scroll, "DEC 1007 is still ON");
    t.process(b"\x1b[?1007l\x1b[?1049h");
    t.foreground_handback().expect("the alt screen is evidence");
    assert!(t.modes().alternate_scroll);
}

#[test]
fn live_session_archive_carries_the_process_origin() {
    let a = new_live_terminal(10, 40, None, None);
    let b = new_live_terminal(12, 50, None, None);
    let origin = alt_archive_origin();
    assert_ne!(origin, 0, "an origin of 0 is the unset value");
    assert_eq!(origin, alt_archive_origin(), "fixed for the process");
    assert_eq!(a.alt_archive().origin(), origin);
    assert_eq!(b.alt_archive().origin(), origin);
}

#[test]
fn live_session_attaches_tiered_scrollback_driven_by_config() {
    use aterm_core::config::TerminalConfig;
    let t = new_live_terminal(10, 40, None, None);
    assert!(
        t.scrollback().is_some(),
        "SCROLL-1: a live session must attach a tiered scrollback store"
    );
    let tc = TerminalConfig {
        scrollback_limit: Some(5_000),
        ..TerminalConfig::default()
    };
    let t = new_live_terminal(10, 40, Some(&tc), None);
    assert_eq!(
        t.grid().scrollback_line_limit(),
        Some(5_000),
        "SCROLL-1: scrollback_limit must drive the attached store"
    );
    let tc = TerminalConfig {
        scrollback_limit: None,
        ..TerminalConfig::default()
    };
    let t = new_live_terminal(10, 40, Some(&tc), None);
    assert!(
        t.scrollback().is_some(),
        "the store stays attached when unlimited"
    );
    assert_eq!(
        t.grid().scrollback_line_limit(),
        None,
        "SCROLL-1: unlimited (0) yields an unbounded line limit"
    );
}

#[test]
fn live_session_adopts_the_os_color_scheme_at_construction() {
    use aterm_types::Appearance;
    let mut dark = new_live_terminal(10, 40, None, None);
    dark.process(b"\x1b[?996n");
    assert_eq!(dark.take_response().as_deref(), Some(&b"\x1b[?997;1n"[..]));
    let tc = aterm_core::config::TerminalConfig {
        color_scheme: Some(Appearance::Light),
        ..aterm_core::config::TerminalConfig::default()
    };
    let mut light = new_live_terminal(10, 40, Some(&tc), None);
    assert_eq!(
        light.color_scheme(),
        Appearance::Light,
        "BROKEN-2: a tab spawned under a light palette must report light"
    );
    light.process(b"\x1b[?996n");
    assert_eq!(
        light.take_response().as_deref(),
        Some(&b"\x1b[?997;2n"[..]),
        "BROKEN-2: DSR ?996n must report the live scheme, not the dark default"
    );
}

fn term_with_resolver() -> Arc<Mutex<Terminal>> {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term_lock(&term).set_cell_pixel_size(10, 20);
    configure_kitty_file_transfer(&term, HostReach::Platform, true);
    term
}

/// A spare's taint resolver goes at adoption unless the setting and the reach admit the real
/// one, so a `t=f` query answers medium-disabled as a fresh tab's does.
#[test]
fn an_adopted_spares_taint_resolver_goes_unless_the_medium_is_allowed() {
    for (reach, allow) in [
        (HostReach::Platform, false),
        (HostReach::Refused, true),
        (HostReach::Refused, false),
    ] {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).set_kitty_file_resolver(|_, _| None);
        configure_kitty_file_transfer(&term, reach, allow);
        let mut t = term_lock(&term);
        assert!(
            !t.kitty_file_resolver_installed(),
            "{reach:?} allow={allow}"
        );
        t.process(&apc("a=q,i=1,t=f", b"/etc/hosts"));
        let reply = String::from_utf8_lossy(&t.take_response().unwrap_or_default()).into_owned();
        assert!(
            reply.contains("ENOTSUPPORTED"),
            "{reach:?} allow={allow}: {reply:?}"
        );
    }
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    configure_kitty_file_transfer(&term, HostReach::Platform, true);
    assert!(term_lock(&term).kitty_file_resolver_installed());
}

#[test]
fn a_file_medium_is_read_before_the_terminal_lock() {
    let scratch = scratch_in(&std::env::temp_dir(), "lock");
    let path = cell_file(&scratch.0, "img.rgba");
    let seq = apc(
        "a=T,f=32,s=10,v=20,i=7,t=f",
        path.to_str().unwrap().as_bytes(),
    );

    let term = term_with_resolver();
    term_lock(&term).process(&seq);
    let in_the_hold =
        String::from_utf8_lossy(&term_lock(&term).take_response().unwrap_or_default()).into_owned();
    assert!(
        in_the_hold.contains("EINVAL") && term_lock(&term).cell_frame(24, 80).images[0].is_empty(),
        "the file was read under the terminal lock: {in_the_hold:?}"
    );

    let term = term_with_resolver();
    crate::host_io_prefetch::feed_as_reader(&term, &seq);
    let reply =
        String::from_utf8_lossy(&term_lock(&term).take_response().unwrap_or_default()).into_owned();
    assert!(reply.ends_with(";OK\x1b\\"), "{reply:?}");
    assert!(!term_lock(&term).cell_frame(24, 80).images[0].is_empty());
}

#[test]
#[cfg(unix)]
fn the_pty_reader_reads_a_file_medium_before_its_hold() {
    struct Attached {
        session: crate::Session,
        slave: i32,
    }
    impl Drop for Attached {
        fn drop(&mut self) {
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            let _ = park_reader(&mut self.session, deadline);
            let master = std::mem::replace(&mut self.session.master, -1);
            aterm_pty::close_fd(self.slave);
            aterm_pty::close_fd(master);
        }
    }
    let scratch = scratch_in(&std::env::temp_dir(), "reader");
    let path = cell_file(&scratch.0, "img.rgba");
    let app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut session = crate::stub_session(91);
    session.master = master;
    term_lock(&session.term).set_cell_pixel_size(10, 20);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let post = Arc::new(move |wake: crate::Wake| {
        if matches!(wake, crate::Wake::Ready { .. }) {
            let _ = ready_tx.send(());
        }
        true
    });
    attach_reader_inner(
        &mut session,
        crate::WindowId(0),
        post,
        &app.session_factory,
        None,
    )
    .expect("attach the real reader");
    let rig = Attached { session, slave };
    ready_rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the reader's gather started");
    let term = Arc::clone(&rig.session.term);
    let output = |bytes: &[u8], title: &str| {
        let mut out = bytes.to_vec();
        out.extend_from_slice(format!("\x1b]2;{title}\x07").as_bytes());
        let mut off = 0;
        while off < out.len() {
            // SAFETY: a write of a live buffer to the test-owned slave.
            let n = unsafe { libc::write(rig.slave, out[off..].as_ptr().cast(), out.len() - off) };
            assert!(n > 0, "write the program's output");
            off += n as usize;
        }
        let shown = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || (term_lock(&term).title() == title).then_some(()),
        );
        assert!(shown.is_some(), "the reader never fed {title:?}");
    };
    let seq = apc(
        "a=T,f=32,s=10,v=20,i=7,t=f",
        path.to_str().unwrap().as_bytes(),
    );

    output(b"", "before");
    configure_kitty_file_transfer(&term, HostReach::Platform, true);
    output(b"", "installed");
    output(&seq, "sent");
    assert!(
        !term_lock(&term).cell_frame(24, 80).images[0].is_empty(),
        "the reader did not read the file before its hold"
    );
}

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_in(parent: &Path, tag: &str) -> Scratch {
    let dir = parent.join(format!("aterm-kft-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// A declared temporary root and a directory outside it, canonical, side by side in one
/// scratch directory: the deletion rule is judged against `temp` alone, so its cases never
/// depend on where the build or the machine's temporary directories live.
struct Roots {
    _scratch: Scratch,
    temp: std::path::PathBuf,
    outside: std::path::PathBuf,
}

impl Roots {
    fn new(tag: &str) -> Self {
        let scratch = scratch_in(&std::env::temp_dir(), tag);
        let made = |name: &str| {
            let dir = scratch.0.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::canonicalize(dir).unwrap()
        };
        let (temp, outside) = (made("tmp"), made("outside"));
        Self {
            _scratch: scratch,
            temp,
            outside,
        }
    }

    /// What the engine's consume does with a t=t file it read: reports whether the read
    /// returned bytes, then deletes the file where `temp` allows it.
    fn consume(&self, path: &Path) -> bool {
        let read = read_kitty_file(path).is_some();
        if read {
            consume_kitty_temp_file_with(path, std::slice::from_ref(&self.temp), || {});
        }
        read
    }
}

fn cell_file(dir: &Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, vec![0u8; 10 * 20 * 4]).unwrap();
    path
}

fn transmit_temp_file(path: &Path) -> bool {
    let term = term_with_resolver();
    let seq = apc("a=T,f=32,s=10,v=20,t=t", path.to_str().unwrap().as_bytes());
    crate::host_io_prefetch::feed_as_reader(&term, &seq);
    !term_lock(&term).cell_frame(24, 80).images[0].is_empty()
}

/// A T=T FILE IS DELETED ONLY WHERE THE PROTOCOL ALLOWS (kitty-file-medium): its canonical
/// path must name the marker and lie under a temporary directory. The cases run against a
/// declared root, so the verdict never depends on where the build lives (it once borrowed
/// the build's target directory as "outside", and failed wherever that was under /tmp).
#[test]
fn a_temp_file_is_deleted_only_where_the_protocol_allows() {
    const MARKER: &str = "tty-graphics-protocol";
    let roots = Roots::new("t");
    let (temp, outside) = (&roots.temp, &roots.outside);

    let plain = cell_file(temp, "plain.rgba");
    assert!(
        roots.consume(&plain),
        "a t=t file without the marker is read"
    );
    assert!(
        plain.exists(),
        "a t=t file without the marker is never deleted"
    );

    let consumed = cell_file(temp, &format!("x-{MARKER}.rgba"));
    assert!(roots.consume(&consumed), "a proper t=t file is read");
    assert!(!consumed.exists(), "a proper t=t file is deleted");

    let marked = temp.join(MARKER);
    std::fs::create_dir_all(&marked).unwrap();
    let inside = cell_file(&marked, "inside.rgba");
    assert!(
        roots.consume(&inside),
        "a file in a marked directory is read"
    );
    assert!(
        !inside.exists(),
        "a file in a marked temporary directory is deleted: the marker is in its path"
    );

    let kept = cell_file(outside, &format!("x-{MARKER}.rgba"));
    assert!(roots.consume(&kept), "the file is still read");
    assert!(
        kept.exists(),
        "a file outside the temporary directories is kept"
    );

    // Built on the marked directory's plain spelling: under a Windows verbatim (`\\?\`) path a
    // pushed `..` is resolved as it is pushed, so the path would never name the root as written.
    let plain = |path: &Path| {
        let text = path.to_string_lossy();
        std::path::PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
    };
    let mut escaped = plain(&marked);
    for _ in 0..64 {
        escaped.push("..");
    }
    escaped.extend(
        outside
            .components()
            .filter(|part| matches!(part, std::path::Component::Normal(_))),
    );
    escaped.push(format!("x-{MARKER}.rgba"));
    assert!(
        escaped.starts_with(plain(temp)) && escaped.to_string_lossy().contains(MARKER),
        "as written, the path names the marker under the temporary root"
    );
    assert!(roots.consume(&escaped), "the file is still read");
    assert!(kept.exists(), "`..` cannot carry the delete out of /tmp");

    let unmarked = cell_file(temp, "unmarked.rgba");
    let lent = marked.join("..").join("unmarked.rgba");
    assert!(roots.consume(&lent), "the file is still read");
    assert!(
        unmarked.exists(),
        "a passing marked directory lends no mark"
    );
}

/// The program's t=t file goes through the reader's prefetch and the engine, is placed, and
/// is consumed under this machine's own temporary directories: the platform's is one of
/// them, so a marked file there is deleted and an unmarked one kept.
#[test]
fn a_transmitted_temp_file_is_placed_then_consumed() {
    let scratch = scratch_in(&std::env::temp_dir(), "sent");
    let plain = cell_file(&scratch.0, "plain.rgba");
    assert!(transmit_temp_file(&plain), "an unmarked t=t file is placed");
    assert!(plain.exists(), "an unmarked t=t file is kept");
    let marked = cell_file(&scratch.0, "x-tty-graphics-protocol.rgba");
    assert!(transmit_temp_file(&marked), "a marked t=t file is placed");
    assert!(!marked.exists(), "a marked t=t file is deleted");
}

/// The machine's temporary directories are the protocol's: /tmp, /dev/shm, `$TMPDIR` and
/// the platform's own, each where it exists, each canonical.
#[test]
fn the_temporary_directories_are_the_protocols() {
    let temps = kitty_temp_dirs();
    let named = [
        Some(std::path::PathBuf::from("/tmp")),
        Some(std::path::PathBuf::from("/dev/shm")),
        std::env::var_os("TMPDIR").map(std::path::PathBuf::from),
        Some(std::env::temp_dir()),
    ];
    for dir in named.into_iter().flatten() {
        if let Ok(canonical) = std::fs::canonicalize(&dir) {
            assert!(
                temps.contains(&canonical),
                "{dir:?} is a temporary directory"
            );
        }
    }
    for temp in &temps {
        assert_eq!(
            std::fs::canonicalize(temp).ok().as_ref(),
            Some(temp),
            "{temp:?} is canonical"
        );
    }
    assert!(
        !temps.is_empty(),
        "the platform's temporary directory always exists"
    );
}

#[cfg(unix)]
#[test]
fn a_directory_swapped_after_the_judgement_moves_no_delete() {
    const MARKER: &str = "tty-graphics-protocol";
    let roots = Roots::new("swap");
    let outside = &roots.outside;
    let marked = roots.temp.join(MARKER);
    std::fs::create_dir_all(&marked).unwrap();
    let sent = cell_file(&marked, "victim.rgba");
    let victim = cell_file(outside, "victim.rgba");
    let held = roots.temp.join("held");

    let mut swapped = false;
    let read = read_kitty_file(&sent);
    consume_kitty_temp_file_with(&sent, std::slice::from_ref(&roots.temp), || {
        std::fs::rename(&marked, &held).unwrap();
        std::os::unix::fs::symlink(outside, &marked).unwrap();
        swapped = true;
    });

    assert!(read.is_some(), "the file is read");
    assert!(swapped, "the entry was judged deletable, so the swap ran");
    assert!(
        victim.exists(),
        "the unlink followed the swapped-in symlink out of the temporary directory"
    );
    assert!(
        !held.join("victim.rgba").exists(),
        "the entry judged is the entry deleted"
    );
}

fn transmit_reply(term: &Arc<Mutex<Terminal>>, medium: &str, path: &Path) -> String {
    let control = format!("a=t,f=32,s=10,v=20,i=9,t={medium}");
    crate::host_io_prefetch::feed_as_reader(
        term,
        &apc(&control, path.to_str().unwrap().as_bytes()),
    );
    let reply = term_lock(term).take_response().unwrap_or_default();
    String::from_utf8_lossy(&reply).into_owned()
}

#[test]
fn a_contained_session_reads_nothing_its_program_names() {
    use aterm_containment::ContainmentMode as Cm;
    let scratch = scratch_in(&std::env::temp_dir(), "reach");
    let marked = scratch.0.join("tty-graphics-protocol");
    std::fs::create_dir_all(&marked).unwrap();
    let missing = scratch.0.join("absent.rgba");
    for mode in [Cm::User, Cm::Safety, Cm::Containment] {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term_lock(&term).set_cell_pixel_size(10, 20);
        configure_kitty_file_transfer(&term, host_reach(mode), true);
        let file = cell_file(&scratch.0, "present.rgba");
        let temp = cell_file(&marked, "sent.rgba");
        let present = transmit_reply(&term, "f", &file);
        let absent = transmit_reply(&term, "f", &missing);
        let consumed = transmit_reply(&term, "t", &temp);
        if mode == Cm::Containment {
            for reply in [&present, &absent, &consumed] {
                assert!(
                    reply.contains("ENOTSUPPORTED"),
                    "a contained transmission reached a medium resolver: {reply:?}"
                );
            }
            assert_eq!(present, absent, "a contained program tells a file exists");
            assert!(temp.exists(), "a contained t=t deleted a file as aterm");
        } else {
            assert!(present.ends_with(";OK\x1b\\"), "{mode}: {present:?}");
            assert_ne!(present, absent, "{mode}");
            assert!(consumed.ends_with(";OK\x1b\\"), "{mode}: {consumed:?}");
            assert!(!temp.exists(), "{mode}: the t=t file is consumed");
        }
    }
}

#[test]
fn a_temp_medium_the_engine_refuses_is_not_consumed() {
    let scratch = scratch_in(&std::env::temp_dir(), "refused");
    let marked = scratch.0.join("tty-graphics-protocol");
    std::fs::create_dir_all(&marked).unwrap();
    let path = cell_file(&marked, "sent.rgba");
    let placed =
        |term: &Arc<Mutex<Terminal>>| !term_lock(term).cell_frame(24, 80).images[0].is_empty();
    let name = path.to_str().unwrap().as_bytes();
    let term = term_with_resolver();
    crate::host_io_prefetch::feed_as_reader(&term, &apc("a=T,f=32,t=t", name));
    assert!(!placed(&term), "PRECONDITION: the engine refused it");
    assert!(path.exists(), "a t=t file the engine refused was consumed");
    crate::host_io_prefetch::feed_as_reader(&term, &apc("a=T,f=32,s=10,v=20,t=t", name));
    assert!(placed(&term), "CONTROL: the sized transmission is placed");
    assert!(!path.exists(), "CONTROL: and its t=t file consumed");

    #[cfg(unix)]
    {
        let shm = format!("/aterm-kft-{}", std::process::id());
        let cname = std::ffi::CString::new(shm.clone()).unwrap();
        let exists = || {
            // SAFETY: a valid C string; the fd, when one opens, is closed.
            let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0) };
            if fd >= 0 {
                unsafe { libc::close(fd) };
            }
            fd >= 0
        };
        // SAFETY: `sysconf` takes no pointers.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        // SAFETY: a valid C string; the object is sized and closed.
        unsafe {
            let fd = libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600);
            assert!(fd >= 0, "the shared-memory object opens");
            assert_eq!(libc::ftruncate(fd, page as libc::off_t), 0);
            libc::close(fd);
        }
        let term = term_with_resolver();
        crate::host_io_prefetch::feed_as_reader(&term, &apc("a=T,f=32,t=s", shm.as_bytes()));
        let kept = exists();
        crate::host_io_prefetch::feed_as_reader(
            &term,
            &apc(
                &format!("a=T,f=32,s={},v=16,t=s", page / 64),
                shm.as_bytes(),
            ),
        );
        let taken_placed = placed(&term);
        let left = exists();
        // SAFETY: a valid C string; removing the test's own object if still there.
        unsafe { libc::shm_unlink(cname.as_ptr()) };
        assert!(kept, "a t=s object the engine refused was unlinked");
        assert!(
            taken_placed,
            "CONTROL: the sized t=s transmission is placed"
        );
        assert!(!left, "CONTROL: and its object unlinked");
    }
}

#[test]
fn file_medium_rejects_oversized_file() {
    let dir = std::env::temp_dir().join(format!("aterm-kft-big-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.rgba");
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&vec![0u8; (MAX_KITTY_MEDIUM_BYTES as usize) + 1])
        .unwrap();

    let term = term_with_resolver();
    let seq = apc("a=T,f=32,s=10,v=20,t=f", path.to_str().unwrap().as_bytes());
    crate::host_io_prefetch::feed_as_reader(&term, &seq);
    let placed = !term_lock(&term).cell_frame(24, 80).images[0].is_empty();

    let _ = std::fs::remove_dir_all(&dir);
    assert!(!placed, "an over-cap file must be rejected (fail closed)");
}
