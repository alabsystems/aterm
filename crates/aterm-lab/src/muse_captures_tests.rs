// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_tool_rows_turn_is_over_once_its_result_was_answered() {
    let call = "1 POST /responses -> 200 body=9B mode=toolrow kind=muse auth=key model=lab-stub stream=true items=2 tools=1 instructions=0c last_user=\"x\" custom tool:search";
    let reply = "2 POST /responses -> 200 body=9B mode=toolrow kind=muse auth=key model=lab-stub stream=true items=4 tools=1 instructions=0c last_user=\"x\" custom";
    let side = "3 POST /responses -> 200 body=9B mode=toolrow kind=muse-side role=prompt-hint auth=key custom";
    assert!(tool_answered(
        &format!("{call}\n{reply}\n"),
        " custom tool:search"
    ));
    assert!(!tool_answered(&format!("{call}\n"), " custom tool:search"));
    assert!(!tool_answered(
        &format!("{call}\n{side}\n"),
        " custom tool:search"
    ));
    assert!(!tool_answered(
        &format!("{reply}\n{call}\n"),
        " custom tool:search"
    ));
    assert!(!tool_answered(
        &format!("{call}\n{reply}\n"),
        " custom tool:bash"
    ));
}

#[test]
fn a_slash_command_is_entered_only_when_the_menu_offers_it_first() {
    let rule = "─".repeat(60);
    let screen = |composer: &str, first: &str| {
        format!(
            "  Muse Code 1.4.10\n\n── Voice input (⌥ + v to start) {rule}\n❯ {composer}\n  {first}\n  /compact   Summarize\n{rule}\n  lab-stub · high · /p · Auto-review\n"
        )
    };
    assert!(menu_offers_first(
        &screen(
            "/usage",
            "/usage                Show session usage/cost/context"
        ),
        "/usage"
    ));
    assert!(!menu_offers_first(
        &screen(
            "/usage",
            "/upgrade              Show your subscription plan"
        ),
        "/usage"
    ));
    assert!(!menu_offers_first(
        &screen("/status", "/statusline   Choose the status line"),
        "/status"
    ));
    assert!(!menu_offers_first(
        &screen("/usag", "/usage                Show session usage"),
        "/usage"
    ));
    assert!(!menu_offers_first("no frame here", "/usage"));
}
