// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: `notice later`, the wire's door to the digest. One keyed entry per fact
//! however often a tool says it, the owner's dismissal holding against every repeat, the
//! tool's `done` ending it, a cap that leaves aterm's own entries their place, and the
//! one button that opens a web page and nothing else.

use crate::center::MessageCenter;
use crate::exceptions::{Entry, Family};
use crate::log::{LogLine, MessageLog, Retired};
use crate::model::{ActionIndex, Intent, MessageId, Severity, WallStamp, tags};
use crate::need::{DIGEST_CAP, Reason, Then, Why};
use crate::wire::{
    Applied, DETAIL_LONG, KEY_REFUSAL, LATER_USAGE, LaterRequest, NoticeRequest, OWN_TAG_REFUSAL,
    Paint, REASON_LONG, STEP_LONG, URL_REFUSAL, WireGate, apply,
};
use crate::{Instant, LOG_CAP, WIRE_LATER_CAP, WIRE_MINTS_PER_WINDOW};

const DAY_MS: u64 = 86_400_000;
const T0_MS: u64 = 1_790_000_000_000;

/// The line codex-alab sends for an account's security setup, as the design spells it.
const SETUP: &str = "later codex.security-setup.0123abcd tag=codex \
                     url=https://chatgpt.com/security/setup?src=codex \
                     Set up Codex account security -- \
                     only you can enrol a second factor for your account -- \
                     Finish the setup in your browser -- \
                     Protect your account\\nAdd a passkey or an authenticator app";

fn wall(day_ms: u64) -> WallStamp {
    WallStamp {
        unix_ms: T0_MS + day_ms,
    }
}

fn fresh(now: Instant) -> (MessageCenter, WireGate) {
    (
        MessageCenter::new(MessageLog::empty(), now),
        WireGate::default(),
    )
}

/// Parse and apply one `notice` line at wall clock `at`, as the host's two threads do.
fn notice(c: &mut MessageCenter, g: &mut WireGate, line: &str, at: u64, now: Instant) -> Applied {
    let req = NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    apply(c, g, req, wall(at), now)
}

/// `(id, how)` from an `OK later=<id> how=<word>` reply.
fn later_reply(a: &Applied) -> (MessageId, String) {
    let rest = a
        .reply
        .strip_prefix("OK later=")
        .unwrap_or_else(|| panic!("{}", a.reply));
    let (id, how) = rest
        .split_once(" how=")
        .unwrap_or_else(|| panic!("{}", a.reply));
    (
        MessageId::from_raw(id.parse().unwrap()).unwrap(),
        how.to_string(),
    )
}

/// A one-line `later` under `key` with its own words.
fn line(key: &str, step: &str) -> String {
    format!("later {key} Renew the signing key -- only you hold the passphrase -- {step}")
}

/// One of aterm's own exceptions, under its own family's key.
fn own_entry(c: &mut MessageCenter, subject: &str, at: u64, now: Instant) {
    let msg = Entry::new(
        Family::Disk,
        subject,
        "full",
        "Disk nearly full",
        Why::OnlyYou(Reason::new("only you know which files can go")),
        Then::step("Free some space on the disk"),
    )
    .message()
    .expect("a well-formed entry");
    c.post(msg, wall(at), now);
}

#[test]
fn notice_later_parses_its_form_and_refuses_junk() {
    let Ok(NoticeRequest::Later(req)) = NoticeRequest::parse(SETUP) else {
        panic!("{SETUP}");
    };
    assert_eq!(
        req,
        LaterRequest {
            key: "wire.codex.security-setup.0123abcd".into(),
            tag: crate::model::Tag::try_new("codex").unwrap(),
            url: Some("https://chatgpt.com/security/setup?src=codex".into()),
            title: "Set up Codex account security".into(),
            reason: "only you can enrol a second factor for your account".into(),
            step: "Finish the setup in your browser".into(),
            detail: vec![
                "Protect your account".into(),
                "Add a passkey or an authenticator app".into(),
            ],
        }
    );
    // The least form: no tag, no url, no detail.
    let Ok(NoticeRequest::Later(bare)) = NoticeRequest::parse(&line("k", "Renew it")) else {
        panic!();
    };
    assert_eq!(
        (bare.tag, bare.url, bare.detail.len()),
        (tags::SCRIPT, None, 0)
    );
    // Fields at their caps stand whole.
    let reason90 = format!("only you {}", "a".repeat(81));
    let step90 = format!("Renew {}", "b".repeat(84));
    let detail240 = "c".repeat(240);
    let at_caps = format!("later k Renew it -- {reason90} -- {step90} -- {detail240}");
    let Ok(NoticeRequest::Later(full)) = NoticeRequest::parse(&at_caps) else {
        panic!("{at_caps}");
    };
    assert_eq!((full.reason, full.step), (reason90.clone(), step90.clone()));
    assert_eq!(full.detail, [detail240.clone()]);

    let long = |n: usize, c: char| c.to_string().repeat(n);
    let refused: Vec<(String, &str)> = vec![
        ("later".into(), LATER_USAGE),
        ("later k".into(), LATER_USAGE),
        ("later k Title only".into(), LATER_USAGE),
        ("later k Title -- a reason only".into(), LATER_USAGE),
        ("later k Title -- a reason -- ".into(), LATER_USAGE),
        ("later k Title -- -- a step".into(), LATER_USAGE),
        ("later k -- why -- Do it".into(), LATER_USAGE),
        ("later K Title -- why -- Do it".into(), KEY_REFUSAL),
        (
            format!("later {} Title -- why -- Do it", long(41, 'k')),
            KEY_REFUSAL,
        ),
        (
            "later k tag=update Title -- why -- Do it".into(),
            OWN_TAG_REFUSAL,
        ),
        ("later k tag=Bad Title -- why -- Do it".into(), LATER_USAGE),
        (
            "later k url=http://chatgpt.com/ Title -- why -- Do it".into(),
            URL_REFUSAL,
        ),
        (
            "later k url=file:///etc/hosts Title -- why -- Do it".into(),
            URL_REFUSAL,
        ),
        (
            "later k url=javascript:alert(1) Title -- why -- Do it".into(),
            URL_REFUSAL,
        ),
        (
            "later k url=https://me@chatgpt.com/ Title -- why -- Do it".into(),
            URL_REFUSAL,
        ),
        (
            "later k url=https://me:pw@chatgpt.com/ Title -- why -- Do it".into(),
            URL_REFUSAL,
        ),
        (
            "later k url=https://chatgpt.com:8443/ Title -- why -- Do it".into(),
            URL_REFUSAL,
        ),
        ("later k url= Title -- why -- Do it".into(), URL_REFUSAL),
        (
            format!(
                "later k url=https://chatgpt.com/{} Title -- why -- Do it",
                long(600, 'p')
            ),
            URL_REFUSAL,
        ),
        (
            "later k url=https://a.com/ url=https://b.com/ Title -- why -- Do it".into(),
            LATER_USAGE,
        ),
        (
            format!("later k Title -- {reason90}x -- Do it"),
            REASON_LONG,
        ),
        (format!("later k Title -- why -- {step90}x"), STEP_LONG),
        (
            format!("later k Title -- why -- Do it -- {detail240}x"),
            DETAIL_LONG,
        ),
        (
            "later k Title -- why -- Do it -- a\\nb\\nc\\nd".into(),
            DETAIL_LONG,
        ),
        (
            "later k Title: with a clause -- why -- Do it".into(),
            "notice: title has a clause (\u{2014}, ;, :); put it after --",
        ),
        (
            format!("later k {} -- why -- Do it", long(49, 'T')),
            "notice: title over 48 characters; put the rest after --",
        ),
        (
            "later k Renew it -- it is waiting for Apple -- Renew it".into(),
            "notice: the reason only says something is pending; say why only the owner can close it",
        ),
        (
            "later k Renew it -- only you can -- Snooze it".into(),
            "notice: a snooze is not an action; name the step that closes it",
        ),
        (
            "later k Renew it -- only you can -- Renew it or drop it".into(),
            "notice: two actions in one step; name the one that closes it",
        ),
    ];
    for (line, want) in &refused {
        assert_eq!(NoticeRequest::parse(line), Err(*want), "{line:?}");
    }
    // A refusal of `later` is never the unknown-form usage an older aterm answers,
    // which the tool reads as "no `later` here".
    for (line, _) in &refused {
        let err = NoticeRequest::parse(line).unwrap_err();
        assert!(!err.starts_with("usage: notice <post|"), "{line:?}: {err}");
    }
    assert!(
        NoticeRequest::parse("frobnicate x")
            .unwrap_err()
            .starts_with("usage: notice <post|progress|done|ask|later|")
    );
}

/// The same fact said again (another tab, another launch) is the one entry it was: a
/// repeat folds into it and spends nothing, new words restate it in place, and the key
/// is kept whole. Negative control: another key is another entry.
#[test]
fn a_repeated_later_key_folds_into_one_entry() {
    let now = Instant::now();
    let (mut c, mut g) = fresh(now);
    let first = notice(&mut c, &mut g, SETUP, 0, now);
    let (id, how) = later_reply(&first);
    assert_eq!(how, "new");
    assert_eq!((first.paint, first.note), (Paint::None, true));
    assert_eq!(c.live_rows().count(), 0, "never a band row");
    let view = c.digest();
    assert_eq!(view.len(), 1);
    let e = &view.entries[0];
    assert_eq!(e.id, id);
    assert_eq!(e.key, "wire.codex.security-setup.0123abcd", "kept whole");
    assert_eq!(e.severity, Severity::Info);
    assert_eq!(e.title, "Set up Codex account security");
    assert_eq!(e.why, "only you can enrol a second factor for your account");
    assert_eq!(e.then, "Finish the setup in your browser");
    assert_eq!(
        e.actions,
        [Intent::OpenLink {
            url: "https://chatgpt.com/security/setup?src=codex".into()
        }]
    );
    assert_eq!(
        e.detail,
        [
            "only you can enrol a second factor for your account",
            "Finish the setup in your browser",
            "Opens a page on chatgpt.com",
            "Protect your account",
            "Add a passkey or an authenticator app",
        ]
    );
    // Every tab of every launch says it again: one entry, and the budget untouched.
    for _ in 0..WIRE_MINTS_PER_WINDOW + 5 {
        let again = notice(&mut c, &mut g, SETUP, 0, now);
        assert_eq!(later_reply(&again), (id, "duplicate".into()));
        assert_eq!((again.paint, again.note), (Paint::None, false));
    }
    assert_eq!(c.digest().len(), 1);
    assert_eq!(
        c.digest().entries[0].repeats,
        1 + WIRE_MINTS_PER_WINDOW as u32 + 5
    );
    // A new step is news at once, in place under the same id.
    let reworded = SETUP.replace("Finish the setup", "Complete the setup");
    let r = notice(&mut c, &mut g, &reworded, 0, now);
    assert_eq!(later_reply(&r), (id, "restated".into()));
    assert_eq!(
        c.digest().entries[0].then,
        "Complete the setup in your browser"
    );
    // A 40-character key is kept whole too, not cut to a 20-character cause.
    let k40 = format!("codex.{}", "a".repeat(34));
    let k40b = format!("codex.{}b", "a".repeat(33));
    notice(&mut c, &mut g, &line(&k40, "Renew it"), 0, now);
    notice(&mut c, &mut g, &line(&k40b, "Renew it"), 0, now);
    // Negative control: three keys, three entries.
    let keys: Vec<String> = c.digest().entries.iter().map(|e| e.key.clone()).collect();
    assert_eq!(keys.len(), 3, "{keys:?}");
    assert!(keys.contains(&format!("wire.{k40}")) && keys.contains(&format!("wire.{k40b}")));
}

/// The owner's dismissal holds: the same key said again inside the week is quiet
/// (nothing shown, nothing logged, the record that keeps it quiet named), and after the
/// week it is a new entry. Negative control: another key is not quiet.
#[test]
fn the_owners_dismissal_of_a_later_entry_holds() {
    let now = Instant::now();
    let (mut c, mut g) = fresh(now);
    let (id, _) = later_reply(&notice(&mut c, &mut g, SETUP, 0, now));
    let d = notice(&mut c, &mut g, &format!("dismiss {id}"), DAY_MS, now);
    assert_eq!(d.reply, format!("OK dismissed={id}"));
    assert!(c.digest().is_empty());
    let records = c.log().records().count();
    for day in [1, 3, 6] {
        let q = notice(&mut c, &mut g, SETUP, day * DAY_MS + 1, now);
        assert_eq!(later_reply(&q), (id, "quiet".into()), "day {day}");
        assert_eq!((q.paint, q.note), (Paint::None, false));
    }
    assert!(c.digest().is_empty(), "quiet: nothing shown");
    assert_eq!(c.log().records().count(), records, "quiet: nothing logged");
    // Negative control: a different key is news.
    let other = notice(&mut c, &mut g, &line("other", "Renew it"), 2 * DAY_MS, now);
    assert_eq!(later_reply(&other).1, "new");
    // A week after the dismissal, the fact may be shown again.
    let back = notice(&mut c, &mut g, SETUP, 8 * DAY_MS + 1, now);
    let (again, how) = later_reply(&back);
    assert_eq!(how, "new");
    assert_ne!(again, id, "a new entry");
    // A second dismissal backs off: four weeks, not one.
    notice(&mut c, &mut g, &format!("dismiss {again}"), 9 * DAY_MS, now);
    let q = notice(&mut c, &mut g, SETUP, 20 * DAY_MS, now);
    assert_eq!(later_reply(&q).1, "quiet");
}

/// The dismissal outlasts the wire's own churn: once the log ring is full and the wire
/// holds its share, every new record evicts the oldest retired wire record, and the
/// dismissal of a tool's entry is one. A script's ordinary records after it (a build's
/// `notice post`, many times a ring) must not take it, or the nag is back the next
/// launch of the tool. Negative control: an entry the tool ended (`done`, no filing)
/// keeps nothing quiet whatever the churn.
#[test]
fn a_flood_of_wire_records_never_evicts_the_owners_dismissal() {
    let now = Instant::now();
    let (mut c, mut g) = fresh(now);
    let (id, _) = later_reply(&notice(&mut c, &mut g, SETUP, 0, now));
    notice(&mut c, &mut g, &format!("dismiss {id}"), DAY_MS, now);
    let other = line("other", "Renew it");
    notice(&mut c, &mut g, &other, DAY_MS, now);
    notice(&mut c, &mut g, "done other", DAY_MS, now);
    for i in 0..2 * LOG_CAP {
        // A fresh gate per line: the budget is the wire's pacing, not this test's subject.
        let rec = notice(
            &mut c,
            &mut WireGate::default(),
            &format!("post script Built step {i}"),
            DAY_MS + 1,
            now,
        );
        assert!(rec.reply.starts_with("OK recorded"), "{}", rec.reply);
    }
    assert_eq!(c.log().records().count(), LOG_CAP, "the ring is full");
    let q = notice(&mut c, &mut g, SETUP, 2 * DAY_MS, now);
    assert_eq!(later_reply(&q), (id, "quiet".into()), "the dismissal holds");
    let back = notice(&mut c, &mut g, &other, 2 * DAY_MS, now);
    assert_eq!(later_reply(&back).1, "new", "an end keeps nothing quiet");
}

/// The tool says the fact ended: `done` ends the entry (no row holds the key), its
/// record keeps the words the tool finished with, and the next post is news, since an
/// end is not the owner's dismissal.
#[test]
fn notice_done_ends_a_later_entry() {
    let now = Instant::now();
    let (mut c, mut g) = fresh(now);
    let (id, _) = later_reply(&notice(&mut c, &mut g, SETUP, 0, now));
    let done = notice(
        &mut c,
        &mut g,
        "done codex.security-setup.0123abcd ok Codex account secured",
        DAY_MS,
        now,
    );
    assert_eq!(done.reply, format!("OK done={id} how=resolved-ok"));
    assert_eq!((done.paint, done.note), (Paint::None, true));
    assert!(c.digest().is_empty());
    let rec = c.log().get(id).unwrap();
    assert_eq!(
        rec.retired(),
        Some(&Retired::Resolved(crate::center::Outcome::Ok))
    );
    assert_eq!(rec.title, "Codex account secured");
    let back = notice(&mut c, &mut g, SETUP, 2 * DAY_MS, now);
    assert_eq!(later_reply(&back).1, "new", "an end keeps nothing quiet");
    let warn = notice(
        &mut c,
        &mut g,
        "done codex.security-setup.0123abcd warn",
        3 * DAY_MS,
        now,
    );
    assert!(warn.reply.ends_with(" how=resolved-warn"), "{}", warn.reply);
    let gone = notice(
        &mut c,
        &mut g,
        "done codex.security-setup.0123abcd",
        3 * DAY_MS,
        now,
    );
    assert_eq!(gone.reply, "OK done=- how=gone");
    assert!(!gone.note, "an already resolved record is unchanged");

    // Filing is not proof that the cause ended. Without done it stays quiet, but a
    // confirmed end after either kind of filing allows a genuinely new occurrence.
    for (answered, progress) in [(false, false), (true, false), (false, true), (true, true)] {
        let (mut c, mut g) = fresh(now);
        let (id, _) = later_reply(&notice(&mut c, &mut g, SETUP, 0, now));
        if answered {
            assert!(matches!(
                c.act_digest(id, ActionIndex(0), wall(DAY_MS), now),
                Some(Intent::OpenLink { .. })
            ));
        } else {
            notice(&mut c, &mut g, &format!("dismiss {id}"), DAY_MS, now);
        }
        let other_line = line("other", "Renew it");
        let (other, _) = later_reply(&notice(&mut c, &mut g, &other_line, DAY_MS, now));
        notice(&mut c, &mut g, &format!("dismiss {other}"), DAY_MS, now);
        let other_before = c.log().get(other).unwrap().clone();
        let quiet = notice(&mut c, &mut g, SETUP, 2 * DAY_MS, now);
        assert_eq!(later_reply(&quiet), (id, "quiet".into()));
        let live = progress.then(|| {
            let posted = notice(
                &mut c,
                &mut g,
                "progress codex.security-setup.0123abcd busy Securing the Codex account",
                2 * DAY_MS,
                now,
            );
            let live = c
                .live_by_key("wire.codex.security-setup.0123abcd")
                .unwrap()
                .id;
            assert_ne!(live, id);
            assert_eq!(posted.reply, format!("OK message={live}"));
            let quiet = notice(&mut c, &mut g, SETUP, 2 * DAY_MS, now);
            assert_eq!(later_reply(&quiet), (id, "quiet".into()));
            live
        });
        let revision = c.projection_revision();
        let done = notice(
            &mut c,
            &mut g,
            "done codex.security-setup.0123abcd ok Codex account secured",
            3 * DAY_MS,
            now,
        );
        if let Some(live) = live {
            assert_eq!(done.reply, format!("OK done={live} how=resolved-ok"));
            assert_eq!((done.paint, done.note), (Paint::Now, true));
            assert!(
                c.live_by_key("wire.codex.security-setup.0123abcd")
                    .is_none()
            );
        } else {
            assert_eq!(done.reply, "OK done=- how=gone");
            assert_eq!((done.paint, done.note), (Paint::None, true));
        }
        assert_ne!(
            c.projection_revision(),
            revision,
            "the retired outcome is observable"
        );
        assert!(matches!(
            c.log().get(id).unwrap().retired(),
            Some(Retired::Resolved(crate::center::Outcome::Ok))
        ));
        let back = notice(&mut c, &mut g, SETUP, 4 * DAY_MS, now);
        let (next, how) = later_reply(&back);
        assert_eq!(how, "new");
        assert_ne!(next, id);
        assert_eq!(c.log().get(other).unwrap(), &other_before);
        let other_quiet = notice(&mut c, &mut g, &other_line, 4 * DAY_MS, now);
        assert_eq!(later_reply(&other_quiet), (other, "quiet".into()));
    }
}

/// At most four wire entries are open, and none is filed into a full digest, so a flood
/// of keys never evicts one of aterm's own entries. A refused line changes nothing; a
/// repeat of an open key still folds past the cap. Negative control: a post let into a
/// full digest (aterm's own) files the oldest entry, here one of aterm's, which is what
/// the wire's refusal keeps a wire line from doing.
#[test]
fn the_wire_caps_its_later_entries_and_never_evicts_aterms_own() {
    let now = Instant::now();
    let (mut c, mut g) = fresh(now);
    // aterm's own come first, so they are the oldest the cap would file.
    for i in 0..DIGEST_CAP - WIRE_LATER_CAP {
        own_entry(&mut c, &format!("vol{i}"), 1 + i as u64, now);
    }
    for k in 1..=WIRE_LATER_CAP {
        let a = notice(
            &mut c,
            &mut g,
            &line(&format!("k{k}"), "Renew it"),
            100,
            now,
        );
        assert_eq!(later_reply(&a).1, "new");
    }
    assert_eq!(c.digest().len(), DIGEST_CAP);
    let before = (c.digest(), c.log().records().count());
    let fifth = notice(&mut c, &mut g, &line("k5", "Renew it"), 100, now);
    assert_eq!(
        fifth.reply,
        format!(
            "ERR notice: {WIRE_LATER_CAP} wire entries are open for later; end one with notice done"
        )
    );
    assert_eq!(
        (c.digest(), c.log().records().count()),
        before,
        "side-effect free"
    );
    // An open key still folds, and new words still restate it.
    let again = notice(&mut c, &mut g, &line("k1", "Renew it"), 100, now);
    assert_eq!(later_reply(&again).1, "duplicate");
    let reworded = notice(&mut c, &mut g, &line("k1", "Rotate it"), 100, now);
    assert_eq!(later_reply(&reworded).1, "restated");
    // Under the cap and with room, a new key is filed.
    notice(&mut c, &mut g, "done k4", 100, now);
    let room = notice(&mut c, &mut g, &line("k6", "Renew it"), 100, now);
    assert_eq!(later_reply(&room).1, "new");
    // Under the cap but with the list full of aterm's own, it is refused.
    notice(&mut c, &mut g, "done k6", 100, now);
    own_entry(&mut c, "vol-last", 200, now);
    assert_eq!(c.digest().len(), DIGEST_CAP);
    let own_before = c.digest_keys_under("disk.");
    let full = notice(&mut c, &mut g, &line("k7", "Renew it"), 300, now);
    assert_eq!(
        full.reply,
        format!(
            "ERR notice: the owner has {DIGEST_CAP} entries to look at; aterm's own keep their place"
        )
    );
    assert_eq!(
        c.digest_keys_under("disk."),
        own_before,
        "none of aterm's evicted"
    );
    // Negative control: a post let into the full list files its oldest entry.
    own_entry(&mut c, "vol-over", 400, now);
    assert_eq!(c.digest().len(), DIGEST_CAP);
    assert!(
        !c.digest_keys_under("disk.")
            .contains(&"disk.vol0.full".to_string()),
        "the cap filed aterm's oldest: {:?}",
        c.digest_keys_under("disk.")
    );
}

/// `Open in browser` is the entry's one action: the press hands the host the link and
/// files the entry answered, which holds like a dismissal.
#[test]
fn a_press_of_open_in_browser_files_the_entry_answered() {
    let now = Instant::now();
    let (mut c, mut g) = fresh(now);
    let (id, _) = later_reply(&notice(&mut c, &mut g, SETUP, 0, now));
    let intent = c.act_digest(id, ActionIndex(0), wall(DAY_MS), now);
    assert_eq!(
        intent,
        Some(Intent::OpenLink {
            url: "https://chatgpt.com/security/setup?src=codex".into()
        })
    );
    assert!(c.digest().is_empty());
    assert_eq!(
        c.log().get(id).unwrap().retired(),
        Some(&Retired::Answered {
            label: "Open in browser".into()
        })
    );
    let q = notice(&mut c, &mut g, SETUP, 2 * DAY_MS, now);
    assert_eq!(later_reply(&q), (id, "quiet".into()));
    // The entry's words survive the log's codec with the link on it.
    let line = crate::log::LogLine::Posted(c.log().get(id).unwrap().clone()).encode();
    let back = crate::log::LogLine::decode(&line).unwrap();
    let crate::log::LogLine::Posted(rec) = back else {
        panic!("{line}");
    };
    assert_eq!(rec.actions, c.log().get(id).unwrap().actions);
    assert!(rec.later);
}

/// Across a restart or an update the entry is adopted from the log, and the tool may
/// not say it again for days: its `Open in browser` is still the entry's own button, so
/// the owner's press answers it and the answer holds against the tool's next post.
/// Negative control: an entry with no link keeps its one step in words, no button.
#[test]
fn a_press_after_a_restart_still_answers_a_later_entry() {
    let now = Instant::now();
    let (mut c, mut g) = fresh(now);
    let (id, _) = later_reply(&notice(&mut c, &mut g, SETUP, 0, now));
    let bare = later_reply(&notice(&mut c, &mut g, &line("bare", "Renew it"), 0, now)).0;
    let lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.iter().filter_map(|l| LogLine::decode(l).ok()));
    let mut next = MessageCenter::new(log, now);
    assert_eq!(next.adopt_digest(wall(DAY_MS), now), 2);
    let intent = next.act_digest(id, ActionIndex(0), wall(DAY_MS), now);
    assert!(
        matches!(intent, Some(Intent::OpenLink { .. })),
        "{intent:?}"
    );
    assert_eq!(
        next.log().get(id).unwrap().retired(),
        Some(&Retired::Answered {
            label: "Open in browser".into()
        }),
        "the press answered the adopted entry"
    );
    let q = notice(&mut next, &mut WireGate::default(), SETUP, 2 * DAY_MS, now);
    assert_eq!(later_reply(&q), (id, "quiet".into()));
    let open = next.digest();
    assert_eq!(open.len(), 1);
    assert_eq!(
        (open.entries[0].id, open.entries[0].actions.len()),
        (bare, 0)
    );
}
