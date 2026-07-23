// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{Args, ParsedArgs, SpawnSpec, parse_args_from, record_fixture};
use aterm_containment::ContainmentMode;
use aterm_tempfile::tempdir;

fn init_test_mode() {
    let _ = aterm_containment::init_mode(ContainmentMode::Master);
}

#[test]
fn parse_args_accepts_direct_command_after_separator() {
    let args = vec![
        "--output".to_string(),
        "out".to_string(),
        "--".to_string(),
        "printf".to_string(),
        "hello\n".to_string(),
    ];

    let ParsedArgs::Run(parsed) = parse_args_from(&args).expect("parse direct argv") else {
        panic!("expected run args");
    };

    assert_eq!(
        parsed.spawn,
        SpawnSpec::Direct {
            program: "printf".to_string(),
            argv: vec!["printf".to_string(), "hello\n".to_string()],
        }
    );
}

#[test]
fn parse_args_rejects_mixed_command_modes() {
    let args = vec![
        "--command".to_string(),
        "echo hello".to_string(),
        "--output".to_string(),
        "out".to_string(),
        "--".to_string(),
        "printf".to_string(),
        "hello\n".to_string(),
    ];

    let error = parse_args_from(&args).expect_err("mixed modes must fail");
    assert!(error.contains("either --command or a direct command"));
}

#[test]
fn record_direct_command_with_arguments_writes_fixture() {
    init_test_mode();

    let temp = tempdir().expect("tempdir");
    let output = temp.path().join("direct");
    let args = Args {
        spawn: SpawnSpec::Direct {
            program: "printf".to_string(),
            argv: vec!["printf".to_string(), "hello via argv\n".to_string()],
        },
        rows: 4,
        cols: 40,
        output: output.clone(),
        description: Some("direct argv".to_string()),
        working_dir: None,
        input_file: None,
        startup_delay_ms: 0,
    };

    let captured = record_fixture(&args).expect("record direct argv fixture");
    let output_text = String::from_utf8_lossy(&captured);
    assert!(
        output_text.contains("hello via argv"),
        "captured output should include direct argv output, got {output_text:?}"
    );

    let meta = std::fs::read_to_string(output.join("meta.json")).expect("read meta");
    assert!(meta.contains("\"description\": \"direct argv\""));
}

#[test]
fn record_shell_command_with_scripted_input_writes_fixture() {
    init_test_mode();

    let temp = tempdir().expect("tempdir");
    let input_path = temp.path().join("input.txt");
    std::fs::write(&input_path, b"hello from input\n").expect("write input");

    let output = temp.path().join("shell");
    let args = Args {
        spawn: SpawnSpec::ShellCommand("read line; printf 'echo:%s\\n' \"$line\"".to_string()),
        rows: 4,
        cols: 40,
        output,
        description: Some("stdin script".to_string()),
        working_dir: None,
        input_file: Some(input_path),
        startup_delay_ms: 0,
    };

    let captured = record_fixture(&args).expect("record shell fixture");
    let output_text = String::from_utf8_lossy(&captured);
    assert!(
        output_text.contains("echo:hello from input"),
        "captured output should include scripted stdin echo, got {output_text:?}"
    );
}
