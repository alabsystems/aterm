// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(target_os = "macos")]
#[test]
fn a_prime_exit_without_its_marker_does_not_authorize_removing_the_copy() {
    use std::os::unix::process::ExitStatusExt as _;
    let mut out = std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: b"own-copy\n".to_vec(),
        stderr: Vec::new(),
    };
    assert!(prime_ran(&out, "own-copy"));
    assert!(!prime_ran(&out, "another-copy"));
    out.stdout.clear();
    assert!(
        !prime_ran(&out, "own-copy"),
        "a successful exit alone proves nothing"
    );
    out.stdout = b"own-copy\n".to_vec();
    out.status = std::process::ExitStatus::from_raw(libc::SIGKILL);
    assert!(
        !prime_ran(&out, "own-copy"),
        "an unsettled child keeps its copy"
    );
}

/// A prime whose program outlived its ceiling, inside the verb (its [`PRIME_UNSETTLED`] exit)
/// or around it (the verb's own run timed out), did not settle: macOS's queue is backed up,
/// which says nothing of the grant. The verb's own line names the copy it kept. A verb that
/// could not run or exited otherwise failed, with its first line of complaint.
#[test]
fn a_timed_out_prime_is_not_settled_and_a_refusal_is_a_failure() {
    use aterm_uds::deadline::RunError;
    let exited = |code: i32, stderr: &str| {
        #[cfg(unix)]
        let status = std::os::unix::process::ExitStatusExt::from_raw(code << 8);
        #[cfg(windows)]
        let status = std::os::windows::process::ExitStatusExt::from_raw(code.unsigned_abs());
        Ok(std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    };
    assert_eq!(refresh_outcome(exited(0, "")), Ok(()));
    let kept = "aterm pkg __devtools-prime: not settled: macOS's queue is backed up; kept \
                /var/folders/l5/abcdefghijklmnopqrstuvwxyz01/T/aterm-devtools-prime-\
                0123456789abcdef since aterm-devtools-0123456789abcdef never showed it ran";
    let unsettled = refresh_outcome(exited(i32::from(PRIME_UNSETTLED), &format!("{kept}\nmore")));
    assert_eq!(
        unsettled,
        Err(RefreshFailed::NotSettled(Some(kept.into()))),
        "the prime's program did not finish in time"
    );
    assert!(
        unsettled.is_err_and(|e| e.to_string().ends_with("never showed it ran")),
        "it names the copy it kept"
    );
    assert_eq!(
        refresh_outcome(exited(i32::from(PRIME_UNSETTLED), "")),
        Err(RefreshFailed::NotSettled(None))
    );
    assert_eq!(
        refresh_outcome(Err(RunError::TimedOut)),
        Err(RefreshFailed::NotSettled(None)),
        "the verb itself did not finish in time"
    );
    assert_eq!(
        refresh_outcome(exited(1, "aterm pkg __devtools-prime: copy: denied\nmore")),
        Err(RefreshFailed::Failed(
            "aterm pkg __devtools-prime: copy: denied".into()
        ))
    );
    assert_eq!(
        refresh_outcome(exited(1, "")),
        Err(RefreshFailed::Failed(
            "aterm pkg __devtools-prime exited Some(1)".into()
        ))
    );
    let spawn = std::io::Error::from(std::io::ErrorKind::NotFound);
    assert!(
        matches!(
            refresh_outcome(Err(RunError::Spawn(spawn))),
            Err(RefreshFailed::Failed(why)) if why.starts_with("aterm pkg __devtools-prime: could not start")
        ),
        "a verb that could not start is a failure"
    );
    assert_eq!(
        RefreshFailed::NotSettled(None).to_string(),
        "not settled: macOS's queue is backed up"
    );
    assert_eq!(RefreshFailed::Failed("why".into()).to_string(), "why");
}

#[test]
fn the_prime_runs_the_bundles_front_door_whatever_name_this_aterm_ran_under() {
    let bundle = std::path::Path::new(if cfg!(windows) {
        r"C:\Applications\aterm.app"
    } else {
        "/Applications/aterm.app"
    });
    let command = prime_command(bundle);
    let program = std::path::Path::new(command.get_program());
    assert_eq!(program, bundle.join("Contents/MacOS/aterm"));
    assert_eq!(program.file_name(), Some(std::ffi::OsStr::new("aterm")));
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    assert_eq!(args, ["pkg", PRIME_VERB]);
}

const ATERM: &str = "com.aterm.aterm";

fn facts(list: DevToolList, log: DevToolProbe) -> Facts {
    Facts {
        subject: Some(ATERM.to_string()),
        list,
        log,
        other_responsible: None,
        program_check: None,
    }
}

fn read(listing: DevToolListing, at: i64) -> DevToolList {
    DevToolList {
        listing,
        label: DevToolListLabel::Read,
        modified_at_s: (listing != DevToolListing::Absent).then_some(at),
        rows: Some(1),
    }
}

/// tccd's log is read only when it can change the verdict: a switch on since before this
/// process started leaves `log show` unrun and the verdict the switch's. NEGATIVE CONTROL:
/// a switch moved at or after the start, one switched off, an unknown start and an unread
/// list each read it.
#[test]
fn a_switch_on_since_before_the_start_leaves_tccds_log_unread() {
    let started = 1_000;
    let ran = std::cell::Cell::new(0);
    let log = || {
        ran.set(ran.get() + 1);
        DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest)
    };
    let on_before = read(DevToolListing::On, 999);
    let skipped = log_when_needed(&on_before, Some(started), log);
    assert_eq!(ran.get(), 0, "the log was read");
    assert_eq!(skipped.label, DevToolProbeLabel::NotNeeded);
    assert_eq!(facts(on_before, skipped).grant(), DevToolGrant::Granted);
    for (list, start) in [
        (read(DevToolListing::On, 1_000), Some(started)),
        (read(DevToolListing::On, 1_005), Some(started)),
        (read(DevToolListing::Off, 999), Some(started)),
        (read(DevToolListing::On, 999), None),
        (
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            Some(started),
        ),
    ] {
        let before = ran.get();
        log_when_needed(&list, start, log);
        assert_eq!(
            ran.get(),
            before + 1,
            "{list:?} at {start:?} left the log unread"
        );
    }
}

fn tccd(state: DevToolState, not_allowed: u32, at_ms: i64) -> DevToolProbe {
    DevToolProbe {
        state,
        label: DevToolProbeLabel::Observed,
        requests: Some(not_allowed.max(1)),
        not_allowed: Some(not_allowed),
        answered_at_ms: Some(at_ms),
    }
}

/// The row's level per verdict: `ok` granted, `warn` not granted (with tccd's count and no
/// command that changes the list for the owner), and `note`, never `ok`, when nothing could
/// be read.
#[test]
fn the_row_says_the_state_the_cost_and_the_fix() {
    let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
    let on = row(&facts(read(DevToolListing::On, 100), quiet)).expect("a row");
    assert_eq!(on.level, Level::Ok);
    let absent = row(&facts(
        read(DevToolListing::Absent, 0),
        tccd(DevToolState::Unlisted, 96, 1),
    ))
    .expect("a row");
    assert_eq!(absent.level, Level::Warn);
    assert!(absent.text.contains("96"), "{}", absent.text);
    for absent_word in ["tccutil", "sqlite3", "sudo"] {
        assert!(!absent.text.contains(absent_word), "{absent_word}");
    }
    let off = row(&facts(read(DevToolListing::Off, 100), quiet)).expect("a row");
    assert_eq!(off.level, Level::Warn);
    assert!(
        off.text.contains("fix: switch on aterm's row") && off.text.contains(OPEN_PANE),
        "{}",
        off.text
    );
    for not_said in ["drag", LIST_TERMINAL] {
        assert!(!off.text.contains(not_said), "{not_said}: {}", off.text);
    }
    let shadowed = row(&facts(
        read(DevToolListing::On, 100),
        tccd(DevToolState::Unlisted, 1, 100_001),
    ))
    .expect("a row");
    assert_eq!(shadowed.level, Level::Warn, "{}", shadowed.text);
    assert!(
        shadowed.text.contains("nothing to add"),
        "{}",
        shadowed.text
    );
    for not_said in ["drag", "fix: ", "aterm then has macOS check it again"] {
        assert!(
            !shadowed.text.contains(not_said),
            "{not_said}: {}",
            shadowed.text
        );
    }
    let allowed = row(&facts(
        DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
        tccd(DevToolState::Allowed, 0, 1),
    ))
    .expect("a row");
    assert_eq!(allowed.level, Level::Ok);
    let unknown = row(&facts(
        DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
        DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
    ))
    .expect("a row");
    assert_eq!(unknown.level, Level::Note);
    for label in [
        DevToolListLabel::Pending,
        DevToolListLabel::Read,
        DevToolListLabel::NoFullDiskAccess,
        DevToolListLabel::OpenFailed,
        DevToolListLabel::ToolFailed,
        DevToolListLabel::FormatUnrecognized,
        DevToolListLabel::TimedOut,
        DevToolListLabel::SpawnFailed,
        DevToolListLabel::OutputUnreadable,
        DevToolListLabel::RefusedDisabled,
        DevToolListLabel::RefusedObserverOff,
        DevToolListLabel::RefusedOutOfBundle,
        DevToolListLabel::RefusedNoExe,
        DevToolListLabel::RefusedNoSubject,
    ] {
        let reason = list_reason(label);
        for absent_word in ["tccutil", "sqlite3", "sudo"] {
            assert!(
                !reason.contains(absent_word),
                "{label:?}: {reason:?} names {absent_word}"
            );
        }
    }
}

/// The row counts this user's programs macOS's program check holds now, word for word,
/// under every verdict.
#[test]
fn the_devtools_row_counts_the_live_wait() {
    use aterm_uds::process::HeldNow;
    const NOW: &str = "now 2 programs of this user wait for macOS's program check, the \
                       oldest started 9 h 59 m ago";
    const LOOK: &str = "look at aterm's row in the Developer Tools list";
    let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
    let two = HeldNow {
        count: 2,
        oldest_ms: 35_940_000,
    };
    let one = HeldNow {
        count: 1,
        oldest_ms: 20_000,
    };
    let held = |base: &Facts, held: Option<HeldNow>| {
        row(&Facts {
            program_check: held,
            ..base.clone()
        })
        .expect("a row")
    };
    let off = facts(read(DevToolListing::Off, 100), quiet);
    let on = facts(read(DevToolListing::On, 100), quiet);
    let unread = facts(
        DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
        DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
    );
    for base in [&off, &on, &unread] {
        let plain = row(base).expect("a row");
        for none in [None, Some(HeldNow::default())] {
            assert_eq!(held(base, none), plain, "nothing held, no clause");
        }
    }

    let plain = row(&off).expect("a row");
    let warn = held(&off, Some(two));
    assert_eq!(warn.level, Level::Warn);
    assert_eq!(
        warn.text,
        plain.text.replacen(
            "timing tests flake \u{2014}",
            &format!("timing tests flake; {NOW} \u{2014}"),
            1
        ),
        "the count after tccd's"
    );

    let plain = row(&on).expect("a row");
    assert_eq!(plain.level, Level::Ok);
    let listed = held(&on, Some(two));
    assert_eq!(listed.level, Level::Note, "{}", listed.text);
    assert_eq!(
        listed.text,
        format!(
            "{}; {NOW}; if aterm started them, macOS still checks the programs aterm \
             starts: {LOOK}",
            plain.text
        )
    );
    assert_eq!(
        held(&on, Some(one)).text,
        format!(
            "{}; now 1 program of this user waits for macOS's program check; it started \
             20 s ago; if aterm started it, macOS still checks the programs aterm starts: \
             {LOOK}",
            plain.text
        )
    );

    let plain = row(&unread).expect("a row");
    let why = plain
        .text
        .strip_prefix("not measured (")
        .and_then(|t| t.strip_suffix(')'))
        .expect("the unmeasured words");
    let unknown = held(&unread, Some(two));
    assert_eq!(unknown.level, Level::Note);
    assert_eq!(
        unknown.text,
        format!("{NOW}; whether aterm is listed is not measured ({why})")
    );
}

/// Bare doctor's devtools line is one state and one action, short enough for one row,
/// for every verdict; a granted list has none. NEGATIVE CONTROL: the full row, which
/// `--verbose` prints, is longer than a brief line holds.
#[test]
fn the_brief_line_is_one_action_that_fits_one_row() {
    let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
    let on = facts(read(DevToolListing::On, 100), quiet);
    assert_eq!(brief(&on), None, "granted: nothing to do");
    let off = facts(read(DevToolListing::Off, 100), quiet);
    let absent = facts(
        read(DevToolListing::Absent, 0),
        tccd(DevToolState::Unlisted, 96, 1),
    );
    let shadowed = facts(
        read(DevToolListing::On, 100),
        tccd(DevToolState::Unlisted, 1, 100_001),
    );
    let unread = facts(
        DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
        DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
    );
    let elsewhere = Facts {
        other_responsible: Some("tart-guest-agent".to_owned()),
        ..unread.clone()
    };
    for (label, f, want) in [
        ("off", &off, "fix: switch it on"),
        ("absent", &absent, "fix: add aterm there"),
        ("listed but scanned", &shadowed, "fix: remove aterm's row"),
        ("unread", &unread, "fix: grant aterm Full Disk Access"),
        (
            "another app's shell",
            &elsewhere,
            "fix: run `aterm doctor` in an aterm tab",
        ),
    ] {
        let line = brief(f).unwrap_or_else(|| panic!("{label}: a line"));
        assert!(line.chars().count() <= BRIEF_CHARS, "{label}: {line}");
        assert!(line.contains(want), "{label}: {line}");
        assert_eq!(
            line.matches("fix:").count(),
            1,
            "{label}: one action: {line}"
        );
        assert!(!line.contains('\n'), "{label}: {line}");
    }
    let whole = row(&off).expect("a row").text;
    assert!(whole.chars().count() > BRIEF_CHARS, "{whole}");
}

/// Verify's word for an app's row: on is yes; off and no row are no; a list not read whole
/// is unknown.
#[test]
fn a_listing_reads_yes_only_for_a_row_switched_on() {
    for (list, want) in [
        (read(DevToolListing::On, 1), Listed::Yes),
        (read(DevToolListing::Off, 1), Listed::No),
        (read(DevToolListing::Absent, 0), Listed::No),
        (read(DevToolListing::Unknown, 0), Listed::Unknown),
        (
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            Listed::Unknown,
        ),
        (
            DevToolList::refused(DevToolListLabel::RefusedOutOfBundle),
            Listed::Unknown,
        ),
        (DevToolList::pending(), Listed::Unknown),
    ] {
        assert_eq!(Listed::of(&list), want, "{list:?}");
    }
    assert_eq!(
        [Listed::Yes, Listed::No, Listed::Unknown].map(Listed::word),
        ["yes", "no", "unknown"]
    );
}

/// A program outside a `.app` is refused the read; its word is then the reading of the
/// instance it runs in, made inside that instance's bundle and asked once, when that reading
/// is about the same bundle: on is yes, off and no row are no. A list read here, or a read
/// refused for any other reason, is the word, and the instance is never asked. NEGATIVE
/// CONTROLS: a reading about another bundle, one that is `unknown`, or none at all leaves it
/// unknown; ask the instance whatever the read here was, and a read list asks it.
#[test]
fn a_read_refused_out_of_bundle_takes_the_instances_reading_of_the_same_bundle() {
    let refused = DevToolList::refused(DevToolListLabel::RefusedOutOfBundle);
    let asked = std::cell::Cell::new(0);
    let window = |subject: &'static str, list: &'static str| {
        let asked = &asked;
        move || {
            asked.set(asked.get() + 1);
            Some(WindowList {
                subject: subject.to_owned(),
                list: list.to_owned(),
            })
        }
    };
    let id = "com.aterm.aterm";
    for (list, want) in [
        ("on", Listed::Yes),
        ("off", Listed::No),
        ("absent", Listed::No),
        ("unknown", Listed::Unknown),
    ] {
        asked.set(0);
        assert_eq!(listed_word(&refused, id, window(id, list)), want, "{list}");
        assert_eq!(asked.get(), 1, "{list}: asked once");
    }
    assert_eq!(
        listed_word(&refused, id, window("com.apple.Terminal", "on")),
        Listed::Unknown,
        "another bundle's reading"
    );
    assert_eq!(listed_word(&refused, id, || None), Listed::Unknown);
    asked.set(0);
    assert_eq!(
        listed_word(&read(DevToolListing::Off, 1), id, window(id, "on")),
        Listed::No,
        "the list read here"
    );
    assert_eq!(
        listed_word(
            &DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
            id,
            window(id, "on")
        ),
        Listed::Unknown,
        "a read refused for another reason"
    );
    assert_eq!(
        asked.get(),
        0,
        "the instance asked though the read was not refused out of bundle"
    );
}

/// The hand-over acts only on `not_granted`: an empty list gets Terminal listed first so
/// the pane exists, and the pane opens; `granted` and `unknown` do nothing at all.
#[test]
fn the_hand_over_opens_the_pane_only_for_not_granted() {
    let quiet = DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest);
    let calls = std::cell::RefCell::new(Vec::new());
    let list = || {
        calls.borrow_mut().push("spctl");
        Ok(())
    };
    let open = || {
        calls.borrow_mut().push("open");
        Ok(())
    };
    let reach = Reach {
        list_terminal: &list,
        open_pane: &open,
    };
    let mut empty = read(DevToolListing::Absent, 0);
    empty.rows = Some(0);
    let handled = hand_to_owner(&facts(empty, quiet), &reach);
    assert_eq!(
        handled,
        Handled::Opened {
            listed_terminal: Some(Ok(())),
            step: Step::Add,
        }
    );
    assert_eq!(calls.take(), vec!["spctl", "open"]);
    let words = handled_words(&handled);
    assert!(
        words.starts_with("ran `spctl developer-mode enable-terminal` so the pane appears")
            && words.contains("leave it off"),
        "{words}"
    );
    let handled = hand_to_owner(&facts(read(DevToolListing::Absent, 0), quiet), &reach);
    assert_eq!(
        handled,
        Handled::Opened {
            listed_terminal: None,
            step: Step::Add,
        }
    );
    assert_eq!(calls.take(), vec!["open"]);
    let words = handled_words(&handled);
    assert!(
        words.starts_with(&format!("opened {PANE}: drag aterm")),
        "{words}"
    );
    for said in [PLUS_ROUTE, CHECK_SWITCH] {
        assert!(words.contains(said), "{said}: {words}");
    }
    assert!(!words.contains("Terminal"), "{words}");
    let handled = hand_to_owner(&facts(read(DevToolListing::Off, 1), quiet), &reach);
    assert_eq!(
        handled,
        Handled::Opened {
            listed_terminal: None,
            step: Step::SwitchOn,
        }
    );
    assert_eq!(calls.take(), vec!["open"]);
    let words = handled_words(&handled);
    assert!(words.contains("switch on aterm's row"), "{words}");
    assert!(!words.contains("drag"), "{words}");
    let shadowed = facts(
        read(DevToolListing::On, 1),
        tccd(DevToolState::Unlisted, 1, 100_001),
    );
    let handled = hand_to_owner(&shadowed, &reach);
    assert_eq!(
        handled,
        Handled::Opened {
            listed_terminal: None,
            step: Step::ListedOn,
        }
    );
    assert_eq!(calls.take(), vec!["open"]);
    let words = handled_words(&handled);
    assert!(
        words.contains("nothing to add") && !words.contains("drag"),
        "{words}"
    );
    assert_eq!(
        hand_to_owner(&facts(read(DevToolListing::On, 1), quiet), &reach),
        Handled::Granted
    );
    let unknown = facts(
        DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
        DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
    );
    assert!(matches!(hand_to_owner(&unknown, &reach), Handled::Left(_)));
    assert!(calls.take().is_empty(), "granted and unknown run nothing");
}

#[test]
fn the_marker_is_one_version_and_one_time() {
    assert_eq!(
        parse_opened("0.104.0 1792020637\n"),
        Some(("0.104.0".to_string(), 1_792_020_637))
    );
    for bad in ["", "0.104.0", "0.104.0 x", "0.104.0 1 2"] {
        assert_eq!(parse_opened(bad), None, "{bad:?}");
    }
    let at = 1_792_020_637;
    assert_eq!(opened_ago(at, at + 59, 60), Some(59));
    assert_eq!(opened_ago(at, at + 60, 60), None);
    assert_eq!(
        opened_ago(at, at - 600, REOPEN_AFTER_S),
        Some(0),
        "small skew"
    );
    assert_eq!(opened_ago(at, at - 2 * 3_600, REOPEN_AFTER_S), None);
}

/// Each page keeps its own pair of markers, and the Developer Tools files keep their names.
#[test]
fn each_page_keeps_its_own_markers() {
    assert_eq!(DEVELOPER_TOOLS.opened, "devtools-pane-opened");
    assert_eq!(DEVELOPER_TOOLS.declined, "devtools-declined");
    let names = [
        DEVELOPER_TOOLS.opened,
        DEVELOPER_TOOLS.declined,
        FULL_DISK_ACCESS.opened,
        FULL_DISK_ACCESS.declined,
        ACCESSIBILITY.opened,
        ACCESSIBILITY.declined,
    ];
    for (i, a) in names.iter().enumerate() {
        for b in &names[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

/// A decline holds 30 days, or 90 after an earlier one within 180 days, never for good; a
/// stamp from a clock since corrected keeps nothing quiet, and an unparsable line is skipped.
#[test]
fn a_decline_holds_a_month_then_three_and_never_for_good() {
    const DAY: u64 = 86_400;
    let at = 1_792_092_226;
    let once = with_decline("", "0.104.0", at);
    assert_eq!(once, format!("0.104.0 {at}\n"));
    let held = decline_holding(&once, at + DAY).expect("a fresh decline holds");
    assert_eq!(
        held,
        Declined {
            version: "0.104.0".to_string(),
            at,
            until: at + 30 * DAY,
        }
    );
    assert_eq!(
        decline_holding(&once, at + 30 * DAY),
        None,
        "a month, then over"
    );

    let again = at + 40 * DAY;
    let twice = with_decline(&once, "0.105.0", again);
    assert_eq!(twice.lines().count(), 2);
    let held = decline_holding(&twice, again + 89 * DAY).expect("a repeat holds longer");
    assert_eq!(
        (held.version.as_str(), held.until),
        ("0.105.0", again + 90 * DAY)
    );
    assert_eq!(decline_holding(&twice, again + 90 * DAY), None);

    let late = at + 200 * DAY;
    let lapsed = with_decline(&once, "0.110.0", late);
    assert_eq!(
        decline_holding(&lapsed, late).map(|held| held.until),
        Some(late + 30 * DAY)
    );

    let kept = with_decline(&format!("junk\n{twice}"), "0.106.0", again + DAY);
    assert_eq!(kept, format!("0.105.0 {again}\n0.106.0 {}\n", again + DAY));
    assert_eq!(decline_holding("junk\n0.104.0\n", at), None);
    assert_eq!(decline_holding("", at), None);

    assert_eq!(decline_holding(&once, at - 2 * 3_600), None);
    assert!(decline_holding(&once, at - 600).is_some());
}

/// A key written cold and one written warm for the same revision are one step; another
/// revision, bundle or Mac, or a version, is not. The newest decline is read whatever its
/// age, so a Not now holds for the rest of its revision.
#[test]
fn a_key_matches_its_own_step_warm_or_cold_and_the_newest_decline_is_read() {
    assert_eq!(revision_key(7), "r7");
    let warm = "r4@com.aterm.aterm@UUID";
    for (a, b, want) in [
        ("r4", "r4", true),
        ("r4", warm, true),
        (warm, "r4", true),
        ("r4@com.aterm.aterm", warm, true),
        (warm, warm, true),
        ("r40", "r4", false),
        ("r3", warm, false),
        ("r4@com.aterm.aterm.dev@UUID", warm, false),
        ("r4@com.aterm.aterm@OTHER", warm, false),
        ("0.110.0", "r4", false),
        ("", "r4", false),
    ] {
        assert_eq!(same_step(a, b), want, "{a} vs {b}");
    }
    let state = aterm_tempfile::tempdir().expect("tempdir");
    assert_eq!(FULL_DISK_ACCESS.last_declined_in(state.path()), None);
    let once = with_decline("", "0.110.0", 1_000);
    let twice = with_decline(&once, &revision_key(4), 5_000_000_000);
    std::fs::write(state.path().join(FULL_DISK_ACCESS.declined), twice).expect("write");
    assert_eq!(
        FULL_DISK_ACCESS.last_declined_in(state.path()),
        Some(("r4".to_string(), 5_000_000_000))
    );
    assert_eq!(DEVELOPER_TOOLS.last_declined_in(state.path()), None);
}

/// A page marker is replaced whole: each note reads back as the newest and nothing is left
/// beside it, and a write that cannot finish (into a folder that takes no new file, or one
/// whose flush fails) leaves the marker as it was, where a write in place would have
/// replaced it.
#[cfg(unix)]
#[test]
fn a_page_marker_is_replaced_whole_or_left_as_it_was() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    FULL_DISK_ACCESS
        .note_opened_in(&state, "r4", 1_000)
        .expect("noted");
    FULL_DISK_ACCESS
        .note_opened_in(&state, "r5", 2_000)
        .expect("noted again");
    assert_eq!(
        FULL_DISK_ACCESS.last_opened_in(&state),
        Some(("r5".to_owned(), 2_000))
    );
    DEVELOPER_TOOLS
        .note_declined_in(&state, "r4", 1_000)
        .expect("declined");
    DEVELOPER_TOOLS
        .note_declined_in(&state, "r5", 2_000)
        .expect("declined again");
    assert_eq!(
        DEVELOPER_TOOLS.last_declined_in(&state),
        Some(("r5".to_owned(), 2_000))
    );
    let mut names: Vec<_> = std::fs::read_dir(&state)
        .expect("the state folder")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["devtools-declined", "fda-pane-opened"],
        "nothing staged is left"
    );
    let text = |name: &str| std::fs::read_to_string(state.join(name)).expect("a marker");
    let before = (
        text(FULL_DISK_ACCESS.opened),
        text(DEVELOPER_TOOLS.declined),
    );
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500))
        .expect("a folder that takes no new file");
    let refused = (
        FULL_DISK_ACCESS.note_opened_in(&state, "r6", 3_000),
        DEVELOPER_TOOLS.note_declined_in(&state, "r6", 3_000),
    );
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("the folder writable again");
    assert!(refused.0.is_err() && refused.1.is_err(), "{refused:?}");
    assert_eq!(
        (
            text(FULL_DISK_ACCESS.opened),
            text(DEVELOPER_TOOLS.declined)
        ),
        before
    );
    let path = state.join(FULL_DISK_ACCESS.opened);
    let failed = write_marker_with(&path, "r7 4000\n", |_| {
        Err(std::io::Error::from_raw_os_error(5))
    });
    assert_eq!(failed.map_err(|e| e.raw_os_error()), Err(Some(5)));
    assert_eq!(text(FULL_DISK_ACCESS.opened), before.0, "a failed flush");
}

/// A revision's key reads back as its revision; a version, a key with more words, a signed
/// or overflowing number, and anything not `r` and digits read as none.
#[test]
fn a_revision_key_reads_back_and_nothing_else_does() {
    for revision in [0, 7, u64::MAX] {
        assert_eq!(revision_of_key(&revision_key(revision)), Some(revision));
    }
    for key in [
        "",
        "r",
        "7",
        "R7",
        "r+7",
        "r-7",
        "r7@id",
        "r7 ",
        "0.104.0",
        "r18446744073709551616",
    ] {
        assert_eq!(revision_of_key(key), None, "{key:?}");
    }
}

/// Each TCC identity keeps its own opening beside the others' (a network home's Macs, a dev
/// bundle beside the release): a note replaces only its own identity's line, and one written
/// before its words were read; a reader takes the newest line of its identity, or of any;
/// the oldest identity gives way past the bound, and a file of one line reads as before. The
/// marker stays the one line an older aterm reads, and that aterm's own note is kept beside
/// the others once a newer one writes.
#[test]
fn each_identity_keeps_its_own_opening() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path();
    let (a, b, dev) = (
        "r7@com.aterm.aterm@A",
        "r7@com.aterm.aterm@B",
        "r7@com.aterm.aterm.dev@A",
    );
    FULL_DISK_ACCESS
        .note_opened_beside_in(state, a, 1_000)
        .expect("noted");
    FULL_DISK_ACCESS
        .note_opened_beside_in(state, b, 2_000)
        .expect("noted");
    FULL_DISK_ACCESS
        .note_opened_beside_in(state, dev, 3_000)
        .expect("noted");
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, a),
        Some((a.to_owned(), 1_000)),
        "B's and the dev bundle's pages leave A's"
    );
    assert_eq!(
        FULL_DISK_ACCESS.last_opened_in(state),
        Some((dev.to_owned(), 3_000))
    );
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, "r7@com.aterm.aterm"),
        Some((b.to_owned(), 2_000)),
        "a key read before the Mac takes its bundle's newest line"
    );
    let read = |name: &str| std::fs::read_to_string(state.join(name)).expect("read");
    assert_eq!(
        parse_opened(&read(FULL_DISK_ACCESS.opened)),
        Some((dev.to_owned(), 3_000)),
        "the marker reads whole, as an older aterm reads it"
    );
    let older = "r7@com.aterm.aterm@D";
    FULL_DISK_ACCESS
        .note_opened_in(state, older, 3_500)
        .expect("an older aterm's note");
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, older),
        Some((older.to_owned(), 3_500))
    );
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, a),
        Some((a.to_owned(), 1_000)),
        "an older aterm's note leaves the identities' lines"
    );
    FULL_DISK_ACCESS
        .note_opened_beside_in(state, "r8@com.aterm.aterm@A", 4_000)
        .expect("noted");
    assert_eq!(
        read(&FULL_DISK_ACCESS.opened_by()),
        format!("{b} 2000\n{dev} 3000\n{older} 3500\nr8@com.aterm.aterm@A 4000\n"),
        "A's own line gives way, and the older aterm's is kept"
    );
    assert_eq!(read(FULL_DISK_ACCESS.opened), "r8@com.aterm.aterm@A 4000\n");
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, older),
        Some((older.to_owned(), 3_500))
    );
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, "r8@com.aterm.aterm@C"),
        None
    );
    FULL_DISK_ACCESS
        .note_opened_beside_in(state, "r9", 5_000)
        .expect("noted cold");
    assert_eq!(
        (
            read(FULL_DISK_ACCESS.opened),
            read(&FULL_DISK_ACCESS.opened_by())
        ),
        ("r9 5000\n".to_owned(), "r9 5000\n".to_owned()),
        "a key written cold names every identity"
    );
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, a),
        Some(("r9".to_owned(), 5_000)),
        "and reads back for each"
    );
    let many = (0..20).fold(String::new(), |text, mac| {
        with_opening(&text, &format!("r7@com.aterm.aterm@{mac}"), mac, false)
    });
    assert_eq!(many.lines().count(), OPENED_IDENTITIES);
    assert!(many.ends_with("r7@com.aterm.aterm@19 19\n"));
    assert!(!many.contains("@11 "), "the oldest gave way");
    assert_eq!(
        with_opening("junk\nr7@x@A 1\n", "r7@x@A", 2, false),
        "r7@x@A 2\n"
    );
    assert_eq!(
        with_opening("r7@x@B 1\nr7@x@B 1\n", "r7@x@A", 2, false),
        "r7@x@B 1\nr7@x@A 2\n",
        "a line met again is kept once"
    );
    for (a, b, want) in [
        ("r7@x@A", "r8@x@A", true),
        ("r7", "r8@x@A", true),
        ("r7@x", "r7@x@A", true),
        ("r7@x", "r7@y@A", false),
        ("r7@x@A", "r7@x@B", false),
        ("r7@x@A", "r7@y@A", false),
    ] {
        assert_eq!(same_identity(a, b), want, "{a} vs {b}");
    }
}

/// The window's Developer Tools opening keeps the marker's one plain word, which `aterm
/// doctor` and an older aterm compare whole, and writes its TCC identity beside it: that word
/// then names only the identity beside it, so another Mac's page (a network home, a Mac set up
/// with Migration Assistant) is not this one's. A plain word written alone (doctor's, an older
/// aterm's, with no file beside it) names every identity, as before.
#[test]
fn a_plain_marker_names_only_the_identity_written_beside_it() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path();
    let (a, b) = ("r7@com.aterm.aterm@A", "r7@com.aterm.aterm@B");
    DEVELOPER_TOOLS
        .note_opened_by_in(state, "r7", a, 1_000)
        .expect("noted");
    let read = |name: &str| std::fs::read_to_string(state.join(name)).expect("read");
    assert_eq!(
        (
            read(DEVELOPER_TOOLS.opened),
            read(&DEVELOPER_TOOLS.opened_by())
        ),
        ("r7 1000\n".to_owned(), format!("{a} 1000\n")),
        "the marker keeps its one plain word"
    );
    assert_eq!(
        DEVELOPER_TOOLS.opened_as_in(state, a),
        Some((a.to_owned(), 1_000))
    );
    assert_eq!(
        DEVELOPER_TOOLS.opened_as_in(state, b),
        None,
        "A's page is not B's"
    );
    assert_eq!(
        DEVELOPER_TOOLS.opened_as_in(state, "r7@com.aterm.aterm"),
        Some((a.to_owned(), 1_000)),
        "a key read before the Mac takes its bundle's line"
    );
    DEVELOPER_TOOLS
        .note_opened_in(state, "0.146.0", 1_500)
        .expect("doctor's note");
    assert_eq!(
        DEVELOPER_TOOLS.opened_as_in(state, b),
        Some(("0.146.0".to_owned(), 1_500)),
        "a plain word written alone names every identity"
    );
    let older = aterm_tempfile::tempdir().expect("tempdir");
    DEVELOPER_TOOLS
        .note_opened_in(older.path(), "r7", 1_000)
        .expect("an older aterm's note");
    assert_eq!(
        DEVELOPER_TOOLS.opened_as_in(older.path(), b),
        Some(("r7".to_owned(), 1_000)),
        "with no file beside it, as before"
    );
}

/// A plain word written alone (`aterm doctor`'s, an older aterm's) names every identity
/// whatever windows write after it, so what a Mac reads does not hang on the order of the
/// writes: a window's opening keeps it in `devtools-pane-opened-by`, and so does the next
/// window's, while each window's own line still reads before it for that window; a newer
/// plain word takes its place. NEGATIVE CONTROL: the plain word a window writes beside its own
/// line is not kept as one, so it names only that line's identity.
#[test]
fn a_plain_word_written_alone_names_every_identity_whatever_writes_after_it() {
    let (a, b, c) = (
        "r7@com.aterm.aterm@A",
        "r7@com.aterm.aterm@B",
        "r7@com.aterm.aterm@C",
    );
    let doctor_first = aterm_tempfile::tempdir().expect("tempdir");
    let first = doctor_first.path();
    DEVELOPER_TOOLS
        .note_opened_in(first, "0.146.0", 1_000)
        .expect("doctor's note");
    DEVELOPER_TOOLS
        .note_opened_by_in(first, "r7", a, 2_000)
        .expect("A's");
    DEVELOPER_TOOLS
        .note_opened_by_in(first, "r7", c, 3_000)
        .expect("C's");
    let doctor_between = aterm_tempfile::tempdir().expect("tempdir");
    let between = doctor_between.path();
    DEVELOPER_TOOLS
        .note_opened_by_in(between, "r7", a, 1_000)
        .expect("A's");
    DEVELOPER_TOOLS
        .note_opened_in(between, "0.146.0", 2_000)
        .expect("doctor's note");
    DEVELOPER_TOOLS
        .note_opened_by_in(between, "r7", c, 3_000)
        .expect("C's");
    for (state, doctor_at, a_at) in [(first, 1_000, 2_000), (between, 2_000, 1_000)] {
        assert_eq!(
            DEVELOPER_TOOLS.opened_as_in(state, b),
            Some(("0.146.0".to_owned(), doctor_at)),
            "doctor's word names B after two windows wrote"
        );
        assert_eq!(
            DEVELOPER_TOOLS.opened_as_in(state, a),
            Some((a.to_owned(), a_at)),
            "A's own line reads before the plain word"
        );
    }
    let read = |name: &str| std::fs::read_to_string(first.join(name)).expect("read");
    assert_eq!(
        read(&DEVELOPER_TOOLS.opened_by()),
        format!("0.146.0 1000\n{a} 2000\n{c} 3000\n"),
        "the plain word A's window wrote beside its line is not kept"
    );
    DEVELOPER_TOOLS
        .note_opened_in(first, "0.147.0", 4_000)
        .expect("doctor's next note");
    DEVELOPER_TOOLS
        .note_opened_by_in(first, "r8", "r8@com.aterm.aterm@A", 5_000)
        .expect("A's");
    assert_eq!(
        read(&DEVELOPER_TOOLS.opened_by()),
        format!("{c} 3000\n0.147.0 4000\nr8@com.aterm.aterm@A 5000\n"),
        "the newer plain word takes the older's place"
    );
}

/// A Full Disk Access opening a window wrote before its bundle id was read (a plain key)
/// names every identity only until that identity's next note, which replaces it: another Mac
/// sharing `<state>` (a network home, a Mac set up with Migration Assistant) or a dev bundle
/// beside the release then reads no opening of its own, so its first walk still comes. The
/// cold line names every identity until then. NEGATIVE CONTROL: the Developer Tools marker,
/// which `aterm doctor` writes a plain word to, keeps that word whatever window writes after
/// it (`a_plain_word_written_alone_names_every_identity_whatever_writes_after_it`).
#[test]
fn a_full_disk_access_line_written_cold_gives_way_to_its_identitys_next_note() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path();
    let (a, b, dev) = (
        "r1@com.aterm.aterm@A",
        "r1@com.aterm.aterm@B",
        "r1@com.aterm.aterm.dev@A",
    );
    FULL_DISK_ACCESS
        .note_opened_beside_in(state, "r1", 1_000)
        .expect("noted cold");
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, b),
        Some(("r1".to_owned(), 1_000)),
        "cold, it names every identity"
    );
    FULL_DISK_ACCESS
        .note_opened_beside_in(state, a, 2_000)
        .expect("noted warm");
    let read = |name: &str| std::fs::read_to_string(state.join(name)).expect("read");
    assert_eq!(
        read(&FULL_DISK_ACCESS.opened_by()),
        format!("{a} 2000\n"),
        "the warm note replaces the cold line"
    );
    for other in [b, dev] {
        assert_eq!(
            FULL_DISK_ACCESS.opened_as_in(state, other),
            None,
            "{other} reads no opening of its own"
        );
    }
    assert_eq!(
        FULL_DISK_ACCESS.opened_as_in(state, a),
        Some((a.to_owned(), 2_000))
    );
}

/// The words every surface shares: the drag is never offered without the + route;
/// "should" is said exactly without `skips`; each step reads the list as it stands; the
/// wrong switches close the how.
#[test]
fn the_words_pair_the_drag_with_plus_and_say_should_until_d14() {
    for skips in [false, true] {
        for words in [why(skips), trade(skips), done(skips)] {
            assert_eq!(words.contains("should"), !skips, "skips={skips}: {words}");
        }
        assert!(
            trade(skips).contains("Gatekeeper's prompt"),
            "{}",
            trade(skips)
        );
    }
    assert!(
        HELPER_HOW.contains("Drag aterm")
            && HELPER_HOW.contains(PLUS_ROUTE)
            && HELPER_HOW.contains(CHECK_SWITCH),
        "{HELPER_HOW}"
    );
    assert!(HELPER_HOW.ends_with(WRONG_SWITCHES), "{HELPER_HOW}");
    assert!(
        HELPER_HOW_PLUS.starts_with("Press + below the Developer Tools list")
            && HELPER_HOW_PLUS.contains(CHECK_SWITCH)
            && HELPER_HOW_PLUS.ends_with(WRONG_SWITCHES)
            && !HELPER_HOW_PLUS.to_ascii_lowercase().contains("drag"),
        "{HELPER_HOW_PLUS}"
    );
    assert!(
        HELPER_SWITCH_ON.ends_with(WRONG_SWITCHES) && !HELPER_SWITCH_ON.contains("Drag"),
        "{HELPER_SWITCH_ON}"
    );
    assert!(
        !HELPER_LISTED_ON.to_ascii_lowercase().contains("drag")
            && !HELPER_LISTED_ON.contains("switch on"),
        "{HELPER_LISTED_ON}"
    );
    let list = |listing, label| DevToolList {
        listing,
        label,
        modified_at_s: Some(1),
        rows: Some(2),
    };
    for (listing, label, step) in [
        (DevToolListing::Absent, DevToolListLabel::Read, Step::Add),
        (DevToolListing::Off, DevToolListLabel::Read, Step::SwitchOn),
        (DevToolListing::On, DevToolListLabel::Read, Step::ListedOn),
        (
            DevToolListing::Unknown,
            DevToolListLabel::NoFullDiskAccess,
            Step::Add,
        ),
    ] {
        assert_eq!(
            Step::of(&list(listing, label)),
            step,
            "{listing:?} {label:?}"
        );
    }
    assert_eq!(Step::Add.how(), HELPER_HOW);
    assert_eq!(Step::SwitchOn.how(), HELPER_SWITCH_ON);
    assert_eq!(Step::ListedOn.how(), HELPER_LISTED_ON);
    assert_eq!(Step::Add.action(), HELPER_STEP);
    for step in [Step::SwitchOn, Step::ListedOn] {
        assert!(
            !step.action().to_ascii_lowercase().contains("drag")
                && step.action().contains("Developer Tools list"),
            "{step:?}: {}",
            step.action()
        );
    }
    assert!(STATUS_LINK.ends_with(STATUS_PAGE), "{STATUS_LINK}");
    assert!(PLUS_ROUTE.ends_with(PLUS_PRESS), "{PLUS_ROUTE}");
    assert!(HELPER_TITLE.contains("Developer Tools"));
    assert!(
        HELPER_STEP.contains("Developer Tools list"),
        "names the list"
    );
    assert!(only_aterm(true).contains("Terminal"));
    assert!(!only_aterm(false).contains("Terminal"));
}

#[test]
fn the_wait_ends_at_the_switch_or_at_an_unreadable_list() {
    let reads = std::cell::Cell::new(0);
    let got = await_switch(
        "x",
        std::time::Duration::from_secs(60),
        |_| {
            reads.set(reads.get() + 1);
            read(DevToolListing::On, 1)
        },
        || false,
    );
    assert_eq!((got.listing, reads.get()), (DevToolListing::On, 1));
    let got = await_switch(
        "x",
        std::time::Duration::from_secs(60),
        |_| DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
        || false,
    );
    assert_eq!(got.label, DevToolListLabel::NoFullDiskAccess);
    let got = await_switch(
        "x",
        std::time::Duration::ZERO,
        |_| read(DevToolListing::Absent, 0),
        || false,
    );
    assert_eq!(got.listing, DevToolListing::Absent);
}

/// THE PERSON ENDS THE WAIT: once `ended` reads true (Enter at `aterm doctor`'s terminal)
/// the wait returns the reading it took, the switch still off. The one-minute limit is the
/// hang detector: a wait that ignored `ended` would read the list until it ran out.
#[test]
fn the_wait_ends_when_the_person_stops_it() {
    let reads = std::cell::Cell::new(0);
    let got = await_switch(
        "x",
        aterm_spec::wait::MUST_HAPPEN,
        |_| {
            reads.set(reads.get() + 1);
            read(DevToolListing::Off, 2)
        },
        || reads.get() >= 2,
    );
    assert_eq!((got.listing, reads.get()), (DevToolListing::Off, 2));
}

/// `[privacy] observer = false` stops `aterm doctor`'s reads as it stops the window's; only
/// that boolean in that table does, and a file the window would not load is not "off".
#[test]
fn only_privacy_observer_false_stops_the_devtools_read() {
    assert!(observer_off_in("[privacy]\nobserver = false\n"));
    for not_off in [
        "",
        "[privacy]\n",
        "[privacy]\nobserver = true\n",
        "[privacy]\nobserver = \"false\"\n",
        "observer = false\n",
        "[privacy]\ncheck = false\n",
        "[privacy\nobserver = false\n",
    ] {
        assert!(!observer_off_in(not_off), "{not_off:?}");
    }
}

/// A test binary measures nothing: the probes' fence refuses before any open or spawn.
#[test]
fn the_real_facts_measure_nothing_from_a_test_binary() {
    let got = super::facts();
    if cfg!(target_os = "macos") {
        let got = got.expect("macOS has the list");
        assert_eq!(got.list.label, DevToolListLabel::RefusedOutOfBundle);
        assert_eq!(got.log.label, DevToolProbeLabel::RefusedOutOfBundle);
        assert_eq!(got.grant(), DevToolGrant::Unknown);
    } else {
        assert_eq!(got, None);
    }
}
