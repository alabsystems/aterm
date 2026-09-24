// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The SUPERVISOR CLAIM (lane C's `meta set supervisor <holder> [ttl=<ms>]`):
//! a loop that acts alone (`watch`, the in-GUI host) says so on the session,
//! so `status supervisor=` names who answers its prompts, the server stops
//! raising the agent's own boxes to the menu bar while the claim is live
//! (the loop's keyed escalation does instead), and a SECOND supervisor on
//! the same session learns it is second.
//!
//! * **Taken at the start** of an unattended loop, before its first look,
//!   as a lease: `ttl=`[`CLAIM_TTL_MS`], renewed every [`CLAIM_RENEW`] by
//!   the next request the loop makes ([`Session::renew_claim_if_due`]), so a
//!   loop that dies — a crash, a kill — lets it lapse within the TTL and the
//!   server hands the session's box back to the human. A lease, not the
//!   connection-scoped form: the CLI's transport may open a connection per
//!   request, and a connection-scoped claim would lapse with the first one.
//!   Every wait of the loop is bounded by its 20 s step, so a request comes
//!   at least that often.
//! * **Released at the end** (`meta unset supervisor`), only when held.
//! * **Another holder** (`ERR busy supervisor=<holder>`): the loop WATCHES
//!   ONLY — it says so once (`WATCHING …`), decides no box (every one is
//!   escalated to the review with the holder named), dismisses no survey,
//!   types no probe, raises and clears no badge, and any write that would
//!   still reach the wire is refused locally ([`Session::claim_refuses`]).
//!   It tries for the claim again at every renewal, and says `CLAIMED …`
//!   the moment it has it.
//! * **A claim request not served** (an outage — at the start, or a
//!   renewal): PENDING. The loop does not know whose the session is, so it
//!   types and presses NOTHING ([`Session::claim_refuses`]: its input verbs
//!   are refused locally, its badges still go) and asks again before its
//!   very next request, until one is answered. A held claim whose renewal
//!   went unanswered is pending too: the lease may have lapsed in the
//!   outage and another supervisor taken it (lane B2's review — a pending
//!   loop used to act as if it had none).
//! * **A host without the claim** (a build before lane C: `ERR usage`, or an
//!   edge token: `ERR denied`): the loop acts as it did before the claim
//!   existed and says so once (`CLAIM unavailable …`).
//!
//! No clock of its own and no polling: the renewal rides the requests the
//! loop makes anyway.

use super::*;

/// What a loop that yields to another holder ([`SuperviseOpts::yield_when_held`])
/// ends with, followed by the holder: the in-GUI host reads it as "parked
/// behind another supervisor", not as a fault.
pub const CLAIM_HELD: &str = "supervisor claim held by ";

/// The claim's lease, ms: three renewals' worth.
pub(super) const CLAIM_TTL_MS: u64 = 90_000;
/// How often the claim is renewed (and, behind another holder, tried).
pub(super) const CLAIM_RENEW: Duration = Duration::from_secs(20);

/// Where the loop stands on the session's supervisor claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Not taken: a loop that does not act alone (`supervise`), or before
    /// the start.
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
}

impl Claim {
    /// The other holder, when this loop is watching behind one.
    pub(crate) fn watching_behind(&self) -> Option<&str> {
        match self {
            Claim::Behind { holder, .. } => Some(holder),
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

/// A claim line: `WATCHING …` is said — the watch's reader must know this
/// loop answers nothing — and the rest (`CLAIMED`, `CLAIM unavailable`) are
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
/// the worker, and every attention write.
fn is_write(args: &[&str]) -> bool {
    let words: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('@'))
        .collect();
    matches!(
        words.as_slice(),
        ["key" | "send" | "turn" | "paste" | "feed-bin" | "post", ..]
            | ["meta", "set" | "unset", "attention", ..]
    )
}

impl<C: Ctl> Session<'_, C> {
    /// The name this loop claims the session under
    /// ([`Self::set_supervisor_name`]).
    fn holder(&self) -> String {
        self.supervisor_name
            .clone()
            .unwrap_or_else(|| format!("aterm-supervise:{}", std::process::id()))
    }

    /// One `meta set supervisor <holder> ttl=<ms>`, straight to the
    /// transport (it is no request of the loop's, and it is made from
    /// inside [`Self::call`]); `None` when it was not served.
    fn set_claim(&mut self) -> Option<CtlReply> {
        let holder = self.holder();
        let ttl = format!("ttl={}", self.claim_ttl_ms);
        let mut args: Vec<&str> = Vec::with_capacity(5);
        if let Some(sid) = &self.sid {
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
        let now = Instant::now();
        if r.ok() {
            // Taken quietly (`status supervisor=` says it); only a claim
            // won from behind another holder is news.
            let was_behind = self.claim.watching_behind().map(str::to_string);
            self.claim = Claim::Held { renewed: now };
            return was_behind.map(|h| format!("CLAIMED supervisor={} (after {h})", self.holder()));
        }
        if let Some(holder) = busy_holder(r) {
            let said = self.claim.watching_behind() != Some(holder.as_str());
            self.claim = Claim::Behind {
                holder: holder.clone(),
                tried: now,
            };
            return said.then(|| {
                format!(
                    "WATCHING another supervisor holds this session: {holder} — nothing is \
                     pressed, typed or badged by this one"
                )
            });
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
        match self.set_claim() {
            Some(r) => {
                if let Some(line) = self.claim_from(&r) {
                    say_claim(&line, review);
                }
            }
            None => {
                self.claim = Claim::Pending {
                    tried: Instant::now()
                        .checked_sub(self.claim_renew)
                        .unwrap_or_else(Instant::now),
                };
            }
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
            Claim::Off | Claim::Unavailable { .. } => return,
        };
        if last.is_some_and(|t| t.elapsed() < self.claim_renew) {
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
                    tried: Instant::now(),
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
            Claim::Behind { holder, .. } if is_write(args) => {
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
    /// while it is still this loop's (`holder=`, lane E's server form): a
    /// lease that lapsed in an outage may have been taken by another
    /// supervisor since, and a bare unset would clear that one's live claim.
    pub(super) fn release_claim(&mut self) {
        if matches!(self.claim, Claim::Held { .. }) {
            let mine = format!("holder={}", self.holder());
            let mut args: Vec<&str> = Vec::with_capacity(5);
            if let Some(sid) = &self.sid {
                args.push(sid.as_str());
            }
            args.extend(["meta", "unset", "supervisor", mine.as_str()]);
            let _ = self.ctl.call(&args);
        }
        self.claim = Claim::Off;
    }

    /// A loop that yields ([`SuperviseOpts::yield_when_held`]) behind
    /// another holder: the end it returns ([`CLAIM_HELD`] and the holder).
    pub(super) fn yield_to_holder(&self, opts: &SuperviseOpts) -> Option<String> {
        if !opts.yield_when_held {
            return None;
        }
        self.claim
            .watching_behind()
            .map(|holder| format!("{CLAIM_HELD}{holder}"))
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
        ] {
            assert!(is_write(w), "{w:?}");
        }
        for r in [
            &["@s-1", "text", "--json"][..],
            &["status"],
            &["meta"],
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
    }
}
