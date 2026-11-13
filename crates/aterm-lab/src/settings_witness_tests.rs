// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_uds::spawnfd::Stdio;
use std::cell::Cell;

use super::*;

const SETTINGS: &str = "/l/runs/t/scratch/home/.claude/settings.json";

fn line(event: &str, file: &str) -> String {
    format!(
        "{{\"session_id\":\"s\",\"hook_event_name\":\"{event}\",\"source\":\"user_settings\",\"file_path\":\"{file}\"}}"
    )
}

#[test]
fn a_reread_is_a_config_change_line_naming_the_settings_file() {
    let ok = line("ConfigChange", SETTINGS);
    assert_eq!(rereads(&format!("{ok}\n{ok}\n"), SETTINGS), 2);
    for (what, text) in [
        (
            "another file",
            line("ConfigChange", "/l/runs/t/proj/.claude/settings.json"),
        ),
        ("another event", line("PreToolUse", SETTINGS)),
        ("not JSON", format!("ConfigChange {SETTINGS}")),
        (
            "no file_path",
            "{\"hook_event_name\":\"ConfigChange\"}".to_string(),
        ),
        ("nothing", String::new()),
    ] {
        assert_eq!(rereads(&text, SETTINGS), 0, "{what}: {text}");
    }
}

fn stepped() -> SteppedWait<impl Fn()> {
    SteppedWait::new(REWRITE_EVERY / 3, || {})
}

#[test]
fn an_unanswered_write_is_made_again_until_a_line_answers() {
    let after_a_read = REWRITE_EVERY + REWRITE_EVERY / 3;
    for (what, from_read, after) in [
        ("after a read", 2u32, after_a_read),
        ("during the rewrite", 1, REWRITE_EVERY),
    ] {
        let writes = Cell::new(0u32);
        let reads = Cell::new(0u32);
        let r = until_reread(
            || {
                writes.set(writes.get() + 1);
                reads.set(0);
                Ok(())
            },
            || {
                reads.set(reads.get() + 1);
                usize::from(writes.get() >= 2 && reads.get() >= from_read)
            },
            REWRITE_EVERY,
            HANG,
            &stepped(),
        )
        .expect(what);
        assert_eq!(r, Reread { writes: 2, after }, "{what}");
        assert_eq!(writes.get(), 2, "{what}: no write after the answer");
    }
}

#[test]
fn a_line_from_before_the_write_ends_or_none_at_all_is_no_reread() {
    let made = u32::try_from(HANG.as_millis() / REWRITE_EVERY.as_millis()).unwrap();
    let due: Vec<_> = (0..made).map(|k| REWRITE_EVERY * k).collect();
    for (line, bumps) in [("a line during the write", true), ("no line", false)] {
        for (took, slow) in [("no time", false), ("a step", true)] {
            let what = format!("{line}, each write taking {took}");
            let lines = Cell::new(0usize);
            let wait = stepped();
            let mut at = Vec::new();
            let r = until_reread(
                || {
                    at.push(wait.elapsed());
                    if slow {
                        wait.pause()?;
                    }
                    if bumps && at.len() == 1 {
                        lines.set(lines.get() + 1);
                    }
                    Ok(())
                },
                || lines.get(),
                REWRITE_EVERY,
                HANG,
                &wait,
            );
            let e = r.expect_err(&what);
            assert!(e.msg.unwrap_or_default().contains("no re-read"), "{what}");
            assert_eq!(wait.elapsed(), HANG, "{what}: the hang detector's moment");
            assert_eq!(at, due, "{what}: one write every `every`, from the first");
        }
    }
}

struct Ticking {
    next: Cell<Duration>,
    last: Cell<Duration>,
    tick: Duration,
}

impl Wait for Ticking {
    fn elapsed(&self) -> Duration {
        self.last.set(self.next.get());
        self.next.set(self.next.get() + self.tick);
        self.last.get()
    }

    fn pause(&self) -> Result<(), Fail> {
        Ok(())
    }
}

#[test]
fn no_write_follows_a_reading_at_or_past_hang() {
    let late: Vec<_> = [1, 1000, 7000]
        .into_iter()
        .filter_map(|ms| {
            let wait = Ticking {
                next: Cell::new(Duration::ZERO),
                last: Cell::new(Duration::ZERO),
                tick: Duration::from_millis(ms),
            };
            let mut at = Vec::new();
            let r = until_reread(
                || {
                    at.push(wait.last.get());
                    Ok(())
                },
                || 0,
                REWRITE_EVERY,
                HANG,
                &wait,
            );
            assert!(r.is_err(), "tick {ms} ms: a silent log is no re-read");
            at.iter().any(|t| *t >= HANG).then_some((ms, at))
        })
        .collect();
    assert!(
        late.is_empty(),
        "(tick in ms, the readings before each write): {late:?}"
    );
}

#[test]
fn the_hook_runs_the_script_which_appends_its_input_and_never_fails() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("a dir");
    std::fs::create_dir(&dir).unwrap();
    let sh = dir.join(SCRIPT);
    let h = hooks(&sh);
    let cmd = h
        .get("ConfigChange")
        .and_then(J::as_arr)
        .and_then(|a| a.first())
        .and_then(|m| m.get("hooks"))
        .and_then(J::as_arr)
        .and_then(|a| a.first())
        .and_then(|c| c.get("command"))
        .and_then(J::as_str)
        .unwrap()
        .to_string();
    assert_eq!(cmd, format!("'{}'", sh.display()));
    let fed = root.path().join("input");
    let run = |log: &Path, input: &str| {
        std::fs::write(&sh, script(log)).unwrap();
        std::fs::write(&fed, input).unwrap();
        aterm_uds::spawnfd::command("/bin/sh")
            .arg(&sh)
            .stdin(std::fs::File::open(&fed).unwrap())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
            .wait()
            .unwrap()
            .code()
    };
    let log = dir.join(LOG);
    let input = line("ConfigChange", SETTINGS);
    assert_eq!(run(&log, &input), Some(0));
    assert_eq!(run(&log, &input), Some(0));
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text, format!("{input}\n{input}\n"));
    assert_eq!(rereads(&text, SETTINGS), 2);
    let gone = root.path().join("gone").join(LOG);
    assert_eq!(run(&gone, &input), Some(0));
    assert!(!gone.exists());
}
