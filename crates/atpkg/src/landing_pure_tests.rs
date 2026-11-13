// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn the_hand_over_parses_its_operands_and_runs_the_bin_shim_verbatim() {
    let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let prefix_str = if cfg!(windows) {
        "C:\\Program Files (x86)\\aterm\\pkg"
    } else {
        "/tmp/prefix (x86)"
    };
    let rest = a(&["claude", prefix_str, "--", "-p", "--", "50% done", "a b"]);
    let h = HandOver::parse(&rest).unwrap();
    assert_eq!(h.program, "claude");
    assert_eq!(h.prefix.as_deref(), Some(std::path::Path::new(prefix_str)));
    assert_eq!(h.args, &a(&["-p", "--", "50% done", "a b"])[..]);
    let old = a(&["codex", "--", "--", "x"]);
    let h = HandOver::parse(&old).unwrap();
    assert_eq!(h.prefix, None);
    assert_eq!(h.args, &a(&["--", "x"])[..]);
    let rel = a(&["claude", "notes.md"]);
    let h = HandOver::parse(&rel).unwrap();
    assert_eq!(h.prefix, None);
    assert_eq!(h.args, &a(&["notes.md"])[..]);
    assert_eq!(HandOver::parse(&[]), None);
    let layout = Layout {
        prefix: PathBuf::from(prefix_str),
    };
    let tool = ToolName::new("claude").unwrap();
    let args = a(&["-p", "--", "50% done", "a b"]);
    let command = shim_command(&layout, &tool, &args);
    assert_eq!(
        command.get_program(),
        layout.bin_dir().join(tool.shim_file()).as_os_str()
    );
    assert_eq!(
        command.get_program().to_string_lossy().ends_with(".cmd"),
        cfg!(windows),
        "the .cmd wrapper on Windows, the sh shim elsewhere"
    );
    assert!(!command.get_program().to_string_lossy().contains("agents"));
    let got: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(got, args);
}
