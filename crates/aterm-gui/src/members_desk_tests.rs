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
/// [`Local::of`]'s, which its membership log seat alone makes (owner, 2026-11-02), so `aterm ctl
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

/// The desk lane looks at this Mac's membership every [`LOCAL_EVERY`] and at every network move,
/// and reads the vault again only once a file it is read from moved since a settled read: over
/// ten looks, each followed by a network move and the loop's next look, a member whose vault
/// stands still is read once. Its removal moves the vault, so the next look reads it and drops the
/// desk, and a Mac that left is not read again while its vault stands still either. Counted on a
/// simulated clock, never timed.
///
/// NEGATIVE CONTROLS (2026-11-06): with `Lane::look` reading at every look whatever its inputs
/// say (the lane before), the ten looks read ten times (`left: 10, right: 1`); with `Lane::moved`
/// also starting a look at once (the lane's `looked = None` before), twenty (`left: 20, right:
/// 1`).
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
/// on the desk, and the lane reads it again at every look while its vault stands still, where a
/// whole member's is read once ([`the_lane_reads_the_vault_again_only_once_it_moved`]). Counted
/// on a simulated clock, never timed.
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
/// home at every look and at once after a network move, as it read every vault before it kept
/// one that stood still: over ten looks, each followed by a move and the loop's next look, twenty
/// reads. Counted on a simulated clock, never timed.
///
/// NEGATIVE CONTROL (2026-11-06): with the lane reading the home from the place for every desk
/// (no `HomeFrom::Vault` arm, as first merged with the lab home), the ten looks read once (`left:
/// 1, right: 20`) and the lab's home never follows the route.
#[test]
fn a_lab_vaults_home_is_read_with_its_vault_at_every_look_and_move() {
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
        advance();
    }
    assert_eq!(home_from(slot), Some(HomeFrom::Vault), "the lab's desk");
    assert_eq!(lane.reads, 20);
    fixture.server.stop();
}
