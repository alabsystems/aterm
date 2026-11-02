// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
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
    // A list whose count disagrees with its type, or no list at all, is refused.
    assert!(parse_fixtures("const FIXTURES: [&str; 2] = [\"a\"];").is_err());
    assert!(parse_fixtures("fn main() {}").is_err());
    assert_eq!(
        parse_fixtures("pub(crate) const FIXTURES: [&str; 2] = [\n    \"a\",\n    \"b-c\",\n];")
            .unwrap(),
        vec!["a", "b-c"]
    );
}

#[test]
fn the_command_line_names_the_matrix() {
    let a = parse_args(&args(&["/tmp/out"])).unwrap();
    assert_eq!(a.looks, Look::ALL.to_vec());
    assert_eq!(a.sizes, Size::ALL.to_vec());
    assert!(a.fixtures.is_none() && a.at.is_empty() && !a.quick);
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
        "/tmp/out",
    ]))
    .unwrap();
    assert_eq!(a.looks, vec![Look::HcDark, Look::HcLight, Look::Light]);
    assert_eq!(a.sizes, vec![Size::Compact]);
    assert_eq!(a.at, vec![0, 150]);
    assert_eq!(
        a.fixtures,
        Some(vec!["running".to_string(), "failed".to_string()])
    );
    let quick = parse_args(&args(&["--quick", "/tmp/out"])).unwrap();
    assert_eq!(quick.looks, vec![Look::Dark, Look::Light]);
    assert_eq!(quick.sizes, vec![Size::Regular, Size::Compact]);
    for bad in [
        &["--looks", "sepia", "/tmp/out"][..],
        &["--sizes", "huge", "/tmp/out"],
        &["--at", "soon", "/tmp/out"],
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
                tab id=1 index=0 kind=terminal title=\"sh\" state=idle active=false\n    \
                leaf path=/ view=3 rect_px=0,0,900,600 focused=false kind=terminal session=1\n  \
                tab id=2 index=1 kind=app title=\"System State\" state=idle active=true\n    \
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
fn the_motion_note_says_when_frames_are_only_settled() {
    assert!(motion_note(&[], false).contains("no ATERM_DEV_MOTION_AT_MS seam yet"));
    assert!(motion_note(&[], true).contains("one settled frame"));
    assert!(motion_note(&[0, 300], true).contains("0, 300 ms"));
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
