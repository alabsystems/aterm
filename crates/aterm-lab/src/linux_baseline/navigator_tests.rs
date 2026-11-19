// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn two_samples_agree_on_the_semantics_but_a_missing_channel_or_action_does_not() {
    let sample = |age: &str, preview: &str, latest: &str, count: usize, tail: &str| {
        format!(
            "depth=5 role=push_button name=\"~, idle, bash, {age}, {preview}, Control Shift 1\" description=\"Last minute. Twelve closed five-second buckets, oldest first. CPU {latest}, scale 0 to 2 reported cores, {count} of 12 observed. {tail} Terminal input unavailable, scale 0 to 1 bytes per second, 0 of 12 observed. All gaps.\" actions=click"
        )
    };
    let first = sample("now", "lane$", "unavailable", 0, "All gaps.");
    let second = sample(
        "2m",
        "lane$ echo LANE-$((6*7))",
        "0.5 reported cores",
        1,
        "gap, 0.5.",
    );
    let stable = node(&first, "guest");
    assert_eq!(stable, node(&second, "guest"));
    for wanted in [
        "~, idle, bash",
        "Control Shift 1",
        "CPU (reported cores, 12 buckets)",
        "Terminal input (bytes per second, 12 buckets)",
        "actions=click",
    ] {
        assert!(stable.contains(wanted), "{stable}");
    }
    for (from, to) in [
        ("Terminal input unavailable", "Memory unavailable"),
        ("actions=click", "actions="),
        ("name=\"~,", "name=\"wrong,"),
        ("lane$", "another shell"),
        ("12 observed", "0 observed"),
        ("0.5 reported cores", "NaN reported cores"),
    ] {
        assert_ne!(stable, node(&second.replace(from, to), "guest"), "{from}");
    }
    let title = "depth=2 role=embedded name=\"guest\"";
    assert_eq!(
        node(title, "guest"),
        "depth=2 role=embedded name=\"<guest hostname>\""
    );
    assert_eq!(node(title, "different"), title);
}

#[test]
fn only_the_observed_sessions_heading_can_publish_its_numbered_header() {
    let ancestry = [("/frame", false), ("/outer", true), ("/inner", true)];
    let title = "depth=5 role=heading name=\"Sessions\"";
    let header = Header::from_node("/title", title, &ancestry).unwrap();
    assert!(
        Header::from_node(
            "/other",
            &title.replace("Sessions", "Other sessions"),
            &ancestry
        )
        .is_none()
    );
    let roles = [
        ("/outer", "panel"),
        ("/inner", "panel"),
        ("/title", "heading"),
        ("/other", "heading"),
    ]
    .map(|(p, r)| (p.to_string(), r.to_string()));
    let dock = Dock {
        headers: vec![header],
        ..Dock::default()
    };
    let events = [
        "0 1 Object.ChildrenChanged /outer detail=\"add\" any=ref(/inner)",
        "0 1 Object.ChildrenChanged /outer detail=\"remove\" any=ref(/title)",
        "0 1 Object.PropertyChange /title detail=\"accessible-parent\" any=ref(/inner)",
        "0 1 Object.ChildrenChanged /outer detail=\"remove\" any=ref(/other)",
    ];
    assert!(event_kinds(&events[..3], &roles, &dock).is_empty());
    assert_eq!(
        event_kinds(&events, &roles, &dock),
        ["Object.ChildrenChanged:remove@panel".to_owned()].into()
    );
}

#[test]
fn only_publication_of_the_observed_roster_rows_headings_and_resource_schema_is_dynamic() {
    let mut dock = Dock::default();
    let root = [("/frame", false)];
    let list = [("/frame", false), ("/dock", false)];
    let row = [
        ("/frame", false),
        ("/dock", false),
        ("/list", false),
        ("/panel", true),
    ];
    let resource = "Last minute. Twelve buckets. CPU unavailable, scale 0 to 1 cores, 0 of 12 observed. All gaps.";
    dock.observe("/dock", "node role=embedded", &root);
    dock.observe("/list", "node role=list", &list);
    dock.observe(
        "/heading",
        "node role=heading name=\"Sessions\" extents_window=1,2,3,4",
        &list,
    );
    dock.observe("/button", &format!("node role=push_button name=\"~, idle, bash, now, lane$, Control Shift 1\" description={resource:?}"), &row);
    let roles = [
        ("/frame", "frame"),
        ("/dock", "embedded"),
        ("/list", "list"),
        ("/panel", "panel"),
        ("/heading", "heading"),
        ("/button", "push_button"),
        ("/details", "push_button"),
    ]
    .map(|(p, r)| (p.to_string(), r.to_string()));
    let update = format!(
        "0 1 Object.PropertyChange /button detail=\"accessible-description\" any={resource:?}"
    );
    let expected = [
        "0 1 Object.ChildrenChanged /frame detail=\"add\" any=ref(/dock)",
        "0 1 Object.ChildrenChanged /list detail=\"add\" any=ref(/panel)",
        "0 1 Object.ChildrenChanged /list detail=\"remove\" any=ref(/placeholder)",
        "0 1 Object.StateChanged /placeholder detail=\"defunct\" d1=1",
        "0 1 Object.BoundsChanged /heading detail=\"\" any=(1,2,3,4)",
        &update,
    ];
    assert!(event_kinds(&expected, &roles, &dock).is_empty());

    let static_update = update.replace("/button", "/details");
    let lost_metric = update.replace("CPU unavailable", "Other unavailable");
    let unexpected = [
        "0 1 Object.ChildrenChanged /frame detail=\"add\" any=ref(/button)",
        "0 1 Object.ChildrenChanged /list detail=\"add\" any=ref(/heading)",
        "0 1 Object.ChildrenChanged /list detail=\"remove\" any=ref(/placeholder)",
        "0 1 Object.StateChanged /another detail=\"defunct\" d1=1",
        "0 1 Object.StateChanged /details detail=\"defunct\" d1=1",
        "0 1 Object.BoundsChanged /heading detail=\"\" any=(1,2,3,9)",
        "0 1 Object.BoundsChanged /panel detail=\"\" any=(1,2,3,4)",
        "0 1 Object.BoundsChanged /button detail=\"\" any=(1,2,3,4)",
        &static_update,
        &lost_metric,
    ];
    for line in unexpected {
        assert_eq!(
            event_kinds(&[line], &roles, &dock),
            [event_kind(line, &roles)].into(),
            "{line}"
        );
    }
    assert_eq!(event_kinds(&unexpected[2..4], &roles, &dock).len(), 2);
}
