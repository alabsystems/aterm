// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::alab::journal::seen::Seen;

#[test]
fn cached_journal_policy_refusal_hides_but_preserves_history_until_recovery() {
    let home = aterm_tempfile::tempdir().unwrap();
    let config = home.path().join("aterm.toml");
    let history = Arc::new(alab_seen::Seen {
        accounts: [("ac-fixture".into(), AccountSeen::default())].into(),
        at: 42,
    });
    // Exercise the actual Source getter without starting its registry worker.
    let cache: &'static Cache = Box::leak(Box::new(Cache {
        paths: Paths::for_home(home.path()),
        config: config.clone(),
        spool: None,
        held: Mutex::new(Some(Arc::clone(&history))),
        lab: Mutex::new(None),
        reader: OnceLock::from(()),
    }));
    let active = || alab_seen::Source::seen(&cache, false);
    assert!(
        Arc::ptr_eq(&active().unwrap(), &history),
        "missing config defaults on"
    );
    let write =
        |text: &str| aterm_uds::atomic_file::write(&config, text.as_bytes(), 0o600).unwrap();
    write("[alab]\nroster = true\n");
    assert!(Arc::ptr_eq(&active().unwrap(), &history));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(active().is_none());
        cache.read();
        assert!(Arc::ptr_eq(
            cache.held.lock().unwrap().as_ref().unwrap(),
            &history
        ));
    }
    for malformed in ["[alab\n", "[alab]\nroster = 'not-a-boolean'\n"] {
        write(malformed);
        assert!(active().is_none());
        cache.read();
        assert!(Arc::ptr_eq(
            cache.held.lock().unwrap().as_ref().unwrap(),
            &history
        ));
    }
    write("[alab]\nroster = true\n");
    assert!(Arc::ptr_eq(&active().unwrap(), &history));
    write("[alab]\nroster = false\n");
    assert!(active().is_none());
    cache.read();
    assert!(
        cache.held.lock().unwrap().is_none(),
        "explicit off retains its clearing behavior"
    );
}

/// A Mac with no binding is named by its enrollment's uuid, never left blank, and the harness
/// form of an empty journal is empty.
/// NEGATIVE CONTROL: with `mac` returning the uuid whole, the name is 32 hex long
/// (`left: "uuid:00112233445566778899aabbccddeeff"`).
#[test]
fn an_unbound_mac_is_named_by_its_uuid() {
    let view = LabView::default();
    assert_eq!(
        mac(&view, "00112233445566778899aabbccddeeff"),
        "uuid:00112233"
    );
    let empty = harness_seen(&view, &Seen::empty(5));
    assert!(empty.accounts.is_empty());
    assert_eq!(empty.at, 5);
}

/// The journal names Macs by the last projection through a partial history and an unmoved
/// inventory, and by none once there is no lab; a first partial history has none to keep.
/// NEGATIVE CONTROL: with a partial history read as no view (the journal left unread, as
/// `Partial => return` once did, or named by an empty view), `` a partial history keeps the
/// last projection `` fails.
#[test]
fn the_journal_names_macs_by_the_last_projection_until_there_is_no_lab() {
    let mut named = LabView::default();
    named.me = Some("pk-me".into());
    let first = Inventory([1; 32]);
    let mut last = None;
    assert_eq!(
        *journal_view(Some(LabRead::Partial), &mut last),
        LabView::default(),
        "a first partial history has no projection to keep"
    );
    assert!(last.is_none());
    let view = journal_view(
        Some(LabRead::Projected(Box::new(named.clone()), first)),
        &mut last,
    );
    assert_eq!(*view, named, "a projection names the Macs");
    assert_eq!(last.as_ref().map(|(_, i)| *i), Some(first), "and is held");
    for read in [LabRead::Partial, LabRead::Unchanged] {
        let kept = journal_view(Some(read.clone()), &mut last);
        assert_eq!(
            *kept, named,
            "a partial history keeps the last projection ({read:?})"
        );
        assert_eq!(last.as_ref().map(|(_, i)| *i), Some(first), "{read:?}");
    }
    assert_eq!(
        *journal_view(None, &mut last),
        LabView::default(),
        "no lab names no Mac"
    );
    assert!(last.is_none(), "no lab drops the projection");
}

/// A test process installs no journal source.
/// NEGATIVE CONTROL: with `holds` true, `install` would install one and `current` answer.
#[test]
fn a_test_installs_no_journal_source() {
    install();
    assert!(alab_seen::current(false).is_none());
}

/// A history line carries its event's window, figure and reset as typed fields, so a reader
/// draws a week's readings without parsing the line's words, and says where the window is a
/// workspace's own.
#[test]
fn a_history_line_carries_its_events_window_figure_and_reset() {
    use aterm_db::alab::journal::event::{Event, Kind, Pk, Source, Window, Word};
    use aterm_db::alab::journal::seen::Logged;
    let pk = "ac-00000000000000a1";
    let quota = |t: u64, sub: Option<&str>| Logged {
        uuid: "00112233445566778899aabbccddeeff".into(),
        n: t,
        event: Event {
            t,
            kind: Kind::Quota,
            account: Pk::new(pk),
            cli: Word::new("claude").expect("a CLI word"),
            sub: sub.map(|key| Word::new(key).expect("a workspace key")),
            window: Some(Window::Week),
            pct_x10: Some(370),
            reset: Some(900),
            res: None,
            until: None,
            plan: None,
            src: Source::Statusline,
            why: None,
            to: None,
            tab: None,
            harness: None,
        },
        heard: false,
    };
    let mut seen = Seen::empty(100);
    seen.last_use.insert(pk.into(), ("0011".into(), 30));
    seen.events = vec![
        quota(10, None),
        quota(20, Some("00ff00ff00ff")),
        Logged {
            event: Event {
                kind: Kind::SessionStart,
                window: None,
                pct_x10: None,
                reset: None,
                ..quota(30, None).event
            },
            ..quota(30, None)
        },
    ];
    let held = harness_seen(&LabView::default(), &seen);
    let lines = &held.accounts[pk].history;
    assert_eq!(
        lines
            .iter()
            .map(|line| (
                line.t,
                line.window.as_deref(),
                line.sub,
                line.pct_x10,
                line.reset
            ))
            .collect::<Vec<_>>(),
        [
            (10, Some(alab_seen::WEEK), false, Some(370), Some(900)),
            (20, Some(alab_seen::WEEK), true, Some(370), Some(900)),
            (30, None, false, None, None),
        ],
        "a workspace's window says so"
    );
    assert_eq!(lines[0].kind, alab_seen::QUOTA);
    assert_eq!(
        alab_seen::week_reads(&held.accounts[pk]),
        [(10, 370, 900)],
        "the account's own week, not its workspace's"
    );
}
