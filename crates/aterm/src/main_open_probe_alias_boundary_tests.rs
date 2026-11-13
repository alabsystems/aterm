// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{AliasRoute, alias_route};

#[test]
fn a_window_alias_serves_its_open_probe_child_at_the_front_door() {
    let verb = aterm_containment::consent::OPEN_PROBE_VERB;
    assert_eq!(alias_route("aterm-gui", ""), AliasRoute::AliasWindow);
    assert_eq!(
        alias_route("aterm-windowed", "new-window"),
        AliasRoute::AliasWindowVerb
    );
    assert_eq!(alias_route("aterm-ctl", "status"), AliasRoute::Ctl);
    assert_eq!(alias_route("aterm-askpass", verb), AliasRoute::Askpass);
    let observed: Vec<_> = [
        "aterm",
        "aterm-cli",
        "aterm-gui",
        "aterm-windowed",
        "renamed-copy",
    ]
    .into_iter()
    .map(|name| (name, alias_route(name, verb)))
    .collect();
    assert!(
        observed
            .iter()
            .all(|(_, route)| *route == AliasRoute::FrontDoor),
        "a window alias must route its probe child before window mode: {observed:?}"
    );
}
