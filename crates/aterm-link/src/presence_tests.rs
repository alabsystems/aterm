// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn phase_is_the_servers_word_from_the_closed_set_or_absent() {
    let mut f = Fields::default();
    for word in PHASES.iter().chain(LEGACY_PHASES.iter()).copied().chain([
        "wall:overloaded",
        "wall:usage-session",
        "wall:api-error",
    ]) {
        f.set_agent(Some(word));
        assert_eq!(f.phase, word);
        assert!(f.tokens(Mode::Meta).contains(&format!(" phase={word} ")));
    }
    for other in [
        None,
        Some("-"),
        Some(""),
        Some("walled"),
        Some("wall:"),
        Some("wall:Overloaded"),
        Some("wall:529 SECRET"),
        Some("busy%20SECRET"),
        Some("Busy"),
        Some("esc to interrupt"),
    ] {
        f.set_agent(Some("busy"));
        f.set_agent(other);
        assert_eq!(f.phase, ABSENT, "{other:?}");
    }
}

#[test]
fn a_row_carries_no_context_token() {
    let mut f = Fields::default();
    f.set_agent(Some("idle"));
    for mode in [Mode::Meta, Mode::Minimal] {
        assert!(!f.tokens(mode).contains("context="), "{mode:?}");
    }
    assert_eq!(
        f.tokens(Mode::Meta),
        " attention=- role=- detail=- phase=idle title=-"
    );
}

#[test]
fn minimal_writes_attention_alone() {
    let mut f = Fields::default();
    f.read_meta("OK title=vim user_title=worker description=- icon=- role=worker attention=stuck cwd=- state=alive");
    f.set_detail(Some("claude"));
    f.set_agent(Some("busy"));
    assert_eq!(f.tokens(Mode::Minimal), " attention=stuck");
    assert_eq!(
        f.tokens(Mode::Meta),
        " attention=stuck role=worker detail=claude phase=busy title=worker"
    );
}

#[test]
fn meta_fields_are_read_capped_and_only_the_user_title_is_published() {
    let mut f = Fields::default();
    f.read_meta("OK title=%E2%9C%B3%20Claude%20Code user_title=- description=- icon=- role=- attention=- cwd=- state=alive");
    assert_eq!(
        f.title, ABSENT,
        "no user title: no title, whatever the program wrote"
    );
    assert_eq!(f.role, ABSENT);
    assert_eq!(f.attention, ABSENT);
    f.read_meta("OK title=Rotating%20the%20sk-live-OSCSECRET42%20key%20for%20acme user_title=- description=- icon=- role=- attention=- cwd=- state=alive");
    assert_eq!(f.title, ABSENT);
    for mode in [Mode::Meta, Mode::Minimal] {
        let t = f.tokens(mode);
        assert!(!t.contains("OSCSECRET") && !t.contains("Rotating"), "{t}");
    }
    f.read_meta("OK title=x user_title=worker%3Aclaude-satcomp description=- icon=- role=worker attention=- cwd=- state=alive");
    assert_eq!(f.title, "worker%3Aclaude-satcomp");
    assert_eq!(f.role, "worker");
    f.read_meta("OK title=Rotating%20the%20key user_title=- description=- icon=- role=worker attention=- cwd=- state=alive");
    assert_eq!(f.title, ABSENT);

    let long = "%C3%A9".repeat(400);
    f.read_meta(&format!(
        "OK title=- user_title={long} description=- icon=- role={} attention={} cwd=- state=alive",
        "r".repeat(500),
        "a".repeat(500)
    ));
    assert!(f.title.len() <= TITLE_CAP, "{}", f.title.len());
    assert_eq!(f.title.len() % 3, 0, "cut inside an escape: {}", f.title);
    assert_eq!(f.role.len(), ROLE_CAP);
    assert_eq!(f.attention.len(), ATTENTION_CAP);
    f.set_detail(Some(&"d".repeat(300)));
    assert_eq!(f.detail.len(), DETAIL_CAP);
    f.set_detail(Some("targo\u{1b}[2J test\n"));
    assert_eq!(f.detail, "targo[2Jtest");
    f.set_detail(Some(""));
    assert_eq!(f.detail, ABSENT);
    f.set_detail(None);
    assert_eq!(f.detail, ABSENT);
    assert_eq!(token("%", 8), ABSENT);
    assert_eq!(token("ab%2", 8), "ab");
}

fn published(slot: &mut Slot, mode: Mode, now: Instant) {
    let mark = slot.mark_published(mode, now);
    slot.landed(mark);
}

#[test]
fn the_writer_publishes_on_change_only_and_at_most_once_per_window() {
    let t0 = Instant::now();
    let m = Mode::Meta;
    let mut slot = Slot::default();
    assert_eq!(slot.due(m, t0), Due::First);
    slot.fields.set_agent(Some("busy"));
    published(&mut slot, m, t0);
    assert_eq!(slot.due(m, t0 + Duration::from_secs(5)), Due::Unchanged);
    slot.fields.set_agent(Some("idle"));
    assert_eq!(slot.due(m, t0 + Duration::from_millis(500)), Due::Capped);
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Changed);
    assert!(slot.due(m, t0 + REPUBLISH_MIN).now());
    assert!(!Due::Capped.now() && !Due::Unchanged.now());
    published(&mut slot, m, t0 + REPUBLISH_MIN);
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
    slot.fields.set_agent(None);
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 2), Due::Changed);
}

/// A ROW SENT BEFORE ANY GENERATION IS OWED AGAIN ONCE ONE IS NAMED, and a later generation
/// alone owes none: the `gen=` a row says comes from status reads, never from a screen read
/// of its own, so a first row whose read found the terminal busy (`gen=-`) is said again
/// with the next read's, and a screen that moved is not, by itself, a row.
#[test]
fn a_row_sent_with_no_generation_is_owed_once_one_is_named() {
    let t0 = Instant::now();
    let m = Mode::Meta;
    let mut slot = Slot::default();
    slot.saw_screen(None);
    assert!(!slot.knows_status());
    assert_eq!(slot.row_gen(), ABSENT);
    published(&mut slot, m, t0);
    slot.saw_screen(None);
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
    let first = ScreenGen { epoch: 1, seq: 7 };
    slot.saw_screen(Some(first));
    assert_eq!(slot.row_gen(), gen_token(first));
    assert_eq!(slot.due(m, t0), Due::Capped);
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Changed);
    published(&mut slot, m, t0 + REPUBLISH_MIN);
    slot.saw_screen(Some(ScreenGen { epoch: 1, seq: 8 }));
    slot.saw_screen(None);
    assert_eq!(
        slot.row_gen(),
        gen_token(ScreenGen { epoch: 1, seq: 8 }),
        "a read naming none keeps the newest"
    );
    assert_eq!(
        slot.due(m, t0 + REPUBLISH_MIN * 3),
        Due::Unchanged,
        "a moved screen alone owes no row"
    );
}

/// A SLOT KNOWS ITS STATUS ONCE READS GAVE BOTH ITS SCREEN AND ITS VERDICT: a generation
/// alone (a batch row that deferred the verdict) or a verdict alone (a read that met the
/// terminal busy) leaves the slot one its row reads `@<sid> status` for. A verdict given as
/// no word (an older server's `status`) is still a verdict.
#[test]
fn a_slot_knows_its_status_once_reads_gave_its_screen_and_its_verdict() {
    let screen = ScreenGen { epoch: 1, seq: 3 };
    let mut deferred = Slot::default();
    deferred.saw_screen(Some(screen));
    assert!(!deferred.knows_status(), "a generation without a verdict");
    deferred.saw_agent(None);
    assert!(deferred.knows_status());
    assert_eq!(deferred.fields.phase, ABSENT);

    let mut busy = Slot::default();
    busy.saw_agent(Some("busy"));
    busy.saw_screen(None);
    assert!(!busy.knows_status(), "a verdict without a generation");
    busy.saw_screen(Some(screen));
    assert!(busy.knows_status());
    assert_eq!(busy.fields.phase, "busy");
}

#[test]
fn a_generation_token_is_its_seq_and_a_fold_of_its_epoch_and_seq() {
    let token = gen_token(ScreenGen { epoch: 2, seq: 41 });
    let (seq, fold) = token.split_once(':').expect("<content_seq>:<fp16>");
    assert_eq!(seq, "41");
    assert_eq!(fold, format!("{:016x}", aterm_hash::fnv1a_64(b"2.41")));
    assert_ne!(
        token,
        gen_token(ScreenGen { epoch: 3, seq: 41 }),
        "one seq in two epochs reads apart"
    );
}

#[test]
fn a_refused_row_falls_back_to_the_newest_row_that_landed() {
    let t0 = Instant::now();
    let m = Mode::Meta;
    let mut slot = Slot::default();
    slot.fields.set_agent(Some("busy"));
    let first = slot.mark_published(m, t0);
    assert_eq!(
        slot.due(m, t0),
        Due::Unchanged,
        "in flight counts as on the bus"
    );
    slot.fields.set_agent(Some("idle"));
    assert_eq!(
        slot.due(m, t0),
        Due::Capped,
        "the window counts from the send"
    );
    slot.unmark(first);
    assert_eq!(
        slot.due(m, t0),
        Due::First,
        "refused: nothing is on the bus"
    );

    let idle = slot.mark_published(m, t0);
    slot.landed(idle);
    slot.fields.set_agent(Some("busy"));
    let busy = slot.mark_published(m, t0 + REPUBLISH_MIN);
    slot.fields.set_agent(Some("prompt"));
    let prompt = slot.mark_published(m, t0 + REPUBLISH_MIN * 2);
    slot.unmark(busy);
    assert_eq!(
        slot.due(m, t0 + REPUBLISH_MIN * 2),
        Due::Unchanged,
        "a row sent after the refused one stands until its own answer"
    );
    slot.unmark(prompt);
    slot.fields.set_agent(Some("idle"));
    assert_eq!(
        slot.due(m, t0 + REPUBLISH_MIN * 3),
        Due::Unchanged,
        "the bus says `idle`, the row that landed, and not the refused `busy`"
    );
    slot.fields.set_agent(Some("busy"));
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 3), Due::Changed);
}

#[test]
fn a_minimal_row_is_not_owed_for_a_field_it_never_writes() {
    let t0 = Instant::now();
    let m = Mode::Minimal;
    let mut slot = Slot::default();
    slot.fields.read_meta(
        "OK title=one user_title=one description=- icon=- role=- attention=- cwd=- state=alive",
    );
    published(&mut slot, m, t0);
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
    slot.fields.read_meta("OK title=two user_title=two description=- icon=- role=worker attention=- cwd=- state=alive");
    slot.fields.set_detail(Some("claude"));
    slot.fields.set_agent(Some("busy"));
    assert_eq!(
        slot.due(m, t0 + REPUBLISH_MIN * 2),
        Due::Unchanged,
        "minimal writes attention= alone, so nothing else can make a row due"
    );
    assert_eq!(slot.due(Mode::Meta, t0 + REPUBLISH_MIN * 2), Due::Changed);
    slot.fields.read_meta("OK title=two user_title=two description=- icon=- role=worker attention=stuck cwd=- state=alive");
    assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 2), Due::Changed);
}
