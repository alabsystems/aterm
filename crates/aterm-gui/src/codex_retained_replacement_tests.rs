// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn capability_data_requires_the_actual_spawned_child_and_closed_custody_schema() {
    let target = aterm_agent::harness::native_act::Image {
        path: b"/owned/image".to_vec(),
        device: 13,
        inode: 47,
    };
    let mut reply = aterm_json::json!({"protocol":3,"pid":417,"executable":"/owned/image",
        "device":13,"inode":47,"extensions":{"alab.retainedImageCustody":
            {"e1Version":2,"imageCustody":1,"clipboardCustody":1,"preThreadPin":1}}});
    let check = |reply: &aterm_json::Value, actual_pid, proof| {
        capability_reply(
            &aterm_json::to_vec(reply).unwrap(),
            actual_pid,
            &target,
            proof,
        )
    };
    assert_eq!(check(&reply, 417, false), Ok(()));
    // Same admitted path/device/inode and valid capability cannot substitute a
    // stale reporter from another invocation of that very same native image.
    assert_eq!(check(&reply, 418, false), Err(registry::Error::Unsupported));
    reply
        .as_object_mut()
        .unwrap()
        .insert("pid".into(), aterm_json::json!(418));
    assert_eq!(check(&reply, 417, false), Err(registry::Error::Unsupported));
    reply
        .as_object_mut()
        .unwrap()
        .insert("pid".into(), aterm_json::json!(417));
    extension(&mut reply, "alab.retainedImageCustody").insert("extra".into(), aterm_json::json!(1));
    assert_eq!(check(&reply, 417, false), Err(registry::Error::Unsupported));
    reply.as_object_mut().unwrap().insert(
        "extensions".into(),
        aterm_json::json!({"alab.nativeRetainedTransportProof":
        {"version":1,"e1Version":2,"imageCustody":1,"preThreadPin":1}}),
    );
    assert_eq!(check(&reply, 417, true), Ok(()));
    assert_eq!(check(&reply, 417, false), Err(registry::Error::Unsupported));
    extension(&mut reply, "alab.nativeRetainedTransportProof")
        .insert("clipboardCustody".into(), aterm_json::json!(1));
    assert_eq!(check(&reply, 417, true), Err(registry::Error::Unsupported));
}
fn extension<'a>(reply: &'a mut aterm_json::Value, key: &str) -> &'a mut aterm_json::Map {
    reply
        .as_object_mut()
        .unwrap()
        .get_mut("extensions")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut(key)
        .unwrap()
        .as_object_mut()
        .unwrap()
}
