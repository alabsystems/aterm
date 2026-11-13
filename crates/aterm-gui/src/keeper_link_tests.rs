// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_tag_round_trips_and_refuses_what_it_cannot_trust() {
    let tag = Tag {
        sid: Some("s-00ff".into()),
        owner: Some(4242),
        rekey: true,
        loader: false,
        frozen: true,
        handed: Some((4141, 7)),
        keyed: Some((4040, 3)),
        slog_ep: Some(1_792_000_000),
    };
    assert_eq!(Tag::decode(&tag.encode()), Some(tag));
    assert_eq!(Tag::decode(b"v=2\nsid=s-00\n"), None, "another version");
    let hostile = Tag::decode(
        b"v=1\nsid=../../x\nidentity=a b\nowner=zz\nhanded=1\nkeyed=0:1\nnew=1\nslog_ep=zz\n",
    )
    .expect("v1");
    assert_eq!(
        hostile,
        Tag::default(),
        "a sid, owner or parent that is not one is dropped; unknown keys ignored"
    );
    let retired = Tag::decode(b"v=1\nsid=s-00ff\nidentity=worker\n").expect("v1");
    assert_eq!(retired.sid.as_deref(), Some("s-00ff"));
    assert!(!retired.encode().windows(9).any(|w| w == b"identity="));
    assert!(tag_under_the_wire_bound());
    let older = Tag::decode(b"v=1\nsid=s-00ff\nowner=7\n").expect("v1");
    assert_eq!(older.slog_ep, None);
    assert_eq!(Tag::decode(b"v=1\nslog_ep=0\n").unwrap().slog_ep, None);
    assert_eq!(Tag::decode(b"v=1\nslog_ep=-3\n").unwrap().slog_ep, None);
}

fn tag_under_the_wire_bound() -> bool {
    let long = Tag {
        sid: Some(format!("s-{}", "f".repeat(62))),
        owner: Some(u32::MAX),
        rekey: true,
        loader: true,
        frozen: true,
        handed: Some((u32::MAX, u64::MAX)),
        keyed: Some((u32::MAX, u64::MAX)),
        slog_ep: Some(u32::MAX),
    };
    let bytes = long.encode();
    Tag::decode(&bytes).is_some_and(|t| t == long) && bytes.len() <= aterm_keeper::wire::MAX_TAG
}

#[test]
fn a_headless_launch_of_the_installed_app_takes_no_part() {
    assert!(!takes_part(true, true));
    assert!(takes_part(false, true));
    assert!(takes_part(true, false));
    assert!(takes_part(false, false));
}

#[test]
fn with_the_keeper_off_nothing_registers() {
    let recovery = boot(Some(false), false, false, None);
    assert!(recovery.adopted.is_empty());
    assert!(!on(), "boot(false) leaves the link off");
    assert!(
        pending_before_grant(1),
        "an update never waits on an absent keeper"
    );
    release(7);
    bye(&[]);
    #[cfg(unix)]
    {
        assert!(keeper_state().tracked.is_empty(), "nothing tracked");
        assert!(matches!(keeper_state().phase, Phase::Off));
        assert_eq!(recovery.keeper, KeeperAtLaunch::Off, "the census says off");
    }
}

/// The fingerprint masks the per-frame modes: a TUI's DEC 2026 and `?25` toggles send no
/// META, while a real mode change does.
#[cfg(unix)]
#[test]
fn the_fingerprint_ignores_the_per_frame_modes() {
    let mut t = Terminal::new(24, 80);
    let base = Fingerprint::of(&t);
    t.process(b"\x1b[?2026h\x1b[?25l");
    assert_eq!(
        Fingerprint::of(&t),
        base,
        "sync output and cursor visibility are masked"
    );
    t.process(b"\x1b[?2026l\x1b[?25h\x1b[?2004h");
    assert_ne!(Fingerprint::of(&t), base, "bracketed paste is a real mode");
    let bracketed = Fingerprint::of(&t);
    t.process(b"\x1b[>1u");
    assert_ne!(
        Fingerprint::of(&t),
        bracketed,
        "a kitty keyboard push is a real change"
    );
}

#[test]
fn the_keeper_is_on_unless_the_file_turns_it_off() {
    let parse = |text: &str| crate::app_config::Config::parse(text).expect("parses");
    assert_eq!(
        parse("").keeper_enabled(),
        DEFAULT_ON,
        "absent is the default"
    );
    assert_eq!(parse("[keeper]\n").keeper_enabled(), DEFAULT_ON);
    assert!(!parse("[keeper]\nenabled = false\n").keeper_enabled());
    assert!(parse("[keeper]\nenabled = true\n").keeper_enabled());
}

#[test]
fn a_bundled_window_starts_its_keeper_and_a_check_keeps_to_its_file() {
    // (setting, l(headless, installed, lasting bundle))
    let l = |headless, installed, lasting_bundle| Launch {
        headless,
        installed,
        lasting_bundle,
    };
    if DEFAULT_ON {
        assert_eq!(
            part(None, l(false, true, true)),
            Part::LinkAndStart,
            "a new Mac"
        );
        assert_eq!(
            part(None, l(false, false, true)),
            Part::LinkAndStart,
            "a dev app"
        );
        assert_eq!(part(Some(true), l(false, true, true)), Part::LinkAndStart);
    }
    for (headless, installed, bundled) in [
        (false, true, true),
        (false, false, true),
        (false, false, false),
        (true, false, false),
        (true, true, true),
    ] {
        assert_eq!(
            part(Some(false), l(headless, installed, bundled)),
            Part::None,
            "its off"
        );
    }
    assert_eq!(
        part(None, l(false, false, false)),
        Part::None,
        "a bare build's window"
    );
    assert_eq!(
        part(None, l(true, false, false)),
        Part::None,
        "a lab instance"
    );
    assert_eq!(
        part(None, l(true, false, true)),
        Part::None,
        "a dev app, headless"
    );
    assert_eq!(part(Some(true), l(false, false, false)), Part::Link);
    assert_eq!(
        part(Some(true), l(true, false, false)),
        Part::Link,
        "the live lanes"
    );
    assert_eq!(
        part(None, l(true, true, true)),
        Part::None,
        "the installed app, headless"
    );
    assert_eq!(
        part(Some(true), l(true, true, true)),
        Part::None,
        "whatever the file says"
    );
}

/// [`Launch::of`] reads each fact off the path [`boot`] passes: a copy on a mounted image or a
/// translocated one is no lasting bundle, so its window starts no keeper.
#[cfg(target_os = "macos")]
#[test]
fn a_window_of_a_copy_that_goes_away_starts_no_keeper() {
    use std::path::Path;
    let installed = Launch::of(Path::new(aterm_keeper::job::INSTALLED_EXECUTABLE), false);
    assert_eq!(
        installed,
        Launch {
            headless: false,
            installed: true,
            lasting_bundle: true
        }
    );
    assert_eq!(part(None, installed), Part::LinkAndStart, "a new Mac");
    for (exe, why) in [
        (
            "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
            "opened inside the disk image",
        ),
        (
            "/private/var/folders/x/T/AppTranslocation/0A1B/d/aterm.app/Contents/MacOS/aterm",
            "translocated from Downloads",
        ),
        ("/Users//x/aterm/target/debug/aterm", "a bare build"),
    ] {
        let launch = Launch::of(Path::new(exe), false);
        assert!(!launch.lasting_bundle, "{why}");
        assert!(!launch.installed, "{why}");
        assert_eq!(part(None, launch), Part::None, "{why}");
    }
    let dev_app = Launch::of(
        Path::new("/Users//x/Applications/aterm (dev).app/Contents/MacOS/aterm"),
        false,
    );
    assert!(dev_app.lasting_bundle && !dev_app.installed, "{dev_app:?}");
    let headless = Launch::of(Path::new(aterm_keeper::job::INSTALLED_EXECUTABLE), true);
    assert_eq!(part(Some(true), headless), Part::None);
    assert_eq!(
        part(
            None,
            Launch::of(Path::new("/Users//x/aterm/target/debug/aterm"), true).as_window()
        ),
        Part::LinkAndStart,
        "the live test's seam stands in for a lasting bundle's window"
    );
}

#[test]
fn only_an_admitted_overlap_pair_or_an_adopted_handoff_arms_a_candidate() {
    assert!(
        !is_update_candidate(false, false),
        "a degraded or plain launch"
    );
    assert!(is_update_candidate(true, false), "an admitted pair");
    assert!(is_update_candidate(false, true), "an adopted handoff");
    assert!(is_update_candidate(true, true));
}

/// A real PTY whose shell leads it: `/bin/sh` parked in a `sleep`, and
/// its recorded birth once `record_adopted_shell` proves it leads the
/// terminal (it has run `setsid`).
#[cfg(unix)]
fn live_pty() -> (aterm_pty::SpawnedShell, u64) {
    // SAFETY: A test process; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()];
    let sh = aterm_pty::spawn_shell_with_pid_cell_px(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&exec),
        None,
        None,
        aterm_sandbox::Limits::inherit(),
        None,
        None,
    )
    .expect("spawn");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(birth) = aterm_pty::record_adopted_shell(sh.pid, sh.master).birth {
            return (sh, birth);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the shell never led its pty"
        );
        aterm_uds::deadline::poll_yield();
    }
}

#[cfg(unix)]
fn dup_of(fd: i32) -> crate::spawn::HandedMaster {
    use std::os::fd::{BorrowedFd, IntoRawFd as _};
    // SAFETY: `fd` is a live descriptor the test holds for this call.
    let dup = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .expect("dup");
    crate::spawn::HandedMaster::new(dup.into_raw_fd())
}

#[cfg(unix)]
fn header_for(pid: i32, birth: u64) -> MasterHeader {
    MasterHeader {
        rdev: 1,
        shell_pid: u32::try_from(pid).expect("pid"),
        shell_birth: birth_of(birth),
        local_id: 3,
    }
}

#[cfg(unix)]
fn tag() -> Vec<u8> {
    Tag {
        sid: Some("s-0123456789abcdef0123".into()),
        owner: Some(1),
        rekey: true,
        loader: false,
        frozen: false,
        handed: None,
        keyed: None,
        slog_ep: Some(4),
    }
    .encode()
}

#[cfg(unix)]
fn end(sh: &aterm_pty::SpawnedShell) {
    let identity = aterm_pty::record_spawned_shell(sh.pid);
    let _ = aterm_pty::hangup_shell(&identity, sh.master);
    aterm_pty::reap_shell(identity);
    aterm_pty::close_fd(sh.master);
}

/// The one admission: a master that is not a tty, a live shell's tty, and a shell gone or
/// whose pid names another process.
#[cfg(unix)]
#[test]
fn the_admission_takes_a_live_shell_and_refuses_the_rest() {
    let mut used = 0;
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?2004h");
    let meta = crate::seamless::keeper_meta(&t).expect("meta");

    let null = std::fs::File::open("/dev/null").expect("null");
    use std::os::fd::AsRawFd as _;
    let refused = admit_recovered(
        dup_of(null.as_raw_fd()),
        header_for(i32::try_from(std::process::id()).expect("pid"), 0),
        &tag(),
        &meta,
        &mut used,
    );
    assert_eq!(refused.err(), Some("was handed a master that is not a tty"));

    let (sh, birth) = live_pty();
    let adopted = admit_recovered(
        dup_of(sh.master),
        header_for(sh.pid, birth),
        &tag(),
        &meta,
        &mut used,
    )
    .expect("a live shell is admitted");
    assert_eq!(adopted.pid, sh.pid);
    assert_eq!(adopted.local_id, 3);
    assert_eq!(adopted.sid.as_str(), "s-0123456789abcdef0123");
    assert!(adopted.repaint && adopted.rekey);
    assert_eq!(
        adopted.log,
        crate::session_log::LogAdopt {
            keeper_ep: Some(4),
            mirror: None,
            ..Default::default()
        }
    );
    assert_eq!(
        adopted
            .keeper
            .as_ref()
            .map(|claim| (claim.rdev(), claim.owner())),
        Some((1, Some(1))),
        "an admitted offer carries its release obligation and its window"
    );
    assert!(
        adopted
            .checkpoint
            .as_ref()
            .is_some_and(|cp| cp.modes.bracketed_paste),
        "the META's modes survive"
    );
    assert_eq!((adopted.holds.len(), adopted.topics.len()), (0, 0));
    assert_eq!(adopted.fabric.dropped, 0);
    assert!(
        adopted.fabric.last_post_id >= 1 << 32,
        "{:?}",
        adopted.fabric
    );
    drop(adopted);

    let fabric = crate::fabric::KeeperCarry {
        hold: Some("fleet human%20stop".to_string()),
        more_holds: vec!["local mine".to_string()],
        ids: [8192, 12288, 8192],
        topics: vec!["builds @91234".to_string()],
        queued: vec![4],
        owed: 2,
        dropped: 9,
        ..crate::fabric::KeeperCarry::default()
    };
    let carried = crate::seamless::keeper_meta_of(t.carry_meta_abandoning_partial(), Some(&fabric))
        .expect("meta");
    let adopted = admit_recovered(
        dup_of(sh.master),
        header_for(sh.pid, birth),
        &tag(),
        &carried,
        &mut used,
    )
    .expect("a live shell is admitted");
    assert_eq!(adopted.holds, ["fleet human%20stop", "local mine"]);
    assert_eq!(adopted.topics, ["builds @91234"]);
    assert_eq!(
        (
            adopted.fabric.last_msg_id,
            adopted.fabric.last_post_id,
            adopted.fabric.last_receipt_id,
            adopted.fabric.uncarried.as_slice(),
            adopted.fabric.receipts_uncarried,
            adopted.fabric.dropped,
        ),
        (8192, 12288, 8192, &[4][..], 2, 9)
    );
    assert!(
        adopted
            .checkpoint
            .as_ref()
            .is_some_and(|cp| cp.modes.bracketed_paste),
        "the screen beside it still reads"
    );
    drop(adopted);

    let refused = admit_recovered(
        dup_of(sh.master),
        header_for(sh.pid, birth + 1),
        &tag(),
        &meta,
        &mut used,
    );
    assert_eq!(
        refused.err(),
        Some("its shell's pid now names another process")
    );

    let older = Tag {
        slog_ep: None,
        ..Tag::decode(&tag()).expect("the tag reads")
    }
    .encode();
    assert!(
        !String::from_utf8_lossy(&older).contains("slog_ep"),
        "PRECONDITION: the key is not written"
    );
    let adopted = admit_recovered(
        dup_of(sh.master),
        header_for(sh.pid, birth),
        &older,
        &meta,
        &mut used,
    )
    .expect("a live shell is admitted");
    assert_eq!(adopted.log.keeper_ep, None);
    drop(adopted);

    let refused = admit_recovered(
        dup_of(sh.master),
        header_for(sh.pid, birth),
        b"v=1\n",
        &meta,
        &mut used,
    );
    assert_eq!(refused.err(), Some("carries no session id"));

    let master = dup_of(sh.master);
    end(&sh);
    let refused = admit_recovered(master, header_for(sh.pid, birth), &tag(), &meta, &mut used);
    assert_eq!(
        refused.err(),
        Some("its shell is gone or no longer leads its terminal")
    );
}

/// The census's row of a recovery: none when nothing was offered; the refused offers
/// counted beside the admitted ones, a recovery that refused every offer included.
#[cfg(unix)]
#[test]
fn a_recovery_s_row_counts_the_offers_it_refused() {
    let mut recovery = Recovery::default();
    assert_eq!(recovery.ledger(), None, "nothing offered, no restores");
    recovery.refused.push((3, "carries no session id"));
    let all_refused = recovery.ledger().expect("a refusal is a row");
    assert_eq!((all_refused.recovered(), all_refused.refused), (0, 1));
    recovery.shells.push(4242);
    recovery.restores.counts.exact = 1;
    let some = recovery.ledger().expect("an adopted session is a row");
    assert_eq!((some.exact, some.refused), (1, 1));
}

/// META round-trips through the keeper's bound onto the Repaint rung, the
/// modes intact and no nonce taken.
#[cfg(unix)]
#[test]
fn a_meta_lands_on_the_repaint_rung_with_its_modes() {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?2004h\x1b[?1049h\x1b[>1u");
    let meta = crate::seamless::keeper_meta(&t).expect("fits");
    assert!(meta.len() <= aterm_keeper::wire::MAX_META);
    let text = String::from_utf8(meta.clone()).expect("utf-8");
    assert!(!text.contains("\"shell_integration_nonce\""), "{text}");
    let mut used = 0;
    let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
    assert!(cp.modes.bracketed_paste, "bracketed paste survives");
    assert!(cp.modes.alternate_screen, "the alt screen survives");
    assert!(cp.shell_integration_nonce.is_none());
    let recovered = Terminal::from_checkpoint(&cp);
    assert_eq!(recovered.kitty_keyboard_flags(), t.kitty_keyboard_flags());
    assert!(
        crate::seamless::recovered_checkpoint(b"{}", &mut used).is_none(),
        "a meta without the handoff's keys is none"
    );
    assert!(crate::seamless::recovered_checkpoint(b"", &mut used).is_none());
}

/// The META carries no in-flight output state (`keeper_meta_of` says why), and
/// `recovered_checkpoint` takes none, nor a partial sequence, from a META that holds it.
#[cfg(unix)]
#[test]
fn the_meta_carries_no_in_flight_output_state() {
    let mut t = Terminal::new(30, 100);
    t.process(b"\x1b[?1049h\x1b[?2004h\x1b[31m\x1b[#{\x1b[?7s");
    let uri = format!("https://example.com/{}", "a".repeat(4000));
    t.process(format!("\x1b]8;id=k;{uri}\x1b\\linked").as_bytes());
    assert_eq!(
        t.current_hyperlink().map(AsRef::as_ref),
        Some(uri.as_str()),
        "PRECONDITION: the link is open"
    );
    let full = t.carry_meta_abandoning_partial();
    assert!(
        full.output_state.hyperlink.is_some() && !full.output_state.sgr_stack.is_empty(),
        "PRECONDITION: the handoff's meta holds the in-flight state"
    );
    assert!(
        aterm_json::to_string(&full).expect("json").len() > aterm_keeper::wire::MAX_META,
        "NON-VACUITY: with it the META would not fit"
    );

    let meta = crate::seamless::keeper_meta(&t).expect("fits");
    assert!(meta.len() <= aterm_keeper::wire::MAX_META);
    let text = String::from_utf8(meta.clone()).expect("utf-8");
    assert!(!text.contains("\"output_state\""), "{text}");
    let mut used = 0;
    let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
    assert!(cp.modes.bracketed_paste, "the modes still cross");
    assert!(cp.output_state.is_default());
    let mut recovered = Terminal::from_checkpoint(&cp);
    assert_eq!(recovered.current_hyperlink(), None, "no link reopened");
    let at = recovered.cursor();
    recovered.process(b"later");
    assert_eq!(
        recovered.hyperlink_at(at.row, at.col),
        None,
        "later output is not linked"
    );

    t.process(b"\x1b[3");
    let mut stale = t.carry_meta_abandoning_partial();
    stale.shell_integration_nonce = None;
    assert!(
        stale.parser.is_some() && !stale.output_state.is_default(),
        "PRECONDITION"
    );
    let stale = aterm_json::to_string(&stale).expect("json").into_bytes();
    let cp = crate::seamless::recovered_checkpoint(&stale, &mut used).expect("a checkpoint");
    assert!(cp.output_state.is_default(), "no in-flight output state");
    assert!(cp.parser.is_none(), "no partial sequence");
    let mut recovered = Terminal::from_checkpoint(&cp);
    assert_eq!(recovered.current_hyperlink(), None);
    let at = recovered.cursor();
    recovered.process(b"later");
    assert_eq!(recovered.hyperlink_at(at.row, at.col), None);
    assert!(
        recovered
            .row_text(usize::from(at.row))
            .unwrap_or_default()
            .trim_end()
            .ends_with("later"),
        "no partial CSI swallowed the text"
    );
}

/// A full-screen program's setup, as vim, less, htop and Claude Code send
/// it: the alternate screen, DECCKM and the keypad, SGR button mouse,
/// bracketed paste, a kitty keyboard push and a scroll region.
#[cfg(unix)]
const TUI_SETUP: &[u8] =
    b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[>1u\x1b[2;40r";

/// The old-form META: the sanitized carry as the handoff's JSON, each cursor's stops a
/// bool per column.
#[cfg(unix)]
fn old_form_meta(t: &Terminal) -> Vec<u8> {
    let mut meta = t.carry_meta_abandoning_partial();
    meta.shell_integration_nonce = None;
    meta.parser = None;
    meta.output_state = aterm_core::terminal::OutputStateRepr::default();
    aterm_json::to_string(&meta).expect("json").into_bytes()
}

/// `meta` as a window that predates the packed stops reads it: the packed key unknown, so
/// each cursor's `tab_stops` is the empty list the packed form writes.
#[cfg(unix)]
fn as_an_older_window_reads(meta: &[u8]) -> Vec<u8> {
    let mut value: aterm_json::Value = aterm_json::from_slice(meta).expect("the META is JSON");
    for slot in ["cursor", "alt_cursor"] {
        if let Some(cursor) = value
            .as_object_mut()
            .and_then(|object| object.get_mut(slot))
            .and_then(aterm_json::Value::as_object_mut)
        {
            cursor.remove("tab_stops_packed");
        }
    }
    aterm_json::to_string(&value).expect("json").into_bytes()
}

/// The modes [`TUI_SETUP`] sets, read off a recovered checkpoint.
#[cfg(unix)]
fn assert_tui_modes(cp: &aterm_core::terminal::TerminalCheckpoint, what: &str) {
    let modes = &cp.modes;
    assert!(modes.alternate_screen, "{what}: the alt screen");
    assert!(modes.application_cursor_keys, "{what}: DECCKM");
    assert!(modes.application_keypad, "{what}: the keypad");
    assert!(modes.bracketed_paste, "{what}: bracketed paste");
    assert_eq!(
        modes.mouse_mode,
        aterm_core::terminal::MouseMode::ButtonEvent,
        "{what}: the mouse"
    );
    assert_eq!(
        modes.mouse_encoding,
        aterm_core::terminal::MouseEncoding::Sgr,
        "{what}: the mouse encoding"
    );
}

/// The META fits at any width (`PackedTabStops`): 200- and 400-column alt-screen sessions
/// with custom stops, and a pane once 400 columns wide now 100, each fit and round-trip
/// their stops, the one past the width included.
#[cfg(unix)]
#[test]
fn a_wide_alt_screen_meta_fits_and_carries_its_tab_stops() {
    let recover = |t: &Terminal, what: &str| {
        let meta = crate::seamless::keeper_meta(t)
            .unwrap_or_else(|| panic!("{what}: the META was not built"));
        assert!(
            meta.len() <= aterm_keeper::wire::MAX_META,
            "{what}: {}",
            meta.len()
        );
        let mut used = 0;
        crate::seamless::recovered_checkpoint(&meta, &mut used)
            .unwrap_or_else(|| panic!("{what}: no checkpoint"))
    };
    for cols in [200_u16, 400] {
        let what = format!("{cols} columns");
        let mut t = Terminal::new(50, cols);
        let edge = format!("\x1b[1;{}H\x1bH", cols - 2);
        t.process(b"\x1b[1;6H\x1bH\x1b[1;17H\x1b[g");
        t.process(edge.as_bytes());
        t.process(b"\x1b[H");
        t.process(TUI_SETUP);
        let carried = t.carry_meta_abandoning_partial();
        let alt = carried
            .alt_cursor
            .clone()
            .expect("PRECONDITION: an alt grid");
        assert!(
            carried.cursor.tab_stops[5] && !carried.cursor.tab_stops[16],
            "PRECONDITION: custom stops"
        );
        let cp = recover(&t, &what);
        assert_tui_modes(&cp, &what);
        assert_eq!(cp.cursor.tab_stops, carried.cursor.tab_stops, "{what}");
        let alt_cp = cp.alt_cursor.clone().expect("the saved primary's cursor");
        assert_eq!(alt_cp.tab_stops, alt.tab_stops, "{what}");
        assert_eq!(
            (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
            (1, 39),
            "{what}: the program's cursor is carried, not reset"
        );
        let recovered = Terminal::from_checkpoint(&cp);
        assert_eq!(recovered.grid().tab_stops(), t.grid().tab_stops(), "{what}");
        assert_eq!(recovered.kitty_keyboard_flags(), t.kitty_keyboard_flags());
    }

    let mut t = Terminal::new(50, 400);
    t.process(b"\x1b[1;301H\x1bH\x1b[H");
    t.resize(50, 100);
    t.process(TUI_SETUP);
    assert_eq!(
        t.grid().tab_stops().len(),
        400,
        "PRECONDITION: never shrunk"
    );
    let cp = recover(&t, "once 400 columns, now 100");
    assert_tui_modes(&cp, "once wide");
    assert_eq!(cp.cursor.tab_stops.len(), 400);
    assert!(cp.cursor.tab_stops[300], "the stop past the width");
    let mut recovered = Terminal::from_checkpoint(&cp);
    recovered.resize(50, 400);
    assert!(
        recovered.grid().tab_stops()[300],
        "a widen finds the stop again"
    );
}

/// A META IS BUILT FOR EVERY SESSION (R17-HANDOFF-1): with the stops
/// packed, a default session's META does not grow with the width; and the
/// worst a session can carry — 4,096 columns with a stop at every one, on
/// both screens, a long directory and title, a pushed colour table, marks,
/// full kitty stacks, both DECSC slots and counters at their widest — sheds
/// to the fixed scalars and still fits, its modes and regions intact and
/// its stops the every-8 defaults.
#[cfg(unix)]
#[test]
fn a_meta_is_built_for_every_session() {
    let size = |cols: u16| {
        let mut t = Terminal::new(50, cols);
        t.process(TUI_SETUP);
        crate::seamless::keeper_meta(&t)
            .unwrap_or_else(|| panic!("{cols} columns"))
            .len()
    };
    let sizes = [200, 400, 4096].map(size);
    let spread =
        sizes.iter().max().copied().unwrap_or(0) - sizes.iter().min().copied().unwrap_or(0);
    assert!(spread <= 16, "only the numbers' digits grow: {sizes:?}");

    let cols = aterm_core::grid::MAX_GRID_COLS;
    let mut t = Terminal::new(50, cols);
    let mut setup = Vec::new();
    for col in 1..=cols {
        setup.extend_from_slice(format!("\x1b[1;{col}H\x1bH").as_bytes());
    }
    setup.extend_from_slice(format!("\x1b]7;file://host/{}\x1b\\", "d".repeat(2000)).as_bytes());
    setup.extend_from_slice(format!("\x1b]2;{}\x07", "t".repeat(2000)).as_bytes());
    for index in 0..256 {
        setup.extend_from_slice(format!("\x1b]4;{index};rgb:12/34/56\x1b\\").as_bytes());
    }
    setup.extend_from_slice(b"\x1b[#P\x1b[#P\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07\r\n");
    setup.extend_from_slice(
        b"\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b7",
    );
    setup.extend_from_slice(TUI_SETUP);
    setup.extend_from_slice(b"\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b7");
    for col in 1..=cols {
        setup.extend_from_slice(format!("\x1b[1;{col}H\x1bH").as_bytes());
    }
    t.process(&setup);
    let mut carry = t.carry_meta_abandoning_partial();
    let alt = carry
        .alt_cursor
        .as_ref()
        .expect("PRECONDITION: two cursors");
    assert!(
        carry.cursor.tab_stops.iter().all(|stop| *stop) && alt.tab_stops.iter().all(|stop| *stop),
        "PRECONDITION: a stop at every column of both grids"
    );
    assert!(
        carry.current_working_directory.is_some() && carry.title.is_some(),
        "PRECONDITION: a directory and a title"
    );
    carry.absolute_row_counter = u64::MAX;
    carry.alt_absolute_row_counter = u64::MAX;
    carry.style_fg_bits = u32::MAX;
    carry.style_bg_bits = u32::MAX;
    carry.style_flag_bits = u16::MAX;
    carry.style_protected = true;
    carry.secure_keyboard_entry = true;
    carry.taskbar_progress = Some(aterm_types::TaskbarProgress::Paused(100));
    carry.shell_integration_rev = Some("f".repeat(16));
    let widest = aterm_core::terminal::KittyKeyboardFlags::from_bits(u8::MAX);
    let kitty = &mut carry.kitty_keyboard;
    kitty.flags = widest;
    kitty.main_saved_flags = Some(widest);
    kitty.alt_saved_flags = Some(widest);
    kitty.main_stack = [widest; 8];
    kitty.alt_stack = [widest; 8];
    for saved in [&mut carry.saved_cursor_main, &mut carry.saved_cursor_alt] {
        let slot = saved.as_mut().expect("PRECONDITION: both DECSC slots");
        slot.cursor_row = aterm_core::grid::MAX_GRID_ROWS - 1;
        slot.cursor_col = cols - 1;
        slot.style_fg_bits = u32::MAX;
        slot.style_bg_bits = u32::MAX;
        slot.style_flag_bits = u16::MAX;
        slot.underline_color = Some(u32::MAX);
    }
    let widest = widest_scalars(carry.clone());
    let ceiling = i64::MAX.unsigned_abs();
    let fabric = crate::fabric::KeeperCarry {
        hold: Some(format!("fleet {}", "\"".repeat(128))),
        more_holds: vec![format!("local {}", "\"".repeat(128))],
        ids: [ceiling; 3],
        topics: (0..crate::fabric::MAX_TOPICS)
            .map(|n| format!("{n:02}{} @{}", "t".repeat(30), u64::MAX))
            .collect(),
        queued: (0..crate::fabric::OUTBOX_CAP)
            .map(|n| ceiling - u64::try_from(n).expect("n"))
            .collect(),
        owed: u64::try_from(crate::fabric::RING_CAP).expect("cap"),
        dropped: ceiling,
        ..crate::fabric::KeeperCarry::default()
    };
    let carried = |meta: &[u8], what: &str, reasons: bool| {
        let recovered = crate::seamless::recovered_fabric(meta);
        let origins: Vec<String> = recovered
            .holds
            .iter()
            .map(|row| crate::fabric::parse_hold(row).origin)
            .collect();
        assert_eq!(origins, ["fleet", "local"], "{what}: no hold is ever shed");
        if reasons {
            let whole: Vec<String> = fabric
                .hold
                .iter()
                .chain(&fabric.more_holds)
                .cloned()
                .collect();
            assert_eq!(recovered.holds, whole, "{what}: nor a reason");
        }
        assert_eq!(
            [
                recovered.carry.last_msg_id,
                recovered.carry.last_post_id,
                recovered.carry.last_receipt_id
            ],
            fabric.ids,
            "{what}: nor the lease"
        );
        assert_eq!(recovered.carry.receipts_uncarried, fabric.owed, "{what}");
        assert_eq!(
            recovered.carry.dropped, fabric.dropped,
            "{what}: nor dropped="
        );
        let named = |n: usize| u64::try_from(n).expect("n");
        assert_eq!(
            named(recovered.topics.len()) + recovered.topics_lost,
            named(fabric.topics.len()),
            "{what}: every opt-in is named or counted"
        );
        assert_eq!(
            named(recovered.carry.uncarried.len()) + recovered.queued_lost,
            named(fabric.queued.len()),
            "{what}: every queued post is named or counted"
        );
    };
    let meta =
        crate::seamless::keeper_meta_of(carry, Some(&fabric)).expect("the worst case is built");
    assert!(meta.len() <= aterm_keeper::wire::MAX_META, "{}", meta.len());
    carried(&meta, "the worst case", true);
    let worst = meta.len();
    let mut used = 0;
    let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
    assert_tui_modes(&cp, "the worst case");
    assert_eq!(
        (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
        (1, 39),
        "the region stays"
    );
    let defaults = crate::seamless::default_tab_stops(usize::from(cols));
    assert_eq!(
        cp.cursor.tab_stops, defaults,
        "the custom stops were shed last"
    );
    assert_eq!(cp.alt_cursor.map(|alt| alt.tab_stops), Some(defaults));

    let modes = aterm_json::to_string(&widest.modes).expect("json");
    let charset = aterm_json::to_string(&widest.charset).expect("json");
    assert!(
        !modes.contains("true")
            && modes.contains("SgrPixel")
            && charset.contains("IsoLatin1Supplemental")
            && widest.history_lines == u32::MAX
            && widest.cursor.scroll_bottom == u16::MAX,
        "PRECONDITION: the widest scalars: {modes} {charset}"
    );
    let meta = crate::seamless::keeper_meta_of(widest, Some(&fabric)).expect("the widest is built");
    assert!(meta.len() <= aterm_keeper::wire::MAX_META, "{}", meta.len());
    carried(&meta, "the widest scalars", false);
    eprintln!(
        "the worst case: {worst} bytes; with the widest scalars: {} (cap {})",
        meta.len(),
        aterm_keeper::wire::MAX_META
    );
}

/// `meta` with every scalar a shed META keeps at its widest JSON, to bound its size: each
/// bool `false` (a byte wider than `true`); each number, and each absent optional one, the
/// widest its field takes back unchanged (`u64::MAX`, then `u32::MAX`, `u16::MAX`, `null`
/// and `u8::MAX`, tried in that order); each enum its longest name; and the widest
/// character sets in every slot.
#[cfg(unix)]
fn widest_scalars(
    mut meta: aterm_core::terminal::CheckpointMeta,
) -> aterm_core::terminal::CheckpointMeta {
    use aterm_json::Value;
    fn nth_scalar<'a>(value: &'a mut Value, n: &mut usize) -> Option<&'a mut Value> {
        match value {
            Value::Object(map) => map.values_mut().find_map(|child| nth_scalar(child, n)),
            Value::Array(items) => items.iter_mut().find_map(|child| nth_scalar(child, n)),
            scalar if *n == 0 => Some(scalar),
            _ => {
                *n -= 1;
                None
            }
        }
    }
    let takes_back = |value: &Value| {
        aterm_json::from_value::<aterm_core::terminal::CheckpointMeta>(value.clone())
            .ok()
            .and_then(|meta| aterm_json::to_value(&meta).ok())
            .as_ref()
            == Some(value)
    };
    let main_stops = std::mem::take(&mut meta.cursor.tab_stops);
    let alt_stops = meta
        .alt_cursor
        .as_mut()
        .map(|alt| std::mem::take(&mut alt.tab_stops));
    let mut value = aterm_json::to_value(&meta).expect("json");
    let widest_numbers = [
        Value::from(u64::MAX),
        Value::from(u32::MAX),
        Value::from(u16::MAX),
        Value::Null,
        Value::from(u8::MAX),
    ];
    let mut index = 0;
    while let Some(scalar) = nth_scalar(&mut value, &mut index.clone()) {
        let candidates: &[Value] = match scalar {
            Value::Bool(_) => &[Value::Bool(false)],
            Value::Number(_) | Value::Null => &widest_numbers,
            _ => &[],
        };
        for candidate in candidates {
            let mut trial = value.clone();
            if let Some(scalar) = nth_scalar(&mut trial, &mut index.clone()) {
                *scalar = candidate.clone();
            }
            if takes_back(&trial) {
                value = trial;
                break;
            }
        }
        index += 1;
    }
    let mut meta: aterm_core::terminal::CheckpointMeta =
        aterm_json::from_value(value).expect("every trial taken back");
    meta.cursor.tab_stops = main_stops;
    if let (Some(alt), Some(stops)) = (meta.alt_cursor.as_mut(), alt_stops) {
        alt.tab_stops = stops;
    }
    let modes = &mut meta.modes;
    modes.cursor_style = aterm_types::CursorStyle::BlinkingUnderline;
    modes.mouse_mode = aterm_core::terminal::MouseMode::ButtonEvent;
    modes.mouse_encoding = aterm_core::terminal::MouseEncoding::SgrPixel;
    modes.color_scheme = aterm_types::Appearance::Light;
    modes.bidi_mode = aterm_types::BiDiMode::Disabled;
    modes.bidi_direction = aterm_types::ParagraphDirection::AutoRtl;
    meta.taskbar_progress = Some(aterm_types::TaskbarProgress::Indeterminate);
    let mut charset = aterm_types::CharacterSetState::default();
    for g_set in 0..4 {
        charset.designate(g_set, aterm_types::CharacterSet::NorwegianDanish);
        charset.designate_96(g_set, aterm_types::CharacterSet96::IsoLatin1Supplemental);
    }
    meta.charset = charset;
    for saved in [&mut meta.saved_cursor_main, &mut meta.saved_cursor_alt]
        .into_iter()
        .flatten()
    {
        saved.charset = charset;
    }
    meta
}

/// Version skew, forward: an old-form META (a bool per column for the stops) still
/// restores its stops, cursor and modes.
#[cfg(unix)]
#[test]
fn a_meta_an_older_window_sent_still_restores() {
    let mut t = Terminal::new(50, 80);
    t.process(b"\x1b[1;6H\x1bH\x1b[H");
    t.process(TUI_SETUP);
    let old = old_form_meta(&t);
    let text = String::from_utf8(old.clone()).expect("utf-8");
    assert!(
        text.contains("\"tab_stops\":[false,"),
        "PRECONDITION: the old form: {text}"
    );
    let mut used = 0;
    let cp = crate::seamless::recovered_checkpoint(&old, &mut used).expect("a checkpoint");
    assert_tui_modes(&cp, "an old META");
    let carried = t.carry_meta_abandoning_partial();
    assert_eq!(cp.cursor.tab_stops, carried.cursor.tab_stops);
    assert_eq!((cp.cursor.scroll_top, cp.cursor.scroll_bottom), (1, 39));
    assert!(cp.cursor.tab_stops[5], "the custom stop crosses");
}

/// Version skew, backward (a rollback, or an older build launched after a newer one
/// crashed): a META the old form holds is sent in it byte for byte, and a packed one read
/// by an older window keeps every mode and loses only the region, margins and custom stops
/// (`PackedTabStops`).
#[cfg(unix)]
#[test]
fn an_older_window_reads_a_new_meta_with_its_modes() {
    let mut narrow = Terminal::new(50, 80);
    narrow.process(b"\x1b[1;6H\x1bH\x1b[H");
    narrow.process(TUI_SETUP);
    assert_eq!(
        crate::seamless::keeper_meta(&narrow),
        Some(old_form_meta(&narrow)),
        "what the old form carries goes in it"
    );

    let mut t = Terminal::new(50, 200);
    t.process(TUI_SETUP);
    let meta = crate::seamless::keeper_meta(&t).expect("fits");
    assert!(
        old_form_meta(&t).len() > aterm_keeper::wire::MAX_META
            && String::from_utf8_lossy(&meta).contains("\"tab_stops_packed\""),
        "PRECONDITION: the old form does not fit, so this one is packed"
    );
    let older = as_an_older_window_reads(&meta);
    let parsed: aterm_core::terminal::CheckpointMeta =
        aterm_json::from_slice(&older).expect("an older build parses the new form");
    assert!(parsed.modes.bracketed_paste);
    let mut used = 0;
    let cp = crate::seamless::recovered_checkpoint(&older, &mut used).expect("a checkpoint");
    assert_tui_modes(&cp, "an older reader");
    assert_eq!(cp.cursor.tab_stops.len(), 200, "the blank cursor's stops");
    assert_eq!(
        (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
        (0, 49),
        "the region is the blank cursor's"
    );
}

/// THE BAND. Where the whole carry does not fit the old form but would without the
/// directory and colours (then the title and mark state), the meta goes packed, not
/// shed: every reader keeps the directory, and a newer one the program's region too.
#[cfg(unix)]
#[test]
fn where_only_a_shed_old_form_fit_the_meta_goes_packed_and_whole() {
    let in_band = |cols: u16| {
        let mut t = Terminal::new(50, cols);
        t.process(format!("\x1b]7;file://host/{}\x1b\\", "d".repeat(60)).as_bytes());
        t.process(TUI_SETUP);
        let mut shed = t.carry_meta_abandoning_partial();
        let dir = shed
            .current_working_directory
            .take()
            .expect("PRECONDITION: a directory");
        shed.color = aterm_core::terminal::ColorRepr::default();
        shed.title = None;
        shed.shell = aterm_core::terminal::ShellRepr::default();
        shed.shell_integration_nonce = None;
        shed.parser = None;
        shed.output_state = aterm_core::terminal::OutputStateRepr::default();
        let (whole, shed) = (
            old_form_meta(&t).len(),
            aterm_json::to_string(&shed).expect("json").len(),
        );
        (whole > aterm_keeper::wire::MAX_META && shed <= aterm_keeper::wire::MAX_META)
            .then_some((t, dir))
    };
    let (t, dir) = (40_u16..=400)
        .find_map(in_band)
        .expect("PRECONDITION: a width where only a shed old form fits");

    let meta = crate::seamless::keeper_meta(&t).expect("built");
    assert!(
        String::from_utf8_lossy(&meta).contains("\"tab_stops_packed\""),
        "the band goes packed, not shed"
    );
    let mut used = 0;
    let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
    assert_tui_modes(&cp, "a newer reader");
    assert_eq!(
        cp.current_working_directory.as_deref(),
        Some(dir.as_str()),
        "a newer reader keeps the directory"
    );
    assert_eq!(
        (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
        (1, 39),
        "and the program's region"
    );

    let cp = crate::seamless::recovered_checkpoint(&as_an_older_window_reads(&meta), &mut used)
        .expect("a checkpoint");
    assert_tui_modes(&cp, "an older reader");
    assert_eq!(
        cp.current_working_directory.as_deref(),
        Some(dir.as_str()),
        "an older reader keeps the directory it used to lose"
    );
    assert_eq!(
        (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
        (0, 49),
        "and loses the region it used to keep"
    );
}

/// A META not sent (not built, no link, a full queue) is retried on the next pass: the
/// pump records only the fingerprints it sent.
#[cfg(unix)]
#[test]
fn a_meta_that_was_not_sent_is_retried() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term.lock().expect("term").process(b"\x1b[?2004h");
    let session = crate::session_store::test_handle(7);
    let state = Mutex::new(State {
        phase: Phase::Off,
        buffered: Vec::new(),
        owed: VecDeque::new(),
        tracked: BTreeMap::from([(
            7,
            Tracked {
                rdev: 1,
                prior_claim: false,
                term: Arc::downgrade(&term),
                fabric: Arc::downgrade(&session.ctx.fabric),
                last: None,
            },
        )]),
        pump: false,
    });
    let last = || state.lock().expect("state").tracked[&7].last.clone();
    let build = |meta, fabric: &crate::fabric::KeeperCarry| {
        crate::seamless::keeper_meta_of(meta, Some(fabric))
    };
    let unchanged = |_, _: &crate::fabric::KeeperCarry| -> Option<Vec<u8>> {
        panic!("an unchanged fingerprint and fabric build nothing")
    };

    assert_eq!(meta_pass_over(&state, |_, _| None), 0);
    assert_eq!(last(), None, "a META that was not built is tried again");
    assert_eq!(meta_pass_over(&state, build), 0);
    assert_eq!(last(), None, "a META no link took is tried again");

    let link = KeeperLink::start(
        std::path::PathBuf::from("/nonexistent-aterm-keeper-test/k.sock"),
        aterm_keeper::identity::Identity::same_uid(),
        "test".into(),
    )
    .expect("link");
    state.lock().expect("state").phase = Phase::Live(Arc::new(link));
    assert_eq!(meta_pass_over(&state, build), 1, "the retry sends it");
    let sent = Fingerprint::of(&term.lock().expect("term"));
    assert_eq!(
        last().map(|(fingerprint, _)| fingerprint),
        Some(sent),
        "what was sent is recorded"
    );
    assert_eq!(meta_pass_over(&state, unchanged), 0);

    let held = crate::fabric::Hold {
        reason: "stop".to_string(),
        origin: "local".to_string(),
    };
    assert!(crate::fabric::apply_hold_for_test(&session.ctx, Some(held)));
    let sent_meta = std::sync::Mutex::new(Vec::new());
    let capture = |meta, fabric: &crate::fabric::KeeperCarry| {
        let bytes = crate::seamless::keeper_meta_of(meta, Some(fabric));
        sent_meta
            .lock()
            .expect("capture")
            .clone_from(bytes.as_ref().expect("a META"));
        bytes
    };
    assert_eq!(meta_pass_over(&state, capture), 1, "a hold sends it");
    let recovered = crate::seamless::recovered_fabric(&sent_meta.lock().expect("capture"));
    assert_eq!(recovered.holds, ["local stop"]);
    assert_eq!(meta_pass_over(&state, unchanged), 0);
    let halt = crate::fabric::Hold {
        reason: "halt".to_string(),
        origin: "fleet".to_string(),
    };
    assert!(crate::fabric::apply_hold_for_test(&session.ctx, Some(halt)));
    assert_eq!(meta_pass_over(&state, capture), 1, "a second hold sends it");
    let recovered = crate::seamless::recovered_fabric(&sent_meta.lock().expect("capture"));
    assert_eq!(recovered.holds, ["fleet halt", "local stop"]);
    assert_eq!(meta_pass_over(&state, unchanged), 0);
    assert_eq!(
        session
            .ctx
            .fabric
            .topic_add("builds", "head", &session.ctx.timeline),
        Ok(true)
    );
    assert_eq!(meta_pass_over(&state, capture), 1, "a topic sends it");
    assert!(crate::fabric::apply_hold_for_test(&session.ctx, None));
    assert_eq!(meta_pass_over(&state, capture), 1, "a lift sends it");
    let recovered = crate::seamless::recovered_fabric(&sent_meta.lock().expect("capture"));
    assert_eq!(
        (recovered.holds, recovered.topics),
        (Vec::<String>::new(), vec!["builds head".to_string()]),
        "the last META sent is the fabric as it stands"
    );
    assert_eq!(meta_pass_over(&state, unchanged), 0);
    let phase = std::mem::replace(&mut state.lock().expect("state").phase, Phase::Off);
    if let Phase::Live(link) = phase
        && let Ok(link) = Arc::try_unwrap(link)
    {
        link.shutdown();
    }
}

/// A FULL QUEUE NEVER DROPS A REPLACED REGION'S CUSTODY (R6). The memory shed hands the
/// keeper a fresh region's only read-only descriptor; a link whose keeper is slow to
/// read (paged out under the very pressure being shed) has a full queue, so
/// `replace_companion` hands the region back (`CompanionOffer::Busy`) for another
/// offer: a COMPANION dropped there would leave the keeper on the superseded region
/// for good.
#[cfg(unix)]
#[test]
fn a_full_queue_hands_a_replaced_regions_custody_back_for_another_offer() {
    use std::os::fd::{AsFd as _, OwnedFd};
    use std::os::unix::fs::MetadataExt as _;
    let inode = |fd: &OwnedFd| {
        std::fs::File::from(fd.as_fd().try_clone_to_owned().expect("dup"))
            .metadata()
            .expect("stat")
            .ino()
    };
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let made = std::cell::Cell::new(0u32);
    let region = || {
        made.set(made.get() + 1);
        let path = scratch.path().join(format!("region-{}", made.get()));
        OwnedFd::from(std::fs::File::create(path).expect("a region of its own"))
    };
    let companions = |ops: Vec<LinkOp>| -> Vec<(u64, u64)> {
        ops.into_iter()
            .filter_map(|op| match op {
                LinkOp::Companion { rdev, region, .. } => Some((rdev, inode(&region))),
                _ => None,
            })
            .collect()
    };
    let link = TestLink::stalled(7, 70);
    link.fill();
    let fresh = region();
    let fresh_ino = inode(&fresh);
    assert_ne!(
        inode(&region()),
        fresh_ino,
        "premise: an inode tells one region from another"
    );
    let busy = match link.replace_companion(7, fresh, 4096) {
        CompanionOffer::Busy(back) => back,
        other => panic!("a full queue hands the custody back: {other:?}"),
    };
    assert_eq!(
        inode(&busy),
        fresh_ino,
        "the region it was offered comes back"
    );
    assert_eq!(
        companions(link.drain()),
        Vec::<(u64, u64)>::new(),
        "nothing reached the keeper meanwhile"
    );
    assert!(matches!(
        link.replace_companion(7, busy, 4096),
        CompanionOffer::Handed
    ));
    assert_eq!(
        companions(link.drain()),
        [(70, fresh_ino)],
        "offered again, the keeper is told the fresh region"
    );
    assert!(matches!(
        link.replace_companion(8, region(), 4096),
        CompanionOffer::Unlinked(_)
    ));

    link.fill();
    {
        let st = link.state.lock().expect("test link");
        let Phase::Live(live) = &st.phase else {
            unreachable!("a test link is live")
        };
        let _ = live.enqueue(LinkOp::Companion {
            rdev: 70,
            region: region(),
            len: 4096,
            receipt: None,
        });
    }
    assert_eq!(
        companions(link.drain()),
        Vec::<(u64, u64)>::new(),
        "control: `let _ = enqueue` leaves the keeper on the superseded region"
    );
}

/// Descriptors of files of their own, each told from another by its inode (every open
/// of `/dev/null` shares one), for the owed-op tests: a master, a region.
#[cfg(unix)]
struct Files {
    dir: aterm_tempfile::TempDir,
    made: std::cell::Cell<u32>,
}

#[cfg(unix)]
impl Files {
    fn new() -> Self {
        Self {
            dir: aterm_tempfile::tempdir().expect("scratch"),
            made: std::cell::Cell::new(0),
        }
    }

    fn fd(&self) -> (std::os::fd::OwnedFd, u64) {
        self.made.set(self.made.get() + 1);
        let path = self.dir.path().join(format!("file-{}", self.made.get()));
        let fd = std::os::fd::OwnedFd::from(std::fs::File::create(path).expect("a file"));
        let ino = inode(&fd);
        (fd, ino)
    }
}

#[cfg(unix)]
fn inode(fd: &std::os::fd::OwnedFd) -> u64 {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;
    std::fs::File::from(fd.as_fd().try_clone_to_owned().expect("dup"))
        .metadata()
        .expect("stat")
        .ino()
}

/// What the keeper read, in order, but for [`TestLink::fill`]'s ops: each op's kind, its
/// rdev (a PENDING's pid) and the inode of the descriptor it carries (0 for none).
#[cfg(unix)]
fn seen(ops: Vec<LinkOp>) -> Vec<(&'static str, u64, u64)> {
    ops.into_iter()
        .filter_map(|op| match op {
            LinkOp::Release { rdev } if rdev == FILLER_RDEV => None,
            LinkOp::Release { rdev } => Some(("RELEASE", rdev, 0)),
            LinkOp::Retire { rdev, .. } => Some(("RETIRE", rdev, 0)),
            LinkOp::Register { master, header, .. } => {
                Some(("REGISTER", header.rdev, inode(&master)))
            }
            LinkOp::Companion { rdev, region, .. } => Some(("COMPANION", rdev, inode(&region))),
            LinkOp::CompanionRelease { rdev, .. } => Some(("COMPANION_RELEASE", rdev, 0)),
            LinkOp::Meta { rdev, .. } => Some(("META", rdev, 0)),
            LinkOp::Pending { pid } => Some(("PENDING", u64::from(pid), 0)),
            LinkOp::Bye { .. } => Some(("BYE", 0, 0)),
        })
        .collect()
}

/// A REGISTER A FULL QUEUE COULD NOT TAKE IS OWED, AND A LATER TICK DELIVERS IT (R9 (a)).
#[cfg(unix)]
#[test]
fn a_register_a_full_queue_refused_is_owed_and_a_later_tick_delivers_it() {
    let files = Files::new();
    let link = TestLink::bare();
    link.fill();
    let (master, master_ino) = files.fd();
    link.register(
        7,
        70,
        master,
        LinkOp::CompanionRelease {
            rdev: 70,
            receipt: None,
        },
    );
    assert_eq!(
        link.owed(),
        2,
        "the REGISTER and its claim's settle are owed"
    );
    let (fresh, _) = files.fd();
    assert!(
        matches!(
            link.replace_companion(7, fresh, 4096),
            CompanionOffer::Busy(_)
        ),
        "a COMPANION never overtakes the REGISTER it rides on"
    );
    assert_eq!(
        seen(link.drain()),
        [],
        "nothing reached the keeper meanwhile"
    );
    link.tick();
    assert_eq!(link.owed(), 0, "the tick queued what was owed");
    assert_eq!(
        seen(link.drain()),
        [("REGISTER", 70, master_ino), ("COMPANION_RELEASE", 70, 0)],
        "the keeper reads the REGISTER, then its claim settled"
    );

    link.fill();
    let (master, _) = files.fd();
    link.enqueue_dropping(LinkOp::Register {
        master,
        header: MasterHeader {
            rdev: 80,
            shell_pid: 1,
            shell_birth: Birth {
                seconds: 1,
                micros: 0,
            },
            local_id: 8,
        },
        tag: Vec::new(),
    });
    let first = seen(link.drain());
    link.tick();
    let later = seen(link.drain());
    assert_eq!(
        (first, later),
        (vec![], vec![]),
        "control: `let _ = enqueue` never registers the master"
    );

    let churn = TestLink::bare();
    churn.fill();
    for id in 100..132 {
        let (master, _) = files.fd();
        churn.register(
            id,
            id + 1_000,
            master,
            LinkOp::CompanionRelease {
                rdev: id + 1_000,
                receipt: None,
            },
        );
        assert_eq!(churn.owed(), 2);
        assert!(
            live_link_in(&mut churn.state.lock().expect("state")).is_err(),
            "PENDING is refused while older ops remain outside the channel"
        );
        churn.release(id);
        assert_eq!(churn.owed(), 0, "close retires its unsent lifecycle");
    }
    assert!(churn.state.lock().expect("state").tracked.is_empty());
    assert!(
        seen(churn.drain()).is_empty(),
        "no closed lifecycle entered the channel"
    );

    let admitted = TestLink::bare();
    admitted.fill_but(1);
    let (master, ino) = files.fd();
    admitted.register(
        200,
        2_000,
        master,
        LinkOp::CompanionRelease {
            rdev: 2_000,
            receipt: None,
        },
    );
    admitted.release(200);
    assert_eq!(admitted.owed(), 2);
    assert_eq!(seen(admitted.drain()), [("REGISTER", 2_000, ino)]);
    admitted.tick();
    assert_eq!(
        seen(admitted.drain()),
        [("COMPANION_RELEASE", 2_000, 0), ("RELEASE", 2_000, 0)]
    );
    assert!(matches!(
        live_link_in(&mut admitted.state.lock().expect("state")),
        Ok(Some(_))
    ));

    let prior = TestLink::bare();
    prior.fill();
    let (master, ino) = files.fd();
    prior.register(
        250,
        2_500,
        master,
        LinkOp::CompanionRelease {
            rdev: 2_500,
            receipt: None,
        },
    );
    prior
        .state
        .lock()
        .expect("state")
        .tracked
        .get_mut(&250)
        .expect("tracked")
        .prior_claim = true;
    prior.release(250);
    assert_eq!(prior.owed(), 3, "the earlier keeper claim still must end");
    assert!(seen(prior.drain()).is_empty());
    prior.tick();
    assert_eq!(
        seen(prior.drain()),
        [
            ("REGISTER", 2_500, ino),
            ("COMPANION_RELEASE", 2_500, 0),
            ("RELEASE", 2_500, 0)
        ]
    );

    let unclaimed = TestLink::bare();
    unclaimed.fill();
    send_in(
        &mut unclaimed.state.lock().expect("state"),
        None,
        LinkOp::Release { rdev: 9_000 },
    );
    let (master, _) = files.fd();
    unclaimed.register(
        u64::MAX,
        9_001,
        master,
        LinkOp::CompanionRelease {
            rdev: 9_001,
            receipt: None,
        },
    );
    unclaimed.release(u64::MAX);
    assert_eq!(unclaimed.owed(), 1);
    assert!(seen(unclaimed.drain()).is_empty());
    unclaimed.tick();
    assert_eq!(seen(unclaimed.drain()), [("RELEASE", 9_000, 0)]);

    let quitting = TestLink::bare();
    quitting.fill();
    let (master, _) = files.fd();
    quitting.register(
        300,
        3_000,
        master,
        LinkOp::CompanionRelease {
            rdev: 3_000,
            receipt: None,
        },
    );
    let live = quit_link_in(&mut quitting.state.lock().expect("state"));
    assert!(live.is_some());
    assert_eq!(quitting.owed(), 0);
    assert!(quitting.state.lock().expect("state").tracked.is_empty());
    assert!(seen(quitting.drain()).is_empty());
}

/// A SESSION'S FIRST COMPANION A FULL QUEUE COULD NOT TAKE IS OWED, AND A LATER TICK
/// DELIVERS IT (R9 (a)).
#[cfg(unix)]
#[test]
fn a_first_companion_a_full_queue_refused_is_owed_and_a_later_tick_delivers_it() {
    let files = Files::new();
    let link = TestLink::bare();
    link.fill_but(1);
    let (master, master_ino) = files.fd();
    let (region, region_ino) = files.fd();
    link.register(
        7,
        70,
        master,
        LinkOp::Companion {
            rdev: 70,
            region,
            len: 4096,
            receipt: None,
        },
    );
    assert_eq!(link.owed(), 1, "the COMPANION is owed");
    let (fresh, fresh_ino) = files.fd();
    let fresh = match link.replace_companion(7, fresh, 4096) {
        CompanionOffer::Busy(back) => back,
        other => panic!("the replacement waits behind the first region: {other:?}"),
    };
    assert_eq!(
        seen(link.drain()),
        [("REGISTER", 70, master_ino)],
        "the queue took the REGISTER alone"
    );
    link.tick();
    assert_eq!(link.owed(), 0, "the tick queued what was owed");
    assert!(matches!(
        link.replace_companion(7, fresh, 4096),
        CompanionOffer::Handed
    ));
    assert_eq!(
        seen(link.drain()),
        [("COMPANION", 70, region_ino), ("COMPANION", 70, fresh_ino)],
        "the first region, then its replacement: the keeper ends on the fresh one"
    );

    link.fill_but(1);
    let (master, master_ino) = files.fd();
    let (region, _) = files.fd();
    link.enqueue_dropping(LinkOp::Register {
        master,
        header: MasterHeader {
            rdev: 80,
            shell_pid: 1,
            shell_birth: Birth {
                seconds: 1,
                micros: 0,
            },
            local_id: 8,
        },
        tag: Vec::new(),
    });
    link.enqueue_dropping(LinkOp::Companion {
        rdev: 80,
        region,
        len: 4096,
        receipt: None,
    });
    let first = seen(link.drain());
    link.tick();
    let later = seen(link.drain());
    assert_eq!(
        (first, later),
        (vec![("REGISTER", 80, master_ino)], vec![]),
        "control: `let _ = enqueue` leaves the master with no region"
    );
}

/// A COMPANION_RELEASE A FULL QUEUE COULD NOT TAKE IS OWED, AND A LATER TICK DELIVERS IT
/// (R9 (a)).
#[cfg(unix)]
#[test]
fn a_companion_release_a_full_queue_refused_is_owed_and_a_later_tick_delivers_it() {
    let link = TestLink::stalled(7, 70);
    link.fill();
    link.release_companion(7);
    link.release(7);
    assert_eq!(
        link.owed(),
        2,
        "the COMPANION_RELEASE and the RELEASE are owed"
    );
    assert_eq!(
        seen(link.drain()),
        [],
        "nothing reached the keeper meanwhile"
    );
    link.tick();
    assert_eq!(link.owed(), 0, "the tick queued what was owed");
    assert_eq!(
        seen(link.drain()),
        [("COMPANION_RELEASE", 70, 0), ("RELEASE", 70, 0)],
        "the keeper drops the region, then the master"
    );
    link.release_companion(7);
    assert_eq!(
        (link.owed(), seen(link.drain())),
        (0, vec![]),
        "nothing for a session the link no longer carries"
    );

    let link = TestLink::stalled(8, 80);
    link.fill();
    link.enqueue_dropping(LinkOp::CompanionRelease {
        rdev: 80,
        receipt: None,
    });
    let first = seen(link.drain());
    link.tick();
    let later = seen(link.drain());
    assert_eq!(
        (first, later),
        (vec![], vec![]),
        "control: `let _ = enqueue` leaves the keeper on the stopped region"
    );
}

/// PENDING refuses while older lifecycle work is external, even when waited
/// room is available.
#[cfg(unix)]
#[test]
fn a_pending_waits_for_older_lifecycle_ops_and_a_bye_ends_them() {
    let files = Files::new();
    let behind = || {
        let link = TestLink::bare();
        link.fill();
        let (master, master_ino) = files.fd();
        link.register(
            7,
            70,
            master,
            LinkOp::CompanionRelease {
                rdev: 70,
                receipt: None,
            },
        );
        assert_eq!(
            link.owed(),
            2,
            "premise: the REGISTER and its claim's settle are owed"
        );
        link.fill_but(1);
        link.tick();
        assert_eq!(
            link.owed(),
            1,
            "premise: the pass queued the REGISTER, and the queue is full again"
        );
        (link, master_ino)
    };

    let (link, master_ino) = behind();
    assert!(
        live_link_in(&mut link.state.lock().expect("state")).is_err(),
        "PENDING does not overtake external lifecycle work"
    );
    assert!(!link.pending(9), "blocked before waited admission");
    link.bye(&[]);
    link.tick();
    assert_eq!(
        seen(link.drain()),
        [("REGISTER", 70, master_ino), ("BYE", 0, 0)],
        "no blocked PENDING was queued; terminal BYE follows admitted work"
    );
    assert_eq!(link.owed(), 0, "the BYE dropped what was owed");

    let (link, master_ino) = behind();
    link.enqueue_dropping(LinkOp::Bye { keep: Vec::new() });
    assert_eq!(
        seen(link.drain()),
        [("REGISTER", 70, master_ino)],
        "control: the full queue's ordinary admission takes no BYE"
    );
}

/// A quit's BYE names the master of each session it carries ([`bye`]), which the keeper keeps
/// past it (`aterm_keeper::core::KeeperCore::bye_keeping`).
///
/// NEGATIVE CONTROL: a quit that carries nothing sends the plain BYE, and a session the link
/// never registered adds nothing to it.
#[cfg(unix)]
#[test]
fn a_quits_bye_names_the_masters_it_carries() {
    let keeps = |ops: Vec<LinkOp>| -> Vec<Vec<u64>> {
        ops.into_iter()
            .filter_map(|op| match op {
                LinkOp::Bye { keep } => Some(keep),
                _ => None,
            })
            .collect()
    };
    let link = TestLink::stalled(7, 70);
    link.bye(&[7, 9]);
    assert_eq!(
        keeps(link.drain()),
        [vec![70]],
        "the carried session's master, and only a registered one's"
    );
    let link = TestLink::stalled(7, 70);
    link.bye(&[]);
    assert_eq!(
        keeps(link.drain()),
        [Vec::<u64>::new()],
        "control: a quit that carries nothing keeps nothing"
    );
}

/// A keeper in this process, stepped by the test: what a crash restore's boot connection,
/// link and old journal speak to. Apple only, as the keeper is: it takes a journal companion
/// only once libproc reads its descriptor as POSIX shared memory (`companion_fault`).
#[cfg(target_vendor = "apple")]
struct Keeper {
    _dir: aterm_tempfile::TempDir,
    sock: std::path::PathBuf,
    server: aterm_keeper::server::Server,
}

#[cfg(target_vendor = "apple")]
impl Keeper {
    fn new(for_what: &str) -> Self {
        let dir = crate::test_scratch::short_dir(for_what);
        let sock = dir.path().join("k.sock");
        let server = aterm_keeper::server::Server::bind(aterm_keeper::server::ServeConfig {
            socket: sock.clone(),
            identity: aterm_keeper::identity::Identity::same_uid(),
            relaunch: aterm_keeper::server::Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("a keeper");
        Self {
            _dir: dir,
            sock,
            server,
        }
    }

    /// Step the keeper until `done`, under the one-minute hang detector.
    #[track_caller]
    fn step_until(&mut self, what: &str, done: impl Fn(&aterm_keeper::server::Server) -> bool) {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !done(&self.server) {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            self.server.step(20).expect("a keeper turn");
        }
    }

    /// A link to this keeper on `booted` (or dialled), idle for good: only an op or the
    /// keeper's own frame wakes it, so nothing but the keeper's answer confirms a read.
    fn link(&self, booted: Option<KeeperClient>) -> KeeperLink {
        KeeperLink::start_idling(
            self.sock.clone(),
            aterm_keeper::identity::Identity::same_uid(),
            "t".into(),
            None,
            booted,
            true,
            Duration::from_secs(3_600),
        )
        .expect("a link")
    }

    /// The dead window's custody of `sh`'s master, as the keeper keeps it: a claim of an
    /// older connection of this process, with its journal region beside it (the old journal
    /// an offer of it carries). The link that holds that claim, and the master's rdev.
    fn old_claim(&mut self, sh: &aterm_pty::SpawnedShell, birth: u64) -> (KeeperLink, u64) {
        use std::os::unix::fs::MetadataExt as _;
        let older = self.link(None);
        let master = owned_dup(sh.master);
        let rdev = std::fs::File::from(master.try_clone().expect("dup"))
            .metadata()
            .expect("stat the master")
            .rdev();
        assert!(older.enqueue(LinkOp::Register {
            master,
            header: MasterHeader {
                rdev,
                ..header_for(sh.pid, birth)
            },
            tag: tag(),
        }));
        let (_region, custody) = journal_region();
        assert!(older.enqueue(LinkOp::Companion {
            rdev,
            region: custody,
            len: aterm_uds::shm::PAGE as u64,
            receipt: None,
        }));
        self.step_until("the keeper holds the old journal", |s| {
            s.holds(rdev) && old_journal(s, rdev).is_some()
        });
        (older, rdev)
    }

    /// A window's boot connection, its HELLO answered (the test hands it the offer).
    fn boot(&mut self) -> KeeperClient {
        let sock = self.sock.clone();
        let hello = std::thread::spawn(move || {
            let c = KeeperClient::connect(
                &sock,
                &aterm_keeper::identity::Identity::same_uid(),
                aterm_spec::wait::MUST_HAPPEN,
            )
            .expect("dial");
            let offers = c.hello_app_marked("t", None, true).expect("the WELCOME");
            assert!(offers.is_empty(), "premise: the claim is not orphaned");
            c
        });
        self.step_until("the boot HELLO is answered", |_| hello.is_finished());
        hello.join().expect("the boot HELLO")
    }
}

/// A journal region a window would make: read-only custody of a fresh shared-memory object.
#[cfg(target_vendor = "apple")]
fn journal_region() -> (aterm_uds::shm::Region, std::os::fd::OwnedFd) {
    // SAFETY: nothing modifies, resizes or invalidates the object: the returned `Region` stays
    // inert (no test writes through it), the keeper only holds the custody descriptor, and
    // the window's restore at most maps it read-only (`map_readonly`) to read a blank page.
    unsafe { aterm_uds::shm::Region::create(aterm_uds::shm::PAGE) }.expect("a region")
}

/// The region an offer of `rdev` would carry now.
#[cfg(target_vendor = "apple")]
fn old_journal(server: &aterm_keeper::server::Server, rdev: u64) -> Option<u64> {
    server.core().companion_view(rdev).and_then(|v| v.offered)
}

/// A duplicate of `fd`, a live descriptor the test holds.
#[cfg(target_vendor = "apple")]
fn owned_dup(fd: i32) -> std::os::fd::OwnedFd {
    // SAFETY: `fd` is a live descriptor the test holds for this call.
    unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .expect("dup")
}

/// The boot HELLO's offer of `sh`'s master at `rdev`, its shell born at `birth`, with the old
/// journal beside it.
#[cfg(target_vendor = "apple")]
fn offer_of(sh: &aterm_pty::SpawnedShell, rdev: u64, birth: u64) -> Offered {
    let (_region, custody) = journal_region();
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?2004h");
    Offered {
        master: owned_dup(sh.master),
        header: MasterHeader {
            rdev,
            ..header_for(sh.pid, birth)
        },
        tag: tag(),
        meta: crate::seamless::keeper_meta(&t).expect("meta"),
        companion: Some(aterm_keeper::client::OfferedCompanion {
            region: custody,
            kind: aterm_keeper::wire::COMPANION_JOURNAL,
            len: aterm_uds::shm::PAGE as u64,
        }),
        attempts: 0,
        kept: false,
    }
}

/// A RELEASE written behind every op queued before it: what the keeper has unread when the
/// test asserts the fence still holds.
#[cfg(target_vendor = "apple")]
fn written_behind(link: &KeeperLink) {
    assert!(
        link.send_and_wait(
            LinkOp::Release { rdev: u64::MAX - 1 },
            aterm_spec::wait::MUST_HAPPEN
        ),
        "control: the ops queued before are written"
    );
}

/// F01, a refused offer: the window refuses the old journal's offer (its shell's pid now
/// names another process) and releases it on the boot connection, and its fence, which no
/// session's journal will ever lift, lifts once the keeper answers the SYNC behind that
/// RELEASE, not while the keeper holds the connection unread.
#[cfg(target_vendor = "apple")]
#[test]
fn a_refused_offer_lifts_the_fence_once_the_keeper_reads_its_release() {
    let mut keeper = Keeper::new("refused offer");
    let (sh, birth) = live_pty();
    let (older, rdev) = keeper.old_claim(&sh, birth);
    let boot = keeper.boot();
    let mut recovery = Recovery::default();
    let (cohort, _) = admit_offers(
        &boot,
        vec![offer_of(&sh, rdev, birth + 1)],
        &mut recovery,
        None,
    );
    assert_eq!(
        recovery.refused,
        [(3, "its shell's pid now names another process")]
    );
    let admission = RecoveryCountAdmission::from_retained(cohort);
    let link = keeper.link(Some(boot));
    written_behind(&link);
    assert!(
        keeper.server.holds(rdev),
        "premise: the keeper read nothing"
    );
    assert!(!admission.ready(), "a written RELEASE lifts no fence");
    keeper.step_until("the keeper's answer lifts the fence", |_| admission.ready());
    assert!(
        !keeper.server.holds(rdev),
        "the keeper closed the old master and its journal"
    );
    link.shutdown();
    older.shutdown();
    end(&sh);
}

/// Quit's RELEASEs reach the keeper before its BYE: once [`release_at_quit`] has returned the
/// link has written every one, the last waited on, and the keeper closes its copies of the
/// masters as it reads them, with no BYE sent, so no master's last close waits for this process
/// to exit. A session the keeper never held is passed over.
#[cfg(target_vendor = "apple")]
#[test]
fn quits_releases_are_written_before_its_bye_and_the_keeper_closes_on_them() {
    use std::os::unix::fs::MetadataExt as _;
    let mut keeper = Keeper::new("quit release");
    let shells = [live_pty(), live_pty()];
    let state = Mutex::new(State {
        phase: Phase::Live(Arc::new(keeper.link(None))),
        buffered: Vec::new(),
        owed: VecDeque::new(),
        tracked: BTreeMap::new(),
        pump: false,
    });
    let mut rdevs = Vec::new();
    for (id, (sh, birth)) in (10..).zip(&shells) {
        let master = owned_dup(sh.master);
        let rdev = std::fs::File::from(master.try_clone().expect("dup"))
            .metadata()
            .expect("stat the master")
            .rdev();
        register_in(
            &mut state.lock().expect("state"),
            id,
            Tracked {
                rdev,
                prior_claim: false,
                term: Weak::new(),
                fabric: Weak::new(),
                last: None,
            },
            LinkOp::Register {
                master,
                header: MasterHeader {
                    rdev,
                    local_id: id,
                    ..header_for(sh.pid, *birth)
                },
                tag: tag(),
            },
            LinkOp::CompanionRelease {
                rdev,
                receipt: None,
            },
        );
        rdevs.push(rdev);
    }
    keeper.step_until("the keeper holds both masters", |s| {
        rdevs.iter().all(|rdev| s.holds(*rdev))
    });
    let written = std::thread::scope(|scope| {
        let quit = scope
            .spawn(|| release_at_quit_over(&state, &[10, 99, 11], aterm_spec::wait::MUST_HAPPEN));
        keeper.step_until("the quit's RELEASEs are written", |_| quit.is_finished());
        quit.join().expect("the quit's release")
    });
    assert!(written, "the last RELEASE was written");
    keeper.step_until("the keeper closes both masters with no BYE", |s| {
        rdevs.iter().all(|rdev| !s.holds(*rdev))
    });
    let mut st = state.lock().expect("state");
    assert!(st.tracked.is_empty() && st.owed.is_empty());
    if let Some(link) = quit_link_in(&mut st).and_then(Arc::into_inner) {
        link.shutdown();
    }
    for (sh, _) in &shells {
        end(sh);
    }
}

/// A boot offer a quit kept (the Mac Setup tab's installer, which aterm's quit carried:
/// `Offered::kept`) is adopted, but as no crash's: `Recovery::shells`, which the launch's
/// "aterm stopped unexpectedly" notice counts and whose End recovered sessions hangs each up,
/// leaves it out, and the recovery census counts no restore of it.
///
/// NEGATIVE CONTROL: the same offer from a crash is a recovered shell the notice counts.
#[cfg(target_vendor = "apple")]
#[test]
fn a_kept_offer_is_adopted_as_no_crash_s() {
    for kept in [true, false] {
        let mut keeper = Keeper::new("kept offer");
        let (sh, birth) = live_pty();
        let (older, rdev) = keeper.old_claim(&sh, birth);
        let boot = keeper.boot();
        let mut recovery = Recovery::default();
        let offer = Offered {
            kept,
            ..offer_of(&sh, rdev, birth)
        };
        let _ = admit_offers(&boot, vec![offer], &mut recovery, None);
        assert_eq!(
            recovery.adopted.len(),
            1,
            "kept={kept}: the offer is adopted"
        );
        if kept {
            assert!(
                recovery.shells.is_empty(),
                "a carried session is no crash's recovery: {:?}",
                recovery.shells
            );
            assert_eq!(recovery.carried, 1);
            assert!(recovery.ledger().is_none(), "nor the census's");
        } else {
            assert_eq!(
                recovery.shells,
                [sh.pid],
                "CONTROL: a crash's orphan is a recovered shell"
            );
            assert_eq!(recovery.carried, 0);
        }
        let mut adopted = recovery.adopted.pop().expect("adopted");
        if let Some(claim) = adopted.keeper.take() {
            claim.discharge();
        }
        drop(adopted);
        drop(boot);
        older.shutdown();
        end(&sh);
    }
}

/// aterm's quit carries a session's program only once the keeper holds its master
/// ([`carries_at_quit`]): the link's connection to a keeper that keeps what a BYE names
/// carried its REGISTER, which the keeper reads before the BYE.
///
/// NEGATIVE CONTROL: the same session while its REGISTER is still owed behind a full queue
/// (which a BYE drops), and a session never registered, are not carried, though the keeper
/// keeps what a BYE names.
#[cfg(target_vendor = "apple")]
#[test]
fn a_quit_carries_a_session_only_once_the_keeper_holds_its_master() {
    use std::os::unix::fs::MetadataExt as _;
    let mut keeper = Keeper::new("carries at quit");
    let (sh, birth) = live_pty();
    let boot = keeper.boot();
    let master = owned_dup(sh.master);
    let rdev = std::fs::File::from(master.try_clone().expect("dup"))
        .metadata()
        .expect("stat the master")
        .rdev();
    let mut state = State {
        phase: Phase::Live(Arc::new(keeper.link(Some(boot)))),
        buffered: Vec::new(),
        owed: VecDeque::new(),
        tracked: BTreeMap::new(),
        pump: false,
    };
    let Phase::Live(link) = &state.phase else {
        unreachable!("the test's link is live")
    };
    assert!(
        link.keeper_keeps(),
        "premise: this keeper keeps what a BYE names"
    );
    state.tracked.insert(
        7,
        Tracked {
            rdev,
            prior_claim: false,
            term: Weak::new(),
            fabric: Weak::new(),
            last: None,
        },
    );
    state.owed.push_back((
        Some(7),
        LinkOp::Register {
            master,
            header: MasterHeader {
                rdev,
                ..header_for(sh.pid, birth)
            },
            tag: tag(),
        },
    ));
    assert!(
        !carries_in(&state, 7),
        "CONTROL: a REGISTER still owed is no master in the keeper's custody"
    );
    let (id, op) = state.owed.pop_front().expect("owed");
    send_in(&mut state, id, op);
    keeper.step_until("the keeper holds the master", |server| server.holds(rdev));
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !carries_in(&state, 7) {
        assert!(
            std::time::Instant::now() < deadline,
            "the link never said the keeper holds the master it wrote"
        );
        aterm_uds::deadline::poll_yield();
    }
    assert!(
        !carries_in(&state, 8),
        "CONTROL: a session never registered"
    );
    let Phase::Live(link) = std::mem::replace(&mut state.phase, Phase::Off) else {
        unreachable!("the test's link is live")
    };
    Arc::into_inner(link).expect("the one link").shutdown();
    end(&sh);
}

/// F01, an adoption never placed: the window admits the offer, but its session never comes
/// to be, so the adoption is dropped, and its armed claim's drop retires the old journal
/// through the link; the fence lifts once the keeper answers the SYNC behind that RELEASE.
#[cfg(target_vendor = "apple")]
#[test]
fn an_adoption_never_placed_lifts_the_fence_once_the_keeper_reads_its_release() {
    let mut keeper = Keeper::new("unplaced adoption");
    let (sh, birth) = live_pty();
    let (older, rdev) = keeper.old_claim(&sh, birth);
    let boot = keeper.boot();
    let mut recovery = Recovery::default();
    let (cohort, _) = admit_offers(&boot, vec![offer_of(&sh, rdev, birth)], &mut recovery, None);
    let adopted = recovery.adopted.pop().expect("the offer is admitted");
    assert!(
        adopted
            .keeper
            .as_ref()
            .is_some_and(|claim| claim.recovery_count().is_some()),
        "premise: its claim holds the old journal's observer"
    );
    let admission = RecoveryCountAdmission::from_retained(cohort);
    let state = Arc::new(Mutex::new(State {
        phase: Phase::Live(Arc::new(keeper.link(Some(boot)))),
        buffered: Vec::new(),
        owed: VecDeque::new(),
        tracked: BTreeMap::new(),
        pump: false,
    }));
    // The claim's own drop, over this test's link in place of the process's.
    claims_release_to(Some(Arc::clone(&state)));
    drop(adopted);
    claims_release_to(None);
    assert!(
        released_unclaimed(rdev),
        "control: the claim's drop released the master"
    );
    let Phase::Live(link) = std::mem::replace(&mut state.lock().expect("state").phase, Phase::Off)
    else {
        unreachable!("the test's link is live")
    };
    written_behind(&link);
    assert!(
        keeper.server.holds(rdev),
        "premise: the keeper read nothing"
    );
    assert!(!admission.ready(), "a written RELEASE lifts no fence");
    keeper.step_until("the keeper's answer lifts the fence", |_| admission.ready());
    assert!(
        !keeper.server.holds(rdev),
        "the keeper closed the old master and its journal"
    );
    Arc::into_inner(link).expect("the one link").shutdown();
    older.shutdown();
    end(&sh);
}

/// F01, an adopted session with no journal (`SessionJournal::create` failed, so the
/// registration has no region's custody to send): its COMPANION_RELEASE tells the keeper to
/// drop the old journal, and the fence lifts once the keeper answers the SYNC behind it.
#[cfg(target_vendor = "apple")]
#[test]
fn a_session_with_no_journal_lifts_the_fence_once_the_keeper_drops_the_old_one() {
    let mut keeper = Keeper::new("journal-less adoption");
    let (sh, birth) = live_pty();
    let (older, rdev) = keeper.old_claim(&sh, birth);
    let boot = keeper.boot();
    let mut recovery = Recovery::default();
    let (cohort, _) = admit_offers(&boot, vec![offer_of(&sh, rdev, birth)], &mut recovery, None);
    let mut adopted = recovery.adopted.pop().expect("the offer is admitted");
    let claim = adopted.keeper.take().expect("its release obligation");
    let admission = RecoveryCountAdmission::from_retained(cohort);
    let link = keeper.link(Some(boot));
    let mut state = State {
        phase: Phase::Live(Arc::new(link)),
        buffered: Vec::new(),
        owed: VecDeque::new(),
        tracked: BTreeMap::new(),
        pump: false,
    };
    // What `register` sends for the placed session ([`register_unix`]), its journal off.
    register_in(
        &mut state,
        adopted.local_id,
        Tracked {
            rdev,
            prior_claim: true,
            term: Weak::new(),
            fabric: Weak::new(),
            last: None,
        },
        LinkOp::Register {
            master: owned_dup(sh.master),
            header: MasterHeader {
                rdev,
                ..header_for(sh.pid, birth)
            },
            tag: tag(),
        },
        journal_op(rdev, None, claim.recovery_count()),
    );
    claim.discharge();
    let Phase::Live(link) = std::mem::replace(&mut state.phase, Phase::Off) else {
        unreachable!("the test's link is live")
    };
    written_behind(&link);
    assert!(
        old_journal(&keeper.server, rdev).is_some(),
        "premise: the keeper read nothing, and offers the old journal still"
    );
    assert!(
        !admission.ready(),
        "a written COMPANION_RELEASE lifts no fence"
    );
    keeper.step_until("the keeper's answer lifts the fence", |_| admission.ready());
    assert_eq!(
        old_journal(&keeper.server, rdev),
        None,
        "the keeper holds the master with no journal an offer of it could carry"
    );
    assert!(
        keeper.server.holds(rdev),
        "the session's master stays in custody"
    );
    drop(adopted);
    Arc::into_inner(link).expect("the one link").shutdown();
    older.shutdown();
    end(&sh);
}

/// F01, an adopted session closed (`release_in`), or whose journal stopped
/// (`release_companion_in`, `SessionJournal::stop`), before the link read the keeper's answer
/// to the SYNC behind the frame that settles its journal claim: that frame's observer goes on
/// to the frame that superseded it (the RELEASE, or the receipt-less COMPANION_RELEASE), and
/// the fence lifts once the keeper answers the SYNC behind that one. Every op is written
/// before the keeper reads any, as when the keeper is slow or the link is between
/// connections.
#[cfg(target_vendor = "apple")]
#[test]
fn a_session_gone_before_the_keeper_read_its_journal_frame_still_lifts_the_fence() {
    for (case, journaled) in [
        ("a journal-less session closed", false),
        ("a session's journal stopped", true),
    ] {
        let mut keeper = Keeper::new(case);
        let (sh, birth) = live_pty();
        let (older, rdev) = keeper.old_claim(&sh, birth);
        let boot = keeper.boot();
        let mut recovery = Recovery::default();
        let (cohort, _) =
            admit_offers(&boot, vec![offer_of(&sh, rdev, birth)], &mut recovery, None);
        let mut adopted = recovery.adopted.pop().expect("the offer is admitted");
        let claim = adopted.keeper.take().expect("its release obligation");
        let admission = RecoveryCountAdmission::from_retained(cohort);
        let mut state = State {
            phase: Phase::Live(Arc::new(keeper.link(Some(boot)))),
            buffered: Vec::new(),
            owed: VecDeque::new(),
            tracked: BTreeMap::new(),
            pump: false,
        };
        let (_region, custody) = journal_region();
        // What `register_unix` sends: the journal's first region with the receipt it binds to
        // the old journal's observer (`SessionJournal::bind_recovery_count`), or none.
        let journal = if journaled {
            let observer = claim.recovery_count().expect("the old journal's observer");
            journal_op(
                rdev,
                Some(crate::crash_restore::JournalCustody {
                    region: custody,
                    len: aterm_uds::shm::PAGE as u64,
                    receipt: Some(Arc::new(
                        aterm_keeper::client::CompanionReceipt::reporting_confirmation(observer),
                    )),
                }),
                None,
            )
        } else {
            journal_op(rdev, None, claim.recovery_count())
        };
        register_in(
            &mut state,
            adopted.local_id,
            Tracked {
                rdev,
                prior_claim: true,
                term: Weak::new(),
                fabric: Weak::new(),
                last: None,
            },
            LinkOp::Register {
                master: owned_dup(sh.master),
                header: MasterHeader {
                    rdev,
                    ..header_for(sh.pid, birth)
                },
                tag: tag(),
            },
            journal,
        );
        claim.discharge();
        if journaled {
            release_companion_in(&mut state, adopted.local_id);
        } else {
            release_in(&mut state, adopted.local_id);
        }
        let Phase::Live(link) = std::mem::replace(&mut state.phase, Phase::Off) else {
            unreachable!("the test's link is live")
        };
        written_behind(&link);
        assert!(
            old_journal(&keeper.server, rdev).is_some(),
            "{case}: premise: the keeper read nothing, and offers the old journal still"
        );
        assert!(!admission.ready(), "{case}: a written frame lifts no fence");
        keeper.step_until(case, |_| admission.ready());
        assert_eq!(
            old_journal(&keeper.server, rdev),
            None,
            "{case}: the keeper holds no journal an offer of the master could carry"
        );
        assert_eq!(
            keeper.server.holds(rdev),
            journaled,
            "{case}: the master stays in custody only while its session lives"
        );
        drop(adopted);
        Arc::into_inner(link).expect("the one link").shutdown();
        older.shutdown();
        end(&sh);
    }
}
