// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SOURCE: &str = r#"
schema = 2
[quit]
run = "owned-fixture-run"
[[windows]]
rows = 48
cols = 110
active_tab = 0
active_item = 0
[[windows.tabs]]
kind = "leaf"
title = "legacy mirror"
[[windows.restored_tabs]]
zoomed = false
[windows.restored_tabs.root]
node = "leaf"
[windows.restored_tabs.root.view]
kind = "terminal"
sid = "bootstrap"
local_id = 17
log_ep = 23
title = "bootstrap title"
[[windows.restored_tabs]]
zoomed = false
[windows.restored_tabs.root]
node = "leaf"
[windows.restored_tabs.root.view]
kind = "native"
restore_tag = "editor"
uri = "file:///private/fixture.txt"
durable_seq = 19
source_anchor = 7
viewport_anchor = 3
unknown_document_metadata = ["retained", "verbatim"]
[[windows.restored_tabs]]
zoomed = false
focused_path = ["second"]
[windows.restored_tabs.root]
node = "split"
axis = "vertical"
ratio = 0.375
[windows.restored_tabs.root.first]
node = "leaf"
[windows.restored_tabs.root.first.view]
kind = "terminal"
sid = "left"
local_id = 18
log_ep = 24
cwd = "/private/left"
[windows.restored_tabs.root.second]
node = "leaf"
[windows.restored_tabs.root.second.view]
kind = "terminal"
sid = "right"
local_id = 19
log_ep = 25
unknown_terminal_metadata = "retained"
"#;

fn compose_split(text: &str) -> Result<Composition, String> {
    compose(
        text,
        Case::Split,
        &["file:///private/fixture.txt".into()],
        3,
    )
}

#[test]
fn composed_split_preserves_opaque_payloads_and_exact_focus_route() {
    let composed = compose_split(SOURCE).unwrap();
    let source: Value = aterm_toml::from_str(SOURCE).unwrap();
    let result: Value = aterm_toml::from_str(&composed.text).unwrap();
    let before = source.get("windows").unwrap().get(0).unwrap();
    let after = result.get("windows").unwrap().get(0).unwrap();
    assert_eq!(result.get("quit"), source.get("quit"));
    assert_eq!(after.get("rows"), before.get("rows"));
    assert!(after.get("tabs").is_none());
    let tabs = after.get("restored_tabs").unwrap().as_array().unwrap();
    let originals = before.get("restored_tabs").unwrap().as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0], originals[0]);
    let combined = tabs[1].get("root").unwrap();
    assert_eq!(combined.get("first"), originals[1].get("root"));
    assert_eq!(combined.get("second"), originals[2].get("root"));
    assert_eq!(
        combined.get("axis").and_then(Value::as_str),
        Some("horizontal")
    );
    assert_eq!(combined.get("ratio").and_then(Value::as_float), Some(0.5));
    assert_eq!(
        tabs[1].get("focused_path"),
        Some(&Value::Array(vec![
            Value::String("second".into()),
            Value::String("second".into())
        ])),
    );
}

#[test]
fn malformed_sources_refuse_before_composition() {
    for invalid in [
        SOURCE.replace("schema = 2", "schema = 1"),
        SOURCE.replace("active_item = 0", "active_item = 1"),
        SOURCE.replace("active_item = 0", ""),
        SOURCE.replace("zoomed = false", ""),
        SOURCE.replace("restore_tag = \"editor\"", "restore_tag = \"allocation\""),
        SOURCE.replace("fixture.txt", "other.txt"),
        SOURCE.replace("focused_path = [\"second\"]", "focused_path = [\"third\"]"),
        SOURCE.replace("focused_path = [\"second\"]", "focused_path = []"),
        SOURCE.replace("sid = \"right\"", "sid = \"left\""),
        SOURCE.replace("sid = \"right\"", "sid = \"\""),
        SOURCE.replace("ratio = 0.375", "ratio = 0.0"),
        SOURCE.replace("ratio = 0.375", "ratio = 1.0"),
        SOURCE.replace("zoomed = false", "zoomed = true"),
        SOURCE.replace(
            "focused_path = [\"second\"]",
            "focused_path = [\"second\"]\nfuture_owner = \"cannot discard\"",
        ),
    ] {
        assert_ne!(invalid, SOURCE);
        assert!(
            compose_split(&invalid).is_err(),
            "accepted malformed source: {invalid}"
        );
    }
    assert!(
        compose(
            SOURCE,
            Case::Mixed,
            &["file:///private/fixture.txt".into()],
            3
        )
        .is_err()
    );
    assert!(compose(SOURCE, Case::Split, &[], 3).is_err());
}

#[test]
fn round_trip_allows_runtime_reminting_but_rejects_lost_document_identity() {
    let composed = compose_split(SOURCE).unwrap();
    let reminted = composed
        .text
        .replace("local_id = 17", "local_id = 70")
        .replace("log_ep = 23", "log_ep = 99")
        .replace("sid = \"bootstrap\"", "sid = \"new-runtime\"");
    composed.check_round_trip(&reminted).unwrap();
    for damaged in [
        reminted.replace("durable_seq = 19", "durable_seq = 0"),
        reminted.replace("fixture.txt", "other.txt"),
        reminted.replace("source_anchor = 7", "source_anchor = 0"),
        reminted.replace("axis = \"horizontal\"", "axis = \"vertical\""),
        reminted.replace("[\"second\", \"second\"]", "[\"second\", \"first\"]"),
        reminted.replace("sid = \"right\"", "sid = \"left\""),
    ] {
        assert_ne!(damaged, reminted);
        assert!(composed.check_round_trip(&damaged).is_err());
    }
}

fn editor_first_source(extra: bool) -> String {
    let mut manifest = parse(SOURCE).unwrap();
    let main = &mut manifest.windows[0];
    main.restored_tabs.truncate(2);
    main.restored_tabs.swap(0, 1);
    main.active_item = 1;
    if extra {
        let mut second = parse(&SOURCE.replace("fixture.txt", "extra.txt"))
            .unwrap()
            .windows
            .remove(0);
        let editor = second.restored_tabs.remove(1);
        second.restored_tabs = vec![editor];
        second.active_item = 0;
        manifest.windows.push(second);
    }
    aterm_toml::to_string(&manifest).unwrap()
}

#[test]
fn editor_first_capture_is_preserved_and_bootstrap_is_discovered_after_the_editor() {
    for extra in [false, true] {
        let text = editor_first_source(extra);
        let case = if extra {
            Case::EditorFirstNativeWindow
        } else {
            Case::EditorFirst
        };
        let mut uris = vec!["file:///private/fixture.txt".into()];
        if extra {
            uris.push("file:///private/extra.txt".into());
        }
        let composed = compose(&text, case, &uris, 2).unwrap();
        assert_eq!(
            composed.text, text,
            "UI-captured order is never repaired by the fixture"
        );
        assert_eq!(composed.bootstrap_key.as_deref(), Some("1:2:/"));
        let mut positions = BTreeMap::from([("1:1:/".into(), uris[0].clone())]);
        if extra {
            positions.insert("2:1:/".into(), uris[1].clone());
        }
        assert_eq!(composed.native_uris().unwrap(), positions);
        composed.check_round_trip(&text).unwrap();
        let mut wrong = parse(&text).unwrap();
        wrong.windows[0].restored_tabs.swap(0, 1);
        assert!(
            composed
                .check_round_trip(&aterm_toml::to_string(&wrong).unwrap())
                .is_err()
        );
        assert!(compose(&aterm_toml::to_string(&wrong).unwrap(), case, &uris, 2).is_err());
    }
}

#[test]
fn native_window_round_trip_rejects_folded_windows_swapped_uris_and_fake_bootstraps() {
    let text = editor_first_source(true);
    let uris = [
        "file:///private/fixture.txt".into(),
        "file:///private/extra.txt".into(),
    ];
    let composed = compose(&text, Case::EditorFirstNativeWindow, &uris, 2).unwrap();
    let mut folded = parse(&text).unwrap();
    let second = folded.windows.remove(1);
    folded.windows[0].restored_tabs.extend(second.restored_tabs);
    assert!(
        composed
            .check_round_trip(&aterm_toml::to_string(&folded).unwrap())
            .is_err()
    );

    let mut swapped = parse(&text).unwrap();
    let second = swapped.windows[1].restored_tabs.remove(0);
    let first = std::mem::replace(&mut swapped.windows[0].restored_tabs[0], second);
    swapped.windows[1].restored_tabs.push(first);
    assert!(
        composed
            .check_round_trip(&aterm_toml::to_string(&swapped).unwrap())
            .is_err()
    );

    let mut fake = parse(&text).unwrap();
    let terminal = parse(SOURCE)
        .unwrap()
        .windows
        .remove(0)
        .restored_tabs
        .remove(0);
    fake.windows[1].restored_tabs.push(terminal);
    let fake = aterm_toml::to_string(&fake).unwrap();
    assert!(compose(&fake, Case::EditorFirstNativeWindow, &uris, 2).is_err());
    assert!(composed.check_round_trip(&fake).is_err());
    assert!(compose(&text, Case::EditorFirst, &uris[..1], 2).is_err());
}

#[test]
fn native_only_proof_has_no_bootstrap_and_checks_the_one_preserved_identity() {
    let mut native = parse(&editor_first_source(false)).unwrap();
    let terminal = native.windows[0].restored_tabs.remove(1);
    native.windows[0].active_item = 0;
    let text = aterm_toml::to_string(&native).unwrap();
    let proof = capture_native_only(&text, &["file:///private/fixture.txt".into()]).unwrap();
    assert!(proof.bootstrap_key.is_none());
    proof.check_round_trip(&text).unwrap();
    let mut used = parse(&text).unwrap();
    used.windows[0].restored_tabs.insert(0, terminal);
    let used = aterm_toml::to_string(&used).unwrap();
    proof
        .check_with_preserved_bootstrap(&used, "bootstrap")
        .unwrap();
    assert!(proof.check_round_trip(&used).is_err());
    assert!(
        proof
            .check_with_preserved_bootstrap(&used, "different")
            .is_err()
    );
    assert!(
        proof
            .check_with_preserved_bootstrap(
                &used.replace("durable_seq = 19", "durable_seq = 0"),
                "bootstrap"
            )
            .is_err()
    );
    assert!(
        proof
            .check_with_preserved_bootstrap(
                &used.replace("active_item = 0", "active_item = 1"),
                "bootstrap"
            )
            .is_err()
    );
    assert!(capture_native_only(&used, &["file:///private/fixture.txt".into()]).is_err());
}
