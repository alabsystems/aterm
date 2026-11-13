// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|t| t.to_string()).collect()
}

#[test]
fn direction_mapping_produces_the_right_dst_src_pairs() {
    assert_eq!(
        direction_pair("@s-b", false, None),
        ("@s-b".to_string(), "@self".to_string()),
        "default: @self -> <sel>"
    );
    assert_eq!(
        direction_pair("@s-b", true, None),
        ("@self".to_string(), "@s-b".to_string()),
        "--to-me: <sel> -> @self"
    );
    assert_eq!(
        direction_pair("@s-b", false, Some("@s-c")),
        ("@s-b".to_string(), "@s-c".to_string()),
        "--from: <from> -> <sel>"
    );
}

#[test]
fn pair_args_parse_flags_and_refuse_conflicts() {
    let spec = parse_pair_args("add", &s(&["@s-b", "--kind", "pull", "--to-me"])).unwrap();
    assert_eq!(
        spec,
        PairSpec {
            sel: "@s-b".to_string(),
            kind: Some("pull".to_string()),
            to_me: true,
            from: None,
        }
    );
    let spec = parse_pair_args("rm", &s(&["--kind=push", "@7", "--from=@s-c"])).unwrap();
    assert_eq!(spec.kind.as_deref(), Some("push"));
    assert_eq!(spec.from.as_deref(), Some("@s-c"));
    assert_eq!(spec.sel, "@7");
    assert!(parse_pair_args("add", &s(&["@s-b", "--to-me", "--from", "@s-c"])).is_err());
    assert!(parse_pair_args("add", &s(&["@s-b", "--kind", "sideways"])).is_err());
    assert!(parse_pair_args("add", &s(&[])).is_err());
    assert!(parse_pair_args("add", &s(&["@s-b", "@s-c"])).is_err());
    assert!(parse_pair_args("add", &s(&["@s-b", "--force"])).is_err());
}

#[test]
fn selectors_refuse_titles_and_the_active_tab() {
    for bad in ["build", "worker (2)", "s-abc", "@."] {
        let e = classify_selector(bad).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad}");
    }
    assert_eq!(classify_selector("@self").unwrap(), SelTok::SelfSid);
    assert_eq!(classify_selector("@env").unwrap(), SelTok::SelfSid);
    assert_eq!(classify_selector("@s-ab12").unwrap(), SelTok::Sid("s-ab12"));
    assert_eq!(classify_selector("@7").unwrap(), SelTok::Local(7));
    assert!(classify_selector("@+7").is_err());
    assert!(classify_selector("@sideways").is_err());
    assert!(classify_selector("@s-ab 12").is_err());
    assert!(classify_selector("@s-ab\n12").is_err());
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
    for bad_env in ["", " ", "s-me extra", "s-me\n"] {
        assert!(
            resolve_selector_with("@self", Some(bad_env), &mut no_lookup).is_err(),
            "{bad_env:?} must be refused"
        );
    }
    let ok = resolve_selector_with("@self", Some("s-me"), &mut no_lookup).unwrap();
    assert_eq!(ok, "s-me");
    let ok = resolve_selector_with("@s-peer", None, &mut no_lookup).unwrap();
    assert_eq!(ok, "s-peer");
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
        {\"src\":\"s-a\",\"dst\":\"s-b\",\"ops\":[\"read-screen\",\"write-input\",\"signal\"]},\
        {\"src\":\"s-b\",\"dst\":\"s-a\",\"ops\":[\"read-screen\"]}]}";
    assert_eq!(
        parse_flows_json(flows),
        vec![
            FlowPair {
                src: "s-a".to_string(),
                dst: "s-b".to_string(),
                ops: s(&["read-screen", "write-input", "signal"]),
            },
            FlowPair {
                src: "s-b".to_string(),
                dst: "s-a".to_string(),
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
        "0 s-aa11 - running cargo%20build meta=0",
        "1 s-bb22 s-aa11 exited operator meta=1",
        "2 s-cc33 - running meta=0",
        "+3 s-dd44 - running plus meta=0",
        "garbage line",
    ]));
    assert_eq!(
        rows,
        vec![
            SessionRow {
                local: 0,
                sid: "s-aa11".to_string(),
                title: "cargo build".to_string(),
            },
            SessionRow {
                local: 1,
                sid: "s-bb22".to_string(),
                title: "operator".to_string(),
            },
            SessionRow {
                local: 2,
                sid: "s-cc33".to_string(),
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
    let r = |st: &str, src: Option<&str>| refusal(st, src, "s-b");
    for status in [
        "ERR no such session",
        "ERR exited",
        "ERR self-loop",
        "ERR no such connection",
    ] {
        let said = r(status, Some("s-a")).unwrap_or_else(|| panic!("{status}"));
        assert!(said.contains("@s-b"), "{status}: {said}");
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
