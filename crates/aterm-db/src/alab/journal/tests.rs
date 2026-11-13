// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal publisher's tests (design §12, step 8): the event vocabulary's privacy
//! property, the `use` and `relay` rows' caps and truncation priority, the pace, the weekly
//! carry, and the flusher end to end through Commons (shares, relay across a deleted week, the
//! heard archive, a refused slot kept local).

use super::{
    book::{self, BURST, Book, Facts, WEEK},
    event::MAX_EVENT,
    flush::{self, Flushed},
    relay::{Entry, Heard, Rk, believed},
    *,
};
use crate::{
    catalog::{Directory, MAX_TABLES, ring::Used},
    table::{
        Scope, Session,
        store::Store,
        tests::{Lab, repair_commons_at},
    },
    typed::{
        keys,
        owned::{self, caps, relay_bucket, relay_duty},
    },
};
use aterm_codec::hex;
use aterm_json::{Value, json};
use aterm_secrets::{client::Paths, commons::roster::Role};

/// A fixed week: day 20,000 since the Unix epoch falls in it.
const W: u64 = 20_000 / 7;

fn pk(email: &str) -> Pk {
    Pk::new(&keys::account_pk("claude", email).unwrap()).unwrap()
}

fn ev(t: u64, kind: Kind, account: Pk) -> Event {
    Event {
        t,
        kind,
        account: Some(account),
        cli: Word::new("claude").unwrap(),
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        plan: None,
        src: Source::CliOutput,
        why: None,
        to: None,
        tab: None,
        harness: None,
    }
}

fn quota(t: u64, account: Pk, window: Window, pct: u16) -> Event {
    Event {
        window: Some(window),
        pct_x10: Some(pct),
        reset: Some(t + 3600),
        res: Some(Res::Exact),
        src: Source::Statusline,
        ..ev(t, Kind::Quota, account)
    }
}

fn set(value: &mut Value, key: &str, to: Value) {
    value.as_object_mut().unwrap().insert(key.to_owned(), to);
}

/// A small deterministic generator for the property tests (xorshift64*).
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A string of up to 24 bytes drawn from letters, digits and the characters free text,
    /// addresses, URLs and paths are made of.
    fn text(&mut self) -> String {
        const ALPHABET: &[u8] = b"abcxyzABZ019-_@.:/ ?=&%+~\"\\\n";
        let len = (self.next() % 24) as usize + 1;
        (0..len)
            .map(|_| ALPHABET[(self.next() % ALPHABET.len() as u64) as usize] as char)
            .collect()
    }
}

/// PRIVACY PROPERTY (design §12, step 8): no word an event can hold carries an address, URL,
/// path or free text. Over 20,000 generated strings, every one a `Word`, `Pk` or the decoder
/// admits is `[a-z0-9-]` alone, and every encoded event is made of those bytes and JSON's
/// punctuation; an event with an unknown key (an `email` field a modified peer adds) or a
/// word holding `@` is refused whole.
/// NEGATIVE CONTROL: with `Word::new` also admitting `@` (`|| *b == b'@'`), a generated
/// address-like word is admitted: `` admitted "c@" `` (the first property's message).
#[test]
fn no_event_word_can_carry_an_address_url_or_free_text() {
    let mut generator = Gen(0x5eed_1234_abcd_0001);
    let plain = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let mut admitted = 0;
    for _ in 0..20_000 {
        let text = generator.text();
        if let Some(word) = Word::<16>::new(&text) {
            assert!(plain(word.as_str()), "admitted {:?}", word.as_str());
            admitted += 1;
        }
        assert!(Pk::new(&text).is_none(), "a generated string is no key");
        let mut value = ev(1, Kind::Auth, pk("ann@example.test")).to_value();
        set(&mut value, "why", Value::from(text.as_str()));
        if let Some(event) = Event::from_value(&value) {
            assert!(plain(event.why.unwrap().as_str()), "decoded {text:?}");
        }
    }
    assert!(admitted > 0, "the generator also makes plain words");
    // The widest event a producer makes: a sub-account's reading, ten-digit times.
    let event = Event {
        cli: Word::new("codex-alab").unwrap(),
        sub: Word::new("0123456789ab"),
        res: Some(Res::Minute),
        src: Source::SessionUsage,
        ..quota(
            9_999_999_999 - 3600,
            pk("ann@example.test"),
            Window::WeekModel,
            1000,
        )
    };
    let mut buf = [0; MAX_EVENT];
    let n = event.encode(&mut buf).unwrap();
    assert!(
        buf[..n]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-\":,{}".contains(b)),
        "{}",
        String::from_utf8_lossy(&buf[..n])
    );
    assert_eq!(Event::from_line(&buf[..n]), Some(event));
    // One past the cap is refused whole, never cut mid-word.
    let wider = Event {
        why: Word::new("reset-failed-now"),
        to: Some(pk("bob@example.test")),
        plan: Word::new("max-20x-team"),
        ..event
    };
    assert_eq!(wider.encode(&mut [0; 4 * MAX_EVENT]), None);
    let mut extra = event.to_value();
    set(&mut extra, "email", json!("ann"));
    assert_eq!(Event::from_value(&extra), None, "an unknown key is refused");
    let mut at = event.to_value();
    set(&mut at, "c", json!("ann@proton.example"));
    assert_eq!(Event::from_value(&at), None, "an address is refused");
}

/// The journal's schemas compile, and every reference in them names a collection the registry
/// still holds, so a registry change that removes a collection (revision 2 removed `seat`)
/// cannot leave a journal row pointing at it. The journal holds no address column.
/// NEGATIVE CONTROL: with `PLACE` given back its revision-1 `seat` column
/// (`Column::new("seat", Ty::Ref("seat"))`), the reference is refused:
/// `` left: ["place.seat -> seat"] right: [] ``.
#[test]
fn journal_references_name_live_registry_collections() {
    fn refs(ty: &crate::typed::Ty, out: &mut Vec<&'static str>) {
        use crate::typed::Ty;
        match ty {
            Ty::Ref(collection) => out.push(collection),
            Ty::Set(inner) | Ty::List(inner, _) => refs(inner, out),
            Ty::Struct(members) => members.iter().for_each(|m| refs(&m.ty, out)),
            _ => {}
        }
    }
    let registry: Vec<&str> = crate::alab::registry::COLLECTIONS
        .iter()
        .map(|c| c.name)
        .collect();
    let mut dangling = Vec::new();
    for collection in JOURNAL.collections {
        assert_eq!(collection.check(), Ok(()), "{}", collection.name);
        for column in collection.columns {
            assert!(!column.ty.pii(), "{}.{}", collection.name, column.name);
            let mut named = Vec::new();
            refs(&column.ty, &mut named);
            dangling.extend(
                named
                    .into_iter()
                    .filter(|target| !registry.contains(target))
                    .map(|target| format!("{}.{} -> {target}", collection.name, column.name)),
            );
        }
    }
    assert_eq!(dangling, Vec::<String>::new());
}

/// The worst-case `use` row fits its 1,536 B cap whole, and over a smaller cap it keeps
/// standing walls first, then readings newest first, and drops lapsed walls first (design
/// §4.17, the `fit` priority).
/// NEGATIVE CONTROL: with `use_row` sorting readings oldest first (`sort_by_key(|e| e.t)`),
/// the newest reading is the one dropped: `` the newest reading stays `` (its message).
#[test]
fn a_use_row_fits_its_cap_in_the_stated_priority() {
    let ann = pk("ann@example.test");
    let now = W * WEEK + 3600;
    let mut facts = Facts::default();
    let mut t = now - 600;
    for (n, window) in Window::ALL.iter().enumerate() {
        t += 1;
        facts.fold(&Event {
            sub: Word::new("0123456789ab"),
            plan: Word::new("max-20x"),
            ..quota(t, ann, *window, 1000 - n as u16)
        });
        // Standing walls for half the windows, lapsed ones for the rest.
        let until = if n % 2 == 0 { now + 86_400 } else { now - 60 };
        facts.fold(&Event {
            until: Some(until),
            window: Some(*window),
            ..ev(t, Kind::Wall, ann)
        });
    }
    facts.fold(&Event {
        plan: Word::new("max-20x"),
        ..ev(t, Kind::PlanSeen, ann)
    });
    facts.fold(&ev(t, Kind::SignedIn, ann));
    let whole = facts
        .use_row(u64::MAX, u64::MAX, now, caps::USE_ROW)
        .unwrap();
    let bytes = aterm_json::to_vec(&whole).unwrap().len();
    assert!(bytes <= caps::USE_ROW, "{bytes}");
    assert_eq!(whole["quota"].as_array().unwrap().len(), 6);
    assert_eq!(whole["wall"].as_array().unwrap().len(), 6);
    // A cap that holds the base, the standing walls and some readings.
    let tight = bytes - 300;
    let row = facts.use_row(u64::MAX, u64::MAX, now, tight).unwrap();
    assert!(aterm_json::to_vec(&row).unwrap().len() <= tight);
    let walls = row["wall"].as_array().unwrap();
    assert!(
        walls.iter().all(|w| w["until"].as_u64().unwrap() > now),
        "lapsed walls go first: {walls:?}"
    );
    assert_eq!(walls.len(), 3, "every standing wall stays");
    let readings: Vec<u64> = row["quota"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["t"].as_u64().unwrap())
        .collect();
    assert!(readings.len() < 6, "some readings go");
    assert!(readings.contains(&t), "the newest reading stays");
    assert!(readings.windows(2).all(|p| p[0] > p[1]), "newest first");
}

/// A relay row over its cap drops the oldest `signout` entries, then the oldest `signin`
/// entries, and keeps every account's newest `use` (design §4.17); the worst case (every
/// account of a bucket, 16 B uuids, 20-digit times) fits 12 KiB after truncation.
/// NEGATIVE CONTROL: with `Rk` declared `SignOut, SignIn, Use` (its derived order), the
/// truncation drops uses first: `` every use stays: ["signout", … left: 0 right: 27 ``.
#[test]
fn a_relay_row_keeps_uses_last_of_all() {
    let mut heard = Heard::default();
    let bucket = relay_bucket(pk("a0@example.test").as_str());
    let p = W;
    let mut accounts = Vec::new();
    for n in 0..400 {
        let account = pk(&format!("a{n}@example.test"));
        if relay_bucket(account.as_str()) != bucket {
            continue;
        }
        accounts.push(account);
        for (i, kind) in [Rk::Use, Rk::SignIn, Rk::SignOut].into_iter().enumerate() {
            heard.offer(
                account,
                kind,
                Entry {
                    m: [n as u8; 16],
                    t: p * WEEK + n * 10 + i as u64,
                    p,
                },
            );
        }
    }
    assert!(accounts.len() > 10);
    let row = heard
        .row(bucket, u64::MAX, u64::MAX, caps::RELAY_ROW)
        .unwrap()
        .unwrap();
    assert!(aterm_json::to_vec(&row).unwrap().len() <= caps::RELAY_ROW);
    let small = heard.row(bucket, 1, p, 4096).unwrap().unwrap();
    assert!(aterm_json::to_vec(&small).unwrap().len() <= 4096);
    let kinds: Vec<&str> = small["e"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["k"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "use").count(),
        accounts.len(),
        "every use stays: {kinds:?}"
    );
    assert!(!kinds.contains(&"signout"), "signouts go first: {kinds:?}");
}

/// Two Macs filling the same uncovered bucket settle on the one whose uuid sorts lower: it sees
/// the other's row and keeps relaying, while the other sees its row and stands down, so the
/// bucket is relayed every week instead of both dropping it at once. A Mac whose own two buckets
/// include it covers it for anyone.
/// NEGATIVE CONTROL: with any other Mac's row covering its bucket (`writer != *me` alone), the
/// lower Mac drops the gap too: `` the lower Mac keeps the gap … ``.
#[test]
fn two_macs_filling_one_gap_settle_on_the_lower() {
    let (low, high) = ([1u8; 16], [2u8; 16]);
    let gap = (0..caps::RELAY_BUCKETS)
        .find(|b| !relay_duty(&low).contains(b) && !relay_duty(&high).contains(b))
        .unwrap();
    let row = json!({"r": 1, "n": 1, "period": W, "b": gap, "e": []});
    let mut seen_by_high = Heard::default();
    seen_by_high.read_relay(low, &high, &row, W);
    assert!(
        !seen_by_high.duty(&high).contains(&gap),
        "the higher Mac stands down"
    );
    let mut seen_by_low = Heard::default();
    seen_by_low.read_relay(high, &low, &row, W);
    assert!(
        seen_by_low.duty(&low).contains(&gap),
        "the lower Mac keeps the gap: {:?}",
        seen_by_low.covered
    );
    let owner = (3..=u8::MAX)
        .map(|n| [n; 16])
        .find(|m| relay_duty(m).contains(&gap))
        .unwrap();
    seen_by_low.read_relay(owner, &low, &row, W);
    assert!(
        !seen_by_low.duty(&low).contains(&gap),
        "its owner covers it"
    );
}

/// A relayed time is believed only within the week of the table it was first read from ± 1
/// (design §4.17), so a wrong clock cannot carry a fact forward.
/// NEGATIVE CONTROL: with `believed` returning true always, a time two weeks past its table's
/// week is believed: `` assertion failed: !believed((W + 2) * WEEK, W) ``.
#[test]
fn a_relayed_time_is_believed_only_near_its_week() {
    assert!(believed(W * WEEK, W));
    assert!(believed((W - 1) * WEEK, W));
    assert!(believed((W + 2) * WEEK - 1, W));
    assert!(!believed((W + 2) * WEEK, W));
    assert!(!believed((W - 1) * WEEK - 1, W));
    let mut heard = Heard::default();
    heard.offer(
        pk("ann@example.test"),
        Rk::Use,
        Entry {
            m: [1; 16],
            t: (W + 3) * WEEK,
            p: W,
        },
    );
    assert!(heard.newest.is_empty());
}

/// A relay entry claiming a source week later than the table that relays it is dropped: a
/// modified build (or a clock a week ahead) cannot plant a future "last use" that outranks
/// every real one and then rides each honest Mac's weekly restatement for good. The same entry
/// read from a table of its own week stands.
/// NEGATIVE CONTROL: with the source week unchecked (`&& entry.p <= period` removed), the
/// planted entry names the account's last use: `` left: [2, 2, … right: [1, 1, … ``.
#[test]
fn a_relay_entry_from_after_its_table_is_dropped() {
    let ann = pk("ann@example.test");
    let bucket = relay_bucket(ann.as_str());
    let entry = |m: u8, p: u64, t: u64| json!({"a": ann.as_str(), "k": "use", "m": hex::encode_lower(&[m; 16]), "p": p, "t": t});
    let honest =
        json!({"r": 1, "n": 1, "period": W, "b": bucket, "e": [entry(1, W, W * WEEK + 10)]});
    let planted = json!({"r": 1, "n": 1, "period": W, "b": bucket,
        "e": [entry(2, W + 5, (W + 5) * WEEK)]});
    let me = [9; 16];
    let mut heard = Heard::default();
    heard.read_relay([1; 16], &me, &honest, W);
    heard.read_relay([2; 16], &me, &planted, W);
    assert_eq!(heard.newest[&(ann, Rk::Use)].m, [1; 16]);
    heard.read_relay([2; 16], &me, &planted, W + 5);
    assert_eq!(heard.newest[&(ann, Rk::Use)].m, [2; 16]);
}

/// The pace line (design §3.9): a Mac may spend [`BURST`] records at once, then its share
/// grows evenly to 54 by the week's end; a flush is due for the week's first record even idle,
/// otherwise under the line for a sharp event, 128 waiting events, or six hours with something
/// waiting. The outbox keeps each event once, dropping those past 30 days.
/// NEGATIVE CONTROL: with `under_pace` ignoring the burst (`line = (share - BURST) * …`), a
/// new week's first records are refused: `` assertion failed: Book::under_pace(&spent(0, 0),
/// 0, 0) ``.
#[test]
fn the_pace_line_and_what_is_due() {
    assert!(Book::under_pace(&spent(0, 0), 0, 0));
    assert!(Book::under_pace(&spent(BURST - 1, 0), 0, 0));
    assert!(!Book::under_pace(&spent(BURST, 0), 0, 0));
    assert!(Book::under_pace(&spent(53, 0), WEEK, 0));
    assert!(!Book::under_pace(&spent(54, 0), WEEK, 0));
    assert!(Book::under_pace(&spent(30, 0), WEEK / 2, 0));
    assert!(!Book::under_pace(&spent(31, 0), WEEK / 2, 0));
    // The byte line is the record line's fraction of the byte share: 8/54 of 864 KiB at once.
    let burst_bytes = BYTE_SHARE * BURST / JOURNAL_SHARES_RECORDS;
    assert!(Book::under_pace(&spent(2, 0), 0, burst_bytes));
    assert!(!Book::under_pace(&spent(2, 1), 0, burst_bytes));
    assert!(Book::under_pace(&spent(53, BYTE_SHARE - 1), WEEK, 1));
    assert!(!Book::under_pace(&spent(53, BYTE_SHARE), WEEK, 1));
    assert_eq!(Book::paced_at(&spent(0, 0), 1), Some(0));
    let at = Book::paced_at(&spent(BURST, 0), 1).unwrap();
    assert!(!Book::under_pace(&spent(BURST, 0), at - 1, 1));
    assert!(Book::under_pace(&spent(BURST, 0), at, 1));
    assert_eq!(
        Book::paced_at(&spent(54, 0), 1),
        None,
        "a full share waits for the next week"
    );
    let ann = pk("ann@example.test");
    let now = W * WEEK + 3600;
    let mut book = Book::default();
    assert!(
        book.due(&[], false, now, W, false, false),
        "a week's first record"
    );
    book.sum_period = Some(W);
    book.flushed_at = now;
    assert!(!book.due(&[], false, now + 1, W, true, false));
    let sharp = [ev(now, Kind::SignedIn, ann)];
    assert!(book.due(&sharp, false, now + 1, W, true, false));
    assert!(
        !book.due(&sharp, false, now + 1, W, false, false),
        "over the line"
    );
    let routine = [ev(now, Kind::SessionStart, ann)];
    assert!(!book.due(&routine, false, now + 60, W, true, false));
    assert!(book.due(&routine, false, now + book::ROUTINE_S, W, true, false));
    let many: Vec<Event> = (0..book::BATCH_EVENTS as u64)
        .map(|i| ev(now + i, Kind::SessionStart, ann))
        .collect();
    assert!(book.due(&many, false, now + 60, W, true, false));
    let old = ev(now - spool::KEEP_S - 1, Kind::SessionStart, ann);
    let (all, fresh) = book::merge(vec![routine[0]], &[routine[0], sharp[0], old], now);
    assert_eq!(all, vec![routine[0], sharp[0]]);
    assert_eq!(fresh, vec![sharp[0], old]);
}

/// The weekly carry (design §4.17): an unchanged `use` row is restated in each new week for
/// an account used within 180 days, and not for one used longer ago; within a week an
/// unchanged row is never restated.
/// NEGATIVE CONTROL: with `uses_due` never carrying (`let carry = false;`), the new week
/// restates nothing: `` left: [] right: [ac-295697eefaaa7d3d] ``.
#[test]
fn the_weekly_carry_restates_recent_accounts_only() {
    let ann = pk("ann@example.test");
    let bob = pk("bob@example.test");
    let now = W * WEEK + 3600;
    let mut book = Book::default();
    book.fold(&[ev(now - 60, Kind::SessionStart, ann)]);
    book.fold(&[ev(now - book::CARRY_S - WEEK, Kind::SessionStart, bob)]);
    let planned = book::Planned {
        uses: book.uses_due(1, W, now).unwrap(),
        ..Default::default()
    };
    assert_eq!(planned.uses.len(), 2, "both rows are new");
    book.wrote(&planned, 1, W, now);
    assert!(book.uses_due(2, W, now + 7200).unwrap().is_empty());
    let next = (W + 1) * WEEK + 60;
    let carried: Vec<Pk> = book
        .uses_due(3, W + 1, next)
        .unwrap()
        .into_iter()
        .map(|(pk, _)| pk)
        .collect();
    assert_eq!(carried, vec![ann]);
}

/// The spool is taken one pass late: an append lands in the file the next pass reads, never in
/// a file being read, and the flusher's intake returns each line once.
/// NEGATIVE CONTROL: with `take` removing the spool instead of renaming it
/// (`std::fs::remove_file(&self.file)`), the noted event is lost: `` left: [] right: [Event
/// { t: 5, kind: SignedIn, … }] ``.
#[test]
fn the_spool_is_taken_one_pass_late_and_each_line_once() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let event = ev(5, Kind::SignedIn, pk("ann@example.test"));
    let intake = || {
        let events = spool.absorb().unwrap();
        spool.take().unwrap();
        events
    };
    assert_eq!(spool.append(&event).unwrap(), Appended::Written);
    assert!(intake().is_empty(), "taken, read next pass");
    assert_eq!(intake(), vec![event]);
    assert!(intake().is_empty());
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(spool.dir()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

fn spools(scratch: &std::path::Path, n: usize) -> Vec<Spool> {
    (0..n)
        .map(|i| Spool::at(scratch.join(format!("m{}", i + 1)).join("alab")))
        .collect()
}

/// What a writer spent of a week's share: `records` records of `bytes` wire bytes.
fn spent(records: usize, bytes: usize) -> Used {
    Used {
        writer_records: records,
        writer_bytes: bytes,
        ..Used::default()
    }
}

/// This Mac's Commons machine uuid, the window's typed operations and the records it spent in
/// the current week's table, at `now`.
fn read(paths: &Paths, now: u64) -> ([u8; 16], Vec<owned::Op>, usize) {
    let (me, window) = window_at(paths, now);
    let used = window
        .used
        .get(&window.current)
        .map_or(0, |used| used.writer_records);
    (me, window.ops, used)
}

/// This Mac's Commons machine uuid and the journal window it holds at `now`.
fn window_at(paths: &Paths, now: u64) -> ([u8; 16], flush::Window) {
    let (bootstrap, journal) =
        aterm_secrets::commons::bootstrap::Bootstrap::required_with_journal_until(
            paths,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
    let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
    let me = session
        .members()
        .unwrap()
        .iter()
        .find(|binding| binding.actor() == session.actor())
        .map(|binding| *binding.machine())
        .unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    (me, flush::window(&session, &directory, now).unwrap())
}

fn rows(paths: &Paths, now: u64, collection: &crate::typed::Collection) -> owned::OwnedRead {
    let (bootstrap, journal) =
        aterm_secrets::commons::bootstrap::Bootstrap::required_with_journal_until(
            paths,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
    let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
    let directory = Directory::new(paths, bootstrap.trust());
    let window = flush::window(&session, &directory, now).unwrap();
    owned::read_owned(
        collection,
        &window.ops,
        owned::commons::historical(&session),
    )
}

fn heard_lines(spool: &Spool) -> Vec<Value> {
    let dir = spool.dir().join("heard");
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        lines.extend(
            text.lines()
                .map(|l| aterm_json::from_str::<Value>(l).unwrap()),
        );
    }
    lines
}

/// END TO END (design §12, step 8: carry and relay): one Mac's flush publishes its week's
/// `sum`, its `ev` batch and its `use` row for the account it used; the first Mac's record
/// also relays the use in the account's bucket, its own; the other Mac, after a Commons repair,
/// archives that batch once in its heard archive and stands down from the covered bucket, keeping
/// the facts in its book; three weeks later, with the first Mac gone and the week that held its
/// rows outside the window, the second Mac fills the gap and its relay alone still says who last
/// used the account, and when.
/// NEGATIVE CONTROL: with `relayable` not offering the book's known facts (its `book.known`
/// loop removed), the second Mac has stated no relay and knows nothing in week three, so none is
/// written:
/// `` no relay names ann's last use in week three: OwnedRead { rows: {}, inert: 0 } ``.
#[test]
fn a_peer_hears_relays_and_carries_the_last_use_past_a_deleted_week() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spools = spools(scratch.path(), 2);
    let now = W * WEEK + 3600;
    // The publisher `a` is the Mac with the lower uuid and the account sits in a bucket the other
    // Mac `b` does not own, so `a`'s relay covers it for `b` (its own bucket, or a gap both fill
    // and the lower keeps): `b` stands down once `a` relays the use, and must still carry it.
    let uuids = [
        window_at(&lab.paths[0], now).0,
        window_at(&lab.paths[1], now).0,
    ];
    let (a, b) = if uuids[0] < uuids[1] { (0, 1) } else { (1, 0) };
    let (m1, m2) = (uuids[a], uuids[b]);
    let ann = (0..)
        .map(|i| pk(&format!("ann{i}@example.test")))
        .find(|account| {
            let bucket = relay_bucket(account.as_str());
            !relay_duty(&m2).contains(&bucket)
        })
        .unwrap();
    let t0 = now - 600;
    spools[a].append(&ev(t0, Kind::SessionStart, ann)).unwrap();
    spools[a].append(&ev(t0 + 1, Kind::SignedIn, ann)).unwrap();
    let pass = |n: usize, now: u64| {
        flush::pass(&lab.paths[n], &spools[n], &Presence::default, false, now).unwrap()
    };
    assert_eq!(
        pass(a, now),
        Flushed::Wrote {
            period: W,
            events: 0,
            rows: 1
        },
        "the week's sum; the spool is read next pass"
    );
    assert_eq!(
        pass(a, now + 60),
        Flushed::Wrote {
            period: W,
            events: 2,
            rows: 2
        },
        "the batch, ann's use row and the relay of ann's bucket, heard from the batch it packs"
    );
    let (_, _, used) = read(&lab.paths[a], now + 60);
    assert_eq!(used, 2);
    let uses = rows(&lab.paths[a], now + 60, &USE);
    let row = &uses.rows[&format!("{}.{}", ann.as_str(), hex::encode_lower(&m1))].value;
    assert_eq!(row["last_start"], json!(t0));
    assert_eq!(row["signin"]["state"], json!("in"));
    let sums = rows(&lab.paths[a], now + 60, &SUM);
    assert_eq!(
        sums.rows[&hex::encode_lower(&m1)].value["journal"],
        json!("on")
    );

    repair_commons_at(&lab, 2, &[now + 60, now + 60]);
    assert_eq!(
        pass(b, now + 120),
        Flushed::Wrote {
            period: W,
            events: 0,
            rows: 1
        },
        "the second Mac's sum: the first Mac's relay covers ann's bucket"
    );
    let lines = heard_lines(&spools[b]);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["m"], json!(hex::encode_lower(&m1)));
    assert_eq!(lines[0]["e"].as_array().unwrap().len(), 2);
    // A later pass reads the tables again but archives no batch twice.
    assert_eq!(pass(b, now + 120 + book::ROUTINE_S), Flushed::Idle);
    assert_eq!(heard_lines(&spools[b]).len(), 1);

    let later = (W + 3) * WEEK + 3600;
    repair_commons_at(&lab, 1, &[later, later]);
    assert!(matches!(pass(b, later), Flushed::Wrote { period, .. } if period == W + 3));
    let relays = rows(&lab.paths[b], later, &RELAY);
    let carried = relays.rows.values().any(|row| {
        row.rank.0 == W + 3
            && row.value["e"].as_array().unwrap().iter().any(|e| {
                e["a"] == json!(ann.as_str())
                    && e["k"] == json!("use")
                    && e["m"] == json!(hex::encode_lower(&m1))
                    && e["t"] == json!(t0)
                    && e["p"] == json!(W)
            })
    });
    assert!(
        carried,
        "no relay names ann's last use in week three: {relays:?}"
    );
}

/// The book's known facts (design §11 step 12) are bounded: past [`book::MAX_KNOWN`] the oldest
/// go first, a fact older than 180 days goes at once, and what is kept survives the book's file.
/// NEGATIVE CONTROL: with `relayable` not truncating (`kept.truncate(book::MAX_KNOWN)` removed),
/// every fact stays: `` left: 1538 right: 1536 ``.
#[test]
fn the_books_known_facts_keep_the_newest_within_their_cap() {
    let me = [1; 16];
    let now = W * WEEK + 3600;
    let entry = |t: u64| Entry {
        m: [2; 16],
        t,
        p: W,
    };
    let mut heard = Heard::default();
    let extra = 2;
    for i in 0..book::MAX_KNOWN + extra {
        heard.newest.insert(
            (pk(&format!("u{i}@example.test")), Rk::Use),
            entry(now - 10_000 + i as u64),
        );
    }
    let stale = pk("stale@example.test");
    heard
        .newest
        .insert((stale, Rk::Use), entry(W * WEEK - book::CARRY_S - 1));
    let mut book = Book::default();
    flush::relayable(&mut heard, &mut book, &[], &me, W);
    assert_eq!(book.known.len(), book::MAX_KNOWN);
    assert!(!book.known.contains_key(&(stale, Rk::Use)), "past 180 days");
    for i in 0..extra {
        let oldest = (pk(&format!("u{i}@example.test")), Rk::Use);
        assert!(!book.known.contains_key(&oldest), "the oldest go first");
    }
    let newest = (
        pk(&format!("u{}@example.test", book::MAX_KNOWN + extra - 1)),
        Rk::Use,
    );
    assert!(book.known.contains_key(&newest));
    assert_eq!(Book::from_value(&book.to_value()).known, book.known);
}

/// END TO END (design §12, step 8: journal shares): a busy Mac with a sharp event before every
/// pass spends [`BURST`] records at the week's start and then waits, keeps under its 54-record
/// share by the week's end, and loses no event: every noted event is in exactly one `ev` batch
/// once the next week's flush has gone out.
/// NEGATIVE CONTROL: with `under_pace` always true, the week's start spends a record a pass:
/// `` left: 20 right: 8 ``.
#[test]
fn a_busy_mac_keeps_to_its_share_and_loses_no_event() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let start = W * WEEK + 60;
    let mut noted = Vec::new();
    let mut busy = |now: u64, passes: u64| {
        let mut wrote = 0;
        for i in 0..passes {
            let event = ev(now + i, Kind::Wall, ann);
            spool.append(&event).unwrap();
            noted.push(event);
            if matches!(
                flush::pass(&lab.paths[0], spool, &Presence::default, false, now + i).unwrap(),
                Flushed::Wrote { .. }
            ) {
                wrote += 1;
            }
        }
        wrote
    };
    assert_eq!(busy(start, 20), BURST);
    let end = (W + 1) * WEEK - 120;
    busy(end, 60);
    let (_, _, used) = read(&lab.paths[0], end + 60);
    assert!(used < JOURNAL_SHARES_RECORDS, "{used}");
    assert!(used > 40, "the line grew with the week: {used}");
    let next = (W + 1) * WEEK + 60;
    // Two passes: the first takes the spool, the second the file it took.
    for i in 0..2 {
        flush::pass(&lab.paths[0], spool, &Presence::default, false, next + i).unwrap();
    }
    let (_, ops, _) = read(&lab.paths[0], next + 2);
    let (bootstrap, journal) =
        aterm_secrets::commons::bootstrap::Bootstrap::required_with_journal_until(
            &lab.paths[0],
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
    let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
    let log = owned::read_log(&EV, &ops, owned::commons::historical(&session));
    let mut published: Vec<Event> = log
        .entries
        .iter()
        .flat_map(|entry| entry.value["e"].as_array().unwrap().clone())
        .map(|value| Event::from_value(&value).unwrap())
        .collect();
    published.sort();
    noted.sort();
    assert_eq!(published.len(), noted.len(), "each event once");
    assert_eq!(published, noted);
}

const JOURNAL_SHARES_RECORDS: usize = crate::catalog::ring::JOURNAL_SHARES.writer_records;
const BYTE_SHARE: usize = crate::catalog::ring::JOURNAL_SHARES.writer_bytes;

/// END TO END (design §3.9, the byte share): a Mac whose records are large (about 40 KiB: a
/// full `ev` batch and 62 new `use` rows) keeps to its pace in bytes as well as records. At the
/// week's start it spends no more than the burst's fraction of its byte share (and the week's
/// first record); at the week's end, once its share of the week is full and records land in the
/// next week's table, it spends no more of that table than the next week's own line allows, so
/// the next week's records go into the next week, never a week early.
/// NEGATIVE CONTROL: with the pace judged by records in the current week alone, as before
/// (`paced` = `Book::under_pace` of the current week's record count, no bytes), the start
/// spends eight large records: `` the burst's bytes: 276701 ``.
#[test]
fn large_records_keep_to_the_byte_line_and_never_spill_a_week_ahead() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let mut next_account = 0u64;
    let mut big = |now: u64, passes: u64| {
        (0..passes)
            .map(|i| {
                for _ in 0..250 {
                    next_account += 1;
                    let account = pk(&format!("u{next_account}@example.test"));
                    spool.append(&ev(now + i, Kind::SignedIn, account)).unwrap();
                }
                flush::pass(&lab.paths[0], spool, &Presence::default, false, now + i).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let burst_bytes = BYTE_SHARE * BURST / JOURNAL_SHARES_RECORDS;
    let largest = crate::catalog::ring::record_bytes(caps::RECORD);
    big(W * WEEK + 60, 12);
    let (_, window) = window_at(&lab.paths[0], W * WEEK + 80);
    let start = window.used[&W];
    assert!(
        start.writer_bytes <= burst_bytes + largest,
        "the burst's bytes: {}",
        start.writer_bytes
    );
    assert!(
        start.writer_records > 1,
        "large records still go: {start:?}"
    );

    // The week's end: passes until a record goes into the next week's table, which only a full
    // share sends there, then on until two passes have written nothing there, so passes past the
    // next week's line are tried whatever number of records it admits.
    let end = (W + 1) * WEEK - 600;
    let (mut written_ahead, mut refused) = (0, 0);
    for i in 0..40 {
        let flushed = big(end + i, 1);
        if matches!(flushed[..], [Flushed::Wrote { period, .. }] if period == W + 1) {
            written_ahead += 1;
        } else if written_ahead > 0 {
            refused += 1;
            if refused == 2 {
                break;
            }
        }
    }
    let (_, window) = window_at(&lab.paths[0], (W + 1) * WEEK - 500);
    let full = window.used[&W];
    assert!(
        full.writer_bytes > BYTE_SHARE - largest,
        "the week's share filled: {full:?}"
    );
    let ahead = window.used.get(&(W + 1)).copied().unwrap_or_default();
    assert!(
        ahead.writer_records <= BURST && ahead.writer_bytes <= burst_bytes,
        "spent ahead in next week's table: {ahead:?}"
    );
    assert_eq!(
        refused, 2,
        "passes past the next week's line were tried: {written_ahead} written ahead"
    );
    let next = (W + 1) * WEEK + 60;
    assert!(
        matches!(
            flush::pass(&lab.paths[0], spool, &Presence::default, false, next).unwrap(),
            Flushed::Wrote { period, .. } if period == W + 1
        ),
        "the next week's first record goes into its own week"
    );
}

/// A Mac whose retained storage fills every slot keeps its journal local: the flush is kept
/// with `Kept::reason`'s words, `status` reports them, and the outbox keeps the events (design
/// §4.17, step 4's refused enrollment).
/// NEGATIVE CONTROL: with the flusher ignoring a kept append (`Err(_kept) =>
/// Ok(Flushed::Idle)`), the reason is never kept: `` left: Idle right: Kept("journal: no
/// storage slot") ``.
#[test]
fn a_flush_with_no_storage_slot_stays_local_and_says_why() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let root = lab.paths[0].dir.join("commons/tables");
    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(&lab.paths[0], bootstrap.trust());
        aterm_journal::create_private_dir_durable(&root).unwrap();
        while std::fs::read_dir(&root).map_or(0, Iterator::count) <= MAX_TABLES {
            let scope = Scope::fresh(directory.trust()).unwrap();
            Store::join(&session, &directory.path(scope).unwrap(), scope).unwrap();
        }
    }
    let now = W * WEEK + 3600;
    spool.append(&ev(now - 5, Kind::SignedIn, ann)).unwrap();
    let pass = |now| flush::pass(&lab.paths[0], spool, &Presence::default, false, now).unwrap();
    assert_eq!(pass(now), Flushed::Kept("journal: no storage slot"));
    assert_eq!(status(spool, None), "local: journal: no storage slot");
    assert_eq!(
        pass(now + 60),
        Flushed::Idle,
        "a kept append waits its retry"
    );
    let retry = now + book::KEPT_RETRY_S;
    assert_eq!(pass(retry), Flushed::Kept("journal: no storage slot"));
    assert_eq!(spool.outbox().unwrap().len(), 1, "the event waits");
}

/// The lab's Commons folder of `paths` moved aside (`away`) or back, so a pass that reads any
/// table fails and one that reads none does not.
fn commons_away(paths: &Paths, away: bool) {
    let (here, aside) = (paths.dir.join("commons"), paths.dir.join("commons.aside"));
    let (from, to) = if away { (here, aside) } else { (aside, here) };
    std::fs::rename(from, to).unwrap();
}

/// An idle Mac reads its window once in six hours, and a Mac over its pace line reads none until
/// the line admits a record again: with the Commons folder moved aside, so any table read fails,
/// such passes still succeed, while the week's first record (which goes whatever the pace) does
/// read and fails. The routine interval runs from the last read that found nothing to write as
/// from a write.
/// NEGATIVE CONTROL: with the gate's routine interval measured from the last write alone
/// (`book.flushed_at.saturating_add(ROUTINE_S)`), the idle Mac reads again a minute later:
/// `` an idle minute reads no table: Err(…) ``.
#[test]
fn an_idle_or_paced_mac_reads_no_table_until_a_record_can_go() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let pass = |now| flush::pass(&lab.paths[0], spool, &Presence::default, false, now);
    let start = W * WEEK + 60;
    assert!(
        matches!(pass(start).unwrap(), Flushed::Wrote { .. }),
        "the week's sum"
    );
    let idle = start + book::ROUTINE_S + 1;
    assert_eq!(
        pass(idle).unwrap(),
        Flushed::Idle,
        "a routine read finds nothing"
    );
    commons_away(&lab.paths[0], true);
    let minute = pass(idle + 60);
    assert!(
        matches!(minute, Ok(Flushed::Idle)),
        "an idle minute reads no table: {minute:?}"
    );
    assert!(
        pass(idle + book::ROUTINE_S).is_err(),
        "the next routine read does read"
    );
    assert!(pass((W + 1) * WEEK + 60).is_err(), "a new week reads");
    commons_away(&lab.paths[0], false);

    // Over the line: the burst spent, a sharp event waits, and nothing is read until it admits.
    let busy = (W + 2) * WEEK + 60;
    let mut wrote = 0;
    for i in 0..(BURST as u64 + 4) {
        spool.append(&ev(busy + i, Kind::Wall, ann)).unwrap();
        if matches!(pass(busy + i).unwrap(), Flushed::Wrote { .. }) {
            wrote += 1;
        }
    }
    assert_eq!(wrote, BURST);
    let held = Book::load(spool).held_until;
    assert!(held > busy + 60, "held until the line admits: {held}");
    commons_away(&lab.paths[0], true);
    spool.append(&ev(busy + 100, Kind::Wall, ann)).unwrap();
    assert!(matches!(pass(busy + 100), Ok(Flushed::Idle)));
    assert!(pass(held).is_err(), "the line admits: the table is read");
    commons_away(&lab.paths[0], false);
    assert!(matches!(pass(held).unwrap(), Flushed::Wrote { .. }));
}

/// A failed outbox write loses no event of the file the spool was taken into: that file stays
/// until the outbox holds what was read from it, so the next pass reads it again.
/// NEGATIVE CONTROL: with `absorb` removing the taken file as it reads it (as before), the
/// event is lost to the failed write: `` the taken event is published: Idle ``.
#[test]
fn a_failed_outbox_write_loses_no_taken_event() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let pass = |now| flush::pass(&lab.paths[0], spool, &Presence::default, false, now);
    let now = W * WEEK + 3600;
    spool.append(&ev(now - 5, Kind::SignedIn, ann)).unwrap();
    assert!(matches!(
        pass(now).unwrap(),
        Flushed::Wrote { events: 0, .. }
    ));
    let outbox = spool.dir().join(spool::OUTBOX);
    std::fs::remove_file(&outbox).unwrap();
    std::fs::create_dir(&outbox).unwrap();
    assert!(pass(now + 60).is_err(), "the outbox cannot be written");
    std::fs::remove_dir(&outbox).unwrap();
    let again = pass(now + 120).unwrap();
    assert!(
        matches!(again, Flushed::Wrote { events: 1, .. }),
        "the taken event is published: {again:?}"
    );
}

/// A lost or torn book never makes the flusher reuse an `ev` key: it takes the highest counter
/// its own rows in the window carry as its floor, so a batch written after the tear is a new key
/// and every reader keeps it.
/// NEGATIVE CONTROL: with the counter not recovered from the window (`book.n =
/// book.n.max(own_n)` removed), the next batch repeats key 2 and readers drop it: `` the batch
/// after the tear is read: [Event { t: 1727917159, kind: SignedIn, … }] `` (only the first).
#[test]
fn a_torn_book_never_reuses_a_batch_key() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let pass = |now| flush::pass(&lab.paths[0], spool, &Presence::default, false, now).unwrap();
    let now = W * WEEK + 3600;
    let first = ev(now - 41, Kind::SignedIn, ann);
    spool.append(&first).unwrap();
    pass(now);
    assert!(matches!(pass(now + 60), Flushed::Wrote { events: 1, .. }));
    spool.write(book::BOOK, b"{\"torn").unwrap();
    let after = ev(now + 100, Kind::SignedOut, ann);
    spool.append(&after).unwrap();
    pass(now + 120);
    pass(now + 180);
    let (me, window) = window_at(&lab.paths[0], now + 180);
    let (bootstrap, journal) =
        aterm_secrets::commons::bootstrap::Bootstrap::required_with_journal_until(
            &lab.paths[0],
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
    let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
    let log = owned::read_log(&EV, &window.ops, owned::commons::historical(&session));
    let published: Vec<Event> = log
        .entries
        .iter()
        .filter(|entry| entry.machine == me)
        .flat_map(|entry| entry.value["e"].as_array().unwrap().clone())
        .map(|value| Event::from_value(&value).unwrap())
        .collect();
    assert!(
        published.contains(&after),
        "the batch after the tear is read: {published:?}"
    );
    assert!(published.contains(&first));
}

/// `Spool::append` and `heard::archive` refuse symlinks and tighten non-0600 regular files so a
/// renamed spool file never wedges `Spool::absorb`, and `heard` pruning skips non-canonical
/// filenames, directories and symlinks instead of failing the archive pass.
/// NEGATIVE CONTROL: with `open_append` following symlinks and leaving a pre-existing `0644`
/// `spool.jsonl` loose, `spool.absorb()` after `spool.take()` fails with `NotPrivate`.
#[test]
fn spool_and_heard_refuse_symlinks_tighten_modes_and_prune_ignores_non_files() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    aterm_uds::private_file::ensure_private_dir(spool.dir()).unwrap();
    let ann = pk("ann@example.test");
    let event = ev(100, Kind::SignedIn, ann);

    let outside = scratch.path().join("outside.jsonl");
    std::fs::write(&outside, b"").unwrap();
    let spool_path = spool.dir().join(spool::SPOOL);
    symlink(&outside, &spool_path).unwrap();
    assert!(spool.append(&event).is_err(), "spool symlink is refused");
    assert_eq!(std::fs::read(&outside).unwrap(), b"");
    std::fs::remove_file(&spool_path).unwrap();

    // A pre-existing 0644 regular file is tightened to 0600 on append so `take` + `absorb` reads it.
    std::fs::write(&spool_path, b"").unwrap();
    std::fs::set_permissions(&spool_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(spool.append(&event).unwrap(), Appended::Written);
    spool.take().unwrap();
    assert_eq!(spool.absorb().unwrap(), vec![event]);

    let heard_dir = spool.dir().join(heard::HEARD);
    aterm_uds::private_file::ensure_private_dir(&heard_dir).unwrap();
    let today_file = heard_dir.join("20000.jsonl");
    symlink(&outside, &today_file).unwrap();
    let batch = vec![("01".repeat(16), 1, W, vec![event])];
    assert!(
        heard::archive(spool.dir(), 20_000 * 86_400, &batch).is_err(),
        "heard symlink is refused"
    );
    std::fs::remove_file(&today_file).unwrap();

    // Non-canonical leading-zero files, directories and symlinks in `heard/` do not wedge `prune`.
    std::fs::create_dir(heard_dir.join("1.jsonl")).unwrap();
    symlink(&outside, heard_dir.join("2.jsonl")).unwrap();
    std::fs::write(heard_dir.join("0003.jsonl"), b"keep\n").unwrap();
    std::fs::write(heard_dir.join("3.jsonl"), b"drop\n").unwrap();
    heard::archive(spool.dir(), 20_000 * 86_400, &batch).unwrap();
    assert!(
        !heard_dir.join("3.jsonl").exists(),
        "expired canonical day pruned"
    );
    assert!(
        heard_dir.join("0003.jsonl").exists(),
        "non-canonical name untouched"
    );
    assert!(heard_dir.join("1.jsonl").is_dir());
}

/// An idle Mac whose window holds only `sum` rows still recovers its counter after a torn
/// `book.json`, so its next `sum` row outranks the earlier one in the same week; and a `relay`
/// row whose key nibble disagrees with its `"b"` field is ignored rather than spoofing another
/// bucket's coverage or entries.
/// NEGATIVE CONTROLS: without `SUM` in `read_window`'s `own_n`, the post-tear `sum` row in week
/// `W + 1` writes `n = 1` below `n = 2` and `read_owned(&SUM)` keeps the pre-tear row; with
/// `read_window` reading every `relay` row (its `key_relay_bucket` check removed), the spoofed
/// row covers the gap and `high_i` stands down: `` mismatched rl-0 row did not cover bucket ``.
#[test]
fn torn_book_recovers_sum_counter_and_mismatched_relay_bucket_keys_are_ignored() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spools = spools(scratch.path(), 2);

    assert!(matches!(
        flush::pass(&lab.paths[0], &spools[0], &Presence::default, false, W * WEEK + 60).unwrap(),
        Flushed::Wrote { period, events: 0, rows: 1 } if period == W
    ));
    let w1 = (W + 1) * WEEK + 60;
    assert!(matches!(
        flush::pass(&lab.paths[0], &spools[0], &Presence::default, false, w1).unwrap(),
        Flushed::Wrote { period, events: 0, rows: 1 } if period == W + 1
    ));
    spools[0].write(book::BOOK, b"{\"torn").unwrap();
    let labelled = Presence {
        label: Word::new("m22"),
        os: Some(27),
    };
    assert!(matches!(
        flush::pass(&lab.paths[0], &spools[0], &|| labelled.clone(), false, w1 + 60).unwrap(),
        Flushed::Wrote { period, events: 0, rows: 1 } if period == W + 1
    ));
    let (m0, _, _) = read(&lab.paths[0], w1 + 60);
    let sums = rows(&lab.paths[0], w1 + 60, &SUM);
    let sum = &sums.rows[&hex::encode_lower(&m0)].value;
    assert_eq!(sum["n"], json!(3));
    assert_eq!(sum["label"], json!("m22"));

    // Of the two Macs, let `low_i` be the one whose machine uuid sorts lower (`m_low < m_high`),
    // so a valid `relay` row from `low_i` for an uncovered bucket would make `high_i` stand down.
    let (m0_only, _, _) = read(&lab.paths[0], w1 + 60);
    let (m1_only, _, _) = read(&lab.paths[1], w1 + 60);
    let (low_i, high_i, m_low, m_high) = if m0_only < m1_only {
        (0, 1, m0_only, m1_only)
    } else {
        (1, 0, m1_only, m0_only)
    };
    let gap = (1..caps::RELAY_BUCKETS)
        .find(|b| !relay_duty(&m_low).contains(b) && !relay_duty(&m_high).contains(b))
        .unwrap();
    let gap_account = (0..500)
        .map(|i| pk(&format!("gap-{i}@example.test")))
        .find(|a| relay_bucket(a.as_str()) == gap)
        .unwrap();

    // `low_i` writes its week `W + 2` record, then appends a mismatched `relay` row at
    // `rl-0.<m_low>` claiming `"b": gap` (`gap != 0`) and a timestamp `w2 + 50`. The honest use
    // is `high_i`'s own `SignedIn` at `w2 + 5`: a record that publishes a use relays it (design
    // §11 step 12), so an honest relay of `low_i`'s would cover the bucket for `high_i` itself.
    let w2 = (W + 2) * WEEK + 60;
    assert!(matches!(
        flush::pass(&lab.paths[low_i], &spools[low_i], &Presence::default, false, w2 + 10).unwrap(),
        Flushed::Wrote { period, .. } if period == W + 2
    ));
    spools[high_i]
        .append(&ev(w2 + 5, Kind::SignedIn, gap_account))
        .unwrap();
    {
        let bootstrap = lab.bootstrap(low_i);
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(&lab.paths[low_i], bootstrap.trust());
        let changes = vec![crate::database::Change {
            collection: RELAY.name.into(),
            key: format!("rl-0.{}", hex::encode_lower(&m_low)),
            value: Some(json!({
                "r": 1,
                "n": 99,
                "period": W + 2,
                "b": gap,
                "e": [{
                    "a": gap_account.as_str(),
                    "k": "signin",
                    "m": hex::encode_lower(&m_low),
                    "p": W + 2,
                    "t": w2 + 50
                }]
            })),
        }];
        let request = crate::typed::request_id(APPLICATION, ROLE, &changes).unwrap();
        let payload = crate::database::Command::Write { request, changes }
            .encode()
            .unwrap();
        directory
            .append_journal(&session, w2 + 30, w2 + 30, &payload)
            .unwrap()
            .unwrap();
    }
    repair_commons_at(&lab, 6, &[w2 + 40, w2 + 40]);
    let bad_key = format!("rl-0.{}", hex::encode_lower(&m_low));
    assert!(
        rows(&lab.paths[high_i], w2 + 40, &RELAY)
            .rows
            .contains_key(&bad_key),
        "high_i received the mismatched relay row before flushing"
    );

    // `high_i` flushes week `W + 2` (its sum, then the batch the spool held): because
    // `rl-0.<m_low>` disagrees with `"b": gap`, `high_i` neither stands down for `gap` nor relays
    // the spoofed `w2 + 50` timestamp.
    for at in [w2 + 60, w2 + 120] {
        assert!(matches!(
            flush::pass(&lab.paths[high_i], &spools[high_i], &Presence::default, false, at).unwrap(),
            Flushed::Wrote { period, .. } if period == W + 2
        ));
    }
    let relays_high = rows(&lab.paths[high_i], w2 + 60, &RELAY);
    let gap_key = format!("rl-{gap:x}.{}", hex::encode_lower(&m_high));
    assert!(
        relays_high.rows.contains_key(&gap_key),
        "mismatched rl-0 row did not cover bucket {gap}: {relays_high:?}"
    );
    let entries = relays_high.rows[&gap_key].value["e"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["t"], json!(w2 + 5));
}

/// The events this Mac's own `ev` batches in the window at `now` hold.
fn own_published(paths: &Paths, now: u64) -> Vec<Event> {
    let (me, window) = window_at(paths, now);
    let (bootstrap, journal) =
        aterm_secrets::commons::bootstrap::Bootstrap::required_with_journal_until(
            paths,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
    let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
    owned::read_log(&EV, &window.ops, owned::commons::historical(&session))
        .entries
        .iter()
        .filter(|entry| entry.machine == me)
        .flat_map(|entry| entry.value["e"].as_array().unwrap().clone())
        .map(|value| Event::from_value(&value).unwrap())
        .collect()
}

/// An account's row gives up a window a workspace's reading now states: a reading or wall folded
/// on the account's row before readings named their workspace is not restated beside the live
/// one on the sub-account's row, while a window no workspace reading names stays.
/// NEGATIVE CONTROL: with `Book::fold` not calling `yield_window`, the stale 95% week reading
/// stays on the account's row: `` the account's row gives up the week ``.
#[test]
fn an_account_row_gives_up_a_window_its_workspace_now_states() {
    let ann = pk("ann@example.test");
    let now = W * WEEK + 3600;
    let mut book = Book::default();
    book.fold(&[
        ev(now - 40, Kind::SessionStart, ann),
        quota(now - 30, ann, Window::Week, 950),
        quota(now - 30, ann, Window::FiveHour, 100),
        Event {
            window: Some(Window::Week),
            until: Some(now + 3600),
            ..ev(now - 30, Kind::Wall, ann)
        },
    ]);
    book.fold(&[
        Event {
            sub: Word::new("0123456789ab"),
            ..quota(now - 10, ann, Window::Week, 200)
        },
        Event {
            sub: Word::new("0123456789ab"),
            window: Some(Window::Week),
            ..ev(now - 10, Kind::WallCleared, ann)
        },
    ]);
    let account = &book.accounts[&ann];
    assert!(
        !account.quota.contains_key(&Window::Week),
        "the account's row gives up the week"
    );
    assert!(account.walls.is_empty(), "and its wall");
    assert!(account.quota.contains_key(&Window::FiveHour), "not the 5h");
    assert_eq!(account.last_start, now - 40, "nor its sessions");
    let sa = Pk::new(&keys::subaccount_pk(ann.as_str(), "0123456789ab").unwrap()).unwrap();
    assert_eq!(book.accounts[&sa].quota[&Window::Week].pct_x10, Some(200));
}

/// Two workspaces of one login keep their own `use` rows (design §4.13, §4.17, attack fix 15):
/// a reading that names a workspace folds into its sub-account's row, keyed `<sa pk>.<uuid32>`
/// by the same derivation the registry uses, naming its `AccountKey` in `key` and its account in
/// `account`; the session stays on the account's row, which names neither; the `sum` row counts
/// the account once.
/// NEGATIVE CONTROL: with `row_of` ignoring the workspace (`let Some(sub) = &None::<SubKey>`),
/// both readings fold into the account's row and one replaces the other: `` left: 1 right: 3 ``.
#[test]
fn each_workspace_of_a_login_keeps_its_own_use_row() {
    let ann = pk("ann@example.test");
    let now = W * WEEK + 3600;
    let workspace = |t: u64, key: &str, pct: u16| Event {
        sub: Word::new(key),
        ..quota(t, ann, Window::Week, pct)
    };
    let mut book = Book::default();
    book.fold(&[
        ev(now - 30, Kind::SessionStart, ann),
        workspace(now - 20, "0123456789ab", 400),
        workspace(now - 10, "ba9876543210", 900),
    ]);
    assert_eq!(book.accounts.len(), 3, "{:?}", book.accounts.keys());
    let sa = |key: &str| Pk::new(&keys::subaccount_pk(ann.as_str(), key).unwrap()).unwrap();
    let me = [7; 16];
    let uuid = hex::encode_lower(&me);
    let planned = book
        .plan(
            &[],
            &Heard::default(),
            &Presence::default(),
            &me,
            1,
            W,
            now,
            false,
        )
        .unwrap();
    let use_row = |pk: Pk| {
        planned
            .changes
            .iter()
            .find(|c| c.collection == "use" && c.key == format!("{}.{uuid}", pk.as_str()))
            .and_then(|c| c.value.clone())
            .unwrap()
    };
    for (key, pct) in [("0123456789ab", 400), ("ba9876543210", 900)] {
        let row = use_row(sa(key));
        assert_eq!(row["key"], json!(key));
        assert_eq!(row["account"], json!(ann.as_str()));
        assert_eq!(row["quota"][0]["p"], json!(pct), "{row}");
        assert_eq!(row["last_start"], json!(0));
    }
    let account = use_row(ann);
    assert_eq!(account["last_start"], json!(now - 30));
    assert!(account.get("key").is_none() && account.get("account").is_none());
    assert_eq!(account["quota"], json!([]));
    let sum = planned
        .changes
        .iter()
        .find(|c| c.collection == "sum")
        .unwrap();
    assert_eq!(sum.value.as_ref().unwrap()["used"], json!(1));
    // The book's file keeps the join.
    let reloaded = Book::from_value(&book.to_value());
    assert_eq!(reloaded.accounts[&sa("0123456789ab")].account, Some(ann));
}

/// `[alab] journal = false` stops this Mac's publishing (design §8): with it off, the events
/// noted (here a sign-in taken into the spool) are dropped unpublished and the only record is the
/// `sum` row saying `journal: off`; turned on again, the `sum` says `on` within the week and
/// nothing noted while it was off is ever published. `publishing` reads the key from `aterm.toml`
/// and leaves the journal on for anything else.
/// The file an off pass took is dropped by the next pass, on or off. A file that is there but
/// does not parse leaves the journal off, so a typo never cancels the opt-out.
/// NEGATIVE CONTROL: with `pass` keeping what the spool held while off (`(held, absorbed)` in
/// both arms), the sign-in waits in the outbox to go out once the journal is on again:
/// `` dropped, not kept ``. With the first on pass reading what the last off pass took
/// (`|| book.took_off` removed), the sign-out goes out with the `sum`: `` left: Wrote { …,
/// events: 1, … } ``. With an unparsed file read as on, `` a typo keeps it off ``.
#[test]
fn a_journal_turned_off_publishes_only_its_sum_and_nothing_from_then() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    assert!(publishing(Some(&config)), "no file: on");
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = true\n", 0o600).unwrap();
    assert!(publishing(Some(&config)));
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
    assert!(!publishing(Some(&config)));
    assert!(publishing(None));
    // The hook's gate follows the file as it changes.
    let gate = Publishing::new(Some(config.clone()));
    assert!(!gate.now());
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = true\n", 0o600).unwrap();
    assert!(gate.now(), "the gate sees the file change");
    aterm_uds::atomic_file::write(&config, b"[alab]\njournal = false\n", 0o600).unwrap();
    assert!(!gate.now());

    let lab = Lab::new(&[Role::Writer]);
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let pass = |off, now| flush::pass(&lab.paths[0], spool, &Presence::default, off, now).unwrap();
    let journal_word = |now| {
        let sums = rows(&lab.paths[0], now, &SUM);
        sums.rows.values().next().unwrap().value["journal"].clone()
    };
    let now = W * WEEK + 3600;
    assert!(matches!(
        pass(false, now),
        Flushed::Wrote {
            events: 0,
            rows: 1,
            ..
        }
    ));
    spool.append(&ev(now + 10, Kind::SignedIn, ann)).unwrap();
    // The spool is taken on this pass and read on the next, which is off.
    assert_eq!(
        pass(false, now + 20),
        Flushed::Idle,
        "the sign-in is taken, not read"
    );
    assert_eq!(
        pass(true, now + 60),
        Flushed::Wrote {
            period: W,
            events: 0,
            rows: 1
        },
        "turned off: the sum alone"
    );
    assert_eq!(journal_word(now + 60), json!("off"));
    assert_eq!(status(spool, Some(&config)), "off");
    assert!(spool.outbox().unwrap().is_empty(), "dropped, not kept");
    // Noted while off (a hook that read the setting before it changed), and taken by an off
    // pass, which does not read it: the next pass drops it though the journal is on by then.
    spool.append(&ev(now + 90, Kind::SignedOut, ann)).unwrap();
    assert_eq!(
        pass(true, now + 120),
        Flushed::Idle,
        "off: nothing else is due"
    );
    assert_eq!(
        pass(false, now + 180),
        Flushed::Wrote {
            period: W,
            events: 0,
            rows: 1
        },
        "on again: the sum says so"
    );
    assert_eq!(journal_word(now + 180), json!("on"));
    assert_eq!(pass(false, now + 240), Flushed::Idle);
    let published = own_published(&lab.paths[0], now + 240);
    assert!(
        published.is_empty(),
        "nothing noted while off is published: {published:?}"
    );
    aterm_uds::atomic_file::write(&config, b"[alab\njournal = false\n", 0o600).unwrap();
    assert!(!publishing(Some(&config)), "a typo keeps it off");
    assert_eq!(status(spool, Some(&config)), "off: aterm.toml unreadable");
}

/// An event one of this Mac's own batches in the window holds is not published again: an outbox
/// that still holds a published batch (a crash between the append and the outbox's save, or a
/// backfill note of a ledger row the hooks noted first) sends only what is new.
/// NEGATIVE CONTROL: with `pass` not dropping what its own batches hold (`outbox.retain(...)`
/// removed), the batch goes out again: `` only the new event: Wrote { …, events: 3, … } ``.
#[test]
fn an_event_already_in_an_own_batch_is_not_published_twice() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let pass = |now| flush::pass(&lab.paths[0], spool, &Presence::default, false, now).unwrap();
    let now = W * WEEK + 3600;
    let first = [
        ev(now - 20, Kind::SessionStart, ann),
        ev(now - 10, Kind::SignedIn, ann),
    ];
    for event in &first {
        spool.append(event).unwrap();
    }
    pass(now);
    assert!(matches!(pass(now + 60), Flushed::Wrote { events: 2, .. }));
    let new = ev(now + 70, Kind::SignedOut, ann);
    spool.save_outbox(&[first[0], first[1], new]).unwrap();
    let again = pass(now + 80);
    assert!(
        matches!(again, Flushed::Wrote { events: 1, .. }),
        "only the new event: {again:?}"
    );
    let published = own_published(&lab.paths[0], now + 80);
    assert_eq!(published.len(), 3, "{published:?}");
}

/// A lost book archives no batch twice: with no record of what it archived, the flusher reads
/// the archive's own day files from the window's first week on.
/// NEGATIVE CONTROL: with `pass` not reading the archive for a lost book (the
/// `book.heard.is_empty()` block removed), the peer's batch is archived again:
/// `` a lost book archives nothing twice: left: 2 right: 1 ``.
#[test]
fn a_lost_book_archives_no_batch_twice() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spools = spools(scratch.path(), 2);
    let ann = pk("ann@example.test");
    let now = W * WEEK + 3600;
    let pass = |n: usize, now: u64| {
        flush::pass(&lab.paths[n], &spools[n], &Presence::default, false, now).unwrap()
    };
    spools[0]
        .append(&ev(now - 10, Kind::SignedIn, ann))
        .unwrap();
    pass(0, now);
    assert!(matches!(
        pass(0, now + 60),
        Flushed::Wrote { events: 1, .. }
    ));
    repair_commons_at(&lab, 2, &[now + 60, now + 60]);
    pass(1, now + 120);
    assert_eq!(heard_lines(&spools[1]).len(), 1);
    std::fs::remove_file(spools[1].dir().join(book::BOOK)).unwrap();
    pass(1, now + 180);
    assert_eq!(
        heard_lines(&spools[1]).len(),
        1,
        "a lost book archives nothing twice"
    );
}

/// A `probe` may name its realm alone (an agy or Muse read whose login was not seen, design §11
/// step 9): it round-trips without `a`, folds into no row and relays nothing; any other kind
/// without an account, or a realm-only probe that names a workspace, is refused.
/// NEGATIVE CONTROL: with `from_value` admitting any kind without `a` (its `kind != Kind::Probe`
/// test removed), an account-less sign-in is admitted: `` a sign-in names its account ``.
#[test]
fn only_a_probe_may_name_its_realm_alone() {
    let realm_only = Event {
        account: None,
        cli: Word::new("agy").unwrap(),
        src: Source::Probe,
        why: Word::new("launching"),
        ..ev(W * WEEK, Kind::Probe, pk("ann@example.test"))
    };
    let value = realm_only.to_value();
    assert!(value.get("a").is_none());
    assert_eq!(Event::from_value(&value), Some(realm_only));
    let mut book = Book::default();
    book.fold(&[realm_only]);
    assert!(book.accounts.is_empty());
    let signin = Event {
        kind: Kind::SignedIn,
        ..realm_only
    };
    assert_eq!(
        Event::from_value(&signin.to_value()),
        None,
        "a sign-in names its account"
    );
    let workspace = Event {
        sub: Word::new("0123456789ab"),
        ..realm_only
    };
    assert_eq!(Event::from_value(&workspace.to_value()), None);
}

/// A holding is on probation until this Mac's journal saw the account run or sign in here, then
/// takes the harness's reason, its `since` moving only when its reason does; an account the
/// harness stops reporting is stated once more with `until` at its end and then forgotten (one
/// never stated is forgotten at once); a live row is carried into each new week with an `until`
/// past that week; and `None` from the report (nothing known) ends nothing.
/// NEGATIVE CONTROL: with `hold` ignoring `proven` (always the harness's reason), the first row
/// says `borrow` (`left: "borrow" right: "probation"`); with `hold` dropping a stated row at once
/// instead of ending it, no ending row is due (`an end is stated once`).
#[test]
fn a_holding_is_on_probation_until_it_runs_here_and_ends_once() {
    use place::{Holding, Why};
    let ann = pk("ann@example.test");
    let bob = pk("bob@example.test");
    let now = W * WEEK + 3600;
    let mut book = Book::default();
    let holding = |account, why| Holding { account, why };
    book.hold(&[holding(ann, Why::Borrow), holding(bob, Why::Fill)], now);
    let due = book.places_due(1, W, now);
    assert_eq!(due.len(), 2);
    let row = |due: &[(Pk, Value)], account: Pk| {
        due.iter()
            .find(|(pk, _)| *pk == account)
            .map(|(_, row)| row.clone())
            .unwrap()
    };
    assert_eq!(row(&due, ann)["why"], json!("probation"));
    assert_eq!(row(&due, ann)["until"], json!((W + 2) * WEEK));
    let planned = book::Planned {
        places: due,
        ..book::Planned::default()
    };
    book.wrote(&planned, 1, W, now);
    assert!(
        book.places_due(2, W, now + 60).is_empty(),
        "nothing changed"
    );

    // ann ran here: an hour later the row takes the harness's reason, since then.
    book.fold(&[ev(now + 100, Kind::SessionStart, ann)]);
    book.hold(
        &[holding(ann, Why::Borrow), holding(bob, Why::Fill)],
        now + 200,
    );
    assert!(book.places_due(2, W, now + 300).is_empty(), "hourly");
    let due = book.places_due(2, W, now + 3600 + 200);
    assert_eq!(due.len(), 1);
    assert_eq!(row(&due, ann)["why"], json!("borrow"));
    assert_eq!(row(&due, ann)["since"], json!(now + 200));
    book.wrote(
        &book::Planned {
            places: due,
            ..book::Planned::default()
        },
        2,
        W,
        now + 3800,
    );

    // Nothing known: nothing ends. bob dropped: its row is stated once, ended, then forgotten.
    let later = now + 2 * 3600;
    book.hold(&[holding(ann, Why::Borrow)], later);
    let due = book.places_due(3, W, later + 3600);
    assert_eq!(due.len(), 1, "an end is stated once");
    assert_eq!(row(&due, bob)["until"], json!(later));
    book.wrote(
        &book::Planned {
            places: due,
            ..book::Planned::default()
        },
        3,
        W,
        later + 3600,
    );
    assert!(!book.places.contains_key(&bob));
    // A holding never stated is forgotten at once when it goes.
    let cy = pk("cy@example.test");
    book.hold(
        &[holding(ann, Why::Borrow), holding(cy, Why::Fill)],
        later + 4000,
    );
    book.hold(&[holding(ann, Why::Borrow)], later + 4100);
    assert!(!book.places.contains_key(&cy));

    // The next week carries ann's live row with an `until` past that week.
    let next = (W + 1) * WEEK + 10;
    let due = book.places_due(4, W + 1, next);
    assert_eq!(due.len(), 1);
    assert_eq!(row(&due, ann)["until"], json!((W + 3) * WEEK));
    // The book keeps its places across a save.
    assert_eq!(Book::from_value(&book.to_value()).places, book.places);
}

/// End to end: the flusher writes this Mac's holdings as `place` rows at `<account pk>.<uuid32>`
/// in the same records as its other rows, the reader finds them live with this Mac's `sum` word,
/// and a report of nothing known (`None`) changes no row.
/// NEGATIVE CONTROL: with `pass_with` not calling `book.hold`, no place row is read
/// (`left: 0 right: 1`).
#[test]
fn the_flusher_writes_holdings_as_place_rows_the_reader_finds() {
    use place::{Holding, Why};
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = &spools(scratch.path(), 1)[0];
    let ann = pk("ann@example.test");
    let now = W * WEEK + 3600;
    let held = || {
        Some(vec![Holding {
            account: ann,
            why: Why::Fill,
        }])
    };
    let pass = |holdings: &dyn Fn() -> Option<Vec<Holding>>, now| {
        flush::pass_with(
            &lab.paths[0],
            spool,
            &Presence::default,
            holdings,
            &|| None,
            false,
            now,
        )
        .unwrap()
    };
    assert!(matches!(pass(&held, now), Flushed::Wrote { .. }));
    let read = rows(&lab.paths[0], now + 1, &PLACE);
    assert_eq!(read.rows.len(), 1);
    let (key, row) = read.rows.iter().next().unwrap();
    let (me, _) = window_at(&lab.paths[0], now + 1);
    assert_eq!(*key, format!("{}.{}", ann.as_str(), hex::encode_lower(&me)));
    assert_eq!(row.value["why"], json!("probation"), "never ran here");
    let places = place::read(&lab.paths[0], now + 1).unwrap().unwrap();
    assert_eq!(places.rows.len(), 1);
    assert_eq!(places.rows[0].account, ann.as_str());
    assert_eq!(places.rows[0].uuid, hex::encode_lower(&me));
    // Past the routine interval, a report of nothing known ends nothing.
    let later = now + book::ROUTINE_S + 60;
    pass(&|| None, later);
    let places = place::read(&lab.paths[0], later + 1).unwrap().unwrap();
    assert_eq!(places.rows.len(), 1, "None ends nothing");
    // An empty report ends the row, which the reader no longer finds live.
    let end = later + book::ROUTINE_S + 60;
    spool.append(&ev(end - 30, Kind::SignedIn, ann)).unwrap();
    pass(&|| Some(Vec::new()), end - 20);
    pass(&|| Some(Vec::new()), end);
    let places = place::read(&lab.paths[0], end + 1).unwrap().unwrap();
    assert!(places.rows.is_empty(), "ended: {places:?}");
}

/// The book's public read (one-mesh §4.14, the metrics publisher's quota row): what a pass
/// folded and saved reads back per account as its sign-in word, last start and end and its
/// sessions in the 7 days before the read, and an account the book never saw is absent. A
/// missing book reads as an empty one.
/// NEGATIVE CONTROL: a read 8 days after the starts counts none of them, so the count is the
/// window's, not the book's whole list.
#[test]
fn book_facts_read_back_what_the_book_saved() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    assert_eq!(book_facts(&spool, 1), BookFacts::default(), "no book yet");
    let ann = pk("ann@example.test");
    let bob = pk("bob@example.test");
    let now = W * WEEK + 3600;
    let mut book = Book::default();
    book.fold(&[
        ev(now - 7200, Kind::SessionStart, ann),
        ev(now - 3600, Kind::SessionStart, ann),
        ev(now - 1800, Kind::SessionEnd, ann),
        ev(now - 900, Kind::SignedIn, ann),
    ]);
    book.save(&spool).unwrap();
    let read = book_facts(&spool, now);
    assert_eq!(
        read.accounts.get(&ann),
        Some(&AccountFacts {
            signin: Some(("in", now - 900)),
            last_start: Some(now - 3600),
            last_end: Some(now - 1800),
            sessions_7d: 2,
            plan: None,
        })
    );
    assert!(!read.accounts.contains_key(&bob), "never seen");
    let later = book_facts(&spool, now + 8 * 86_400);
    assert_eq!(later.accounts.get(&ann).map(|f| f.sessions_7d), Some(0));
}

/// A reading of a real Commons lock another holder keeps: `read` runs as an admission that started
/// `age` ago, and the holder lets go the first time the admission waits on that very inode. The
/// count of those waits comes back with what `read` returned, so a test tells an admission that
/// waited from one that gave up before waiting, with no sleep.
#[cfg(unix)]
fn against_held_custody<R>(
    age: std::time::Duration,
    read: impl FnOnce(&Paths, std::time::Instant) -> R,
) -> (R, usize) {
    use std::{cell::Cell, os::unix::fs::MetadataExt as _, rc::Rc};
    let lab = Lab::new(&[Role::Writer]);
    lab.bootstrap(0);
    let paths = &lab.paths[0];
    let holder = aterm_uds::private_file::PrivateLock::take_with(
        &paths.dir,
        "commons.lock",
        std::time::Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    let named = std::fs::metadata(paths.dir.join("commons.lock")).unwrap();
    let identity = (named.dev(), named.ino());
    let waits = Rc::new(Cell::new(0));
    let observed = waits.clone();
    let mut holder = Some(holder);
    let read = aterm_uds::file_lock::with_lock_wait_observer(
        move |file| {
            let actual = file.metadata().unwrap();
            assert_eq!((actual.dev(), actual.ino()), identity);
            observed.set(observed.get() + 1);
            drop(holder.take());
        },
        || read(paths, std::time::Instant::now() - age),
    );
    (read, waits.get())
}

/// Older than any latency budget, inside the hang detector: the admission waits for the holder.
#[cfg(unix)]
const WITHIN_THE_MINUTE: std::time::Duration = std::time::Duration::from_secs(3);
/// Past the hang detector: the admission gives up without waiting at all.
#[cfg(unix)]
const PAST_THE_MINUTE: std::time::Duration = std::time::Duration::from_secs(61);

#[cfg(unix)]
#[test]
fn a_due_flush_waits_out_held_custody_until_the_hang_detector() {
    let flush = |age| {
        against_held_custody(age, |paths, started| {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let spool = Spool::at(scratch.path().join("alab"));
            let now = W * WEEK + 600;
            flush::pass_started(
                paths,
                &spool,
                &Presence::default,
                &|| None,
                &|| None,
                false,
                now,
                started,
            )
        })
    };
    let (written, waits) = flush(WITHIN_THE_MINUTE);
    assert!(matches!(written, Ok(Flushed::Wrote { .. })), "{written:?}");
    assert_eq!(waits, 1, "the write came after the wait on the holder");
    let (refused, waits) = flush(PAST_THE_MINUTE);
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(waits, 0, "the detector ended the admission");
}

#[cfg(unix)]
#[test]
fn a_place_read_waits_out_held_custody_until_the_hang_detector() {
    let read = |age| {
        against_held_custody(age, |paths, started| {
            place::read_started(paths, W * WEEK + 600, started)
        })
    };
    let (read_held, waits) = read(WITHIN_THE_MINUTE);
    assert!(read_held.unwrap().is_some(), "held custody is no absence");
    assert_eq!(waits, 1, "the read came after the wait on the holder");
    let (given_up, waits) = read(PAST_THE_MINUTE);
    assert!(given_up.unwrap().is_none());
    assert_eq!(waits, 0, "the detector ended the admission");
}

#[cfg(unix)]
#[test]
fn a_seen_read_waits_out_held_custody_until_the_hang_detector() {
    let read = |age| {
        against_held_custody(age, |paths, started| {
            seen::read_started(paths, None, W * WEEK + 600, started, || None)
        })
    };
    let (read_held, waits) = read(WITHIN_THE_MINUTE);
    assert!(read_held.unwrap().is_some(), "held custody is no absence");
    assert_eq!(waits, 1, "the read came after the wait on the holder");
    let (given_up, waits) = read(PAST_THE_MINUTE);
    assert!(given_up.unwrap().is_none());
    assert_eq!(waits, 0, "the detector ended the admission");
}

/// A flush asks for this Mac's holdings, whose first look at the lab takes `commons.lock`, while
/// the journal writer is free: an admission holds that lock while it waits for this writer.
#[cfg(unix)]
#[test]
fn a_due_flush_asks_for_holdings_before_it_takes_the_journal_writer() {
    use aterm_secrets::commons::journal::Journal;
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spool = Spool::at(scratch.path().join("alab"));
    let asked = std::cell::Cell::new(false);
    let holdings = || {
        asked.set(true);
        let writer = Journal::open_existing_until(
            bootstrap.journal_dir(),
            bootstrap.trust(),
            std::time::Instant::now(),
        );
        assert!(
            writer.is_ok(),
            "the pass held the writer: {:?}",
            writer.err()
        );
        None
    };
    let flushed = flush::pass_with(
        &lab.paths[0],
        &spool,
        &Presence::default,
        &holdings,
        &|| None,
        false,
        W * WEEK + 600,
    );
    assert!(matches!(flushed, Ok(Flushed::Wrote { .. })), "{flushed:?}");
    assert!(asked.get(), "a due pass asks for holdings");
}

/// A seen read asks for the claims ring, which takes `commons.lock`, only once the journal writer
/// is free: an admission holds that lock while it waits for this writer.
#[cfg(unix)]
#[test]
fn a_seen_read_lets_go_of_the_journal_writer_before_it_reads_the_claims() {
    use aterm_secrets::commons::journal::Journal;
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let asked = std::cell::Cell::new(false);
    let read = seen::read_started(
        &lab.paths[0],
        None,
        W * WEEK + 600,
        std::time::Instant::now(),
        || {
            asked.set(true);
            let writer = Journal::open_existing_until(
                bootstrap.journal_dir(),
                bootstrap.trust(),
                std::time::Instant::now(),
            );
            assert!(
                writer.is_ok(),
                "the read kept the writer: {:?}",
                writer.err()
            );
            None
        },
    );
    assert!(read.unwrap().is_some());
    assert!(asked.get(), "the claims ring was read");
}

/// A tab's sighting as the harness notes it (`docs/DESIGN-account-discovery-2026-11-09.md`):
/// the observed account, where its login was read, the tab and the harness.
fn seen_event(t: u64, why: &str) -> Event {
    Event {
        src: Source::Profile,
        why: Word::new(why),
        tab: Word::new("0a1b2c3d"),
        harness: Word::new("claude"),
        ..ev(t, Kind::Seen, pk("unicorn.m15.2@proton.example"))
    }
}

/// A `seen` keeps its tab and harness through the spool line and the batch value, and both
/// readers take it back whole.
/// NEGATIVE CONTROL: with `encode` dropping `x`, the line reads back without its tab:
/// `` left: Some(Event { … tab: None … }) right: Some(Event { … tab: Some(0a1b2c3d) … }) ``.
#[test]
fn seen_round_trips_with_x_and_h() {
    let event = seen_event(1_790_000_000, "first");
    let mut buf = [0; MAX_EVENT];
    let n = event.encode(&mut buf).unwrap();
    let line = std::str::from_utf8(&buf[..n]).unwrap();
    assert!(line.contains("\"x\":\"0a1b2c3d\""), "{line}");
    assert!(line.contains("\"h\":\"claude\""), "{line}");
    assert_eq!(Event::from_line(&buf[..n]), Some(event));
    assert_eq!(Event::from_value(&event.to_value()), Some(event));
    assert_eq!(
        aterm_json::to_vec(&event.to_value()).unwrap(),
        buf[..n].to_vec(),
        "the value and the line are one canonical encoding"
    );
}

/// The widest `seen` a harness makes (a ten-digit time, the longest CLI word, a 16-byte harness
/// word, a reason and the believed account's `to`) fits [`MAX_EVENT`].
/// NEGATIVE CONTROL: with `MAX_EVENT` cut to 160, `` the widest seen fits ``.
#[test]
fn seen_widest_fits_192() {
    let widest = Event {
        cli: Word::new("codex-alab").unwrap(),
        harness: Word::new("codex-alab-agent"),
        src: Source::VaultToken,
        why: Word::new("changed"),
        ..seen_event(9_999_999_999, "changed")
    };
    let mut buf = [0; MAX_EVENT];
    assert!(widest.encode(&mut buf).is_some(), "the widest seen fits");
    let wrong = Event {
        kind: Kind::WrongAccount,
        to: Some(pk("brontosaurus.m22@proton.example")),
        ..widest
    };
    assert!(
        wrong.encode(&mut buf).is_some(),
        "the widest wrong-account fits"
    );
}

/// A tab and a harness ride only on the kinds about one tab's run: a quota reading that names a
/// tab, as a modified peer could write it, is refused whole.
/// NEGATIVE CONTROL: with the `names_tab` check in `from_value` removed, the quota with `x`
/// reads: `` left: Some(Event { kind: Quota, … }) right: None ``.
#[test]
fn x_refused_on_quota() {
    let mut value = quota(1_790_000_000, pk("ann@example.test"), Window::Week, 120).to_value();
    set(&mut value, "x", json!("0a1b2c3d"));
    assert_eq!(Event::from_value(&value), None);
    let mut value = ev(1_790_000_000, Kind::Auth, pk("ann@example.test")).to_value();
    set(&mut value, "h", json!("claude"));
    assert_eq!(Event::from_value(&value), None);
    // A tab id is 8 hex digits, never another word.
    let mut value = seen_event(1_790_000_000, "first").to_value();
    set(&mut value, "x", json!("tab-0001"));
    assert_eq!(Event::from_value(&value), None);
}

/// A first sighting or a changed account goes out on the flusher's next pass; an hourly refresh
/// of the same sighting waits for the routine flush.
/// NEGATIVE CONTROL: with `Event::sharp` ignoring `why`, the refresh is sharp:
/// `` assertion failed: !seen_event(1, "seen").sharp() ``.
#[test]
fn seen_is_sharp_on_a_first_sighting_and_not_on_a_refresh() {
    assert!(seen_event(1, "first").sharp());
    assert!(seen_event(1, "changed").sharp());
    assert!(!seen_event(1, "seen").sharp());
    assert!(!Kind::Seen.sharp(), "the kind alone is not sharp");
}

/// THE DEAD LOGIN BUG: a login ended at an auth wall, and the same tab's next sighting of its
/// profile (which still names the account) is no sign-in word: the account stays `ended`, and
/// a sighting alone states none. A real sign-in afterwards says `in`.
/// NEGATIVE CONTROL: with `Kind::Seen` folded as `in`, `` left: "in" right: "ended" ``.
#[test]
fn a_sighting_never_says_signed_in_over_an_ended_login() {
    let account = pk("unicorn.m15.2@proton.example");
    let word = |facts: &Facts| facts.signin.as_ref().map(|s| (s.state, s.t));
    let mut facts = Facts::default();
    facts.fold(&seen_event(90, "first"));
    assert_eq!(word(&facts), None, "a sighting alone");
    facts.fold(&ev(100, Kind::LoginEnded, account));
    facts.fold(&seen_event(200, "seen"));
    facts.fold(&seen_event(300, "first"));
    assert_eq!(word(&facts), Some(("ended", 100)));
    assert_eq!(
        facts.used, 300,
        "the sighting still dates the account's use"
    );
    facts.fold(&ev(400, Kind::SignedIn, account));
    assert_eq!(word(&facts), Some(("in", 400)));
}

/// This Mac's book hears a heal and a withdrawal as a peer does: `auth` saying `ok` is `in` over
/// an older `signed-out`, and `auth` saying `unseen` withdraws the word, which the book keeps
/// (and saves) so an older event folded later restores nothing, while its `use` row and its
/// readers are given no sign-in at all.
/// NEGATIVE CONTROLS: with the `Kind::Auth` arm of `Event::signin` removed, `` left:
/// Some(("out", 100)) right: Some(("in", 200)) ``; with `use_row` writing the held word unfiltered
/// (`SignIn::stated` not asked), the row says `unknown`: `` left: Object {"cli": …, "state":
/// String("unknown"), …} right: Null ``.
#[test]
fn the_book_hears_a_heal_and_keeps_a_withdrawn_word_to_itself() {
    let account = pk("unicorn.m15.2@proton.example");
    let auth = |t, why: &str| Event {
        why: Word::new(why),
        ..ev(t, Kind::Auth, account)
    };
    let word = |facts: &Facts| facts.signin.as_ref().map(|s| (s.state, s.t));
    let mut facts = Facts::default();
    facts.fold(&ev(100, Kind::SignedOut, account));
    facts.fold(&auth(150, "epoch"));
    assert_eq!(word(&facts), Some(("out", 100)), "no word in an epoch");
    facts.fold(&auth(200, "ok"));
    assert_eq!(word(&facts), Some(("in", 200)));
    facts.fold(&auth(300, "unseen"));
    facts.fold(&ev(250, Kind::SignedIn, account));
    assert_eq!(word(&facts), Some(("unknown", 300)), "kept against older");
    let row = facts.use_row(1, 0, 400, caps::USE_ROW).unwrap();
    assert_eq!(row["signin"], Value::Null, "no reader is given it");
    let mut book = Book::default();
    book.accounts.insert(account, facts);
    assert_eq!(book.facts(400).accounts[&account].signin, None);
    let back = Book::from_value(&book.to_value());
    assert_eq!(word(&back.accounts[&account]), Some(("unknown", 300)));
}

/// A record for the `upd` tests: build 162 running and proven, 198 offered.
fn upd_record() -> aterm_update_core::update_record::UpdateRecord {
    let mut record = aterm_update_core::update_record::UpdateRecord::fresh();
    record.running = 162;
    record.proven = true;
    record.offered = Some(198);
    record
}

/// [`upd_record`] holding build 198 since `now`, after it did not start cleanly.
fn upd_held(now: u64, why: &str) -> aterm_update_core::update_record::UpdateRecord {
    use aterm_update_core::update_record::{Cause, Failure, Sha, Witness};
    let mut record = upd_record();
    assert!(record.set_hold(
        Failure {
            build: 198,
            version: "0.198.0".into(),
            sha: Sha([7; 32]),
            cause: Cause::CrashLoop,
            witness: Witness::Headless,
            why: why.into(),
        },
        now,
    ));
    record
}

/// The `upd` row: the worst-case record (both reasons at their caps, every one a character JSON
/// escapes) fits its 2 KiB cap whole and reads back as the record it spells, its counter the
/// row's; over the cap the trial's reason goes first, then the hold's, and a record that fits
/// with neither is no row; `until` is the end of the next week, or the hold's end where later;
/// and `live` drops a row whose `until` passed or that is no record.
/// NEGATIVE CONTROL: with `row` returning `spell` unchecked, the oversized record's row passes
/// the cap (`` the trial's reason goes first ``).
#[test]
fn an_upd_row_fits_its_cap_reads_back_and_lapses_at_its_until() {
    use aterm_update_core::update_record::{Sha, TRIAL_WHY_MAX, TrialNote, WHY_MAX, Witness};
    let now = W * WEEK + 3600;
    let mut worst = upd_held(now, &"\"".repeat(WHY_MAX));
    worst.running_sha = Some(Sha([9; 32]));
    worst.trial = Some(TrialNote {
        build: 198,
        sha: Sha([7; 32]),
        since: now,
        witness: Witness::Headless,
        misses: 2,
        why: "\"".repeat(TRIAL_WHY_MAX),
    });
    let row = upd::row(&worst, 5, W).unwrap();
    assert!(aterm_json::to_vec(&row).unwrap().len() <= caps::UPD_ROW);
    let (back, until) = upd::of_row(&row).unwrap();
    assert_eq!(until, (W + 2) * WEEK);
    assert_eq!(
        back,
        aterm_update_core::update_record::UpdateRecord {
            n: 5,
            ..worst.clone()
        }
    );

    // A version long enough to crowd the reasons out.
    let mut long = worst.clone();
    long.hold.as_mut().unwrap().version = "9".repeat(caps::UPD_ROW - 900);
    let row = upd::row(&long, 5, W).unwrap();
    assert!(
        aterm_json::to_vec(&row).unwrap().len() <= caps::UPD_ROW,
        "the trial's reason goes first"
    );
    let (back, _) = upd::of_row(&row).unwrap();
    assert_eq!(back.trial.unwrap().why, "");
    long.hold.as_mut().unwrap().version = "9".repeat(caps::UPD_ROW);
    assert_eq!(upd::row(&long, 5, W), None);

    // A hold reaching past the next week's end carries the row until it ends.
    let late = (W + 2) * WEEK - 600;
    let held_late = upd_held(late, "x");
    let until = held_late.hold.as_ref().unwrap().until;
    assert!(until > (W + 2) * WEEK);
    assert_eq!(upd::until(&held_late, W), until);

    let mut read = owned::OwnedRead::default();
    let mut put = |uuid: &str, value: Value| {
        read.rows.insert(
            uuid.to_owned(),
            owned::OwnedRow {
                machine: [1; 16],
                actor: [1; 48],
                value,
                rank: (W, 1, 1),
                operation: [0; 32],
            },
        );
    };
    put(
        "00000000000000000000000000000001",
        upd::row(&worst, 1, W).unwrap(),
    );
    let mut lapsed = upd::row(&upd_record(), 1, W).unwrap();
    set(&mut lapsed, "until", json!(now));
    put("00000000000000000000000000000002", lapsed);
    put(
        "00000000000000000000000000000003",
        json!({"n": 1, "until": now + 10}),
    );
    let live = upd::live(&read, now, Some("00000000000000000000000000000002"));
    assert_eq!(live.len(), 1, "{live:?}");
    assert_eq!(live[0].uuid, "00000000000000000000000000000001");
    assert_eq!(live[0].record.hold, worst.hold);
    assert!(!live[0].own, "another Mac's row");
    let mine = upd::live(&read, now, Some("00000000000000000000000000000001"));
    assert!(
        mine[0].own,
        "this Mac's own row is marked, so no reader takes it as a peer"
    );
}

/// What a row cuts to fit its cap does not depend on its counter or week, so the book, which
/// asks whether a record is waiting with one spelling and states it with another, sees the row
/// it wrote as saying what the record says: across every version length that crosses the cap,
/// the row at counter 0 of week 0 and at a real counter and week say the same.
/// NEGATIVE CONTROL: with `row` judging the cut on `spell(&record, n, period)`, some length
/// fits at 0 and not at the real counter (`` the cut depends on the counter ``).
#[test]
fn an_upd_rows_cut_is_the_same_at_every_counter() {
    use aterm_update_core::update_record::{Sha, TRIAL_WHY_MAX, TrialNote, WHY_MAX, Witness};
    let now = W * WEEK + 3600;
    let mut record = upd_held(now, &"x".repeat(WHY_MAX));
    record.trial = Some(TrialNote {
        build: 198,
        sha: Sha([7; 32]),
        since: now,
        witness: Witness::Headless,
        misses: 2,
        why: "y".repeat(TRIAL_WHY_MAX),
    });
    for len in 1000..caps::UPD_ROW {
        record.hold.as_mut().unwrap().version = "9".repeat(len);
        let at = |n, period| upd::row(&record, n, period).map(|row| upd::content(&row));
        assert_eq!(
            at(0, 0),
            at(1_800_000_000_123, W),
            "the cut depends on the counter at version length {len}"
        );
    }
}

/// The book states an `upd` row as promptly as a sharp event whenever the record says
/// something its last row did not, within the pace line; an unchanged record is not due again;
/// the first record of a new week restates it while a hold or a trial stands, and not for an
/// idle, proven Mac; and only what the last row said survives the book's file.
/// NEGATIVE CONTROL: with `upd_due`'s carry ignoring the hold
/// (`record.hold.is_some() ||` removed), the held record is not restated in the new week
/// (`` restated while held ``).
#[test]
fn the_book_states_an_upd_row_when_the_record_moves_and_carries_it_only_while_held() {
    let now = W * WEEK + 3600;
    let me = [1; 16];
    let mut book = Book {
        sum_period: Some(W),
        flushed_at: now,
        ..Book::default()
    };
    assert!(!book.upd_waiting(), "no record read");
    assert!(!book.due(&[], false, now + 1, W, true, false));
    book.upd = Some(upd_record());
    assert!(book.upd_waiting());
    assert!(book.due(&[], false, now + 1, W, true, false));
    assert!(
        !book.due(&[], false, now + 1, W, false, false),
        "over the line"
    );
    let planned = book
        .plan(
            &[],
            &Heard::default(),
            &Presence::default(),
            &me,
            2,
            W,
            now + 1,
            false,
        )
        .unwrap();
    assert_eq!(planned.changes.len(), 1);
    assert_eq!(planned.changes[0].collection, "upd");
    assert_eq!(planned.changes[0].key, hex::encode_lower(&me));
    book.wrote(&planned, 2, W, now + 1);
    assert!(!book.upd_waiting(), "said");
    assert_eq!(book.upd_due(3, W), None);
    // Saved, the book keeps what the row said and never the record it read.
    let saved = Book::from_value(&book.to_value());
    assert_eq!(saved.upd_stated, book.upd_stated);
    assert_eq!(saved.upd, None);

    // An idle, proven Mac's row is not carried into the next week.
    assert_eq!(book.upd_due(3, W + 1), None, "idle and proven");
    // A record that moves is due at once.
    book.upd = Some(upd_held(now + 60, "the listener did not answer"));
    assert!(book.upd_waiting());
    let planned = book
        .plan(
            &[],
            &Heard::default(),
            &Presence::default(),
            &me,
            3,
            W,
            now + 61,
            false,
        )
        .unwrap();
    assert!(planned.upd.is_some());
    book.wrote(&planned, 3, W, now + 61);
    assert_eq!(book.upd_due(4, W), None);
    assert!(book.upd_due(4, W + 1).is_some(), "restated while held");
    // Off, no `upd` row goes out.
    let planned = book
        .plan(
            &[],
            &Heard::default(),
            &Presence::default(),
            &me,
            4,
            W + 1,
            (W + 1) * WEEK,
            true,
        )
        .unwrap();
    assert_eq!(planned.upd, None);
}

/// End to end: each Mac's flusher writes its update record from the installed report as the
/// `upd` row at its own uuid, the reader finds both Macs' rows after a repair, each by its
/// writer and spelling that Mac's record; an unchanged record writes nothing more, and a hold a
/// minute later goes out on the next pass.
/// NEGATIVE CONTROL: with `pass_started` not reading `record` (`book.upd = None`), no row is
/// written (`` a's row ``).
#[test]
fn the_flusher_writes_each_macs_update_record_and_the_reader_finds_it_by_writer() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let spools = spools(scratch.path(), 2);
    let now = W * WEEK + 3600;
    let pass = |n: usize, record: &upd::Records, now: u64| {
        flush::pass_with(
            &lab.paths[n],
            &spools[n],
            &Presence::default,
            &|| None,
            record,
            false,
            now,
        )
        .unwrap()
    };
    let idle = || Some(upd_record());
    assert!(matches!(pass(0, &idle, now), Flushed::Wrote { .. }));
    let held = upd_held(now + 30, "the listener did not answer");
    let held_now = held.clone();
    assert!(matches!(
        pass(1, &move || Some(held_now.clone()), now + 30),
        Flushed::Wrote { .. }
    ));
    repair_commons_at(&lab, 2, &[now + 60, now + 60]);
    let (a, _) = window_at(&lab.paths[0], now + 60);
    let (b, _) = window_at(&lab.paths[1], now + 60);
    let rows = upd::read(&lab.paths[0], now + 60).unwrap().unwrap();
    let find = |uuid: &[u8; 16]| {
        rows.iter()
            .find(|row| row.uuid == hex::encode_lower(uuid))
            .map(|row| row.record.clone())
    };
    assert_eq!(find(&a).map(|r| r.offered), Some(Some(198)), "a's row");
    assert_eq!(find(&b).map(|r| r.hold), Some(held.hold.clone()), "b's row");
    assert_eq!(rows.len(), 2);

    // Unchanged, nothing is due; held a minute later, the next pass writes it.
    assert_eq!(pass(0, &idle, now + 120), Flushed::Idle);
    let later = upd_held(now + 180, "the listener did not answer");
    assert!(matches!(
        pass(0, &move || Some(later.clone()), now + 180),
        Flushed::Wrote { rows: 1, .. }
    ));
    let rows = upd::read(&lab.paths[0], now + 181).unwrap().unwrap();
    let mine = rows
        .iter()
        .find(|row| row.uuid == hex::encode_lower(&a))
        .unwrap();
    assert!(mine.record.hold.is_some());
}

/// A nudge asks this process's flusher for one pass: the ask is taken once.
#[test]
fn a_nudge_is_taken_once() {
    upd::nudge();
    assert!(upd::nudged());
    assert!(!upd::nudged());
}
