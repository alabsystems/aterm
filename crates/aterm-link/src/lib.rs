// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-link** — the fabric bridge (`DESIGN-aterm-fabric.md` §11.2).
//!
//! One per aterm instance, launched BY the instance as a child holding the far
//! ends of two `socketpair(AF_UNIX)`s at fds 3 and 4. It is the *node*: the bus
//! principal under which every session the instance hosts is addressed and
//! attested.
//!
//! ## The two rules the whole crate exists to keep
//!
//! **aterm's engine owns zero I/O.** Everything on this side of the socket —
//! every broker connection, every capability, every byte of the fabric wire —
//! lives in this process. aterm speaks only its own control protocol, and the
//! endpoint it exposes (`aterm-gui`'s `fabric` module) is state, not I/O.
//!
//! **Screen content is untrusted data.** A record's body can never become
//! terminal input by any path. Exactly ONE record shape is ever converted to
//! PTY bytes — `/f/<F>/term/<node>/<sid>/in/<src>`, and only when `<src>` holds
//! the keyboard, the session is not held, the body's `epoch=` equals the live
//! launch nonce, and `gen=` is absent or equal to the live `content_seq:fp16`
//! (§6.6). That fourth condition is the one this sentence must not overstate:
//! `gen=` is what an APPROVAL PROMPT carries so that a stale screen cannot be
//! answered, and a drive record that omits it is applied on the holder, the
//! hold and the epoch alone. Every other record, of every kind, from every
//! principal, carrying any `re=` or `gen=` it likes, becomes an inbox row and
//! nothing else.
//! [`bridge::Bridge::on_inbox_record`] and [`bridge::Bridge::on_term_record`]
//! are separate functions over separate subscriptions for exactly that reason:
//! there is no code path from one to the other to audit.
//!
//! A `re=` is NEVER AN INPUT TO AN AUTHORITY DECISION. That is the whole of the
//! claim, and it is deliberately narrower than "read in one place": `re=` is
//! read for the auditor in [`replay`], where it is causality and authority for
//! nobody, and it is read as CORRELATION at the delivery seam and by the two
//! renderers — [`bridge::Bridge::deliver_record`] forwards it onto the
//! `deliver` line, `aterm-gui`'s `fabric` resolves it against the recipient's
//! OWN outbound posts into the `re-id=` a reader sees, and [`mirror`] and
//! [`tui`] print it. A correlation label is not a permission: none of those
//! readers can grant, apply, admit or deliver anything on the strength of one,
//! and a `re=` a sender chose can therefore point a rendered row at an
//! unrelated post of the recipient's — which is why the row also carries the
//! sender's trust label, and why nothing downstream may treat `re-id=` as
//! proof of who answered. §6.6 states why the authority half has to hold: "a
//! `re=` is dense and guessable and a `gen=` is observable from presence, so a
//! body that could *trigger* keystrokes on their strength would let any
//! lane-writer drive a worker."
//!
//! The one function that writes to a PTY is `Bridge::feed`, and its two callers
//! are `Bridge::on_term_record` and `Bridge::resolve_pending_feed` — the crash
//! recovery of §6.5, which can only re-feed an offset the first already
//! journalled after passing every condition. `bridge`'s module header states the
//! full argument.

pub mod body;
pub mod bridge;
/// The bridge command line, as a library entry - see [`cli::dispatch`].
pub mod cli;
pub mod ctl;
pub mod glance;
pub mod handoff;
pub mod hook;
pub mod mailbox;
pub mod mirror;
pub mod notify;
pub mod pct;
pub mod replay;
pub mod state;
pub mod subject;
pub mod transport;
pub mod tui;

/// Milliseconds since the Unix epoch — the body's informational `t=`.
///
/// INFORMATIONAL is the whole of its contract: records carry no timestamp of
/// their own, nothing orders by it, and a broker-stamped time is a named seed
/// (§4.1, §14). A clock that jumps costs a confusing display and nothing else.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    /// THIS CRATE'S HEADER IS A CLAIM, and aterm has no evidence manifest to
    /// check it against — so the claim is checked here, against itself.
    ///
    /// §6.6's fence has FOUR conditions and the fourth is a DISJUNCTION:
    /// `gen=` absent, or equal to the live `content_seq:fp16`. A header that
    /// says the body's `gen=` must match describes a stricter wall than
    /// [`bridge::Bridge::on_term_record`] keeps, and an auditor reading it
    /// would conclude that a stale screen cannot be answered without a live
    /// `gen=` and stop looking. Commit 36d2f973 over-tightened this paragraph
    /// in the very edit that set out to stop it misdescribing the fence, which
    /// is why the wording is now pinned rather than merely reviewed.
    #[test]
    fn the_untrusted_data_claim_states_the_fence_the_code_actually_keeps() {
        let flat = include_str!("lib.rs")
            .lines()
            .filter_map(|l| l.strip_prefix("//!"))
            .collect::<Vec<_>>()
            .join(" ")
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            flat.contains("gen= is absent or equal to the live content_seq:fp16"),
            "the header must state §6.6's fourth condition as the disjunction it \
             is — an absent gen= passes the fence:\n{flat}"
        );
        assert!(
            !flat.contains("epoch= and gen= match"),
            "the header promises a gen= the fence does not require:\n{flat}"
        );
    }

    /// THE `re=` SENTENCE IS A CLAIM TOO, and the one that drifted.
    ///
    /// It used to read "a re= is read in one place only — replay". It is read in
    /// at least four: `bridge::Bridge::deliver_record` forwards it onto the
    /// `deliver` line, `aterm-gui`'s `fabric::deliver_row` turns it into
    /// `re-id=<post id>` against the RECIPIENT's own posts, and `mirror` and
    /// `tui` render it. An auditor who believed the old sentence stopped looking
    /// outside `replay` and never saw the seam where a sender-chosen offset
    /// becomes a correlation the reader trusts.
    ///
    /// So the claim is narrowed to the one that holds — never an AUTHORITY
    /// input — and pinned twice: the narrow sentence must be there, the broad
    /// one must not, and the readers must actually still be readers. `.re` in
    /// this crate is grepped rather than trusted, because the failure mode is a
    /// FIFTH reader arriving with nobody updating the sentence.
    #[test]
    fn the_re_claim_names_correlation_and_no_reader_of_re_is_missing_from_it() {
        let src = include_str!("lib.rs");
        let flat = src
            .lines()
            .filter_map(|l| l.strip_prefix("//!"))
            .collect::<Vec<_>>()
            .join(" ")
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            flat.contains("re= is NEVER AN INPUT TO AN AUTHORITY DECISION"),
            "the header must state the claim that holds:\n{flat}"
        );
        assert!(
            !flat.contains("re= is read in one place"),
            "the header claims a seam the delivery path contradicts:\n{flat}"
        );
        // EVERY MODULE THAT READS A BODY'S `re=` MUST BE NAMED. `body` is the
        // codec itself and `replay`/`bridge` are named in prose above; the rest
        // are named by module. A new reader fails here until the sentence grows.
        let named = ["replay", "bridge", "mirror", "tui"];
        for (module, source) in [
            ("bridge", include_str!("bridge.rs")),
            ("mirror", include_str!("mirror.rs")),
            ("tui", include_str!("tui.rs")),
            ("replay", include_str!("replay.rs")),
            ("body", include_str!("body.rs")),
            ("glance", include_str!("glance.rs")),
            ("notify", include_str!("notify.rs")),
            ("handoff", include_str!("handoff.rs")),
            ("mailbox", include_str!("mailbox.rs")),
            ("state", include_str!("state.rs")),
            ("subject", include_str!("subject.rs")),
            ("ctl", include_str!("ctl.rs")),
            ("hook", include_str!("hook.rs")),
            ("transport", include_str!("transport.rs")),
        ] {
            let reads = source
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .any(|l| {
                    l.contains(".re ")
                        || l.contains(".re;")
                        || l.contains(".re)")
                        || l.contains(".re,")
                });
            assert_eq!(
                reads,
                named.contains(&module) || module == "body",
                "`{module}` reads a body's `re=` but the crate header does not \
                 name it (or is named and no longer reads one)"
            );
        }
    }
}
