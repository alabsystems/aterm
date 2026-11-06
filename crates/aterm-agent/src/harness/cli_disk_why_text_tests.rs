// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn fixture(value: &str) -> (Tmp, Env) {
    let tmp = tmp_dir("disk-why-text");
    let env = env_at(&tmp);
    let lease = disk::ask::Lease {
        volumes: vec![disk::ask::Entry {
            key: value.into(),
            ..disk::ask::Entry::default()
        }],
        ..disk::ask::Lease::default()
    };
    std::fs::create_dir_all(&env.state).unwrap();
    std::fs::write(env.state.join(disk::ask::LEASE_FILE), lease.encode()).unwrap();
    let host = disk::ask::HostId {
        pid: 7,
        start: "fixture".into(),
    };
    disk_ask_journal(
        &env.state,
        &[disk::ask::step_row(env.now, value, value, value, &host)],
    );
    assert!(disk::ask::read(&env.state).unwrap().volumes[0].key == value);
    (tmp, env)
}

fn snapshot(env: &Env) -> [Vec<u8>; 2] {
    [env.state.join(disk::ask::LEASE_FILE), env.disk_ledger()].map(|p| std::fs::read(p).unwrap())
}

fn render(value: &str, json: bool) -> String {
    let (_tmp, env) = fixture(value);
    let before = snapshot(&env);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_disk_why(&env, json, &mut out, &mut err),
        ExitCode::SUCCESS
    );
    assert!(err.is_empty());
    assert_eq!(snapshot(&env), before, "reporting must remain read-only");
    String::from_utf8(out).unwrap()
}

fn one_safe_line(text: &str) -> bool {
    text.bytes().filter(|b| *b == b'\n').count() == 1
        && text
            .chars()
            .all(|c| c == '\n' || !aterm_codec::tty::is_active(c))
}

#[test]
fn disk_why_text_metadata_is_visible_with_intended_line_boundaries() {
    let baseline = render("AB", false);
    let lines = baseline.bytes().filter(|b| *b == b'\n').count();
    assert!(
        lines >= 3,
        "lease and journal fixture must both be displayed"
    );
    let mut failures = Vec::new();
    for (control, escaped) in [
        ('\0', "\\x00"),
        ('\t', "\\t"),
        ('\n', "\\n"),
        ('\r', "\\r"),
        ('\u{1b}', "\\x1b"),
        ('\u{9b}', "\\u{009b}"),
        ('\u{2028}', "\\u{2028}"),
        ('\u{202e}', "\\u{202e}"),
    ] {
        let out = render(&format!("A{control}B"), false);
        let framed = out.bytes().filter(|b| *b == b'\n').count() == lines;
        let visible = out.contains(&format!("A{escaped}B"));
        let inactive = out
            .chars()
            .all(|c| c == '\n' || !aterm_codec::tty::is_active(c));
        if !framed || !visible || !inactive {
            failures.push(format!(
                "control={control:?}: framed={framed}, visible={visible}, inactive={inactive}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn disk_why_text_json_preserves_lease_and_verbatim_journal_values() {
    let value = "A\n\r\t\u{1b}\u{9b}\u{2028}\u{202e}B";
    let (_tmp, env) = fixture(value);
    let before = snapshot(&env);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_disk_why(&env, true, &mut out, &mut err),
        ExitCode::SUCCESS
    );
    assert!(err.is_empty());
    let expected = [before[0].as_slice(), b"\n", before[1].as_slice()].concat();
    assert!(
        out == expected,
        "JSON lease and raw journal bytes must be preserved"
    );
    let out = String::from_utf8(out).unwrap();
    let rows: Vec<Value> = out
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|s| aterm_json::from_str(s).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0]["volumes"][0]["key"] == Value::from(value));
    for key in ["sid", "volume", "step"] {
        assert!(rows[1][key] == Value::from(value));
    }
    assert_eq!(snapshot(&env), before);
}

#[test]
fn disk_why_text_ordinary_unicode_and_backslashes_remain_literal() {
    let value = "café 名字 👩\u{200d}💻\\normal trust=literal";
    assert!(render(value, false).contains(value));
}

#[test]
fn disk_why_text_delivery_error_is_one_safe_line_and_read_only() {
    struct Failed {
        flush: bool,
    }
    impl Write for Failed {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.flush {
                Ok(bytes.len())
            } else {
                Err(io::Error::other("failureA\n\u{1b}[2JfailureB"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("failureA\n\u{1b}[2JfailureB"))
        }
    }
    let mut failures = Vec::new();
    for json in [false, true] {
        let (_tmp, env) = fixture("ordinary");
        let before = snapshot(&env);
        for flush in [false, true] {
            let mut err = Vec::new();
            assert_eq!(
                run_disk_why(&env, json, &mut Failed { flush }, &mut err),
                ExitCode::from(1)
            );
            let diagnostic = String::from_utf8(err).unwrap();
            if !one_safe_line(&diagnostic) || !diagnostic.contains("failureA\\n\\x1b[2JfailureB") {
                failures.push(format!("unsafe delivery error json={json} flush={flush}"));
            }
            assert_eq!(snapshot(&env), before);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn disk_why_text_read_error_escapes_synthetic_path_without_creating_state() {
    let tmp = tmp_dir("disk-why-invalid-path");
    let mut env = env_at(&tmp);
    // NUL makes this an invalid path on every OS. No control-bearing file is created.
    env.state = tmp.path().join("state\n\u{1b}[2J\0tail");
    let mut failures = Vec::new();
    for json in [false, true] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_disk_why(&env, json, &mut out, &mut err),
            ExitCode::from(1)
        );
        assert!(out.is_empty(), "read failure must emit no result");
        let diagnostic = String::from_utf8(err).unwrap();
        if !one_safe_line(&diagnostic)
            || !diagnostic.contains("state\\n\\x1b[2J\\x00tail")
            || !diagnostic.contains(DISK_LEDGER)
        {
            failures.push(format!("unsafe read error json={json}"));
        }
    }
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
