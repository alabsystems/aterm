// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor claim (`meta set supervisor <holder> [ttl=<ms>]`): a loop
//! that acts alone (the in-GUI host's) says so on the session, so
//! `status supervisor=` names who answers its prompts, the server stops
//! raising the agent's own boxes to the menu bar while the claim is live, and
//! a second supervisor on the same session learns it is second.
//!
//! * Taken at the start of an unattended loop as a lease
//!   (`ttl=`[`ttl_ms`]), renewed every [`CLAIM_RENEW`] by the loop's
//!   next request ([`Session::renew_claim_if_due`]), so a dead loop's claim
//!   lapses. A lease because the CLI may open a connection per request.
//! * Released at the end (`meta unset supervisor`), only when held.
//! * Another holder (`ERR busy supervisor=<holder>`): the loop only watches,
//!   says `WATCHING …` once, escalates every box naming the holder, and
//!   refuses its own writes locally ([`Session::claim_refuses`]); it retries
//!   at every renewal and says `CLAIMED …` when it gets it.
//! * A claim request not served (an outage): pending. The loop types and
//!   presses nothing until a request is answered; a held claim whose renewal
//!   went unanswered is pending too.
//! * A host without the claim (`ERR usage`, or `ERR denied` for an edge
//!   token): the loop acts without it and says `CLAIM unavailable …` once.
//! * Autopilot `off` (`lifecycle.rs`'s `park_while_off`): given back for the
//!   park ([`Session::give_back_claim`]) — released as at the end, and
//!   neither renewed nor tried while parked, so the session looks as
//!   unsupervised as it is — and taken again as the park ends on another
//!   word ([`Session::retake_claim`]). The holder a loop was behind as it
//!   parked is remembered, so `WATCHING …` is said once a holder, not once
//!   a park.
//!
//! No clock of its own: the renewal rides the requests the loop makes anyway.

use super::*;

/// What a loop that yields to another holder ([`SuperviseOpts::yield_when_held`])
/// ends with, followed by the holder: the in-GUI host reads it as "parked
/// behind another supervisor", not as a fault.
pub const CLAIM_HELD: &str = "supervisor claim held by ";

/// The claim's lease, ms, on the clock `t` ([`crate::harness::Timings::claim_ttl`]):
/// the product's is 90 s, three renewals' worth.
pub(super) fn ttl_ms(t: &crate::harness::Timings) -> u64 {
    u64::try_from(t.claim_ttl.as_millis()).unwrap_or(u64::MAX)
}
/// How often the claim is renewed (and, behind another holder, tried). Public
/// because the in-GUI host's adopted-claim grace after a seamless update is
/// counted in these steps (aterm-gui `harness_host`).
pub const CLAIM_RENEW: Duration = Duration::from_secs(20);

/// Where the loop stands on the session's supervisor claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Not taken: a loop that does not act alone (the engine tests'
    /// stop-at-review `supervise`), or before the start.
    #[default]
    Off,
    /// Asked for and not answered (the request was not served): the loop
    /// presses and types nothing, and asks again before its next request.
    Pending { tried: Instant },
    /// Held by this loop; last set at `renewed`.
    Held { renewed: Instant },
    /// Another supervisor holds it: this loop watches only; last tried at
    /// `tried`.
    Behind { holder: String, tried: Instant },
    /// The host has no claim to take (`why`): the loop acts without one.
    Unavailable { why: String },
    /// A PROVEN SELF-CLAIM holds the session ([`Session::set_beside`]): the
    /// agent supervises itself, and this loop watches beside it. It is
    /// watching behind `holder` in every way an act reads
    /// ([`Self::watching_behind`]: nothing typed, pressed, badged or mailed),
    /// save that aterm's remedies acting from outside the agent stand
    /// ([`Self::remedies_withheld`]: the stall's restart). Never claimed,
    /// renewed or yielded to: the host started this loop for it.
    Beside { holder: String },
    /// Given back while the session's Autopilot is off
    /// ([`Session::give_back_claim`]): held by nobody on this loop's account,
    /// never renewed or tried, and taken again as the park ends
    /// ([`Session::retake_claim`]). Refuses nothing: a parked loop sends no
    /// input. `behind`: the other holder the loop was watching behind as it
    /// parked, so the retake that finds the same holder says nothing new
    /// ([`Self::known_holder`]): `WATCHING …` is said once a holder, not
    /// once a park.
    GivenBack { behind: Option<String> },
}

impl Claim {
    /// The other holder, when this loop is watching behind one.
    pub(crate) fn watching_behind(&self) -> Option<&str> {
        match self {
            Claim::Behind { holder, .. } | Claim::Beside { holder } => Some(holder),
            _ => None,
        }
    }

    /// Whether aterm's remedies that act from outside the agent (the stall's
    /// explicit restart) stand down: behind another supervisor, which
    /// answers for the session — never beside a proven self-claim, whose
    /// agent cannot restart itself out of a stall.
    pub(crate) fn remedies_withheld(&self) -> bool {
        matches!(self, Claim::Behind { .. })
    }

    /// The other holder this loop last knew of: the one it watches behind,
    /// or the one it was behind as an Off park gave the claim back. What
    /// [`Session::claim_from`] compares a new reply against, so neither a
    /// renewal nor a park's retake says a holder twice.
    fn known_holder(&self) -> Option<&str> {
        match self {
            Claim::Behind { holder, .. } | Claim::Beside { holder } => Some(holder),
            Claim::GivenBack { behind } => behind.as_deref(),
            _ => None,
        }
    }
}

/// The holder word of an `ERR busy supervisor=<holder>` reply.
fn busy_holder(r: &CtlReply) -> Option<String> {
    let text = r.err_text();
    let rest = text.split("busy").nth(1)?;
    rest.split_whitespace()
        .find_map(|w| w.strip_prefix("supervisor="))
        .map(super::super::report::pct_decode)
}

/// A claim line: `WATCHING …` is said (the watch's reader must know this loop
/// answers nothing), and the rest (`CLAIMED`, `CLAIM unavailable`) are
/// journaled.
fn say_claim(line: &str, review: &mut dyn Review) {
    if line.starts_with("WATCHING") {
        review.say(line).ok();
    } else {
        review.note(line);
    }
}

/// The verbs that type into the worker: what a loop whose claim is pending
/// may not send, and what a stopped loop sends no more ([`Session::call`]).
pub(super) fn is_input(args: &[&str]) -> bool {
    let words: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('@'))
        .collect();
    matches!(
        words.as_slice(),
        ["key" | "send" | "turn" | "paste" | "feed-bin", ..]
    )
}

/// The verbs a watching-only loop may not send: everything that types into
/// the worker, every attention write, and the task record's mark (the
/// supervisor that holds the session publishes its `done`). A pending claim
/// holds back input only ([`is_input`]): its badges and its marks still go.
pub(crate) fn is_write(args: &[&str]) -> bool {
    let words: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('@'))
        .collect();
    matches!(
        words.as_slice(),
        ["key" | "send" | "turn" | "paste" | "feed-bin" | "post", ..]
            | ["meta", "set" | "unset", "attention", ..]
            | ["task", "mark", ..]
    )
}

/// The holder a loop no one named claims the session under:
/// `aterm-supervise:<pid>`, this process's. Read where a setup is made
/// ([`super::SessionSetup::new`], [`Session::set_supervisor_name`]`(None)`),
/// never as the claim is taken, so a loop rebuilt from a tape's header
/// claims under the recorded holder and does not read its own pid.
pub(super) fn default_holder() -> String {
    format!("aterm-supervise:{}", std::process::id())
}

impl<C: Ctl> Session<'_, C> {
    /// The name this loop claims the session under
    /// ([`Self::set_supervisor_name`]; resolved in its setup).
    fn holder(&self) -> String {
        self.setup.holder.clone()
    }

    /// One `meta set supervisor <holder> ttl=<ms>`, straight to the
    /// transport (it is no request of the loop's, and it is made from
    /// inside [`Self::call`]); `None` when it was not served.
    fn set_claim(&mut self) -> Option<CtlReply> {
        let holder = self.holder();
        let ttl = format!("ttl={}", self.setup.claim_ttl_ms);
        let mut args: Vec<&str> = Vec::with_capacity(5);
        if let Some(sid) = &self.setup.sid {
            args.push(sid.as_str());
        }
        args.extend(["meta", "set", "supervisor", holder.as_str(), ttl.as_str()]);
        match self.ctl.call(&args) {
            Ok(r) if !r.lost() => Some(r),
            _ => None,
        }
    }

    /// Where a `meta set supervisor` reply leaves the claim, and the line
    /// that says a change.
    fn claim_from(&mut self, r: &CtlReply) -> Option<String> {
        let now = self.fx.mono();
        if r.ok() {
            // Taken quietly (`status supervisor=` says it); only a claim
            // won from behind another holder is news — a park's retake of
            // one it was behind as it parked included.
            let was_behind = self.claim.known_holder().map(str::to_string);
            self.claim = Claim::Held { renewed: now };
            return was_behind.map(|h| format!("CLAIMED supervisor={} (after {h})", self.holder()));
        }
        if let Some(holder) = busy_holder(r) {
            // A holder already said — at a renewal, or before an Off park
            // gave the claim back — is not said again.
            let said = self.claim.known_holder() != Some(holder.as_str());
            self.claim = Claim::Behind {
                holder: holder.clone(),
                tried: now,
            };
            return said
                .then(|| format!("WATCHING another supervisor holds this session: {holder}"));
        }
        let why = one_line(r.err_text());
        let said = !matches!(self.claim, Claim::Unavailable { .. });
        self.claim = Claim::Unavailable { why: why.clone() };
        said.then(|| format!("CLAIM unavailable ({why}): acting without one"))
    }

    /// The unattended loop's first act: take the claim, and say where it
    /// stands. A request not served leaves it untaken; the first renewal
    /// tries again.
    pub(super) fn take_claim(&mut self, review: &mut dyn Review) {
        // Beside a proven self-claim: nothing is claimed, and it is said once.
        if let Some(holder) = self.setup.beside.clone() {
            let said = self.claim.known_holder() != Some(holder.as_str());
            self.claim = Claim::Beside {
                holder: holder.clone(),
            };
            if said {
                say_claim(
                    &format!(
                        "WATCHING a proven self-claim supervises this session: {holder} (aterm's \
                         stall and memory remedies stand)"
                    ),
                    review,
                );
            }
            return;
        }
        if let Some(line) = self.try_claim() {
            say_claim(&line, review);
        }
    }

    /// Ask for the claim once: what the answer says to say, if anything. A
    /// request not served leaves it pending, due at the next request.
    fn try_claim(&mut self) -> Option<String> {
        match self.set_claim() {
            Some(r) => self.claim_from(&r),
            None => {
                self.claim = Claim::Pending {
                    tried: self
                        .fx
                        .mono()
                        .checked_sub(self.setup.claim_renew)
                        .unwrap_or_else(|| self.fx.mono()),
                };
                None
            }
        }
    }

    /// CLAIM AT NAMING (card engine §8, lane A4): the hosted loop's claim,
    /// asked for as its first request — before it opens its journal, reads
    /// its ledger or looks at the screen — so the claim stands by the time
    /// the agent it was started for paints (the harness card wraps a pane
    /// only under a live claim, `aterm-gui`'s `claude_card`). What the take
    /// found is said at the loop's start, where [`Self::take_claim`] says it
    /// ([`Self::claim_said`]). Beside a proven self-claim nothing is taken
    /// here: the start's take says so once.
    pub(super) fn claim_at_naming(&mut self) {
        if self.setup.beside.is_some() {
            return;
        }
        self.claimed_at_naming = true;
        if let Some(line) = self.try_claim() {
            self.claim_said.push(line);
        }
    }

    /// Before a request: renew a held claim, or try again for one another
    /// holder had, when [`CLAIM_RENEW`] has passed; a pending one before
    /// every request. What changed is kept for the next look to say
    /// ([`Self::claim_said`]).
    pub(super) fn renew_claim_if_due(&mut self) {
        // A stopped loop gives its claim back; it renews nothing.
        if self.stopped() {
            return;
        }
        let last = match &self.claim {
            Claim::Held { renewed } => Some(*renewed),
            Claim::Behind { tried, .. } => Some(*tried),
            Claim::Pending { .. } => None,
            Claim::Off
            | Claim::Unavailable { .. }
            | Claim::GivenBack { .. }
            | Claim::Beside { .. } => return,
        };
        if last.is_some_and(|t| self.fx.since(t) < self.setup.claim_renew) {
            return;
        }
        match self.set_claim() {
            Some(r) => {
                if let Some(line) = self.claim_from(&r) {
                    self.claim_said.push(line);
                }
            }
            // Not served: the loop's own request is about to find the
            // outage. Whose the session is, is not known until one is
            // answered: pending, asked again before the next request.
            None => {
                self.claim = Claim::Pending {
                    tried: self.fx.mono(),
                };
            }
        }
    }

    /// Say what the renewals changed since the last look.
    pub(super) fn claim_said(&mut self, review: &mut dyn Review) {
        for line in std::mem::take(&mut self.claim_said) {
            say_claim(&line, review);
        }
    }

    /// A write a watching-only loop may not send, and an input a loop whose
    /// claim is pending may not, refused locally as the server would refuse
    /// a turn: `ERR busy supervisor=<holder>` (`-` while pending).
    pub(super) fn claim_refuses(&self, args: &[&str]) -> Option<CtlReply> {
        let (holder, why) = match &self.claim {
            Claim::Behind { holder, .. } | Claim::Beside { holder } if is_write(args) => {
                (holder.as_str(), "this loop watches only")
            }
            Claim::Pending { .. } if is_input(args) => ("-", "the claim is not answered yet"),
            _ => return None,
        };
        Some(CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: format!("aterm-ctl: ERR busy supervisor={holder} ({why})\n"),
        })
    }

    /// The loop's last act: the claim released, when it is held — and only
    /// while it is still this loop's (`holder=`): a
    /// lease that lapsed in an outage may have been taken by another
    /// supervisor since, and a bare unset would clear that one's live claim.
    pub(super) fn release_claim(&mut self) {
        if matches!(self.claim, Claim::Held { .. }) {
            self.unset_claim();
        }
        self.claim = Claim::Off;
    }

    /// One `meta unset supervisor holder=<this loop>`, straight to the
    /// transport as [`Self::set_claim`] is: a claim that is no longer this
    /// loop's is left to its holder. What it answers changes nothing here.
    fn unset_claim(&mut self) {
        let mine = format!("holder={}", self.holder());
        let mut args: Vec<&str> = Vec::with_capacity(5);
        if let Some(sid) = &self.setup.sid {
            args.push(sid.as_str());
        }
        args.extend(["meta", "unset", "supervisor", mine.as_str()]);
        let _ = self.ctl.call(&args);
    }

    /// AUTOPILOT OFF's park (`lifecycle.rs`'s `park_while_off`): the claim
    /// given back, so the session looks as unsupervised as it is — `status
    /// supervisor=` names nobody on this loop's account and the server raises
    /// its boxes to the menu bar again — and [`Claim::GivenBack`], which
    /// nothing renews or retries ([`Self::renew_claim_if_due`]) until the
    /// park ends ([`Self::retake_claim`]). A held claim is released as at the
    /// loop's end ([`Self::release_claim`]: only while it is this loop's); so
    /// is a pending one, whose set may have landed though its answer did
    /// not. Behind another holder there is nothing of this loop's to release:
    /// only its retries stop, so it never wins the claim while parked — and
    /// the holder is kept (`GivenBack { behind }`), so the retake that finds
    /// it again does not say it again. A host with no claim, and a claim
    /// already given back, are left as they stand.
    pub(super) fn give_back_claim(&mut self) {
        let behind = match &self.claim {
            Claim::Off | Claim::Unavailable { .. } | Claim::GivenBack { .. } => return,
            Claim::Held { .. } | Claim::Pending { .. } => {
                self.unset_claim();
                None
            }
            Claim::Behind { holder, .. } | Claim::Beside { holder } => Some(holder.clone()),
        };
        self.claim = Claim::GivenBack { behind };
    }

    /// The park is over on a word that is not `off`: a claim it gave back
    /// ([`Self::give_back_claim`]) is taken again as at the loop's start
    /// ([`Self::take_claim`]) — held, behind another holder (said only where
    /// it is not the one the loop was behind as it parked, and a loop that
    /// yields then ends), unavailable, or pending on an outage. Any other
    /// claim is left as it stands.
    pub(super) fn retake_claim(&mut self, review: &mut dyn Review) {
        if matches!(self.claim, Claim::GivenBack { .. }) {
            self.take_claim(review);
        }
    }

    /// A loop that yields ([`SuperviseOpts::yield_when_held`]) behind
    /// another holder: the end it returns ([`CLAIM_HELD`] and the holder).
    pub(super) fn yield_to_holder(&self, opts: &SuperviseOpts) -> Option<String> {
        if !opts.yield_when_held {
            return None;
        }
        // Never beside a proven self-claim: the host started this loop for it.
        match &self.claim {
            Claim::Behind { holder, .. } => Some(format!("{CLAIM_HELD}{holder}")),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(code: i32, stderr: &str) -> CtlReply {
        CtlReply {
            code,
            stdout: if code == 0 {
                "OK\n".to_string()
            } else {
                String::new()
            },
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn a_busy_reply_names_the_other_holder() {
        let r = reply(1, "aterm-ctl: ERR busy supervisor=gui%3A42\n");
        assert_eq!(busy_holder(&r).as_deref(), Some("gui:42"));
        // Negative controls: another `busy`, and an unrelated error.
        assert_eq!(busy_holder(&reply(1, "aterm-ctl: ERR busy sink\n")), None);
        assert_eq!(busy_holder(&reply(1, "aterm-ctl: ERR denied\n")), None);
    }

    #[test]
    fn only_writes_are_refused_to_a_loop_that_watches() {
        for w in [
            &["@s-1", "key", "if=x", "1"][..],
            &["turn", "idle=600", "hi"],
            &["meta", "set", "attention", "owner=supervisor", "x"],
            &["meta", "unset", "attention", "owner=supervisor"],
            &["post", "to=@s-2", "kind=ask", "x"],
            &["task", "mark", "done", "why=marker"],
        ] {
            assert!(is_write(w), "{w:?}");
        }
        for r in [
            &["@s-1", "text", "--json"][..],
            &["status"],
            &["meta"],
            &["task"],
            &["await", "gone", "x", "timeout", "20000"],
            &["meta", "set", "supervisor", "x", "ttl=90000"],
        ] {
            assert!(!is_write(r), "{r:?}");
        }
        // A pending claim holds back input only: its badges still go.
        assert!(is_input(&["@s-1", "key", "if=x", "1"]));
        assert!(is_input(&["send", "if-gen=1.2", "--", "keep going"]));
        assert!(!is_input(&[
            "meta",
            "set",
            "attention",
            "owner=supervisor",
            "x"
        ]));
        assert!(!is_input(&["post", "to=@s-2", "kind=ask", "x"]));
        assert!(!is_input(&["task", "mark", "done", "why=marker"]));
    }
}
