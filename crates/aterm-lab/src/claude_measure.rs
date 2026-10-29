// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `claude-measure [--keep]`: MEASURE THE INSTALLED CLAUDE CODE BUILD for the session log's
//! transcript steps, or refuse with one named class and leave the checkout as it was.
//!
//! Ruling T8: a Claude Code build's transcript steps are exact only once that build is measured,
//! matched whole; until then they are strong (`partial`, fix E1). A build is measured when its
//! transcript's reduction is committed in aterm-agent's step testdata as `<build>.jsonl`, since
//! `MEASURED_BUILDS` is the set of builds those reductions' own lines name (aterm-agent's
//! `build.rs`, which reads the files a checkout holds, so the verb reads the step testdata only as
//! HEAD holds it). The verb does for the managed Claude Code what `lab codex-measure` does for a
//! codex-alab package, by the same steps ([`crate::measure`], read through [`CLAUDE`]): one lab
//! session (a private headless instance, the harness off so it presses nothing, Claude Code on the
//! stub's subscriber path) whose transcript is reduced, judged and written. Like codex-measure it
//! writes no golden: the build is held to its session log and to the keys and record kinds every
//! measured build showed.
//!
//! 1. the key: the build the managed Claude Code says it is (`<claude> --version`, a scratch
//!    HOME, nothing inherited: [`installed_build`]), KEY when it says none; NOT RUN when a Claude
//!    Code reduction in the step testdata is not as HEAD holds it ([`committed_in`]); exit 0,
//!    writing nothing, when HEAD holds `<build>.jsonl` naming it; STEM when that name holds
//!    another build's reduction, or another file names the build ([`admit`]);
//! 2. build aterm, under the measurements' one lock from here to the end ([`hold_clean`]), and the
//!    lab's aterm is that build ([`same_aterm`]);
//! 3. run 1, one session expecting `partial` ([`session`]): a turn, a Bash call through its
//!    approval box, and a turn that meets the stub's five-hour limit; the lab's Claude Code must
//!    say the key (KEY), and its session log is judged ([`judge_log`]); PROOF-RED on any FAIL;
//! 4. the session's one transcript (TRANSCRIPTS), every row that names a version naming the key
//!    (KEY), reduced twice and a third pass byte-identical (FIXED-POINT), its own line naming the
//!    key (KEY), and held to the privacy oracle every committed reduction is held to, against the
//!    transcript itself: keys, row order, numbers and closed words only (TEXT)
//!    ([`reduce_session`]);
//! 5. its keys and record kinds against every committed Claude Code reduction (NEW-KEY) and the
//!    transcript keys the fold reads that the previous build showed (GONE) ([`judge`]); other
//!    drift is noted;
//! 6. `<build>.jsonl` written create-new;
//! 7. the standing tests ([`STANDING`]), the crates named (STANDING);
//! 8. aterm rebuilt and run 2 expecting `on` (NOT-BOUND);
//! 9. the note, `docs/measured/claude-code-<build>-session-log-<date>.md`, only when all passed;
//! 10. both worlds removed (kept and named on a refusal, or with `--keep`).
//!
//! What [`crate::measure`] says of a refusal after step 6, an interrupt, a panic and a second
//! interrupt holds here: the checkout ends as it began. What stays a person's or an agent's: the
//! CHANGELOG line, the commit, the merge and the push.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::Value;
use aterm_phase::anchors::TRANSCRIPT_KEYS;
use aterm_uds::spawnfd::Stdio;

use crate::codex_alab_proof::Run;
use crate::door::Door;
use crate::instance::{World, take_flag};
use crate::lab::{Launch, UpOpts};
use crate::measure::{
    Class, Family, Judged, MEASURED, Reduction, Refusal, STEPS, build_aterm, committed_in, gate,
    hold_clean, judge, landed, not_run, reduce_checked, reduce_leg, rows, sha256, standing,
    standing_line, today, undo_on_hard_exit, with_world, word,
};
use crate::paths::paths;
use crate::reduce::{self, Program, oracle};
use crate::run::Run as Lab;
use crate::{Fail, LAB, Res};

/// The one leg a Claude Code measurement reduces: the session.
pub(crate) const LEG: &str = "session";

/// A made-up word every prompt of the session carries: the reduction must keep it nowhere.
pub(crate) const WORD: &str = "quokkafern";

/// How long a wait for what must happen holds: a one-minute hang detector, never a latency
/// budget.
const HANG: Duration = Duration::from_mins(1);

/// [`HANG`] in seconds, as the door's waits take it.
const HANG_S: u64 = 60;

/// The call the stub makes in turn 2: a Bash `touch` in the project, which the approval box
/// asks about (`--permission-mode default`), then its reply.
const TOOL: &str = r#"[{"name": "Bash", "input": {"command": "touch @PROJ@/quokkafern.txt", "description": "Touch a marker"}}, {"text": "Done."}]"#;

/// The libtest filters step 7 runs: the step fold's tests (the measured builds held to their
/// reductions, each folding whole and exact with a lab run's events, and the corpus), the
/// reducer's tests over every committed Claude Code reduction, and this verb's own, which read
/// the step testdata with the new reduction in place.
const FILTERS: [&str; 4] = [
    "harness::steps::",
    "reduce::tests::a_tool_argument_name_is_never_kept_as_written",
    "reduce::tests::every_committed_claude_reduction_keeps_its_limit_words",
    "claude_measure::tests::",
];

/// The standing tests step 7 must see pass, by their libtest names: every measured build's
/// reduction folds whole, exact and with a lab run's events, and `MEASURED_BUILDS` is what the
/// committed reductions name; the corpus still folds to its lines; every committed reduction keeps
/// no text and its limit's words; every Claude Code build in the step testdata reads as measured;
/// and every key the fold reads of a committed reduction is one GONE holds the next build to.
pub(crate) const STANDING: [&str; 7] = [
    "harness::steps::tests::a_build_is_exact_iff_its_reduction_is_committed",
    "harness::steps::tests::the_measured_builds_are_what_the_committed_reductions_name",
    "harness::steps::corpus_tests::every_corpus_slice_folds_to_its_lines",
    "reduce::tests::a_tool_argument_name_is_never_kept_as_written",
    "reduce::tests::every_committed_claude_reduction_keeps_its_limit_words",
    "claude_measure::tests::every_claude_build_in_the_step_testdata_reads_as_measured",
    "claude_measure::tests::gone_holds_a_build_to_every_transcript_key_the_fold_reads",
];

/// What `claude-measure` reads and holds where the measurements differ ([`crate::measure`]).
pub(crate) const CLAUDE: Family = Family {
    program: Program::Claude,
    verb: "claude-measure",
    source: "transcript",
    read: TRANSCRIPT_KEYS,
    holds: is_reduction,
    measures: every,
    kind: record_kind,
    name_of: reduction_name,
    key_holds,
    crates: &["aterm-agent", "aterm-lab"],
    filters: &FILTERS,
    standing: &STANDING,
};

/// Whether the file `name` is a Claude Code transcript reduction, as aterm-agent's `build.rs`
/// reads one: every `.jsonl` in the step testdata that is no Codex rollout's.
fn is_reduction(name: &str) -> bool {
    name.ends_with(".jsonl") && !name.starts_with("codex-")
}

/// Every Claude Code build a committed reduction names is measured.
fn every(_: &str) -> bool {
    true
}

/// A transcript row's record kind: `<type>/<subtype>`.
fn record_kind(v: &Value) -> String {
    format!("{}/{}", word(v.get("type")), word(v.get("subtype")))
}

/// The reduction a build's session is committed as: `<build>.jsonl`.
fn reduction_name(build: &str, _leg: &str) -> String {
    format!("{build}.jsonl")
}

/// The build a `claude --version` line says (`2.1.312 (Claude Code)`): its first word, when that
/// is a version (digits and dots, a digit at each end).
#[must_use]
pub(crate) fn version_of(said: &str) -> Option<String> {
    let w = said.lines().next()?.split_whitespace().next()?;
    let digit_ends =
        w.starts_with(|c: char| c.is_ascii_digit()) && w.ends_with(|c: char| c.is_ascii_digit());
    (digit_ends && w.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| w.to_string())
}

/// STEP 1: the build the Claude Code at `bin` says it is: `<bin> --version` run with a scratch
/// HOME, `PATH=/usr/bin:/bin`, its updater off and nothing else inherited, within the first
/// exec's hang detector ([`version_of`]).
/// # Errors
/// KEY: it could not run, failed, or said no build.
pub(crate) fn installed_build(bin: &str) -> Result<String, Refusal> {
    let key = |why: String| Refusal::new(Class::Key, why);
    let home = aterm_tempfile::tempdir().map_err(|e| key(format!("a scratch HOME: {e}")))?;
    let mut cmd = aterm_uds::spawnfd::command(bin);
    cmd.arg("--version")
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("DISABLE_AUTOUPDATER", "1")
        .stdin(Stdio::null());
    let out = aterm_uds::deadline::output_within(&mut cmd, aterm_uds::deadline::FIRST_EXEC_HANG)
        .map_err(|e| key(format!("cannot run {bin} --version: {e}")))?;
    let said = String::from_utf8_lossy(&out.stdout);
    version_of(&said)
        .filter(|_| out.status.success())
        .ok_or_else(|| {
            key(format!(
                "{bin} --version exited {} and said no build ({:?})",
                out.status,
                said.lines().next().unwrap_or("")
            ))
        })
}

/// STEP 1 on `build` against the committed reductions: `Ok(None)` when `<build>.jsonl` is
/// committed naming it (it is measured, and nothing is to be done), else the name its reduction
/// takes.
/// # Errors
/// STEM: `<build>.jsonl` holds another build's reduction or none's, or another file names
/// `build` (`build.rs` would list it under a name no reader looks up).
pub(crate) fn admit(build: &str, committed: &[Reduction]) -> Result<Option<String>, Refusal> {
    let name = reduction_name(build, LEG);
    if let Some(r) = committed.iter().find(|r| r.name == name) {
        return match r.build.as_deref() {
            Some(b) if b == build => Ok(None),
            other => Err(Refusal::new(
                Class::Stem,
                format!(
                    "{build}'s reduction would be {name}, which holds {}'s",
                    other.unwrap_or("no build")
                ),
            )),
        };
    }
    let elsewhere: Vec<&str> = committed
        .iter()
        .filter(|r| r.build.as_deref() == Some(build))
        .map(|r| r.name.as_str())
        .collect();
    if elsewhere.is_empty() {
        Ok(Some(name))
    } else {
        Err(Refusal::new(
            Class::Stem,
            format!(
                "{} name {build} under another name than {name}",
                elsewhere.join(", ")
            ),
        ))
    }
}

/// STEP 4: every row of the transcript `text` that names a version names `build`, and one does:
/// the fold reads each row's own build.
/// # Errors
/// KEY: a row names another version or none, or no row names one.
pub(crate) fn key_holds(leg: &str, text: &str, build: &str) -> Result<(), Refusal> {
    let mut named = 0;
    for (n, row) in rows(text).enumerate() {
        let Ok(v) = aterm_json::from_str::<Value>(row) else {
            continue;
        };
        let Some(said) = v.get("version") else {
            continue;
        };
        named += 1;
        if said.as_str() != Some(build) {
            return Err(Refusal::new(
                Class::Key,
                format!(
                    "{leg}: row {} names version {} where the build measured is {build}",
                    n + 1,
                    said.as_str().unwrap_or("none")
                ),
            ));
        }
    }
    if named == 0 {
        return Err(Refusal::new(
            Class::Key,
            format!("{leg}: no row names a version"),
        ));
    }
    Ok(())
}

/// STEP 4: the one transcript the session's Claude Code wrote, among the files under `dir`.
/// # Errors
/// TRANSCRIPTS: none, or more than one.
pub(crate) fn one_transcript(dir: &Path) -> Result<PathBuf, Refusal> {
    let mut all = Vec::new();
    crate::util::walk_files(dir, &mut all);
    all.retain(|p| p.extension().is_some_and(|e| e == "jsonl"));
    all.sort();
    match all.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(Refusal::new(
            Class::Transcripts,
            format!(
                "{LEG}: {} transcripts under {} where one is reduced ({})",
                all.len(),
                dir.display(),
                all.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

/// STEP 4's privacy check of `text`, the reduction of the transcript `source` whose first pass is
/// `first`: the privacy oracle every committed reduction is held to ([`oracle::check_no_text`],
/// ruling T7), here against the transcript itself, whose rows the first pass holds one to one;
/// and the session's own word ([`WORD`]) nowhere in `text`.
/// # Errors
/// TEXT: either keeps text of the transcript.
pub(crate) fn text_kept(
    source: &str,
    first: &reduce::Reduction,
    text: &str,
) -> Result<(), Refusal> {
    oracle::check_no_text(LEG, source, first, Program::Claude)
        .map_err(|e| Refusal::new(Class::Text, format!("{LEG}: {e}")))?;
    if text.contains(WORD) {
        return Err(Refusal::new(
            Class::Text,
            format!("{LEG}: the reduction keeps the session's own word {WORD:?}"),
        ));
    }
    Ok(())
}

/// STEP 4 on the transcript `source`: its rows name `build`, its fixed point by `by` names
/// `build` ([`reduce_leg`]), and it keeps no text of the transcript ([`text_kept`]): the
/// reduction.
/// # Errors
/// KEY, FIXED-POINT or TEXT.
pub(crate) fn reduce_session(
    source: &str,
    build: &str,
    by: &dyn Fn(&str) -> Result<String, String>,
) -> Result<String, Refusal> {
    let text = reduce_leg(&CLAUDE, LEG, source, build, by)?;
    let first = reduce::reduce(source, Program::Claude)
        .map_err(|e| Refusal::new(Class::FixedPoint, format!("{LEG}: pass 1: {e}")))?;
    text_kept(source, &first, &text)?;
    Ok(text)
}

/// What a session expects of the build: unmeasured (run 1) or measured (run 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expect {
    /// Read `partial`: every transcript step strong.
    Partial,
    /// Read `on`: every transcript step exact.
    On,
}

impl Expect {
    /// The transcript source's state word in the session log.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Expect::Partial => "partial",
            Expect::On => "on",
        }
    }

    /// The confidence of every transcript record but the source's own mark.
    #[must_use]
    pub(crate) const fn conf(self) -> &'static str {
        match self {
            Expect::Partial => "strong",
            Expect::On => "exact",
        }
    }
}

/// One session-log record (`<seq> <verb> <id> <kind> <k=v>…`): its verb, id and kind, and its
/// fields.
struct Rec<'a> {
    verb: &'a str,
    id: &'a str,
    kind: &'a str,
    fields: Vec<(&'a str, &'a str)>,
}

impl Rec<'_> {
    fn get(&self, k: &str) -> Option<&str> {
        self.fields.iter().find(|(a, _)| *a == k).map(|(_, v)| *v)
    }

    fn transcript(&self) -> bool {
        self.get("src") == Some("transcript")
    }

    fn said(&self) -> String {
        format!("{} {} {}", self.verb, self.id, self.kind)
    }
}

/// The records of `aterm ctl log`'s text (its lines that open with a sequence number).
fn records(log: &str) -> Vec<Rec<'_>> {
    log.lines()
        .filter_map(|l| {
            let mut t = l.split_whitespace();
            t.next()?.parse::<u64>().ok()?;
            let (verb, id, kind) = (t.next()?, t.next()?, t.next()?);
            Some(Rec {
                verb,
                id,
                kind,
                fields: t.filter_map(|w| w.split_once('=')).collect(),
            })
        })
        .collect()
}

/// The turn ends the transcript gave (`amend t<n> turn vdur_ms=…`).
fn turn_ends(recs: &[Rec<'_>]) -> usize {
    recs.iter()
        .filter(|r| {
            r.transcript() && r.verb == "amend" && r.kind == "turn" && r.get("vdur_ms").is_some()
        })
        .count()
}

/// Whether the transcript opened the session's Bash call (`open p<n> tool … cls=shell`).
fn shell_opened(log: &str) -> bool {
    records(log).iter().any(|r| {
        r.transcript() && r.verb == "open" && r.kind == "tool" && r.get("cls") == Some("shell")
    })
}

/// Whether the session log holds everything the transcript gives of the session: its three turn
/// ends, and, read exact, the usage wall it confirms.
fn settled(log: &str, expect: Expect) -> bool {
    let recs = records(log);
    turn_ends(&recs) >= 3
        && (expect == Expect::Partial || recs.iter().any(|r| r.get("confirmed") == Some("1")))
}

/// STEP 3's judgment of the session log `log` of one session of `build` expecting `expect`: each
/// check, and whether it held. The transcript source reads `expect`; the transcript names the
/// build; it opens the Bash call and ends it ok; it ends the three turns; every record it gives
/// but the source's own mark is at `expect`'s confidence (strong while partial, fix E1; exact once
/// measured); the usage wall is confirmed once measured and never while partial; and no step is
/// lost (`dropped=`).
#[must_use]
pub(crate) fn judge_log(log: &str, build: &str, expect: Expect) -> Vec<(bool, String)> {
    let recs = records(log);
    let conf = expect.conf();
    let tx: Vec<&Rec<'_>> = recs.iter().filter(|r| r.transcript()).collect();
    let source = tx
        .iter()
        .rev()
        .find(|r| r.kind == "source" && r.get("field") == Some("transcript"))
        .and_then(|r| r.get("state"));
    let mut out = vec![(
        source == Some(expect.word()),
        format!(
            "the transcript source reads {} (the log says {})",
            expect.word(),
            source.unwrap_or("nothing")
        ),
    )];
    out.push((
        tx.iter()
            .any(|r| r.kind == "agent" && r.get("ver") == Some(build)),
        format!("the transcript names the build (ver={build})"),
    ));
    let call = tx
        .iter()
        .find(|r| r.verb == "open" && r.kind == "tool" && r.get("cls") == Some("shell"));
    out.push((
        call.is_some_and(|r| r.get("conf") == Some(conf)),
        format!("the transcript opens the Bash call at conf={conf}"),
    ));
    let ended = call.is_some_and(|c| {
        tx.iter().any(|r| {
            r.id == c.id
                && r.kind == "tool"
                && matches!(r.verb, "close" | "amend")
                && r.get("out") == Some("ok")
                && r.get("conf") == Some(conf)
        })
    });
    out.push((
        ended,
        format!("the transcript ends the Bash call ok at conf={conf}"),
    ));
    let ends = turn_ends(&recs);
    out.push((
        ends == 3,
        format!("the transcript ends the three turns (vdur_ms= on {ends})"),
    ));
    let off: Vec<String> = tx
        .iter()
        .filter(|r| r.kind != "source" && r.get("conf") != Some(conf))
        .map(|r| r.said())
        .collect();
    out.push((
        off.is_empty(),
        format!(
            "every transcript record is at conf={conf}{}",
            if off.is_empty() {
                String::new()
            } else {
                format!(" (not: {})", off.join(", "))
            }
        ),
    ));
    match expect {
        Expect::On => out.push((
            tx.iter()
                .any(|r| r.kind == "wall" && r.get("confirmed") == Some("1")),
            "the transcript confirms the usage wall (confirmed=1)".to_string(),
        )),
        Expect::Partial => out.push((
            !recs.iter().any(|r| r.get("confirmed").is_some()),
            "no wall is confirmed while the build is unmeasured (no confirmed=)".to_string(),
        )),
    }
    let dropped: Vec<String> = recs
        .iter()
        .filter(|r| r.get("dropped").is_some())
        .map(|r| r.said())
        .collect();
    out.push((
        dropped.is_empty(),
        format!(
            "no step is lost{}",
            if dropped.is_empty() {
                String::new()
            } else {
                format!(" (dropped= on {})", dropped.join(", "))
            }
        ),
    ));
    out
}

/// A run's checks: how many held, and each that failed.
#[derive(Debug, Default)]
struct Checks {
    oks: usize,
    fails: Vec<String>,
}

impl Checks {
    /// Count `what`'s check: whether it held.
    fn check(&mut self, ok: bool, what: &str) -> bool {
        if ok {
            self.oks += 1;
        } else {
            self.fails.push(what.to_string());
        }
        ok
    }
}

/// The verb's own lab, taken down when it drops (a refusal, an interrupt, a panic), so no run
/// leaves one up; its lifeline ends it if the verb is killed.
struct Own(String);

impl Drop for Own {
    fn drop(&mut self) {
        let _ = crate::lab::down(&self.0, false);
    }
}

/// The session log so far, polled until `done` holds of it or the hang detector ends: whether it
/// did, and the last log read.
fn log_until(door: &Door<'_>, done: &dyn Fn(&str) -> bool) -> Result<(bool, String), Fail> {
    let start = Instant::now();
    loop {
        let log = door.out(&["log", "n=400"]);
        if done(&log) {
            return Ok((true, log));
        }
        if start.elapsed() >= HANG {
            return Ok((false, log));
        }
        crate::stop::sleep(Duration::from_millis(250))?;
    }
}

/// STEPS 3 AND 8, the session the lab `tag` runs, each step a check that must hold before the
/// next is taken: Claude Code launched on the subscriber path in the approval mode, a turn
/// answered, a Bash call the transcript reads before its approval box is answered, and a turn
/// that meets the stub's five-hour limit.
fn drive(tag: &str, c: &mut Checks) -> Result<(), Fail> {
    let lab = Lab::load(tag)?;
    let launch = Launch {
        subscriber: true,
        args: vec!["--permission-mode".into(), "default".into()],
        ..Launch::default()
    };
    crate::lab::launch_claude(&lab, &launch)?;
    let lab = Lab::load(tag)?;
    let door = Door::new(&lab)?;
    if !c.check(
        door.seen("Claude Code v", HANG_S)?,
        "Claude Code drew its banner",
    ) {
        return Ok(());
    }
    door.await_idle(1500, HANG_S)?;
    crate::verbs::set_mode(&lab, "ok")?;
    if !c.check(
        crate::verbs::turn(&door, &format!("say hello {WORD}"))?,
        "turn 1 typed",
    ) || !c.check(door.seen("local lab stub", HANG_S)?, "turn 1 answered")
    {
        return Ok(());
    }
    door.await_idle(1500, HANG_S)?;
    crate::verbs::set_tool(&lab, TOOL)?;
    if !c.check(
        crate::verbs::turn(&door, &format!("toolstart {WORD}"))?,
        "turn 2 typed",
    ) || !c.check(
        door.seen("Do you want to proceed", HANG_S)?,
        "the Bash call's approval box drawn",
    ) {
        return Ok(());
    }
    let (read, _) = log_until(&door, &shell_opened)?;
    if !c.check(
        read,
        "the transcript's Bash call read before its box is answered",
    ) {
        return Ok(());
    }
    door.ctl(&["key", "enter"])?;
    if !c.check(door.seen("Done\\.", HANG_S)?, "turn 2 answered") {
        return Ok(());
    }
    door.await_idle(1500, HANG_S)?;
    crate::verbs::set_limit(&lab, "five_hour", crate::util::now_secs() + 3600, "")?;
    if !c.check(
        crate::verbs::turn(&door, &format!("one more {WORD}"))?,
        "turn 3 typed",
    ) || !c.check(door.seen("hit your", HANG_S)?, "turn 3 met the limit")
    {
        return Ok(());
    }
    door.await_idle(1500, HANG_S)?;
    Ok(())
}

/// Write `text` to `path` in a run's world.
fn save(path: &Path, text: &str) -> Result<(), Fail> {
    std::fs::write(path, text).map_err(|e| not_run(format!("{}: {e}", path.display())))
}

/// STEPS 3 AND 8: one session of `build` in a lab of the verb's own (`up`, the folder trusted,
/// the harness off, nothing else of `LAB_*` read), expecting `expect`, the `n`th: its checks
/// ([`drive`], then [`judge_log`] on its session log), and a world holding its session log, its
/// screen and its transcripts (`transcripts/`), kept until the verb releases it. The lab is down
/// when it returns.
/// # Errors
/// The run's interrupt; KEY: the lab's Claude Code says another build; NOT RUN: the lab could not
/// come up or its world be written.
fn session(n: u8, build: &str, expect: Expect) -> Result<Run, Fail> {
    let t = Instant::now();
    let mut world = World::new("").map_err(not_run)?;
    world.keep();
    let tag = format!("cm{}-{n}", std::process::id());
    let o = UpOpts {
        trust: true,
        harness_keys: "enabled = false".into(),
        cols: "144".into(),
        lines: "50".into(),
        lifeline: true,
        ..UpOpts::default()
    };
    let lab = crate::lab::up(&tag, &o).map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            not_run(format!("lab up {tag}: {}", f.msg.unwrap_or_default()))
        }
    })?;
    let _down = Own(tag.clone());
    if lab.claude_ver != build {
        world.release();
        return Err(Refusal::new(
            Class::Key,
            format!(
                "the lab's Claude Code ({}) says it is {} where the key is {build}",
                lab.claude_bin, lab.claude_ver
            ),
        )
        .into());
    }
    let mut c = Checks::default();
    drive(&tag, &mut c).map_err(|f| with_world(f, &world.dir, None))?;
    let lab = Lab::load(&tag)?;
    let door = Door::new(&lab)?;
    let (_, log) = log_until(&door, &|l| settled(l, expect))?;
    save(&world.dir.join("session-log.txt"), &log)?;
    save(&world.dir.join("screen.txt"), &door.out(&["text"]))?;
    let mut found = Vec::new();
    crate::util::walk_files(&lab.w.join("home/.claude/projects"), &mut found);
    found.retain(|p| p.extension().is_some_and(|e| e == "jsonl"));
    found.sort();
    let into = world.dir.join("transcripts");
    std::fs::create_dir_all(&into).map_err(|e| not_run(format!("{}: {e}", into.display())))?;
    for (i, p) in found.iter().enumerate() {
        let to = into.join(format!("{}.jsonl", i + 1));
        std::fs::copy(p, &to).map_err(|e| not_run(format!("{}: {e}", p.display())))?;
    }
    if c.fails.is_empty() {
        for (ok, what) in judge_log(&log, build, expect) {
            c.check(ok, &what);
        }
    }
    Ok(Run {
        world,
        oks: c.oks,
        fails: c.fails,
        fences: Vec::new(),
        took: Some(t.elapsed()),
    })
}

/// STEPS 2 AND 8: the aterm the lab runs ([`crate::paths::Paths::need_aterm`]) is the one the
/// build made, by its path or, for a copy of it (`target/debug/aterm` beside a build directory
/// elsewhere), by its bytes ([`same_bytes`]), so the session runs the checkout's code.
/// # Errors
/// NOT RUN: the lab runs another (`LAB_ATERM` names it) or none.
fn same_aterm(built: &Path) -> Result<(), Fail> {
    let lab = paths().need_aterm().map_err(not_run)?;
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if real(lab) == real(built) || same_bytes(lab, built) {
        Ok(())
    } else {
        Err(not_run(format!(
            "the lab runs {} where the build made {}: unset LAB_ATERM, or set it to the build",
            lab.display(),
            built.display()
        )))
    }
}

/// Whether the files `a` and `b` hold the same bytes (either unreadable: no).
fn same_bytes(a: &Path, b: &Path) -> bool {
    use std::io::Read as _;
    let fill = |r: &mut std::fs::File, buf: &mut [u8]| -> Option<usize> {
        let mut n = 0;
        while n < buf.len() {
            match r.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
        Some(n)
    };
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let len = |f: &std::fs::File| f.metadata().map(|m| m.len()).ok();
    if len(&fa).is_none() || len(&fa) != len(&fb) {
        return false;
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        match (fill(&mut fa, &mut ba), fill(&mut fb, &mut bb)) {
            (Some(0), Some(0)) => return true,
            (Some(x), Some(y)) if x == y && ba[..x] == bb[..y] => {}
            _ => return false,
        }
    }
}

/// A run's line in the note and the report.
fn run_line(n: u8, expect: Expect, run: &Run) -> String {
    format!(
        "run {n} (expecting `{}`): {}, {} ok, {} FAIL; {}",
        expect.word(),
        if run.fails.is_empty() { "PASS" } else { "FAIL" },
        run.oks,
        run.fails.len(),
        run.took
            .map_or_else(|| "-".to_string(), |d| format!("{} s", d.as_secs()))
    )
}

/// `path` as a note says it: `~/…` below the owner's home `home`, so no committed note names the
/// owner's account.
fn said_path(path: &str, home: Option<&str>) -> String {
    home.and_then(|h| path.strip_prefix(h))
        .and_then(|below| below.strip_prefix('/'))
        .map_or_else(|| path.to_string(), |below| format!("~/{below}"))
}

/// What the note says.
struct Note<'a> {
    build: &'a str,
    /// Claude Code's path as the note says it ([`said_path`]).
    bin_said: &'a str,
    date: &'a str,
    head: (&'a str, &'a str),
    runs: [&'a Run; 2],
    text: &'a str,
    judged: &'a Judged,
    passed: &'a [String],
}

impl Note<'_> {
    fn text(&self) -> String {
        let (commit, tree) = self.head;
        let mut t = format!(
            "<!-- SPDX-License-Identifier: Apache-2.0 -->\n<!-- Copyright 2026 Andrew Yates -->\n\
             # Claude Code {build}: measured for the session log ({date})\n\n\
             `{LAB} claude-measure` measured Claude Code `{build}` on this Mac: one lab session \
             (a private headless instance, its harness off, Claude Code on the lab stub's subscriber \
             path: a turn, a Bash call through its approval box, a turn that meets the stub's \
             five-hour limit) run twice, first expecting the build unmeasured (read partial), then \
             measured (read on), with run 1's transcript reduced between the runs. This page is the \
             verb's own record.\n\n\
             ## What ran\n\n\
             * **Claude Code:** `{build}` at `{bin_said}`.\n\
             * **The aterm:** debug builds (`targo --unverified build -p aterm`) of commit `{commit}` \
             (tree `{tree}`): run 1 as committed, run 2 with the reduction below added.\n\n\
             ## The runs\n\n",
            build = self.build,
            date = self.date,
            bin_said = self.bin_said,
        );
        for (n, (run, expect)) in self
            .runs
            .iter()
            .zip([Expect::Partial, Expect::On])
            .enumerate()
        {
            t.push_str(&format!("* {}\n", run_line(n as u8 + 1, expect, run)));
        }
        t.push_str(&format!(
            "\n## The build measured\n\n\
             The session's one transcript names `{build}` on every row that names a version. It was \
             reduced (`lab reduce --program claude`, its check of what it writes included), then \
             reduced again; a third pass gave the second's bytes, so the second is a fixed point, \
             and the privacy oracle found no text of the transcript in it. It is committed as \
             `{STEPS}/{build}.jsonl`: {} bytes, {} rows, sha256 `{}`.\n",
            self.text.len(),
            rows(self.text).count(),
            sha256(self.text),
            build = self.build,
        ));
        let j = self.judged;
        t.push_str(&format!(
            "\n**Transcript keys.** It shows {} keys and {} record kinds, none that the {} measured \
             builds' reductions do not show, and no transcript key the fold reads that {} showed is \
             gone.",
            j.keys,
            j.kinds,
            j.builds,
            j.prev.as_deref().unwrap_or("a previous build")
        ));
        if j.noted.is_empty() {
            t.push_str(" No other key, record kind or step shape moved.\n");
        } else {
            t.push_str(" What moved and does not refuse:\n\n");
            for n in &j.noted {
                t.push_str(&format!("* {n}\n"));
            }
        }
        t.push_str(&format!(
            "\n**The standing tests passed** with the reduction in place (`{}`): {}.\n",
            standing_line(&CLAUDE),
            self.passed
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        t
    }
}

/// A refusal of step 4 or 5 as the run's failure, naming run 1's world, kept for it.
fn kept(world: &Path) -> impl Fn(Refusal) -> Fail + '_ {
    move |r| with_world(Fail::from(r), world, None)
}

/// `claude-measure [--keep]`.
/// # Errors
/// Usage; NOT RUN (no managed Claude Code, a dirty checkout, the lock held, a build that failed,
/// a lab that did not come up); a refusal by class; an interrupt.
pub fn cmd_claude_measure(args: &[String]) -> Res {
    crate::stop::install();
    crate::stop::at_hard_exit(undo_on_hard_exit);
    let mut a = args.to_vec();
    let keep = take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "claude-measure: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let repo = paths().repo.clone();
    let steps = repo.join(STEPS);
    let bin = crate::lab::find_claude().map_err(not_run)?;
    let bin_said = said_path(&bin, paths().need_home().ok());

    let build = installed_build(&bin)?;
    let committed = committed_in(&CLAUDE, &repo)?;
    let Some(name) = admit(&build, &committed)? else {
        println!(
            "claude-measure: 1. {build} ({bin_said}) is measured already: HEAD holds {build}.jsonl, so MEASURED_BUILDS names it; nothing written"
        );
        return Ok(0);
    };
    println!(
        "claude-measure: 1. {build} ({bin_said}) is not measured; its reduction will be {name}"
    );
    let (_held, head, tree) = hold_clean(&CLAUDE, &repo)?;
    let (head, tree) = (head.as_str(), tree.as_str());

    println!("claude-measure: 2. building aterm at {head}");
    let aterm = build_aterm(&CLAUDE, &repo)?;
    same_aterm(&aterm)?;
    println!("claude-measure: 3. run 1, expecting {build} unmeasured (read partial)");
    let mut run1 = session(1, &build, Expect::Partial)?;
    println!("claude-measure: 3. {}", run_line(1, Expect::Partial, &run1));
    gate(Class::ProofRed, &run1)?;
    let world1 = run1.world.dir.clone();
    let from = one_transcript(&world1.join("transcripts")).map_err(kept(&world1))?;
    let source = std::fs::read_to_string(&from).map_err(|e| {
        kept(&world1)(Refusal::new(
            Class::Transcripts,
            format!("{LEG}: {}: {e}", from.display()),
        ))
    })?;
    let text =
        reduce_session(&source, &build, &reduce_checked(Program::Claude)).map_err(kept(&world1))?;
    println!(
        "claude-measure: 4. one transcript ({} bytes, {} rows), version {build} on every row that names one, a fixed point ({} bytes; pass 3 = pass 2), no text of it kept",
        source.len(),
        rows(&source).count(),
        text.len()
    );
    let judged =
        judge(&CLAUDE, &build, &[(LEG, text.clone())], &committed).map_err(kept(&world1))?;
    println!(
        "claude-measure: 5. {} keys and {} record kinds, nothing new against {} measured builds' reductions, no transcript key gone against {}{}",
        judged.keys,
        judged.kinds,
        judged.builds,
        judged.prev.as_deref().unwrap_or("no previous build"),
        if judged.noted.is_empty() {
            "; nothing else moved".to_string()
        } else {
            format!("; noted: {}", judged.noted.join("; "))
        }
    );

    let fixtures = vec![(steps.join(&name), text.clone())];
    let date = today();
    let note_path = repo
        .join(MEASURED)
        .join(format!("claude-code-{build}-session-log-{date}.md"));
    let (written, (passed, mut run2)) = landed(&CLAUDE, &fixtures, |w| {
        let passed = standing(&CLAUDE, &repo).map_err(|f| with_world(f, &world1, None))?;
        println!("claude-measure: 8. rebuilding aterm with the reduction");
        let aterm = build_aterm(&CLAUDE, &repo).map_err(|f| with_world(f, &world1, None))?;
        same_aterm(&aterm).map_err(|f| with_world(f, &world1, None))?;
        println!("claude-measure: 8. run 2, expecting {build} measured (read on)");
        let run2 = session(2, &build, Expect::On).map_err(|f| with_world(f, &world1, None))?;
        println!("claude-measure: 8. {}", run_line(2, Expect::On, &run2));
        gate(Class::NotBound, &run2).map_err(|f| with_world(f, &world1, None))?;
        let note = Note {
            build: &build,
            bin_said: &bin_said,
            date: &date,
            head: (head, tree),
            runs: [&run1, &run2],
            text: &text,
            judged: &judged,
            passed: &passed,
        };
        w.create(&note_path, note.text().as_bytes())
            .map_err(|e| with_world(not_run(e), &world1, Some(&run2.world.dir)))?;
        println!("claude-measure: 9. wrote {}", note_path.display());
        Ok((passed, run2))
    })?;
    if keep {
        println!(
            "claude-measure: 10. the worlds are kept (--keep): {} and {}",
            world1.display(),
            run2.world.dir.display()
        );
    } else {
        println!(
            "claude-measure: 10. removing the worlds {} and {}",
            world1.display(),
            run2.world.dir.display()
        );
        run1.world.release();
        run2.world.release();
    }
    println!(
        "MEASURED {build}: {} tests passed; wrote {}",
        passed.len(),
        written
            .iter()
            .map(|p| p.strip_prefix(&repo).unwrap_or(p).display().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "left to do by hand: a CHANGELOG line under [Unreleased], the commit, the merge and the push"
    );
    Ok(0)
}

#[cfg(test)]
#[path = "claude_measure_tests.rs"]
mod tests;
