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

/// WHAT A REGISTRY READ IS TO THE FLEET TABLES: a projection, and the one held through an
/// unmoved inventory, are the registry read now; a partial history is held, dated by the read
/// that projected the rows the tables still show, however many partial reads follow.
/// NEGATIVE CONTROL: with the offer chosen by whether a projection is held (`last.is_some()`),
/// a partial history after a projection is offered as read: `registry=live` at the partial
/// read's time, `` a partial history after a projection `` fails.
#[test]
fn a_partial_history_is_offered_to_the_fleet_tables_as_held_never_as_a_fresh_read() {
    use aterm_db::fleet::{Folder, HELD_PARTIAL, Reading};
    const T0: u64 = 1_800_000_000_000;

    let mut named = LabView::default();
    named.me = Some("pk-me".into());
    let mut last = None;
    let folder = Folder::new();
    let mut offer = |read: Option<LabRead>, at_ms: u64| {
        let (view, lab) = lab_offer(read, &mut last);
        let word = match &lab {
            Reading::Read(offered) => {
                assert!(
                    Arc::ptr_eq(offered, &view),
                    "the projection the journal names by"
                );
                "read"
            }
            Reading::Off => "off",
            Reading::Held(why) => why,
        };
        folder.offer_lab(lab, Reading::Off, at_ms);
        folder.fold_now(at_ms);
        let lab = folder.tables().value.sources.lab;
        (word, lab.word(), lab.why(), lab.at_ms())
    };

    assert_eq!(
        offer(Some(LabRead::Partial), T0 - 300_000),
        (HELD_PARTIAL, "held", Some(HELD_PARTIAL), None),
        "a first partial history has no rows to keep"
    );
    assert_eq!(
        offer(
            Some(LabRead::Projected(
                Box::new(named.clone()),
                Inventory([1; 32])
            )),
            T0
        ),
        ("read", "live", None, Some(T0))
    );
    // The m22 case: a record whose predecessor has not arrived, for hours.
    for later in [300_000, 600_000, 6 * 3_600_000] {
        assert_eq!(
            offer(Some(LabRead::Partial), T0 + later),
            (HELD_PARTIAL, "held", Some(HELD_PARTIAL), Some(T0)),
            "a partial history after a projection, {later} ms on"
        );
    }
    // The history completes with nothing new in it: the registry was read, and is unmoved.
    assert_eq!(
        offer(Some(LabRead::Unchanged), T0 + 7 * 3_600_000),
        ("read", "live", None, Some(T0 + 7 * 3_600_000))
    );
    assert_eq!(
        offer(None, T0 + 8 * 3_600_000),
        ("off", "off", None, None),
        "no lab is no registry"
    );
}

/// WHAT THE READER OFFERS THE FLEET TABLES WHERE IT READS NO REGISTRY: a policy it cannot read
/// holds both inputs, a lab turned off drops both, and a home that holds no lab has none.
#[test]
fn the_reader_offers_the_fleet_tables_its_policy_refusal_its_off_and_its_absence() {
    use aterm_db::fleet::{Folder, HELD_POLICY};

    let home = aterm_tempfile::tempdir().unwrap();
    let config = home.path().join("aterm.toml");
    let cache = Cache {
        paths: Paths::for_home(home.path()),
        config: config.clone(),
        spool: None,
        held: Mutex::new(None),
        lab: Mutex::new(None),
        reader: OnceLock::from(()),
    };
    let write =
        |text: &str| aterm_uds::atomic_file::write(&config, text.as_bytes(), 0o600).unwrap();
    let folder = Folder::new();
    let offered = || {
        cache.read_into(&folder);
        folder.fold_now(0);
        let sources = folder.tables().value.sources;
        aterm_db::fleet::command::sources_line(&sources)
    };
    assert_eq!(
        aterm_db::fleet::command::sources_line(&folder.tables().value.sources),
        "registry=never journal=never network=never fleet_read=never",
        "CONTROL: nothing is offered before a read"
    );

    write("[alab\n");
    assert_eq!(
        offered(),
        format!(
            "registry=held({HELD_POLICY}) journal=held({HELD_POLICY}) network=never fleet_read=never"
        )
    );
    write("[alab]\nroster = false\n");
    assert_eq!(
        offered(),
        "registry=off journal=off network=never fleet_read=never"
    );
    // The policy allows the lab and this home holds none: there is no registry to read.
    write("[alab]\nroster = true\n");
    assert_eq!(
        offered(),
        "registry=off journal=off network=never fleet_read=never"
    );
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
