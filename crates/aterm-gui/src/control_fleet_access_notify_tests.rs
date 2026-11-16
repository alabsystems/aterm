// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The status item hears the notification a fleet verb run in this process posts: `aterm fleet
/// access`, a join and a leave announce a change for the state home they keep the fleet's state
/// in (`aterm_agent::fleet_access_announced_here`, the name for the home that aterm-agent's
/// `a_verbs_change_is_announced_for_the_state_home_it_keeps` holds `announcing`'s post to), and
/// the item registers on the change of the home the lock reads its level from
/// ([`fleet_access_changed`]). In this test binary both are its scratch state root. CONTROL: the
/// name is the home's, so another home's is not it.
/// NEGATIVE CONTROL: `fleet_access_changed` keyed on `aterm_types::dirs::logs_dir()` in place of
/// `fleet_access_home()`: "assertion `left == right` failed: the window hears the home a fleet
/// verb announces for", left `com.aterm.aterm.fleet-access.changed.8a540b994bf938da`, right
/// `….1ced502f13193b84` (that run's scratch folders' keys).
#[test]
fn the_window_hears_the_home_a_fleet_verb_announces_for() {
    let heard = fleet_access_changed().expect("a test binary names a state folder");
    let posted =
        aterm_agent::fleet_access_announced_here().expect("a test binary names both folders");
    assert_eq!(
        heard, posted,
        "the window hears the home a fleet verb announces for"
    );
    let home = fleet_access_home().expect("a state folder");
    assert_ne!(
        heard,
        aterm_agent::fleet_access::changed(&home.join("another")),
        "control: another home's notification is another"
    );
}

/// The lock's change is announced for the state home it stored the level under
/// ([`store_fleet_access_under`], which the window's [`store_fleet_access`] runs on
/// [`fleet_access_home`]), the name a window of that home hears, and for no other home. The post
/// is waited for until the stored home's ear heard it, so notifyd had handled it when the other
/// ear is asked. The scratch homes' names are this test's alone.
/// NEGATIVE CONTROL: `store_fleet_access_under` announced for `state.with_file_name("elsewhere")`:
/// "timed out after 60s waiting for: the stored home's ear to hear the change".
#[cfg(target_os = "macos")]
#[test]
fn the_locks_change_is_announced_for_the_home_it_stores_under() {
    use aterm_sysprobe::notify::Posts;
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-gui-fleet-access-notify-")
        .tempdir()
        .expect("a scratch directory");
    let (state, other) = (tmp.path().join("state"), tmp.path().join("other"));
    let mut ear = Posts::new(&aterm_agent::fleet_access::changed(&state)).expect("registers");
    let mut other_ear = Posts::new(&aterm_agent::fleet_access::changed(&other)).expect("registers");

    assert_eq!(
        store_fleet_access_under(&state, FleetAccess::ReadOnly, None),
        Ok(true),
        "a lower is stored whatever stands"
    );
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the stored home's ear to hear the change",
        || (ear.posted() == Ok(true)).then_some(()),
    );
    assert_eq!(
        other_ear.posted(),
        Ok(false),
        "another home's ear hears no change of this one"
    );
}
