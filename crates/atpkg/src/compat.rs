// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `supports_toolchain` range invariant (§16.3) — the anti-brick backbone of the
//! unified (aterm-as-index-member) model.
//!
//! aterm is *loosely-coupled*: it is outside the `rustc` coherence tuple and instead
//! declares a `supports_toolchain` range (e.g. `">=0.60, <0.70"`) over the toolchain
//! version it can drive. Because the tools flip immediately but the app self-swaps on next
//! launch, a `new-tools / old-app` intermediate is unavoidable; the range makes it
//! **provably safe** — every reachable state is runnable iff the *running* app's range
//! includes the *live* tools' version. [`supports`] is that check.
//!
//! **Fail-closed.** An empty/garbage range or an unparseable version returns `false`: we
//! never *claim* a compatibility we cannot prove (claiming it falsely is the brick vector
//! the design calls out). The range itself must additionally be an EXECUTED preflight gate
//! upstream (§16.3) — this function is the runtime predicate, not the authoring check.

use std::cmp::Ordering;

/// A single range comparator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `>=`
    Ge,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `<`
    Lt,
    /// `=` / `==` / bare
    Eq,
}

/// Whether `version` satisfies the comma-separated `supports_toolchain` `range`. All
/// comparators must hold (AND, like Cargo/npm). Fail-closed: an empty/blank range, a stray
/// empty clause, an unknown comparator, or an unparseable version/operand yields `false`.
#[must_use]
pub fn supports(range: &str, version: &str) -> bool {
    let Some(v) = parse_version(version) else {
        return false;
    };
    let range = range.trim();
    if range.is_empty() {
        return false;
    }
    for clause in range.split(',') {
        let clause = clause.trim();
        if clause.is_empty() {
            return false; // a stray comma is malformed → fail closed
        }
        let (op, operand) = split_op(clause);
        let Some(o) = parse_version(operand) else {
            return false;
        };
        let ord = cmp_version(&v, &o);
        let ok = match op {
            Op::Ge => ord != Ordering::Less,
            Op::Gt => ord == Ordering::Greater,
            Op::Le => ord != Ordering::Greater,
            Op::Lt => ord == Ordering::Less,
            Op::Eq => ord == Ordering::Equal,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Split a clause's leading comparator from its version operand. A clause with no operator
/// is an exact match ([`Op::Eq`]).
fn split_op(clause: &str) -> (Op, &str) {
    for (prefix, op) in [
        (">=", Op::Ge),
        ("<=", Op::Le),
        ("==", Op::Eq),
        (">", Op::Gt),
        ("<", Op::Lt),
        ("=", Op::Eq),
    ] {
        if let Some(rest) = clause.strip_prefix(prefix) {
            return (op, rest.trim());
        }
    }
    (Op::Eq, clause)
}

/// Parse a dotted-numeric version (`"0.67.0"`) into its components. `None` on any
/// non-numeric or empty component (fail-closed — we never guess a version's ordering).
fn parse_version(s: &str) -> Option<Vec<u64>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.split('.').map(|p| p.trim().parse::<u64>().ok()).collect()
}

/// Component-wise version compare; the shorter version is zero-padded (`0.6` == `0.6.0`).
fn cmp_version(a: &[u64], b: &[u64]) -> Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_a_bounded_range() {
        assert!(supports(">=0.60, <0.70", "0.67"));
        assert!(supports(">=0.60, <0.70", "0.60")); // >= is inclusive
        assert!(supports(">=0.60, <0.70", "0.69.99"));
        assert!(supports(">=0.60, <0.70", "0.67.5")); // 3-component within range
    }

    #[test]
    fn outside_the_range_is_false() {
        assert!(!supports(">=0.60, <0.70", "0.70")); // < 0.70 excludes 0.70
        assert!(!supports(">=0.60, <0.70", "0.59"));
        assert!(!supports(">=0.60, <0.70", "1.0"));
    }

    #[test]
    fn exact_and_single_bound() {
        assert!(supports("0.67", "0.67"));
        assert!(supports("=0.67", "0.67.0")); // 0.67 == 0.67.0 (zero-pad)
        assert!(!supports("0.67", "0.68"));
        assert!(supports(">=0.60", "2.0"));
        assert!(!supports("<=0.60", "0.61"));
    }

    #[test]
    fn fail_closed_on_unparseable_or_empty() {
        assert!(
            !supports("", "0.67"),
            "empty range never claims compatibility"
        );
        assert!(!supports("   ", "0.67"));
        assert!(!supports(">=0.60, <0.70", "abc"), "unparseable version");
        assert!(!supports("garbage", "0.67"), "unparseable operand");
        assert!(
            !supports(">=0.60,", "0.67"),
            "stray trailing comma is malformed"
        );
        assert!(
            !supports(">=0.6.x", "0.67"),
            "non-numeric operand component"
        );
    }
}
