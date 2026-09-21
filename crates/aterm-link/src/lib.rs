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
//! **Screen content is untrusted data, and no record becomes terminal input by
//! any path.** Since round 21 that is an ABSENCE, not a check: this crate
//! contains no function that writes to a PTY. A record of every kind, from
//! every principal, carrying any `re=`, `epoch=` or `gen=` it likes, becomes an
//! inbox row and nothing else.
//!
//! It used to be a fence. Exactly one record shape was converted to PTY bytes —
//! `/f/<F>/term/<node>/<sid>/in/<src>` — under §6.6's four conditions: the
//! `<src>` held the keyboard, the session was not held, the body's `epoch=`
//! equalled the live launch nonce, and `gen=` was absent or equal to the live
//! `content_seq:fp16`. Nothing in the tree wrote such a record; `aterm drive
//! --dial` is the cross-host driving path and it does not go through the bus.
//! The face, its conditions, its holder table and its feed journal are gone,
//! and `bridge`'s module header states what is left in their place. The SUBJECT
//! stays reserved in [`subject`] and the node ring still carries both `term`
//! grants, so an older node's record is still recognised for what it is — and
//! then not served.
//!
//! A `re=` is NEVER AN INPUT TO AN AUTHORITY DECISION. That is the whole of the
//! claim, and it is deliberately narrower than "read in one place": `re=` is
//! read as CORRELATION at the delivery seam, by the renderers, and by the
//! deadline machinery — [`bridge::Bridge::deliver_record`]
//! forwards it onto the `deliver` line (and reads it to SETTLE the deadline it
//! names and to flag a reply that arrives after `expired` as `late=1`),
//! [`bridge::Bridge::expire_deadlines`] reads the asker's own lane to learn
//! whether an `answer`/`report`/`ack` carrying it arrived before it publishes a
//! verdict, this crate's own [`fabric`] report reads it to fold `answer`s
//! against overdue asks for the `aterm fabric` WARNINGS, `aterm-gui`'s `fabric`
//! resolves it against the recipient's OWN outbound posts into the `re-id=` a
//! reader sees, and [`mirror`] prints it. A correlation label is not
//! a permission — a deadline verdict is a notification, never a keystroke or an
//! admission: none of those readers can grant, apply, admit or deliver anything
//! on the strength of one,
//! and a `re=` a sender chose can therefore point a rendered row at an
//! unrelated post of the recipient's — which is why the row also carries the
//! sender's trust label, and why nothing downstream may treat `re-id=` as
//! proof of who answered. §6.6 states why the authority half has to hold: "a
//! `re=` is dense and guessable and a `gen=` is observable from presence, so a
//! body that could *trigger* keystrokes on their strength would let any
//! lane-writer drive a worker."
//!
//! There is no function in this crate that writes to a PTY. `bridge`'s module
//! header states the full argument, and its own doc test asserts the absence by
//! name rather than describing it.

pub mod body;
pub mod bridge;
/// The bridge command line, as a library entry - see [`cli::dispatch`].
pub mod cli;
pub mod ctl;
/// `aterm fabric on|off|doctor` — see [`enable::on`].
pub mod enable;
/// `aterm fabric` — the fabric's state on one screen, and its traffic live.
pub mod fabric;
pub mod glance;
pub mod hook;
/// `aterm fabric mint-for|join` — a SECOND HOST joins the fleet over the
/// sealed transport; see [`join::join`].
pub mod join;
/// A minimal JSON value, for the one document `hook install --merge` edits.
pub mod json;
pub mod mailbox;
pub mod mirror;
pub mod notify;
pub mod pct;
/// The presence row's meaning fields (`role= detail= phase= context= title=`).
pub mod presence;
pub mod render;
pub mod state;
pub mod subject;
pub mod transport;

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
    /// IT USED TO PIN A FENCE AND NOW IT PINS AN ABSENCE. §6.6's fence had
    /// FOUR conditions and the fourth was a DISJUNCTION — `gen=` absent, or
    /// equal to the live `content_seq:fp16` — and a header saying the body's
    /// `gen=` must MATCH described a stricter wall than the code kept, so an
    /// auditor would conclude a stale screen could not be answered without a
    /// live `gen=` and stop looking. Commit 36d2f973 over-tightened the
    /// paragraph in the very edit meant to stop it misdescribing the fence,
    /// which is why the wording was pinned rather than merely reviewed.
    ///
    /// Round 21 cut the drive face, so there is no fence left to overstate and
    /// the failure mode inverts: what a later author could now write is a
    /// present-tense description of a check this crate does not perform. The
    /// assertions below therefore require the ABSENCE to be stated and refuse
    /// the old conditional wording.
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
        // THE CLAIM IS NOW AN ABSENCE. Round 21 cut the drive face, so the
        // header no longer describes a fence with conditions to overstate — it
        // states that nothing converts a record to keystrokes at all, and the
        // thing that can drift is a later author re-describing a check.
        assert!(
            flat.contains("no record becomes terminal input by any path"),
            "the header must state the absence, not a condition on a path:\n{flat}"
        );
        assert!(
            !flat.contains("Exactly ONE record shape is ever converted"),
            "the header describes a live fence this crate no longer has:\n{flat}"
        );
        // AND THE HISTORY MAY BE TOLD, but only in the past tense: the
        // paragraph that explains what was removed must not read as a
        // description of what runs.
        assert!(
            flat.contains("It used to be a fence"),
            "say what was removed, so an auditor reading §6.6 against this crate \
             is not left to wonder where it went:\n{flat}"
        );
    }

    /// THE `re=` SENTENCE IS A CLAIM TOO, and the one that drifted.
    ///
    /// It used to read "a re= is read in one place only — replay". It was read
    /// in at least four: `bridge::Bridge::deliver_record` forwards it onto the
    /// `deliver` line, `aterm-gui`'s `fabric::deliver_row` turns it into
    /// `re-id=<post id>` against the RECIPIENT's own posts, and `mirror` and
    /// `fabric` render it. An auditor who believed the old sentence stopped
    /// looking outside `replay` and never saw the seam where a sender-chosen
    /// offset becomes a correlation the reader trusts — and round 21 cut
    /// `replay` itself, so the one place the sentence named is now the one
    /// place it is NOT read.
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
        // codec itself and `bridge` is named in prose above; the rest are named
        // by module. A new reader fails here until the sentence grows.
        let named = ["bridge", "mirror", "fabric"];
        for (module, source) in [
            ("bridge", include_str!("bridge.rs")),
            ("mirror", include_str!("mirror.rs")),
            ("body", include_str!("body.rs")),
            ("glance", include_str!("glance.rs")),
            ("notify", include_str!("notify.rs")),
            ("mailbox", include_str!("mailbox.rs")),
            ("state", include_str!("state.rs")),
            ("subject", include_str!("subject.rs")),
            ("ctl", include_str!("ctl.rs")),
            ("hook", include_str!("hook.rs")),
            ("json", include_str!("json.rs")),
            ("fabric", include_str!("fabric.rs")),
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
