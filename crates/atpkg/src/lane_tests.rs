// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn input_past_the_limit_is_refused_not_cut() {
    let limit = usize::try_from(super::MAX_INPUT_BYTES).unwrap();
    let at = vec![b'a'; limit];
    assert_eq!(super::read_input(&at[..]).map(|v| v.len()), Ok(limit));
    let over = vec![b'a'; limit + 1];
    assert!(super::read_input(&over[..]).is_err());
}

use super::*;

fn s(list: &[&str]) -> Vec<String> {
    list.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn the_command_line_is_named_exactly_once() {
    assert_eq!(
        parse_args(&s(&["--", "cargo", "+1.97.1", "test", "--help"])),
        Ok(Args {
            cwd: None,
            source: Source::Words(s(&["cargo", "+1.97.1", "test", "--help"])),
        }),
        "everything after `--` is the command, its own flags included"
    );
    assert_eq!(
        parse_args(&s(&["--cwd", "/r", "-"])),
        Ok(Args {
            cwd: Some(PathBuf::from("/r")),
            source: Source::Stdin,
        })
    );
    assert_eq!(
        parse_args(&s(&["--json=tool_input.command", "--cwd=/r"])),
        Ok(Args {
            cwd: Some(PathBuf::from("/r")),
            source: Source::Json("tool_input.command".into()),
        })
    );
    for bad in [
        &[][..],
        &["--"][..],
        &["--json"][..],
        &["--cwd"][..],
        &["-", "--json", "a"][..],
        &["--json", "a", "--", "cargo"][..],
        &["cargo", "build"][..],
        &["--verbose", "-"][..],
    ] {
        assert!(parse_args(&s(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn a_json_field_is_read_by_its_dotted_path() {
    let payload = br#"{"session_id":"x","cwd":"/r","tool_name":"Bash","tool_input":{"command":"cargo +1.97.1 test -p x","description":"d"}}"#;
    let field = Source::Json("tool_input.command".into());
    assert_eq!(
        command_line(&field, payload),
        Ok(Some("cargo +1.97.1 test -p x".into()))
    );
    assert_eq!(
        command_line(&Source::Json("tool_input.file_path".into()), payload),
        Ok(None)
    );
    assert_eq!(
        command_line(&Source::Json("tool_input".into()), payload),
        Ok(None),
        "an object is not a command line"
    );
    assert!(command_line(&field, b"cargo build").is_err());
    assert!(command_line(&Source::Stdin, b"\xff\xfe").is_err());
    assert_eq!(
        command_line(&Source::Stdin, b"cargo build\n"),
        Ok(Some("cargo build\n".into()))
    );
}

#[test]
fn the_answer_is_silence_an_escape_line_or_the_refusal() {
    assert_eq!(
        answer("targo --unverified test -p x", None, None, None),
        Answer::Clean
    );
    assert_eq!(
        answer("git commit -m 'cargo build'", None, None, None),
        Answer::Clean
    );
    assert!(matches!(
        answer(
            "ATERM_STOCK_REASON='wasm32 cell' cargo +stable build --target wasm32-unknown-unknown",
            None,
            None,
            None,
        ),
        Answer::Escaped(_)
    ));
    let Answer::Refused(text) = answer("stdbuf -oL cargo +1.97.1 test -p x", None, None, None)
    else {
        panic!("a stock command is refused");
    };
    assert!(text.contains("targo --unverified test -p x"), "{text}");
    let dir = crate::testdir::dir("atpkg-lane-");
    let sub = dir.join("src");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        dir.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.97.1\"\n",
    )
    .unwrap();
    let Answer::Refused(pinned) = answer("cargo clippy", Some(&sub), None, None) else {
        panic!("refused");
    };
    assert!(pinned.contains("\"1.97.1\""), "{pinned}");
    let Answer::Refused(env) = answer("cargo clippy", Some(&sub), None, Some("stable")) else {
        panic!("refused");
    };
    assert!(env.contains("\"stable\""), "{env}");
    assert!(!env.contains("1.97.1"), "{env}");
    let _ = std::fs::remove_dir_all(&dir);
}
