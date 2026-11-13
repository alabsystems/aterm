// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::{export, user};
use crate::port::{Fake, Ran};

fn field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
    let PValue::Dict(entries) = value else {
        return None;
    };
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

#[test]
fn widget_clear_removes_instances_and_placement_but_keeps_unknown_widget_fields() {
    let sys = Fake::root();
    let who = user("alice", 501);
    sys.on(
        &placement_conversion(&who).display(),
        Ran::exit(1, "invalid plist"),
    );
    let read = Read::Value(PValue::Dict(vec![
        (
            "instances".into(),
            PValue::Array(vec![PValue::Data(vec![1, 2])]),
        ),
        (
            "DesktopWidgetPlacementStorage".into(),
            PValue::Data(vec![3]),
        ),
        ("vers".into(), PValue::Int(1)),
        ("future".into(), PValue::Bool(true)),
    ]));
    let want = cleared(&sys, &who, &read).unwrap().unwrap();
    assert_eq!(field(&want, "instances"), Some(&PValue::Array(vec![])));
    assert_eq!(field(&want, "future"), Some(&PValue::Bool(true)));
    assert_eq!(field(&want, "DesktopWidgetPlacementStorage"), None);
    assert_eq!(cleared(&sys, &who, &Read::Value(want)).unwrap(), None);
    assert!(cleared(&sys, &who, &Read::Absent).unwrap().is_some());
    let residual = Read::Value(PValue::Dict(vec![
        ("instances".into(), PValue::Array(Vec::new())),
        (
            "DesktopWidgetPlacementStorage".into(),
            PValue::Data(vec![3]),
        ),
        ("future".into(), PValue::Bool(true)),
    ]));
    let clear = cleared(&sys, &who, &residual).unwrap().unwrap();
    assert_eq!(field(&clear, "DesktopWidgetPlacementStorage"), None);
    assert_eq!(field(&clear, "future"), Some(&PValue::Bool(true)));
    assert_eq!(cleared(&sys, &who, &Read::Value(clear)).unwrap(), None);
}

#[test]
fn notificationcenters_regenerated_empty_binary_placement_is_already_clear() {
    let bytes = aterm_codec::base64::decode("YnBsaXN0MDDSAQIDBF8QFENvbXBhdGliaWxpdHlWZXJzaW9uXxAQTnVtYmVyZWREaXNwbGF5cxQAAAAAAAAAAAAAAAAAAAABoAgNJDdIAAAAAAAAAQEAAAAAAAAABQAAAAAAAAAAAAAAAAAAAEk=").unwrap();
    let sys = Fake::root();
    let who = user("alice", 501);
    let cmd = placement_conversion(&who);
    let empty = PValue::Dict(vec![
        ("CompatibilityVersion".into(), PValue::Int(1)),
        ("NumberedDisplays".into(), PValue::Array(Vec::new())),
    ]);
    sys.on(&cmd.display(), export(&crate::plist::fragment(&empty)));
    let read = Read::Value(PValue::Dict(vec![
        ("vers".into(), PValue::Int(1)),
        ("instances".into(), PValue::Array(Vec::new())),
        ("DesktopWidgetPlacementStorage".into(), PValue::Data(bytes)),
    ]));
    assert_eq!(cleared(&sys, &who, &read).unwrap(), None);
    assert_eq!(sys.calls(), [cmd.display()]);
    for shape in [
        PValue::Dict(vec![
            ("CompatibilityVersion".into(), PValue::Int(1)),
            (
                "NumberedDisplays".into(),
                PValue::Array(vec![PValue::Int(1)]),
            ),
        ]),
        PValue::Dict(vec![
            ("CompatibilityVersion".into(), PValue::Int(1)),
            ("NumberedDisplays".into(), PValue::Array(Vec::new())),
            ("future".into(), PValue::Bool(true)),
        ]),
    ] {
        let next = Fake::root();
        next.on(&cmd.display(), export(&crate::plist::fragment(&shape)));
        assert!(cleared(&next, &who, &read).unwrap().is_some());
    }
    sys.refuse_complete_stdout(&cmd.display(), "incomplete XML capture");
    assert!(cleared(&sys, &who, &read).is_err());
}
