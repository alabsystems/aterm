// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `IdleHost::needs_standing`, `store_needs`, and `SessionTimeline::set_harness_needs`.
use std::sync::mpsc;

use aterm_types::needs::AttentionKind;

use super::*;
use crate::App;
use crate::needs::{HarnessNeeds, ToldNeeds};
use crate::session_timeline::MetaEdit;

const NEED_A: &str = "grant Screen Recording to com.aterm.aterm in System Settings ▸ Privacy \
                      & Security ▸ Screen Recording, so the windowed check can capture the \
                      menu";
const NEED_B: &str = "the release credential (~/.aterm/release-credentials.toml) to sign 0.48.0";
const NEED_C: &str = "approve the 0.48.0 release notes";
const ENTRY_AB: &str = "claude needs you: grant Screen Recording to com.aterm.aterm in System \
                        Settings ▸ … (+1 more) \u{2014} carrying on";
const ENTRY_B: &str = "claude needs you: the release credential \
                       (~/.aterm/release-credentials.toml) to sign 0.48.0 \u{2014} carrying on";
const ENTRY_C: &str = "claude needs you: approve the 0.48.0 release notes \u{2014} carrying on";
const ENTRY_C_CUT: &str = "claude needs you: approve the 0.48.0 rel\u{2026}";
const ENTRY_OTHER: &str = "claude needs you: something nobody told the window";

fn set(needs: &[&str]) -> Vec<String> {
    needs.iter().map(|need| (*need).to_string()).collect()
}

enum Step {
    /// The loop tells the window's host the set and the entry's text
    /// (`IdleHost::needs_standing`).
    Tell(Vec<String>, String),
    /// The loop's `meta set attention owner=needs kind=ask <text>` reaches
    /// the server, which writes it (`None`: its `unset`).
    Land(Option<String>),
}

struct Desk {
    app: App,
    host: HostHandle,
    steps: mpsc::Sender<Step>,
    /// After each step, what the timeline held as it was taken.
    done: mpsc::Receiver<ToldNeeds>,
    facts: mpsc::Receiver<crate::HarnessFact>,
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.host.shutdown_and_join();
    }
}

impl Desk {
    fn new() -> Self {
        let app = App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        let sid = ctx.self_id.as_str().to_string();
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
        }
        let (steps, steps_rx) = mpsc::channel::<Step>();
        let (done_tx, done) = mpsc::channel();
        let (steps_rx, done_tx) = (Mutex::new(steps_rx), Mutex::new(done_tx));
        let body_ctx = Arc::clone(&ctx);
        let body: Arc<BodyFn> = Arc::new(move |job: &WorkerJob| {
            let idle = job.opts.idle_host.as_ref().expect("the worker's idle host");
            while !job.stop.load(Ordering::SeqCst) {
                let step = steps_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(5));
                let held = match step {
                    Ok(Step::Tell(needs, entry)) => {
                        idle.needs_standing(&needs, &entry);
                        body_ctx.timeline.lock().unwrap().harness_needs()
                    }
                    Ok(Step::Land(text)) => {
                        let held = body_ctx.timeline.lock().unwrap().harness_needs();
                        let edit = text.as_deref().map_or(MetaEdit::Clear, MetaEdit::Set);
                        crate::session_timeline::write_attention_kind(
                            &body_ctx,
                            NEEDS_ATTENTION_OWNER,
                            edit,
                            text.as_ref().map(|_| AttentionKind::Ask),
                            aterm_slog::Actor::Ctl,
                        )
                        .expect("the needs owner's entry written");
                        held
                    }
                    Err(_) => continue,
                };
                let _ = done_tx.lock().unwrap().send(held);
            }
            BodyEnd::Stopped
        });
        let world = Arc::new(World::default());
        world.set(&[(sid.as_str(), Program::Claude)]);
        world.word(&sid, Some(Autopilot::Cruise));
        let mut h = hooks(&world, body);
        let store = app.store.clone();
        h.needs = Arc::new(move |sid, needs, entry| store_needs(&store, sid, needs, entry));
        let (fact_tx, facts) = mpsc::channel();
        let fact_tx = Mutex::new(fact_tx);
        let mine = sid;
        h.fact = Arc::new(move |sid, fact| {
            // The needs fact alone: a run's end also tells `Task(None)` and
            // `Pause(None)`, which other tests read.
            if sid == mine && fact == crate::HarnessFact::Needs {
                let _ = fact_tx.lock().unwrap().send(fact);
            }
        });
        let host = HostHandle::start(on(), false, false, h);
        Self {
            app,
            host,
            steps,
            done,
            facts,
        }
    }

    fn step(&self, step: Step) -> ToldNeeds {
        self.steps.send(step).expect("the body listens");
        self.done
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("not within 60 s: the step taken")
    }

    fn woken(&self) -> usize {
        self.facts.try_iter().count()
    }

    fn facts(&self) -> crate::needs::NeedFacts {
        let handle = self.app.store.read().unwrap().by_local(0).cloned();
        let handle = handle.expect("session 0");
        self.app.need_facts_with(
            &handle,
            |_| true,
            &Default::default(),
            crate::metrics::now_us(),
            0,
        )
    }
}

fn needs_entry(facts: &crate::needs::NeedFacts) -> Option<&str> {
    facts
        .attention
        .iter()
        .find(|entry| entry.owner == NEEDS_ATTENTION_OWNER)
        .map(|entry| entry.text.as_str())
}

#[test]
fn the_words_are_on_the_timeline_before_the_entry_lands() {
    let desk = Desk::new();
    let ab = set(&[NEED_A, NEED_B]);
    assert!(
        NEED_A.chars().count() > 64,
        "need (a) is longer than the entry's cut"
    );
    assert!(!ENTRY_AB.contains(NEED_A), "the entry carries need (a) cut");

    let told = desk.step(Step::Tell(ab.clone(), ENTRY_AB.to_string()));
    let at_request = desk.step(Step::Land(Some(ENTRY_AB.to_string())));
    let held = at_request
        .current()
        .cloned()
        .expect("the whole set stood on the timeline as the request arrived");
    assert_eq!(told.current(), Some(&held), "stored by the tell itself");
    assert_eq!(held.needs(), ab.as_slice(), "the set whole, need (a) uncut");
    assert_eq!(held.entry(), ENTRY_AB, "with the entry's exact text");
    let facts = desk.facts();
    assert_eq!(needs_entry(&facts), Some(ENTRY_AB), "{facts:?}");
    assert_eq!(facts.told_needs(), Some(ab.as_slice()), "{facts:?}");
    assert_eq!(
        desk.woken(),
        0,
        "a tell for an entry not yet standing wakes nothing: its request's write does"
    );

    let ac = set(&[NEED_A, NEED_C]);
    let told = desk.step(Step::Tell(ac.clone(), ENTRY_AB.to_string()));
    assert_eq!(told.current().map(HarnessNeeds::needs), Some(ac.as_slice()));
    assert_eq!(desk.facts().told_needs(), Some(ac.as_slice()));
    assert_eq!(desk.woken(), 1, "the window woken for the moved set");
    desk.step(Step::Tell(ac.clone(), ENTRY_AB.to_string()));
    assert_eq!(desk.woken(), 0, "the same set told again wakes nothing");

    let told = desk.step(Step::Tell(Vec::new(), String::new()));
    assert_eq!(told.current(), None, "the take-down holds no set told last");
    assert_eq!(
        desk.facts().told_needs(),
        Some(ac.as_slice()),
        "the entry keeps its words until the `unset` lands"
    );
    assert_eq!(desk.step(Step::Land(None)).current(), None);
    let facts = desk.facts();
    assert_eq!((needs_entry(&facts), facts.told_needs()), (None, None));
    assert_eq!(desk.woken(), 0, "the take-down's `unset` wakes the window");
}

#[test]
fn the_entry_that_still_stands_keeps_its_words_until_it_goes() {
    let desk = Desk::new();
    let ab = set(&[NEED_A, NEED_B]);
    let b = set(&[NEED_B]);
    let c = set(&[NEED_C]);
    let read = |why: &str| {
        let facts = desk.facts();
        let entry = needs_entry(&facts).map(str::to_string);
        (
            entry,
            facts.told_needs().map(<[String]>::to_vec),
            why.to_string(),
        )
    };
    let expect = |entry: &str, needs: &[String], why: &str| {
        (
            Some(entry.to_string()),
            Some(needs.to_vec()),
            why.to_string(),
        )
    };

    desk.step(Step::Tell(ab.clone(), ENTRY_AB.to_string()));
    desk.step(Step::Land(Some(ENTRY_AB.to_string())));
    desk.step(Step::Tell(b.clone(), ENTRY_B.to_string()));
    let why = "the entry that still stands keeps the words it was told with";
    assert_eq!(read(why), expect(ENTRY_AB, &ab, why));
    desk.step(Step::Land(Some(ENTRY_B.to_string())));
    let why = "the landed entry reads its own set";
    assert_eq!(read(why), expect(ENTRY_B, &b, why));

    desk.step(Step::Tell(ab.clone(), ENTRY_AB.to_string()));
    let why = "a lost request leaves the standing entry its words";
    assert_eq!(read(why), expect(ENTRY_B, &b, why));
    desk.step(Step::Tell(b.clone(), ENTRY_B.to_string()));
    let why = "told again what stands";
    assert_eq!(read(why), expect(ENTRY_B, &b, why));

    desk.step(Step::Tell(c.clone(), ENTRY_C.to_string()));
    let why = "the refused request leaves the standing entry its words";
    assert_eq!(read(why), expect(ENTRY_B, &b, why));
    desk.step(Step::Tell(c.clone(), ENTRY_C_CUT.to_string()));
    let why = "a cap's retry keeps the standing entry's words";
    assert_eq!(read(why), expect(ENTRY_B, &b, why));
    desk.step(Step::Land(Some(ENTRY_C_CUT.to_string())));
    let why = "the cut entry landed reads its set";
    assert_eq!(read(why), expect(ENTRY_C_CUT, &c, why));

    desk.step(Step::Tell(Vec::new(), String::new()));
    let why = "the entry that still stands at the take-down keeps its words";
    assert_eq!(read(why), expect(ENTRY_C_CUT, &c, why));
    desk.step(Step::Land(None));
    assert_eq!(read("gone"), (None, None, "gone".to_string()));
    assert_eq!(desk.woken(), 0, "no tell here moved a reading in place");
}

#[test]
fn a_told_set_is_read_only_while_its_entry_stands() {
    let ab = set(&[NEED_A, NEED_B]);
    let told = HarnessNeeds::told(&ab, &format!("  {ENTRY_AB} ")).expect("a set told");
    assert_eq!(told.entry(), ENTRY_AB, "sanitized as the server stores it");
    let mut held = ToldNeeds::default();
    held.tell(Some(Arc::new(told.clone())), None);
    let facts = |entries: &[(&str, &str)]| crate::needs::NeedFacts {
        attention: entries
            .iter()
            .map(|(owner, text)| crate::needs::AttentionFact {
                owner: (*owner).to_string(),
                text: (*text).to_string(),
                kind: Some(AttentionKind::Ask),
            })
            .collect(),
        told: held.clone(),
        ..crate::needs::NeedFacts::default()
    };
    let other = crate::input_stall::SERVER_ATTENTION_OWNER;
    assert_eq!(
        facts(&[(other, "frozen"), (NEEDS_ATTENTION_OWNER, ENTRY_AB)]).told_needs(),
        Some(ab.as_slice()),
        "beside the entry it was told with"
    );
    assert_eq!(
        facts(&[(NEEDS_ATTENTION_OWNER, ENTRY_B)]).told_needs(),
        None,
        "the entry an earlier set left standing: the request never landed"
    );
    assert_eq!(facts(&[]).told_needs(), None, "no entry stands");
    assert_eq!(
        facts(&[("supervisor", ENTRY_AB)]).told_needs(),
        None,
        "another owner's entry of the same text"
    );
    let hidden = ENTRY_AB.replacen(' ', " \u{200b}", 1);
    assert!(crate::session_timeline::metadata_has_forbidden_formatting(
        &hidden
    ));
    assert_eq!(told.words_for(Some(&hidden)), Some(ab.as_slice()));
    assert_eq!(
        told.words_for(Some(&format!("{ENTRY_AB} "))),
        Some(ab.as_slice())
    );
    assert_eq!(HarnessNeeds::told(&[], ""), None, "the take-down");
    assert_eq!(HarnessNeeds::told(&[], ENTRY_AB), None, "no set");
    assert_eq!(HarnessNeeds::told(&ab, " "), None, "no entry");
    assert_eq!(crate::needs::NeedFacts::default().told_needs(), None);
}

#[test]
fn the_store_follows_the_tells_and_wakes_only_where_a_reading_moved() {
    let app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    let held = || ctx.timeline.lock().unwrap().harness_needs();
    let told_last = || held().current().map(|told| told.needs().to_vec());
    let tell = |needs: &[&str], entry: &str| store_needs(&app.store, &sid, &set(needs), entry);
    let put = |text: Option<&str>| {
        ctx.meta
            .lock()
            .unwrap()
            .set_attention_kind(
                NEEDS_ATTENTION_OWNER,
                text.map(str::to_string),
                text.map(|_| AttentionKind::Ask),
            )
            .expect("a slot");
    };
    let reads = |entry: &str| held().words_for(Some(entry)).map(<[String]>::to_vec);
    assert_eq!(held(), ToldNeeds::default(), "nothing told");
    assert!(
        !tell(&[NEED_A, NEED_B], ENTRY_AB),
        "a set told for an entry not standing yet wakes nothing"
    );
    assert_eq!(told_last(), Some(set(&[NEED_A, NEED_B])), "a set stored");
    put(Some(ENTRY_AB));
    assert!(!tell(&[NEED_A, NEED_B], ENTRY_AB), "the same set: no move");
    assert!(
        tell(&[NEED_A, NEED_C], ENTRY_AB),
        "the set moved under the entry that stands: woken"
    );
    assert_eq!(reads(ENTRY_AB), Some(set(&[NEED_A, NEED_C])));
    assert!(
        !tell(&[NEED_B], ENTRY_B),
        "a set told for another entry: the outgoing one is read meanwhile"
    );
    assert_eq!(reads(ENTRY_AB), Some(set(&[NEED_A, NEED_C])));
    assert_eq!(reads(ENTRY_B), Some(set(&[NEED_B])));
    put(Some(ENTRY_B));
    assert!(!tell(&[NEED_C], ENTRY_C), "told for another entry, refused");
    assert!(
        !tell(&[NEED_C], ENTRY_C_CUT),
        "the same set under a cut text"
    );
    assert_eq!(held().current().map(HarnessNeeds::entry), Some(ENTRY_C_CUT));
    assert_eq!(reads(ENTRY_B), Some(set(&[NEED_B])), "(b) read meanwhile");
    put(Some(ENTRY_C_CUT));
    assert!(
        !tell(&[], ""),
        "the take-down wakes nothing: its `unset` does"
    );
    assert_eq!(told_last(), None, "the take-down holds no set told last");
    assert_eq!(reads(ENTRY_C_CUT), Some(set(&[NEED_C])), "until it lands");
    put(None);
    assert!(!tell(&[], ""), "nothing left to clear");
    assert_eq!(held().words_for(None), None);
    put(Some(ENTRY_OTHER));
    assert!(
        !tell(&[NEED_A], ENTRY_AB),
        "told for an entry someone else moved"
    );
    assert_eq!(reads(ENTRY_OTHER), None, "stale: nothing describes it");
    assert!(
        !store_needs(&app.store, "s-gone", &set(&[NEED_B]), ENTRY_B),
        "a session the store does not hold stores nothing"
    );
}

#[test]
fn a_needs_fact_publishes_the_need_and_the_open_page_once() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    app.refresh_presence_session(0);
    assert_eq!(ctx.timeline.lock().unwrap().need().needs_word(), "-");
    assert!(!store_needs(
        &app.store,
        &sid,
        &set(&[NEED_A, NEED_B]),
        ENTRY_AB
    ));
    ctx.meta
        .lock()
        .unwrap()
        .set_attention_kind(
            NEEDS_ATTENTION_OWNER,
            Some(ENTRY_AB.to_string()),
            Some(AttentionKind::Ask),
        )
        .expect("a slot");
    let face = ctx.timeline.lock().unwrap().task_face();
    let revision = app.harness_screen.revision;
    app.apply_harness_fact(&sid, crate::HarnessFact::Needs);
    assert_eq!(
        ctx.timeline.lock().unwrap().need().needs_word(),
        "ask",
        "the need published"
    );
    assert_eq!(
        app.harness_screen.revision,
        revision + 1,
        "the open page published once"
    );
    assert_eq!(
        ctx.timeline.lock().unwrap().task_face(),
        face,
        "no face moved"
    );
    app.apply_harness_fact("s-not-this-windows", crate::HarnessFact::Needs);
    assert_eq!(app.harness_screen.revision, revision + 1);
}

#[test]
fn a_harness_fact_stays_copy_and_mains_size() {
    const MAIN_FACT_BYTES: usize = 24;
    fn copy<T: Copy>() {}
    copy::<crate::HarnessFact>();
    assert!(
        std::mem::size_of::<crate::HarnessFact>() <= MAIN_FACT_BYTES,
        "HarnessFact grew to {} bytes",
        std::mem::size_of::<crate::HarnessFact>()
    );
}

#[test]
fn status_prints_the_told_set_for_the_entry_that_stands() {
    let app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    let line = |app: &App| app.session_status_record(0).expect("status");
    let field = |line: &str, key: &str| {
        let prefix = format!("{key}=");
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(prefix.as_str()))
            .map(str::to_owned)
    };
    let told = |app: &App| field(&line(app), "needs_told");
    let put = |text: Option<&str>| {
        ctx.meta
            .lock()
            .unwrap()
            .set_attention_kind(
                NEEDS_ATTENTION_OWNER,
                text.map(str::to_string),
                text.map(|_| AttentionKind::Ask),
            )
            .expect("a slot");
    };
    assert_eq!(told(&app).as_deref(), Some("-"), "none told");
    store_needs(&app.store, &sid, &set(&[NEED_A, NEED_B]), ENTRY_AB);
    assert_eq!(told(&app).as_deref(), Some("stale"), "told, not landed");
    put(Some(ENTRY_AB));
    assert_eq!(told(&app).as_deref(), Some("2"), "told, and landed");
    store_needs(&app.store, &sid, &set(&[NEED_B]), ENTRY_B);
    assert_eq!(
        told(&app).as_deref(),
        Some("2"),
        "the outgoing set, its entry up"
    );
    put(Some(ENTRY_B));
    assert_eq!(told(&app).as_deref(), Some("1"), "the new set, landed");
    put(Some(ENTRY_OTHER));
    assert_eq!(told(&app).as_deref(), Some("stale"), "another entry stands");
    store_needs(&app.store, &sid, &[], "");
    put(None);
    assert_eq!(told(&app).as_deref(), Some("-"), "taken down");
    let l = line(&app);
    for key in ["needs", "needs_n", "hold_by"] {
        assert!(field(&l, key).is_some(), "{key}= reads by key: {l}");
    }
}
