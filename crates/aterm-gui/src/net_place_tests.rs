// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::status_item::{FleetLine, Glance, StatusAction, StatusRow, compose_status_menu};

const SINCE_MS: u64 = 1_800_000_000_000;

fn utc(_: i64) -> Option<i64> {
    Some(0)
}

fn words_at(now_ms: u64) -> Words<'static> {
    Words { now_ms, zone: &utc }
}

fn in_fleet_no_route() -> FleetLine {
    FleetLine::InFleet {
        read: Some((0, 3, 0, SINCE_MS)),
        reading: false,
        failed: None,
        no_route: true,
        member_of: None,
        unbound_issuer: None,
    }
}

fn privacy_rows(rows: &[StatusRow]) -> usize {
    rows.iter()
        .filter(|row| {
            matches!(
                row,
                StatusRow::Action {
                    action: StatusAction::OpenLocalNetwork,
                    ..
                }
            )
        })
        .count()
}

/// The menu bar says where this Mac is on the network in the place's own summary, and sends the
/// person to the Local Network privacy setting exactly when the fleet read judged a no-route
/// this Mac's privacy (its on-link verdict, `aterm_netplace::words::on_link`), wherever this Mac
/// is: on a phone hotspot a member on the same hotspot refused with none there answering is this
/// Mac's setting too, and a read that judged no such refusal (members off this network, or one
/// gone off the link) points nowhere, at home as away.
/// NEGATIVE CONTROL: gating the privacy row on the place again (`*no_route && !away`, with `away`
/// the place being elsewhere) hides it under the hotspot's Network row, and this test fails at
/// "the read's verdict, away from the ALab LAN too".
#[test]
fn the_menu_says_the_place_and_blames_privacy_only_on_the_reads_on_link_verdict() {
    let now = SINCE_MS + 60_000;
    let at = i64::try_from(now / 1000).unwrap_or(0);
    let away = menu_line_of(
        &aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS),
        &words_at(now),
    );
    let home = menu_line_of(&aterm_netplace::sample::home(SINCE_MS, 0), &words_at(now));
    let menu = |network: &MenuLine, no_route: bool| {
        let fleet = match in_fleet_no_route() {
            FleetLine::InFleet {
                read,
                reading,
                failed,
                member_of,
                unbound_issuer,
                ..
            } => FleetLine::InFleet {
                read,
                reading,
                failed,
                no_route,
                member_of,
                unbound_issuer,
            },
            other => other,
        };
        compose_status_menu(
            &Glance {
                fleet,
                network: Some(network.clone()),
                ..Glance::default()
            },
            at,
        )
    };
    let rows = menu(&away, true);
    assert!(
        rows.iter()
            .any(|row| matches!(row, StatusRow::Info(label) if *label == away.label)),
        "the Network row: {rows:?}"
    );
    assert_eq!(
        privacy_rows(&rows),
        1,
        "the read's verdict, away from the ALab LAN too: {rows:?}"
    );
    assert_eq!(privacy_rows(&menu(&home, true)), 1, "on the ALab LAN");
    assert_eq!(privacy_rows(&menu(&away, false)), 0, "no verdict, away");
    assert_eq!(privacy_rows(&menu(&home, false)), 0, "no verdict, at home");
}

/// `sys`'s place is the place's own line and object, its unknown values `not_sampled` as every
/// other `sys` value that was not read, nested objects kept nested.
/// NEGATIVE CONTROL: mapping `Field::Absent` to `PlaceValue::Text("-")` gives the hotspot's
/// unmeasured internet time a `-` string, and this test fails at "an unknown value is not_sampled".
#[test]
fn sys_carries_the_places_line_and_object_with_unknowns_not_sampled() {
    use aterm_vitals::PlaceValue;
    let place = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let words = words_at(SINCE_MS + 60_000);
    let view = sys_view_of(&place, &words);
    assert_eq!(view.line, aterm_netplace::words::line(&place, &words));
    assert_eq!(
        view.fields.first(),
        Some(&("state", PlaceValue::Text("remote".to_owned())))
    );
    let Some((_, PlaceValue::Object(internet))) =
        view.fields.iter().find(|(key, _)| *key == "internet")
    else {
        panic!("the internet object: {:?}", view.fields);
    };
    assert!(
        internet.contains(&("checked_ms", PlaceValue::Absent("not_sampled"))),
        "an unknown value is not_sampled: {internet:?}"
    );
}

/// With a current, history-ready observer, the network band leads with the place away from the
/// ALab LAN (`Away from ALab LAN · phone
/// hotspot · 41 known`, short `Away · hotspot`), and on the ALab LAN, before any place was read,
/// while checking (learning) and with no fleet to learn a home from, the observer's own words
/// (Commons' reachable count) stand.
/// NEGATIVE CONTROLS: leaving `summary_at`'s text as the observer's whatever the place gives the
/// hotspot the observer's words, and this test fails at "away, the place's words lead"; the
/// first caption rule (any place but the ALab LAN) replaces them while learning, and
/// "Checking(Learning): the observer's words stand" fails.
#[test]
fn the_network_band_leads_with_the_place_away_from_the_alab_lan() {
    let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    snapshot.network.visibility = aterm_types::network_state::Visibility::Active;
    snapshot.network.history = aterm_types::network_state::HistoryState::Ready;
    let observer = crate::network_deck::summary_at(&snapshot, None);
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let caption = crate::network_deck::place_caption(Some(&away)).expect("away words");
    let (long, short) = (caption.long, caption.short);
    assert_eq!(
        crate::network_deck::summary_at(&snapshot, Some(&away)),
        long,
        "away, the place's words lead"
    );
    assert_ne!(long, observer);
    assert!(short.len() < long.len(), "{short} / {long}");
    let home = aterm_netplace::sample::home(SINCE_MS, 3);
    assert_eq!(crate::network_deck::place_caption(Some(&home)), None);
    assert_eq!(
        crate::network_deck::summary_at(&snapshot, Some(&home)),
        observer
    );
    for (at, alab) in [
        (
            aterm_netplace::Where::Checking(aterm_netplace::Checking::Learning),
            aterm_netplace::Alab::Learning,
        ),
        (
            aterm_netplace::Where::Network(aterm_netplace::Kind::Lan),
            aterm_netplace::Alab::NoFleet,
        ),
    ] {
        let unlearned = aterm_netplace::Place {
            at,
            alab,
            ..home.clone()
        };
        assert_eq!(
            crate::network_deck::summary_at(&snapshot, Some(&unlearned)),
            observer,
            "{at:?}: the observer's words stand"
        );
    }
}

/// PAINT READS THE CACHED PLACE AND ASKS FOR NO READ (the structural change c0ec13186b made, as
/// behaviour): the place words a resource frame captures for the network deck
/// (`network_deck::capture_place_caption`), the deck's paint of them on the render thread, and
/// the stats lens's summary take the place as last read ([`cached`]), never [`latest`], which
/// starts a read thread once the last read is stale; the hotspot's words in the paint and the
/// summary show that they read the place all the same.
/// NEGATIVE CONTROL: the frame's capture taking `crate::net_place::latest()` (as the paint did
/// before c0ec13186b) asks for a read, and this test fails at "the frame's capture asks for no
/// read".
#[test]
fn paint_reads_the_cached_place_and_asks_for_no_read() {
    let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    snapshot.network.visibility = aterm_types::network_state::Visibility::Active;
    snapshot.network.history = aterm_types::network_state::HistoryState::Ready;
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let caption = crate::network_deck::place_caption(Some(&away)).expect("away words");
    let (long, short) = (caption.long, caption.short);
    hold(Some(away));
    let g = crate::stats_deck::DeckGeom {
        w: 700.0,
        h: 200.0,
        pad: 0.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    let before = reads_asked();
    let caption = crate::network_deck::capture_place_caption();
    assert_eq!(
        reads_asked(),
        before,
        "the frame's capture asks for no read"
    );
    let (prims, _) = crate::network_deck::paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        caption.as_ref(),
        &inks,
        false,
    );
    assert!(
        prims.iter().any(|prim| matches!(prim,
            crate::widget::DrawPrim::Text { s, .. } if *s == long || *s == short)),
        "paint says the held place"
    );
    assert_eq!(reads_asked(), before, "paint asks for no read");
    assert_eq!(crate::network_deck::summary(&snapshot), long);
    assert_eq!(reads_asked(), before, "nor does the summary it paints");
    let _ = latest();
    assert_eq!(
        reads_asked(),
        before + 1,
        "where a latest() is one read asked"
    );
    hold(None);
}

/// A harness's internet answer (a turn's real reply, an API probe's measure) is noted into the
/// place's evidence with its source, so the internet axis comes from a measurement that runs in
/// normal use; a local proxy's answer says nothing past this Mac and is noted nowhere.
/// A proxy named the way a LAN names its hosts (`m22`, `proxy.lan`) is a local proxy too.
/// NEGATIVE CONTROLS: noting whatever the host (dropping `note_internet`'s `host_is_internet`
/// return) notes the proxy's reply, and this test fails at "a local proxy's answer says nothing
/// of the internet"; counting every name as the internet (the old `Err(_) => true`) notes
/// `m22`'s and fails there too.
#[cfg(unix)]
#[test]
fn harness_internet_answers_are_noted_and_a_local_proxys_are_not() {
    use aterm_netplace::InternetSource;
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = aterm_netplace::record::dir(root.path());
    hold_evidence_dir(Some(dir.clone()));
    for proxy in [
        "127.0.0.1",
        "localhost",
        "192.168.4.2",
        "[::1]",
        "100.64.0.9",
        "m22",
        "proxy.lan",
        "Gateway.Home.Arpa.",
        "mac-11.local",
    ] {
        note_internet(
            true,
            InternetSource::TurnReply,
            proxy,
            aterm_netplace::unix_ms_now(),
        );
    }
    assert!(
        aterm_netplace::heard::read(&dir).internet.is_empty(),
        "a local proxy's answer says nothing of the internet"
    );
    let now = aterm_netplace::unix_ms_now();
    note_internet(true, InternetSource::TurnReply, "api.anthropic.com", now);
    note_internet(false, InternetSource::ApiProbe, "api.anthropic.com", now);
    hold_evidence_dir(None);
    let mut noted: Vec<(bool, InternetSource, String)> = aterm_netplace::heard::read(&dir)
        .internet
        .into_iter()
        .map(|note| (note.ok, note.source, note.endpoint))
        .collect();
    noted.sort_by_key(|(ok, ..)| *ok);
    assert_eq!(
        noted,
        [
            (
                false,
                InternetSource::ApiProbe,
                "api.anthropic.com".to_owned()
            ),
            (
                true,
                InternetSource::TurnReply,
                "api.anthropic.com".to_owned()
            ),
        ]
    );
}

/// A turn's reply is noted at the time it came, not when the turn-end loop booked it, so a reply
/// from before a move counts for no stay after it (the place reads only notes in its stay).
/// NEGATIVE CONTROL: stamping the note now (the old `aterm_netplace::unix_ms_now()` in place of
/// `at_ms`) fails "noted at the reply's own time".
#[cfg(unix)]
#[test]
fn a_turns_reply_is_noted_at_its_own_time() {
    use aterm_netplace::InternetSource;
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = aterm_netplace::record::dir(root.path());
    hold_evidence_dir(Some(dir.clone()));
    let came = aterm_netplace::unix_ms_now() - 30_000;
    note_internet(true, InternetSource::TurnReply, "api.anthropic.com", came);
    hold_evidence_dir(None);
    let noted: Vec<u64> = aterm_netplace::heard::read(&dir)
        .internet
        .iter()
        .map(|note| note.at_ms)
        .collect();
    assert_eq!(noted, [came], "noted at the reply's own time");
}

/// A turn's reply older than [`TURN_REPLY_FRESH_MS`] when booked (a relaunch or a wake booking a
/// session's last reply from home, hours ago) is noted nowhere; an API probe's measure is always
/// noted, at the time it was made.
/// NEGATIVE CONTROL: dropping the age rule notes the hours-old reply, and "an hours-old reply is
/// noted nowhere" fails.
#[cfg(unix)]
#[test]
fn an_hours_old_turn_reply_is_noted_nowhere() {
    use aterm_netplace::InternetSource;
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = aterm_netplace::record::dir(root.path());
    hold_evidence_dir(Some(dir.clone()));
    let now = aterm_netplace::unix_ms_now();
    note_internet(
        true,
        InternetSource::TurnReply,
        "api.anthropic.com",
        now - 3 * 60 * 60_000,
    );
    hold_evidence_dir(None);
    assert!(
        aterm_netplace::heard::read(&dir).internet.is_empty(),
        "an hours-old reply is noted nowhere"
    );
}

fn away_entries_in(app: &crate::App) -> usize {
    let prefix = aterm_messages::exceptions::Family::NetworkPlace.prefix();
    app.messages
        .digest()
        .entries
        .iter()
        .filter(|entry| entry.key.starts_with(&prefix))
        .count()
}

fn alerts_in(app: &crate::App) -> usize {
    app.messages
        .live_rows()
        .filter(|live| matches!(live.msg.need(), aterm_messages::need::Need::Now(_)))
        .count()
}

fn settle(app: &mut crate::App) {
    if let Some(due) = app.messages_deadline() {
        let _ = app.settle_messages(due + std::time::Duration::from_secs(1));
    }
}

/// BEING AWAY IS ONE FACT FOR LATER: while this Mac is on a phone hotspot away from the ALab
/// LAN the exceptions digest holds one `network.place` entry and no alert is up; back home the
/// entry is gone. A first run that has learned no ALab LAN raises nothing.
/// NEGATIVE CONTROL: `away_entries` answering none whatever the place (its `let Some(away)`
/// arm made to return early) leaves the digest empty on the hotspot, and "one fact while away"
/// fails.
#[test]
fn the_away_entry_stands_while_away_and_ends_on_return_with_no_alert() {
    let mut app = crate::App::headless_for_test();
    let words = words_at(SINCE_MS + 10 * 60_000);
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    app.state_network_place(Some(&away), &words);
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 1, "one fact while away");
    assert_eq!(alerts_in(&app), 0, "no alert for being away");
    let learning = aterm_netplace::Place {
        alab: aterm_netplace::Alab::Learning,
        ..away.clone()
    };
    app.state_network_place(Some(&learning), &words);
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 0, "a first run raises nothing");
    app.state_network_place(Some(&away), &words);
    app.state_network_place(
        Some(&aterm_netplace::sample::home(SINCE_MS + 11 * 60_000, 3)),
        &words,
    );
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 0, "gone once back");
    assert_eq!(alerts_in(&app), 0);
}

/// The follow lane restates the entry when its fact moves (away begins, past the blip; the
/// network it is away on changes; it ends), not when only the internet's word does.
#[test]
fn the_follow_lane_marks_a_change_of_the_away_fact_alone() {
    let early = words_at(SINCE_MS + 1_000);
    let later = words_at(SINCE_MS + 10 * 60_000);
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    assert_eq!(away_key(Some(&away), &early), None, "a blip states nothing");
    let stated = away_key(Some(&away), &later);
    assert!(stated.is_some());
    let mut answering = away.clone();
    answering.internet.state = aterm_netplace::InternetState::Ok;
    assert_eq!(away_key(Some(&answering), &later), stated);
    let home = aterm_netplace::sample::home(SINCE_MS, 3);
    assert_eq!(away_key(Some(&home), &later), None);
    assert_eq!(away_key(None, &later), None);
}
