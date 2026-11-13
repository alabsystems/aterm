// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TAPE CORPUS'S AUTOPILOT SCENES (`lab scn run c5-land` …, kept with `lab tape promote`):
//! Land and Cruise on a real Claude Code against the stub, the conversation scripted turn by turn
//! as the forever lab scripts its chains ([`crate::forever`]), in a repository laid in the lab's
//! own project, run until the act or the hold the scene is kept for is journaled.

use std::path::Path;

use aterm_agent::supervise::journal::Word;
use aterm_agent::supervise::policy::go_ahead::{
    GO_AHEAD_ANSWER, GO_AHEAD_DROP, RULE_GO_AHEAD, TOKEN_CLOSE, TOKEN_OPEN,
};
use aterm_agent::supervise::policy::needs::NEEDS_YOU;
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_agent::supervise::policy::turn_end::{
    RULE_CONTINUE, RULE_DONE_CHECK, RULE_LAND, TurnEndTiming,
};

use crate::forever::{self, Chain, Repo};
use crate::lab::Launch;
use crate::pyjson::J;
use crate::record::{rec_s, rec_t};
use crate::scn::{Ctx, LabKw, ScnErr};
use crate::screen::field;
use crate::stop;
use crate::util::now;

/// One scene the corpus keeps a tape of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    /// Land's landing, land-a's L1 and L5: a work reply, `land@v1`; a clean defense in a clean
    /// repository, the land check (`land-check@v1`) at its point, its `LANDED`, landed (`DONE …
    /// rule=land@v1`). Its conversation is land-c's (whose replies land-a's are but for L3's
    /// `NEEDS YOU:`), in land-a's clean repository: a nudge carries the need's acknowledgement,
    /// whose words have moved since `b42ae72fc3`; the corpus's Land tapes replayed on that tree's
    /// policy until Land's instruction stopped waiting the back-off (v6 §14.10).
    Landing,
    /// Land's repository hold, land-c's: one untracked file beside a pushed `main`. A clean
    /// defense, a nudge naming the file, the same defense again: held (`kind=stalled`).
    RepoHold,
    /// Cruise recovers from repeated replies: after its done checks it changes approach
    /// with a continuation instead of asking the person to resume it.
    SameReply,
    /// Cruise over Claude Code's grey suggestion: a work reply after a tool call, Cruise's whole
    /// continuation (`continue@v1`) typed over its suggestion, never the suggestion accepted bare
    /// (v6 §14.5); a done claim, the done check typed over the suggestion it drew again; the
    /// defense, and the next point decided. 2.1.310 draws the suggestion's first glyph inverse,
    /// under the caret it draws itself, and the rest dim.
    Placeholder,
    /// THE INCIDENT'S PARK UNDER CRUISE (DESIGN-harness-autopilot §15.1, §15.5): the first reply
    /// says the parser is done and ends on a NEEDS YOU line asking the owner to say "rerun the
    /// checks" so the agent can build, test, push its own branch to main and remove its worktree
    /// ([`park_reply`]), and no key of a person's is typed in the scene (the first prompt is a
    /// control write, which stamps none). Kept for: the owner's standing go-ahead (`go-ahead@v1`)
    /// typed once, its ledger row the answer's words with the word asked for and
    /// [`GO_AHEAD_DROP`]; the real Claude Code taking that answer as one turn, whole
    /// ([`one_turn_whole`]: the request after the park's opens with it, and the stub's log names
    /// the rule that answered that request as the one for a last message carrying
    /// [`GO_AHEAD_DROP`], which answers [`GO_ON`]); [`GO_ON`] drawn, the work going on with the
    /// line dropped; and Cruise's own continuation (`continue@v1`) as the next act, no second
    /// go-ahead. Throughout, the tab's `status needs=` and `needs_n=` never show the answered line
    /// (§15.6), read once a second. It measures what a real Claude Code does with the typed
    /// answer as a request; it reads no frame for how the TUI drew the prompt, and it cannot
    /// measure what a real model does with the answer, because the model here is the stub, whose
    /// replies are scripted.
    GoAhead,
    /// The hinted land check, land-hinted's (LH): a work reply, `land@v1`; a clean defense whose
    /// clause the word reader reads open, the hinted check quoting it, its `LANDED` read off a
    /// real Claude Code's screen and transcript, landed.
    Hinted,
}

impl Scene {
    /// Every scene, in the corpus's order.
    pub const ALL: [Scene; 6] = [
        Scene::Landing,
        Scene::RepoHold,
        Scene::SameReply,
        Scene::Placeholder,
        Scene::GoAhead,
        Scene::Hinted,
    ];

    /// Its scenario's name, and its corpus tape's (`lab tape rerecord <name>`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scene::Landing => "c5-land",
            Scene::RepoHold => "c6-land-repo",
            Scene::SameReply => "c7-cruise-same",
            Scene::Placeholder => "c8-cruise-placeholder",
            Scene::GoAhead => "c9-cruise-go-ahead",
            Scene::Hinted => "c10-land-hinted",
        }
    }

    /// Its `[harness]` lines: the Autopilot every session of its lab starts under.
    #[must_use]
    pub const fn harness_keys(self) -> &'static str {
        match self {
            Scene::Landing | Scene::RepoHold | Scene::Hinted => "autopilot = \"land\"",
            Scene::SameReply | Scene::Placeholder | Scene::GoAhead => "autopilot = \"cruise\"",
        }
    }

    /// What its repository holds beside its pushed `main`.
    #[must_use]
    pub const fn repo(self) -> Repo {
        match self {
            Scene::RepoHold => Repo::Untracked,
            Scene::Landing
            | Scene::SameReply
            | Scene::Placeholder
            | Scene::GoAhead
            | Scene::Hinted => Repo::Clean,
        }
    }

    /// The forever chain whose conversation it plays, word for word.
    const fn chain(self) -> Option<Chain> {
        match self {
            Scene::Landing | Scene::RepoHold => Some(Chain::LandC),
            Scene::SameReply => Some(Chain::Cruise),
            Scene::Hinted => Some(Chain::LandHinted),
            Scene::Placeholder | Scene::GoAhead => None,
        }
    }

    /// The word a scene of no chain carries.
    const fn own_token(self) -> &'static str {
        match self {
            Scene::GoAhead => GO_AHEAD_TOKEN,
            _ => PLACEHOLDER_TOKEN,
        }
    }

    /// The word its conversation carries, by which the stub's rules know it.
    #[must_use]
    pub fn token(self) -> String {
        self.chain()
            .map_or_else(|| self.own_token().to_string(), Chain::token)
    }

    /// Its first prompt, typed into the composer as a person would.
    #[must_use]
    pub fn prompt(self) -> String {
        self.chain().map_or_else(
            || {
                format!(
                    "{} write the parser for this repository and land it",
                    self.own_token()
                )
            },
            Chain::prompt,
        )
    }

    /// Its stub mode's spec ([`crate::stub::scripted_reply`]); `readme` the project's
    /// `README.md`, which the placeholder scene's first reply reads.
    #[must_use]
    pub fn spec(self, readme: &Path) -> J {
        match (self.chain(), self) {
            (Some(chain), _) => forever::spec(&[chain]),
            (None, Scene::GoAhead) => go_ahead_spec(),
            (None, _) => placeholder_spec(readme),
        }
    }

    /// How its Claude Code is launched: lean ([`LEAN`]), and with prompt suggestions on for the
    /// placeholder scene, which this build drafts only where a served flag or this variable says
    /// so.
    #[must_use]
    pub fn launch(self) -> Launch {
        Launch {
            args: LEAN.iter().map(|a| (*a).to_string()).collect(),
            envs: if self == Scene::Placeholder {
                vec![format!("{SUGGESTIONS_ON}=1")]
            } else {
                Vec::new()
            },
            ..Launch::default()
        }
    }

    /// Its lab: the instance under [`INSTANCE_ENV`].
    #[must_use]
    pub fn lab_kw(self) -> LabKw {
        LabKw {
            instance_env: INSTANCE_ENV,
            ..LabKw::default()
        }
    }

    /// Whether its end is a landing or a hold: a quiet the corpus pins, its tape run on past the
    /// act a policy that did not land or hold would take next ([`settle_after`]).
    #[must_use]
    pub const fn ends_quiet(self) -> bool {
        !matches!(self, Scene::SameReply | Scene::Placeholder | Scene::GoAhead)
    }

    /// How long its end may take after the first prompt: the scaled back-off's first four steps
    /// (15, 30, 60 and 120 s; no scene waits out more than four) and two minutes more.
    #[must_use]
    pub fn within_s(self) -> f64 {
        let step = TurnEndTiming::default()
            .ladder_scaled(forever::LADDER_SCALE)
            .short_backoff
            .as_secs_f64();
        step * f64::from(1 + 2 + 4 + 8) + 120.0
    }
}

/// The word the placeholder scene's conversation carries.
const PLACEHOLDER_TOKEN: &str = "CORPUSLAB-CRUISE-PLACEHOLDER.";

/// The instance environment of every scene's lab: the turn-end back-off ladder scaled to an
/// eighth ([`forever::ladder_env`], a seam only a development build reads), so a scene of three or
/// four turn ends waits seconds rather than minutes, and its tape holds that many fewer of the
/// loop's `status` looks. The tape records the timing its loop ran under, and a replay keeps it.
pub const INSTANCE_ENV: &str = "ATERM_DEV_LADDER_SCALE=0.125";

/// THE LEAN LAUNCH: a one-line system prompt, `Read` the one tool, no skills and no MCP server.
/// Claude Code 2.1.310 writes its whole prompt into its transcript (`prompt_snapshot`,
/// `api-request-shape`: about 165 KB after a turn at its defaults, 15 KB lean), and the loop
/// reads the transcript's tail at each decision, so a scene of four or more decisions keeps its
/// tape under the corpus's 512 KiB only lean. What the loop judges, the screen and the replies,
/// is the same.
pub const LEAN: [&str; 6] = [
    "--system-prompt",
    "You are the lab's agent.",
    "--tools",
    "Read",
    "--disable-slash-commands",
    "--strict-mcp-config",
];

/// Claude Code's variable that turns its prompt suggestions on where no served flag does
/// (2.1.310: `CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION`, read before the `tengu_chomp_inflection`
/// flag, which the stub never serves).
pub const SUGGESTIONS_ON: &str = "CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION";

/// The opening of the request Claude Code sends to draft its grey suggestion after a turn
/// (2.1.310): the conversation so far, then this as the last user message.
pub const SUGGESTION_KEY: &str = "[SUGGESTION MODE:";

/// The suggestion the stub drafts: a continuation a build before v6's 5a accepted bare
/// (`turn_end::suggestion_allowed`), which Cruise now types its own words over.
pub const SUGGESTION: &str = "keep going";

/// The placeholder scene's spec: the suggestion request answered with [`SUGGESTION`] at once;
/// the done check with the clean defense; Cruise's continuation typed over the suggestion (whose
/// words hold it, as an accepted suggestion's did) with the done claim; the first prompt with a
/// read of `readme` and the work reply,
/// so that its turn holds the two replies Claude Code drafts a suggestion after. (It drafts none
/// after a reply that cost more than 10,000 tokens, which the stub counts from the request's
/// size: the lean launch's requests stay well under.)
#[must_use]
pub fn placeholder_spec(readme: &Path) -> J {
    let rule = |last: &str, text: &str| {
        J::obj([
            ("when", J::s(PLACEHOLDER_TOKEN)),
            ("last", J::s(last)),
            ("text", J::s(text)),
        ])
    };
    let mut suggest = rule(SUGGESTION_KEY, SUGGESTION);
    suggest.set("delay_ms", J::Int(0));
    suggest.set("pace_s", J::Float(0.0));
    let mut first = rule("", forever::WORK);
    first.set("tool", forever::read_call(forever::Harness::Claude, readme));
    let mut sp = forever::spec(&[]);
    sp.set(
        "replies",
        J::Arr(vec![
            suggest,
            rule(forever::CHECK_KEY, forever::CLEAN),
            rule(SUGGESTION, forever::DONE_CLAIM),
            first,
        ]),
    );
    sp
}

/// The word the go-ahead scene's conversation carries.
const GO_AHEAD_TOKEN: &str = "CORPUSLAB-CRUISE-GO-AHEAD.";

/// The word the incident's park asked the owner to say (§15.1).
pub const PARK_WORD: &str = "rerun the checks";

/// THE GO-AHEAD SCENE'S PARK, the incident's last reply (§15.1) on the lab's parser: a done
/// report, the build waiting only for the person's go-ahead, and a NEEDS YOU line (its head the
/// harness's own [`NEEDS_YOU`]) asking the owner to say [`PARK_WORD`] for the agent's own build,
/// test, push to main and worktree cleanup.
#[must_use]
pub fn park_reply() -> String {
    format!(
        "The parser is done and committed on its branch. Nothing has changed since: the build is \
         waiting only for your go-ahead.\n\n{NEEDS_YOU} say \"{PARK_WORD}\" so I can build and \
         test the parser branch, push it to main and remove the worktree."
    )
}

/// The stub's reply to the go-ahead: the work goes on, and names no need.
pub const GO_ON: &str =
    "Reran the checks: the parser branch builds and its tests pass. Pushing it to main next.";

/// The go-ahead scene's spec: the go-ahead (a last message carrying [`GO_AHEAD_DROP`]) answered
/// with [`GO_ON`]; a done check, should the run reach one, with the clean defense; Cruise's
/// continuation with the done claim; the first prompt with [`park_reply`].
#[must_use]
pub fn go_ahead_spec() -> J {
    let rule = |last: &str, text: &str| {
        J::obj([
            ("when", J::s(GO_AHEAD_TOKEN)),
            ("last", J::s(last)),
            ("text", J::s(text)),
        ])
    };
    let mut sp = forever::spec(&[]);
    sp.set(
        "replies",
        J::Arr(vec![
            rule(GO_AHEAD_DROP, GO_ON),
            rule(forever::CHECK_KEY, forever::CLEAN),
            rule(forever::CONTINUE_KEY, forever::DONE_CLAIM),
            rule("", &park_reply()),
        ]),
    );
    sp
}

/// The opening of the go-ahead's words by which the stub's request log names its turn: the
/// harness's mark and the answer's first words, inside the 70 characters the log keeps of a
/// request's last user message.
fn go_ahead_head() -> String {
    GO_AHEAD_ANSWER.chars().take(GO_AHEAD_HEAD_CHARS).collect()
}

/// How many characters of [`GO_AHEAD_ANSWER`] [`go_ahead_head`] takes: `[aterm harness] The
/// owner's standing go-ahead`.
const GO_AHEAD_HEAD_CHARS: usize = 45;

/// The verb a journal line opens with (`DONE`, `EXCEPTION`, `CONTINUED` …).
fn verb(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or_default()
}

/// Whether `line` carries `word` as one of its words (`rule=land@v1`, `kind=stalled`).
fn says(line: &str, word: &str) -> bool {
    line.split_whitespace().any(|w| w == word)
}

/// The rule a `CONTINUED` line typed under, or `None` for any other line.
#[must_use]
pub fn continued_rule(line: &str) -> Option<&str> {
    (verb(line) == "CONTINUED")
        .then(|| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix("rule="))
        })
        .flatten()
}

/// The words a `CONTINUED` line says it typed: what follows its rule.
fn continued_words(line: &str) -> &str {
    line.split_once(" rule=")
        .and_then(|(_, rest)| rest.split_once(' '))
        .map_or("", |(_, words)| words)
}

/// Whether a composer showing `shown` is `words` being typed: their openings agree.
fn typing(shown: &str, words: &str) -> bool {
    let shown = shown.trim();
    let head: String = words.chars().take(16).collect();
    !shown.is_empty() && !head.is_empty() && (shown.starts_with(&head) || head.starts_with(shown))
}

/// A line that says a turn's end was decided: an act, a wait, a landing, a hold.
fn decides(line: &str) -> bool {
    matches!(
        verb(line),
        "CONTINUED" | "WAITING" | "DONE" | "EXCEPTION" | "SKIPPED" | "LEFT" | "ESCALATED"
    )
}

/// A line that says a turn's end was acted on or held ([`decides`], but no wait and no skip).
fn acts(line: &str) -> bool {
    decides(line) && !matches!(verb(line), "WAITING" | "SKIPPED")
}

/// THE SCENE'S END in the journal's `lines` (oldest first): the index of the line the scene is
/// kept for, or `None` while it has not come.
#[must_use]
pub fn end_of(scene: Scene, lines: &[String]) -> Option<usize> {
    let stalled = format!("kind={}", ExceptionKind::Stalled.word());
    let landed = format!("rule={RULE_LAND}");
    match scene {
        Scene::Landing | Scene::Hinted => lines
            .iter()
            .position(|l| verb(l) == "DONE" && says(l, &landed)),
        Scene::RepoHold => lines
            .iter()
            .position(|l| verb(l) == "EXCEPTION" && says(l, &stalled)),
        Scene::SameReply => {
            let checked = lines
                .iter()
                .position(|l| continued_rule(l) == Some(RULE_DONE_CHECK))?;
            lines[checked + 1..]
                .iter()
                .position(|l| continued_rule(l) == Some(RULE_CONTINUE))
                .map(|at| checked + 1 + at)
        }
        Scene::Placeholder => {
            let continued = lines
                .iter()
                .position(|l| continued_rule(l) == Some(RULE_CONTINUE))?;
            let checked = continued
                + 1
                + lines[continued + 1..]
                    .iter()
                    .position(|l| continued_rule(l) == Some(RULE_DONE_CHECK))?;
            lines[checked + 1..]
                .iter()
                .position(|l| decides(l))
                .map(|at| checked + 1 + at)
        }
        Scene::GoAhead => {
            let answered = lines
                .iter()
                .position(|l| continued_rule(l) == Some(RULE_GO_AHEAD))?;
            lines[answered + 1..]
                .iter()
                .position(|l| acts(l))
                .map(|at| answered + 1 + at)
        }
    }
}

/// Lays the scene's repository in the lab's project, where its tab starts, and sets its stub
/// mode: what `scn run` does before it launches Claude Code there.
/// # Errors
/// A git step or the stub's mode that fails.
pub fn prepare(ctx: &Ctx, scene: Scene) -> Result<Launch, ScnErr> {
    let run = &ctx.run;
    let proj = Path::new(&run.proj);
    let bare = Path::new(&run.sockdir).join("origin.git");
    let dir = forever::lay_repo_at(run, proj, &bare, &scene.token(), scene.repo())
        .map_err(|e| ScnErr::Failed(format!("the scene's repository: {e}")))?;
    crate::verbs::set_custom_mode(run, forever::MODE, &scene.spec(&dir.join("README.md")))?;
    Ok(scene.launch())
}

/// Plays `scene` on a lab whose Claude Code shows its composer: types the first prompt as `lab
/// turn` does ([`Ctx::composer_turn`]; one the composer did not take is an `error`) and waits for the scene's end ([`end_of`]), then a moment more for the loop to settle on it.
/// What the loop typed, the end's line and (for the placeholder) the composers its continuation
/// and its done check were typed over go into the summary; a scene that does not reach its end,
/// a placeholder scene whose acts were not typed over the suggestion, or a go-ahead scene that
/// falls short of what it is kept for ([`go_ahead_kept`]), puts an `error`, which `tape promote`
/// refuses. The go-ahead scene reads the tab's `status` once a second from its first prompt to
/// the end of its settle ([`needs_read`]).
/// # Errors
/// Interrupted.
pub fn play(ctx: &mut Ctx, scene: Scene) -> Result<(), ScnErr> {
    let from = ctx.journal().len();
    let watch = scene == Scene::GoAhead;
    let mut needs = NeedsWatch::default();
    if let Some(why) = ctx.composer_turn(&scene.prompt())? {
        ctx.put(
            "error",
            format!("{}: its first prompt: {why}", scene.name()),
        );
        return Ok(());
    }
    let by = now() + scene.within_s();
    let end = loop {
        if watch {
            needs.read(ctx);
        }
        let lines: Vec<String> = ctx
            .journal()
            .iter()
            .skip(from)
            .map(|(_, r)| rec_s(r, "line"))
            .collect();
        if let Some(at) = end_of(scene, &lines) {
            break Some((at, lines));
        }
        if now() >= by {
            break None;
        }
        stop::sleep(std::time::Duration::from_secs(1))?;
    };
    let journal = ctx.journal();
    let typed: Vec<J> = journal
        .iter()
        .skip(from)
        .filter_map(|(_, r)| {
            let line = rec_s(r, "line");
            continued_rule(&line).map(J::s)
        })
        .collect();
    ctx.put("typed", J::Arr(typed));
    let Some((at, lines)) = end else {
        ctx.put(
            "error",
            format!(
                "{}: its end was not journaled within {:.0} s of the first prompt",
                scene.name(),
                scene.within_s()
            ),
        );
        return Ok(());
    };
    ctx.mark(&format!("scene end: {}", lines[at]), None);
    ctx.put("end", J::s(lines[at].as_str()));
    if scene == Scene::Placeholder {
        for (rule, what, key) in [
            (RULE_CONTINUE, "continuation", "composer_at_continue"),
            (RULE_DONE_CHECK, "done check", "composer_at_check"),
        ] {
            let typed = journal.iter().skip(from).find_map(|(w, r)| {
                let line = rec_s(r, "line");
                (continued_rule(&line) == Some(rule)).then(|| {
                    let t = rec_t(r);
                    (
                        if t > 0.0 { t } else { *w },
                        continued_words(&line).to_string(),
                    )
                })
            });
            let over = typed.and_then(|(t, words)| {
                ctx.frames()
                    .iter()
                    .filter(|f| f.wall < t)
                    .filter_map(crate::record::Frame::comp)
                    .rfind(|c| !typing(c, &words))
            });
            if !over_suggestion(over.as_deref()) {
                ctx.put(
                    "error",
                    format!(
                        "{}: its {what} was typed over {over:?}, not the suggestion {SUGGESTION:?}",
                        scene.name()
                    ),
                );
            }
            ctx.put(key, over.map_or(J::Null, J::s));
        }
    }
    // The completed recovery act is this scene's end. A further reply starts another
    // turn and adds its entire transcript without proving anything about that act.
    let settle = if scene == Scene::SameReply {
        std::time::Duration::ZERO
    } else {
        settle_after(scene.ends_quiet(), &lines[..at])
    };
    ctx.put("settle_s", J::Float(settle.as_secs_f64()));
    if !watch {
        stop::sleep(settle)?;
        return Ok(());
    }
    let until = now() + settle.as_secs_f64();
    while now() < until {
        needs.read(ctx);
        stop::sleep(std::time::Duration::from_secs(1))?;
    }
    let wrong = go_ahead_kept(ctx, from, &lines[at], &needs);
    if !wrong.is_empty() {
        ctx.put("error", format!("{}: {}", scene.name(), wrong.join("; ")));
    }
    Ok(())
}

/// The tab's shown needs, as the go-ahead scene reads them from `status` once a second.
#[derive(Debug, Default)]
struct NeedsWatch {
    /// How many reads answered with the two fields.
    reads: u32,
    /// Each reading that showed a need ([`shows_need`]), once.
    shown: Vec<String>,
}

impl NeedsWatch {
    fn read(&mut self, ctx: &Ctx) {
        if let Some(read) = needs_read(&ctx.ctl(&["status"]).stdout) {
            self.reads += 1;
            if shows_need(&read) && !self.shown.contains(&read) {
                self.shown.push(read);
            }
        }
    }
}

/// The tab's shown needs in a `status` reply, as `needs=<word> needs_n=<n>`; `None` where the
/// reply names neither (a refused or failed read).
fn needs_read(status: &str) -> Option<String> {
    let needs = field(status, "needs")?;
    let n = field(status, "needs_n")?;
    Some(format!("needs={needs} needs_n={n}"))
}

/// Whether a [`needs_read`] reading shows a need: anything but none of either.
fn shows_need(read: &str) -> bool {
    read != "needs=- needs_n=-"
}

/// WHAT THE GO-AHEAD SCENE IS KEPT FOR, read once its settle is over: each way the run fell short,
/// empty where it did not. `from` is the journal's length before the first prompt, `end` the
/// scene's end ([`end_of`]), `needs` what `status` showed. Kept: the go-ahead typed once (one
/// `go-ahead@v1` journal line, one ledger row `typed` with [`GO_AHEAD_ANSWER`]'s words, the word
/// asked for and [`GO_AHEAD_DROP`]); one request of Claude Code's whose last message opens with
/// the go-ahead ([`go_ahead_head`]), the request after the park's, which the stub answered by its
/// rule for a last message carrying [`GO_AHEAD_DROP`] ([`one_turn_whole`]), whose received text
/// digest matches the complete intended ledger command: it took the answer as one turn, whole;
/// [`GO_ON`] drawn; Cruise's continuation the end; no need shown on a `status`
/// read, and some read answered; and no key of a person's on the tab (`human_typed_ms=-`,
/// `human_ms=-`). What each read gives goes into the summary, the stub's `msgs=` and `reply=` for
/// each of Claude Code's requests among them (`stub_replies`).
fn go_ahead_kept(ctx: &mut Ctx, from: usize, end: &str, needs: &NeedsWatch) -> Vec<String> {
    let mut wrong = Vec::new();
    let journal: Vec<String> = ctx
        .journal()
        .iter()
        .skip(from)
        .map(|(_, r)| rec_s(r, "line"))
        .collect();
    let answered = journal
        .iter()
        .filter(|l| continued_rule(l) == Some(RULE_GO_AHEAD))
        .count();
    if answered != 1 {
        wrong.push(format!(
            "{answered} {RULE_GO_AHEAD} journal line(s), not one"
        ));
    }
    let (_, ledger) = forever::records(&ctx.run, &ctx.sid);
    let rows: Vec<&forever::Row> = ledger.iter().filter(|r| r.rule == RULE_GO_AHEAD).collect();
    let word = format!("{TOKEN_OPEN}{PARK_WORD}{TOKEN_CLOSE}");
    match rows.as_slice() {
        [row]
            if row.decision == "typed"
                && row.command.starts_with(GO_AHEAD_ANSWER)
                && row.command.contains(&word)
                && row.command.contains(GO_AHEAD_DROP) => {}
        _ => wrong.push(format!(
            "the ledger's {RULE_GO_AHEAD} rows are not one typed answer with {word:?} and \
             {GO_AHEAD_DROP:?}: {:?}",
            rows.iter()
                .map(|r| format!("{} {}", r.decision, r.command))
                .collect::<Vec<_>>()
        )),
    }
    ctx.put(
        "go_ahead_typed",
        J::Arr(rows.iter().map(|r| J::s(r.command.as_str())).collect()),
    );
    let main = crate::scn::stub_main_lines(ctx);
    let turns = main
        .as_deref()
        .map(|lines| lines.iter().filter(|l| opens_go_ahead(l)).count());
    match (main.as_deref(), rows.as_slice()) {
        (Some(lines), [row]) => {
            if let Err(why) = one_turn_whole(lines, &row.command) {
                wrong.push(why);
            }
        }
        (None, _) => wrong.push("no stub log to read Claude Code's requests from".to_string()),
        // The ledger mismatch above already refused an absent or ambiguous intended command.
        (Some(_), _) => {}
    }
    ctx.put(
        "go_ahead_turns",
        turns.map_or(J::Null, |n| J::Int(i64::try_from(n).unwrap_or(i64::MAX))),
    );
    ctx.put(
        "stub_replies",
        J::Arr(
            main.iter()
                .flatten()
                .map(|l| {
                    J::s(format!(
                        "msgs={} reply={}",
                        crate::scn::word_of(l, "msgs=").unwrap_or("-"),
                        crate::scn::word_of(l, "reply=").unwrap_or("-")
                    ))
                })
                .collect(),
        ),
    );
    let go_on: String = GO_ON.chars().take(GO_ON_HEAD_CHARS).collect();
    let drawn = ctx
        .frames()
        .iter()
        .any(|f| f.rows.iter().any(|r| r.contains(&go_on)));
    if !drawn {
        wrong.push(format!("no frame drew {go_on:?}"));
    }
    ctx.put("go_on_drawn", J::Bool(drawn));
    if continued_rule(end) != Some(RULE_CONTINUE) {
        wrong.push(format!(
            "after the go-ahead the loop took {end:?}, not {RULE_CONTINUE}"
        ));
    }
    ctx.put("needs_reads", J::Int(i64::from(needs.reads)));
    ctx.put(
        "needs_shown",
        J::Arr(needs.shown.iter().map(|s| J::s(s.as_str())).collect()),
    );
    if needs.reads == 0 {
        wrong.push("no status read named needs= and needs_n=".to_string());
    }
    if !needs.shown.is_empty() {
        wrong.push(format!("status showed a need: {:?}", needs.shown));
    }
    let status = ctx.ctl(&["status"]).stdout;
    for key in ["human_typed_ms", "human_ms"] {
        let value = field(&status, key);
        if value.as_deref() != Some("-") {
            wrong.push(format!("{key}={value:?}: a person's key on the tab"));
        }
        ctx.put(key, value.map_or(J::Null, J::Str));
    }
    wrong
}

/// Whether a request of the stub's log ([`crate::scn::stub_main_lines`]) opens its last message
/// with the go-ahead ([`go_ahead_head`]).
fn opens_go_ahead(line: &str) -> bool {
    crate::scn::last_user_of(line).is_some_and(|u| u.starts_with(&go_ahead_head()))
}

/// The index of the go-ahead spec's reply rule whose `last` is `last` (`""`: the catch-all that
/// answers [`park_reply`]), as the stub's log names it (`reply=`, [`crate::stub::scripted_rule`]).
fn spec_rule(last: &str) -> Option<usize> {
    go_ahead_spec()
        .get("replies")
        .and_then(J::as_arr)?
        .iter()
        .position(|r| r.get("last").and_then(J::as_str) == Some(last))
}

/// WHETHER CLAUDE CODE TOOK THE GO-AHEAD AS ONE TURN, WHOLE, read off the stub's main requests
/// (`lines`, in order): exactly one opens its last message with the go-ahead
/// ([`opens_go_ahead`]), the stub answered that one by the rule that answers only a last message
/// carrying [`GO_AHEAD_DROP`] (the answer's last sentence), the request before it is one the
/// stub answered with [`park_reply`], and the complete received text's digest matches `intended`
/// from the typed ledger row ([`crate::stub::message_digest`]). No middle may be omitted while
/// the two ends survive. `Err` says how it fell short.
fn one_turn_whole(lines: &[String], intended: &str) -> Result<(), String> {
    let head = go_ahead_head();
    let at: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| opens_go_ahead(l))
        .map(|(i, _)| i)
        .collect();
    let [at] = at[..] else {
        return Err(format!(
            "Claude Code's requests whose last message opens with {head:?}: {}, not one",
            at.len()
        ));
    };
    let reply =
        |i: usize| crate::scn::word_of(&lines[i], "reply=").and_then(|r| r.parse::<usize>().ok());
    let (drop, park) = (spec_rule(GO_AHEAD_DROP), spec_rule(""));
    if drop.is_none() || reply(at) != drop {
        return Err(format!(
            "the request opening with {head:?} was answered by reply rule {:?}, not the one for \
             a last message carrying {GO_AHEAD_DROP:?} ({drop:?}): no request was shown to \
             hold the answer whole",
            reply(at)
        ));
    }
    let before = at.checked_sub(1).and_then(reply);
    if park.is_none() || before != park {
        return Err(format!(
            "the request before the go-ahead's was answered by reply rule {before:?}, not the \
             park's ({park:?})"
        ));
    }
    let expected = crate::stub::message_digest(intended);
    let received = crate::scn::word_of(&lines[at], "last_user_sha256=");
    if received != Some(expected.as_str()) {
        return Err(format!(
            "the go-ahead request's received text digest {received:?} does not match its complete \
             intended ledger command {expected}: the answer was not received whole"
        ));
    }
    Ok(())
}

/// How many characters of [`GO_ON`] a frame must draw on one row for it to be drawn: its opening,
/// which no width the lab draws at wraps.
const GO_ON_HEAD_CHARS: usize = 28;

/// Whether the composer a done check was typed over (`None`: none was read) is the stub's grey
/// suggestion, the one the placeholder scene is kept for.
fn over_suggestion(composer: Option<&str>) -> bool {
    composer.is_some_and(|c| c.trim() == SUGGESTION)
}

/// HOW LONG A SCENE RUNS ON PAST ITS END before the loop is ended. A landing or a hold (`quiet`)
/// runs past the act a policy that did not land or hold would take next: the scaled ladder's
/// back-off after the short turns its journal's `lines` before the end waited out (each a
/// `WAITING`) and the one it ended on, and [`QUIET_MARGIN_S`] more, so that act's moment is on
/// the tape and a build that would type there takes an act its replay never took. Any other end
/// runs [`SETTLE_S`] more, for the loop's next look.
#[must_use]
pub fn settle_after(quiet: bool, lines: &[String]) -> std::time::Duration {
    if !quiet {
        return std::time::Duration::from_secs(SETTLE_S);
    }
    let waited = lines
        .iter()
        .filter(|l| Word::of_printed(verb(l)) == Some(Word::Waiting))
        .count();
    let streak = u32::try_from(waited).unwrap_or(u32::MAX).saturating_add(1);
    TurnEndTiming::default()
        .ladder_scaled(forever::LADDER_SCALE)
        .short_wait_at(streak)
        .unwrap_or_default()
        + std::time::Duration::from_secs(QUIET_MARGIN_S)
}

/// How long a scene that is not kept for a quiet runs on past its end: the loop's next look.
const SETTLE_S: u64 = 10;

/// How long a landing or a hold runs on past the moment its next act would have been typed.
const QUIET_MARGIN_S: u64 = 20;

#[cfg(test)]
#[path = "autopilot_scenes_tests.rs"]
mod tests;
