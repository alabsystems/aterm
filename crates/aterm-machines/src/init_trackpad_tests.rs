// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, export, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran};

const READ: &str = "alice$ /usr/bin/defaults export -g -";
const WRITE: &str = "alice$ /usr/bin/defaults write -g com.apple.swipescrolldirection -bool false";

fn pref(value: bool) -> Ran {
    export(&format!(
        "<dict><key>{KEY}</key><{}/></dict>",
        if value { "true" } else { "false" }
    ))
}

fn context() -> Ctx {
    ctx(&[user("alice", 501)])
}

/// After a Restore of a scroll direction the Mac had never set, the saved key is gone and
/// the live direction stays off until the next login: System State reads Off, marked so,
/// and the row differs, as the Mac will be on natural scrolling after that login.
#[test]
fn natural_scrolling_off_until_the_next_login_reads_so() {
    let seen = |live: bool, saved: &str| {
        let sys = Fake::root();
        sys.scroll_state(live).on(READ, export(saved));
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Plan,
            &sys,
            &context(),
            &mut NoJournal,
        );
        let rows: Vec<&crate::model::ItemReport> = rows.iter().collect();
        crate::init_observe::observation(Setting::NaturalScrolling, &rows)
    };
    let restored = seen(false, "<dict></dict>");
    assert_eq!(
        restored.standing,
        aterm_types::macos_setup::Standing::Differs
    );
    assert_eq!(restored.now.as_deref(), Some("Off · until login"));
    assert_eq!(restored.after_login.as_deref(), Some("On"));
    let saved_on = seen(false, &format!("<dict><key>{KEY}</key><true/></dict>"));
    assert_eq!(saved_on.now.as_deref(), Some("Off · until login"));
    assert_eq!(saved_on.after_login.as_deref(), Some("On"));
    // On now reads On, whatever is saved.
    assert_eq!(seen(true, "<dict></dict>").now.as_deref(), Some("On"));
}

#[test]
fn a_correct_saved_setting_gets_exactly_one_native_refresh_without_a_defaults_write() {
    let sys = Fake::root();
    sys.scroll_state(false).on(READ, pref(false));
    let rows = engine::run(
        &items(&context()).unwrap(),
        Mode::Apply,
        &sys,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.as_str() == "scroll-direction set false")
            .count(),
        1
    );
    assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
}

#[test]
fn correct_persistence_with_stale_native_state_still_applies_immediately() {
    let sys = Fake::root();
    sys.scroll_state(true).on(READ, pref(false));
    let rows = engine::run(
        &items(&context()).unwrap(),
        Mode::Apply,
        &sys,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Changed);
    assert!(sys.calls().contains(&"scroll-direction set false".into()));
    assert!(!sys.calls().iter().any(|call| call.contains(" write ")));
}

#[test]
fn changing_the_preference_also_applies_native_state_and_checks_both_readbacks() {
    let sys = Fake::root();
    sys.scroll_state(true)
        .on(READ, pref(true))
        .on(READ, pref(true))
        .on(READ, pref(false));
    sys.on(WRITE, Ran::ok(""));
    let rows = engine::run(
        &items(&context()).unwrap(),
        Mode::Apply,
        &sys,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Changed);
    assert!(sys.calls().contains(&WRITE.into()));
    assert!(!sys.natural_scrolling().unwrap());
}

#[test]
fn a_refusing_native_setter_or_a_failed_runtime_readback_never_reports_success() {
    for mismatches in [false, true] {
        let sys = Fake::root();
        sys.scroll_state(true).on(READ, pref(false));
        if mismatches {
            sys.scroll_readback_after_set(true);
        } else {
            sys.refuse_scroll_set("synthetic native refusal");
        }
        let rows = engine::run(
            &items(&context()).unwrap(),
            Mode::Apply,
            &sys,
            &context(),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Failed);
    }
}

#[test]
fn a_persistent_readback_mismatch_is_also_a_failure() {
    let sys = Fake::root();
    sys.scroll_state(true)
        .on(READ, pref(true))
        .on(WRITE, Ran::ok(""));
    let rows = engine::run(
        &items(&context()).unwrap(),
        Mode::Apply,
        &sys,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert!(!sys.natural_scrolling().unwrap());
}

#[test]
fn plan_only_probes_preferences_and_native_state() {
    let sys = Fake::root();
    sys.scroll_state(true).on(READ, pref(true));
    let rows = engine::run(
        &items(&context()).unwrap(),
        Mode::Plan,
        &sys,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Planned);
    assert_eq!(sys.calls(), [READ, "scroll-direction get"]);
    // System State reads natural scrolling as on from the probe itself.
    assert_eq!(
        crate::init_observe::Reading::of(&rows[0]),
        Some(crate::init_observe::Reading::NaturalOn)
    );
}

#[test]
fn a_correct_setting_is_also_read_only_in_plan_and_a_failed_refresh_is_not_success() {
    let sys = Fake::root();
    sys.scroll_state(false).on(READ, pref(false));
    let rows = engine::run(
        &items(&context()).unwrap(),
        Mode::Plan,
        &sys,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert_eq!(sys.calls(), [READ, "scroll-direction get"]);
    sys.refuse_scroll_set("synthetic refresh refusal");
    let rows = engine::run(
        &items(&context()).unwrap(),
        Mode::Apply,
        &sys,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert_eq!(rows[0].why.as_deref(), Some("synthetic refresh refusal"));
}
