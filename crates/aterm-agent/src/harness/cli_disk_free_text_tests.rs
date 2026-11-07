// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn fixture() -> (Tmp, Env) {
    let tmp = tmp_dir("disk-free-text");
    let mut env = env_at(&tmp);
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    env.home = Some(std::fs::canonicalize(home).unwrap());
    crate::test_config::write(env.config.as_ref().unwrap(), b"[disk]\nauto_free_gib = 0\n")
        .unwrap();
    (tmp, env)
}

fn invoke(
    env: &Env,
    path: &Path,
    dry_run: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let paths = [path.to_path_buf()];
    let ask = DiskFree {
        paths: &paths,
        dry_run,
        json,
    };
    let nobody = |_: &[PathBuf], _: &[u32]| -> io::Result<aterm_uds::holders::PathScan> {
        Ok(aterm_uds::holders::PathScan::default())
    };
    let table = aterm_sysprobe::volumes::Table {
        complete: true,
        ..Default::default()
    };
    run_disk_free_with(env, &ask, &nobody, &table, out, err)
}

fn unchanged_cache_and_refusal(env: &Env, path: &Path, dry_run: bool) {
    let cache = env.home.as_ref().unwrap().join(".cache");
    assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0);
    if dry_run {
        assert!(!env.disk_ledger().exists());
    } else {
        let ledger = std::fs::read_to_string(env.disk_ledger()).unwrap();
        let rows: Vec<Value> = ledger
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|s| aterm_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]["action"] == Value::from("refused"));
        assert!(rows[0]["path"] == Value::from(path.display().to_string()));
    }
}

fn render(value: &str, dry_run: bool, json: bool) -> String {
    let (_tmp, env) = fixture();
    let path = env.home.as_ref().unwrap().join(".cache").join(value);
    // The grant is off, and the path does not exist. No control-bearing file is created.
    let config = std::fs::read(env.config.as_ref().unwrap()).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        invoke(&env, &path, dry_run, json, &mut out, &mut err),
        ExitCode::from(1)
    );
    unchanged_cache_and_refusal(&env, &path, dry_run);
    assert_eq!(std::fs::read(env.config.as_ref().unwrap()).unwrap(), config);
    if json {
        let text = std::str::from_utf8(&out).unwrap();
        let rows: Vec<Value> = text
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|s| aterm_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]["path"] == Value::from(path.display().to_string()));
        assert!(rows[0]["dry_run"] == Value::from(dry_run));
        assert!(rows[0]["removed"] == Value::from(false));
        assert!(rows[0]["grant"] == Value::from(false));
        assert!(rows[0]["refused"].as_str().is_some_and(|s| !s.is_empty()));
    }
    String::from_utf8(out).unwrap()
}

fn safe_lines(text: &str, count: usize) -> bool {
    text.bytes().filter(|b| *b == b'\n').count() == count
        && text
            .chars()
            .all(|c| c == '\n' || !aterm_codec::tty::is_active(c))
}

#[test]
fn disk_free_text_metadata_is_visible_with_intended_line_boundaries() {
    let mut failures = Vec::new();
    for dry_run in [false, true] {
        let lines = render("AB", dry_run, false)
            .bytes()
            .filter(|b| *b == b'\n')
            .count();
        assert_eq!(lines, 2, "one outcome and the summary");
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
            let text = render(&format!("A{control}B"), dry_run, false);
            if !safe_lines(&text, lines) || !text.contains(&format!("A{escaped}B")) {
                failures.push(format!(
                    "unsafe metadata dry_run={dry_run} control={control:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn disk_free_text_json_keeps_original_path_values() {
    for dry_run in [false, true] {
        render("A\0\n\r\t\u{1b}\u{9b}\u{2028}\u{202e}B", dry_run, true);
    }
}

#[test]
fn disk_free_text_ordinary_unicode_and_backslashes_remain_literal() {
    let value = "café 名字 👩\u{200d}💻\\normal trust=literal";
    for dry_run in [false, true] {
        assert!(render(value, dry_run, false).contains(value));
    }
}

#[test]
fn disk_free_text_delivery_errors_are_one_safe_line() {
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
    for dry_run in [false, true] {
        for json in [false, true] {
            for flush in [false, true] {
                let (_tmp, env) = fixture();
                crate::test_config::write(
                    env.config.as_ref().unwrap(),
                    b"[disk]\nauto_free_gib = 10\n",
                )
                .unwrap();
                let config = std::fs::read(env.config.as_ref().unwrap()).unwrap();
                let path = env.home.as_ref().unwrap().join(".cache/not-created");
                let mut err = Vec::new();
                assert_eq!(
                    invoke(&env, &path, dry_run, json, &mut Failed { flush }, &mut err),
                    ExitCode::from(1)
                );
                let diagnostic = String::from_utf8(err).unwrap();
                if !safe_lines(&diagnostic, 1)
                    || !diagnostic.contains("could not write results: failureA\\n\\x1b[2JfailureB")
                {
                    failures.push(format!(
                        "unsafe delivery error dry_run={dry_run} json={json} flush={flush}"
                    ));
                }
                unchanged_cache_and_refusal(&env, &path, dry_run);
                assert_eq!(std::fs::read(env.config.as_ref().unwrap()).unwrap(), config);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn disk_free_text_config_read_error_escapes_synthetic_path() {
    let mut failures = Vec::new();
    for dry_run in [false, true] {
        for json in [false, true] {
            let (tmp, mut env) = fixture();
            let config_path = env.config.clone().unwrap();
            let before = std::fs::read(&config_path).unwrap();
            // NUL makes this path invalid on every OS; it is never created.
            env.config = Some(tmp.path().join("cfg\n\u{1b}[2J\0tail"));
            let path = env.home.as_ref().unwrap().join(".cache/not-created");
            let (mut out, mut err) = (Vec::new(), Vec::new());
            assert_eq!(
                invoke(&env, &path, dry_run, json, &mut out, &mut err),
                ExitCode::from(1)
            );
            let diagnostic = String::from_utf8(err).unwrap();
            if !safe_lines(&diagnostic, if dry_run { 2 } else { 1 })
                || !diagnostic.contains("cfg\\n\\x1b[2J\\x00tail")
            {
                failures.push(format!("unsafe config error dry_run={dry_run} json={json}"));
            }
            unchanged_cache_and_refusal(&env, &path, dry_run);
            assert_eq!(std::fs::read(config_path).unwrap(), before);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
