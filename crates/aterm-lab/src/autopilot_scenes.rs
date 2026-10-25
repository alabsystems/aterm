// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TAPE CORPUS'S AUTOPILOT SCENES (`lab scn run c5-land` …, kept with `lab tape promote`):
//! Land and Cruise on a real Claude Code against the stub, the conversation scripted turn by turn
//! as the forever lab scripts its chains ([`crate::forever`]), in a repository laid in the lab's
//! own project, run until the act or the hold the scene is kept for is journaled.

use std::path::Path;

use aterm_agent::supervise::journal::Word;
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_agent::supervise::policy::turn_end::{
    RULE_DONE_CHECK, RULE_LAND, RULE_SUGGESTION, TurnEndTiming,
};

use crate::forever::{self, Chain, Repo};
use crate::lab::Launch;
use crate::pyjson::J;
use crate::record::{rec_s, rec_t};
use crate::scn::{Ctx, LabKw, ScnErr};
use crate::stop;
use crate::util::now;

/// One scene the corpus keeps a tape of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    /// Land's landing, land-a's L1 and L5: a work reply, `land@v1`; a clean defense in a clean
    /// repository, landed (`DONE … rule=land@v1`). Its conversation is land-c's (whose replies
    /// land-a's are but for L3's `NEEDS YOU:`), in land-a's clean repository: a nudge carries the
    /// need's acknowledgement, whose words have moved since `b42ae72fc3`, and the corpus's Land
    /// tapes replay on that tree's policy as well as this one's.
    Landing,
    /// Land's repository hold, land-c's: one untracked file beside a pushed `main`. A clean
    /// defense, a nudge naming the file, the same defense again: held (`kind=stalled`).
    RepoHold,
    /// Cruise's same-reply hold, the `cruise` chain's: a done claim, a done check, a defense, a
    /// done check, the same defense again: held (`kind=stalled`).
    SameReply,
    /// Cruise over Claude Code's grey suggestion: a work reply after a tool call, its suggestion
    /// accepted (`continue-suggestion@v1`); a done claim, the done check typed over the
    /// suggestion it drew again; the defense, and the next point decided. 2.1.310 draws the
    /// suggestion's first glyph inverse, under the caret it draws itself, and the rest dim.
    Placeholder,
}

impl Scene {
    /// Every scene, in the corpus's order.
    pub const ALL: [Scene; 4] = [
        Scene::Landing,
        Scene::RepoHold,
        Scene::SameReply,
        Scene::Placeholder,
    ];

    /// Its scenario's name, and its corpus tape's (`lab tape rerecord <name>`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scene::Landing => "c5-land",
            Scene::RepoHold => "c6-land-repo",
            Scene::SameReply => "c7-cruise-same",
            Scene::Placeholder => "c8-cruise-placeholder",
        }
    }

    /// Its `[harness]` lines: the Autopilot every session of its lab starts under.
    #[must_use]
    pub const fn harness_keys(self) -> &'static str {
        match self {
            Scene::Landing | Scene::RepoHold => "autopilot = \"land\"",
            Scene::SameReply | Scene::Placeholder => "autopilot = \"cruise\"",
        }
    }

    /// What its repository holds beside its pushed `main`.
    #[must_use]
    pub const fn repo(self) -> Repo {
        match self {
            Scene::RepoHold => Repo::Untracked,
            Scene::Landing | Scene::SameReply | Scene::Placeholder => Repo::Clean,
        }
    }

    /// The forever chain whose conversation it plays, word for word.
    const fn chain(self) -> Option<Chain> {
        match self {
            Scene::Landing | Scene::RepoHold => Some(Chain::LandC),
            Scene::SameReply => Some(Chain::Cruise),
            Scene::Placeholder => None,
        }
    }

    /// The word its conversation carries, by which the stub's rules know it.
    #[must_use]
    pub fn token(self) -> String {
        self.chain()
            .map_or_else(|| PLACEHOLDER_TOKEN.to_string(), Chain::token)
    }

    /// Its first prompt, typed into the composer as a person would.
    #[must_use]
    pub fn prompt(self) -> String {
        self.chain().map_or_else(
            || format!("{PLACEHOLDER_TOKEN} write the parser for this repository and land it"),
            Chain::prompt,
        )
    }

    /// Its stub mode's spec ([`crate::stub::scripted_reply`]); `readme` the project's
    /// `README.md`, which the placeholder scene's first reply reads.
    #[must_use]
    pub fn spec(self, readme: &Path) -> J {
        match self.chain() {
            Some(chain) => forever::spec(&[chain]),
            None => placeholder_spec(readme),
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
        !matches!(self, Scene::Placeholder)
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

/// The suggestion the stub drafts: a continuation the turn-end policy accepts bare
/// (`turn_end::suggestion_allowed`).
pub const SUGGESTION: &str = "keep going";

/// The placeholder scene's spec: the suggestion request answered with [`SUGGESTION`] at once;
/// the done check with the clean defense; an accepted suggestion (or Cruise's own words, which
/// hold it) with the done claim; the first prompt with a read of `readme` and the work reply,
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

/// THE SCENE'S END in the journal's `lines` (oldest first): the index of the line the scene is
/// kept for, or `None` while it has not come.
#[must_use]
pub fn end_of(scene: Scene, lines: &[String]) -> Option<usize> {
    let stalled = format!("kind={}", ExceptionKind::Stalled.word());
    let landed = format!("rule={RULE_LAND}");
    match scene {
        Scene::Landing => lines
            .iter()
            .position(|l| verb(l) == "DONE" && says(l, &landed)),
        Scene::RepoHold | Scene::SameReply => lines
            .iter()
            .position(|l| verb(l) == "EXCEPTION" && says(l, &stalled)),
        Scene::Placeholder => {
            let accepted = lines
                .iter()
                .position(|l| continued_rule(l) == Some(RULE_SUGGESTION))?;
            let checked = accepted
                + 1
                + lines[accepted + 1..]
                    .iter()
                    .position(|l| continued_rule(l) == Some(RULE_DONE_CHECK))?;
            lines[checked + 1..]
                .iter()
                .position(|l| decides(l))
                .map(|at| checked + 1 + at)
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
/// What the loop typed, the end's line and (for the placeholder) the composer the done check
/// was typed over go into the summary; a scene that does not reach its end puts an `error`,
/// which `tape promote` refuses.
/// # Errors
/// Interrupted.
pub fn play(ctx: &mut Ctx, scene: Scene) -> Result<(), ScnErr> {
    let from = ctx.journal().len();
    ctx.turn(&scene.prompt())?;
    let by = now() + scene.within_s();
    let end = loop {
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
        let checked = journal.iter().skip(from).find_map(|(w, r)| {
            let line = rec_s(r, "line");
            (continued_rule(&line) == Some(RULE_DONE_CHECK)).then(|| {
                let t = rec_t(r);
                (
                    if t > 0.0 { t } else { *w },
                    continued_words(&line).to_string(),
                )
            })
        });
        let over = checked.and_then(|(t, words)| {
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
                    "{}: its done check was typed over {over:?}, not the suggestion {SUGGESTION:?}",
                    scene.name()
                ),
            );
        }
        ctx.put("composer_at_check", over.map_or(J::Null, J::s));
    }
    let settle = settle_after(scene.ends_quiet(), &lines[..at]);
    ctx.put("settle_s", J::Float(settle.as_secs_f64()));
    stop::sleep(settle)?;
    Ok(())
}

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
    use aterm_agent::supervise::policy::turn_end::{DONE_CHECK, suggestion_allowed};

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
            "the policy accepts the suggestion bare"
        );
        for typed in [SUGGESTION, CONTINUE_TEXT] {
            assert_eq!(reply(&spec, &[&prompt, typed]).0, forever::DONE_CLAIM);
        }
        assert_eq!(
            reply(&spec, &[&prompt, SUGGESTION, DONE_CHECK]).0,
            forever::CLEAN
        );
    }

    #[test]
    fn the_land_and_cruise_scenes_play_their_forever_chains() {
        let readme = Path::new("/lab/proj/README.md");
        for (scene, chain, repo) in [
            (Scene::Landing, Chain::LandC, Chain::LandA.repo()),
            (Scene::RepoHold, Chain::LandC, Chain::LandC.repo()),
            (Scene::SameReply, Chain::Cruise, Chain::Cruise.repo()),
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
        for scene in [Scene::RepoHold, Scene::SameReply] {
            assert_eq!(end_of(scene, &held), Some(2), "{scene:?}");
            assert_eq!(end_of(scene, &held[..2]), None, "{scene:?}");
        }
        let placeheld = lines(&[
            "CONTINUED seq=30 rule=continue-done-check@v1 before the suggestion",
            "CONTINUED seq=40 rule=continue-suggestion@v1 keep going",
            "WAITING seq=41 4 short turn(s) in a row: 4 min back-off",
            "CONTINUED seq=55 rule=continue-done-check@v1 …",
            "NOTE seq=56 the reply is read",
            "WAITING seq=60 3 short turn(s) in a row: 8 min back-off",
        ]);
        assert_eq!(end_of(Scene::Placeholder, &placeheld), Some(5));
        assert_eq!(end_of(Scene::Placeholder, &placeheld[..5]), None);
        assert_eq!(end_of(Scene::Placeholder, &placeheld[2..]), None);
    }

    /// A landing or a hold runs on past its next rung: c5's landing, the second short turn, a
    /// 30 s back-off; c6's and c7's holds, the third, 60 s; each with the margin. NEGATIVE
    /// CONTROLS: the placeholder scene's end is no quiet, and runs only the settle; a quiet with
    /// fewer turns waited runs the shorter rung.
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
        assert!(Scene::SameReply.ends_quiet());
        assert!(!Scene::Placeholder.ends_quiet());
        assert_eq!(settle_after(false, &held), s(SETTLE_S));
        assert_eq!(settle_after(true, &[]), s(15) + margin);
    }

    /// The placeholder scene is kept only where its done check was typed over the stub's
    /// suggestion. NEGATIVE CONTROLS: none read, a person's draft, the idle hint.
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

    /// The composer a done check was typed over is the last one before its typing began.
    /// NEGATIVE CONTROL: the suggestion it replaced is not its typing.
    #[test]
    fn a_composer_mid_act_is_the_acts_typing_and_the_suggestion_is_not() {
        let words = DONE_CHECK;
        let head: String = words.chars().take(5).collect();
        assert!(typing(&head, words));
        assert!(typing(words, words));
        assert!(!typing(SUGGESTION, words));
        assert!(!typing("", words));
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
    fn every_scene_launches_lean_on_the_forever_labs_scaled_ladder() {
        assert_eq!(INSTANCE_ENV, forever::ladder_env());
        for scene in Scene::ALL {
            assert_eq!(scene.launch().args, LEAN, "{scene:?}");
            assert_eq!(scene.lab_kw().instance_env, INSTANCE_ENV, "{scene:?}");
        }
    }
}
