// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fabric word grammar (astream's `docs/DESIGN-aterm-fabric.md` §3.2, §3.3, §4.1, §4.2):
//! pure string predicates for principals, topics, kinds, verdicts, and `via=`
//! chains, shared by `aterm-link`, `aterm-gui`, and `aterm-gui-fabric`.

use astream_wire::{ReceiptVerdict, WakeClass};

/// Principal class prefixes for `<src>` segments (§3.2): session, node, human, agent.
pub const PRINCIPAL_CLASSES: [&str; 4] = ["s-", "n-", "h-", "a-"];

const CLASS_PREFIX_LEN: usize = 2;

/// Maximum principal name length after its class prefix (§3.2).
pub const PRINCIPAL_NAME_MAX: usize = 32;

/// Maximum byte length of a bare principal: class prefix plus [`PRINCIPAL_NAME_MAX`].
pub const PRINCIPAL_MAX: usize = CLASS_PREFIX_LEN + PRINCIPAL_NAME_MAX;

/// Maximum topic name length.
pub const TOPIC_MAX: usize = PRINCIPAL_NAME_MAX;

/// Whether `p` is a well-formed bare principal (`s-`/`n-`/`h-`/`a-` plus `[a-z0-9-]{1,32}`).
#[inline]
#[must_use]
pub fn is_principal(p: &str) -> bool {
    let Some(name) = PRINCIPAL_CLASSES.iter().find_map(|c| p.strip_prefix(c)) else {
        return false;
    };
    (1..=PRINCIPAL_NAME_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `t` is a well-formed broadcast topic (`[a-z0-9][a-z0-9._-]{0,31}`).
#[inline]
#[must_use]
pub fn is_topic(t: &str) -> bool {
    let mut cs = t.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && t.len() <= TOPIC_MAX
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Closed set of `in` record kinds (§4.2) accepted by `deliver` and `await inbox`: astream's
/// [`WakeClass::ALL`] spelled, highest priority first (`control` ..= `note`).
pub const KINDS: [&str; WakeClass::ALL.len()] = {
    let [a, b, c, d, e, f, g, h, i] = WakeClass::ALL;
    [
        a.as_str(),
        b.as_str(),
        c.as_str(),
        d.as_str(),
        e.as_str(),
        f.as_str(),
        g.as_str(),
        h.as_str(),
        i.as_str(),
    ]
};

/// Whether `k` is one of [`KINDS`]: a word astream's [`WakeClass::parse`] reads.
#[inline]
#[must_use]
pub const fn is_kind(k: &str) -> bool {
    WakeClass::parse(k).is_some()
}

/// Kinds that settle an `ask`/`task` with `dl=` when carrying `re=<off>` (R8).
pub const ANSWER_KINDS: [&str; 3] = [
    WakeClass::Answer.as_str(),
    WakeClass::Report.as_str(),
    WakeClass::Ack.as_str(),
];

/// Receipt verdicts accepted by `inbox seen <id> <verdict>` and `deliver` (`verdict=`):
/// astream's [`ReceiptVerdict::ALL`] spelled, in its wire order (`handled`, `refused`,
/// `deferred`).
pub const VERDICTS: [&str; ReceiptVerdict::ALL.len()] = {
    let [a, b, c] = ReceiptVerdict::ALL;
    [a.as_str(), b.as_str(), c.as_str()]
};

/// Whether `v` is one of [`VERDICTS`]: a word astream's [`ReceiptVerdict::parse`] reads.
#[inline]
#[must_use]
pub const fn is_verdict(v: &str) -> bool {
    ReceiptVerdict::parse(v).is_ok()
}

/// Maximum relay hops in a `via=` chain (§6.7).
pub const VIA_MAX_HOPS: usize = 16;

/// Maximum byte length of a `via=` chain, separators included.
pub const VIA_MAX_BYTES: usize = VIA_MAX_HOPS * (PRINCIPAL_MAX + 1);

/// Whether `via` is 1..=[`VIA_MAX_HOPS`] comma-separated bare principals within [`VIA_MAX_BYTES`].
#[inline]
#[must_use]
pub fn via_ok(via: &str) -> bool {
    !via.is_empty()
        && via.len() <= VIA_MAX_BYTES
        && via.split(',').count() <= VIA_MAX_HOPS
        && via.split(',').all(is_principal)
}

/// A bus offset as an agent writes it: `<n>` or `@<n>`, the form `inbox`, `cause=` and a brief's
/// `head=` print it in; digits only (`u64::from_str` would take `+5`).
#[must_use]
pub fn parse_bus_offset(v: &str) -> Option<u64> {
    let digits = v.strip_prefix('@').unwrap_or(v);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Declares [`MirrorState`] and [`MirrorState::ALL`] from one variant list, so every word
/// `fabric status mirror=` can print is one a reader of the words enumerates.
macro_rules! mirror_states {
    ($($(#[$doc:meta])* $variant:ident => $word:literal,)*) => {
        /// The session log's bus mirror as `fabric status` says it (`mirror=<word>`).
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum MirrorState {
            $($(#[$doc])* $variant,)*
        }

        impl MirrorState {
            /// Every state, from the same list as the enum's.
            pub const ALL: &'static [MirrorState] = &[$(MirrorState::$variant,)*];

            /// The word `fabric status` prints after `mirror=`.
            #[inline]
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self {
                    $(MirrorState::$variant => $word,)*
                }
            }
        }
    };
}

mirror_states! {
    /// The bridge asked for the mirror in its `hello` and has reported nothing since.
    Starting => "starting",
    /// Mirroring (`link mirror on`).
    On => "on",
    /// Stopped, with its `:<reason>` (a [`MirrorReason`] said in this state).
    Stopped => "stopped",
    /// The bus holds as much of the mirror as it may (`capped:total`, [`MirrorReason::Total`]).
    Capped => "capped",
    /// The broker's retention window holds the next take back.
    Budget => "budget",
    /// An `auto` mirror over a broker that retains nothing.
    Unretained => "unretained",
    /// No bridge serves the logs this instance retains (with `tails=` and `tombstones=`).
    Unserved => "unserved",
}

impl MirrorState {
    /// The state a bridge reports as `link mirror <word>`: every one but the two the endpoint
    /// says itself ([`Self::Starting`], [`Self::Unserved`]).
    #[must_use]
    pub fn reported(word: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .filter(|s| !matches!(s, Self::Starting | Self::Unserved))
            .find(|s| s.word() == word)
    }

    /// The line a bridge reports this state with on its control lane, `link mirror <word>`,
    /// for a state it reports with no reason ([`Self::On`], [`Self::Budget`],
    /// [`Self::Unretained`]); a stop or a cap goes with its reason ([`MirrorReason::request`]).
    /// [`Self::Starting`] and [`Self::Unserved`] are the endpoint's own, which
    /// [`Self::reported`] refuses.
    #[must_use]
    pub fn request(self) -> String {
        format!("link mirror {}", self.word())
    }
}

/// Declares [`MirrorReason`] and [`MirrorReason::ALL`] from one variant list, each reason with
/// the [`MirrorState`] it is said in, so the words a bridge sends after `reason=` are one closed
/// set beside the states.
macro_rules! mirror_reasons {
    ($($(#[$doc:meta])* $variant:ident => $word:literal ($state:ident),)*) => {
        /// Why a bridge's mirror stopped or capped: the `reason=` of its `link mirror` line and
        /// the `:<reason>` `fabric status mirror=` prints after the state.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum MirrorReason {
            $($(#[$doc])* $variant,)*
        }

        impl MirrorReason {
            /// Every reason, from the same list as the enum's.
            pub const ALL: &'static [MirrorReason] = &[$(MirrorReason::$variant,)*];

            /// The reason's word.
            #[inline]
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self {
                    $(MirrorReason::$variant => $word,)*
                }
            }

            /// The state the reason is said in.
            #[inline]
            #[must_use]
            pub const fn state(self) -> MirrorState {
                match self {
                    $(MirrorReason::$variant => MirrorState::$state,)*
                }
            }
        }
    };
}

mirror_reasons! {
    /// The mirror's pinned batch, or its copies, could not be kept or read back.
    Pin => "pin" (Stopped),
    /// The endpoint refused a `log-take`, or answered one or a `log-landed` past its grammar.
    Endpoint => "endpoint" (Stopped),
    /// The broker refused a mirrored record for good.
    Refused => "refused" (Stopped),
    /// A `turns` mirror over a broker that retains nothing reached its lifetime total.
    Total => "total" (Capped),
}

impl MirrorReason {
    /// The line a bridge reports its mirror's stop or cap with, `link mirror <state>
    /// reason=<word>`.
    #[must_use]
    pub fn request(self) -> String {
        format!("link mirror {} reason={}", self.state().word(), self.word())
    }
}

/// Declares [`DeadReason`] and [`DeadReason::ALL`] from one variant list, so every word a post
/// can be retired undelivered with is one a reader of the words enumerates.
macro_rules! dead_reasons {
    ($($(#[$doc:meta])* $variant:ident => $word:literal,)*) => {
        /// Why an outbound post was retired undelivered: the word the waiting client is told
        /// (`ERR <word> id=<n>`), the bridge's `outbox sent … off=- reason=<word>`, and the session
        /// log's `mail-landed … why=<word>`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum DeadReason {
            $($(#[$doc])* $variant,)*
        }

        impl DeadReason {
            /// Every reason, from the same list as the enum's.
            pub const ALL: &'static [DeadReason] = &[$(DeadReason::$variant,)*];

            /// The reason's word.
            #[inline]
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self {
                    $(DeadReason::$variant => $word,)*
                }
            }
        }
    };
}

dead_reasons! {
    /// The bridge gave no reason (`outbox sent … off=-` alone).
    Undeliverable => "undeliverable",
    /// No known principal or live advertised session by that address (`state=exited` too).
    Unroutable => "unroutable",
    /// More than one node claims the session id and the address pins none.
    Ambiguous => "ambiguous",
    /// The broker refused the publish for good.
    Refused => "refused",
    /// The publish would open a subject past this node's producer budget.
    SubjectBudget => "subject-budget",
    /// A seamless update could not carry the queued post (aterm's own retirement).
    NotCarried => "not-carried",
    /// The bus log the reply's `re=` names was replaced (aterm's own retirement).
    LogReplaced => "log-replaced",
}

impl DeadReason {
    /// Whether aterm itself retired the post, not the bridge.
    #[inline]
    #[must_use]
    pub const fn aterms_own(self) -> bool {
        matches!(self, Self::NotCarried | Self::LogReplaced)
    }

    /// The reason a bridge's `reason=<word>` names: one of the bridge's own words, else
    /// [`Self::Undeliverable`] (a word this build does not know, or one only aterm retires with).
    #[must_use]
    pub fn told(word: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|r| !r.aterms_own() && r.word() == word)
            .unwrap_or(Self::Undeliverable)
    }
}

/// The words a help text names after `mirror=`, whether as `<a|b>`, `a|b` or one word: the
/// seam the tests holding `help fabric` and `aterm help fabric` to [`MirrorState`] read. Each
/// hands it only the text's reply slots (`help fabric`'s `OK ...` span, the page's `aterm ctl
/// fabric status` row), since prose that repeats a word would stand in for a dropped slot.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn mirror_words_named(text: &str) -> std::collections::BTreeSet<&str> {
    text.split("mirror=")
        .skip(1)
        .flat_map(|after| {
            let after = after.strip_prefix('<').unwrap_or(after);
            let end = after
                .find(|c: char| !c.is_ascii_lowercase() && c != '-' && c != '|')
                .unwrap_or(after.len());
            after[..end].split('|')
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// Exit word sent before closing bridge lanes (`EVENT * bye <word>` / `link bye <word>`).
/// Unannounced closes (crashes, `kill -9`) act like [`Bye::Handoff`]: nothing is withdrawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bye {
    /// Instance is quitting: bridge withdraws hosted sessions (`state=exited`).
    Quit,
    /// Seamless self-update committing: successor re-adopts the sessions.
    Handoff,
}

impl Bye {
    /// The wire word: `quit` or `handoff`.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Bye::Quit => "quit",
            Bye::Handoff => "handoff",
        }
    }

    /// Parses `"quit"` or `"handoff"`.
    #[inline]
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "quit" => Some(Bye::Quit),
            "handoff" => Some(Bye::Handoff),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "fabric_grammar_tests.rs"]
mod tests;
