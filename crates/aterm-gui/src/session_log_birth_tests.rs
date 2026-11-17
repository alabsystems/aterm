// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A session's birth claims (CX-04): a spawn's `why=`, its task's first line and its `package=`
//! as the caller's claims at the creator's rank (or the control rank, or aterm's derived one over
//! a fleet drive), the creator's standing reason copied where no `why=` was given, a joined
//! request's given why as aterm's derived reason, a request's guessed why written nowhere, and
//! the two closed words `title_src` and `matters_src` read from the record and never from its
//! text.

use std::sync::Arc;

use aterm_slog::{Actor, Conf, Key, Kind, MattersSrc, Node, Rank, ShortId, Src, TitleSrc};

use super::{Birth, BirthAuthor, BirthClaims};
use crate::control::control_media::{AgentSpec, SpawnForm, parse_spawn_args};
use crate::session_log::mark::{DescOn, DescWrite};
use crate::session_log::{ObservedClaim, sid_ident};
use crate::session_store::ExitActor;
use crate::tab_marks::ActScope;
use crate::{App, SessionCtx, WindowId, stub_session};

const WID: WindowId = WindowId(0);

/// A worker's task as its spawn's frame carries it: a blank first line, the line, then more.
const TASK: &[u8] = b"\n  Fix the parser's stale lock\nthen run verify\n";

/// [`TASK`]'s first line that draws something.
const TASK_LINE: &str = "Fix the parser's stale lock";

/// The reason the spawns of these tests give.
const WHY: &str = "ship the card";

/// The creator's standing reason, where a test gives it one.
const STANDING: &str = "keep the release green";

/// The asking tab's goal line, a local request's first fallback why.
const GOAL: &str = "keep the card honest";

/// The asking tab's asked line, a local request's fallback why where it has no goal.
const ASKED: &str = "wire the survey";

/// A fleet drive's node.
fn drive_node() -> String {
    "87be5228".repeat(4)
}

fn ctx(app: &App, id: u64) -> Arc<SessionCtx> {
    Arc::clone(&app.pool.get(id).expect("its session").ctx)
}

fn sid_of(ctx: &SessionCtx) -> String {
    ctx.self_id.as_str().to_string()
}

/// The id a session's sid ranks by, asserted as a PRECONDITION.
fn ident(sid: &str) -> ShortId {
    sid_ident(sid).unwrap_or_else(|| panic!("PRECONDITION: {sid} is a minted sid"))
}

/// A tab landed parked under the window's Workers tab, as a spawn's land installs it.
fn parked(app: &mut App) -> (u64, Arc<SessionCtx>) {
    let id = app.next_session_id;
    app.next_session_id += 1;
    let parent = app.new_tab_parent(WID);
    app.install_tab_session_parked(WID, stub_session(id), parent, false);
    app.sync_window(WID);
    (id, ctx(app, id))
}

/// A person's own new tab in the headless app's window.
fn person_tab(app: &mut App) -> Arc<SessionCtx> {
    let id = app.next_session_id;
    app.push_stub_tab(WID, stub_session(id));
    app.sync_window(WID);
    ctx(app, id)
}

/// THE SESSION A SPAWN OF `line` BY `by` OPENS (over a fleet drive where `fleet`): the line's
/// plain form, an agent's or a program's order of [`TASK`] as `worker_order` makes it, and the
/// open under the act the spawn's Wake enters.
fn born(app: &mut App, line: &str, by: ExitActor, fleet: bool) -> (u64, Arc<SessionCtx>) {
    born_joining(app, line, by, fleet, None)
}

/// [`born`] for a spawn that joined a request whose given why is `joined`
/// ([`Birth::joining`]).
fn born_joining(
    app: &mut App,
    line: &str,
    by: ExitActor,
    fleet: bool,
    joined: Option<&str>,
) -> (u64, Arc<SessionCtx>) {
    let Ok(SpawnForm::Plain {
        agent,
        why,
        package,
        ..
    }) = parse_spawn_args(line)
    else {
        panic!("{line}: a plain spawn");
    };
    let order = match agent {
        Some(AgentSpec::Worker {
            agent, model, from, ..
        }) => Some(
            crate::task_launch::TaskOrder::of_frame(Some(TASK.to_vec()), agent, model, None, from)
                .expect("a worker's order"),
        ),
        Some(AgentSpec::Program { run, from }) => Some(
            crate::task_launch::TaskOrder::of_program_frame(Some(TASK.to_vec()), run, None, from)
                .expect("a program worker's order"),
        ),
        None => None,
    };
    let requester = fleet.then(aterm_types::control_verbs::Requester::default);
    let _act = ActScope::asking(
        by,
        "task",
        Birth::of(order.as_ref(), why, package).joining(joined),
        requester,
    );
    parked(app)
}

/// THE SESSION A SPAWN OF `line` BY `by` OPENS WITH ITS REQUEST, the two from the one line as
/// `cmd_spawn` composes them: the request the asker `(fleet, node, host, by)` asks for, its tab's
/// goal and asked lines `lines`, prepared from the line's `req=` and `why=` with [`TASK`] as its
/// brief and begun (opened, or joined); the birth of the line ([`born`]) joining that request's
/// given why ([`crate::requests_host::Begun::joined_why`]); and the request settled by the tab.
/// The session, and its request's id. Needs a request folder ([`RequestsHere`]).
fn spawned(
    app: &mut App,
    asker: (bool, &str, &str, &str),
    lines: (Option<&str>, Option<&str>),
    line: &str,
    by: ExitActor,
) -> (Arc<SessionCtx>, String) {
    use crate::requests_host::{Begun, Made, asker_for_test, begin, hand, prepare};
    let Ok(SpawnForm::Plain { why, req, .. }) = parse_spawn_args(line) else {
        panic!("{line}: a plain spawn");
    };
    let fleet = asker.0;
    let prepared = prepare(
        Some(&asker_for_test(asker, lines)),
        req.as_deref(),
        Made::Agent("claude"),
        why.as_deref(),
        Some(TASK),
    )
    .expect("prepared");
    let begun = begin(prepared).expect("begun");
    let joined = begun
        .as_ref()
        .and_then(Begun::joined_why)
        .map(str::to_owned);
    let (pending, receipt) = hand(begun, None);
    let (_, session) = born_joining(app, line, by, fleet, joined.as_deref());
    pending
        .expect("a request: the folder is set")
        .settle(Some(&sid_of(&session)));
    let id = receipt
        .expect("a receipt")
        .named()
        .expect("the tab recorded");
    (session, id)
}

/// What a reader is shown of `slot`, with its record's author, source, confidence and name.
#[derive(Debug, PartialEq, Eq)]
struct Stands {
    value: String,
    scope: Node,
    rank: Rank,
    by: Actor,
    src: Src,
    conf: Conf,
    as_name: Option<String>,
}

fn stands(ctx: &SessionCtx, slot: &str) -> Option<Stands> {
    let g = ctx.timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    aterm_slog::shown(g.slog().log().state(), slot, now).map(|s| Stands {
        value: s.claim.value.clone(),
        scope: s.scope,
        rank: s.rank,
        by: s.claim.by,
        src: s.claim.src,
        conf: s.claim.conf,
        as_name: s.claim.as_name.clone(),
    })
}

/// The `title_src` and `matters_src` of `ctx`'s card.
fn srcs(ctx: &SessionCtx) -> (Option<TitleSrc>, MattersSrc) {
    let g = ctx.timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    let st = g.slog().log().state();
    (
        aterm_slog::title_src(st, now),
        aterm_slog::matters_src(st, now),
    )
}

/// One `desc` record of a slot: its seq, author, source, confidence, `as=` and text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DescRec {
    seq: u64,
    by: Actor,
    src: Src,
    conf: Conf,
    as_name: Option<String>,
    value: Option<String>,
}

/// Every `desc` record of `slot` in `ctx`'s log, oldest first.
fn desc_recs(ctx: &SessionCtx, slot: &str) -> Vec<DescRec> {
    let g = ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .filter(|r| r.k == Kind::Desc && r.f.get_str(Key::Field) == Some(slot))
        .map(|r| DescRec {
            seq: r.seq,
            by: r.by,
            src: r.src,
            conf: r.conf,
            as_name: r.f.get_str(Key::As).map(str::to_owned),
            value: r.f.get_str(Key::Value).map(str::to_owned),
        })
        .collect()
}

/// The seq of `ctx`'s tab open.
fn open_seq(ctx: &SessionCtx) -> u64 {
    let g = ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .find(|r| r.k == Kind::Tab)
        .map(|r| r.seq)
        .expect("PRECONDITION: the tab's open")
}

/// The value of `key=` on `ctx`'s tab open, as `log kinds=tab` lists it.
fn open_field(ctx: &SessionCtx, key: &str) -> Option<String> {
    let reply = crate::control::cmd_log(ctx, "kinds=tab");
    assert!(reply.starts_with("OK "), "{reply}");
    let open = reply.lines().nth(1).expect("the tab's open").to_owned();
    open.split(' ')
        .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_owned)
}

/// `by`'s claim of `ctx`'s own `slot`, or its withdrawal (`None`), through the `mark` verb's
/// writer, on `on`.
fn claim_on(ctx: &SessionCtx, on: DescOn, slot: &str, value: Option<&str>, by: Actor) {
    let mut g = ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    g.slog_mut()
        .claim_desc(
            &DescWrite {
                on,
                ..DescWrite::of_session(slot, value)
            },
            by,
            at,
        )
        .expect("the claim is written");
}

/// `by`'s claim of the session's own `slot` ([`claim_on`]).
fn claim(ctx: &SessionCtx, slot: &str, value: Option<&str>, by: Actor) {
    claim_on(ctx, DescOn::Session, slot, value, by);
}

/// Claude Code in front of `ctx`'s shell, its run open: the run.
fn agent_run(ctx: &SessionCtx) -> Node {
    let mut g = ctx.timeline.lock().unwrap();
    g.note_root_group(100);
    assert!(g.note_foreground_group(300));
    assert!(g.set_program(300, Some("claude".into())));
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
    g.slog()
        .log()
        .state()
        .desc
        .open_agent()
        .expect("PRECONDITION: the agent's run is open")
}

/// The program's in-band title, as a drift flush reads it: Claude Code's own.
fn inband_title(ctx: &SessionCtx) {
    let mut g = ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    let read = crate::session_log::InbandRead {
        shown: "✳ Claude Code",
        untitled: false,
        stands: crate::session_log::observed::TitleStands::Own,
        program: None,
        program_seq: 0,
    };
    g.slog_mut().observe_inband_title(&read, at);
    let later = aterm_slog::Stamp::new(at.wall_ms + 5_000, at.mono_ms + 5_000);
    g.slog_mut().inband_tick(later);
}

/// The claim that stands for `slot` on `scope` alone, as its rank.
fn rank_in(ctx: &SessionCtx, scope: Node, slot: &str) -> Option<Rank> {
    let g = ctx.timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    g.slog()
        .log()
        .state()
        .desc
        .shown_in(scope, slot, now)
        .map(|s| s.rank)
}

/// A SPAWN'S `why=` IS THE NEW SESSION'S REASON, AT ITS CREATOR'S RANK (CX-04's exit test): a
/// worker that session S0 spawns naming itself in `from=`, with `why="ship the card"`, holds
/// `matters` on the session as S0's own claim (`by=<S0> src=mark conf=exact`, no `as=`), ranked
/// creator, written after its tab's open, which keeps the line whole as `why_text=`; its
/// `matters_src` reads `creator`.
///
/// CONTROLS: (a) the same from an `aterm ctl` in no tab is ctl's claim, ranked ctl; (b) over a
/// fleet drive it is aterm's derived claim (`as=why`); (c) S0's later claim replaces its birth
/// claim, and its withdrawal leaves no reason; (d) a third session's claim is listed and does not
/// stand; (e) a session that is not the one `from=` names writes aterm's derived claim, and none
/// of its own; (f) through the `mark` verb, S0's `mark - desc` replaces its birth claim on the
/// session while no agent runs there, and once one runs it lands on the agent's run, where the
/// claim on the session still stands over it and its withdrawal removes nothing (what `help
/// described birth` says, ATERM-KI-1115-270 (a)).
#[test]
fn a_spawn_why_is_the_reason_at_its_creators_rank() {
    let mut app = App::headless_for_test();
    let s0 = sid_of(&ctx(&app, 0));
    let s0_id = ident(&s0);
    let line = format!(r#"agent=claude from={s0} why="{WHY}" len=1"#);
    let (_, child) = born(&mut app, &line, ExitActor::Sid(s0.clone()), false);
    assert_eq!(
        stands(&child, "matters"),
        Some(Stands {
            value: WHY.to_owned(),
            scope: Node::SESSION,
            rank: Rank::Creator,
            by: Actor::Sid(s0_id),
            src: Src::Mark,
            conf: Conf::Exact,
            as_name: None,
        })
    );
    let records = desc_recs(&child, "matters");
    assert_eq!(records.len(), 1, "{records:?}");
    assert!(records[0].seq > open_seq(&child), "after the tab's open");
    assert_eq!(
        open_field(&child, "why_text").as_deref(),
        Some("ship%20the%20card")
    );
    assert_eq!(srcs(&child).1, MattersSrc::Creator);

    // (a) An `aterm ctl` in no tab.
    let (_, by_ctl) = born(
        &mut app,
        &format!(r#"agent=claude why="{WHY}" len=1"#),
        ExitActor::Ctl,
        false,
    );
    let shown = stands(&by_ctl, "matters").expect("CONTROL (a): a reason stands");
    assert_eq!(
        (shown.rank, shown.by, shown.conf),
        (Rank::Ctl, Actor::Ctl, Conf::Exact)
    );
    assert_eq!(srcs(&by_ctl).1, MattersSrc::Creator);

    // (b) Over a fleet drive.
    let (_, driven) = born(
        &mut app,
        &line,
        ExitActor::fleet_drive(&drive_node(), Some(&s0)),
        true,
    );
    let shown = stands(&driven, "matters").expect("CONTROL (b): a reason stands");
    assert_eq!(
        (shown.by, shown.src, shown.rank, shown.as_name.as_deref()),
        (Actor::Aterm, Src::Aterm, Rank::Derived, Some("why"))
    );
    assert_eq!(srcs(&driven).1, MattersSrc::Creator);

    // (d) A third session's claim is listed, never standing.
    let s1 = crate::tab_ids::mint();
    let s1_id = ident(s1.as_str());
    claim(
        &child,
        "matters",
        Some("not mine to say"),
        Actor::Sid(s1_id),
    );
    assert_eq!(
        stands(&child, "matters").map(|s| s.value),
        Some(WHY.to_owned()),
        "CONTROL (d)"
    );
    {
        let g = child.timeline.lock().unwrap();
        let now = g.slog().now().wall_ms;
        let listed = g
            .slog()
            .log()
            .state()
            .desc
            .listing(now)
            .into_iter()
            .find(|row| row.slot == "matters" && row.claim.by == Actor::Sid(s1_id))
            .map(|row| (row.rank, row.standing));
        assert_eq!(listed, Some((Rank::Listed, false)), "CONTROL (d)");
    }

    // (c) S0's later claim replaces its birth claim; its withdrawal leaves no reason.
    claim(
        &child,
        "matters",
        Some("ship the card today"),
        Actor::Sid(s0_id),
    );
    let mine: Vec<Conf> = {
        let g = child.timeline.lock().unwrap();
        let now = g.slog().now().wall_ms;
        g.slog()
            .log()
            .state()
            .desc
            .listing(now)
            .into_iter()
            .filter(|row| row.slot == "matters" && row.claim.by == Actor::Sid(s0_id))
            .map(|row| row.claim.conf)
            .collect()
    };
    assert_eq!(mine, [Conf::Claim], "CONTROL (c): one claim by S0");
    assert_eq!(
        stands(&child, "matters").map(|s| s.value),
        Some("ship the card today".to_owned())
    );
    claim(&child, "matters", None, Actor::Sid(s0_id));
    assert_eq!(srcs(&child).1, MattersSrc::Unset, "CONTROL (c): withdrawn");

    // (e) A caller that is not the session `from=` names.
    let (_, other) = born(
        &mut app,
        &line,
        ExitActor::Sid(s1.as_str().to_owned()),
        false,
    );
    let records = desc_recs(&other, "matters");
    assert_eq!(
        records
            .iter()
            .map(|r| (r.by, r.as_name.as_deref()))
            .collect::<Vec<_>>(),
        [(Actor::Aterm, Some("why"))],
        "CONTROL (e): {records:?}"
    );
    assert!(
        desc_recs(&other, "title")
            .iter()
            .all(|r| r.by != Actor::Sid(s1_id)),
        "CONTROL (e): no claim by S1"
    );

    // (f) Through the verb, before and while an agent runs.
    let (_, marked) = born(&mut app, &line, ExitActor::Sid(s0.clone()), false);
    let mark = |rest: &str| crate::session_log::cmd_mark(&marked, Actor::Sid(s0_id), true, rest).0;
    let reply = mark("- desc field=matters value=land%20the%20release");
    assert!(reply.contains(" node=- "), "CONTROL (f): {reply}");
    let shown = stands(&marked, "matters").expect("CONTROL (f): a reason stands");
    assert_eq!(
        (shown.value.as_str(), shown.scope, shown.conf),
        ("land the release", Node::SESSION, Conf::Claim),
        "CONTROL (f): the birth claim is replaced"
    );
    let run = agent_run(&marked);
    let reply = mark("- desc field=matters value=ship%20it%20now");
    assert!(
        reply.contains(&format!(" node={run} ")),
        "CONTROL (f): {reply}"
    );
    assert_eq!(
        stands(&marked, "matters").map(|s| (s.value, s.scope)),
        Some(("land the release".to_owned(), Node::SESSION)),
        "CONTROL (f): the session's claim stands over the run's"
    );
    let reply = mark("- desc field=matters");
    assert!(
        reply.contains(&format!(" node={run} ")),
        "CONTROL (f): {reply}"
    );
    assert_eq!(
        srcs(&marked).1,
        MattersSrc::Creator,
        "CONTROL (f): a withdrawal on the run removes nothing on the session"
    );
}

/// A SPAWN WITH NO `why=` TAKES ITS CREATOR'S REASON (CX-04's exit test): S0 holds a person's
/// `matters`, and the worker it spawns naming itself in `from=` with no `why=` holds that line,
/// copied at the open as aterm's derived claim (`as=creator conf=exact`); `matters_src` reads
/// `creator`.
///
/// CONTROLS: (a) S0 changing its reason later moves no copy; (b) a creator with no standing
/// reason gives no `matters` record; (c) with `why=` given, nothing is copied; (d) the agent's
/// own reason on its run (rank 2) stands over the copy (rank 7), which R1 does not keep; (e) a
/// plain spawn from S0's tab (no `from=`) copies nothing; (f) nor does the same spawn over a fleet
/// drive.
#[test]
fn a_spawn_with_no_why_takes_its_creators_reason() {
    let mut app = App::headless_for_test();
    let creator = ctx(&app, 0);
    let s0 = sid_of(&creator);
    ident(&s0);
    claim(&creator, "matters", Some(WHY), Actor::Human);
    let line = format!("agent=claude from={s0} len=1");
    let (_, child) = born(&mut app, &line, ExitActor::Sid(s0.clone()), false);
    let shown = stands(&child, "matters").expect("a reason stands");
    assert_eq!(
        (
            shown.value.as_str(),
            shown.rank,
            shown.as_name.as_deref(),
            shown.conf
        ),
        (WHY, Rank::Derived, Some("creator"), Conf::Exact)
    );
    assert_eq!(srcs(&child).1, MattersSrc::Creator);

    // (a) The creator's later reason moves no copy.
    claim(&creator, "matters", Some("land the release"), Actor::Human);
    assert_eq!(
        stands(&child, "matters").map(|s| s.value),
        Some(WHY.to_owned()),
        "CONTROL (a)"
    );

    // (b) A creator with no standing reason.
    let quiet = person_tab(&mut app);
    let quiet_sid = sid_of(&quiet);
    let (_, unreasoned) = born(
        &mut app,
        &format!("agent=claude from={quiet_sid} len=1"),
        ExitActor::Sid(quiet_sid.clone()),
        false,
    );
    assert_eq!(desc_recs(&unreasoned, "matters"), [], "CONTROL (b)");

    // (c) A `why=` given.
    let (_, given) = born(
        &mut app,
        &format!(r#"agent=claude from={s0} why="mind the cache" len=1"#),
        ExitActor::Sid(s0.clone()),
        false,
    );
    let records = desc_recs(&given, "matters");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].as_name, None, "CONTROL (c): no copy");
    assert_eq!(records[0].value.as_deref(), Some("mind the cache"));

    // (d) The agent's own reason on its run.
    let run = agent_run(&child);
    let own = ident(&sid_of(&child));
    claim_on(
        &child,
        DescOn::Agent(run),
        "matters",
        Some("prove the fold"),
        Actor::Sid(own),
    );
    let shown = stands(&child, "matters").expect("a reason stands");
    assert_eq!(
        (shown.value.as_str(), shown.scope, shown.rank),
        ("prove the fold", run, Rank::Program),
        "CONTROL (d)"
    );
    assert_eq!(srcs(&child).1, MattersSrc::Program);

    // (e) A plain spawn from S0's tab: no `from=`.
    let (_, plain) = born(&mut app, "", ExitActor::Sid(s0.clone()), false);
    assert_eq!(desc_recs(&plain, "matters"), [], "CONTROL (e)");

    // (f) Over a fleet drive.
    let (_, driven) = born(
        &mut app,
        &line,
        ExitActor::fleet_drive(&drive_node(), Some(&s0)),
        true,
    );
    assert_eq!(desc_recs(&driven, "matters"), [], "CONTROL (f)");
}

/// A TASK'S LINE TITLES THE SESSION AND OUTRANKS THE PROGRAM'S OSC TITLE (CX-04's exit test): the
/// worker S0 spawns holds its task's first line as `title` on the session, S0's claim at the
/// creator's rank (`as=task conf=exact`, `title_src=task`); once its agent runs and prints its own
/// title, which stands on the run at the in-band rank, the task's line still stands (R1). From
/// an `aterm ctl` in no tab, the line at the control rank stands over the in-band title too.
///
/// CONTROLS: (a) S0 withdrawing its title leaves the in-band title standing (`stated`); (b) a
/// person's rename stands over both (`stated`); (c) over a fleet drive no birth title is written,
/// and once the task is set as a worker's start sets it, aterm's default writer's task line
/// stands (`title_src=task`).
#[test]
fn a_task_line_titles_the_session_and_outranks_the_programs_osc_title() {
    let mut app = App::headless_for_test();
    let s0 = sid_of(&ctx(&app, 0));
    let s0_id = ident(&s0);
    let line = format!("agent=claude from={s0} len=1");
    let (_, child) = born(&mut app, &line, ExitActor::Sid(s0.clone()), false);
    let title = stands(&child, "title").expect("a title stands");
    assert_eq!(
        (
            title.value.as_str(),
            title.scope,
            title.rank,
            title.as_name.as_deref(),
            title.conf
        ),
        (
            TASK_LINE,
            Node::SESSION,
            Rank::Creator,
            Some("task"),
            Conf::Exact
        )
    );
    assert_eq!(srcs(&child).0, Some(TitleSrc::Task));
    let run = agent_run(&child);
    inband_title(&child);
    assert_eq!(
        rank_in(&child, run, "title"),
        Some(Rank::Inband),
        "PRECONDITION: the program's own title stands on its run"
    );
    assert_eq!(
        stands(&child, "title").map(|t| (t.value, t.scope)),
        Some((TASK_LINE.to_owned(), Node::SESSION)),
        "R1: the task's line stands"
    );
    assert_eq!(srcs(&child).0, Some(TitleSrc::Task));

    let (_, by_ctl) = born(&mut app, "agent=claude len=1", ExitActor::Ctl, false);
    let ctl_run = agent_run(&by_ctl);
    inband_title(&by_ctl);
    assert_eq!(rank_in(&by_ctl, ctl_run, "title"), Some(Rank::Inband));
    assert_eq!(
        stands(&by_ctl, "title").map(|t| (t.value, t.scope, t.rank)),
        Some((TASK_LINE.to_owned(), Node::SESSION, Rank::Ctl)),
        "the control rank stands over the in-band one"
    );
    assert_eq!(srcs(&by_ctl).0, Some(TitleSrc::Task));

    // (a) S0 withdraws its title.
    claim(&child, "title", None, Actor::Sid(s0_id));
    let shown = stands(&child, "title").expect("a title stands");
    assert_eq!(
        (shown.scope, shown.rank),
        (run, Rank::Inband),
        "CONTROL (a)"
    );
    assert_eq!(srcs(&child).0, Some(TitleSrc::Stated));

    // (b) A person's rename.
    claim(&by_ctl, "title", Some("Parser fix"), Actor::Human);
    assert_eq!(
        stands(&by_ctl, "title").map(|t| (t.value, t.rank)),
        Some(("Parser fix".to_owned(), Rank::Person)),
        "CONTROL (b)"
    );
    assert_eq!(srcs(&by_ctl).0, Some(TitleSrc::Stated));

    // (c) Over a fleet drive: aterm's default writer titles it from the task.
    let (local, driven) = born(
        &mut app,
        &line,
        ExitActor::fleet_drive(&drive_node(), Some(&s0)),
        true,
    );
    assert_eq!(
        desc_recs(&driven, "title"),
        [],
        "CONTROL (c): no birth title"
    );
    let mut mark = crate::task_record::TaskMark::new(crate::task_record::TaskState::Working);
    mark.from = Some(s0.clone());
    crate::task_record::mark_session_task(&driven, local, &mark).expect("unfenced");
    driven
        .timeline
        .lock()
        .unwrap()
        .set_task_line(crate::task_record::task_line(TASK));
    let _ = crate::describe::pass(&app.store);
    let shown = stands(&driven, "title").expect("a title stands");
    assert_eq!(
        (
            shown.value.as_str(),
            shown.rank,
            shown.as_name.as_deref(),
            shown.conf
        ),
        (TASK_LINE, Rank::Derived, Some("task"), Conf::Heuristic),
        "CONTROL (c)"
    );
    assert_eq!(srcs(&driven).0, Some(TitleSrc::Task));
}

/// This test thread's requests in a folder of its own until dropped.
struct RequestsHere {
    _dir: aterm_tempfile::TempDir,
}

impl RequestsHere {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("a folder");
        crate::requests_host::set_dir_for_test(Some(dir.path().to_path_buf()));
        Self { _dir: dir }
    }
}

impl Drop for RequestsHere {
    fn drop(&mut self) {
        assert!(
            crate::requests_host::flush_within(std::time::Duration::from_secs(60)),
            "the writer flushed"
        );
        crate::requests_host::forget_for_test();
        crate::requests_host::set_dir_for_test(None);
    }
}

/// A GUESSED WHY WRITES NO REASON (CX-04's exit test): a worker an agent's tab S0 spawns with no
/// `why=` makes a request whose why is a fallback, never `given`: the asking tab's goal line, or
/// its asked line where it has no goal. The session that spawn opens, its request and its birth
/// from the one line, holds no `matters` record: its `matters_src` reads `none`. Over a fleet
/// drive, the request's fallback is the brief's first line, and that line is no reason either.
///
/// CONTROLS: (a) with `why=` given, the request's why is `given`, and the birth writes it as the
/// creator's (aterm's derived one over a fleet drive); (b) the goal, the asked line and the
/// brief's line are in no `matters` record.
#[test]
fn a_guessed_why_writes_no_reason() {
    use crate::requests_host::placed;
    use aterm_requests::WhySrc;
    let _here = RequestsHere::new();
    let mut app = App::headless_for_test();
    let creator = ctx(&app, 0);
    let s0 = sid_of(&creator);
    ident(&s0);
    let _ = agent_run(&creator);
    assert_eq!(
        srcs(&creator).1,
        MattersSrc::Unset,
        "PRECONDITION: no reason"
    );
    let node = drive_node();
    let here = (false, "", "this-mac", s0.as_str());
    let fleet = (true, node.as_str(), "mini", s0.as_str());
    let worker = format!("agent=claude from={s0} len=1");
    let mut sessions = Vec::new();
    for (lines, fallback) in [
        ((Some(GOAL), Some(ASKED)), (GOAL, WhySrc::Goal)),
        ((None, Some(ASKED)), (ASKED, WhySrc::Asked)),
    ] {
        let (guessed, _) = spawned(&mut app, here, lines, &worker, ExitActor::Sid(s0.clone()));
        let request = placed(&sid_of(&guessed), None).expect("the request names its tab");
        assert_eq!(
            (&*request.why, request.why_src),
            fallback,
            "PRECONDITION: the tab's line is the fallback"
        );
        assert_eq!(desc_recs(&guessed, "matters"), [], "{fallback:?}");
        assert_eq!(srcs(&guessed).1, MattersSrc::Unset, "{fallback:?}");
        sessions.push(guessed);
    }

    let (briefed, _) = spawned(
        &mut app,
        fleet,
        (None, None),
        &worker,
        ExitActor::fleet_drive(&node, Some(&s0)),
    );
    let request = placed(&sid_of(&briefed), None).expect("the request names its tab");
    assert_eq!(
        (&*request.why, request.why_src),
        (TASK_LINE, WhySrc::Brief),
        "PRECONDITION: the brief's line"
    );
    assert_eq!(desc_recs(&briefed, "matters"), []);
    assert_eq!(srcs(&briefed).1, MattersSrc::Unset);
    sessions.push(briefed);

    // (a) A why given.
    let given_line = format!(r#"agent=claude from={s0} why="{WHY}" len=1"#);
    let (given, _) = spawned(
        &mut app,
        here,
        (Some(GOAL), Some(ASKED)),
        &given_line,
        ExitActor::Sid(s0.clone()),
    );
    assert_eq!(
        placed(&sid_of(&given), None).map(|r| r.why_src),
        Some(WhySrc::Given)
    );
    assert_eq!(
        stands(&given, "matters").map(|s| (s.value, s.rank)),
        Some((WHY.to_owned(), Rank::Creator)),
        "CONTROL (a)"
    );
    let (driven, _) = spawned(
        &mut app,
        fleet,
        (None, None),
        &given_line,
        ExitActor::fleet_drive(&node, Some(&s0)),
    );
    assert_eq!(
        stands(&driven, "matters").map(|s| (s.value, s.rank, s.as_name)),
        Some((WHY.to_owned(), Rank::Derived, Some("why".to_owned()))),
        "CONTROL (a): over a fleet drive"
    );
    sessions.extend([given, driven]);

    // (b) No guessed line is a reason anywhere.
    for session in &sessions {
        assert!(
            desc_recs(session, "matters")
                .iter()
                .all(|r| ![GOAL, ASKED, TASK_LINE]
                    .iter()
                    .any(|guess| r.value.as_deref() == Some(*guess))),
            "CONTROL (b)"
        );
    }
}

/// A JOINED REQUEST'S GIVEN WHY IS THE REASON (CX-04, the review's CX04-BIRTH-1): a worker S0's
/// agent tab spawns into its own open request (`req=<id>`) with no `why=`, where that request's
/// why was given at its open (`why_src=given`), holds that why as its `matters`, aterm's derived
/// claim (`as=why by=aterm src=aterm conf=exact`) whoever the caller is, read
/// `matters_src=creator`. Its tab's open records no `why_text=` (that is the spawn's own line),
/// and S0's standing reason is not copied: one reason is written. Over a fleet drive the same
/// join writes the same derived claim.
///
/// CONTROLS: (a) a join of a request whose why was guessed (the asking tab's goal) writes nothing
/// from it, and copies S0's standing reason as a spawn that joins nothing does (`as=creator`);
/// (b) a join that gives its own `why=` writes that, as S0's claim at the creator's rank, and not
/// the request's.
#[test]
fn a_joined_requests_given_why_is_the_reason() {
    let _here = RequestsHere::new();
    let mut app = App::headless_for_test();
    let creator = ctx(&app, 0);
    let s0 = sid_of(&creator);
    let s0_id = ident(&s0);
    let _ = agent_run(&creator);
    claim(&creator, "matters", Some(STANDING), Actor::Human);
    let node = drive_node();
    let here = (false, "", "this-mac", s0.as_str());
    let fleet = (true, node.as_str(), "mini", s0.as_str());
    let rec = |r: &DescRec| {
        (
            r.by,
            r.src,
            r.conf,
            r.as_name.clone(),
            r.value.clone().unwrap_or_default(),
        )
    };
    let reasons = |session: &SessionCtx| -> Vec<_> {
        desc_recs(session, "matters").iter().map(rec).collect()
    };
    let derived_why = (
        Actor::Aterm,
        Src::Aterm,
        Conf::Exact,
        Some("why".to_owned()),
        WHY.to_owned(),
    );
    let opening = format!(r#"agent=claude from={s0} why="{WHY}" len=1"#);
    let (_, id) = spawned(
        &mut app,
        here,
        (None, None),
        &opening,
        ExitActor::Sid(s0.clone()),
    );
    let (joined, joined_id) = spawned(
        &mut app,
        here,
        (None, None),
        &format!("agent=claude from={s0} req={id} len=1"),
        ExitActor::Sid(s0.clone()),
    );
    assert_eq!(joined_id, id, "PRECONDITION: the spawn joined the request");
    assert_eq!(reasons(&joined), [derived_why.clone()]);
    assert_eq!(srcs(&joined).1, MattersSrc::Creator);
    assert_eq!(open_field(&joined, "why_text"), None, "no why= of its own");

    // Over a fleet drive.
    let driver = || ExitActor::fleet_drive(&node, Some(&s0));
    let (_, fleet_id) = spawned(
        &mut app,
        fleet,
        (None, None),
        &format!(r#"agent=claude why="{WHY}" len=1"#),
        driver(),
    );
    let (driven, _) = spawned(
        &mut app,
        fleet,
        (None, None),
        &format!("agent=claude req={fleet_id} len=1"),
        driver(),
    );
    assert_eq!(reasons(&driven), [derived_why], "over a fleet drive");

    // (a) A request whose why was guessed.
    let (_, guessed_id) = spawned(
        &mut app,
        here,
        (Some(GOAL), None),
        &format!("agent=claude from={s0} len=1"),
        ExitActor::Sid(s0.clone()),
    );
    let (copied, _) = spawned(
        &mut app,
        here,
        (Some(GOAL), None),
        &format!("agent=claude from={s0} req={guessed_id} len=1"),
        ExitActor::Sid(s0.clone()),
    );
    assert_eq!(
        reasons(&copied),
        [(
            Actor::Aterm,
            Src::Aterm,
            Conf::Exact,
            Some("creator".to_owned()),
            STANDING.to_owned()
        )],
        "CONTROL (a): the creator's reason, not the guess"
    );

    // (b) A join with a why= of its own.
    let (own, _) = spawned(
        &mut app,
        here,
        (None, None),
        &format!(r#"agent=claude from={s0} req={id} why="mind the cache" len=1"#),
        ExitActor::Sid(s0.clone()),
    );
    assert_eq!(
        reasons(&own),
        [(
            Actor::Sid(s0_id),
            Src::Mark,
            Conf::Exact,
            None,
            "mind the cache".to_owned()
        )],
        "CONTROL (b): its own why, not the request's"
    );
    assert_eq!(stands(&own, "matters").map(|s| s.rank), Some(Rank::Creator));
}

/// `spawn package=` SETS THE SLOT (CX-04's exit test): the worker S0 spawns holds it as `package`
/// on the session, S0's claim at the creator's rank (`conf=exact`). A malformed value, an empty
/// one, five ids and a second `package=` are each refused by the word that breaks them, and the
/// connected form takes none (`spawn_parses_package_on_every_plain_form` holds the plain forms).
///
/// CONTROLS: (a) no `package=`, no `package` record; (b) over a fleet drive, aterm's derived claim
/// (`as=spawn`); (c) the agent's own `package` on its run (rank 2) stands over the creator's.
#[test]
fn a_spawn_package_sets_the_slot() {
    let mut app = App::headless_for_test();
    let s0 = sid_of(&ctx(&app, 0));
    ident(&s0);
    let line = format!("agent=claude from={s0} package=FL-04,CX-01 len=5");
    let package_of = |line: &str| match parse_spawn_args(line) {
        Ok(SpawnForm::Plain { package, .. }) => package,
        other => panic!("{line}: {other:?}"),
    };
    assert_eq!(package_of(&line).as_deref(), Some("FL-04,CX-01"));
    for bad in [
        "package=fl-04",
        "package=FL-04,FL-04",
        "package=FL-1,FL-2,FL-3,FL-4,FL-5",
        "package=",
    ] {
        assert_eq!(
            parse_spawn_args(&format!("agent=claude {bad} len=1")),
            Err(Some(bad.to_owned())),
            "{bad}"
        );
    }
    assert_eq!(
        parse_spawn_args("package=FL-04 package=CX-01"),
        Err(Some("package=CX-01".to_owned())),
        "a second package= names the repeat"
    );
    assert_eq!(
        parse_spawn_args("connected=controlled place=tab of=tab12000000 package=FL-04"),
        Err(None)
    );
    let (_, child) = born(&mut app, &line, ExitActor::Sid(s0.clone()), false);
    let shown = stands(&child, "package").expect("a package stands");
    assert_eq!(
        (shown.value.as_str(), shown.scope, shown.rank, shown.conf),
        ("FL-04,CX-01", Node::SESSION, Rank::Creator, Conf::Exact)
    );

    // (a) No package=.
    let (_, none) = born(
        &mut app,
        &format!("agent=claude from={s0} len=1"),
        ExitActor::Sid(s0.clone()),
        false,
    );
    assert_eq!(desc_recs(&none, "package"), [], "CONTROL (a)");

    // (b) Over a fleet drive.
    let (_, driven) = born(
        &mut app,
        &line,
        ExitActor::fleet_drive(&drive_node(), Some(&s0)),
        true,
    );
    assert_eq!(
        stands(&driven, "package").map(|s| (s.value, s.rank, s.as_name)),
        Some((
            "FL-04,CX-01".to_owned(),
            Rank::Derived,
            Some("spawn".to_owned())
        )),
        "CONTROL (b)"
    );

    // (c) The agent's own package on its run.
    let run = agent_run(&child);
    let own = ident(&sid_of(&child));
    claim_on(
        &child,
        DescOn::Agent(run),
        "package",
        Some("FL-05"),
        Actor::Sid(own),
    );
    assert_eq!(
        stands(&child, "package").map(|s| (s.value, s.scope, s.rank)),
        Some(("FL-05".to_owned(), run, Rank::Program)),
        "CONTROL (c)"
    );
}

/// The same text every card of [`title_src_and_matters_src_are_closed_words_read_from_the_record`]
/// holds.
const SAME: &str = "fix the parser";

/// A card's session: registered, its own sid and its creator's told.
fn card(store: &crate::session_store::Store, local: u64) -> (Arc<SessionCtx>, ShortId, ShortId) {
    let h = crate::session_store::test_handle(local);
    store.write().unwrap().register(h.clone());
    let own = ident(h.sid.as_str());
    let creator_sid = crate::tab_ids::mint();
    let creator = ident(creator_sid.as_str());
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let slog = g.slog_mut();
        slog.set_desc_own(h.sid.as_str());
        slog.set_desc_creator(Some(creator_sid.as_str()));
    }
    (Arc::clone(&h.ctx), own, creator)
}

/// How one card is written: the writer and what it writes.
#[derive(Clone, Copy, Debug)]
enum Write {
    /// `claim_birth`: a stated birth's title and `why=`, by the creator.
    Birth,
    /// `claim_observed`: aterm's default writer's title, naming its rung.
    Default(Option<&'static str>),
    /// `claim_observed`: the program's in-band title.
    Inband,
    /// `claim_desc`: a claim of a slot by an actor, naming itself.
    Mark(&'static str, Actor, Option<&'static str>),
    /// `claim_observed`: aterm's derived reason, naming its source.
    Derived(&'static str),
    /// Nothing.
    Nothing,
}

fn write(ctx: &SessionCtx, w: Write, own: ShortId, creator: ShortId) {
    let mut g = ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    let slog = g.slog_mut();
    let observed = |slot, content, by, src, conf, as_name| ObservedClaim {
        on: Node::SESSION,
        slot,
        text: Some(SAME),
        content,
        by,
        src,
        conf,
        as_name,
    };
    match w {
        Write::Birth => {
            let birth = Birth {
                title: Some(SAME.to_owned()),
                matters: Some(SAME.to_owned()),
                ..Birth::default()
            };
            let wrote = slog.claim_birth(
                &BirthClaims {
                    birth: &birth,
                    author: BirthAuthor::Stated(Actor::Sid(creator)),
                    // `card` told the log its creator.
                    creator: None,
                    copied: None,
                },
                at,
            );
            assert_eq!(wrote, 2, "the title and the reason");
        }
        Write::Default(as_name) => {
            slog.claim_observed(
                &observed(
                    "title",
                    false,
                    Actor::Aterm,
                    Src::Aterm,
                    Conf::Heuristic,
                    as_name,
                ),
                at,
            )
            .expect("written");
        }
        Write::Inband => {
            slog.claim_observed(
                &observed("title", true, Actor::Nobody, Src::Engine, Conf::Claim, None),
                at,
            )
            .expect("written");
        }
        Write::Mark(slot, by, as_name) => {
            let by = match by {
                Actor::Sid(_) if as_name == Some("folder") => Actor::Sid(own),
                by => by,
            };
            slog.claim_desc(
                &DescWrite {
                    as_name,
                    ..DescWrite::of_session(slot, Some(SAME))
                },
                by,
                at,
            )
            .expect("written");
        }
        Write::Derived(as_name) => {
            slog.claim_observed(
                &observed(
                    "matters",
                    false,
                    Actor::Aterm,
                    Src::Aterm,
                    Conf::Exact,
                    Some(as_name),
                ),
                at,
            )
            .expect("written");
        }
        Write::Nothing => {}
    }
}

/// `title_src` AND `matters_src` ARE CLOSED WORDS READ FROM THE RECORD (CX-04's exit test): cards
/// whose every value is the same text, each written through the real writer, read the words the
/// design gives for their record: a birth title and `why=` `(task, creator)`; aterm's default
/// writer's titles by their `as=` (`prompt`, `program`, `folder`, and its constant `shell`, with
/// none, `program`); the in-band title `stated`; a person's, the program's, a writer's and ctl's
/// reasons `person`, `program`, `writer` and `creator`; aterm's derived reasons by their `as=`
/// (`package`, else `creator`); nothing `(-, none)`. Every word is one of its list's, and reads
/// back as itself.
///
/// CONTROLS: (a) a person's title naming `as=prompt` reads `stated`; (b) the program's naming
/// `as=folder` reads `stated`; (c) the creator's later mark naming `as=task` (`conf=claim`) reads
/// `stated`; (d) the same text reads different words: the text never decides.
#[test]
fn title_src_and_matters_src_are_closed_words_read_from_the_record() {
    use {MattersSrc as M, TitleSrc as T};
    let store = crate::session_store::new_store();
    let creator_is = Actor::Sid(ident(crate::tab_ids::mint().as_str()));
    let cases: [(&str, Write, (Option<TitleSrc>, MattersSrc)); 19] = [
        ("birth", Write::Birth, (Some(T::Task), M::Creator)),
        (
            "default prompt",
            Write::Default(Some("prompt")),
            (Some(T::Prompt), M::Unset),
        ),
        (
            "default program",
            Write::Default(Some("program")),
            (Some(T::Program), M::Unset),
        ),
        (
            "default folder",
            Write::Default(Some("folder")),
            (Some(T::Folder), M::Unset),
        ),
        (
            "default shell",
            Write::Default(None),
            (Some(T::Program), M::Unset),
        ),
        ("in-band", Write::Inband, (Some(T::Stated), M::Unset)),
        (
            "person's reason",
            Write::Mark("matters", Actor::Human, None),
            (None, M::Person),
        ),
        (
            "program's reason",
            Write::Mark("matters", Actor::Harness, None),
            (None, M::Program),
        ),
        (
            "writer's reason",
            Write::Mark("matters", Actor::Aterm, Some("notes")),
            (None, M::Writer),
        ),
        (
            "ctl's reason",
            Write::Mark("matters", Actor::Ctl, None),
            (None, M::Creator),
        ),
        (
            "derived package",
            Write::Derived("package"),
            (None, M::Package),
        ),
        ("derived why", Write::Derived("why"), (None, M::Creator)),
        (
            "derived creator",
            Write::Derived("creator"),
            (None, M::Creator),
        ),
        ("nothing", Write::Nothing, (None, M::Unset)),
        (
            "CONTROL (a): a person's title as=prompt",
            Write::Mark("title", Actor::Human, Some("prompt")),
            (Some(T::Stated), M::Unset),
        ),
        (
            "CONTROL (b): the program's title as=folder",
            Write::Mark("title", creator_is, Some("folder")),
            (Some(T::Stated), M::Unset),
        ),
        (
            "CONTROL (c): the creator's later title as=task",
            Write::Mark("title", creator_is, Some("task")),
            (Some(T::Stated), M::Unset),
        ),
        (
            "ctl's later title as=task",
            Write::Mark("title", Actor::Ctl, Some("task")),
            (Some(T::Stated), M::Unset),
        ),
        (
            "the program's title as=task",
            Write::Mark("title", Actor::Harness, Some("task")),
            (Some(T::Stated), M::Unset),
        ),
    ];
    let mut words = std::collections::BTreeSet::new();
    for (i, (name, w, want)) in cases.into_iter().enumerate() {
        let (ctx, own, creator) = card(&store, 100 + i as u64);
        // The creator's own claims are by this card's creator; a program's `as=folder` by itself.
        let w = match w {
            Write::Mark(slot, by, as_name) if by == creator_is && as_name == Some("task") => {
                Write::Mark(slot, Actor::Sid(creator), as_name)
            }
            w => w,
        };
        write(&ctx, w, own, creator);
        let got = srcs(&ctx);
        assert_eq!(got, want, "{name}");
        let (title, matters) = got;
        if let Some(title) = title {
            assert!(T::ALL.contains(&title), "{name}");
            assert_eq!(T::from_word(title.word()), Some(title), "{name}");
            words.insert(title.word());
        }
        assert!(M::ALL.contains(&matters), "{name}");
        assert_eq!(M::from_word(matters.word()), Some(matters), "{name}");
        words.insert(matters.word());
    }
    assert_eq!(
        T::ALL.map(TitleSrc::word),
        ["task", "prompt", "program", "folder", "stated"]
    );
    assert_eq!(
        M::ALL.map(MattersSrc::word),
        ["person", "program", "creator", "package", "writer", "none"]
    );
    assert_eq!(T::from_word("shell"), None);
    assert_eq!(M::from_word("unset"), None);
    assert!(
        words.len() > 2,
        "CONTROL (d): one text, many words: {words:?}"
    );
}

/// A BIRTH IS TAKEN BY THE FIRST OPEN OF ITS ACT: two tabs opened under one spawn's act, and only
/// the first holds the birth's claims. CONTROL: the second's open still names the act.
#[test]
fn a_birth_is_taken_by_the_first_open_of_its_act() {
    let mut app = App::headless_for_test();
    let s0 = sid_of(&ctx(&app, 0));
    let s0_id = ident(&s0);
    let Ok(SpawnForm::Plain {
        agent: Some(AgentSpec::Worker { agent, from, .. }),
        why,
        package,
        ..
    }) = parse_spawn_args(&format!(
        r#"agent=claude from={s0} why="{WHY}" package=FL-04 len=1"#
    ))
    else {
        panic!("a worker's spawn");
    };
    let order =
        crate::task_launch::TaskOrder::of_frame(Some(TASK.to_vec()), agent, None, None, from)
            .expect("an order");
    let (first, second) = {
        let _act = ActScope::asking(
            ExitActor::Sid(s0.clone()),
            "task",
            Birth::of(Some(&order), why, package),
            None,
        );
        let (_, first) = parked(&mut app);
        let (_, second) = parked(&mut app);
        (first, second)
    };
    for slot in ["title", "matters", "package"] {
        let records = desc_recs(&first, slot);
        assert_eq!(
            records.iter().map(|r| r.by).collect::<Vec<_>>(),
            [Actor::Sid(s0_id)],
            "{slot}: {records:?}"
        );
    }
    {
        let g = second.timeline.lock().unwrap();
        let descs: Vec<_> = g
            .slog()
            .log()
            .window()
            .filter(|r| r.k == Kind::Desc)
            .collect();
        assert!(descs.is_empty(), "{descs:?}");
    }
    assert_eq!(
        open_field(&second, "by").as_deref(),
        Some(s0.as_str()),
        "CONTROL: the act still names the second open"
    );
}
