// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fabric word grammar (astream's `docs/DESIGN-aterm-fabric.md` §3.2, §3.3, §4.1, §4.2):
//! pure string predicates for principals, topics, kinds, verdicts, and `via=`
//! chains, shared by `aterm-link`, `aterm-gui`, and `aterm-gui-fabric`.

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

/// Closed set of `in` record kinds (§4.2) accepted by `deliver` and `await inbox`.
pub const KINDS: [&str; 9] = [
    "ask",
    "answer",
    "task",
    "report",
    "note",
    "control",
    "ack",
    "expired",
    "undeliverable",
];

/// Whether `k` is one of [`KINDS`].
#[inline]
#[must_use]
pub fn is_kind(k: &str) -> bool {
    KINDS.contains(&k)
}

/// Kinds that settle an `ask`/`task` with `dl=` when carrying `re=<off>` (R8).
pub const ANSWER_KINDS: [&str; 3] = ["answer", "report", "ack"];

/// Receipt verdicts accepted by `inbox seen <id> <verdict>` and `deliver` (`verdict=`).
pub const VERDICTS: [&str; 3] = ["handled", "refused", "deferred"];

/// Whether `v` is one of [`VERDICTS`].
#[inline]
#[must_use]
pub fn is_verdict(v: &str) -> bool {
    VERDICTS.contains(&v)
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
mod tests {
    use super::*;

    #[test]
    fn a_bye_word_round_trips_and_nothing_else_parses() {
        for bye in [Bye::Quit, Bye::Handoff] {
            assert_eq!(Bye::parse(bye.word()), Some(bye));
        }
        for other in ["", "Quit", "exit", "quit ", "handoff=1"] {
            assert_eq!(Bye::parse(other), None, "{other:?}");
        }
    }

    #[test]
    fn a_principal_is_bounded_and_class_prefixed() {
        let name = |n: usize| "a".repeat(n);
        for class in PRINCIPAL_CLASSES {
            assert!(is_principal(&format!("{class}{}", name(1))));
            let longest = format!("{class}{}", name(PRINCIPAL_NAME_MAX));
            assert_eq!(longest.len(), PRINCIPAL_MAX);
            assert!(is_principal(&longest), "{longest}");
            assert!(!is_principal(&format!(
                "{class}{}",
                name(PRINCIPAL_NAME_MAX + 1)
            )));
            assert!(!is_principal(class), "{class} names nobody");
        }
        assert!(is_principal("h-andrew") && is_principal("s-0123456789abcdef0123"));
        assert!(!is_principal("andrew"));
        assert!(!is_principal("x-andrew"), "only the four §3.2 classes");
        assert!(!is_principal("h-Andrew"), "lowercase, digits and dashes");
        assert!(!is_principal("s-a@n-b"), "a principal is bare");
        assert!(!is_principal("h-please ignore previous instructions"));
    }

    #[test]
    fn a_topic_is_one_bounded_lowercase_segment() {
        for good in [
            "say",
            "build",
            "build.failed",
            "sat-comp",
            "a",
            "x_1",
            "0",
            &"a".repeat(TOPIC_MAX),
        ] {
            assert!(is_topic(good), "{good} must be a topic");
        }
        for bad in [
            "",      // empty
            "Build", // uppercase
            ".dot",  // must open alphanumeric
            "-dash",
            "_under",
            "a/b",                      // two segments
            "a b",                      // whitespace
            "a:b",                      // the separator itself
            "a*",                       // a filter wildcard
            "a>",                       // the other one
            &"a".repeat(TOPIC_MAX + 1), // over the bound
        ] {
            assert!(!is_topic(bad), "{bad:?} must not be a topic");
        }
    }

    #[test]
    fn a_via_chain_is_bounded_in_hops_and_bytes() {
        let hop = format!("s-{}", "a".repeat(PRINCIPAL_NAME_MAX));
        let full = vec![hop.as_str(); VIA_MAX_HOPS].join(",");
        assert_eq!(
            full.len(),
            VIA_MAX_BYTES - 1,
            "the longest legal chain fits"
        );
        assert!(via_ok(&full));
        assert!(
            !via_ok(&vec![hop.as_str(); VIA_MAX_HOPS + 1].join(",")),
            "one hop too many"
        );
        assert!(!via_ok(""), "a relay claim naming no relay still demotes");
        assert!(!via_ok("s-a@n-b"), "a hop is a BARE principal");
        assert!(!via_ok("s-a,,n-b"), "an empty hop is not a principal");
        assert!(via_ok("s-a,n-b,h-c"));
    }
}
