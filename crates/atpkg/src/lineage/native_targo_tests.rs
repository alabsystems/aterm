// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn closed_retirement_rejects_bool_incomplete_and_normalized_signal() {
    retirement(&aterm_json::json!({"completed":true,"actual_exit":0,"owned_command_drained":true}))
        .unwrap();
    for exit in [
        V::from(false),
        V::from(true),
        V::from(130),
        V::from(-15),
        V::Null,
    ] {
        assert!(retirement(&aterm_json::json!({"completed":true,"actual_exit":exit,"owned_command_drained":true})).is_err());
    }
    assert!(
        retirement(
            &aterm_json::json!({"completed":true,"actual_exit":0,"owned_command_drained":false})
        )
        .is_err()
    );
    assert!(retirement(&aterm_json::json!({"completed":true,"actual_exit":0,"owned_command_drained":true,"unknown":0})).is_err());
}

#[test]
fn exact_refs_and_versions_refuse_foreign_or_partial_authority() {
    let reference = aterm_json::json!({"path":"transport.stdout","sha256":"a".repeat(64)});
    fixed(&reference, "transport.stdout").unwrap();
    for name in [
        "../transport.stdout",
        "/transport.stdout",
        "transport.stderr",
    ] {
        assert!(
            fixed(
                &aterm_json::json!({"path":name,"sha256":"a".repeat(64)}),
                "transport.stdout"
            )
            .is_err()
        );
    }
    assert!(parse(br#"{"exit":0,"exit":0}"#).is_err());
    let source = "123456789aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let value = aterm_json::json!({"CFG_VER_HASH":source,"CFG_COMMIT_HASH":source,"CFG_SHORT_COMMIT_HASH":"123456789",
        "CFG_COMMIT_DATE":"2026-11-08","CFG_RELEASE":"1.101.0-dev","CFG_TRUST_VERSION":"0.13.0"});
    version_environment(&value, source).unwrap();
    let mut wrong = value.clone();
    *member_mut(&mut wrong, "CFG_SHORT_COMMIT_HASH").unwrap() = V::from("1234567");
    assert!(version_environment(&wrong, source).is_err());
    let mut wrong = value;
    *member_mut(&mut wrong, "CFG_VER_HASH").unwrap() = V::from(COMPILER);
    assert!(version_environment(&wrong, source).is_err());
    assert!(
        version(
            b"targo 1.101.0-dev\n",
            source,
            "targo",
            "1.101.0-dev",
            "0.13.0"
        )
        .is_err()
    );
}

#[test]
fn whole_parent_pair_preserves_private_abi_and_rejects_partial_install() {
    let before = aterm_json::json!({"schema":"trust.sdk-component-inventory.v1","entries":[
        {"path":"rustc-private-sysroots","kind":"directory","mode":448},
        {"path":"rustc-private-sysroots/abi","kind":"file","mode":384,"bytes":1,"sha256":"a".repeat(64)},
        {"path":"sdk","kind":"directory","mode":493},
        {"path":"sdk/bin/cargo","kind":"file","mode":493,"bytes":1,"sha256":"b".repeat(64)},
        {"path":"sdk/bin/targo","kind":"file","mode":493,"bytes":1,"sha256":"b".repeat(64)}]});
    let artifact = aterm_json::json!({"mode":493,"bytes":2,"sha256":"c".repeat(64)});
    let mut after = before.clone();
    for row in member_mut(&mut after, "entries")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .skip(3)
    {
        *member_mut(row, "bytes").unwrap() = V::from(2);
        *member_mut(row, "sha256").unwrap() = artifact["sha256"].clone();
    }
    inventory_pair(&before, &after, &artifact).unwrap();
    let mut partial = after.clone();
    member_mut(&mut partial, "entries")
        .unwrap()
        .as_array_mut()
        .unwrap()[3] = before["entries"][3].clone();
    assert!(inventory_pair(&before, &partial, &artifact).is_err());
    *member_mut(
        &mut member_mut(&mut after, "entries")
            .unwrap()
            .as_array_mut()
            .unwrap()[1],
        "sha256",
    )
    .unwrap() = V::from("d".repeat(64));
    assert!(inventory_pair(&before, &after, &artifact).is_err());
    let mut sdk_only = before;
    drop(
        member_mut(&mut sdk_only, "entries")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .drain(..2),
    );
    assert!(inventory_shape(&sdk_only).is_err());
}

#[test]
fn portable_canonical_hash_uses_python_ascii_and_utf16_escape_contract() {
    let value = aterm_json::json!({"z":0,"a":"β🙂"});
    assert_eq!(
        canonical(&value).unwrap(),
        br#"{"a":"\u03b2\ud83d\ude42","z":0}"#
    );
}

#[test]
fn transport_frames_require_finite_matching_typed_status_and_canonical_bytes() {
    let commands = aterm_json::json!([{"name":"case","argv":[["selected","argument"]]}]);
    let row = aterm_json::json!({"name":"case","index":0,"argv":["selected","argument"],"exit":0,"raw_base64":"b2s="});
    let raw = aterm_json::to_vec(&row).unwrap();
    assert_eq!(frames(&raw, &commands).unwrap()[0].raw, b"ok");
    for value in [V::from(false), V::from(130), V::from(-15)] {
        let mut bad = row.clone();
        *member_mut(&mut bad, "exit").unwrap() = value;
        assert!(frames(&aterm_json::to_vec(&bad).unwrap(), &commands).is_err());
    }
    let mut bad = row;
    *member_mut(&mut bad, "raw_base64").unwrap() = V::from("b2t=");
    assert!(frames(&aterm_json::to_vec(&bad).unwrap(), &commands).is_err());
}

#[cfg(unix)]
#[test]
fn finite_transport_rejects_wrong_pipe_false_zero_and_false_refusal() {
    let subject = Path::new("/unqualified/sdk");
    let child = aterm_json::json!({"kind":"native-run","observed_pipe":[1,2,1,2],"available_before":0,"available_after":0,
        "compiler_exit":0,"compiler_stdout_hex":aterm_codec::hex::encode_lower(b"/unqualified/sdk\n"),
        "compiler_stderr_hex":"","sysroot":"/unqualified/sdk","marker":"native-targo-custody-positive-v1"});
    let raw = |child: &V| format!("NATIVE_TARGO_CUSTODY {}\n", child).into_bytes();
    let mut observed = child.as_object().unwrap().clone();
    observed.remove("compiler_stdout_hex");
    observed.remove("compiler_stderr_hex");
    observed.insert("child_exit".to_owned(), V::from(0));
    observed.insert("original_pipe".to_owned(), aterm_json::json!([1, 2, 1, 2]));
    observed.insert(
        "compiler_stdout_sha256".to_owned(),
        V::from(hash(b"/unqualified/sdk\n")),
    );
    observed.insert("compiler_stderr_sha256".to_owned(), V::from(hash(b"")));
    let observed = V::Object(observed);
    transport_observation(
        2,
        &observed,
        &[Frame {
            code: 0,
            raw: raw(&child),
        }],
        subject,
    )
    .unwrap();
    let mut wrong = child.clone();
    *member_mut(&mut wrong, "available_after").unwrap() = V::from(false);
    assert!(
        transport_observation(
            2,
            &observed,
            &[Frame {
                code: 0,
                raw: raw(&wrong)
            }],
            subject
        )
        .is_err()
    );
    let mut wrong = child;
    *member_mut(&mut wrong, "observed_pipe").unwrap() = aterm_json::json!([9, 9, 9, 9]);
    assert!(
        transport_observation(
            2,
            &observed,
            &[Frame {
                code: 0,
                raw: raw(&wrong)
            }],
            subject
        )
        .is_err()
    );
    assert!(transport_observation(3, &aterm_json::json!({}), &[], subject).is_err());
}
