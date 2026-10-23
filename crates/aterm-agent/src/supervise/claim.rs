// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor claim (`meta set supervisor <holder> [ttl=<ms>]`): a loop that acts alone (the in-GUI host's) says so on the session, so `status supervisor=` names who answers its prompts, the server stops raising the agent's own boxes to the menu bar while the claim is live, and a second supervisor on the same session learns it is second.

use super::*;

/// What a loop that yields to another holder ([`SuperviseOpts::yield_when_held`]) ends with, followed by the holder: the in-GUI host reads it as "parked behind another supervisor", not as a fault.
pub const CLAIM_HELD: &str = "supervisor claim held by ";

/// The claim's lease, ms, on the clock `t` ([`crate::harness::Timings::claim_ttl`]): the product's is 90 s, three renewals' worth.
pub(super) fn ttl_ms(t: &crate::harness::Timings) -> u64 {
    u64::try_from(t.claim_ttl.as_millis()).unwrap_or(u64::MAX)
}
/// How often the claim is renewed (and, behind another holder, tried).
pub const CLAIM_RENEW: Duration = Duration::from_secs(20);

/// Where the loop stands on the session's supervisor claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Not taken: a loop that does not act alone (the engine tests' stop-at-review `supervise`), or before the start.
    #[default]
    Off,
    /// Asked for and not answered (the request was not served): the loop presses and types nothing, and asks again before its next request.
    Pending { tried: Instant },
    /// Held by this loop; last set at `renewed`.
    Held { renewed: Instant },
    /// Another supervisor holds it: this loop watches only; last tried at `tried`.
    Behind { holder: String, tried: Instant },
    /// The host has no claim to take (`why`): the loop acts without one.
    Unavailable { why: String },
    /// A PROVEN SELF-CLAIM holds the session ([`Session::set_beside`]): the agent supervises itself, and this loop watches beside it.
    Beside { holder: String },
    /// Given back while the session's Autopilot is off ([`Session::give_back_claim`]): held by nobody on this loop's account, never renewed or tried, and taken again as the park ends ([`Session::retake_claim`]).
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

    /// Whether aterm's remedies that act from outside the agent (the stall's explicit restart) stand down: behind another supervisor, which answers for the session — never beside a proven self-claim, whose agent cannot restart itself out of a stall.
    pub(crate) const fn remedies_withheld(&self) -> bool {
        matches!(self, Claim::Behind { .. })
    }

    /// The other holder this loop last knew of: the one it watches behind,
    /// or the one it was behind as an Off park gave the claim back.
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

/// The verbs that type into the worker: what a loop whose claim is pending may not send, and what a stopped loop sends no more ([`Session::call`]).
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

/// The verbs a watching-only loop may not send: everything that types into the worker, every attention write, and the task record's mark (the supervisor that holds the session publishes its `done`).
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

/// The holder a loop no one named claims the session under: `aterm-supervise:<pid>`, this process's.
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
            let was_behind = self.claim.known_holder().map(str::to_string);
            self.claim = Claim::Held { renewed: now };
            return was_behind.map(|h| format!("CLAIMED supervisor={} (after {h})", self.holder()));
        }
        if let Some(holder) = busy_holder(r) {
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

    /// The unattended loop's first act: take the claim, and say where it stands.
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

    /// CLAIM AT NAMING (card engine §8, lane A4): the hosted loop's claim, asked for as its first request — before it opens its journal, reads its ledger or looks at the screen — so the claim stands by the time the agent it was started for paints (the harness card wraps a pane only under a live claim, `aterm-gui`'s `claude_card`).
    pub(super) fn claim_at_naming(&mut self) {
        if self.setup.beside.is_some() {
            return;
        }
        self.claimed_at_naming = true;
        if let Some(line) = self.try_claim() {
            self.claim_said.push(line);
        }
    }

    /// Before a request: once [`CLAIM_RENEW`] has passed, renew a held claim or retry one another holder had; retry a pending one before every request.
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

    /// A write a watching-only loop may not send, and an input a loop whose claim is pending may not, refused locally as the server would refuse a turn: `ERR busy supervisor=<holder>` (`-` while pending).
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

    /// The loop's last act: the claim released when held, and only while it is still this loop's (`holder=`): a lease that lapsed in an outage may be another supervisor's now, and a bare unset would clear it.
    pub(super) fn release_claim(&mut self) {
        if matches!(self.claim, Claim::Held { .. }) {
            self.unset_claim();
        }
        self.claim = Claim::Off;
    }

    /// One `meta unset supervisor holder=<this loop>`, straight to the
    /// transport as [`Self::set_claim`] is: a claim that is no longer this
    /// loop's is left to its holder.
    fn unset_claim(&mut self) {
        let mine = format!("holder={}", self.holder());
        let mut args: Vec<&str> = Vec::with_capacity(5);
        if let Some(sid) = &self.setup.sid {
            args.push(sid.as_str());
        }
        args.extend(["meta", "unset", "supervisor", mine.as_str()]);
        let _ = self.ctl.call(&args);
    }

    /// AUTOPILOT OFF's park (`lifecycle.rs`'s `park_while_off`): the claim given back, so the session looks as unsupervised as it is — `status supervisor=` names nobody on this loop's account and the server raises its boxes to the menu bar again — and [`Claim::GivenBack`], which nothing renews or retries ([`Self::renew_claim_if_due`]) until the park ends ([`Self::retake_claim`]).
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

    /// The park is over on a word that is not `off`: a claim it gave back ([`Self::give_back_claim`]) is taken again as at the loop's start ([`Self::take_claim`]) — held, behind another holder (said only where it is not the one the loop was behind as it parked, and a loop that yields then ends), unavailable, or pending on an outage.
    pub(super) fn retake_claim(&mut self, review: &mut dyn Review) {
        if matches!(self.claim, Claim::GivenBack { .. }) {
            self.take_claim(review);
        }
    }

    /// A loop that yields ([`SuperviseOpts::yield_when_held`]) behind another holder: the end it returns ([`CLAIM_HELD`] and the holder).
    pub(super) fn yield_to_holder(&self, opts: &SuperviseOpts) -> Option<String> {
        if !opts.yield_when_held {
            return None;
        }
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
