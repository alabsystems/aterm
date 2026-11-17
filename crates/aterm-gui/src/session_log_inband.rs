// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TITLE A PROGRAM PRINTS IS A CLAIM (streams design §6, in-band): the window title of OSC 0
//! and OSC 2, and the `title` of an OSC 7501 report from any program, become the session's
//! `desc field=title` claim by nobody at `src=engine conf=claim`, which ranks
//! [`aterm_slog::Rank::Inband`]: it stands only where nobody of a better rank (a person, the
//! session's own program, its creator, a writer aterm ran, a control client) claims the title, so
//! printing a file that holds the sequence changes nothing shown while any of them does. The
//! text is kept at the content tier, in `title=` and never in `value=`, so it reaches neither
//! the journal nor the bus, and no update carries it. A successor claims the window title
//! again, which the terminal's checkpoint carries, at its first drift flush, that is, at the
//! program's next output. It claims a 7501 title, which no checkpoint carries
//! ([`aterm_core::terminal::program_state::ProgramStateReport::title`]), only when the program's
//! next report names one. A command line set as the title enters no claim, as it enters no
//! record.
//!
//! The observed `title` mark ([`SessionLog::observe_title`]) and its `title-change` row stay
//! as they are; this claim is a second record beside them, written only when what it says
//! moves.

use aterm_slog::{Actor, Conf, Node, Src, Stamp};

use super::SessionLog;
use super::mark::{ObservedClaim, observed_stored};
use super::observed::{MarkBucket, TitleStands};

/// The slot an in-band claim fills: in-band bytes carry a title only.
const SLOT: &str = "title";

/// WHAT THE DRIFT FLUSH READ OFF THE ENGINE for the in-band title, under its guard
/// ([`SessionLog::observe_inband_title`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct InbandRead<'a> {
    /// The window title (OSC 0 or 2) the engine holds, as a label reads it
    /// ([`crate::tab_label::informative`]): the text the title mark is given
    /// ([`SessionLog::observe_title`]).
    pub(crate) shown: &'a str,
    /// Whether the window title is the shell's own, or says nothing a label reads
    /// ([`crate::app_take_over::untitled_by_its_program`]).
    pub(crate) untitled: bool,
    /// What the window title stands for ([`crate::session_status::title_stands`]).
    pub(crate) stands: TitleStands,
    /// The title the program's OSC 7501 report gives itself
    /// ([`aterm_core::terminal::program_state::ProgramStateReport::title`]).
    pub(crate) program: Option<&'a str>,
    /// Its sequence ([`aterm_core::terminal::program_state::ProgramStateReport::title_seq`]).
    pub(crate) program_seq: u64,
}

impl InbandRead<'_> {
    /// The window title's words: [`Self::shown`] without the status glyph an agent leads it with
    /// ([`crate::app_take_over::title_without_agent_glyph`]), so a spinner's phase or a turn's
    /// edge says nothing new.
    fn window_words(&self) -> &str {
        crate::app_take_over::title_without_agent_glyph(self.shown)
    }

    /// Whether the window title claims nothing: it is the shell's own, says nothing, or is a
    /// command line, whose text enters no record.
    fn unclaimed(&self) -> bool {
        self.untitled || self.stands == TitleStands::CommandLine
    }

    /// The text the window title claims: its words, [`stored`]; none where it claims nothing
    /// ([`Self::unclaimed`]).
    fn window_claim(&self) -> Option<String> {
        if self.unclaimed() {
            return None;
        }
        stored(self.window_words())
    }
}

/// `words` held as a claim aterm observed holds them ([`observed_stored`]): redacted and cut to
/// `title=`'s cap. None where nothing is left, nothing but redaction notices, or a text in which
/// the harness's turn-end readers read a head: its writer then withdraws the program's claim, so
/// an earlier title does not stand while the current one reads as a protocol line, and claims
/// again once the title moves.
fn stored(words: &str) -> Option<String> {
    observed_stored(SLOT, words, true, Actor::Nobody, Src::Engine).ok()
}

/// The text a 7501 title claims: its informative part ([`crate::tab_label::informative`]) without
/// the status glyph an agent leads it with ([`crate::app_take_over::title_without_agent_glyph`]),
/// [`stored`].
fn claim_text(title: &str) -> Option<String> {
    let shown = crate::tab_label::informative(title).to_string();
    stored(crate::app_take_over::title_without_agent_glyph(&shown))
}

/// WHAT THE WINDOW TITLE SAID as last worked out: the fingerprint of the text it claims, or,
/// where it claims nothing (a command line, the shell's own, nothing), of its own words, kept
/// apart from every claim's text by the variant, so a prompt's title after a command line is
/// still news and withdraws what the program left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowSaid {
    Claims(u64),
    Unclaimed(u64),
}

/// THE IN-BAND TITLE'S AUTHOR: what it last read of each source, what it last wrote, the
/// newest decision not yet written and its own bucket. Never carried across an update.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InbandTitle {
    /// The fingerprint of the window title's words ([`InbandRead::window_words`]) and whether
    /// they claim nothing ([`InbandRead::unclaimed`]), as last read: while both hold, what the
    /// window claims cannot have moved, so nothing more is worked out (`None`: never read).
    words: Option<(u64, bool)>,
    /// What the window title said when its words last moved ([`WindowSaid`]): a move of it is
    /// the window's news (`None`: never read, so the first read is news).
    window: Option<WindowSaid>,
    /// The program title's sequence as last read (0: no program has titled itself).
    program_seq: u64,
    /// The newest decision not yet written: the text to claim, or `None` to withdraw.
    pending: Option<Option<String>>,
    /// Where the last decision written left the claim: its subject, `None` when it withdrew.
    on: Option<Node>,
    /// That decision's text.
    text: Option<String>,
    /// Its own share of records: [`super::observed::PROGRAM_BURST`] at once, then one each
    /// [`super::observed::PROGRAM_REFILL_MS`], as a program's moves have.
    bucket: MarkBucket,
}

impl SessionLog {
    /// THE IN-BAND TITLE, read at a drift flush ([`InbandRead`]), under the timeline lock alone:
    /// a claim of rank 6 never moves the pinned title, so the session's metadata is not read.
    /// The window title and the program's 7501 title are two sources of one author, and the
    /// one that changed since the last read gives the text; where both did, the program's when
    /// it names one (a report's title is the program's own word, never a command line), else
    /// the window's. The window changed only where what it claims did ([`WindowSaid`]): a
    /// spinner's phase or a status glyph leaves its words as they were, and a read whose words
    /// and standing are unchanged is worked out no further, so it costs one hash and neither
    /// redaction nor a look at the fold. The decision is written ([`Self::write_inband`]) when
    /// the bucket has a token; the newest replaces one still waiting.
    pub(crate) fn observe_inband_title(&mut self, read: &InbandRead<'_>, at: Stamp) {
        let state = &mut self.observed.inband;
        let program_moved = state.program_seq != read.program_seq;
        state.program_seq = read.program_seq;
        let words = (super::name_fp(Some(read.window_words())), read.unclaimed());
        // `Some(claim)` where what the window says moved, the claim worked out once.
        let window = if state.words == Some(words) {
            None
        } else {
            state.words = Some(words);
            let claim = read.window_claim();
            let said = match claim.as_deref() {
                Some(text) => WindowSaid::Claims(super::name_fp(Some(text))),
                None => WindowSaid::Unclaimed(words.0),
            };
            let moved = state.window != Some(said);
            state.window = Some(said);
            moved.then_some(claim)
        };
        let decided = if program_moved && (read.program.is_some() || window.is_none()) {
            Some(read.program.and_then(claim_text))
        } else {
            window
        };
        if let Some(text) = decided {
            state.pending = Some(text);
        }
        self.write_inband(at);
    }

    /// THE IN-BAND TITLE'S TICK, the session log's settle tick's part ([`SessionLog::tick`],
    /// which runs whether or not tab status is on): write a decision the bucket held back, once
    /// it has a token, and move a claim made on the session to its agent once one opens.
    pub(super) fn inband_tick(&mut self, at: Stamp) {
        self.write_inband(at);
    }

    /// When the in-band author wants its tick ([`Self::inband_tick`], monotonic ms): when its
    /// bucket next has a token, while a decision waits for one or a claim made on the session
    /// waits to move to the agent that opened since; `None` while nothing is held, so an idle
    /// session arms no wake.
    pub(super) fn inband_wake(&self) -> Option<u64> {
        let state = &self.observed.inband;
        let owed = state.pending.is_some()
            || (state.on == Some(Node::SESSION) && self.inband_subject() != Node::SESSION);
        owed.then(|| state.bucket.next_token_at())
    }

    /// What the in-band claim describes: the session's open agent run (it is that agent's topic,
    /// and lapses when the agent ends), else the session.
    fn inband_subject(&self) -> Node {
        self.agent
            .as_ref()
            .map(|a| a.node)
            .filter(|n| self.log.is_open(*n))
            .unwrap_or(Node::SESSION)
    }

    /// The text the in-band author's claim on `scope` holds, as the fold keeps it.
    fn inband_held(&self, scope: Node) -> Option<&str> {
        self.log
            .state()
            .desc
            .claim_of(scope, SLOT, Actor::Nobody, Src::Engine, None)
            .map(|c| c.value.as_str())
    }

    /// WRITE WHAT THE IN-BAND AUTHOR STATES, against what the fold holds of it: the pending
    /// decision, or, with none, the last one where it was made on the session and an agent has
    /// opened since. A text is claimed on the subject ([`Self::inband_subject`]) unless the
    /// author's claim there already holds it, so a spinner's phase or a status glyph writes
    /// nothing; a withdrawal is written only where the author holds a claim. A claim the fold
    /// dropped for a ninth author of better rank is not sent again until its text or subject
    /// moves. A claim on an agent first withdraws the author's claim on the session, where a tie
    /// would hide the agent's topic. A decision whose records the fold already holds is done
    /// without a token; otherwise one token pays for its records, and with none it waits for
    /// the next flush or for the tick its wake asks for ([`Self::inband_wake`]). A claim on an
    /// agent that ended lapsed with it and is not made again on the session until a title
    /// moves.
    fn write_inband(&mut self, at: Stamp) {
        let subject = self.inband_subject();
        let state = &self.observed.inband;
        let want = match &state.pending {
            Some(decided) => decided.clone(),
            None if state.on == Some(Node::SESSION) && subject != Node::SESSION => {
                state.text.clone()
            }
            None => return,
        };
        let mut writes: Vec<(Node, Option<&str>)> = Vec::with_capacity(2);
        if subject != Node::SESSION && self.inband_held(Node::SESSION).is_some() {
            writes.push((Node::SESSION, None));
        }
        let held = self.inband_held(subject);
        // The fold dropped the last claim written here (a ninth author outranked it): it is not
        // sent again until its text or subject moves.
        let dropped = held.is_none() && state.on == Some(subject) && state.text == want;
        match want.as_deref() {
            Some(text) if held != Some(text) && !dropped => writes.push((subject, Some(text))),
            None if held.is_some() => writes.push((subject, None)),
            _ => {}
        }
        if !writes.is_empty() && !self.observed.inband.bucket.spend(at.mono_ms, false) {
            return;
        }
        for (on, text) in writes {
            let claim = ObservedClaim {
                on,
                slot: SLOT,
                text,
                content: true,
                by: Actor::Nobody,
                src: Src::Engine,
                conf: Conf::Claim,
                as_name: None,
            };
            // A closed session's log takes no record: the claim is dropped with it.
            let _ = self.claim_observed(&claim, at);
        }
        let state = &mut self.observed.inband;
        state.pending = None;
        state.on = want.is_some().then_some(subject);
        state.text = want;
    }
}

#[cfg(test)]
#[path = "session_log_inband_tests.rs"]
mod tests;
