// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn presses_resolve_the_live_buttons_action_and_refuse_missing_or_ambiguous_controls() {
    let controls = "ui key=\"a button\" role=Button label=\"Other action=wrong\" action=do/other state=enabled:true\nui key=\"text\" role=Text action=-\n";
    assert_eq!(button_action(controls, "a button").unwrap(), "do/other");
    assert!(button_action(controls, "text").is_err());
    assert!(button_action(controls, "absent").is_err());
    assert!(button_action(&format!("{controls}{controls}"), "a button").is_err());
}

#[test]
fn the_fixtures_are_the_seams_own_list() {
    let all = parse_fixtures(FIXTURE_SOURCE).unwrap();
    assert!(all.len() > 10, "{all:?}");
    for quick in QUICK_FIXTURES {
        assert!(all.contains(&quick), "--quick's {quick} is a fixture");
    }
    let mut unique = all.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), all.len(), "each fixture once");
    // Missing or unterminated slices are refused; appending names needs no second edit.
    assert!(parse_fixtures("const FIXTURES: &[&str] = &[\"a\",").is_err());
    assert!(parse_fixtures("fn main() {}").is_err());
    assert_eq!(
        parse_fixtures("pub(crate) const FIXTURES: &[&str] = &[\n    \"a\",\n    \"b-c\",\n];")
            .unwrap(),
        vec!["a", "b-c"]
    );
    assert_eq!(
        parse_fixtures(
            "pub(crate) const FIXTURES: &[&str] = &[\n    \"a\",\n    \"b-c\",\n    \"new\",\n];"
        )
        .unwrap(),
        vec!["a", "b-c", "new"]
    );
}

#[test]
fn the_command_line_names_the_matrix() {
    let a = parse_args(&args(&["/tmp/out"])).unwrap();
    assert_eq!(a.looks, Look::ALL.to_vec());
    assert_eq!(a.sizes, Size::ALL.to_vec());
    assert!(a.fixtures.is_none() && a.at.is_empty() && !a.quick);
    assert_eq!(a.scroll, None);
    let a = parse_args(&args(&[
        "--quick",
        "--fixtures",
        "running,failed",
        "--looks",
        "high-contrast",
        "--looks",
        "light",
        "--sizes",
        "compact",
        "--at",
        "0,150",
        "--scroll",
        "bottom",
        "--press",
        "system-state/repositories/disclose",
        "--press",
        "system-state/repositories/repo//home/person/project,one",
        "--press",
        "system-state/repositories/disclose",
        "/tmp/out",
    ]))
    .unwrap();
    assert_eq!(a.looks, vec![Look::HcDark, Look::HcLight, Look::Light]);
    assert_eq!(a.sizes, vec![Size::Compact]);
    assert_eq!(a.at, vec![0, 150]);
    assert_eq!(a.scroll, Some(ScrollPosition::Bottom));
    assert_eq!(
        a.press,
        vec![
            "system-state/repositories/disclose",
            "system-state/repositories/repo//home/person/project,one",
            "system-state/repositories/disclose",
        ]
    );
    assert_eq!(
        a.fixtures,
        Some(vec!["running".to_string(), "failed".to_string()])
    );
    let quick = parse_args(&args(&["--quick", "--scroll", "top", "/tmp/out"])).unwrap();
    assert_eq!(quick.scroll, Some(ScrollPosition::Top));
    assert_eq!(quick.looks, vec![Look::Dark, Look::Light]);
    assert_eq!(quick.sizes, vec![Size::Regular, Size::Compact]);
    for bad in [
        &["--looks", "sepia", "/tmp/out"][..],
        &["--sizes", "huge", "/tmp/out"],
        &["--at", "soon", "/tmp/out"],
        &["--scroll", "middle", "/tmp/out"],
        &["--frobnicate", "/tmp/out"],
        &[],
        &["/tmp/a", "/tmp/b"],
    ] {
        assert_eq!(
            parse_args(&args(bad)).map_err(|f| f.code),
            Err(2),
            "{bad:?}"
        );
    }
}

#[test]
fn fixtures_are_chosen_from_the_list_alone() {
    let all = ["checking", "running", "failed", "mixed"];
    let a = parse_args(&args(&["--quick", "/tmp/out"])).unwrap();
    assert_eq!(
        chosen_fixtures(&all, &a).unwrap(),
        vec!["checking", "running", "failed"],
        "--quick takes those of its fixtures the list holds"
    );
    let a = parse_args(&args(&["--fixtures", "mixed", "/tmp/out"])).unwrap();
    assert_eq!(chosen_fixtures(&all, &a).unwrap(), vec!["mixed"]);
    let a = parse_args(&args(&["--fixtures", "nonesuch", "/tmp/out"])).unwrap();
    assert_eq!(chosen_fixtures(&all, &a).map_err(|f| f.code), Err(2));
    let a = parse_args(&args(&["/tmp/out"])).unwrap();
    assert_eq!(chosen_fixtures(&all, &a).unwrap().len(), 4);
}

#[test]
fn a_frames_name_reads_back() {
    for (look, size, at) in [
        (Look::Dark, Size::Regular, None),
        (Look::HcLight, Size::Regular2x, Some(150)),
        (Look::RtDark, Size::Compact, Some(0)),
    ] {
        let name = frame_name("needs-you", look, size, at);
        assert_eq!(
            parse_frame_name(&name),
            Some(("needs-you".to_string(), look, size, at)),
            "{name}"
        );
    }
    assert_eq!(
        frame_name("set-up", Look::Light, Size::Compact, None),
        "set-up.light.compact"
    );
    assert_eq!(parse_frame_name("dark.regular"), None);
    assert_eq!(parse_frame_name(".dark.regular"), None);
    assert_eq!(parse_frame_name("running.sepia.regular"), None);
    assert_eq!(parse_frame_name("running.dark.regular.gui"), None);
}

#[test]
fn the_system_state_view_is_found_in_the_tab_topology() {
    let tabs = "window id=1 focused=true tabs=2 active=2 rows=36 cols=110\n  \
                tab id=1 index=1 kind=terminal title=\"sh\" state=idle active=false\n    \
                leaf path=/ view=3 rect_px=0,0,900,600 focused=false kind=terminal session=1\n  \
                tab id=2 index=2 kind=app title=\"System State\" state=idle active=true\n    \
                leaf path=/ view=9 rect_px=0,0,900,600 focused=true kind=app app=system-state title=\"System State\"\n";
    assert_eq!(system_state_view(tabs), Some(9));
    assert_eq!(system_state_view("window id=1 rows=2"), None);
    let view = View {
        id: 9,
        tabs: tabs.to_string(),
    };
    assert_eq!(view.regular_rows().unwrap(), "36");
}

#[test]
fn a_layouts_shape_ignores_the_ticking_fingerprint() {
    let audit = |fingerprint: &str, y: &str| {
        format!(
            "app-inspection version=app/v1\n\
             inspection-source source=staged compiled-fingerprint={fingerprint} scale=1.000\n\
             paint-node key=\"village\" kind=custom rect=0.0,{y},10.0,10.0\n\
             paint-text key=\"t\" kind=text overflow=false painted=\"Hi\"\n"
        )
    };
    // The village's tick changes the fingerprint and no rect: the same shape.
    assert_eq!(
        layout_shape(&audit("aa", "0.0")),
        layout_shape(&audit("bb", "0.0"))
    );
    // A node that moves is another shape (the negative control).
    assert_ne!(
        layout_shape(&audit("aa", "0.0")),
        layout_shape(&audit("aa", "4.0"))
    );
    assert!(layout_shape("inspection-source scale=1.000\n").is_empty());
}
