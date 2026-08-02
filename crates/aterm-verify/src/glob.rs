// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `case … in` patterns the smokes decide on, ported as patterns.
//!
//! Every control-socket assertion in the script is a shell glob:
//!
//! ```text
//!   case "$got" in
//!     OK\ *sync_rel_timeout=0\ *) : ;;
//!     *) fail "…" ;;
//!   esac
//! ```
//!
//! These are not "contains" checks. `OK *sync_rel_timeout=0 *` requires the reply
//! to START with `OK `, to hold `sync_rel_timeout=0` FOLLOWED BY a space (so a
//! field at the very end of the line does not match, and `sync_rel_timeout=0` can
//! never be satisfied by `…sync_rel_timeout=01`), and — where two fields appear —
//! to hold them IN ORDER. Rewriting them as `contains` would quietly change what
//! the smokes decide, so the glob is ported instead.

/// Match `text` against a shell glob supporting `*` (any run, including empty).
/// There are no other metacharacters in any pattern this gate uses.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Classic linear backtracking matcher: `star`/`mark` remember the last `*`
    // and where to resume the text if the tail stops matching.
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_must_start_with_the_ok_prefix() {
        assert!(glob_match("OK *", "OK cursor row=1 col=1"));
        assert!(glob_match("OK *", "OK "));
        assert!(!glob_match("OK *", "ERR no such verb"));
        assert!(
            !glob_match("OK *", "OK"),
            "the space is part of the protocol"
        );
        assert!(
            !glob_match("OK *", "banner: something\nOK fine"),
            "a prefix banner breaks it"
        );
    }

    #[test]
    fn a_field_assertion_needs_the_trailing_space_the_shell_required() {
        let good = "OK frames=31 sync_rel_timeout=0 perf_reduced=0 wake_heals=0 x=1";
        assert!(glob_match("OK *sync_rel_timeout=0 *", good));
        assert!(glob_match("*perf_reduced=0 *", good));
        assert!(glob_match("*wake_heals=0 *", good));
        // Same reply, counters armed: every assertion flips.
        let bad = "OK frames=31 sync_rel_timeout=2 perf_reduced=1 wake_heals=3 x=1";
        assert!(!glob_match("OK *sync_rel_timeout=0 *", bad));
        assert!(!glob_match("*perf_reduced=0 *", bad));
        assert!(!glob_match("*wake_heals=0 *", bad));
    }

    #[test]
    fn a_longer_value_cannot_satisfy_a_zero_assertion() {
        // `sync_rel_timeout=0` inside `sync_rel_timeout=07` must NOT match: the
        // trailing space in the pattern is what stops it.
        assert!(!glob_match(
            "OK *sync_rel_timeout=0 *",
            "OK sync_rel_timeout=07 more"
        ));
        assert!(!glob_match("*wake_heals=0 *", "OK wake_heals=012 "));
    }

    #[test]
    fn two_fields_must_appear_in_the_patterns_order() {
        let p = "*redraw_retry_gated=0 *present_drops=0 *";
        assert!(glob_match(
            p,
            "OK redraw_retry_gated=0 present_drops=0 frames=40"
        ));
        assert!(!glob_match(
            p,
            "OK present_drops=0 redraw_retry_gated=0 frames=40"
        ));
    }

    #[test]
    fn empty_and_degenerate_patterns_behave() {
        assert!(glob_match("*", ""));
        assert!(glob_match("**", "anything"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(!glob_match("OK *", ""), "no reply is not an OK reply");
    }

    #[test]
    fn backtracking_finds_a_late_match() {
        assert!(glob_match("*ab*", "aaab"));
        assert!(glob_match("*a*b*c*", "xxaxxbxxc"));
        assert!(!glob_match("*a*b*c*", "xxaxxcxxb"));
    }
}
