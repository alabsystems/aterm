// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The title as a claim: what `meta set title`, `meta unset title` and a person's rename
//! write, whose title the pin shows, and what a title write refuses.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_slog::{Actor, Stamp, Tier};

use crate::session_store::{SessionHandle, Store, new_store, test_handle};
use crate::session_timeline::{
    MetaEdit, MetaField, MetaWriteError, SessionMeta, restore_carried_meta, write_session_meta,
};

/// A registered session and its store.
fn session(local: u64) -> (SessionHandle, Store) {
    let store = new_store();
    let h = test_handle(local);
    store.write().unwrap().register(h.clone());
    (h, store)
}

/// `meta <rest>` as a control client; the reply and whether the pin moved.
fn meta(h: &SessionHandle, store: &Store, rest: &str) -> (String, bool) {
    crate::control::cmd_meta(&h.term, store, h.local_id, &h.ctx, rest)
}

/// A title write by `by`.
fn title(h: &SessionHandle, edit: MetaEdit<'_>, by: Actor) -> Result<bool, MetaWriteError> {
    write_session_meta(&h.ctx, MetaField::Title, edit, by)
}

fn pin(h: &SessionHandle) -> Option<String> {
    h.ctx.meta.lock().unwrap().user_title.clone()
}

/// The session's `desc` and `meta` records at the host tier, each from its kind on (the seq,
/// the `mark -` and the time left off).
fn records(h: &SessionHandle) -> Vec<String> {
    let g = h.ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .map(|r| aterm_slog::encode(r, Tier::Host).expect("a logged record fits"))
        .filter_map(|l| {
            let from = l.find(" desc ").or_else(|| l.find(" meta "))?;
            let to = l.rfind(" t=")?;
            Some(l[from + 1..to].to_string())
        })
        .collect()
}

fn described(h: &SessionHandle) -> String {
    crate::session_log::cmd_described(&h.ctx, "")
}

/// `meta set title` writes a description claim of the session's `title`, by the caller, and
/// the pin and its `meta` record follow because that claim stands; `meta unset title`
/// withdraws it; a repeat of either writes nothing.
///
/// CONTROL: `meta set role` on the same session, a field that is no claim, writes its `meta`
/// record and no `desc`.
#[test]
fn meta_set_title_writes_a_desc_claim() {
    let (h, store) = session(1);
    assert_eq!(
        meta(&h, &store, "set title build agent"),
        ("OK\n".to_string(), true)
    );
    assert_eq!(pin(&h).as_deref(), Some("build agent"));
    assert_eq!(
        records(&h),
        vec![
            "desc field=title value=build%20agent by=ctl src=mark conf=claim",
            "meta field=title value=build%20agent by=ctl src=meta conf=exact",
        ]
    );
    let listed = described(&h);
    assert!(
        listed.contains("\ntitle - value=build%20agent by=ctl rank=ctl behind=- seq="),
        "{listed}"
    );

    assert_eq!(
        meta(&h, &store, "set title build agent"),
        ("OK\n".to_string(), false)
    );
    assert_eq!(records(&h).len(), 2, "a repeat writes no record");

    assert_eq!(
        meta(&h, &store, "set role lead"),
        ("OK\n".to_string(), true)
    );
    assert_eq!(
        records(&h)[2..],
        ["meta field=role value=lead by=ctl src=meta conf=exact"],
        "a field that is no claim writes its meta record alone"
    );

    assert_eq!(meta(&h, &store, "unset title"), ("OK\n".to_string(), true));
    assert_eq!(pin(&h), None);
    assert_eq!(
        records(&h)[3..],
        [
            "desc field=title by=ctl src=mark conf=claim",
            "meta field=title by=ctl src=meta conf=exact",
        ]
    );
    assert_eq!(meta(&h, &store, "unset title"), ("OK\n".to_string(), false));
    assert_eq!(records(&h).len(), 5, "a second unset writes no record");
    assert!(described(&h).contains("\ntitle - value=-\n"));
}

/// A control client's `meta set title` under a person's rename is recorded and listed,
/// answers OK, and leaves the title; its `meta unset title` withdraws its own claim and does
/// not clear the person's.
///
/// CONTROL: once the person clears their own title, the same control client's claim, still
/// held, becomes the title; and with no claim of a person's a control client's stands at
/// once (`meta_set_title_writes_a_desc_claim`).
#[test]
fn a_ctl_title_under_a_persons_rename_is_listed_and_leaves_the_title() {
    let (h, store) = session(2);
    assert_eq!(
        title(&h, MetaEdit::Set("Fold laws"), Actor::Human),
        Ok(true)
    );
    assert_eq!(
        meta(&h, &store, "set title review the fold"),
        ("OK\n".to_string(), false)
    );
    assert_eq!(pin(&h).as_deref(), Some("Fold laws"));
    assert_eq!(
        records(&h),
        vec![
            "desc field=title value=Fold%20laws by=human src=mark conf=claim",
            "meta field=title value=Fold%20laws by=human src=meta conf=exact",
            "desc field=title value=review%20the%20fold by=ctl src=mark conf=claim",
        ],
        "the client's claim is recorded; the title did not move, so no meta record"
    );
    let listed = described(&h);
    assert!(
        listed.contains("\ntitle - value=Fold%20laws by=human rank=person ")
            && listed.contains("\ntitle - other value=review%20the%20fold by=ctl rank=ctl "),
        "{listed}"
    );

    assert_eq!(meta(&h, &store, "unset title"), ("OK\n".to_string(), false));
    assert_eq!(pin(&h).as_deref(), Some("Fold laws"), "a person's title");
    assert!(
        !described(&h).contains(" other "),
        "the client's own is gone"
    );

    assert_eq!(
        meta(&h, &store, "set title review the fold"),
        ("OK\n".to_string(), false)
    );
    assert_eq!(title(&h, MetaEdit::Clear, Actor::Human), Ok(true));
    assert_eq!(
        pin(&h).as_deref(),
        Some("review the fold"),
        "with the person's claim withdrawn the client's stands"
    );
    assert_eq!(
        records(&h).last().map(String::as_str),
        Some("meta field=title value=review%20the%20fold by=human src=meta conf=exact"),
        "the meta record names who moved the title"
    );
}

/// A title from another session, the bridge or another machine's driver is listed and never
/// stands, and neither its write nor its clear moves a pin nobody claims (one a restore
/// carried). Its repeat of either, which could not move that pin, writes no record.
///
/// CONTROL: the same text from a writer aterm ran itself stands, and the listed claims stay
/// beside it; a control client's clear, whose claims can stand, clears the unclaimed pin
/// though that client held no claim to withdraw; and a listed author's first write and first
/// clear each did write their record.
#[test]
fn a_listed_authors_title_never_stands() {
    let (h, _store) = session(3);
    h.ctx.meta.lock().unwrap().user_title = Some("carried".to_string());
    let other = Actor::Sid(aterm_slog::ShortId::new("s-00000000000000000abc").expect("a sid"));
    for by in [other, Actor::Bridge, Actor::Nobody] {
        assert_eq!(title(&h, MetaEdit::Set("theirs"), by), Ok(false), "{by:?}");
        assert_eq!(pin(&h).as_deref(), Some("carried"), "{by:?}");
    }
    let written = records(&h);
    assert_eq!(
        written,
        vec![
            "desc field=title value=theirs by=s-00000000000000000abc src=mark conf=claim",
            "desc field=title value=theirs by=bridge src=mark conf=claim",
            "desc field=title value=theirs src=mark conf=claim",
        ],
        "each listed author's first write is recorded"
    );
    assert_eq!(title(&h, MetaEdit::Set("theirs"), Actor::Bridge), Ok(false));
    assert_eq!(pin(&h).as_deref(), Some("carried"), "a listed repeat");
    assert_eq!(records(&h), written, "a listed repeat writes no record");

    assert_eq!(title(&h, MetaEdit::Clear, Actor::Bridge), Ok(false));
    assert_eq!(pin(&h).as_deref(), Some("carried"), "a listed clear");
    let cleared = records(&h);
    assert_eq!(
        cleared[written.len()..],
        ["desc field=title by=bridge src=mark conf=claim"],
        "a listed author's withdrawal of the claim it holds is recorded"
    );
    assert_eq!(title(&h, MetaEdit::Clear, Actor::Bridge), Ok(false));
    assert_eq!(pin(&h).as_deref(), Some("carried"), "a listed clear again");
    assert_eq!(
        records(&h),
        cleared,
        "a second listed clear writes no record"
    );

    assert_eq!(title(&h, MetaEdit::Clear, Actor::Ctl), Ok(true));
    assert_eq!(pin(&h), None, "a control client's clear");
    assert_eq!(title(&h, MetaEdit::Set("theirs"), Actor::Bridge), Ok(false));
    let listed = described(&h);
    assert!(
        listed.contains("\ntitle - value=-\n")
            && listed.matches(" other value=theirs ").count() == 3
            && listed.matches(" rank=listed ").count() == 3,
        "{listed}"
    );
    assert_eq!(title(&h, MetaEdit::Set("theirs"), Actor::Aterm), Ok(true));
    assert_eq!(pin(&h).as_deref(), Some("theirs"));
    let listed = described(&h);
    assert!(
        listed.contains("\ntitle - value=theirs by=aterm rank=writer ")
            && listed.matches(" rank=listed ").count() == 3,
        "{listed}"
    );
}

/// A title whose percent-encoding is over 256 bytes is refused whole with its own message,
/// though it fits the 120 raw bytes: a claim's text is never cut.
///
/// CONTROL: a title of the same characters that encodes to 252 bytes is taken whole.
#[test]
fn a_title_over_256_encoded_bytes_is_refused_whole() {
    let (h, store) = session(4);
    let long = "构".repeat(40);
    assert_eq!(long.len(), 120);
    assert_eq!(
        title(&h, MetaEdit::Set(&long), Actor::Human),
        Err(MetaWriteError::TooLongEncoded { cap: 256 })
    );
    assert_eq!(
        meta(&h, &store, &format!("set title {long}")),
        (
            "ERR title too long (max 256 bytes percent-encoded)\n".to_string(),
            false
        )
    );
    assert_eq!(pin(&h), None);
    assert_eq!(
        records(&h),
        Vec::<String>::new(),
        "a refusal writes nothing"
    );
    let fits = "构".repeat(28);
    assert_eq!(title(&h, MetaEdit::Set(&fits), Actor::Human), Ok(true));
    assert_eq!(pin(&h).as_deref(), Some(fits.as_str()));
    assert!(records(&h)[0].contains(&format!("value={} ", "%E6%9E%84".repeat(28))));
}

/// A title from anyone but a person is redacted before it is stored, and one that is nothing
/// but a credential is refused, be it a sensitive phrase or a bare token (of which redaction
/// would leave only `[redacted]`): neither the claim nor the pin holds it.
///
/// CONTROL: a person's title of the same texts is stored and shown as typed.
#[test]
fn a_title_that_reads_as_a_credential_is_redacted_or_refused() {
    let (h, store) = session(5);
    let key = format!("sk-proj-{}", "a1B2c3D4".repeat(4));
    assert_eq!(
        meta(&h, &store, "set title password hunter2"),
        ("ERR title reads as a credential\n".to_string(), false)
    );
    assert_eq!(pin(&h), None);
    assert_eq!(records(&h), Vec::<String>::new());
    let bare = [format!("ghp_{}", "0123456789abcdefghij"), "ab12".repeat(16)];
    for token in &bare {
        assert_eq!(
            meta(&h, &store, &format!("set title {token}")),
            ("ERR title reads as a credential\n".to_string(), false),
            "{token}"
        );
        assert_eq!(pin(&h), None, "{token}");
        assert_eq!(records(&h), Vec::<String>::new(), "{token}");
    }
    assert_eq!(
        meta(&h, &store, &format!("set title use {key} here")),
        ("OK\n".to_string(), true)
    );
    assert_eq!(pin(&h).as_deref(), Some("use [redacted] here"));
    assert!(
        records(&h).iter().all(|l| !l.contains("a1B2c3D4")),
        "{:?}",
        records(&h)
    );
    assert_eq!(
        meta(&h, &store, &format!("set title use {key} here")),
        ("OK\n".to_string(), false),
        "the same title again repeats the redacted claim: nothing is written"
    );
    assert_eq!(records(&h).len(), 2);

    let typed = format!("use {key} here");
    assert_eq!(title(&h, MetaEdit::Set(&typed), Actor::Human), Ok(true));
    assert_eq!(pin(&h), Some(typed));
    assert_eq!(
        title(&h, MetaEdit::Set("password hunter2"), Actor::Human),
        Ok(true)
    );
    assert_eq!(pin(&h).as_deref(), Some("password hunter2"));
    for token in &bare {
        assert_eq!(title(&h, MetaEdit::Set(token), Actor::Human), Ok(true));
        assert_eq!(pin(&h).as_ref(), Some(token), "a person's is as typed");
    }
}

/// A title put back by a restore has no author: the restore writes no claim, so the first
/// claim replaces it and a withdrawal that leaves nothing standing clears it, as before
/// titles were claims.
///
/// CONTROL: the restored title is in the pin and `described` lists no claim for it.
#[test]
fn a_restored_title_is_no_ones_claim() {
    let (h, store) = session(6);
    let carried = SessionMeta {
        user_title: Some("from last time".into()),
        ..SessionMeta::default()
    };
    assert!(restore_carried_meta(&h.ctx, &carried));
    assert_eq!(pin(&h).as_deref(), Some("from last time"));
    assert!(described(&h).contains("\ntitle - value=-\n"));
    assert!(
        records(&h).iter().all(|l| !l.starts_with("desc ")),
        "a restore writes no claim: {:?}",
        records(&h)
    );
    assert_eq!(meta(&h, &store, "unset title"), ("OK\n".to_string(), true));
    assert_eq!(pin(&h), None);

    let (h, store) = session(7);
    assert!(restore_carried_meta(&h.ctx, &carried));
    assert_eq!(
        meta(&h, &store, "set title mine now"),
        ("OK\n".to_string(), true)
    );
    assert_eq!(pin(&h).as_deref(), Some("mine now"));
}

/// A person who commits the rename field as it opened, holding the pinned title they do not
/// claim, states nothing: no claim of theirs is written, so the title stays its author's and
/// follows that author's next write. The same holds over a restored title nobody claims.
///
/// CONTROL: a person's commit of an edited text does take the title, and the control
/// client's next write is listed and leaves it.
#[test]
fn an_unedited_rename_commit_leaves_the_title_its_authors() {
    let (h, store) = session(9);
    assert_eq!(meta(&h, &store, "set title C"), ("OK\n".to_string(), true));
    let before = records(&h);
    assert_eq!(title(&h, MetaEdit::Set("C"), Actor::Human), Ok(false));
    assert_eq!(title(&h, MetaEdit::Set("  C "), Actor::Human), Ok(false));
    assert_eq!(records(&h), before, "an unedited commit writes no record");
    assert!(!described(&h).contains(" by=human "), "{}", described(&h));
    assert_eq!(meta(&h, &store, "set title D"), ("OK\n".to_string(), true));
    assert_eq!(
        pin(&h).as_deref(),
        Some("D"),
        "the title follows its author"
    );

    let (h, store) = session(10);
    let carried = SessionMeta {
        user_title: Some("from last time".into()),
        ..SessionMeta::default()
    };
    assert!(restore_carried_meta(&h.ctx, &carried));
    assert_eq!(
        title(&h, MetaEdit::Set("from last time"), Actor::Human),
        Ok(false)
    );
    assert!(
        records(&h).iter().all(|l| !l.starts_with("desc ")),
        "an unedited commit of a restored title writes no claim: {:?}",
        records(&h)
    );
    assert_eq!(pin(&h).as_deref(), Some("from last time"));
    assert_eq!(
        meta(&h, &store, "set title mine now"),
        ("OK\n".to_string(), true),
        "no claim of a person's stands over it"
    );
    assert_eq!(pin(&h).as_deref(), Some("mine now"));

    let (h, store) = session(11);
    assert_eq!(meta(&h, &store, "set title C"), ("OK\n".to_string(), true));
    assert_eq!(title(&h, MetaEdit::Set("C2"), Actor::Human), Ok(true));
    assert_eq!(pin(&h).as_deref(), Some("C2"));
    assert_eq!(
        records(&h)[2..],
        [
            "desc field=title value=C2 by=human src=mark conf=claim",
            "meta field=title value=C2 by=human src=meta conf=exact",
        ]
    );
    assert_eq!(meta(&h, &store, "set title D"), ("OK\n".to_string(), false));
    assert_eq!(pin(&h).as_deref(), Some("C2"), "an edited commit is theirs");
}

/// A time-limited claim is never pinned, because the pin has no clock to take it back: under
/// a program's live `ttl=` title claim, which outranks a control client's, a listed author's
/// title write leaves the pin unset and the control client's pins the client's own text. A
/// control client's `meta set title` of the text it holds under a `ttl=` is no repeat: it
/// replaces that claim with one that has no limit, which is pinned.
///
/// CONTROL: the same program claim with no `ttl=` stands for good, so the control client's
/// title write pins the program's text; and in both sessions the program's claim was
/// written and is listed beside the client's.
#[test]
fn a_time_limited_title_claim_is_never_pinned() {
    let harness = |h: &SessionHandle, rest: &str| {
        crate::session_log::cmd_mark(&h.ctx, Actor::Harness, true, rest).0
    };
    let (h, store) = session(12);
    let reply = harness(&h, "- desc field=title value=Deploying ttl=60");
    assert!(reply.starts_with("OK seq="), "{reply}");
    assert_eq!(pin(&h), None, "a mark's claim pins nothing by itself");
    assert_eq!(title(&h, MetaEdit::Set("theirs"), Actor::Bridge), Ok(false));
    assert_eq!(
        pin(&h),
        None,
        "a listed write does not pin the limited claim"
    );
    assert_eq!(
        meta(&h, &store, "set title Build"),
        ("OK\n".to_string(), true)
    );
    assert_eq!(pin(&h).as_deref(), Some("Build"));
    let listed = described(&h);
    assert!(
        listed.contains(" value=Deploying by=harness rank=program ")
            && listed.contains(" value=Build by=ctl rank=ctl "),
        "{listed}"
    );

    let (h, store) = session(14);
    let (reply, _) = crate::session_log::cmd_mark(
        &h.ctx,
        Actor::Ctl,
        true,
        "- desc field=title value=X ttl=60",
    );
    assert!(reply.starts_with("OK seq="), "{reply}");
    assert_eq!(pin(&h), None, "a mark's claim pins nothing by itself");
    assert_eq!(meta(&h, &store, "set title X"), ("OK\n".to_string(), true));
    assert_eq!(pin(&h).as_deref(), Some("X"));
    assert_eq!(
        records(&h)[1..],
        [
            "desc field=title value=X by=ctl src=mark conf=claim",
            "meta field=title value=X by=ctl src=meta conf=exact",
        ],
        "the claim written has no ttl"
    );
    assert_eq!(meta(&h, &store, "set title X"), ("OK\n".to_string(), false));
    assert_eq!(records(&h).len(), 3, "and its repeat writes nothing");

    let (h, store) = session(13);
    let reply = harness(&h, "- desc field=title value=Deploying");
    assert!(reply.starts_with("OK seq="), "{reply}");
    assert_eq!(pin(&h), None, "a mark's claim pins nothing by itself");
    assert_eq!(
        meta(&h, &store, "set title Build"),
        ("OK\n".to_string(), true)
    );
    assert_eq!(pin(&h).as_deref(), Some("Deploying"));
    let listed = described(&h);
    assert!(
        listed.contains("\ntitle - value=Deploying by=harness rank=program ")
            && listed.contains("\ntitle - other value=Build by=ctl rank=ctl "),
        "{listed}"
    );
}

/// A title write spends its author's mark tokens, so the twenty-first distinct title of a
/// burst answers `ERR busy retry_ms=` and moves nothing; a person's rename is never refused
/// for rate.
///
/// CONTROL: a second later the same author's title is taken, and the person's twenty-five
/// renames in the same instant all were.
#[test]
fn a_title_write_spends_the_authors_mark_tokens_and_a_person_spends_none() {
    let store = new_store();
    let h = test_handle(8);
    let ms = Arc::new(AtomicU64::new(0));
    let read = Arc::clone(&ms);
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .set_clock(crate::session_log::Clock::reading(move || {
            let ms = read.load(Ordering::SeqCst);
            Stamp::new(1_760_000_000_000 + ms, ms)
        }));
    store.write().unwrap().register(h.clone());
    for i in 0..aterm_slog::MARK_BURST {
        assert_eq!(
            meta(&h, &store, &format!("set title t{i}")),
            ("OK\n".to_string(), true),
            "{i}"
        );
    }
    let (reply, moved) = meta(&h, &store, "set title one more");
    assert!(reply.starts_with("ERR busy retry_ms=") && !moved, "{reply}");
    assert_eq!(
        pin(&h),
        Some(format!("t{}", aterm_slog::MARK_BURST - 1)),
        "a busy write moves nothing"
    );
    for i in 0..aterm_slog::MARK_BURST + 5 {
        assert_eq!(
            title(&h, MetaEdit::Set(&format!("rename {i}")), Actor::Human),
            Ok(true),
            "{i}"
        );
    }
    ms.store(aterm_slog::MARK_REFILL_MS, Ordering::SeqCst);
    assert_eq!(
        meta(&h, &store, "set title one more"),
        ("OK\n".to_string(), false),
        "taken, and listed under the person's"
    );
}
