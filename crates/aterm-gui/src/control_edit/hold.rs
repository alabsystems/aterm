// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which sessions an open `edit` ticket holds, and the refusal that enforces it (amacs design §6.7; slices N1 and N4).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Instant;

use amacs::protocol::edit::Ticket;
use aterm_types::MutexExt as _;
use aterm_types::control_verbs::input_of;

/// The ticket and view holding one session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hold {
    pub(crate) ticket: Ticket,
    /// The ticket's view (`inspect app/v1 view <view>`).
    pub(crate) view: u64,
}

#[derive(Debug)]
struct Entry {
    hold: Hold,
    /// When an orphaned ticket's hold lapses; `None` while a waiter is parked
    /// or none has had the chance to arrive.
    expires: Option<Instant>,
}

#[derive(Debug)]
struct Holds {
    /// Each held session's holds, in the order their tickets first held it.
    sessions: BTreeMap<u64, Vec<Entry>>,
    /// The tickets of `edit … wait=0` requests being served ([`Unwaited`]).
    unwaited: BTreeSet<Ticket>,
}

static EDIT_HOLD: Mutex<Holds> = Mutex::new(Holds {
    sessions: BTreeMap::new(),
    unwaited: BTreeSet::new(),
});

/// The map's lock, a leaf (see the module doc). Callers lock it themselves
/// (`edit_hold().lock_or_recover()`), so every acquisition carries the name.
fn edit_hold() -> &'static Mutex<Holds> {
    &EDIT_HOLD
}

/// The session verbs a held session refuses.
pub(crate) fn holds_verb(verb: &str) -> bool {
    matches!(
        verb,
        "send"
            | "key"
            | "ctrl"
            | "feed"
            | "feed-bin"
            | "mouse"
            | "paste"
            | "paste-bin"
            | "turn"
            | "focus"
    )
}

/// An `edit … wait=0` request's ticket, marked for as long as its lane serves it.
#[derive(Debug)]
pub(crate) struct Unwaited(Ticket);

impl Unwaited {
    /// Mark `ticket` before its request is posted to the main thread, whose answer comes after its view has opened.
    pub(crate) fn mark(ticket: Ticket) -> Self {
        edit_hold().lock_or_recover().unwaited.insert(ticket);
        Self(ticket)
    }
}

impl Drop for Unwaited {
    fn drop(&mut self) {
        edit_hold().lock_or_recover().unwaited.remove(&self.0);
    }
}

/// Ticket `hold.ticket` holds `session` until cleared.
pub(crate) fn set(session: u64, hold: Hold) {
    let mut holds = edit_hold().lock_or_recover();
    if holds.unwaited.contains(&hold.ticket) {
        return;
    }
    let entries = holds.sessions.entry(session).or_default();
    match entries
        .iter_mut()
        .find(|entry| entry.hold.ticket == hold.ticket)
    {
        Some(entry) => {
            entry.hold = hold;
            entry.expires = None;
        }
        None => entries.push(Entry {
            hold,
            expires: None,
        }),
    }
}

/// Ticket `ticket` no longer holds `session`.
pub(crate) fn clear(session: u64, ticket: Ticket) {
    let mut holds = edit_hold().lock_or_recover();
    if let Some(entries) = holds.sessions.get_mut(&session) {
        entries.retain(|entry| entry.hold.ticket != ticket);
        if entries.is_empty() {
            holds.sessions.remove(&session);
        }
    }
}

/// `ticket`'s hold on `session` lapses at `expires` (`None`: it stands).
pub(crate) fn set_expiry(session: u64, ticket: Ticket, expires: Option<Instant>) {
    if let Some(entry) = edit_hold()
        .lock_or_recover()
        .sessions
        .get_mut(&session)
        .and_then(|entries| entries.iter_mut().find(|entry| entry.hold.ticket == ticket))
    {
        entry.expires = expires;
    }
}

/// What holds `session` now, if anything.
pub(crate) fn get(session: u64) -> Option<Hold> {
    get_at(session, Instant::now())
}

/// `get` as of `now`.
pub(crate) fn get_at(session: u64, now: Instant) -> Option<Hold> {
    let mut holds = edit_hold().lock_or_recover();
    live(&mut holds.sessions, session, now)
        .last()
        .map(|entry| entry.hold)
}

/// How many tickets hold `session` now (`status editing=<n>`).
pub(crate) fn count(session: u64) -> usize {
    count_at(session, Instant::now())
}

/// `count` as of `now`.
pub(crate) fn count_at(session: u64, now: Instant) -> usize {
    let mut holds = edit_hold().lock_or_recover();
    live(&mut holds.sessions, session, now).len()
}

/// The holds standing at `now` through one of `views`, as `(session, ticket)` pairs ascending.
pub(crate) fn holds_through(views: &[u64], now: Instant) -> Vec<(u64, Ticket)> {
    let mut holds = edit_hold().lock_or_recover();
    let sessions: Vec<u64> = holds.sessions.keys().copied().collect();
    let mut through: Vec<(u64, Ticket)> = Vec::new();
    for session in sessions {
        through.extend(
            live(&mut holds.sessions, session, now)
                .iter()
                .filter(|entry| views.contains(&entry.hold.view))
                .map(|entry| (session, entry.hold.ticket)),
        );
    }
    drop(holds);
    through.sort_unstable();
    through
}

/// `session`'s holds standing at `now`, the lapsed ones dropped from the map.
fn live(sessions: &mut BTreeMap<u64, Vec<Entry>>, session: u64, now: Instant) -> &[Entry] {
    let Some(entries) = sessions.get_mut(&session) else {
        return &[];
    };
    entries.retain(|entry| entry.expires.is_none_or(|at| at > now));
    if entries.is_empty() {
        sessions.remove(&session);
        return &[];
    }
    sessions.get(&session).map_or(&[], Vec::as_slice)
}

/// The answer to `verb` (argument tail `rest`) while an edit holds the session it types into.
pub(crate) fn refusal(
    verb: &str,
    rest: &str,
    session: Option<u64>,
    front: Option<u64>,
) -> Option<String> {
    let typed_into = if holds_verb(verb) {
        session
    } else if front.is_some()
        && crate::input_stall::writes_input(input_of(verb), verb, rest.as_bytes())
        && !reads_the_pointer(verb, rest)
    {
        front
    } else {
        None
    };
    let hold = get(typed_into?)?;
    Some(format!(
        "ERR busy editing ticket={} view={}\n",
        hold.ticket, hold.view
    ))
}

/// `pointer` or `pointer status` (`control_input::parse_pointer`'s
/// `PointerAction::Status`): a read of where the pointer is, which stays
/// answerable as every read does.
fn reads_the_pointer(verb: &str, rest: &str) -> bool {
    verb == "pointer" && matches!(rest.split_whitespace().next(), None | Some("status"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SESSION: u64 = u64::MAX - 7;
    const REFUSING: u64 = u64::MAX - 8;
    #[cfg(unix)]
    const HARNESS: u64 = u64::MAX - 10;
    const FRONT: u64 = u64::MAX - 12;

    /// [`refusal`] for a session verb aimed at `session`, nothing on screen.
    fn typed(verb: &str, session: u64) -> Option<String> {
        refusal(verb, "", Some(session), None)
    }

    #[test]
    fn edit_hold_is_set_cleared_by_its_own_ticket_and_lapses_at_expiry() {
        let first = Hold {
            ticket: Ticket(1),
            view: 10,
        };
        let second = Hold {
            ticket: Ticket(2),
            view: 20,
        };
        set(SESSION, first);
        assert_eq!(get(SESSION), Some(first));
        set(SESSION, second);
        assert_eq!(get(SESSION), Some(second), "the latest ticket is named");
        assert_eq!(count(SESSION), 2, "both hold it");
        clear(SESSION, first.ticket);
        assert_eq!(get(SESSION), Some(second), "only its own ticket clears it");
        assert_eq!(count(SESSION), 1);

        let now = Instant::now();
        set_expiry(SESSION, first.ticket, Some(now));
        assert_eq!(
            get_at(SESSION, now),
            Some(second),
            "another ticket's expiry"
        );
        set_expiry(SESSION, second.ticket, Some(now + Duration::from_secs(5)));
        assert_eq!(get_at(SESSION, now), Some(second), "not lapsed yet");
        set_expiry(SESSION, second.ticket, None);
        assert_eq!(
            get_at(SESSION, now + Duration::from_secs(60)),
            Some(second),
            "a returned waiter stops the clock"
        );
        set_expiry(SESSION, second.ticket, Some(now + Duration::from_secs(5)));
        assert_eq!(get_at(SESSION, now + Duration::from_secs(5)), None);
        assert_eq!(get(SESSION), None, "a lapsed hold is gone");
        assert_eq!(count(SESSION), 0);
        clear(SESSION, second.ticket);
    }

    /// Two tickets of one session: the first to resolve leaves the other's
    /// hold standing, and a lapsed one stops counting while the other holds.
    #[test]
    fn a_session_stays_held_while_any_of_its_tickets_holds_it() {
        const TWO: u64 = u64::MAX - 11;
        let older = Hold {
            ticket: Ticket(11),
            view: 110,
        };
        let newer = Hold {
            ticket: Ticket(12),
            view: 120,
        };
        set(TWO, older);
        set(TWO, newer);
        clear(TWO, newer.ticket);
        assert_eq!(get(TWO), Some(older), "the older edit still holds it");
        set(TWO, newer);
        let now = Instant::now();
        set_expiry(TWO, older.ticket, Some(now));
        assert_eq!(count_at(TWO, now), 1, "the orphan lapsed");
        assert_eq!(get_at(TWO, now), Some(newer));
        set(TWO, newer);
        set_expiry(TWO, newer.ticket, Some(now));
        set(TWO, newer);
        assert_eq!(get_at(TWO, now + Duration::from_secs(60)), Some(newer));
        clear(TWO, newer.ticket);
        assert_eq!(get(TWO), None);
    }

    /// `doc ls`'s tickets: every standing hold through the views asked
    /// about, across sessions, each with its session and ascending.
    #[test]
    fn holds_through_names_the_standing_holds_through_the_views() {
        const ONE: u64 = u64::MAX - 13;
        const OTHER: u64 = u64::MAX - 14;
        const VIEW: u64 = u64::MAX - 1_000;
        const SIBLING: u64 = u64::MAX - 1_001;
        const ELSEWHERE: u64 = u64::MAX - 1_002;
        let hold = |ticket, view| Hold {
            ticket: Ticket(ticket),
            view,
        };
        set(ONE, hold(31, VIEW));
        set(OTHER, hold(30, SIBLING));
        set(OTHER, hold(32, ELSEWHERE));
        let now = Instant::now();
        assert_eq!(
            holds_through(&[VIEW, SIBLING], now),
            vec![(OTHER, Ticket(30)), (ONE, Ticket(31))]
        );
        assert_eq!(holds_through(&[ELSEWHERE], now), vec![(OTHER, Ticket(32))]);
        set_expiry(ONE, Ticket(31), Some(now));
        assert_eq!(
            holds_through(&[VIEW, SIBLING], now),
            vec![(OTHER, Ticket(30))],
            "a lapsed hold names no ticket"
        );
        clear(OTHER, Ticket(30));
        clear(OTHER, Ticket(32));
        assert_eq!(holds_through(&[VIEW, SIBLING, ELSEWHERE], now), Vec::new());
    }

    /// Every verb that types into a held session is refused `ERR busy
    /// editing`, naming the ticket and view; reads, `signal` and the verbs
    /// that only open or wait on views are not.
    #[test]
    fn held_session_refuses_send_key_paste_turn_with_busy_editing() {
        let typing = [
            "send",
            "key",
            "ctrl",
            "feed",
            "feed-bin",
            "mouse",
            "paste",
            "paste-bin",
            "turn",
            "focus",
        ];
        for verb in typing {
            assert_eq!(typed(verb, REFUSING), None, "{verb}: nothing holds it");
        }
        set(
            REFUSING,
            Hold {
                ticket: Ticket(41),
                view: 7,
            },
        );
        for verb in typing {
            assert_eq!(
                typed(verb, REFUSING).as_deref(),
                Some("ERR busy editing ticket=41 view=7\n"),
                "{verb}"
            );
        }
        for verb in [
            "signal", "status", "text", "screen", "meta", "lease", "edit", "await", "resize",
            "hwkey", "pointer", "invoke",
        ] {
            assert_eq!(typed(verb, REFUSING), None, "{verb} stays answerable");
        }
        assert_eq!(
            typed("send", REFUSING - 100),
            None,
            "another session is not held"
        );
        clear(REFUSING, Ticket(41));
        for verb in typing {
            assert_eq!(typed(verb, REFUSING), None, "{verb}: the hold cleared");
        }
    }

    /// The App verbs that type (`hwkey`, `pointer`, `invoke Paste`) type into
    /// the terminal on screen, so they are refused while that terminal's
    /// session is held, and go on while a native view (the edit's own) is in
    /// front.
    #[test]
    fn app_verbs_that_type_are_refused_while_the_front_terminal_is_held() {
        set(
            FRONT,
            Hold {
                ticket: Ticket(61),
                view: 8,
            },
        );
        let busy = Some("ERR busy editing ticket=61 view=8\n".to_string());
        for (verb, rest) in [("hwkey", "a"), ("pointer", "click"), ("invoke", "Paste")] {
            assert_eq!(
                refusal(verb, rest, None, Some(FRONT)),
                busy,
                "{verb} {rest}"
            );
            assert_eq!(
                refusal(verb, rest, None, None),
                None,
                "{verb} {rest}: a native view is in front"
            );
            assert_eq!(
                refusal(verb, rest, None, Some(FRONT - 100)),
                None,
                "{verb} {rest}: an unheld terminal is in front"
            );
        }
        assert_eq!(refusal("invoke", "NewTab", None, Some(FRONT)), None);
        for rest in ["", "status"] {
            assert_eq!(
                refusal("pointer", rest, None, Some(FRONT)),
                None,
                "`pointer {rest}` reads"
            );
        }
        assert_eq!(
            refusal("send", "x", Some(FRONT - 100), Some(FRONT)),
            None,
            "a session verb asks its own target"
        );
        assert_eq!(refusal("send", "x", Some(FRONT), None), busy);
        clear(FRONT, Ticket(61));
        assert_eq!(refusal("hwkey", "a", None, Some(FRONT)), None);
    }

    /// Through the App's own ticket seams: a session is refused from the
    /// moment its ticket's view opens, and the refusal ends when the ticket
    /// resolves (its view closed) and when it is orphaned (its waiter hung up
    /// and none came back within [`crate::app_edit::ORPHAN_AFTER`]); the
    /// orphan keeps its view.
    #[test]
    fn the_hold_clears_on_resolve_and_on_orphan() {
        use amacs::protocol::edit::{AwaitKey, EditOk, EditRequest};

        use crate::control_edit::{AwaitCall, EditCall, WaiterId};

        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-edit-hold-app-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("COMMIT_EDITMSG");
        std::fs::write(&path, b"draft\n").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = crate::App::headless_for_test();
        app.serve_edits_headless_for_test();
        let wid = crate::WindowId(0);
        let sid = 1 << 45;
        app.next_session_id = sid;
        app.push_stub_tab(wid, crate::stub_session(sid));
        let caller_tab = app.windows[&wid].tab_set.active_id().unwrap();

        let edit_and_wait = |app: &mut crate::App, waiter: u64| {
            let ticket = crate::control_edit::mint_ticket();
            let (reply, answer) = std::sync::mpsc::channel();
            app.begin_edit(EditCall {
                ticket,
                session: sid,
                request: EditRequest {
                    uri: uri.clone(),
                    line: None,
                    create: false,
                    for_word: None,
                    wait: true,
                },
                real_uri: crate::control_edit::real_file_uri(&uri),
                reply,
            });
            let ok = EditOk::parse(&answer.try_recv().expect("answered inline")).unwrap();
            let (reply, done) = std::sync::mpsc::channel();
            let waiter = WaiterId(waiter);
            app.await_edit(AwaitCall {
                waiter,
                key: AwaitKey::Ticket(ticket),
                real_uri: None,
                reply,
            });
            (ok, waiter, done)
        };
        let busy =
            |ok: &EditOk| format!("ERR busy editing ticket={} view={}\n", ok.ticket, ok.view);

        let (ok, _, done) = edit_and_wait(&mut app, u64::MAX - 1);
        for verb in ["send", "key", "turn", "paste"] {
            assert_eq!(typed(verb, sid), Some(busy(&ok)), "{verb} while open");
        }
        assert_eq!(count(sid), 1);
        app.close_active_native_tab(wid).unwrap();
        assert!(
            done.try_recv()
                .unwrap()
                .starts_with("OK edit done result=closed")
        );
        assert_eq!(typed("send", sid), None, "the ticket resolved");
        assert_eq!(count(sid), 0);

        assert!(
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .switch_to(caller_tab)
        );
        app.sync_window(wid);
        let (ok, waiter, _done) = edit_and_wait(&mut app, u64::MAX - 2);
        assert_eq!(typed("turn", sid), Some(busy(&ok)), "held while waited on");
        let left = Instant::now();
        app.withdraw_edit_waiter_at(waiter, left);
        assert_eq!(get_at(sid, left).map(|hold| hold.ticket), Some(ok.ticket));
        app.sweep_orphaned_edits(left + crate::app_edit::ORPHAN_AFTER);
        assert_eq!(typed("turn", sid), None, "the orphan's hold is gone");
        assert!(
            app.view_store
                .get(crate::tab_model::ViewId::from_stored(ok.view))
                .is_some(),
            "an orphan keeps its view"
        );
    }

    /// `edit … wait=0` (`amacs --no-wait`) opens its view holding nothing:
    /// through the real lane ([`crate::control_edit::serve_edit`]) and the
    /// App's own admission, the caller's session stays typeable and
    /// `editing=` does not count it; a waiter that adopts the ticket later
    /// holds the session as any waiter does.
    #[test]
    fn an_edit_nobody_waits_on_holds_nothing() {
        use amacs::protocol::edit::{AwaitKey, EditOk};

        use crate::control::control_media::{Hops, MainPost, take_hop};
        use crate::control_edit::{AwaitCall, WaiterId};

        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-edit-unwaited-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("plan.md");
        std::fs::write(&path, b"plan\n").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut app = crate::App::headless_for_test();
        app.serve_edits_headless_for_test();
        let wid = crate::WindowId(0);
        let sid = 1 << 46;
        app.next_session_id = sid;
        app.push_stub_tab(wid, crate::stub_session(sid));
        let edit = |app: &mut crate::App, tail: &str| {
            let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
            let line = crate::control_edit::serve_edit(
                crate::control::Scope::Owner,
                Some(sid),
                &format!("{uri}{tail}"),
                &mut MainPost::injected(
                    |wake| match take_hop(wake) {
                        crate::Wake::EditRequest(call) => {
                            app.begin_edit(*call);
                            true
                        }
                        other => panic!("unexpected {other:?}"),
                    },
                    hops,
                ),
                &mut || false,
                Duration::from_secs(60),
                |left| left.min(Duration::from_millis(5)),
            );
            EditOk::parse(&line).unwrap_or_else(|why| panic!("{line}: {why}"))
        };

        let unwaited = edit(&mut app, " wait=0");
        assert!(
            app.view_store
                .get(crate::tab_model::ViewId::from_stored(unwaited.view))
                .is_some(),
            "its view opened"
        );
        assert_eq!(typed("send", sid), None, "nothing waits on it");
        assert_eq!(count(sid), 0, "editing=0");

        let waited = edit(&mut app, "");
        assert_eq!(
            get(sid).map(|hold| hold.ticket),
            Some(waited.ticket),
            "a request that will be waited on holds its session at once"
        );
        assert_eq!(count(sid), 1);

        let (reply, _done) = std::sync::mpsc::channel();
        app.await_edit(AwaitCall {
            waiter: WaiterId(u64::MAX - 3),
            key: AwaitKey::Ticket(unwaited.ticket),
            real_uri: None,
            reply,
        });
        assert_eq!(count(sid), 2, "an adopted ticket holds as any does");
        clear(sid, unwaited.ticket);
        clear(sid, waited.ticket);
    }

    /// THE HARNESS BACKS OFF FROM THE HOLD (design §6.7): the refusal reaches
    /// the harness's own transport (`RelayCtl`, which the hosted loop's
    /// `HostCtl` wraps) as an `ERR busy` reply, the transient class its
    /// `write_refused` backs off from and decides again, never `ERR halted`
    /// (a hold's park) or a failure that ends the loop; and the write it
    /// refused goes out once the ticket resolves.
    #[cfg(unix)]
    #[test]
    fn the_harness_transport_reads_the_edit_hold_as_a_back_off() {
        use std::io::{BufRead, BufReader, Write};

        use aterm_agent::supervise::{Ctl as _, Endpoint, RelayCtl};

        let dir = crate::test_scratch::short_dir("the scratch server's socket");
        let sock = dir.path().join("a.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            'conns: while let Ok((stream, _)) = listener.accept() {
                let mut w = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { continue 'conns };
                    if line == "BYE" {
                        break 'conns;
                    }
                    if line.starts_with("AUTH ") {
                        continue;
                    }
                    let verb = line
                        .split_whitespace()
                        .find(|word| !word.starts_with('@'))
                        .unwrap_or_default();
                    let reply = typed(verb, HARNESS).unwrap_or_else(|| "OK\n".to_string());
                    let _ = w.write_all(reply.as_bytes());
                    seen.push(format!("{line} -> {}", reply.trim_end()));
                }
            }
            seen
        });

        let endpoint = Endpoint::Socket(sock.to_string_lossy().into_owned());
        let mut ctl = RelayCtl::new(endpoint, Some("tok".to_string()));
        set(
            HARNESS,
            Hold {
                ticket: Ticket(77),
                view: 3,
            },
        );
        let refused = ctl
            .call(&["@s-h", "turn", "idle=600", "keep going"])
            .unwrap();
        assert!(!refused.ok(), "{refused:?}");
        assert!(refused.is_err("busy"), "a back-off: {refused:?}");
        assert!(!refused.is_err("halted"), "not a hold's park: {refused:?}");
        assert_eq!(refused.err_text(), "ERR busy editing ticket=77 view=3");
        assert!(ctl.call(&["@s-h", "status"]).unwrap().ok(), "reads go on");
        clear(HARNESS, Ticket(77));
        let typed = ctl
            .call(&["@s-h", "turn", "idle=600", "keep going"])
            .unwrap();
        assert!(typed.ok(), "typed once the ticket resolved: {typed:?}");
        drop(ctl);
        let mut bye = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        bye.write_all(b"BYE\n").unwrap();
        assert_eq!(
            server.join().unwrap(),
            [
                "@s-h turn idle=600 keep going -> ERR busy editing ticket=77 view=3",
                "@s-h status -> OK",
                "@s-h turn idle=600 keep going -> OK",
            ]
        );
    }
}
