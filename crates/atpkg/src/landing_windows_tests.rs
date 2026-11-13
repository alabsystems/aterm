// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn the_verb_runs_the_bin_shim_as_a_child_with_its_real_code() {
    let p = crate::testdir::dir("atpkg-landing-win-");
    let layout = Layout {
        prefix: p.to_path_buf(),
    };
    let tool = ToolName::new("claude").unwrap();
    let log = layout.prefix.join("log.txt");
    let target = layout
        .build_dir("claude", 2026091601)
        .join("bin\\claude.cmd");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let mut body = String::from("@echo ran %*>> \"");
    body.push_str(&log.to_string_lossy());
    body.push_str("\"\r\n@exit /b 3\r\n");
    std::fs::write(&target, body).unwrap();
    std::fs::create_dir_all(layout.bin_dir()).unwrap();
    crate::platform::install_shim_to(&layout.shim(&tool), &target).unwrap();
    let args = vec![String::from("--"), String::from("x")];
    let st = shim_command(&layout, &tool, &args).status().unwrap();
    assert_eq!(st.code(), Some(3));
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "ran -- x");
    let _ = std::fs::remove_dir_all(&layout.prefix);
}
