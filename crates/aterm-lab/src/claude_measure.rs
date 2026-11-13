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
//! 3. run 1, one session expecting `partial` ([`session`]): a turn, four Bash calls ([`CALLS`]:
//!    one granted once in its approval box, one granted for the session, one declined with Esc,
//!    and one run sandboxed, where Claude Code offers no box), each call's end read off the screen
//!    ([`shown`]), and a turn that meets the stub's five-hour limit; the lab's Claude Code must
//!    say the key (KEY), and its session log is judged, each call's end against what the screen
//!    showed of it ([`judge_log`]); PROOF-RED on any FAIL;
//! 4. the session's one transcript (TRANSCRIPTS), every row that names a version naming the key
//!    (KEY), reduced twice and a third pass byte-identical (FIXED-POINT), its own line naming the
//!    key (KEY), and held to the privacy oracle every committed reduction is held to, against the
//!    transcript itself: keys, row order, numbers and closed words only (TEXT)
//!    ([`reduce_session`]);
//! 5. its keys and record kinds against every committed Claude Code reduction and the transcript
//!    keys the fold reads that the previous build showed ([`judge`], by the judge's rule in
//!    [`crate::measure`]): what is new on a row the fold does not act on ([`inert`]) is noted, a
//!    new key the fold reads refuses (NEW-KEY), a lost one refuses (GONE), any other new key or
//!    record kind waits on run 2; other drift is noted;
//! 6. `<build>.jsonl` written create-new;
//! 7. the standing tests ([`STANDING`]), the crates named (STANDING);
//! 8. aterm rebuilt and run 2 expecting `on` (NOT-BOUND); then each key step 5 left to it is
//!    admitted by the row of run 2's transcript that carries it and the check that read the step
//!    the fold made of that row, or refused as one no run exercised (NEW-KEY) ([`admitted_by`]);
//! 9. the note, `docs/measured/claude-code-<build>-session-log-<date>.md`, only when all passed;
//! 10. both worlds removed (kept and named on a refusal, or with `--keep`).
//!
//! What [`crate::measure`] says of a refusal after step 6, an interrupt, a panic and a second
//! interrupt holds here: the checkout ends as it began. What stays a person's or an agent's: the
//! CHANGELOG line, the commit, the merge and the push.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::steps::StepEvent;
use aterm_json::Value;
use aterm_phase::anchors::TRANSCRIPT_KEYS;
use aterm_uds::spawnfd::Stdio;

use crate::codex_alab_proof::Run;
use crate::door::Door;
use crate::instance::{World, take_flag};
use crate::lab::{Launch, UpOpts};
use crate::measure::{
    Class, Family, Judged, MEASURED, Reduction, Refusal, STEPS, build_aterm, committed_in,
    exercised, gate, hold_clean, judge, landed, not_run, reduce_checked, reduce_leg, row_steps,
    rows, sha256, standing, standing_line, today, undo_on_hard_exit, with_world, word,
};
use crate::paths::paths;
use crate::pyjson::J;
use crate::reduce::{self, Program, oracle};
use crate::run::Run as Lab;
use crate::{Fail, LAB, Res};

/// The one leg a Claude Code measurement reduces: the session.
pub(crate) const LEG: &str = "session";

/// Run 2, as a verdict names the run that admitted a key.
const RUN2: &str = "run 2 (expecting `on`)";

/// A made-up word every prompt of the session carries: the reduction must keep it nowhere.
pub(crate) const WORD: &str = "quokkafern";

/// How long a wait for what must happen holds: a one-minute hang detector, never a latency
/// budget.
const HANG: Duration = Duration::from_mins(1);

/// [`HANG`] in seconds, as the door's waits take it.
const HANG_S: u64 = 60;

/// The call the stub makes for Bash call `k` (from 1): a `touch` of a marker of the call's own in
/// the project, which the approval box asks about (`--permission-mode default`), then its reply
/// `Done <k>.`, so each call's row, box and reply on the screen are its own.
fn tool(k: usize) -> String {
    format!(
        r#"[{{"name": "Bash", "input": {{"command": "touch @PROJ@/{WORD}-{k}.txt", "description": "Touch marker {k}"}}}}, {{"text": "Done {k}."}}]"#
    )
}

/// How the session answers one of its Bash calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// `1. Yes` in its approval box: granted once (2.1.316 writes `grantScope` `once`).
    Once,
    /// `2. Yes, and always allow access to <folder> from this project`: granted for the session
    /// (2.1.316 writes `grantScope` `session` and persists nothing, so the next call's box is
    /// drawn).
    Session,
    /// Esc in its approval box: declined (`⎿  Interrupted · What should Claude do instead?`).
    Declined,
    /// No box: the sandbox on (`sandbox.enabled`, re-read before the call), Claude Code runs the
    /// call sandboxed and offers none. In the lab's own sandbox the nested one cannot start, so
    /// the call fails (`Error: Exit code 71`).
    Sandboxed,
}

/// The session's Bash calls, in order, by how each is answered: the ways a call's permission ends
/// whose rows a measured build must have read (fix J2-4, step 2).
pub(crate) const CALLS: [Answer; 4] = [
    Answer::Once,
    Answer::Session,
    Answer::Declined,
    Answer::Sandboxed,
];

impl Answer {
    /// How a check says it.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Answer::Once => "granted once",
            Answer::Session => "granted for the session",
            Answer::Declined => "declined",
            Answer::Sandboxed => "sandboxed, no box",
        }
    }

    /// The key that answers its approval box, or none: no box is drawn.
    #[must_use]
    pub(crate) const fn key(self) -> Option<&'static str> {
        match self {
            Answer::Once => Some("1"),
            Answer::Session => Some("2"),
            Answer::Declined => Some("escape"),
            Answer::Sandboxed => None,
        }
    }

    /// What the screen shows once call `k`'s turn has ended (a pattern for [`Door::seen`]): the
    /// stub's reply, or for a declined call Claude Code's question to the person.
    fn ended(self, k: usize) -> String {
        match self {
            Answer::Declined => "What should Claude do instead\\?".to_string(),
            _ => format!("Done {k}\\."),
        }
    }

    /// Whether a call so answered can end as the screen shows `s`: a declined one interrupted,
    /// any other one not.
    #[must_use]
    pub(crate) fn ends_as(self, s: Shown) -> bool {
        (self == Answer::Declined) == (s == Shown::Interrupted)
    }
}

/// The turns whose end the session log takes from the transcript (`turn_duration`, `vdur_ms=`):
/// every turn of the session (the first, one per call and the last) but the declined call's.
/// 2.1.316 writes a `turn_duration` after that call's interrupt too, but the session log has
/// ended the vendor turn at the interrupt, and a turn already ended takes no second end.
#[must_use]
pub(crate) fn turns_ended() -> usize {
    2 + CALLS.iter().filter(|a| **a != Answer::Declined).count()
}

/// What the screen shows of a Bash call's end: the `⎿` row under the call's own row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    /// It ran (`⎿  Done`, or its output).
    Ran,
    /// It failed (`⎿  Error: …`).
    Failed,
    /// A person interrupted it (the anchor `turn.interrupted`).
    Interrupted,
}

impl Shown {
    /// The `out=` the transcript must end a call with that the screen shows so: `ok` for one that
    /// ran, `error` for one that failed or that a person interrupted (Claude Code writes a
    /// declined call's result as an error).
    #[must_use]
    pub(crate) const fn out(self) -> &'static str {
        match self {
            Shown::Ran => "ok",
            Shown::Failed | Shown::Interrupted => "error",
        }
    }

    /// How a check says it.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Shown::Ran => "ran",
            Shown::Failed => "failed",
            Shown::Interrupted => "interrupted",
        }
    }
}

/// What `screen` (`aterm ctl text`) shows of Bash call `k`'s end: the first `⎿` row under the
/// call's own `⏺ Bash(… <WORD>-<k>.txt)` row, before the next `⏺` or `❯` row; none while it runs
/// (`Waiting…`, `Running…`) or when the screen shows no such row.
#[must_use]
pub(crate) fn shown(screen: &str, k: usize) -> Option<Shown> {
    let rows: Vec<&str> = screen.lines().map(str::trim).collect();
    let marker = format!("/{WORD}-{k}.txt)");
    let at = rows
        .iter()
        .position(|r| r.starts_with("⏺ Bash(") && r.ends_with(&marker))?;
    let rest = rows[at + 1..]
        .iter()
        .take_while(|r| !r.starts_with(['⏺', '❯']))
        .find_map(|r| r.strip_prefix('⎿'))?
        .trim_start();
    if rest.starts_with(aterm_phase::anchor!("turn.interrupted")) {
        Some(Shown::Interrupted)
    } else if rest.starts_with("Error") {
        Some(Shown::Failed)
    } else if rest.is_empty() || ["Waiting", "Running"].iter().any(|w| rest.starts_with(w)) {
        None
    } else {
        Some(Shown::Ran)
    }
}

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
/// every key the fold reads of a committed reduction is one GONE holds the next build to; and the
/// judge's rule (a) holds with the new reduction among the committed ones (an inert row changes
/// nothing the fold makes of it, and `TRANSCRIPT_KEYS` is the fold's read set).
pub(crate) const STANDING: [&str; 9] = [
    "harness::steps::tests::a_build_is_exact_iff_its_reduction_is_committed",
    "harness::steps::tests::the_measured_builds_are_what_the_committed_reductions_name",
    "harness::steps::corpus_tests::every_corpus_slice_folds_to_its_lines",
    "harness::steps::inert_tests::an_inert_row_changes_nothing_the_fold_makes",
    "harness::steps::inert_tests::the_fold_reads_what_the_transcript_keys_name",
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
    read_scope: |_, _| true,
    inert,
    words: &[],
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

/// Whether the step fold acts on no row of the record kind `kind` (`<type>/<subtype>`): its
/// `type` is none of the fold's [`ROW_KINDS`](aterm_agent::harness::steps::ROW_KINDS).
fn inert(kind: &str) -> bool {
    !aterm_agent::harness::steps::acts_on(kind.split('/').next().unwrap_or(kind))
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

/// Whether `r` is the transcript's opening of a Bash call (`open p<n> tool … cls=shell`).
fn opens_call(r: &Rec<'_>) -> bool {
    r.transcript() && r.verb == "open" && r.kind == "tool" && r.get("cls") == Some("shell")
}

/// How many of the session's Bash calls the transcript opened.
fn shell_calls(log: &str) -> usize {
    records(log).iter().filter(|r| opens_call(r)).count()
}

/// How many approval boxes the screen opened (`open p<n> approval`).
fn boxes(log: &str) -> usize {
    records(log)
        .iter()
        .filter(|r| r.verb == "open" && r.kind == "approval")
        .count()
}

/// Whether the session log holds everything the transcript gives of the session: every turn end
/// it writes ([`turns_ended`]), and, read exact, the usage wall it confirms.
fn settled(log: &str, expect: Expect) -> bool {
    let recs = records(log);
    turn_ends(&recs) >= turns_ended()
        && (expect == Expect::Partial || recs.iter().any(|r| r.get("confirmed") == Some("1")))
}

/// What a check of a session log reads: the source, the build, a Bash call's opening or end (the
/// call's number in the session, from 1), the turns, every record, the wall, or what was lost.
/// The judge's rule names, for each key it admits, the check that read the step the fold made of
/// the row carrying it ([`check_of_row`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum About {
    Source,
    Build,
    Opens(usize),
    Ends(usize),
    Turns,
    Records,
    Wall,
    Lost,
}

/// One check of a session log: whether it held, what it says, what it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub ok: bool,
    pub what: String,
    pub about: About,
}

fn check(ok: bool, what: String, about: About) -> Check {
    Check { ok, what, about }
}

/// STEP 3's judgment of the session log `log` of one session of `build` expecting `expect`, whose
/// screen showed its Bash calls end as `shown` says (in order, [`shown`]): each check, and whether
/// it held. The transcript source reads `expect`; the transcript names the build; it opens each
/// Bash call and ends it as the screen showed it ended ([`Shown::out`]); it ends every turn
/// Claude Code writes an end for ([`turns_ended`]); every record it gives but the source's own
/// mark is at `expect`'s confidence (strong while partial, fix E1; exact once measured); the usage
/// wall is confirmed once measured and never while partial; and no step is lost (`dropped=`).
#[must_use]
pub(crate) fn judge_log(log: &str, build: &str, expect: Expect, shown: &[Shown]) -> Vec<Check> {
    let recs = records(log);
    let conf = expect.conf();
    let tx: Vec<&Rec<'_>> = recs.iter().filter(|r| r.transcript()).collect();
    let source = tx
        .iter()
        .rev()
        .find(|r| r.kind == "source" && r.get("field") == Some("transcript"))
        .and_then(|r| r.get("state"));
    let mut out = vec![check(
        source == Some(expect.word()),
        format!(
            "the transcript source reads {} (the log says {})",
            expect.word(),
            source.unwrap_or("nothing")
        ),
        About::Source,
    )];
    out.push(check(
        tx.iter()
            .any(|r| r.kind == "agent" && r.get("ver") == Some(build)),
        format!("the transcript names the build (ver={build})"),
        About::Build,
    ));
    let calls: Vec<&&Rec<'_>> = tx.iter().filter(|r| opens_call(r)).collect();
    for (i, s) in shown.iter().enumerate() {
        let k = i + 1;
        let call = calls.get(i);
        out.push(check(
            call.is_some_and(|r| r.get("conf") == Some(conf)),
            format!("the transcript opens Bash call {k} at conf={conf}"),
            About::Opens(k),
        ));
        let ended = call.is_some_and(|c| {
            tx.iter().any(|r| {
                r.id == c.id
                    && r.kind == "tool"
                    && matches!(r.verb, "close" | "amend")
                    && r.get("out") == Some(s.out())
                    && r.get("conf") == Some(conf)
            })
        });
        out.push(check(
            ended,
            format!(
                "the transcript ends Bash call {k} {} at conf={conf}, as the screen showed it {}",
                s.out(),
                s.word()
            ),
            About::Ends(k),
        ));
    }
    let (ends, due) = (turn_ends(&recs), turns_ended());
    out.push(check(
        ends == due,
        format!(
            "the transcript ends the {due} turns Claude Code writes an end for (vdur_ms= on {ends})"
        ),
        About::Turns,
    ));
    let off: Vec<String> = tx
        .iter()
        .filter(|r| r.kind != "source" && r.get("conf") != Some(conf))
        .map(|r| r.said())
        .collect();
    out.push(check(
        off.is_empty(),
        format!(
            "every transcript record is at conf={conf}{}",
            if off.is_empty() {
                String::new()
            } else {
                format!(" (not: {})", off.join(", "))
            }
        ),
        About::Records,
    ));
    match expect {
        Expect::On => out.push(check(
            tx.iter()
                .any(|r| r.kind == "wall" && r.get("confirmed") == Some("1")),
            "the transcript confirms the usage wall (confirmed=1)".to_string(),
            About::Wall,
        )),
        Expect::Partial => out.push(check(
            !recs.iter().any(|r| r.get("confirmed").is_some()),
            "no wall is confirmed while the build is unmeasured (no confirmed=)".to_string(),
            About::Wall,
        )),
    }
    let dropped: Vec<String> = recs
        .iter()
        .filter(|r| r.get("dropped").is_some())
        .map(|r| r.said())
        .collect();
    out.push(check(
        dropped.is_empty(),
        format!(
            "no step is lost{}",
            if dropped.is_empty() {
                String::new()
            } else {
                format!(" (dropped= on {})", dropped.join(", "))
            }
        ),
        About::Lost,
    ));
    out
}

/// What a check reads of the steps the step fold made of row `row` (0-based among its rows) of
/// the reduction `text`: a call's opening or end by the call's number among the reduction's calls
/// (an interrupt ends the call it cut short), a prompt or a turn's end the turns, a wall the
/// wall, a model the build, any other step every record, and a row it made no step of what was
/// lost. A row the fold made several steps of (a turn's first assistant row opens the turn and a
/// call) is read by the check of its call, else of its wall, its build, its turn, in that order.
#[must_use]
pub(crate) fn about_row(text: &str, row: usize) -> About {
    let steps = reduce::readings(text, Program::Claude).steps;
    let mut calls = Vec::new();
    for s in &steps {
        if let StepEvent::ToolUse { id, .. } = s.ev
            && !calls.contains(&id)
        {
            calls.push(id);
        }
    }
    let number = |id| calls.iter().position(|c| *c == id).map(|i| i + 1);
    let rank = |a: About| match a {
        About::Opens(_) | About::Ends(_) => 0,
        About::Wall => 1,
        About::Build => 2,
        About::Turns => 3,
        _ => 4,
    };
    let (mut last, mut best) = (None, None);
    for s in &steps {
        if s.row == row {
            let about = match s.ev {
                StepEvent::ToolUse { id, .. } => number(id).map_or(About::Records, About::Opens),
                StepEvent::ToolResult { id, .. } => number(id).map_or(About::Records, About::Ends),
                StepEvent::Interrupt => last.map_or(About::Turns, About::Ends),
                StepEvent::Prompt { .. } | StepEvent::TurnEnd { .. } => About::Turns,
                StepEvent::EndWall { .. } => About::Wall,
                StepEvent::Model { .. } => About::Build,
                _ => About::Records,
            };
            if best.is_none_or(|b| rank(about) < rank(b)) {
                best = Some(about);
            }
        }
        if let StepEvent::ToolUse { id, .. } = s.ev {
            last = number(id);
        }
    }
    best.unwrap_or(About::Lost)
}

/// The check of `checks` (a run's, every one passed) that read the step the fold made of row
/// `row` of the reduction `text` ([`about_row`]), as a verdict names it.
#[must_use]
pub(crate) fn check_of_row(checks: &[Check], text: &str, row: usize) -> String {
    let about = about_row(text, row);
    let steps = row_steps(Program::Claude, text, row);
    checks.iter().find(|c| c.about == about).map_or_else(
        || format!("{steps}; every one of its {} checks passed", checks.len()),
        |c| format!("{steps}; its check \"{}\" passed", c.what),
    )
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
/// answered, the Bash calls ([`CALLS`], [`call`]), and a turn that meets the stub's five-hour
/// limit. What the screen showed of each call's end, in order, once every step held.
fn drive(tag: &str, c: &mut Checks) -> Result<Vec<Shown>, Fail> {
    let lab = Lab::load(tag)?;
    let launch = Launch {
        subscriber: true,
        args: vec!["--permission-mode".into(), "default".into()],
        ..Launch::default()
    };
    crate::lab::launch_claude(&lab, &launch)?;
    let lab = Lab::load(tag)?;
    let door = Door::new(&lab)?;
    let mut shown = Vec::new();
    if !c.check(
        door.seen("Claude Code v", HANG_S)?,
        "Claude Code drew its banner",
    ) {
        return Ok(shown);
    }
    door.await_idle(1500, HANG_S)?;
    crate::verbs::set_mode(&lab, "ok")?;
    if !c.check(
        crate::verbs::turn(&door, &format!("say hello {WORD}"))?,
        "turn 1 typed",
    ) || !c.check(door.seen("local lab stub", HANG_S)?, "turn 1 answered")
    {
        return Ok(shown);
    }
    door.await_idle(1500, HANG_S)?;
    for (i, answer) in CALLS.into_iter().enumerate() {
        match call(&lab, &door, c, i + 1, answer)? {
            Some(s) => shown.push(s),
            None => return Ok(shown),
        }
    }
    let last = CALLS.len() + 2;
    crate::verbs::set_limit(&lab, "five_hour", crate::util::now_secs() + 3600, "")?;
    if !c.check(
        crate::verbs::turn(&door, &format!("one more {WORD}"))?,
        &format!("turn {last} typed"),
    ) || !c.check(
        door.seen("hit your", HANG_S)?,
        &format!("turn {last} met the limit"),
    ) {
        return Ok(shown);
    }
    door.await_idle(1500, HANG_S)?;
    Ok(shown)
}

/// Bash call `k` (from 1) of the session, answered `answer`, in turn `k + 1`: for a sandboxed
/// call the sandbox turned on first, Claude Code having re-read its settings; the call the stub
/// makes ([`tool`]); its approval box drawn and the transcript's call read before the box is
/// answered with the answer's key, or for a call with no box the transcript's call read and no box
/// opened; the turn's end on the screen; and what the screen shows of the call's end, as the
/// answer ends one ([`Answer::ends_as`]). None once a check failed.
fn call(
    lab: &Lab,
    door: &Door<'_>,
    c: &mut Checks,
    k: usize,
    answer: Answer,
) -> Result<Option<Shown>, Fail> {
    let turn = k + 1;
    let what = format!("Bash call {k} ({})", answer.word());
    if answer == Answer::Sandboxed {
        let on = J::obj([("sandbox", J::obj([("enabled", J::Bool(true))]))]);
        let reread = match crate::settings_witness::merge_reread(lab, &on) {
            Err(f) if f.stopped.is_some() => return Err(f),
            r => r.is_ok(),
        };
        if !c.check(
            reread,
            &format!("{what}: Claude Code re-read its settings with the sandbox on"),
        ) {
            return Ok(None);
        }
    }
    crate::verbs::set_tool(lab, &tool(k))?;
    if !c.check(
        crate::verbs::turn(door, &format!("toolstart {WORD} {k}"))?,
        &format!("turn {turn} typed"),
    ) {
        return Ok(None);
    }
    if let Some(key) = answer.key() {
        if !c.check(
            door.seen("Do you want to proceed", HANG_S)?,
            &format!("{what}: its approval box drawn"),
        ) {
            return Ok(None);
        }
        let (read, _) = log_until(door, &|l| shell_calls(l) >= k)?;
        if !c.check(
            read,
            &format!("{what}: the transcript's call read before its box is answered"),
        ) {
            return Ok(None);
        }
        door.ctl(&["key", key])?;
    } else {
        let (read, _) = log_until(door, &|l| shell_calls(l) >= k)?;
        if !c.check(read, &format!("{what}: the transcript's call read")) {
            return Ok(None);
        }
    }
    if !c.check(
        door.seen(&answer.ended(k), HANG_S)?,
        &format!("turn {turn} ended ({})", answer.word()),
    ) {
        return Ok(None);
    }
    door.await_idle(1500, HANG_S)?;
    if answer.key().is_none() {
        let (drawn, offered) = (
            boxes(&door.out(&["log", "n=400"])),
            CALLS[..k].iter().filter(|a| a.key().is_some()).count(),
        );
        if !c.check(
            drawn == offered,
            &format!("{what}: no approval box offered ({drawn} opened, {offered} answered)"),
        ) {
            return Ok(None);
        }
    }
    let s = shown(&door.out(&["text"]), k);
    if !c.check(
        s.is_some_and(|s| answer.ends_as(s)),
        &format!(
            "{what}: the screen shows its end as a call {} ends ({})",
            answer.word(),
            s.map_or("no end", Shown::word)
        ),
    ) {
        return Ok(None);
    }
    Ok(s)
}

/// Write `text` to `path` in a run's world.
fn save(path: &Path, text: &str) -> Result<(), Fail> {
    crate::util::write_fresh(path, text).map_err(|e| not_run(format!("{}: {e}", path.display())))
}

/// STEPS 3 AND 8: one session of `build` in a lab of the verb's own (`up`, the folder trusted,
/// the harness off, nothing else of `LAB_*` read), expecting `expect`, the `n`th: its checks
/// ([`drive`], then [`judge_log`] on its session log and what the screen showed of each call's
/// end), and a world holding its session log, its
/// screen and its transcripts (`transcripts/`), kept until the verb releases it; and the session
/// log's checks, when the drive passed. The lab is down when it returns.
/// # Errors
/// The run's interrupt; KEY: the lab's Claude Code says another build; NOT RUN: the lab could not
/// come up or its world be written.
fn session(n: u8, build: &str, expect: Expect) -> Result<(Run, Vec<Check>), Fail> {
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
    let shown = drive(&tag, &mut c).map_err(|f| with_world(f, &world.dir, None))?;
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
        crate::util::copy_regular_file(p, &to)
            .map_err(|e| not_run(format!("{}: {e}", p.display())))?;
    }
    let mut checks = Vec::new();
    if c.fails.is_empty() {
        checks = judge_log(&log, build, expect, &shown);
        for k in &checks {
            c.check(k.ok, &k.what);
        }
    }
    Ok((
        Run {
            world,
            oks: c.oks,
            fails: c.fails,
            fences: Vec::new(),
            took: Some(t.elapsed()),
        },
        checks,
    ))
}

/// STEPS 2 AND 8: the aterm the lab runs ([`crate::paths::Paths::need_aterm`]) is the one the
/// build made, by its path or, for a copy beside a separate compiler build directory,
/// by its bytes ([`same_bytes`]), so the session runs the checkout's code.
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
    let (Ok((mut fa, ma)), Ok((mut fb, mb))) = (
        crate::util::open_regular_file_with_meta(a),
        crate::util::open_regular_file_with_meta(b),
    ) else {
        return false;
    };
    if ma.len() != mb.len() {
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
    admitted: &'a [String],
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
             path: a turn; four Bash calls, one granted once in its approval box, one granted for \
             the session, one declined with Esc and one run sandboxed with no box offered, each \
             call's end read off the screen and held to the transcript's; a turn that meets the \
             stub's five-hour limit) run twice, first expecting the build unmeasured (read partial), then \
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
            "\n**Transcript keys.** It shows {} keys and {} record kinds. None of them is a key the \
             fold reads that the {} measured builds' reductions do not show, and no transcript key \
             the fold reads that {} showed is gone.",
            j.keys,
            j.kinds,
            j.builds,
            j.prev.as_deref().unwrap_or("a previous build")
        ));
        if self.admitted.is_empty() {
            t.push_str(" Nothing new is on a row the fold acts on.");
        } else {
            t.push_str(
                " What is new on rows the fold acts on, and no key it reads, was admitted by run 2, \
                 which wrote rows carrying it and passed every check expecting the build exact:\n\n",
            );
            for a in self.admitted {
                t.push_str(&format!("* {a}\n"));
            }
        }
        if j.noted.is_empty() {
            t.push_str(" No other key, record kind or step shape moved.\n");
        } else {
            t.push_str(
                "\nWhat moved and does not refuse (new on rows the fold does not act on, gone, or \
                 a step shape):\n\n",
            );
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

/// STEP 8's judgment of what step 5 left to run 2 ([`Judged::pending`]): run 2's one transcript,
/// reduced as run 1's was, its privacy check included ([`reduce_session`]), and each new key
/// admitted by the row of run 2's that carries it and the check of `checks` (run 2's, every one
/// passed) that read the step the fold made of that row ([`exercised`], [`check_of_row`]).
/// # Errors
/// TRANSCRIPTS, KEY, FIXED-POINT or TEXT on run 2's transcript; NEW-KEY: run 2 exercised one not.
fn admitted_by(
    build: &str,
    judged: &Judged,
    run2: &Run,
    checks: &[Check],
) -> Result<Vec<String>, Refusal> {
    if judged.pending.is_empty() {
        return Ok(Vec::new());
    }
    let from = one_transcript(&run2.world.dir.join("transcripts"))?;
    let source = crate::util::read_regular_text(&from).map_err(|e| {
        Refusal::new(
            Class::Transcripts,
            format!("{LEG}: {}: {e}", from.display()),
        )
    })?;
    let text = reduce_session(&source, build, &reduce_checked(Program::Claude))?;
    exercised(
        &CLAUDE,
        build,
        &judged.pending,
        RUN2,
        &[(LEG, text)],
        &|_, text, row| check_of_row(checks, text, row),
    )
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
    let (mut run1, _) = session(1, &build, Expect::Partial)?;
    println!("claude-measure: 3. {}", run_line(1, Expect::Partial, &run1));
    gate(Class::ProofRed, &run1)?;
    let world1 = run1.world.dir.clone();
    let from = one_transcript(&world1.join("transcripts")).map_err(kept(&world1))?;
    let source = crate::util::read_regular_text(&from).map_err(|e| {
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
        "claude-measure: 5. {} keys and {} record kinds, no key the fold reads new against {} measured builds' reductions, no transcript key gone against {}; {}{}",
        judged.keys,
        judged.kinds,
        judged.builds,
        judged.prev.as_deref().unwrap_or("no previous build"),
        if judged.pending.is_empty() {
            "nothing new on a row the fold acts on".to_string()
        } else {
            format!(
                "left to run 2 to exercise: {}",
                judged
                    .pending
                    .iter()
                    .map(crate::measure::Pending::said)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
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
        let (run2, checks2) =
            session(2, &build, Expect::On).map_err(|f| with_world(f, &world1, None))?;
        println!("claude-measure: 8. {}", run_line(2, Expect::On, &run2));
        gate(Class::NotBound, &run2).map_err(|f| with_world(f, &world1, None))?;
        let admitted = admitted_by(&build, &judged, &run2, &checks2)
            .map_err(|r| with_world(r.into(), &world1, Some(&run2.world.dir)))?;
        for a in &admitted {
            println!("claude-measure: 8. admitted {a}");
        }
        let note = Note {
            build: &build,
            bin_said: &bin_said,
            date: &date,
            head: (head, tree),
            runs: [&run1, &run2],
            text: &text,
            judged: &judged,
            admitted: &admitted,
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
