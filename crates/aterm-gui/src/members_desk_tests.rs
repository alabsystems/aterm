// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_messages::wire;
use aterm_secrets::client::{self, Paths};
use aterm_secrets::commons::bootstrap::Device;
use aterm_secrets::commons::journal::Trust;

const TRUST: Trust = Trust {
    network: [1; 16],
    authority: [2; 32],
    genesis: [3; 32],
};

/// A request from this member's home ALab LAN on the default ALab vault files no entry: the
/// vault's own admission takes it, and `aterm members requests` reads it as joining. One heard
/// from another network keeps its entry, however private its address (D1).
///
/// NEGATIVE CONTROLS: `join_entries::desk_askers` without its `row.automatic` filter keeps the
/// entry once the row is automatic ("files no entry"); `Local::automatically_admits` ignoring the
/// member's home network ends the entry of `10.0.0.9` while the member's home is
/// `192.168.1.0/24` ("another network still needs a person").
#[test]
fn automatic_lan_admission_files_no_entry() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (desk, request) = heard(root.path(), true);
    let now = aterm_time::Instant::now();
    let mut app = app_with(Arc::clone(&desk));
    let key = entry_of(&app, request).key;
    let member = || {
        Local::with(device(root.path(), "m1"), TRUST, true, "m1")
            .unwrap()
            .with_automatic_lan([])
    };
    desk.lock().unwrap().refresh(member());
    app.refresh_member_rows();
    assert_eq!(
        join_keys(&app),
        std::slice::from_ref(&key),
        "another network still needs a person"
    );
    let home = aterm_secrets::home_lan::HomeNet::at(
        std::net::Ipv4Addr::new(10, 0, 0, 20),
        std::net::Ipv4Addr::new(255, 255, 255, 0),
    );
    desk.lock().unwrap().refresh(member().with_home(Some(home)));
    app.refresh_member_rows();
    assert_eq!(desk.lock().unwrap().rows(now).len(), 1);
    assert!(
        join_keys(&app).is_empty(),
        "files no entry: {:?}",
        join_keys(&app)
    );
    assert!(app.take_members(&Asked::Requests).contains("state=joining"));
}

fn device(root: &std::path::Path, name: &str) -> Device {
    let paths = Paths::for_home(&root.join(name));
    client::machine_identity(&paths).expect("identity");
    Device::open(&paths).expect("device").expect("an identity")
}

/// The `ask` a Mac (`asker`, whose request is `request`) sends `desk` under `name`, signed as
/// the joiner signs it.
struct SignedAsk {
    wire: Vec<u8>,
    name: String,
    signature: [u8; 64],
}

impl SignedAsk {
    fn asking(&self) -> desk::Asking<'_> {
        desk::Asking {
            wire: &self.wire,
            node: [7; 32],
            name: &self.name,
            model: "Mac15,3",
            signature: &self.signature,
        }
    }
}

fn signed_ask(
    asker: &Device,
    desk: &Desk,
    request: &aterm_secrets::commons::binding::Request,
    name: &str,
) -> SignedAsk {
    let signature = aterm_secrets::members::joiner::ask_signature(
        asker,
        desk.local().key(),
        &[7; 32],
        name,
        "Mac15,3",
        request.bytes(),
    )
    .expect("signed");
    SignedAsk {
        wire: request.bytes().to_vec(),
        name: name.to_owned(),
        signature,
    }
}

/// A member desk under `root` that heard one Mac, `j`, ask: the desk and that row's handle.
fn heard(root: &std::path::Path, can_allow: bool) -> (Shared, u64) {
    let local = Local::with(device(root, "m1"), TRUST, can_allow, "m1").expect("member");
    let mut desk = Desk::new(local);
    let j = device(root, "j");
    let request = j.request(TRUST.network, TRUST.authority).expect("request");
    let now = aterm_time::Instant::now();
    let asked = signed_ask(&j, &desk, &request, "j");
    desk.hear(&asked.asking(), [10, 0, 0, 9].into(), now)
        .expect("heard");
    let id = desk.rows(now)[0].id;
    (Arc::new(Mutex::new(desk)), id)
}

fn app_with(desk: Shared) -> App {
    let mut app = App::headless_for_test();
    app.members = Members::for_test(desk);
    app.refresh_member_rows();
    app
}

/// The digest entry of the desk row `request` (its join code's).
fn entry_of(app: &App, request: u64) -> aterm_messages::DigestEntry {
    let code = app
        .members
        .desk()
        .and_then(|desk| {
            desk.lock_or_recover()
                .rows(aterm_time::Instant::now())
                .into_iter()
                .find(|row| row.id == request)
                .map(|row| row.code)
        })
        .expect("the desk holds the row");
    let key = format!("members.join.{code}.asking");
    app.messages
        .digest()
        .entries
        .into_iter()
        .find(|entry| entry.key == key)
        .expect("the entry shows")
}

fn row_of(app: &App, request: u64) -> aterm_messages::MessageId {
    entry_of(app, request).id
}

/// The keys of the asking Macs' entries in the digest.
fn join_keys(app: &App) -> Vec<String> {
    app.messages
        .digest()
        .entries
        .into_iter()
        .map(|entry| entry.key)
        .filter(|key| key.starts_with("members.join."))
        .collect()
}

/// Whether any row on screen is an alert (a standing ask that needs the owner now).
fn alerts(app: &App) -> Vec<String> {
    app.messages
        .live_rows()
        .filter(|live| matches!(live.msg.need(), aterm_messages::need::Need::Now(_)))
        .map(|live| live.msg.title.clone())
        .collect()
}

/// Every word of every message the window holds, as `aterm ctl messages` reads them.
fn messages_read(app: &App) -> String {
    let query = wire::parse_read_args("512").expect("a query");
    app.read_messages(&query).join("\n")
}

/// The Allow entry takes only a person's own press: `notice act` (how a script, a fleet drive's
/// `ctl --host … notice act` and a harness answer reach a band row, which the entry is not) and
/// a control client's gesture are refused, and no phrase shows; the person's own press shows
/// it. The entry is Info, for later, and nothing is on screen.
#[test]
fn the_allow_row_refuses_a_press_through_ctl_a_fleet_drive_or_a_harness_answer() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, request) = heard(root.path(), true);
    let mut app = app_with(Arc::clone(&desk));
    let entry = entry_of(&app, request);
    let id = entry.id;
    assert_eq!(entry.actions, [Intent::AllowJoin { request }]);
    assert_eq!(entry.title, "New Mac calling itself j");
    assert_eq!(entry.severity, aterm_messages::Severity::Info);
    assert!(entry.actions.iter().all(Intent::never_default));
    assert!(
        alerts(&app).is_empty(),
        "nothing on screen: {:?}",
        alerts(&app)
    );

    // The entry is in the digest, not a band row `notice act` presses: refused.
    let acted =
        app.take_notice(wire::NoticeRequest::parse(&format!("act {id} Allow")).expect("act"));
    assert!(acted.starts_with("ERR notice:"), "{acted}");
    let wid = crate::WindowId(0);
    let driven = app.with_gesture_source(true, |app| {
        app.perform_intent(wid, id, Intent::AllowJoin { request })
    });
    assert!(!driven, "a control client's gesture");
    assert!(app.members.recorded().is_empty(), "no phrase showed");
    assert!(app.members.shown.is_none());
    assert!(matches!(
        desk.lock_or_recover().rows(aterm_time::Instant::now())[0].state,
        RowState::Asking
    ));

    // CONTROL: the person's own press shows the phrase, on this Mac's sheet alone.
    assert!(app.with_gesture_source(false, |app| {
        app.perform_intent(wid, id, Intent::AllowJoin { request })
    }));
    let shown = app.members.recorded();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].starts_with("Type ") && shown[0].ends_with(" on j."),
        "{}",
        shown[0]
    );
    assert!(matches!(
        desk.lock_or_recover().rows(aterm_time::Instant::now())[0].state,
        RowState::Allowed(_)
    ));
}

/// `aterm members allow <name>` run in a terminal (a person's, or one a driver types into)
/// answers that the phrase is on this Mac's screen and nothing else: the phrase is in the sheet
/// alone, never in the reply, `aterm ctl messages` or the log. A headless instance refuses.
/// Each reply names how long its own phrase works: an invite's lasts longer than an Allow's.
/// NEGATIVE CONTROL: with the reply's lifetime fixed at `ALLOW_LIFETIME` (the old "for 5
/// minutes" on every phrase), the invite's reply understates its phrase and the test fails.
#[test]
fn members_allow_in_a_driven_terminal_never_prints_the_phrase() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, _) = heard(root.path(), true);
    let mut app = app_with(Arc::clone(&desk));
    let reply = app.take_members(&Asked::parse("allow j").expect("allow"));
    assert_eq!(
        reply,
        format!("OK {}\n", on_screen(aterm_secrets::pake::ALLOW_LIFETIME))
    );
    let invited = app.take_members(&Asked::parse("invite").expect("invite"));
    assert_eq!(
        invited,
        format!("OK {}\n", on_screen(aterm_secrets::pake::INVITE_LIFETIME))
    );
    let shown = app.members.recorded().to_vec();
    assert_eq!(shown.len(), 2);
    let read = messages_read(&app);
    for text in &shown {
        let phrase = text
            .strip_prefix("Type ")
            .and_then(|rest| rest.split(" on ").next())
            .expect("the sheet's phrase");
        assert_eq!(phrase.split(' ').count(), 3, "{text}");
        for heard in [&reply, &invited, &read] {
            assert!(!heard.contains(phrase), "the phrase leaked: {heard}");
        }
    }
    let requests = app.take_members(&Asked::Requests);
    assert!(
        requests.starts_with("OK 1\nrequest name=j model=Mac15,3 from=10.0.0.9 code="),
        "{requests}"
    );
    assert!(requests.ends_with(" state=allowed\n"), "{requests}");

    // A Mac no person reads refuses, and makes no phrase.
    let mut headless = App::headless_for_test();
    headless.members = Members {
        sheets: Sheets::None,
        ..Members::for_test(heard(root.path(), true).0)
    };
    assert_eq!(
        headless.take_members(&Asked::parse("allow j").expect("allow")),
        format!("ERR members: {NO_SCREEN}\n")
    );
    // A member with no roster binding shows the row without Allow, and allows nothing.
    let (unbound, request) = heard(root.path(), false);
    let mut app = app_with(unbound);
    let entry = entry_of(&app, request);
    assert!(entry.actions.is_empty(), "{:?}", entry.actions);
    assert!(
        entry.then.contains("a member that can allow"),
        "{}",
        entry.then
    );
    let refused = app.take_members(&Asked::parse("allow j").expect("allow"));
    assert!(refused.contains("no Commons binding"), "{refused}");
    assert!(app.members.recorded().is_empty());
}

/// Ignore (`aterm members ignore`) ends the row and its entry on this Mac at once; one entry
/// stands in for `join_entries::MANY_AT` or more asking Macs.
#[test]
fn ignore_ends_the_entry_and_four_asking_macs_are_one_entry() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, request) = heard(root.path(), true);
    let mut app = app_with(Arc::clone(&desk));
    let id = row_of(&app, request);
    assert!(app.with_gesture_source(false, |app| {
        app.perform_intent(crate::WindowId(0), id, Intent::AllowJoin { request })
    }));
    assert!(app.members.shown.is_some(), "phrase sheet is shown");
    assert!(app.perform_intent(crate::WindowId(0), id, Intent::IgnoreJoin { request }));
    assert!(
        app.members.shown.is_none(),
        "ignoring a row closes its phrase sheet"
    );
    assert!(join_keys(&app).is_empty(), "{:?}", join_keys(&app));

    let now = aterm_time::Instant::now();
    for n in 0..crate::join_entries::MANY_AT {
        let asker = device(root.path(), &format!("k{n}"));
        let request = asker
            .request(TRUST.network, TRUST.authority)
            .expect("request");
        let mut held = desk.lock_or_recover();
        let asked = signed_ask(&asker, &held, &request, &format!("k{n}"));
        // Each Mac asks from its own address: one address holds `desk::PER_SOURCE` rows.
        let from = [10, 0, 1, u8::try_from(n).expect("a small count")];
        held.hear(&asked.asking(), from.into(), now).expect("heard");
    }
    app.refresh_member_rows();
    assert_eq!(join_keys(&app), ["members.join.many"]);
    let many = app
        .messages
        .digest()
        .entries
        .into_iter()
        .find(|entry| entry.key == "members.join.many")
        .expect("many");
    assert_eq!(
        many.title,
        format!("{} Macs ask to join ALab", crate::join_entries::MANY_AT)
    );
    assert!(many.actions.is_empty(), "no Allow without a name");
}

/// No join is an alert on screen (D3): a member desk's asking Mac and an admin approver's are
/// each an Info entry in the digest, with the press that lets it in beside it, and an entry ends
/// at once when its Mac no longer asks, not a family pass's ten minutes later.
/// NEGATIVE CONTROLS: posting the old standing notice beside the entry (`Message::now`) puts it
/// on screen ("nothing on screen: [\"j wants to join ALab\"]"); dropping the pass's
/// `except_end` of entries no longer named keeps m31's entry ("m31's entry ended at once").
#[test]
fn a_join_files_info_entries_and_puts_nothing_on_screen() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, request) = heard(root.path(), true);
    let mut app = App::headless_for_test();
    app.members = Members::for_test(desk);
    let m31 = aterm_secrets::client::Asking {
        code: "WXYZ-WXYZ-WXYZ-WXYZ".to_owned(),
        recipient: "age1m31".to_owned(),
        host: "m31".to_owned(),
    };
    app.members.set_approver(vec![m31]);
    app.refresh_member_rows();
    assert!(
        alerts(&app).is_empty(),
        "nothing on screen: {:?}",
        alerts(&app)
    );
    let desk_entry = entry_of(&app, request);
    assert_eq!(desk_entry.actions, [Intent::AllowJoin { request }]);
    let keys = join_keys(&app);
    assert_eq!(keys.len(), 2, "{keys:?}");
    let approver = app
        .messages
        .digest()
        .entries
        .into_iter()
        .find(|entry| entry.key == "members.join.WXYZ-WXYZ-WXYZ-WXYZ.asking")
        .expect("the approver's entry");
    assert_eq!(
        approver.actions,
        [Intent::ApproveJoin {
            recipient: "age1m31".to_owned()
        }]
    );
    assert_eq!(approver.severity, aterm_messages::Severity::Info);

    app.members.set_approver(Vec::new());
    app.refresh_member_rows();
    assert_eq!(
        join_keys(&app),
        std::slice::from_ref(&desk_entry.key),
        "m31's entry ended at once"
    );
}

/// An update carrying 0.147's standing join alerts retires them on the first poll,
/// including requests no longer in its snapshot. Current requests remain
/// available as Info entries, and unrelated messages stay live.
#[test]
fn a_desk_poll_retires_carried_join_alerts() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, request) = heard(root.path(), true);
    let mut app = app_with(desk);
    for key in [
        "members.ask.1",
        "members.ask.99",
        "secrets.asking.ABCD",
        "unrelated.warning",
    ] {
        app.post_unsynced(
            Message::new(tags::PRIVACY, Severity::Warn, "An existing warning")
                .key(key)
                .now(Ask::standing(
                    StandingWhy::OnlyYou(Reason::new("a person must act")),
                    Then::step("Review the request"),
                )),
            wall_stamp_now(),
        );
    }
    assert!(app.messages.live_by_key("members.ask.1").is_some());
    app.follow_member_requests();
    assert!(app.messages.live_by_key("members.ask.1").is_none());
    assert!(app.messages.live_by_key("members.ask.99").is_none());
    assert!(app.messages.live_by_key("secrets.asking.ABCD").is_none());
    assert!(app.messages.live_by_key("unrelated.warning").is_some());
    assert_eq!(entry_of(&app, request).severity, Severity::Info);
}

#[test]
fn polling_without_a_desk_retires_carried_join_alerts() {
    let mut app = App::headless_for_test();
    app.members.slot = Box::leak(Box::new(Mutex::new(None)));
    app.members.set_approver(Vec::new());
    app.post_unsynced(
        Message::new(tags::PRIVACY, Severity::Warn, "Old join request").key("members.ask.99"),
        wall_stamp_now(),
    );
    assert!(app.messages.live_by_key("members.ask.99").is_some());
    app.follow_member_requests();
    assert!(app.messages.live_by_key("members.ask.99").is_none());
    assert!(join_keys(&app).is_empty());
}

/// A phrase whose sheet did not open is burned at once, Allow's and invite's alike: nobody saw
/// it, so it gives up its place in the book and no run can use it, and the row asks again
/// rather than tell the asking Mac to wait for a phrase no person can read.
/// NEGATIVE CONTROL: `show_member_phrase` returning its error without `Desk::withdraw` leaves
/// the row Allowed; a withdraw that cancels no phrase leaves both unseen phrases live, holding
/// two of the four places.
#[test]
fn a_phrase_whose_sheet_did_not_open_is_burned_at_once() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, request) = heard(root.path(), true);
    let mut app = app_with(Arc::clone(&desk));
    app.members.sheets = Sheets::Refused;
    for asked in ["allow j", "invite"] {
        let reply = app.take_members(&Asked::parse(asked).expect("asked"));
        assert!(reply.starts_with("ERR members: "), "{asked}: {reply}");
    }
    assert!(app.members.shown.is_none(), "no sheet holds a phrase");
    let now = aterm_time::Instant::now();
    let rows = desk.lock_or_recover().rows(now);
    assert_eq!((rows[0].id, rows[0].state), (request, RowState::Asking));
    let requests = app.take_members(&Asked::Requests);
    assert!(requests.ends_with(" state=asking\n"), "{requests}");
    // Neither phrase is live or running: every place in the book is free for a phrase a person
    // can see.
    let mut held = desk.lock_or_recover();
    for _ in 0..aterm_secrets::pake::MAX_LIVE {
        held.invite(now)
            .expect("a place the unseen phrases gave up");
    }
}

/// A Mac that left the vault drops its desk at the lane's next read of its Commons state: its
/// rows end, the phrase it showed closes, an asking Mac is told this Mac is no member, and the
/// row's Allow, `allow`, `invite` and `requests` find no desk.
/// NEGATIVE CONTROL: a read that finds no member but keeps the desk (the lane's old
/// `Ok(None) => {}`) leaves the row up and the phrase showing.
#[test]
fn a_mac_that_left_the_vault_drops_its_desk_and_allows_nothing() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, request) = heard(root.path(), true);
    let mut app = app_with(desk);
    let _ = row_of(&app, request);
    assert_eq!(
        app.take_members(&Asked::parse("allow j").expect("allow")),
        format!("OK {}\n", on_screen(aterm_secrets::pake::ALLOW_LIFETIME))
    );
    assert!(app.members.shown.is_some(), "the phrase shows");

    // m1's scratch home keeps its identity but holds no vault now: Commons reads no member.
    let gone = Paths::for_home(&root.path().join("m1"));
    assert!(Local::of(&gone).expect("readable").is_none());
    follow_local(app.members.slot, &gone);
    app.refresh_member_rows();
    app.close_spent_phrase_sheet();

    let rows = join_keys(&app);
    assert!(rows.is_empty(), "the entries ended: {rows:?}");
    assert!(app.members.shown.is_none(), "the phrase sheet closed");
    let not_member = "this Mac is not an ALab member yet";
    assert_eq!(
        app.with_gesture_source(false, |app| app.allow_member_request(request)),
        Err(not_member.to_owned())
    );
    for asked in ["allow j", "invite", "requests"] {
        assert_eq!(
            app.take_members(&Asked::parse(asked).expect("a members form")),
            format!("ERR members: {not_member}\n"),
            "{asked}"
        );
    }
    assert_eq!(
        app.members.recorded().len(),
        1,
        "no phrase showed after the leave"
    );

    let mut answered = std::io::Cursor::new(Vec::new());
    serve_ask(
        app.members.desk().as_ref(),
        &mut answered,
        SocketAddr::from(([10, 0, 0, 9], 7)),
    );
    let why = members::joiner::greeting(&mut answered.get_ref().as_slice())
        .expect_err("an asking Mac is refused");
    assert!(why.to_string().contains(not_member), "{why}");
}

/// F16: a Mac the log removed that asks again shows as removed, not new: the row says who removed
/// it and when and offers [Allow again], a person's own press that shows the phrase and marks the
/// run to restore it; `aterm ctl members requests` says `state=removed by=m7`. A Mac a quorum
/// removed gets no Allow at all.
///
/// NEGATIVE CONTROL: in `join_entries::desk_askers`, drop the `row.removed` arms: the entry
/// reads "New Mac calling itself j" with a plain Allow (the ghost returns), and the title assert
/// fails.
#[test]
fn a_removed_mac_shows_as_removed_and_allow_again_is_a_persons_press() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let j = device(root.path(), "j");
    let request = j.request(TRUST.network, TRUST.authority).expect("request");
    let note = |quorum| client::RemovedNote {
        label: "m12".into(),
        by: vec!["m7".into()],
        on_ms: 1_793_318_400_000,
        quorum,
    };
    let desk_for = |quorum| {
        let mut notes = client::RemovedNotes::default();
        notes
            .by_recipient
            .insert(request.binding().recipient().to_bech32(), note(quorum));
        let local = Local::with(device(root.path(), "m1"), TRUST, true, "m1")
            .expect("member")
            .with_removed(notes);
        let mut desk = Desk::new(local);
        let now = aterm_time::Instant::now();
        let asked = signed_ask(&j, &desk, &request, "j");
        desk.hear(&asked.asking(), [10, 0, 0, 9].into(), now)
            .expect("heard");
        let id = desk.rows(now)[0].id;
        (Arc::new(Mutex::new(desk)), id)
    };

    let (desk, request_id) = desk_for(false);
    let mut app = app_with(Arc::clone(&desk));
    let entry = entry_of(&app, request_id);
    let id = entry.id;
    assert_eq!(entry.title, "Removed Mac calling itself j");
    assert!(
        entry
            .detail
            .iter()
            .any(|line| line == "m12 was removed by m7 on Oct 30. Allow again?"),
        "{:?}",
        entry.detail
    );
    assert_eq!(
        entry.actions,
        [Intent::AllowJoinAgain {
            request: request_id
        }]
    );
    let requests = app.take_members(&Asked::parse("requests").expect("requests"));
    assert!(requests.contains("state=removed by=m7"), "{requests}");
    let plain = app.take_members(&Asked::parse("allow j").expect("allow"));
    assert!(
        plain.contains("Allow again"),
        "a plain Allow says why: {plain}"
    );

    let acted =
        app.take_notice(wire::NoticeRequest::parse(&format!("act {id} Allow again")).expect("act"));
    assert!(acted.starts_with("ERR notice:"), "{acted}");
    let wid = crate::WindowId(0);
    assert!(!app.with_gesture_source(true, |app| {
        app.perform_intent(
            wid,
            id,
            Intent::AllowJoinAgain {
                request: request_id,
            },
        )
    }));
    assert!(app.members.recorded().is_empty(), "no phrase showed");
    assert!(app.with_gesture_source(false, |app| {
        app.perform_intent(
            wid,
            id,
            Intent::AllowJoinAgain {
                request: request_id,
            },
        )
    }));
    assert_eq!(app.members.recorded().len(), 1);
    let row = desk.lock_or_recover().rows(aterm_time::Instant::now())[0].clone();
    assert!(matches!(row.state, RowState::Allowed(_)) && row.restore);

    let (desk, request_id) = desk_for(true);
    let app = app_with(desk);
    let entry = entry_of(&app, request_id);
    assert!(
        entry
            .detail
            .iter()
            .any(|line| line.starts_with("m12 was removed by a quorum of m7")),
        "{:?}",
        entry.detail
    );
    assert!(entry.actions.is_empty(), "no Allow for a quorum's removal");
}

/// F16: the "Undo m7's removals?" row a removed Mac shows offers [Undo removals] until it signed
/// its share, then says how many more the quorum needs.
#[test]
fn the_undo_row_offers_undo_until_this_mac_signed() {
    let offer = |signed| client::UndoOffer {
        remover: [7; 48],
        label: "m7".into(),
        code: "abcd-efgh-ijkl-mn23".into(),
        others: 3,
        signed,
        needed: if signed { 1 } else { 2 },
    };
    let row = undo_notice(&offer(false));
    assert_eq!(
        row.title,
        "m7 removed this Mac and 3 others. Undo m7's removals?"
    );
    assert_eq!(
        row.actions,
        [Intent::UndoRemovals {
            remover: "abcd-efgh-ijkl-mn23".into()
        }]
    );
    assert!(row.actions[0].person_only() && row.actions[0].never_default());
    assert_eq!(row.key.as_deref(), Some("members.undo.abcd-efgh-ijkl-mn23"));
    let signed = undo_notice(&offer(true));
    assert!(signed.actions.is_empty(), "{:?}", signed.actions);
    // Both rows stand on the band: a complete ask, keyed.
    let mut app = App::headless_for_test();
    for row in [row, signed] {
        let posted = app.post_unsynced(row, wall_stamp_now());
        assert!(app.messages.live(posted).is_some(), "the undo row shows");
    }
}
