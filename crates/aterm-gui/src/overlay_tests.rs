// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn status_line_reports_open_kind_fp_and_extent() {
    let cases = [
        Overlay::Palette(PaletteState::new()),
        Overlay::Overview(OverviewState::new(
            crate::WindowId(0),
            Vec::new(),
            Vec::new(),
        )),
    ];
    for o in &cases {
        let line = o.status_line();
        assert!(line.starts_with("overlay kind="), "shape: {line}");
        assert!(line.contains("open=true"), "open flag: {line}");
        assert!(
            line.contains(&format!("fp={}", o.fingerprint())),
            "fp binds the repaint key: {line}"
        );
        assert!(line.contains("scroll="), "extent scroll: {line}");
        assert!(line.contains("total="), "extent total: {line}");
        assert!(line.contains("visible="), "extent visible: {line}");
    }
}
