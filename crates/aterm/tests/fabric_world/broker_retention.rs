// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE CONSTRUCTOR OPENS THE DEFAULT BROKER:
//! a world's in-process broker retains its log by aterm's
//! default rules (`aterm_link::broker_rules::default_rules`), the rules
//! `aterm link broker` opens with (`fabric_cli`'s
//! `the_broker_binary_advertises_the_default_rules`), and its `Hello`
//! advertises them; `World::without_retention` is the named broker that
//! retains by none and advertises none. Every wait is a one-minute hang
//! detector.

use super::harness::World;

/// The rules a fresh connection's `Hello` advertises on `w`'s broker.
fn advertised(w: &World) -> Option<astream_broker::Retention> {
    w.read(|c| {
        c.hello()?;
        Ok(c.retention_rules())
    })
    .unwrap_or_else(|e| panic!("hello: {e}"))
}

/// THE WORLD DEFAULT IS THE PRODUCTION DEFAULT: the default world's broker
/// advertises exactly `default_rules()` (its four rules), and a restart keeps
/// them, since astream retains and advertises only the rules set since its
/// open; `World::without_retention`'s broker advertises none, and keeps none
/// across its restart. NEGATIVE CONTROL, run: an `open_broker` without its
/// `retain` call fails it.
#[test]
fn the_default_worlds_broker_advertises_the_default_rules() {
    let mut w = World::boot("br-default", &[]);
    w.wait_ready();
    let rules = advertised(&w).expect("the default broker advertises its rules");
    assert_eq!(rules, aterm_link::broker_rules::default_rules());
    assert_eq!(
        rules.rules().len(),
        3,
        "three filtered rules and the default"
    );
    w.stop_broker();
    w.start_broker();
    assert_eq!(
        advertised(&w),
        Some(aterm_link::broker_rules::default_rules()),
        "a restarted broker sets them again"
    );

    let mut bare = World::without_retention("br-none");
    bare.wait_ready();
    assert_eq!(
        advertised(&bare),
        None,
        "CONTROL: no rules, none advertised"
    );
    bare.stop_broker();
    bare.start_broker();
    assert_eq!(advertised(&bare), None, "and none after its restart");
}
