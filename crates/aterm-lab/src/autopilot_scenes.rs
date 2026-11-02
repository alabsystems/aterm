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

/// Plays `scene` on a lab whose Claude Code shows its composer: types the first prompt and
/// waits for the scene's end ([`end_of`]), then a moment more for the loop to settle on it.
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
    ctx.turn(&scene.prompt())?;
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
mod tests {
    use super::*;
    use aterm_agent::supervise::config::CONTINUE_TEXT;
    use aterm_agent::supervise::policy::turn_end::{
        DONE_CHECK, RULE_SUGGESTION, done_check_words, suggestion_allowed,
    };

    fn reply<'a>(spec: &'a J, users: &[&str]) -> (&'a str, Option<&'a J>) {
        let users: Vec<String> = users.iter().map(|u| (*u).to_string()).collect();
        let rule = crate::stub::scripted_reply(spec, &users).expect("a rule answers");
        (
            rule.get("text").and_then(J::as_str).expect("a text"),
            rule.get("tool"),
        )
    }

    /// The opening of the suggestion request Claude Code 2.1.310 sends (its binary's prompt).
    const SUGGESTION_ASKED: &str = "[SUGGESTION MODE: Suggest what the user might naturally \
                                    type next into Claude Code.]\nFIRST: Look at the user's \
                                    recent messages and original request.";

    #[test]
    fn the_placeholder_scene_answers_each_request_as_its_turn_needs() {
        let readme = Path::new("/lab/proj/README.md");
        let spec = Scene::Placeholder.spec(readme);
        let prompt = Scene::Placeholder.prompt();
        let (text, tool) = reply(&spec, &[&prompt]);
        assert_eq!(text, forever::WORK);
        assert_eq!(
            tool,
            Some(&forever::read_call(forever::Harness::Claude, readme)),
            "the first turn reads, so it holds the two replies a suggestion needs"
        );
        let (text, tool) = reply(&spec, &[&prompt, SUGGESTION_ASKED]);
        assert_eq!((text, tool), (SUGGESTION, None));
        assert!(
            suggestion_allowed(text),
            "a build before 5a accepted the suggestion bare"
        );
        for typed in [SUGGESTION, CONTINUE_TEXT] {
            assert_eq!(reply(&spec, &[&prompt, typed]).0, forever::DONE_CLAIM);
        }
        for check in [
            DONE_CHECK.to_string(),
            done_check_words(0),
            aterm_agent::supervise::policy::go_ahead::unless_held(&done_check_words(0), true)
                .into_owned(),
        ] {
            assert_eq!(
                reply(&spec, &[&prompt, CONTINUE_TEXT, &check]).0,
                forever::CLEAN
            );
        }
    }

    /// THE GO-AHEAD SCENE'S CONVERSATION: its park parks as the incident's did, on the word asked
    /// for, with its NEEDS YOU line as the line the go-ahead answers; the go-ahead the policy
    /// builds for it (with the acknowledgment or without) draws [`GO_ON`], which parks nothing,
    /// names no need, asks nothing and claims no done, so Cruise's next act is its continuation.
    /// NEGATIVE CONTROLS: the continuation and the done check, which carry the sentence that the
    /// go-ahead is no need but not [`GO_AHEAD_DROP`], never draw [`GO_ON`].
    #[test]
    fn the_go_ahead_scene_parks_as_the_incident_did_and_goes_on_after_the_answer() {
        use aterm_agent::supervise::policy::go_ahead::{parked, unless_held};
        use aterm_agent::supervise::policy::needs::needs_you;
        use aterm_agent::supervise::policy::turn_end::{
            Said, classify_said, go_ahead_answer, says_done,
        };
        let spec = Scene::GoAhead.spec(Path::new("/lab/proj/README.md"));
        let prompt = Scene::GoAhead.prompt();
        let park = park_reply();
        assert_eq!(reply(&spec, &[&prompt]), (park.as_str(), None));
        assert!(
            park.contains(&format!("{NEEDS_YOU} say \"{PARK_WORD}\"")),
            "{park}"
        );
        let parks = parked("", &park).expect("the scene's park parks");
        assert_eq!(parks.token.as_deref(), Some(PARK_WORD));
        assert!(parks.need, "{parks:?}");
        assert_eq!(
            parks.needs,
            needs_you(&park),
            "the go-ahead answers its one line"
        );
        assert_eq!(parks.needs.len(), 1, "{parks:?}");
        for ack in [false, true] {
            let typed = go_ahead_answer(&parks, ack, None);
            assert!(
                typed.starts_with(GO_AHEAD_ANSWER)
                    && typed.contains(&format!("{TOKEN_OPEN}{PARK_WORD}{TOKEN_CLOSE}"))
                    && typed.contains(GO_AHEAD_DROP),
                "{typed}"
            );
            assert_eq!(reply(&spec, &[&prompt, &park, &typed]).0, GO_ON);
        }
        assert!(parked("", GO_ON).is_none() && needs_you(GO_ON).is_empty());
        assert_eq!(classify_said(Some(GO_ON)), Said::Plain);
        assert!(!says_done(Some(GO_ON)));
        for held in [false, true] {
            let continued = unless_held(CONTINUE_TEXT, held);
            assert_eq!(reply(&spec, &[&prompt, &continued]).0, forever::DONE_CLAIM);
            for n in 0..7 {
                let check = unless_held(&done_check_words(n), held).into_owned();
                assert_eq!(reply(&spec, &[&prompt, &check]).0, forever::CLEAN, "{n}");
            }
        }
    }

    /// The stub's request log names the go-ahead's turn by its opening, inside the 70 characters
    /// it keeps of a last message, and no other text Cruise types opens so.
    #[test]
    fn the_go_ahead_is_told_from_every_other_cruise_text_by_its_opening() {
        let head = go_ahead_head();
        assert!(GO_AHEAD_ANSWER.starts_with(&head) && head.chars().count() <= 70);
        assert!(head.ends_with("go-ahead"), "{head}");
        let others = std::iter::once(CONTINUE_TEXT.to_string())
            .chain(std::iter::once(DONE_CHECK.to_string()))
            .chain((0..7).map(done_check_words));
        for other in others {
            assert!(!other.starts_with(&head), "{other}");
        }
    }

    /// A request line of the stub's log, as the stub writes one: `msgs` messages, the last
    /// `last` (the 70 characters the log keeps), answered by reply rule `reply`.
    fn request(msgs: usize, last: &str, reply: usize) -> String {
        let digest = crate::stub::message_digest(last);
        let last: String = last.chars().take(70).collect();
        format!(
            "1 POST /v1/messages -> 200 body=9B mode=lab kind=main model=lab-stub stream=true \
             max_tokens=32000 msgs={msgs} tools=1 sys=9c last_user=\"{last}\" auth=key \
             reply={reply} last_user_sha256={digest} custom"
        )
    }

    /// CLAUDE CODE TOOK THE GO-AHEAD AS ONE TURN, WHOLE, ONLY WHERE THE STUB ANSWERED IT WHOLE
    /// (2026-10-30 review): the recording's shape passes, the park's request, then the answer's,
    /// which the stub answered by the rule for a last message carrying [`GO_AHEAD_DROP`]
    /// ([`crate::stub::scripted_rule`] names the same rules as [`spec_rule`]), then the
    /// continuation's. NEGATIVE CONTROLS: the answer sent as two turns, its opening drawing the park
    /// again and its last sentence [`GO_ON`], fails though exactly one request still opens with
    /// the answer; so do two go-aheads, none, one after a request the park did not answer, and a
    /// log that names no rule. A word inside the quoted message never stands in for the stub's.
    #[test]
    fn the_go_ahead_is_one_turn_whole_only_where_the_stub_answered_it_whole() {
        let spec = Scene::GoAhead.spec(Path::new("/lab/proj/README.md"));
        let prompt = Scene::GoAhead.prompt();
        let park = park_reply();
        let parks = aterm_agent::supervise::policy::go_ahead::parked("", &park).expect("the park");
        let typed = aterm_agent::supervise::policy::turn_end::go_ahead_answer(&parks, false, None);
        let users = |u: &[&str]| u.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let (drop, park_rule) = (spec_rule(GO_AHEAD_DROP), spec_rule(""));
        let cont = spec_rule(forever::CONTINUE_KEY).expect("the continuation's rule");
        assert_eq!(
            crate::stub::scripted_rule(&spec, &users(&[&prompt])),
            park_rule
        );
        assert_eq!(
            crate::stub::scripted_rule(&spec, &users(&[&prompt, &park, &typed])),
            drop
        );
        let (drop, park_rule) = (drop.expect("the drop rule"), park_rule.expect("the park's"));
        assert_ne!(drop, park_rule);
        let tail = typed
            .rsplit_once(". ")
            .map_or(typed.as_str(), |(_, t)| t)
            .to_string();
        assert!(tail.contains(GO_AHEAD_DROP), "{tail}");
        let recorded = [
            request(2, &prompt, park_rule),
            request(4, &typed, drop),
            request(6, CONTINUE_TEXT, cont),
        ];
        assert_eq!(one_turn_whole(&recorded, &typed), Ok(()));
        let split = [
            request(2, &prompt, park_rule),
            request(4, &typed, park_rule),
            request(6, &tail, drop),
        ];
        let twice = [
            request(2, &prompt, park_rule),
            request(4, &typed, drop),
            request(6, &typed, drop),
        ];
        let none = [
            request(2, &prompt, park_rule),
            request(4, CONTINUE_TEXT, cont),
        ];
        let after = [
            request(2, &prompt, park_rule),
            request(4, CONTINUE_TEXT, cont),
            request(6, &typed, drop),
        ];
        let unnamed: Vec<String> = recorded
            .iter()
            .map(|l| l.replace(&format!(" reply={drop} "), " "))
            .collect();
        let unbound: Vec<String> = recorded
            .iter()
            .map(|l| {
                l.replace(
                    &format!(" last_user_sha256={}", crate::stub::message_digest(&typed)),
                    "",
                )
            })
            .collect();
        for (name, lines) in [
            ("split", &split[..]),
            ("twice", &twice[..]),
            ("none", &none[..]),
            ("after", &after[..]),
            ("unnamed", &unnamed[..]),
            ("unbound", &unbound[..]),
        ] {
            assert!(one_turn_whole(lines, &typed).is_err(), "{name}");
        }
        let quoted = request(4, "x reply=9 msgs=1", drop);
        assert_eq!(
            crate::scn::word_of(&quoted, "reply="),
            Some(drop.to_string().as_str())
        );
        assert_eq!(crate::scn::word_of(&quoted, "msgs="), Some("4"));
    }

    #[test]
    fn the_received_go_ahead_keeps_its_middle_as_well_as_its_ends() {
        let spec = go_ahead_spec();
        let prompt = Scene::GoAhead.prompt();
        let parks = aterm_agent::supervise::policy::go_ahead::parked("", &park_reply())
            .expect("the incident's park");
        let drop = spec_rule(GO_AHEAD_DROP).expect("the answer's rule");
        let park_rule = spec_rule("").expect("the park's rule");
        let mut accepted = Vec::new();
        for ack in [false, true] {
            let typed =
                aterm_agent::supervise::policy::turn_end::go_ahead_answer(&parks, ack, None);
            let complete = [request(2, &prompt, park_rule), request(4, &typed, drop)];
            assert_eq!(
                one_turn_whole(&complete, &typed),
                Ok(()),
                "the full answer: {ack}"
            );

            let missing_middle = format!("{} {GO_AHEAD_DROP}", go_ahead_head());
            assert!(typed.len() > missing_middle.len());
            let users = vec![prompt.clone(), missing_middle.clone()];
            assert_eq!(
                crate::stub::scripted_rule(&spec, &users),
                Some(drop),
                "the old opening and reply-marker premises still hold"
            );
            let shortened = [
                request(2, &prompt, park_rule),
                request(4, &missing_middle, drop),
            ];
            if one_turn_whole(&shortened, &typed).is_ok() {
                accepted.push(ack);
            }
        }
        assert!(
            accepted.is_empty(),
            "a received answer with its middle omitted passed: {accepted:?}"
        );
    }

    /// A `status` reply's shown needs. NEGATIVE CONTROLS: a refused reply reads as none, and only
    /// `-` for both is no need.
    #[test]
    fn a_status_reply_shows_a_need_unless_both_fields_say_none() {
        let none = "OK schema=1 sid=0 needs=- needs_n=- hold_by=-";
        assert_eq!(needs_read(none).as_deref(), Some("needs=- needs_n=-"));
        assert!(!shows_need(&needs_read(none).expect("read")));
        for shown in [
            "OK needs=ask needs_n=1",
            "OK needs=ask needs_n=-",
            "OK needs=- needs_n=1",
        ] {
            assert!(shows_need(&needs_read(shown).expect("read")), "{shown}");
        }
        assert_eq!(needs_read("ERR no session"), None);
        assert_eq!(needs_read("OK needs=-"), None);
    }

    #[test]
    fn the_land_and_cruise_scenes_play_their_forever_chains() {
        let readme = Path::new("/lab/proj/README.md");
        for (scene, chain, repo) in [
            (Scene::Landing, Chain::LandC, Chain::LandA.repo()),
            (Scene::RepoHold, Chain::LandC, Chain::LandC.repo()),
            (Scene::SameReply, Chain::Cruise, Chain::Cruise.repo()),
            (Scene::Hinted, Chain::LandHinted, Chain::LandHinted.repo()),
        ] {
            assert_eq!(scene.spec(readme), forever::spec(&[chain]), "{scene:?}");
            assert_eq!(scene.prompt(), chain.prompt(), "{scene:?}");
            assert_eq!(scene.repo(), repo, "{scene:?}");
            assert!(scene.launch().envs.is_empty(), "{scene:?}");
            let land = chain.autopilot() == "land";
            assert_eq!(
                scene.harness_keys().contains("\"land\""),
                land,
                "{scene:?}: its lab starts under its chain's Autopilot"
            );
        }
        assert_eq!(
            Scene::Placeholder.launch().envs,
            [format!("{SUGGESTIONS_ON}=1")]
        );
    }

    fn lines(ls: &[&str]) -> Vec<String> {
        ls.iter().map(|l| (*l).to_string()).collect()
    }

    #[test]
    fn each_scene_ends_at_the_line_it_is_kept_for() {
        let landed = lines(&[
            "CONTINUED seq=40 rule=land@v1 Autopilot is Land: …",
            "DONE seq=61 rule=finished@v1 the worker reported ATERM-TASK-DONE",
            "CONTINUED seq=80 rule=land-nudge@v1 Still landing: …",
            "DONE seq=97 rule=land@v1 the worker defended that everything is landed",
        ]);
        assert_eq!(end_of(Scene::Landing, &landed), Some(3));
        assert_eq!(end_of(Scene::Landing, &landed[..3]), None);
        let held = lines(&[
            "EXCEPTION seq=12 kind=spend when=later a spend limit",
            "CONTINUED seq=80 rule=land-nudge@v1 Still landing: …",
            "EXCEPTION seq=97 kind=stalled when=later Its program answers aterm's acts with \
             the same reply and no work, so aterm types nothing more. => nothing",
        ]);
        assert_eq!(end_of(Scene::RepoHold, &held), Some(2));
        assert_eq!(end_of(Scene::RepoHold, &held[..2]), None);
        let recovering = lines(&[
            "CONTINUED seq=80 rule=continue-done-check@v1 review the work",
            "WAITING seq=90 repeated report; changing approach on the next turn",
            "CONTINUED seq=97 rule=continue@v1 continue autonomously",
        ]);
        assert_eq!(end_of(Scene::SameReply, &recovering), Some(2));
        assert_eq!(end_of(Scene::SameReply, &recovering[..2]), None);
        assert_eq!(end_of(Scene::SameReply, &held), None);
        let placeheld = lines(&[
            "CONTINUED seq=30 rule=continue-done-check@v1 before the suggestion",
            "CONTINUED seq=40 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going.",
            "WAITING seq=41 4 short turn(s) in a row: 4 min back-off",
            "CONTINUED seq=55 rule=continue-done-check@v1 …",
            "NOTE seq=56 the reply is read",
            "WAITING seq=60 3 short turn(s) in a row: 8 min back-off",
        ]);
        assert_eq!(end_of(Scene::Placeholder, &placeheld), Some(5));
        assert_eq!(end_of(Scene::Placeholder, &placeheld[..5]), None);
        assert_eq!(end_of(Scene::Placeholder, &placeheld[2..]), None);
        let accepted = lines(&[
            "CONTINUED seq=40 rule=continue-suggestion@v1 keep going",
            "CONTINUED seq=55 rule=continue-done-check@v1 …",
            "WAITING seq=60 3 short turn(s) in a row: 8 min back-off",
        ]);
        assert_eq!(
            end_of(Scene::Placeholder, &accepted),
            None,
            "a suggestion accepted bare (2f's decision) is not the scene's end"
        );
        let answered = lines(&[
            "WAITING seq=20 until=… 1 short turn(s) in a row: 15 s back-off",
            "CONTINUED seq=21 rule=go-ahead@v1 [aterm harness] The owner's standing go-ahead: …",
            "EVENT idle seq=30 ⏺ Reran the checks: …",
            "WAITING seq=30 until=… 2 short turn(s) in a row: 30 s back-off",
            "SKIPPED seq=31 a draft stands",
            "CONTINUED seq=40 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going.",
        ]);
        assert_eq!(end_of(Scene::GoAhead, &answered), Some(5));
        assert_eq!(end_of(Scene::GoAhead, &answered[..5]), None);
        // NEGATIVE CONTROLS: with the go-ahead removed (answers off escalate the park), there is
        // no end to reach; the act after it is the end whatever it is, which `play` judges.
        let escalated = lines(&[
            "ESCALATED seq=21 the worker parked its own reversible work on a person's go-ahead",
            "CONTINUED seq=40 rule=continue@v1 [aterm harness] Autopilot is Cruise: keep going.",
        ]);
        assert_eq!(end_of(Scene::GoAhead, &escalated), None);
        let again = lines(&[
            "CONTINUED seq=21 rule=go-ahead@v1 [aterm harness] The owner's standing go-ahead: …",
            "CONTINUED seq=40 rule=go-ahead@v1 [aterm harness] The owner's standing go-ahead: …",
        ]);
        assert_eq!(end_of(Scene::GoAhead, &again), Some(1));
        assert_eq!(continued_rule(&again[1]), Some(RULE_GO_AHEAD));
    }

    /// A landing or a hold runs on past its next rung: c5's landing, the second short turn, a
    /// 30 s back-off; c6's hold, the third, 60 s; each with the margin. NEGATIVE
    /// CONTROLS: the placeholder and go-ahead scenes' ends are no quiet, and run only the settle; a
    /// quiet with fewer turns waited runs the shorter rung.
    #[test]
    fn a_landing_or_a_hold_runs_past_the_act_it_did_not_take() {
        let margin = std::time::Duration::from_secs(QUIET_MARGIN_S);
        let s = std::time::Duration::from_secs;
        let landed = lines(&[
            "WAITING seq=9 until=… 1 short turn(s) in a row: 15 s back-off",
            "CONTINUED seq=10 rule=land@v1 Autopilot is Land: …",
        ]);
        let held = lines(&[
            "WAITING seq=9 until=… 1 short turn(s) in a row: 15 s back-off",
            "CONTINUED seq=10 rule=continue-done-check@v1 …",
            "EVENT idle seq=11 ⏺ Nothing left: …",
            "WAITING seq=11 until=… 2 short turn(s) in a row: 30 s back-off",
            "CONTINUED seq=12 rule=continue-done-check@v1 …",
        ]);
        assert_eq!(settle_after(true, &landed), s(30) + margin);
        assert_eq!(settle_after(true, &held), s(60) + margin);
        assert!(Scene::Landing.ends_quiet() && Scene::RepoHold.ends_quiet());
        assert!(!Scene::SameReply.ends_quiet());
        assert!(!Scene::Placeholder.ends_quiet() && !Scene::GoAhead.ends_quiet());
        assert_eq!(settle_after(false, &held), s(SETTLE_S));
        assert_eq!(settle_after(true, &[]), s(15) + margin);
    }

    /// The placeholder scene is kept only where its continuation and its done check were typed
    /// over the stub's suggestion. NEGATIVE CONTROLS: none read, a person's draft, the idle hint.
    #[test]
    fn the_done_check_is_kept_only_over_the_suggestion() {
        assert!(over_suggestion(Some(SUGGESTION)));
        assert!(over_suggestion(Some(" keep going ")));
        for not in [
            None,
            Some(""),
            Some("fix the parser"),
            Some("Try \"refactor\""),
        ] {
            assert!(!over_suggestion(not), "{not:?}");
        }
    }

    #[test]
    fn a_continued_line_names_its_rule_and_no_other_line_does() {
        let line = "CONTINUED seq=102 rule=continue-suggestion@v1 keep going";
        assert_eq!(continued_rule(line), Some(RULE_SUGGESTION));
        assert_eq!(continued_words(line), SUGGESTION);
        assert_eq!(continued_rule("WAITING seq=9 rule=x@v1 overloaded"), None);
    }

    /// The composer an act was typed over is the last one before its typing began. NEGATIVE
    /// CONTROL: the suggestion it replaced is not its typing.
    #[test]
    fn a_composer_mid_act_is_the_acts_typing_and_the_suggestion_is_not() {
        for words in [done_check_words(0).as_str(), CONTINUE_TEXT] {
            let head: String = words.chars().take(5).collect();
            assert!(typing(&head, words));
            assert!(typing(words, words));
            assert!(!typing(SUGGESTION, words));
            assert!(!typing("", words));
        }
    }

    #[test]
    fn each_scene_is_listed_with_its_keys_and_its_lab() {
        let listed = crate::scn::scenarios();
        for scene in Scene::ALL {
            let s = listed
                .iter()
                .find(|s| s.name == scene.name())
                .expect("listed");
            assert_eq!(s.harness_keys, scene.harness_keys(), "{scene:?}");
            assert_eq!(s.kw.instance_env, INSTANCE_ENV, "{scene:?}");
            assert!(
                matches!(s.plan, crate::scn::Plan::Autopilot(p) if p == scene),
                "{scene:?}: {:?}",
                s.plan
            );
        }
    }

    #[test]
    fn every_scene_launches_lean() {
        for scene in Scene::ALL {
            assert_eq!(scene.launch().args, LEAN, "{scene:?}");
        }
    }
}
