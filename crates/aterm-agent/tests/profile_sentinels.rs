// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Allocation-size and typed-output controls for the allowlist profile reader.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aterm_agent::harness::profile;

/// The planted text. No allowlisted field of the composed file keeps it.
const SENTINEL: &str = "S3NT1NEL";

/// Smaller than the decoded secret below; every normal kept field is at most
/// `profile::WORD_MAX` bytes. A materialized secret crosses this bound.
const LARGE_REQUEST: usize = 64 * 1024;

static ARMED: AtomicBool = AtomicBool::new(false);
static LARGE_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static LARGE_BYTES: AtomicUsize = AtomicUsize::new(0);

struct Allocator;

fn note_request(size: usize) {
    if ARMED.load(Ordering::Relaxed) && size >= LARGE_REQUEST {
        LARGE_REQUESTS.fetch_add(1, Ordering::Relaxed);
        LARGE_BYTES.fetch_add(size, Ordering::Relaxed);
    }
}

// SAFETY: requests are forwarded unchanged to System. Only their sizes are counted; neither
// initialized contents nor allocation padding is read.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies the valid allocation layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            note_request(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller supplies the valid allocation layout.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            note_request(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the caller supplies System's allocation and its layout.
        let next = unsafe { System.realloc(ptr, layout, size) };
        if !next.is_null() {
            note_request(size);
        }
        next
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller supplies System's allocation and its layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

#[derive(Debug)]
struct Requests {
    large: usize,
    bytes: usize,
}

/// Measure allocation requests only. The guard disarms even if `f` unwinds.
fn requests<T>(f: impl FnOnce() -> T) -> (T, Requests) {
    struct Disarm;
    impl Drop for Disarm {
        fn drop(&mut self) {
            ARMED.store(false, Ordering::Relaxed);
        }
    }
    LARGE_REQUESTS.store(0, Ordering::Relaxed);
    LARGE_BYTES.store(0, Ordering::Relaxed);
    let disarm = Disarm;
    ARMED.store(true, Ordering::Relaxed);
    let answer = f();
    drop(disarm);
    let measured = Requests {
        large: LARGE_REQUESTS.load(Ordering::Relaxed),
        bytes: LARGE_BYTES.load(Ordering::Relaxed),
    };
    (answer, measured)
}

/// A composed profile: every allowlisted field, and the sentinel in each
/// field the real file can hold a credential or a person's name in — the
/// account id included, which is only ever hashed.
fn composed() -> String {
    let s = "S3NT1NEL";
    format!(
        r#"{{
  "numStartups": 42,
  "primaryApiKey": "sk-ant-{s}-primary",
  "customApiKeyResponses": {{"approved": ["{s}-approved"], "rejected": []}},
  "mcpServers": {{"tracker": {{"command": "tracker-mcp",
    "env": {{"TRACKER_TOKEN": "{s}-env"}},
    "headers": {{"Authorization": "Bearer {s} header\n"}}}}}},
  "projects": {{"/work/example": {{
    "mcpServers": {{"db": {{"env": {{"DB_PASSWORD": "{s}-project"}}}}}},
    "history": [{{"display": "{s} typed here", "pastedContents": {{}}}}]}}}},
  "oauthAccount": {{
    "accountUuid": "{s}-0f6c2b8e-1d3a-4c5e-9b7f",
    "emailAddress": "ana.{s}@example.com",
    "displayName": "Ana {s}",
    "fullName": "Ana {s} Example",
    "organizationName": "{s} Example Org",
    "organizationUuid": "{s}-org-uuid",
    "organizationType": "claude_max",
    "userRateLimitTier": "default_claude_max_5x",
    "seatTier": "standard",
    "billingType": "stripe_subscription",
    "subscriptionCreatedAt": "2026-01-02T03:04:05.678Z",
    "hasExtraUsageEnabled": true,
    "planDisplayName": "Max 5x",
    "profileFetchedAt": 1792000000123}},
  "cachedExtraUsageDisabledReason": null,
  "cachedUsageUtilization": {{"fetchedAtMs": 1792050000000,
    "accountUuid": "{s}-0f6c2b8e-1d3a-4c5e-9b7f",
    "utilization": {{
      "five_hour": {{"utilization": 62.5, "resets_at": "2026-10-15T18:05:00.123456+00:00"}},
      "seven_day": {{"utilization": 71, "resets_at": "2026-10-18T20:50:00Z"}},
      "limits": [{{"kind": "weekly_scoped", "percent": 40, "resets_at": null,
                  "scope": {{"model": {{"display_name": "Fable"}}}}}}],
      "extra_usage": {{"is_enabled": true, "monthly_limit": 20000, "used_credits": 4100,
                       "utilization": 20.5, "currency": "USD", "disabled_reason": null}},
      "wattle_ember": {{"utilization": 50, "label": "{s} label", "limit_dollars": 50}}}}}},
  "cachedGrowthBookFeatures": {{
    "tengu_saffron_credits_only_tiers": ["enterprise"],
    "tengu_saffron_lattice": {{"perModel": {{"claude-fable-5": {{"enabled": false}}}}}},
    "tengu_other_flag": {{"token": "{s}-flag"}}}}
}}"#
    )
}

#[test]
fn the_profile_reader_skips_large_escaped_secrets_without_materializing_them() {
    let secret = r"\u0053\u0033\u004e\u0054\u0031\u004e\u0045\u004c".repeat(16_384);
    let text = composed().replace("sk-ant-S3NT1NEL-primary", &secret);
    let bytes = text.as_bytes();

    let (answer, parsed) = requests(|| profile::parse(bytes));
    let answer = answer.expect("a profile");
    assert_eq!(answer.plan.word, Some(profile::PlanWord::Max));
    assert!(answer.key.is_some(), "the id was read, and hashed");
    assert!(answer.usage.is_some(), "the cache names the same account");
    assert_eq!(answer.fable, profile::FablePlan::Included);
    assert!(
        !format!("{answer:?}").contains(SENTINEL),
        "a composed credential or identity field reached the typed answer"
    );
    assert_eq!(
        parsed.large, 0,
        "a skipped secret was materialized: {parsed:?}"
    );

    let cut = text.get(..text.len() - 2).expect("cut");
    let (failed, parsed) = requests(|| profile::parse(cut.as_bytes()));
    assert_eq!(failed, Err(profile::NoProfile::Unreadable));
    assert_eq!(
        parsed.large, 0,
        "a failed parse materialized the secret: {parsed:?}"
    );

    let (whole, materialized) = requests(|| aterm_json::from_slice::<aterm_json::Value>(bytes));
    let whole = whole.expect("whole-file control");
    assert!(
        materialized.large > 0,
        "the control was not detected: {materialized:?}"
    );
    assert!(
        whole["primaryApiKey"]
            .as_str()
            .is_some_and(|s| s.contains(SENTINEL)),
        "the control must really hold the decoded secret"
    );

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-profile-sentinels-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join(".claude.json");
    std::fs::write(&path, bytes).expect("write");
    let (read, measured) = requests(|| profile::read(&path));
    assert_eq!(read, Ok(answer));
    assert!(
        measured.large > 0,
        "the input-buffer control was not measured"
    );
    assert!(
        measured.bytes <= bytes.len().saturating_add(1),
        "reading allocated more than its one bounded input buffer: {measured:?}"
    );
}
