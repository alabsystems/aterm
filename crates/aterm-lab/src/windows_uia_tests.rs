// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const EVENTS: &str = "ready\n\
    start structure ChildrenInvalidated ControlType.Window \"aterm\"\n\
    type event TextChanged ControlType.Document \"terminal\"\n\
    type property Value ControlType.Text \"\"\n\
    type event TextChanged ControlType.Document \"terminal\"\n\
    blur focus ControlType.Pane \"Desktop\"\n\
    focus focus ControlType.Document \"terminal\"\n\
    stopped\n";
const TREE: &str = "tree 0 ControlType.Window Name=\"aterm\" FrameworkId=\"aterm\"\n\
    tree 0.0 ControlType.Document Name=\"terminal\" patterns=Text\n\
    pattern 0.0.text selection=None length=6 \"UIA-42\"\n";

#[test]
fn events_are_held_as_each_phases_distinct_set_and_counted_aside() {
    let r = compose(Arch::Aarch64, EVENTS, TREE);
    let get = |k: &str| {
        r.lines
            .iter()
            .find(|l| l.key == k)
            .map(|l| (l.class.mark(), l.value.as_str()))
    };
    assert_eq!(
        get("event.type"),
        Some((
            '=',
            "event TextChanged ControlType.Document \"terminal\" | property Value ControlType.Text \"\""
        ))
    );
    assert_eq!(get("event.blur").map(|(m, _)| m), Some('='));
    assert_eq!(get("event.start").map(|(m, _)| m), Some('~'));
    assert_eq!(
        get("event.type.count").map(|(m, v)| (m, v.starts_with("2x event TextChanged"))),
        Some(('~', true))
    );
    assert_eq!(get("tree.0.0").map(|(m, _)| m), Some('='));
    assert_eq!(
        get("pattern.0.0.text"),
        Some(('=', "selection=None length=6 \"UIA-42\""))
    );
    let parsed = Recording::parse(&r.render()).expect("a record parses back");
    assert!(compare(&r, &parsed).same_behaviour());
}

#[test]
fn a_planted_name_or_role_or_lost_event_is_a_difference_and_a_count_is_not() {
    let held = compose(Arch::X86_64, EVENTS, TREE);
    let more = EVENTS.replace(
        "focus focus",
        "type event TextChanged ControlType.Document \"terminal\"\nfocus focus",
    );
    assert!(
        compare(&held, &compose(Arch::X86_64, &more, TREE)).same_behaviour(),
        "how many times an event came is measured, not compared"
    );
    for (events, tree) in [
        (
            EVENTS.to_string(),
            TREE.replace("Name=\"terminal\"", "Name=\"planted\""),
        ),
        (
            EVENTS.to_string(),
            TREE.replace("0.0 ControlType.Document", "0.0 ControlType.Edit"),
        ),
        (
            EVENTS.replace("focus focus ControlType.Document \"terminal\"\n", ""),
            TREE.to_string(),
        ),
    ] {
        assert!(!compare(&held, &compose(Arch::X86_64, &events, &tree)).same_behaviour());
    }
}

#[test]
fn the_click_off_the_window_lands_on_the_wider_side() {
    let display = Display {
        primary: desk::parse_monitors(
            "monitor name=a rect=0,0,1920,1200 dpi=96 primary=1 scale=0/0/4 adapter=x\n",
        )
        .remove(0),
        second: desk::parse_monitors(
            "monitor name=b rect=1920,0,2720,600 dpi=96 primary=0 scale=- adapter=y\n",
        )
        .remove(0),
    };
    let left = desk::Rect {
        l: 100,
        t: 100,
        r: 900,
        b: 700,
    };
    assert_eq!(off_window(&display, &left), (1410, 400));
    let right = desk::Rect {
        l: 1000,
        t: 100,
        r: 1800,
        b: 700,
    };
    assert_eq!(off_window(&display, &right), (500, 400));
}

#[test]
fn arguments() {
    let s = |v: &[&str]| v.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();
    assert_eq!(
        parse_args(&s(&[])).unwrap(),
        Ask {
            arches: vec![Arch::Aarch64, Arch::X86_64],
            record: false
        }
    );
    assert_eq!(
        parse_args(&s(&["--arch", "x86_64", "--record"])).unwrap(),
        Ask {
            arches: vec![Arch::X86_64],
            record: true
        }
    );
    assert!(parse_args(&s(&["--arch", "mips"])).is_err());
    assert!(parse_args(&s(&["--watch"])).is_err());
}
