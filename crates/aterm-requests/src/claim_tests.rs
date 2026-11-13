// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::record::{EndWhy, Form, Open, Out, Via, WhySrc};
use crate::store::Request;
use std::collections::BTreeSet;

const OPENED: u64 = 1_794_233_460_000;
const BY: &str = "fleet:87be5228:s-0123456789abcdef0123";

#[cfg(unix)]
#[test]
fn normal_refuses_root_relative_dotdot_and_too_long() {
    assert_eq!(normal("x"), Err("is not an absolute path"));
    assert_eq!(normal("~/x"), Err("is not an absolute path"));
    assert_eq!(normal("/Users//a/../b"), Err("names a `..`"));
    assert_eq!(normal("/"), Err("is the root folder"));
    assert_eq!(normal("/./"), Err("is the root folder"));
    let long = format!("/{}", "é".repeat(43));
    assert_eq!(normal(&long), Err("is too long to record"));
    // NEGATIVE: a folder in the home, spelled with `.` and a trailing slash, is one claim.
    assert_eq!(normal("/Users//a/./x/").as_deref(), Ok("/Users//a/x"));
    let exact = format!("/{}", "a".repeat(CLAIM_PATH_BYTES - 1));
    assert_eq!(normal(&exact).as_deref(), Ok(exact.as_str()));
}

#[test]
fn born_in_counts_birth_inside_a_span_or_its_skew_and_not_before() {
    let spans = [
        Span {
            from_ms: 10_000,
            to_ms: Some(20_000),
        },
        Span {
            from_ms: 50_000,
            to_ms: None,
        },
    ];
    let skew = BIRTH_SKEW_MS;
    assert!(born_in(15_000, &spans, skew, None));
    assert!(born_in(10_000 - skew, &spans, skew, None));
    assert!(born_in(20_000 + skew, &spans, skew, None));
    assert!(born_in(90_000, &spans, skew, None), "a running span holds");
    // NEGATIVE: 1 ms outside a span's skew, and after the request ended.
    assert!(!born_in(10_000 - skew - 1, &spans, skew, None));
    assert!(!born_in(20_000 + skew + 1, &spans, skew, None));
    assert!(!born_in(90_000, &spans, skew, Some(60_000)));
    assert!(!born_in(15_000, &[], skew, None));
}

#[cfg(unix)]
#[test]
fn inside_finds_a_claim_above_and_not_a_sibling_with_a_shared_prefix() {
    let claims = ["/a/foo", "/b"];
    assert_eq!(inside("/a/foo", claims), Some("/a/foo"));
    assert_eq!(inside("/a/foo/bar", claims), Some("/a/foo"));
    assert_eq!(inside("/b/c/d", claims), Some("/b"));
    // NEGATIVE: a shared prefix is no folder holding it.
    assert_eq!(inside("/a/foobar", claims), None);
    assert_eq!(inside("/a", claims), None);
    // Nested claims (which no store records) answer the deepest, whatever their order.
    assert_eq!(inside("/a/foo/x", ["/a", "/a/foo"]), Some("/a/foo"));
    assert_eq!(inside("/a/foo/x", ["/a/foo", "/a"]), Some("/a/foo"));
}

#[cfg(unix)]
#[test]
fn within_finds_a_claim_under_a_folder_and_not_itself_or_a_sibling() {
    let claims = ["/a/foo/bar", "/b"];
    assert_eq!(within("/a", claims), Some("/a/foo/bar"));
    assert_eq!(within("/a/foo", claims), Some("/a/foo/bar"));
    // NEGATIVE: the claim itself, a folder in it, and a shared prefix.
    assert_eq!(within("/a/foo/bar", claims), None);
    assert_eq!(within("/a/foo/bar/x", claims), None);
    assert_eq!(within("/a/fo", claims), None);
    assert_eq!(within("/c", claims), None);
}

#[test]
fn alone_needs_a_bounded_span_of_its_own_and_none_of_anyone_elses() {
    let ran = |from_ms: u64, to_ms: Option<u64>| Span { from_ms, to_ms };
    let short = [ran(10_000, Some(20_000))];
    assert_eq!(alone(15_000, &short, &[]), Some(true));
    assert_eq!(
        alone(10_000 - BIRTH_SKEW_MS, &short, &[]),
        Some(true),
        "within the skew"
    );
    // Another's command then contests it, its own or not.
    assert_eq!(alone(15_000, &short, &[ran(14_000, None)]), Some(false));
    assert_eq!(
        alone(15_000, &[], &[ran(14_000, Some(16_000))]),
        Some(false)
    );
    // Unknown: no span of its own holds it, one still runs, one ran past ALONE_SPAN_MS.
    assert_eq!(alone(30_000, &short, &[]), None);
    assert_eq!(alone(15_000, &[ran(10_000, None)], &[]), None);
    assert_eq!(
        alone(
            15_000,
            &[ran(10_000, Some(10_000 + ALONE_SPAN_MS + 1))],
            &[]
        ),
        None
    );
    assert_eq!(
        alone(15_000, &[short[0], ran(12_000, None)], &[]),
        None,
        "every span of its own that holds it must be bounded"
    );
    // NEGATIVE: exactly ALONE_SPAN_MS is bounded.
    assert_eq!(
        alone(15_000, &[ran(10_000, Some(10_000 + ALONE_SPAN_MS))], &[]),
        Some(true)
    );
}

fn request() -> Request {
    Request {
        id: "r-20261109-1411-87be5228-01".to_owned(),
        opened_ms: OPENED,
        open: Open {
            via: Via::FleetShell,
            form: Form::Shell,
            node: String::new(),
            host: String::new(),
            by: BY.to_owned(),
            program: None,
            model: None,
            account: None,
            why_src: WhySrc::Given,
        },
        why: None,
        brief: None,
        tabs: Vec::new(),
        end: Some((Out::Failed, EndWhy::TabsLeft)),
        ended_ms: Some(OPENED + 60_000),
        roots: Vec::new(),
        claims: Vec::new(),
        touched: BTreeSet::new(),
        review: None,
        kept: None,
        replies: Vec::new(),
    }
}

fn claim(born_ms: Option<u64>, alone: Option<bool>) -> Claim {
    Claim {
        path: "/Users//a/x".to_owned(),
        src: ClaimSrc::Born,
        by: BY.to_owned(),
        tab: None,
        born_ms,
        alone,
    }
}

#[test]
fn keep_of_keeps_older_contested_unattributed_touched_unpushed_and_unread() {
    let now = OPENED + 120_000;
    let clean = Measured {
        at_ms: now,
        bytes: Some(1),
        complete: true,
        unpushed: Some(false),
    };
    let r = request();
    let born = Some(OPENED + 1_000);
    // NEGATIVE first: a born, alone, untouched, measured-clean claim may go.
    assert_eq!(
        keep_of(&r, &claim(born, Some(true)), Some(&clean), now),
        None
    );
    // Each reason alone.
    for old in [
        None,
        Some(OPENED - BIRTH_SKEW_MS - 1),
        Some(OPENED + 70_000),
    ] {
        assert_eq!(
            keep_of(&r, &claim(old, Some(true)), Some(&clean), now),
            Some(Keep::BornBefore),
            "{old:?}"
        );
    }
    assert_eq!(
        keep_of(&r, &claim(born, Some(false)), Some(&clean), now),
        Some(Keep::Contested)
    );
    // A named claim, or a birth no bounded span of its own accounts for, is unattributed.
    assert_eq!(
        keep_of(&r, &claim(born, None), Some(&clean), now),
        Some(Keep::Unattributed)
    );
    let mut touched = request();
    touched.touched.insert("/Users//a/x".to_owned());
    assert_eq!(
        keep_of(&touched, &claim(born, Some(true)), Some(&clean), now),
        Some(Keep::Touched)
    );
    let unpushed = Measured {
        unpushed: Some(true),
        ..clean
    };
    assert_eq!(
        keep_of(&r, &claim(born, Some(true)), Some(&unpushed), now),
        Some(Keep::Unpushed)
    );
    let unread = Measured {
        unpushed: None,
        ..clean
    };
    assert_eq!(
        keep_of(&r, &claim(born, Some(true)), Some(&unread), now),
        Some(Keep::Unmeasured)
    );
    assert_eq!(
        keep_of(&r, &claim(born, Some(true)), None, now),
        Some(Keep::Unmeasured)
    );
    // A running request's life runs to now.
    let mut running = request();
    running.end = None;
    running.ended_ms = None;
    assert_eq!(
        keep_of(
            &running,
            &claim(Some(OPENED + 70_000), Some(true)),
            Some(&clean),
            now
        ),
        None
    );
}
