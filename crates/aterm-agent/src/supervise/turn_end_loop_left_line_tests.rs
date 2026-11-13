// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// WENT TAKES A COUNT THAT STOOD STILL: a host with no key count at all says nothing of who
/// cleared the composer, though "no count" equals "no count".
#[test]
fn a_line_goes_only_on_a_count_that_held() {
    let line = |stands, human| LeftLine {
        text: String::new(),
        point_key: String::new(),
        stands,
        human,
    };
    let on = |human_seq| Screen {
        human_seq,
        ..Screen::default()
    };
    assert!(line(LeftStands::Whole, Some(3)).went(&on(Some(3))));
    assert!(line(LeftStands::Part, Some(3)).went(&on(Some(3))));
    assert!(!line(LeftStands::Whole, Some(3)).went(&on(Some(4))));
    assert!(!line(LeftStands::Nothing, Some(3)).went(&on(Some(3))));
    assert!(!line(LeftStands::Whole, None).went(&on(None)));
}
