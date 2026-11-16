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

/// A plain ask from this member's home ALab LAN on the default ALab vault files its entry like
/// any other and reads as asking: a plain ask waits for a person's Allow from every network, and
/// a Mac on the home ALab LAN gets in with nobody's Allow only by proving itself to the desk
/// (`docs/DESIGN-automatic-lan-admission-2026-11-02.md`).
///
/// NEGATIVE CONTROL: have `Snapshot::of` drop the rows whose source `Local::automatically_admits`
/// (as the forwarding path filed none): the entry goes once the member's home holds `10.0.0.9`.
#[test]
fn a_plain_ask_from_the_home_lan_files_an_entry_for_a_person() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (desk, request) = heard(root.path(), true);
    let mut app = app_with(Arc::clone(&desk));
    let key = entry_of(&app, request).key;
    let home = aterm_secrets::home_lan::HomeNet::at(
        std::net::Ipv4Addr::new(10, 0, 0, 20),
        std::net::Ipv4Addr::new(255, 255, 255, 0),
    );
    let member = Local::with(device(root.path(), "m1"), TRUST, true, "m1")
        .unwrap()
        .with_automatic_lan([])
        .with_home(Some(home));
    assert!(member.automatically_admits([10, 0, 0, 9].into()));
    desk.lock().unwrap().refresh(member);
    app.refresh_member_rows();
    assert_eq!(join_keys(&app), std::slice::from_ref(&key));
    let listed = app.take_members(&Asked::Requests);
    assert!(listed.contains("state=asking"), "{listed}");
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
/// `ctl --host … notice act` and a harness answer reach a live row, which the entry is not) and
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

    // The entry is in the digest, not a live row `notice act` presses: refused.
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
    follow_local(app.members.slot, &gone, Local::of);
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
    // Both rows are live: a complete ask, keyed.
    let mut app = App::headless_for_test();
    for row in [row, signed] {
        let posted = app.post_unsynced(row, wall_stamp_now());
        assert!(app.messages.live(posted).is_some(), "the undo row shows");
    }
}

/// A member the current Commons epochs leave out of control keeps its desk: the lane's read is
/// [`Local::of`]'s, which its membership log seat alone makes, so `aterm ctl
/// members requests` answers, and an asking Mac is greeted by a member that can allow rather than
/// told this Mac is no member. `left` reads it as a member still, so a read that found no desk
/// for a moment (a vault pass that held its state past the read's deadline) never drops its desk.
/// Without its pinned Commons anchor it is no member by its log while its Commons observation
/// still reads control-excluded, so `left` reads it gone there and the lane drops the desk, which
/// comes back with the pin; once its log removes it, `left` reads it gone and the lane drops the
/// desk again.
///
/// NEGATIVE CONTROLS: the seated arm of `Local::of` answering `Ok(None)` (the old rule, where
/// exclusion meant no desk) fails the `desk().is_some()` assert. `left` counting `control-excluded`
/// as gone without asking `client::member_by_log` (as before) fails "a seated member has not
/// left". `left` never counting `control-excluded` or `control-not-visible` as gone fails "a
/// member with no pinned anchor has left". `left` not counting `dropped` as gone fails "a dropped
/// member has left".
#[test]
fn a_member_commons_control_leaves_out_keeps_its_desk() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let mut app = App::headless_for_test();
    app.members.slot = Box::leak(Box::new(Mutex::new(None)));
    assert!(
        matches!(
            aterm_secrets::commons::observe(&fixture.member),
            Ok(aterm_secrets::commons::Observation::Unavailable(
                aterm_secrets::commons::Unavailable::ControlExcluded
            ))
        ),
        "the member's Commons observation is control-excluded"
    );
    assert!(!left(&fixture.member), "a seated member has not left");
    follow_local(app.members.slot, &fixture.member, Local::of);
    assert!(
        app.members.desk().is_some(),
        "the excluded member has a desk"
    );
    assert_eq!(app.take_members(&Asked::Requests), "OK 0\n");

    let mut answered = std::io::Cursor::new(Vec::new());
    serve_ask(
        app.members.desk().as_ref(),
        &mut answered,
        SocketAddr::from(([10, 0, 0, 9], 7)),
    );
    let greeting =
        members::joiner::greeting(&mut answered.get_ref().as_slice()).expect("a member's greeting");
    assert!(greeting.can_allow, "its log seat lets it allow");

    // Without its pinned anchor the member is no member by its log, while Commons still reads it
    // as control-excluded: `left` asks the log, not the vault's revocation.
    let pin = fixture.member.dir.join("commons/default.pin");
    let kept = fixture.member.dir.join("commons/default.pin.kept");
    std::fs::rename(&pin, &kept).expect("the pin set aside");
    assert!(
        matches!(
            aterm_secrets::commons::observe(&fixture.member),
            Ok(aterm_secrets::commons::Observation::Unavailable(
                aterm_secrets::commons::Unavailable::ControlExcluded
            ))
        ),
        "without its pin the member still reads control-excluded"
    );
    assert!(
        matches!(client::member_by_log(&fixture.member), Ok(false)),
        "without its pin the member is no member by its log"
    );
    assert!(
        left(&fixture.member),
        "a member with no pinned anchor has left"
    );
    follow_local(app.members.slot, &fixture.member, Local::of);
    assert!(
        app.members.desk().is_none(),
        "the desk of a member with no pinned anchor is dropped"
    );
    std::fs::rename(&kept, &pin).expect("the pin put back");
    follow_local(app.members.slot, &fixture.member, Local::of);
    assert!(
        app.members.desk().is_some(),
        "its desk is back with its pin"
    );

    client::remove_member(&fixture.admin, "member", false).expect("the admin removes the member");
    client::sync(&fixture.member, None).expect("the member syncs its removal");
    assert!(left(&fixture.member), "a member its log removed has left");
    follow_local(app.members.slot, &fixture.member, Local::of);
    assert!(app.members.desk().is_none(), "its desk is dropped");
    let unavailable = || match aterm_secrets::commons::observe(&fixture.member) {
        Ok(aterm_secrets::commons::Observation::Unavailable(why)) => Some(why),
        _ => None,
    };
    assert_eq!(
        unavailable(),
        Some(aterm_secrets::commons::Unavailable::Revoked),
        "a removed member reads revoked"
    );
    // A Mac a rotation dropped with no `Remove` (one-mesh E2) reads dropped and has left as well;
    // `client::test_read_dropped` stands in for that judgment.
    client::test_read_dropped(|| {
        assert_eq!(
            unavailable(),
            Some(aterm_secrets::commons::Unavailable::Dropped),
            "a dropped member reads dropped"
        );
        assert!(left(&fixture.member), "a dropped member has left");
    });
    fixture.server.stop();
}

thread_local! {
    /// The simulated clock [`Lane`] reads in [`the_lane_reads_the_vault_again_only_once_it_moved`].
    static SIMULATED: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

fn simulated() -> std::time::Instant {
    SIMULATED
        .with(std::cell::Cell::get)
        .expect("the simulated clock is set")
}

/// The desk lane looks at this Mac's membership at every change its watch hears and at every
/// network move, and reads the vault again only once a file it is read from moved since a settled
/// read: over ten looks, each followed by a network move and the loop's next look, a member whose
/// vault stands still is read once. Its removal moves the vault, so the next look reads it and
/// drops the desk, and a Mac that left is not read again while its vault stands still either.
/// Counted on a simulated clock, never timed.
///
/// NEGATIVE CONTROLS (2026-11-15): with `Lane::look` reading at every look whatever its inputs
/// say, the twenty looks read twenty times (`left: 20, right: 1`); with `Lane::moved` reading the
/// vault again at every move (its `with_vault` arm's `settled = None` and `looked = None` for
/// every desk), eleven, the first read and one after each move (`left: 11, right: 1`).
#[test]
fn the_lane_reads_the_vault_again_only_once_it_moved() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let slot: &'static Slot = Box::leak(Box::new(Mutex::new(None)));
    SIMULATED.with(|now| now.set(Some(std::time::Instant::now())));
    let advance = || SIMULATED.with(|now| now.set(Some(simulated() + LOCAL_EVERY)));
    let mut lane = Lane::new(fixture.member.clone(), simulated);
    for _ in 0..10 {
        lane.look(slot);
        lane.moved(slot);
        lane.look(slot);
        advance();
    }
    assert_eq!(lane.reads, 1);
    assert!(slot.lock().unwrap().is_some(), "the member's desk");

    client::remove_member(&fixture.admin, "member", false).expect("the admin removes the member");
    client::sync(&fixture.member, None).expect("the member syncs its removal");
    lane.look(slot);
    assert_eq!(lane.reads, 2, "the removal moved the vault");
    assert!(slot.lock().unwrap().is_none(), "its desk is dropped");
    for _ in 0..3 {
        advance();
        lane.look(slot);
    }
    assert_eq!(lane.reads, 2, "a Mac that left, its vault standing still");
    fixture.server.stop();
}

/// A member a fault shaped (`Local::whole` false: a read that failed gave way to a default) shows
/// on the desk, and the lane reads it again at every look [`LOCAL_EVERY`] after its last read
/// while its vault stands still, where a whole member's is read once
/// ([`the_lane_reads_the_vault_again_only_once_it_moved`]). Counted on a simulated clock, never
/// timed.
///
/// NEGATIVE CONTROL (2026-11-09): with `follow_local` settling every member it read (`true` in
/// place of `whole`), the three looks read once (`left: 1, right: 3`).
#[test]
fn a_member_a_fault_shaped_is_read_again_at_every_look() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let slot: &'static Slot = Box::leak(Box::new(Mutex::new(None)));
    SIMULATED.with(|now| now.set(Some(std::time::Instant::now())));
    let advance = || SIMULATED.with(|now| now.set(Some(simulated() + LOCAL_EVERY)));
    let mut lane = Lane {
        of: |paths| Ok(Local::of(paths)?.map(|local| local.with_whole(false))),
        ..Lane::new(fixture.member.clone(), simulated)
    };
    for _ in 0..3 {
        lane.look(slot);
        advance();
    }
    assert!(slot.lock().unwrap().is_some(), "the member's desk shows");
    assert_eq!(lane.reads, 3);
    fixture.server.stop();
}

/// Rewrite `paths`' `state.toml` by rename, as every writer of it does, with `text`.
fn write_state(paths: &Paths, text: &str) {
    let state = paths.dir.join("state.toml");
    let next = state.with_extension("next");
    std::fs::write(&next, text).expect("the next state");
    std::fs::set_permissions(
        &next,
        std::fs::metadata(&state).expect("the state").permissions(),
    )
    .expect("the next state's mode");
    std::fs::rename(&next, &state).expect("the next state renamed in");
}

/// [`Local::of`], then `state.toml` rewritten with its `requested` line moved: the state moving
/// during the read, as an apply that fails partway through it moves it.
fn of_moving_the_state(paths: &Paths) -> aterm_secrets::Result<Option<Local>> {
    let local = Local::of(paths)?;
    let text = std::fs::read_to_string(paths.dir.join("state.toml")).expect("the state");
    let line = text
        .lines()
        .find(|line| line.starts_with("requested = "))
        .unwrap_or_else(|| panic!("state.toml holds requested: {text}"));
    let value: i64 = line["requested = ".len()..].parse().expect("a number");
    write_state(
        paths,
        &text.replace(line, &format!("requested = {}", value + 60)),
    );
    Ok(local)
}

/// A read during which `state.toml` moved settles nothing, though the file holds what it held
/// before the read once the read is done: the state is stamped by a digest of what it holds
/// (`client::state_standing`), which can go from A to B and back to A, so a member read under B
/// (an apply that failed partway through `Local::of`, its roster unread) is never kept under A's
/// stamps. Here the read itself writes B, and A is written back after it; the next look reads
/// again, and once read with nothing moving, the member stands. Counted on a simulated clock,
/// never timed.
///
/// NEGATIVE CONTROL (2026-11-09): with `Lane::look` settling by the inputs taken before the read
/// alone (no `desk_inputs` taken after it), the next look reads nothing (`a read during which the
/// state moved is read again`, `left: 1, right: 2`).
#[test]
fn a_read_during_which_the_state_moved_and_came_back_is_read_again() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let before = std::fs::read_to_string(fixture.member.dir.join("state.toml")).expect("the state");
    let standing = client::state_standing(&fixture.member).expect("the state's standing");
    let slot: &'static Slot = Box::leak(Box::new(Mutex::new(None)));
    SIMULATED.with(|now| now.set(Some(std::time::Instant::now())));
    let advance = || SIMULATED.with(|now| now.set(Some(simulated() + LOCAL_EVERY)));
    let mut lane = Lane {
        of: of_moving_the_state,
        ..Lane::new(fixture.member.clone(), simulated)
    };
    lane.look(slot);
    assert_eq!(lane.reads, 1);
    assert!(
        slot.lock().unwrap().is_some(),
        "PRECONDITION: the member's desk"
    );
    assert_ne!(
        client::state_standing(&fixture.member).expect("the state's standing"),
        standing,
        "PRECONDITION: the read moved the state"
    );
    write_state(&fixture.member, &before);
    assert_eq!(
        client::state_standing(&fixture.member).expect("the state's standing"),
        standing,
        "PRECONDITION: the state holds what it held before the read"
    );
    lane.of = Local::of;
    advance();
    lane.look(slot);
    assert_eq!(
        lane.reads, 2,
        "a read during which the state moved is read again"
    );
    advance();
    lane.look(slot);
    assert_eq!(lane.reads, 2, "CONTROL: a read nothing moved during stands");
    fixture.server.stop();
}

/// A debug build's lab vault names its home LAN in its own file (`members::lab_lan`), and its
/// home is that file under this Mac's route at the read, so the lane reads that vault with its
/// home at once after each network move, and every [`LOCAL_EVERY`] whether or not a file moved,
/// since the route can move with no event (the bound the lane's look every thirty seconds kept):
/// over ten looks, each followed by a move and the loop's next look, the first read and one after
/// each move, eleven; once the period passed, the next look reads again, and the look after it,
/// with no period passed, reads nothing. Counted on a simulated clock, never timed.
///
/// NEGATIVE CONTROLS (2026-11-15): with a network move reading nothing again for a home read with
/// the vault (`Lane::moved` without its `with_vault` arm), the ten looks read once (`left: 1,
/// right: 11`); with `Lane::look` reading a lab vault again only where a file moved (no
/// `home_due`), the look after the period reads nothing (`left: 11, right: 12`).
#[test]
fn a_lab_vaults_home_is_read_with_its_vault_at_every_move_and_period() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let cidr = members::lab_lan::Cidr::parse("192.168.64.0/24").expect("a lab home LAN");
    members::lab_lan::write(&fixture.member, &cidr).expect("the lab names its home LAN");
    let slot: &'static Slot = Box::leak(Box::new(Mutex::new(None)));
    SIMULATED.with(|now| now.set(Some(std::time::Instant::now())));
    let advance = || SIMULATED.with(|now| now.set(Some(simulated() + LOCAL_EVERY)));
    let mut lane = Lane::new(fixture.member.clone(), simulated);
    for _ in 0..10 {
        lane.look(slot);
        lane.moved(slot);
        lane.look(slot);
    }
    assert_eq!(home_from(slot), Some(HomeFrom::Vault), "the lab's desk");
    assert_eq!(lane.reads, 11);
    assert_eq!(
        lane.due(),
        Some(simulated() + LOCAL_EVERY),
        "due again a period after its read"
    );
    advance();
    lane.look(slot);
    assert_eq!(lane.reads, 12, "read again once the period passed");
    lane.look(slot);
    assert_eq!(lane.reads, 12, "CONTROL: nothing read within the period");
    fixture.server.stop();
}

/// The desk's rows change by themselves only as one lapses, and the lane wakes at that instant:
/// a row heard at `t` lapses [`desk::STILL_ASKING`] later, and [`next_change`] finds it within
/// [`CHANGE_GRAIN`] after, where the lane's look every two seconds found it up to two seconds
/// late. A desk with nothing heard changes at no instant, and a Mac that asks again moves its
/// row's lapse with its ask. Computed on the desk's own clock, never timed.
///
/// NEGATIVE CONTROL (2026-11-14): with `next_change` searching only half of
/// [`desk::STILL_ASKING`], it finds no lapse (`the row lapses`).
#[test]
fn the_lane_wakes_as_a_row_lapses() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let local = Local::with(device(root.path(), "m1"), TRUST, true, "m1").expect("member");
    let mut desk = Desk::new(local);
    let asked_at = aterm_time::Instant::now();
    assert_eq!(
        next_change(&desk, asked_at),
        None,
        "nothing heard changes at no instant"
    );
    let j = device(root.path(), "j");
    let request = j.request(TRUST.network, TRUST.authority).expect("request");
    let asked = signed_ask(&j, &desk, &request, "j");
    desk.hear(&asked.asking(), [10, 0, 0, 9].into(), asked_at)
        .expect("heard");
    let lapse = asked_at + desk::STILL_ASKING;
    let at = next_change(&desk, asked_at).expect("the row lapses");
    assert!(
        lapse <= at && at <= lapse + CHANGE_GRAIN,
        "at its lapse, within the grain: {:?} after it",
        at.saturating_duration_since(lapse)
    );
    assert!(desk.rows(at).is_empty(), "lapsed at the instant found");
    assert_eq!(desk.rows(lapse - CHANGE_GRAIN).len(), 1, "not before it");
    let again = asked_at + Duration::from_secs(60);
    desk.hear(&asked.asking(), [10, 0, 0, 9].into(), again)
        .expect("heard again");
    let at = next_change(&desk, again).expect("the row lapses");
    assert!(
        again + desk::STILL_ASKING <= at,
        "its lapse moved with its ask"
    );
}

/// An ask on the desk reads the home ALab LAN as it begins, from the network as it is then: a
/// member read on its home LAN that is on none now lets no Mac in by proving itself from it, and
/// one back on it does, where the lane's look read the home every thirty seconds and at network
/// moves. Driven by the reader handed in, never the network.
///
/// NEGATIVE CONTROL (2026-11-14): with `serve_ask_at_home` reading no home, the desk keeps the
/// home it was read with (`the ask read the home as it began`).
#[test]
fn an_ask_reads_the_home_lan_as_it_begins() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let local = Local::with(device(root.path(), "m1"), TRUST, true, "m1")
        .expect("member")
        .with_automatic_lan(Vec::new());
    let desk: Shared = Arc::new(Mutex::new(Desk::new(local)));
    let from: SocketAddr = "192.168.1.9:4000".parse().expect("an address");
    let admits = || {
        desk.lock_or_recover()
            .local()
            .automatically_admits(from.ip())
    };
    assert!(admits(), "PRECONDITION: read on its home LAN");
    serve_ask_at_home(
        Some(&desk),
        &mut std::io::Cursor::new(Vec::new()),
        from,
        &|| None,
    );
    assert!(!admits(), "the ask read the home as it began: none now");
    serve_ask_at_home(
        Some(&desk),
        &mut std::io::Cursor::new(Vec::new()),
        from,
        &|| Some(aterm_secrets::home_lan::HomeNet::test()),
    );
    assert!(admits(), "back on its home LAN at the next ask");
}

/// `f` run with this thread's route scripted onto the network behind `subnet`'s gateway, whose
/// hardware address is read: the network key ([`aterm_secrets::heard::network_key`]) is that
/// network's, whatever this Mac's routes say.
fn on_network<R>(subnet: [u8; 3], f: impl FnOnce() -> R) -> R {
    let [a, b, c] = subnet;
    let route = aterm_netplace::RouteFacts {
        interface: "en0".to_owned(),
        address: std::net::Ipv4Addr::new(a, b, c, 5),
        prefix: 24,
        gateway: Some(std::net::Ipv4Addr::new(a, b, c, 1)),
        gateway_hw: Some(vec![0xaa, 0xbb, 0xcc, a, b, c]),
    };
    let script = aterm_netplace::facts::Script {
        facts: aterm_netplace::Facts {
            route: Some(route),
            ..aterm_netplace::Facts::default()
        },
        via: Vec::new(),
    };
    aterm_netplace::facts::script_here(script, f)
}

/// A member desk under `root`, told this Mac is on `subnet`'s network, that heard one Mac ask
/// there: the desk, in a slot of its own.
fn heard_on(root: &std::path::Path, subnet: [u8; 3]) -> &'static Slot {
    let local = Local::with(device(root, "m1"), TRUST, true, "m1").expect("member");
    let mut desk = Desk::new(local);
    on_network(subnet, || {
        desk.on_network(aterm_secrets::heard::network_key())
    });
    let j = device(root, "j");
    let request = j.request(TRUST.network, TRUST.authority).expect("request");
    let asked = signed_ask(&j, &desk, &request, "j");
    desk.hear(
        &asked.asking(),
        [10, 0, 0, 9].into(),
        aterm_time::Instant::now(),
    )
    .expect("heard");
    Box::leak(Box::new(Mutex::new(Some(Arc::new(Mutex::new(desk))))))
}

/// How many rows the desk in `slot` shows now.
fn rows_in(slot: &Slot) -> usize {
    slot.lock_or_recover()
        .clone()
        .expect("the desk")
        .lock_or_recover()
        .rows(aterm_time::Instant::now())
        .len()
}

/// While the desk files rows, the lane reads which network this Mac is on again every
/// [`LOCAL_EVERY`], since no event tells of the default route moving between two networks this
/// Mac holds at once, nor of ARP resolving a new network's gateway after the move: a move with no
/// event drops the row heard before it once the period passed, the bound the lane's look every
/// thirty seconds kept, and not before. A desk that files no rows has nothing such a move drops,
/// and the lane reads the network on no clock. Counted on a simulated clock over a scripted
/// route, never timed.
///
/// NEGATIVE CONTROL (2026-11-15): with `Lane::follow_rows` reading the network only before its
/// first read (no period), the row heard on the last network stays (`the move drops the row heard
/// before it once the period passed`).
#[test]
fn while_rows_show_a_move_no_event_tells_of_drops_them_within_the_period() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let slot = heard_on(root.path(), [192, 168, 4]);
    SIMULATED.with(|now| now.set(Some(std::time::Instant::now())));
    let advance = |by: Duration| SIMULATED.with(|now| now.set(Some(simulated() + by)));
    let mut lane = Lane::new(Paths::for_home(root.path()), simulated);
    on_network([192, 168, 4], || lane.follow_rows(slot));
    assert_eq!(
        rows_in(slot),
        1,
        "CONTROL: a row heard on this network stays"
    );
    assert_eq!(
        lane.network_due(true),
        Some(simulated() + LOCAL_EVERY),
        "due again a period after its read"
    );

    advance(LOCAL_EVERY - CHANGE_GRAIN);
    on_network([10, 0, 0], || lane.follow_rows(slot));
    assert_eq!(rows_in(slot), 1, "not read again within the period");
    advance(CHANGE_GRAIN);
    on_network([10, 0, 0], || lane.follow_rows(slot));
    assert_eq!(
        rows_in(slot),
        0,
        "the move drops the row heard before it once the period passed"
    );

    let read = lane.networked;
    assert_eq!(lane.network_due(false), None, "no rows, no clock");
    advance(LOCAL_EVERY);
    on_network([172, 16, 0], || lane.follow_rows(slot));
    assert_eq!(lane.networked, read, "no rows, no read");
}

/// The lane waits until the first of its rows' next lapse and its next read of the network while
/// rows show, whichever is sooner, and with no rows and no read due, on no clock: a row's lapse
/// is still woken for within [`CHANGE_GRAIN`] where the network's read falls after it. Computed
/// on the desk's own clock and instants handed in, never timed.
///
/// NEGATIVE CONTROLS (2026-11-15): with `Lane::wake_at` leaving out the rows' lapse, it wakes for
/// the network read after the lapse (`the row's lapse, sooner`); leaving out the network read, it
/// wakes for the lapse, [`desk::STILL_ASKING`] on (`the network read, sooner`).
#[test]
fn the_lane_wakes_at_the_first_of_a_lapse_and_a_network_read() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let local = Local::with(device(root.path(), "m1"), TRUST, true, "m1").expect("member");
    let mut desk = Desk::new(local);
    let now = aterm_time::Instant::now();
    SIMULATED.with(|clock| clock.set(Some(now)));
    let mut lane = Lane::new(Paths::for_home(root.path()), simulated);
    lane.networked = Some(now);
    assert_eq!(lane.wake_at(None, now), None, "no desk, no clock");
    assert_eq!(lane.wake_at(Some(&desk), now), None, "no rows, no clock");
    let j = device(root.path(), "j");
    let request = j.request(TRUST.network, TRUST.authority).expect("request");
    let asked = signed_ask(&j, &desk, &request, "j");
    desk.hear(&asked.asking(), [10, 0, 0, 9].into(), now)
        .expect("heard");
    assert_eq!(
        lane.wake_at(Some(&desk), now),
        Some(now + LOCAL_EVERY),
        "the network read, sooner"
    );
    let lapse = now + desk::STILL_ASKING;
    lane.networked = Some(lapse);
    let at = lane.wake_at(Some(&desk), now).expect("a wake");
    assert!(
        lapse <= at && at <= lapse + CHANGE_GRAIN,
        "the row's lapse, sooner: {:?} after it",
        at.saturating_duration_since(lapse)
    );
}

/// An ask on the desk reads which network this Mac is on as it begins, so a row it hears is
/// heard on the network as it is then: a move no event told of drops the rows heard on the last
/// network at the next ask, where the lane read the network every thirty seconds whatever asked.
/// Driven by a scripted route, never the network.
///
/// NEGATIVE CONTROL (2026-11-15): with `follow_here` reading no network, the row heard on the
/// last network stays (`the ask read the network as it began`).
#[test]
fn an_ask_reads_the_network_as_it_begins() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let slot = heard_on(root.path(), [192, 168, 4]);
    let desk = slot.lock_or_recover().clone().expect("the desk");
    let from: SocketAddr = "10.0.0.9:4000".parse().expect("an address");
    let ask = || {
        serve_ask_at_home(
            Some(&desk),
            &mut std::io::Cursor::new(Vec::new()),
            from,
            &|| None,
        );
    };
    on_network([192, 168, 4], ask);
    assert_eq!(rows_in(slot), 1, "CONTROL: an ask on the same network");
    on_network([10, 0, 0], ask);
    assert_eq!(rows_in(slot), 0, "the ask read the network as it began");
}

/// A change made to the desk off its lane rings the lane's bell ([`RUNG`]), which then works out
/// when the desk's rows next change and whether the network is due again: an ask served, which
/// can hear a row, and a row ignored. Without the ring a lane that settled with no rows waits on
/// no clock past a row it never saw, which lapses unfiled and whose network nothing reads again.
///
/// NEGATIVE CONTROLS (2026-11-15): with `touched` leaving out its ring, the ask rings nothing
/// (`an ask served rings the lane`); with `App::ignore_member_request` leaving out its ring, the
/// ignore rings nothing (`a row ignored rings the lane`).
#[test]
fn a_change_off_the_lane_rings_the_lane() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (desk, request) = heard(root.path(), true);
    let mut app = app_with(Arc::clone(&desk));
    let rung = RUNG.renew();
    assert!(app.ignore_member_request(request), "the row ignored");
    assert!(rung.is_set(), "a row ignored rings the lane");
    let from: SocketAddr = "10.0.0.9:4000".parse().expect("an address");
    let rung = RUNG.renew();
    serve_ask_at_home(
        Some(&desk),
        &mut std::io::Cursor::new(Vec::new()),
        from,
        &|| None,
    );
    assert!(rung.is_set(), "an ask served rings the lane");
}

/// Where no file watch started, the lane looks at the files this Mac's membership is read from
/// every [`LOCAL_EVERY`], as before it waited on events: a move is read at the first look that
/// period allows, and the lane is due again a period after each look. Under a watch the same move
/// is read at the look its change wakes, and a settled read is due at no instant. Counted on a
/// simulated clock, never timed.
///
/// NEGATIVE CONTROL (2026-11-14): with `Lane::look` reading at once whether or not a watch hears
/// the files, the unwatched lane reads the move before its period (`left: 2, right: 1`).
#[test]
fn without_a_file_watch_the_lane_looks_on_todays_period() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let slot: &'static Slot = Box::leak(Box::new(Mutex::new(None)));
    SIMULATED.with(|now| now.set(Some(std::time::Instant::now())));
    let advance = || SIMULATED.with(|now| now.set(Some(simulated() + LOCAL_EVERY)));
    let mut unwatched = Lane {
        watched: false,
        ..Lane::new(fixture.member.clone(), simulated)
    };
    let mut watched = Lane::new(fixture.member.clone(), simulated);
    let other: &'static Slot = Box::leak(Box::new(Mutex::new(None)));
    unwatched.look(slot);
    watched.look(other);
    assert_eq!((unwatched.reads, watched.reads), (1, 1));
    assert_eq!(unwatched.due(), Some(simulated() + LOCAL_EVERY));
    assert_eq!(watched.due(), None, "a settled read under a watch");

    client::remove_member(&fixture.admin, "member", false).expect("the admin removes the member");
    client::sync(&fixture.member, None).expect("the member syncs its removal");
    unwatched.look(slot);
    watched.look(other);
    assert_eq!(unwatched.reads, 1, "the move waits out the period");
    assert_eq!(
        watched.reads, 2,
        "the move read at the look its change wakes"
    );
    advance();
    unwatched.look(slot);
    assert_eq!(unwatched.reads, 2, "the move read once the period passed");
    assert!(slot.lock().unwrap().is_none(), "its desk is dropped");
    assert_eq!(unwatched.due(), Some(simulated() + LOCAL_EVERY));
    fixture.server.stop();
}

/// Wait until `parked` says the lane waits, or woke and is held at the gate, on the one-minute
/// hang detector: a change made while the test holds the gate is read by the lane's next round
/// whole.
fn until_parked(parked: &AtomicBool) {
    let began = std::time::Instant::now();
    while !parked.load(Ordering::Acquire) {
        assert!(
            began.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the lane never waited"
        );
        aterm_uds::deadline::poll_yield();
    }
}

/// The desk lane waits on events and on no clock: once its read of this Mac's membership
/// settled, it waits with no deadline; an ask the desk hears wakes it to read which network this
/// Mac is on again within [`LOCAL_EVERY`] while the row shows, before the row lapses; the row
/// ignored, it waits on no clock again; the member's removal, which moves its vault, wakes it to
/// read once more and drop the desk, which it then waits on no clock again. Two reads in all: at
/// launch and after the removal. The bounds kept: a vault change is read at the look its change
/// wakes (the watch's burst settling, `file_watch::QUIET` to `SETTLE_CAP`, where the lane looked
/// every thirty seconds), and a move no event tells of drops the rows heard before it within
/// [`LOCAL_EVERY`], as the lane's look every thirty seconds did. Each step waits on the
/// one-minute hang detector, never a latency budget.
///
/// NEGATIVE CONTROLS: with `run` waiting at most [`READ_EVERY`] whatever it waits for (the lane
/// before), the settled lane waits on a clock (`a settled lane waits on no clock`), and with the
/// file watch never armed, the removal wakes nothing and its step meets the hang detector (`the
/// removal drops the desk`) (2026-11-14); with `Lane::wake_at` leaving out
/// [`Lane::network_due`], the round that saw the row waits for its lapse (`while the row shows,
/// the lane reads the network again within the period`) (2026-11-15).
#[test]
fn the_desk_lane_wakes_for_its_files_and_its_rows_and_no_clock() {
    use std::sync::mpsc;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = client::test_commons_excluded(root.path()).expect("the excluded member");
    let slot: &'static Slot = Box::leak(Box::new(Mutex::new(None)));
    let bell: &'static Bell = Box::leak(Box::new(Bell::new()));
    let stop = Arc::new(aterm_uds::ready::Doorbell::new().expect("the test's stop"));
    let parked = Arc::new(AtomicBool::new(false));
    let gate: Arc<Mutex<()>> = Arc::default();
    let (tell, rounds) = mpsc::channel::<(bool, Option<Instant>)>();
    let lane = {
        let (member, stop, parked, gate) = (
            fixture.member.clone(),
            Arc::clone(&stop),
            Arc::clone(&parked),
            Arc::clone(&gate),
        );
        std::thread::spawn(move || {
            let mut inputs = Inputs::start(bell).expect("the membership's watch");
            let mut lane = Lane::new(member, std::time::Instant::now);
            let mut wait = |rung: &Stop, deadline: Option<Instant>| {
                let _ = tell.send((slot.lock_or_recover().is_some(), deadline));
                parked.store(true, Ordering::Release);
                let fds = [rung.bell().expect("the lane's bell"), stop.fd()];
                let ready = aterm_uds::ready::any(&fds, deadline).expect("the lane's wait");
                // Parked until it holds the gate: a lane woken while the test holds it reads
                // nothing before the test lets go.
                let _held = gate.lock_or_recover();
                parked.store(false, Ordering::Release);
                if stop.clear() {
                    ControlFlow::Break(())
                } else if ready.is_empty() {
                    ControlFlow::Continue(Heard::Due)
                } else {
                    ControlFlow::Continue(Heard::Rung)
                }
            };
            let flow = run(&mut lane, slot, Some(&mut inputs), bell, &mut wait);
            (flow, lane.reads)
        })
    };
    let until = |what: &str, done: &dyn Fn(bool, Option<Instant>) -> bool| {
        let mut seen = Vec::new();
        loop {
            let (desk, deadline) = rounds
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap_or_else(|_| panic!("{what}: {seen:?}"));
            seen.push((desk, deadline));
            if done(desk, deadline) {
                break seen;
            }
        }
    };
    let launch = until("the member's desk", &|desk, _| desk);
    assert!(
        launch.iter().all(|(_, deadline)| deadline.is_none()),
        "a settled lane waits on no clock: {launch:?}"
    );

    let desk = slot.lock_or_recover().clone().expect("the member's desk");
    let trust = desk.lock_or_recover().local().trust();
    let j = device(root.path(), "j");
    let request = j.request(trust.network, trust.authority).expect("request");
    let asked_at = aterm_time::Instant::now();
    {
        let mut desk = desk.lock_or_recover();
        let asked = signed_ask(&j, &desk, &request, "j");
        desk.hear(&asked.asking(), [10, 0, 0, 9].into(), asked_at)
            .expect("heard");
    }
    bell.ring();
    let lapse = asked_at + desk::STILL_ASKING;
    let filed = until("the row's network read again", &|_, deadline| {
        deadline.is_some()
    });
    let told = std::time::Instant::now();
    let (_, at) = filed.last().copied().expect("the round that saw the row");
    let at = at.expect("a deadline");
    assert!(
        at <= told + LOCAL_EVERY && at < lapse,
        "while the row shows, the lane reads the network again within the period, before the \
         row lapses: {:?} after the round was told",
        at.saturating_duration_since(told)
    );

    {
        let mut desk = desk.lock_or_recover();
        let id = desk.rows(asked_at)[0].id;
        desk.ignore(id).expect("ignored");
    }
    bell.ring();
    until("nothing left to lapse", &|_, deadline| deadline.is_none());

    let held = gate.lock_or_recover();
    until_parked(&parked);
    client::remove_member(&fixture.admin, "member", false).expect("the admin removes the member");
    client::sync(&fixture.member, None).expect("the member syncs its removal");
    drop(held);
    let removed = until("the removal drops the desk", &|desk, _| !desk);
    assert!(
        removed.iter().all(|(_, deadline)| deadline.is_none()),
        "the lane that read the removal waits on no clock: {removed:?}"
    );

    stop.ring();
    let (flow, reads) = lane.join().expect("the lane");
    assert_eq!(flow, ControlFlow::Break(()));
    assert_eq!(reads, 2, "read at launch and after the removal");
    fixture.server.stop();
}

/// The desk lane's watch tells of each file this Mac's membership is read from and of no other
/// file of the vault folder: a record put on the end of the membership records' data file in
/// place, which no entry of any folder shows, is heard, and a file of the vault folder the desk
/// never reads, replaced by rename as the vault replaces its files, tells nothing. The missing
/// tell is shown by order, never by a wait for silence: the replacement comes first, and the next
/// tell is the append's. Each step waits on the one-minute hang detector, never a latency budget.
///
/// NEGATIVE CONTROL (2026-11-14): with [`inputs_of`] watching the vault folder's entries in place
/// of the standing files and [`VAULT_FILES`] (a watch of the folder, not of each file), the
/// replacement tells and the append does not (`the next tell is the append's`).
#[test]
fn the_watch_hears_each_desk_input_and_no_other_file() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::mpsc;
    let home = aterm_tempfile::tempdir().expect("a scratch home");
    let paths = Paths::locate_for_home(home.path());
    std::fs::create_dir_all(&paths.dir).expect("the scratch vault folder");
    std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o700))
        .expect("the scratch vault folder made private");
    let data = paths
        .standing_files()
        .pop()
        .expect("the records' data file");
    std::fs::create_dir_all(data.parent().expect("the records' layout"))
        .expect("the records' layout");
    std::fs::write(&data, b"a record\n").expect("the records' data");
    let (tell, told) = mpsc::channel::<u64>();
    let mut inputs = Inputs {
        watch: aterm_uds::file_watch::FileWatch::platform(
            "members-desk-watch-test",
            Box::new(move |key| {
                let _ = tell.send(key);
            }),
        )
        .expect("the membership's watch"),
        keys: BTreeMap::new(),
        next: 0,
    };
    inputs.arm(&paths);
    let data_key = inputs.keys.get(&Input::File(data.clone())).copied();
    let mut first = BTreeSet::new();
    while first.len() < inputs.keys.len() {
        first.insert(
            told.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("each watched input's first look tells"),
        );
    }

    let other = paths.dir.join("asking");
    let next = paths.dir.join("asking.next");
    std::fs::write(&next, b"not a desk input\n").expect("the other file");
    std::fs::rename(&next, &other).expect("the other file renamed in");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&data)
        .and_then(|mut file| file.write_all(b"another record\n"))
        .expect("a record appended in place");
    let heard = told
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the append is heard");
    assert_eq!(Some(heard), data_key, "the next tell is the append's");
}
