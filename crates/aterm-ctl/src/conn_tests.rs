// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|t| t.to_string()).collect()
}

#[test]
fn direction_mapping_produces_the_right_dst_src_pairs() {
    assert_eq!(
        direction_pair("@tbxxxxxxxxx", false, None),
        ("@tbxxxxxxxxx".to_string(), "@self".to_string()),
        "default: @self -> <sel>"
    );
    assert_eq!(
        direction_pair("@tbxxxxxxxxx", true, None),
        ("@self".to_string(), "@tbxxxxxxxxx".to_string()),
        "--to-me: <sel> -> @self"
    );
    assert_eq!(
        direction_pair("@tbxxxxxxxxx", false, Some("@tcxxxxxxxxx")),
        ("@tbxxxxxxxxx".to_string(), "@tcxxxxxxxxx".to_string()),
        "--from: <from> -> <sel>"
    );
}

#[test]
fn pair_args_parse_flags_and_refuse_conflicts() {
    let spec = parse_pair_args("add", &s(&["@tbxxxxxxxxx", "--kind", "pull", "--to-me"])).unwrap();
    assert_eq!(
        spec,
        PairSpec {
            sel: "@tbxxxxxxxxx".to_string(),
            kind: Some("pull".to_string()),
            to_me: true,
            from: None,
        }
    );
    let spec = parse_pair_args("rm", &s(&["--kind=push", "@7", "--from=@tcxxxxxxxxx"])).unwrap();
    assert_eq!(spec.kind.as_deref(), Some("push"));
    assert_eq!(spec.from.as_deref(), Some("@tcxxxxxxxxx"));
    assert_eq!(spec.sel, "@7");
    assert!(
        parse_pair_args(
            "add",
            &s(&["@tbxxxxxxxxx", "--to-me", "--from", "@tcxxxxxxxxx"])
        )
        .is_err()
    );
    assert!(parse_pair_args("add", &s(&["@tbxxxxxxxxx", "--kind", "sideways"])).is_err());
    assert!(parse_pair_args("add", &s(&[])).is_err());
    assert!(parse_pair_args("add", &s(&["@tbxxxxxxxxx", "@tcxxxxxxxxx"])).is_err());
    assert!(parse_pair_args("add", &s(&["@tbxxxxxxxxx", "--force"])).is_err());
}

#[test]
fn selectors_refuse_titles_and_the_active_tab() {
    for bad in ["build", "worker (2)", "tabcxxxxxxx", "@."] {
        let e = classify_selector(bad).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad}");
    }
    assert_eq!(classify_selector("@self").unwrap(), SelTok::SelfSid);
    assert_eq!(classify_selector("@env").unwrap(), SelTok::SelfSid);
    assert_eq!(
        classify_selector("@tab12xxxxxx").unwrap(),
        SelTok::Sid("tab12xxxxxx")
    );
    assert_eq!(classify_selector("@7").unwrap(), SelTok::Local(7));
    assert!(classify_selector("@+7").is_err());
    assert!(classify_selector("@sideways").is_err());
    assert!(classify_selector("@tabxxxxxxxx 12").is_err());
    assert!(classify_selector("@tabxxxxxxxx\n12").is_err());
}

/// A tab opened before the tab-ID update keeps its `s-` ID until it closes, so a selector names
/// a tab by its ID in either form. NEGATIVE CONTROLS: a malformed ID of either form is no
/// selector, and a bare ID asks for its `@`.
#[test]
fn a_selector_names_a_tab_by_its_id_in_either_form() {
    for sid in ["tqn5r6e0148", "s-0123456789abcdef0123"] {
        assert_eq!(
            classify_selector(&format!("@{sid}")).unwrap(),
            SelTok::Sid(sid)
        );
        let bare = classify_selector(sid).unwrap_err().to_string();
        assert!(bare.contains(&format!("needs an @: @{sid}")), "{bare}");
    }
    for bad in [
        "@tqn5r6e014",
        "@tqn5r6e014i",
        "@Tqn5r6e0148",
        "@S-0123456789abcdef0123",
        "@s-",
        "@t0123456789 abcdef0123",
    ] {
        let e = classify_selector(bad).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad}");
    }
}

#[test]
fn self_selector_outside_aterm_refuses_with_the_env_var_named() {
    let mut no_lookup = |_n: u64| -> io::Result<Option<String>> {
        panic!("@self resolution must not hit the wire")
    };
    let e = resolve_selector_with("@self", None, &mut no_lookup).unwrap_err();
    assert!(
        e.to_string().contains("ATERM_PARENT_SESSION_ID"),
        "must name the env var: {e}"
    );
    for bad_env in ["", " ", "tmexxxxxxxx extra", "tmexxxxxxxx\n"] {
        assert!(
            resolve_selector_with("@self", Some(bad_env), &mut no_lookup).is_err(),
            "{bad_env:?} must be refused"
        );
    }
    let ok = resolve_selector_with("@self", Some("tmexxxxxxxx"), &mut no_lookup).unwrap();
    assert_eq!(ok, "tmexxxxxxxx");
    let ok = resolve_selector_with("@tpeerxxxxxx", None, &mut no_lookup).unwrap();
    assert_eq!(ok, "tpeerxxxxxx");
    let mut lookup =
        |n: u64| -> io::Result<Option<String>> { Ok((n == 3).then(|| "s-three".to_string())) };
    assert_eq!(
        resolve_selector_with("@3", None, &mut lookup).unwrap(),
        "s-three"
    );
    assert!(resolve_selector_with("@9", None, &mut lookup).is_err());
}

#[test]
fn spawn_args_parse_role_place_and_of() {
    assert_eq!(
        parse_spawn_args(&s(&["controlled"])).unwrap(),
        SpawnSpec {
            role: "controlled".to_string(),
            place: "window",
            of: None,
        },
        "--window is the default place"
    );
    let spec = parse_spawn_args(&s(&["controller", "--tab", "--of", "@s-w"])).unwrap();
    assert_eq!(spec.role, "controller");
    assert_eq!(spec.place, "tab");
    assert_eq!(spec.of.as_deref(), Some("@s-w"));
    let spec = parse_spawn_args(&s(&["controller", "--of=@4"])).unwrap();
    assert_eq!(spec.of.as_deref(), Some("@4"));
    assert!(parse_spawn_args(&s(&[])).is_err());
    assert!(parse_spawn_args(&s(&["supervisor"])).is_err());
    assert!(parse_spawn_args(&s(&["controlled", "extra"])).is_err());
}

#[test]
fn flows_and_edges_json_parse_the_wire_shapes() {
    let flows = "{\"flows\":[\
        {\"src\":\"taxxxxxxxxx\",\"dst\":\"tbxxxxxxxxx\",\"ops\":[\"read-screen\",\"write-input\",\"signal\"]},\
        {\"src\":\"tbxxxxxxxxx\",\"dst\":\"taxxxxxxxxx\",\"ops\":[\"read-screen\"]}]}";
    assert_eq!(
        parse_flows_json(flows),
        vec![
            FlowPair {
                src: "taxxxxxxxxx".to_string(),
                dst: "tbxxxxxxxxx".to_string(),
                ops: s(&["read-screen", "write-input", "signal"]),
            },
            FlowPair {
                src: "tbxxxxxxxxx".to_string(),
                dst: "taxxxxxxxxx".to_string(),
                ops: s(&["read-screen"]),
            },
        ]
    );
    assert!(parse_flows_json("{\"flows\":[]}").is_empty());

    let edges = "{\"edges\":[\
        {\"src\":\"s-op\",\"dst\":\"s-me\",\"op\":\"write-input\"},\
        {\"src\":\"s-op\",\"dst\":\"s-me\",\"op\":\"signal\"}],\"dst\":\"s-me\"}";
    assert_eq!(
        parse_edges_json(edges),
        vec![
            (
                "s-op".to_string(),
                "s-me".to_string(),
                "write-input".to_string()
            ),
            ("s-op".to_string(), "s-me".to_string(), "signal".to_string()),
        ]
    );
    assert!(parse_edges_json("{\"edges\":[],\"dst\":\"s-me\"}").is_empty());
}

#[test]
fn sessions_lines_parse_and_percent_decode_titles() {
    let rows = parse_sessions_lines(&s(&[
        "0 taa11xxxxxx - running cargo%20build meta=0",
        "1 tbb22xxxxxx taa11xxxxxx exited operator meta=1",
        "2 tcc33xxxxxx - running meta=0",
        "+3 tdd44xxxxxx - running plus meta=0",
        "garbage line",
    ]));
    assert_eq!(
        rows,
        vec![
            SessionRow {
                local: 0,
                sid: "taa11xxxxxx".to_string(),
                title: "cargo build".to_string(),
            },
            SessionRow {
                local: 1,
                sid: "tbb22xxxxxx".to_string(),
                title: "operator".to_string(),
            },
            SessionRow {
                local: 2,
                sid: "tcc33xxxxxx".to_string(),
                title: String::new(),
            },
        ]
    );
}

#[test]
fn kinds_classify_from_the_op_set() {
    assert_eq!(kind_of_ops(&s(&["read-screen"])), "pull");
    assert_eq!(kind_of_ops(&s(&["write-input", "signal"])), "push");
    assert_eq!(kind_of_ops(&s(&["signal"])), "push");
    assert_eq!(
        kind_of_ops(&s(&["read-screen", "write-input", "signal"])),
        "both"
    );
    assert_eq!(kind_of_ops(&s(&["derive-loop"])), "other");
}

#[test]
fn status_rows_render_glyphs_kinds_and_titles() {
    let index = parse_sessions_lines(&s(&[
        "0 s-me - running me meta=0",
        "1 s-worker - running cargo%20build meta=0",
        "2 s-peer - running peer meta=0",
    ]));
    let outbound = vec![
        (
            "s-worker".to_string(),
            s(&["read-screen", "write-input", "signal"]),
        ),
        ("s-peer".to_string(), s(&["read-screen"])),
    ];
    let inbound = vec![
        ("s-peer".to_string(), s(&["read-screen"])),
        ("s-ghost".to_string(), s(&["write-input", "signal"])),
    ];
    let rows = render_self_rows(&inbound, &outbound, &index);
    assert_eq!(
        rows,
        vec![
            "\u{21e4} push  @s-ghost -".to_string(),
            "\u{21c6} pull  @s-peer \"peer\"".to_string(),
            "\u{21e5} both  @s-worker \"cargo build\"".to_string(),
        ]
    );
    let rows = render_self_rows(
        &[("s-x".to_string(), s(&["read-screen"]))],
        &[(
            "s-x".to_string(),
            s(&["read-screen", "write-input", "signal"]),
        )],
        &[],
    );
    assert_eq!(
        rows,
        vec![
            "\u{21e5} both  @s-x -".to_string(),
            "\u{21e4} pull  @s-x -".to_string(),
        ]
    );
}

#[test]
fn refusals_name_the_resolved_sessions() {
    let r = |st: &str, src: Option<&str>| refusal(st, src, "tbxxxxxxxxx");
    for status in [
        "ERR no such session",
        "ERR exited",
        "ERR self-loop",
        "ERR no such connection",
    ] {
        let said = r(status, Some("taxxxxxxxxx")).unwrap_or_else(|| panic!("{status}"));
        assert!(said.contains("@tbxxxxxxxxx"), "{status}: {said}");
    }
    assert!(r("ERR denied", None).is_some());
    assert_eq!(
        r("ERR headless", None),
        None,
        "unknown refusals pass through"
    );
}

#[test]
fn fish_completion_offers_the_conn_subverbs() {
    let script =
        crate::front_door_completion_script("fish", &["ctl", "conn"], &[]).expect("fish script");
    assert!(
        script.contains("-a 'ctl conn'"),
        "the `conn` verb completes on `aterm`: {script}"
    );
    assert!(
        script.contains("__fish_seen_subcommand_from conn"),
        "the subverbs are gated behind a seen `conn`: {script}"
    );
    assert!(
        script.contains(CONN_SUBVERBS),
        "every conn subverb completes"
    );
    assert!(
        !crate::completion_script("fish")
            .expect("fish script")
            .contains(CONN_SUBVERBS),
        "the aterm-ctl script does not advertise a verb aterm-ctl lacks"
    );
}
