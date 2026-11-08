// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::Path;

#[cfg(unix)]
const EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
const SID: &str = "s-60776b6e2ed1b73bd013";

/// A Claude Code 2.1.300 status-line payload (lab capture `tap-inline.jsonl`, paths replaced).
fn payload(api_ms: u64, five: Option<(u32, i64)>, seven: Option<(u32, i64)>) -> String {
    let window = |w: Option<(u32, i64)>| {
        w.map_or_else(
            || "null".to_string(),
            |(pct, at)| format!(r#"{{"used_percentage":{pct},"resets_at":{at}}}"#),
        )
    };
    let limits = if five.is_none() && seven.is_none() {
        String::new()
    } else {
        format!(
            r#","rate_limits":{{"five_hour":{},"seven_day":{}}}"#,
            window(five),
            window(seven)
        )
    };
    format!(
        r#"{{"session_id":"6cf4169e-20c7-4bcb-a97f-e8a1387ed6ea",
"transcript_path":"/Users//example/.claude/projects/-Users-example-proj/6cf4169e-20c7-4bcb-a97f-e8a1387ed6ea.jsonl",
"cwd":"/Users//example/proj","model":{{"id":"claude-sonnet-5-5","display_name":"Sonnet 5.5"}},
"version":"2.1.300","cost":{{"total_cost_usd":0,"total_duration_ms":234,"total_api_duration_ms":{api_ms}}},
"prompt_cache":{{"warm":false,"expires_at":null,"requests":1}}{limits}}}"#
    )
}

fn vars() -> TapVars {
    TapVars {
        sid: Some(SID.to_string()),
        pid: Some("68537".to_string()),
        login: None,
        home: None,
    }
}

fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-tap-{label}-"))
        .tempdir()
        .expect("scratch")
}

/// A file's permission bits: the reading's privacy is a unix mode.
#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// The tests' now: Thu 15 Oct 2026 08:07:40 UTC.
const NOW: i64 = 1_792_051_660;

/// The tests' local clock: UTC ([`run`]'s `offset`).
fn utc(_: i64) -> Option<i64> {
    Some(0)
}

/// Run the verb on `stdin`, into `dir`: its exit and the one line it printed.
fn tap(stdin: &[u8], vars: &TapVars, dir: Option<&Path>) -> (std::process::ExitCode, String) {
    let mut out = Vec::new();
    let code = run(&mut &stdin[..], false, vars, dir, NOW, utc, &mut out);
    (code, String::from_utf8(out).expect("utf-8"))
}

/// The settings value round-trips for an admissible path, and every near miss (a key more,
/// another command, an argument after `tap`, a relative path, a space or a quote) is not ours.
#[cfg(unix)]
#[test]
fn the_tap_value_is_exact_and_only_it_is_ours() {
    let value = settings_value(Path::new(EXE)).expect("an admissible path");
    assert_eq!(
        value,
        r#"{"statusLine":{"type":"command","command":"/Applications/aterm.app/Contents/MacOS/aterm harness tap","refreshInterval":10}}"#
    );
    assert!(is_ours(&value));
    let other = |from: &str, to: &str| value.replacen(from, to, 1);
    for near in [
        other("\"refreshInterval\":10", "\"refreshInterval\":11"),
        other("}}", "},\"model\":\"x\"}"),
        other("harness tap", "harness tap --x"),
        other("harness tap", "harness usage"),
        other("/Applications", "Applications"),
        r#"{"statusLine":{"type":"command","command":"echo mine"}}"#.to_string(),
        r#"{"model":"x"}"#.to_string(),
        format!(" {value}"),
    ] {
        assert!(!is_ours(&near), "{near}");
    }
    for exe in [
        "/Applications/aterm (dev).app/Contents/MacOS/aterm",
        "/Users//o'neil/aterm",
        "relative/aterm",
        "/tmp/$HOME/aterm",
        "/tmp/a*b/aterm",
        "/",
    ] {
        assert!(settings_value(Path::new(exe)).is_none(), "{exe}");
        let forged = format!("{VALUE_HEAD}{exe}{COMMAND_TAIL}{VALUE_TAIL}");
        assert!(!is_ours(&forged), "{exe}");
    }
    let argv = |rest: &[&str]| -> Vec<String> {
        std::iter::once("claude")
            .chain(rest.iter().copied())
            .map(str::to_owned)
            .collect()
    };
    assert!(carried_by(&argv(&["--settings", &value])));
    assert!(carried_by(&argv(&[&format!("--settings={value}")])));
    assert!(!carried_by(&argv(&[
        "--append-system-prompt",
        "--settings",
        &value
    ])));
    assert!(!carried_by(&argv(&["--", "--settings", &value])));
    assert!(!carried_by(&argv(&["--settings", r#"{"model":"x"}"#])));
    assert!(!carried_by(&argv(&[])));
}

/// ON WINDOWS THE VALUE NAMES A DRIVE PATH WITH `/` SEPARATORS. Claude Code runs a status-line
/// command through Git Bash (else PowerShell), where a `\` is an escape: the path the window's
/// canonical `current_exe` gives (`\\?\C:\…`) is spelled `C:/…`, which Git Bash, pwsh 7 and
/// Windows PowerShell 5.1 each run, and the value round-trips as ours.
#[cfg(windows)]
#[test]
fn a_windows_exe_is_named_with_forward_slashes() {
    let installed = r"C:\Users\m6-an\AppData\Local\Programs\aterm\aterm.exe";
    let value = settings_value(Path::new(installed)).expect("an admissible path");
    assert_eq!(
        value,
        r#"{"statusLine":{"type":"command","command":"C:/Users//m6-an/AppData/Local/Programs/aterm/aterm.exe harness tap","refreshInterval":10}}"#
    );
    assert!(is_ours(&value));
    assert!(exe_admissible(Path::new(installed)));
    let canonical = format!(r"\\?\{installed}");
    assert_eq!(
        settings_value(Path::new(&canonical)).as_deref(),
        Some(value.as_str())
    );
    for exe in [
        r"C:\Program Files\aterm\aterm.exe",
        r"C:\Users\o'neil\aterm.exe",
        r"C:\a$b\aterm.exe",
        r"\\server\share\aterm.exe",
        r"\\?\UNC\server\share\aterm.exe",
        r"aterm\aterm.exe",
        r"C:aterm.exe",
        "/Applications/aterm.app/Contents/MacOS/aterm",
    ] {
        assert!(settings_value(Path::new(exe)).is_none(), "{exe}");
    }
    let backslashed = format!(r"{VALUE_HEAD}C:\\Users\\aterm.exe{COMMAND_TAIL}{VALUE_TAIL}");
    assert!(!is_ours(&backslashed), "{backslashed}");
}

/// The tap's command names the console image: on Windows the window runs `aterm-gui.exe` (a
/// build tree's `aterm-windowed.exe`), which would open a window for `harness tap`, so the
/// console `aterm.exe` beside it is named, and no tap where there is none.
#[test]
fn the_tap_names_the_console_image_beside_a_windowed_one() {
    let dir = Path::new(r"C:\Users\u\AppData\Local\Programs\aterm");
    let gui = dir.join("aterm-gui.exe");
    let console = dir.join("aterm.exe");
    let beside = |path: &Path| path == console;
    if cfg!(windows) {
        assert_eq!(tap_image(&gui, beside), Some(console.clone()));
        assert_eq!(
            tap_image(&dir.join("aterm-windowed.exe"), beside),
            Some(console.clone())
        );
        assert_eq!(tap_image(&gui, |_| false), None);
    } else {
        assert_eq!(tap_image(&gui, |_| false), Some(gui.clone()));
    }
    assert_eq!(tap_image(&console, |_| false), Some(console.clone()));
}

/// The row the tap prints at [`NOW`] for a week 61% used that resets Mon 19 Oct 08:06:36 UTC.
const WEEK_61: &str = "week 61% used \u{b7} resets Mon 08:06\n";

/// A payload with figures writes a v1 file (0600 in a 0700 directory) and prints the week; no
/// figures, a five-hour window alone, non-JSON, empty or oversized stdin prints [`NO_WEEK`] and
/// still writes the reading, with no figures and its row `own`, over the one held (the row Claude
/// shows is the tap's no-figure words, which the glass's claim reads the file for); no tab, no
/// process or no directory writes nothing; every run prints one line and exits 0. Negative
/// control: a `record` that writes only a reading with figures (`has_figures`) leaves the held
/// week in the file and fails the figure-less pin.
#[test]
fn the_verb_writes_its_reading_and_always_prints_one_line_and_exits_zero() {
    let root = scratch("verb");
    let dir = root.path().to_path_buf();
    let rl = dir.join("rl");
    let five = Some((42, 1_792_062_396));
    let seven = Some((61, 1_792_397_196));
    let (code, line) = tap(payload(18, five, seven).as_bytes(), &vars(), Some(&rl));
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(line, WEEK_61);
    let file = rl.join(format!("{SID}-68537.json"));
    #[cfg(unix)]
    {
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&rl), 0o700);
    }
    let (read, _) = Reading::read(&rl, SID, 68537, 0).expect("its own file");
    assert_eq!(read.api_ms, 18);
    assert_eq!(
        read.five_hour,
        Some(Window {
            pct: 42,
            resets_at: 1_792_062_396,
            pct_x10: Some(420),
        })
    );
    assert_eq!(read.seven_day.map(|w| w.pct), Some(61));
    assert_eq!(read.version.as_deref(), Some("2.1.300"));
    assert_eq!(read.model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(
        read.session.as_deref(),
        Some("6cf4169e-20c7-4bcb-a97f-e8a1387ed6ea")
    );
    assert_eq!(read.warm, Some(false));
    let text = std::fs::read_to_string(&file).expect("read");
    assert!(text.contains("\"v\":1"), "{text}");
    let names: Vec<_> = std::fs::read_dir(&rl)
        .expect("ls")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");

    let candidate = rl.join(format!(".{SID}-68537.json.tmp{}", std::process::id()));
    std::fs::write(&candidate, "borrowed sentinel").expect("candidate");
    let (_, collision_line) = tap(payload(19, five, seven).as_bytes(), &vars(), Some(&rl));
    assert_eq!(collision_line, WEEK_61);
    assert_eq!(
        std::fs::read_to_string(&candidate).expect("preserved"),
        "borrowed sentinel"
    );
    assert_eq!(std::fs::read_to_string(&file).expect("last reading"), text);
    std::fs::remove_file(&candidate).expect("remove owned sentinel");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::FileTypeExt as _;
        let name = std::ffi::CString::new(candidate.as_os_str().as_bytes()).expect("path");
        // SAFETY: name is a live terminated path in the owned directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (_, collision_line) = tap(payload(19, five, seven).as_bytes(), &vars(), Some(&rl));
        assert_eq!(collision_line, WEEK_61);
        assert!(
            std::fs::symlink_metadata(&candidate)
                .expect("preserved FIFO")
                .file_type()
                .is_fifo()
        );
        assert_eq!(std::fs::read_to_string(&file).expect("last reading"), text);
        std::fs::remove_file(&candidate).expect("remove owned FIFO");
    }
    let _ = tap(payload(19, five, seven).as_bytes(), &vars(), Some(&rl));
    assert_eq!(
        Reading::read(&rl, SID, 68537, 0)
            .expect("write recovered")
            .0
            .api_ms,
        19
    );

    let over = vec![b' '; STDIN_MAX + 10];
    for stdin in [
        payload(0, None, None).into_bytes(),
        b"not json at all".to_vec(),
        Vec::new(),
        over,
    ] {
        let _ = tap(payload(20, five, seven).as_bytes(), &vars(), Some(&rl));
        let (code, line) = tap(&stdin, &vars(), Some(&rl));
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, format!("{NO_WEEK}\n"));
        let (held, _) = Reading::read(&rl, SID, 68537, 0).expect("the figure-less reading");
        assert_eq!(
            (held.five_hour, held.seven_day, held.row),
            (None, None, Some(Row::Own)),
            "{:?}",
            String::from_utf8_lossy(&stdin[..stdin.len().min(40)])
        );
    }
    let (_, line) = tap(payload(0, five, None).as_bytes(), &vars(), Some(&rl));
    assert_eq!(line, format!("{NO_WEEK}\n"), "a five-hour window alone");
    let nowhere = dir.join("nowhere");
    for vars in [
        TapVars {
            sid: None,
            ..vars()
        },
        TapVars {
            sid: Some("../../etc".to_string()),
            ..vars()
        },
        TapVars {
            pid: Some("0".to_string()),
            ..vars()
        },
        TapVars {
            pid: Some("x".to_string()),
            ..vars()
        },
    ] {
        let (code, line) = tap(payload(1, five, seven).as_bytes(), &vars, Some(&nowhere));
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, format!("{NO_WEEK}\n"));
        assert!(!nowhere.exists(), "{vars:?}");
    }
    let blocked = dir.join("blocked");
    std::fs::write(&blocked, "a file where the directory would go").expect("write");
    let (code, line) = tap(
        payload(1, five, seven).as_bytes(),
        &vars(),
        Some(&blocked.join("rl")),
    );
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(line, WEEK_61);
    let (code, line) = tap(payload(1, five, seven).as_bytes(), &vars(), None);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(line, WEEK_61);
}

/// NOTHING OF STDIN IS WRITTEN AS IT CAME, and no variable but the four is
/// read: a payload carrying a token-shaped value in a field the tap does not
/// keep, and a process whose environment carries two credentials, leave
/// neither in the file nor on the line.
#[test]
fn the_verb_never_writes_a_secret_it_was_handed() {
    if std::env::var_os("ATERM_TAP_TEST_CHILD").is_some() {
        let _ = super::super::cli::main_entry(vec!["tap".into()]);
        return;
    }
    let root = scratch("secret");
    let dir = root.path().to_path_buf();
    let state = aterm_types::dirs::resolve_state_dir(
        None,
        aterm_types::dirs::StatePlatform {
            home: Some(dir.clone()),
            local_app_data: Some(dir.clone()),
            ..aterm_types::dirs::StatePlatform::default()
        },
    )
    .expect("the scratch HOME resolves a state root");
    let mut stdin = payload(5, Some((7, 1_792_062_396)), Some((9, 1_792_397_196)));
    stdin.insert_str(1, r#""oauth":"sk-ant-oat01-SECRET","#);
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("exe"));
    child
        .args([
            "--exact",
            "harness::tap::tests::the_verb_never_writes_a_secret_it_was_handed",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("ATERM_TAP_TEST_CHILD", "1")
        .env("ATERM_STATE_HOME", &state)
        .env("HOME", &dir)
        .env("LOCALAPPDATA", &dir)
        .env("ATERM_PARENT_SESSION_ID", SID)
        .env("CLAUDE_PID", "4242")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-SECRET")
        .env("ANTHROPIC_API_KEY", "sk-ant-api03-SECRET")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped());
    let mut child = child.spawn().expect("spawn");
    {
        use std::io::Write as _;
        let mut pipe = child.stdin.take().expect("stdin");
        pipe.write_all(stdin.as_bytes()).expect("write");
    }
    let out = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("week 9% used\n") && !stdout.contains("5h") && !stdout.contains("7%"),
        "{stdout}"
    );
    let file = state
        .join("harness")
        .join(super::super::cli::HARNESS)
        .join("rl")
        .join(format!("{SID}-4242.json"));
    let text = std::fs::read_to_string(&file).expect("the child wrote its reading");
    assert!(text.contains("\"pct\":7"), "{text}");
    assert!(!text.contains("SECRET"), "{text}");
    assert!(!stdout.contains("SECRET"), "{stdout}");
    assert!(!text.contains("transcript_path") && !text.contains("/Users//example"));
}

/// A RE-REPORT KEEPS THE FIRST REPORT'S STAMP (the account-stats design
/// §3.4, §3.5): Claude Code runs the tap every ten seconds while idle,
/// re-reporting the figure it holds, and the file's `at` stays the run's
/// that first reported it — so a figure read late is as old as it is, and
/// one from before a limit reset used inside its window is cut by that
/// reset however late the host reads it.
#[test]
fn a_re_report_keeps_the_first_reports_stamp() {
    use crate::harness::accounts::{self, Kind, State, TapRead, WindowRead};
    let root = scratch("first-report");
    let dir = root.path().to_path_buf();
    let reset = 1_792_062_396;
    let report = |api_ms: u64, pct: u32, now: i64| {
        let mut out = Vec::new();
        let stdin = payload(api_ms, Some((pct, reset)), None);
        let _ = run(
            &mut stdin.as_bytes(),
            false,
            &vars(),
            Some(&dir),
            now,
            utc,
            &mut out,
        );
        Reading::read(&dir, SID, 68537, 0).expect("its own file").0
    };
    let mut legacy = report(18, 100, 1000);
    assert_eq!(legacy.at, 1000);
    legacy.five_hour.as_mut().expect("five hour").pct_x10 = None;
    let name = file_name(SID, 68537).expect("name");
    write_reading(&dir, &name, &legacy.to_json()).expect("older v1 reading");
    assert_eq!(report(18, 100, 1010).at, 1000, "a re-report");
    let late = report(18, 100, 1366);
    assert_eq!(late.at, 1000, "a re-report, read late");
    let five = |pct_x10: u32, refilled: Option<u32>, at: i64| TapRead {
        five_hour: Some(WindowRead {
            pct_x10,
            reset,
            refilled,
        }),
        seven_day: None,
        at,
        seen_before: true,
        born: None,
        key: None,
    };
    let account = |b_late: TapRead| {
        let mut state = State::default();
        let mut resets = 0;
        let mut note = |state: &mut State, read: &TapRead, now: i64| {
            resets += accounts::note_reading(state, "main", read, now)
                .iter()
                .filter(|e| e.kind == Kind::Reset)
                .count();
        };
        note(&mut state, &five(1000, None, 990), 990);
        note(&mut state, &five(30, Some(1000), 1100), 1100);
        note(&mut state, &b_late, 1366);
        let held = state.accounts["main"].windows[accounts::SESSION_LIMIT].pct_x10;
        note(&mut state, &five(20, Some(1000), 1400), 1400);
        (held, resets)
    };
    let beside = Beside {
        seen: true,
        refilled: [None, None],
    };
    assert_eq!(account(late.tap_read(beside, None)), (30, 1));
    let rewritten = TapRead {
        at: 1366,
        ..late.tap_read(beside, None)
    };
    assert_eq!(account(rewritten), (1000, 2));
    assert_eq!(report(31, 100, 1376).at, 1376, "served since");
    assert_eq!(report(31, 99, 1386).at, 1386, "a figure moved");
    let mut legacy = report(31, 42, 1396);
    legacy.five_hour.as_mut().expect("five hour").pct_x10 = None;
    legacy.seven_day = Some(Window {
        pct: 61,
        resets_at: reset + 3600,
        pct_x10: None,
    });
    write_reading(&dir, &name, &legacy.to_json()).expect("both older windows");
    let mut precise = legacy.clone();
    precise.at = 1406;
    precise.five_hour.as_mut().expect("five hour").pct_x10 = Some(420);
    precise.seven_day.as_mut().expect("week").pct_x10 = Some(610);
    let path = dir.join(&name);
    assert_eq!(first_reported(&path, &precise), 1396);
    for five in [true, false] {
        let mut moved = precise.clone();
        let window = if five {
            moved.five_hour.as_mut()
        } else {
            moved.seven_day.as_mut()
        }
        .expect("window");
        window.pct_x10 = Some(window.x10() + 1);
        assert_eq!(first_reported(&path, &moved), 1406, "a tenth moved");
    }
}

/// A WINDOW THAT ONLY DROPPED OUT AT ITS RESET IS NO NEW REPORT: Claude Code hands a window
/// only while it runs, so an idle tab's five-hour window leaves its re-report at its reset
/// with nothing served and the week unchanged, and the file keeps the week's first stamp (a
/// three-hour-old week never reads as current); a window that appears, or a week that moves,
/// is one.
#[test]
fn a_window_that_only_reset_away_keeps_the_first_reports_stamp() {
    let root = scratch("reset-away");
    let dir = root.path().to_path_buf();
    let (five, week) = (1_792_062_396, 1_792_500_000);
    let report = |pct_5h: Option<u32>, pct_7d: u32, now: i64| {
        let mut out = Vec::new();
        let stdin = payload(18, pct_5h.map(|pct| (pct, five)), Some((pct_7d, week)));
        let _ = run(
            &mut stdin.as_bytes(),
            false,
            &vars(),
            Some(&dir),
            now,
            utc,
            &mut out,
        );
        Reading::read(&dir, SID, 68537, 0).expect("its own file").0
    };
    let first = report(Some(12), 38, 1000);
    assert_eq!(first.at, 1000);
    let idle = report(None, 38, 1000 + 3 * 3600);
    assert_eq!(idle.five_hour, None, "the five-hour window reset away");
    assert_eq!(idle.at, 1000, "the week's first report stands");
    assert_eq!(
        report(None, 38, 1000 + 3 * 3600 + 10).at,
        1000,
        "and its re-report"
    );
    assert_eq!(
        report(Some(0), 38, 1000 + 3 * 3600 + 20).at,
        1000 + 3 * 3600 + 20,
        "a window that appears is a new report"
    );
    assert_eq!(
        report(Some(0), 39, 1000 + 3 * 3600 + 30).at,
        1000 + 3 * 3600 + 30,
        "a week that moved"
    );
}

/// A reading is read only from its own process's regular file: tab and pid in name and text,
/// written since the process started.
#[test]
fn a_reading_is_read_only_from_its_own_processes_file() {
    let root = scratch("read");
    let dir = root.path().to_path_buf();
    let vars = vars();
    let mut out = Vec::new();
    let _ = run(
        &mut payload(3, Some((10, 9)), None).as_bytes(),
        false,
        &vars,
        Some(&dir),
        100,
        utc,
        &mut out,
    );
    let name = format!("{SID}-68537.json");
    let now = aterm_time::unix_secs()
        .and_then(|s| i64::try_from(s).ok())
        .unwrap_or(0);
    assert!(Reading::read(&dir, SID, 68537, now - 60).is_some());
    assert!(Reading::read(&dir, "s-0000", 68537, 0).is_none());
    assert!(Reading::read(&dir, SID, 68538, 0).is_none());
    assert!(
        Reading::read(&dir, SID, 68537, now + 3600).is_none(),
        "older than the process"
    );
    std::fs::copy(dir.join(&name), dir.join(format!("{SID}-1.json"))).expect("copy");
    assert!(Reading::read(&dir, SID, 1, 0).is_none());
    #[cfg(unix)]
    {
        let link_dir = dir.join("links");
        std::fs::create_dir_all(&link_dir).expect("mkdir");
        std::os::unix::fs::symlink(dir.join(&name), link_dir.join(&name)).expect("symlink");
        assert!(Reading::read(&link_dir, SID, 68537, 0).is_none());
    }
    let text = std::fs::read_to_string(dir.join(&name)).expect("read");
    std::fs::write(dir.join(&name), text.replace("\"v\":1", "\"v\":2")).expect("write");
    assert!(Reading::read(&dir, SID, 68537, 0).is_none());
}

fn reading(pid: u32, api_ms: u64, five: Option<u32>, seven: Option<u32>) -> Reading {
    let w = |pct: Option<u32>, at: i64| {
        pct.map(|pct| Window {
            pct,
            resets_at: at,
            pct_x10: None,
        })
    };
    Reading {
        at: 0,
        sid: SID.to_string(),
        pid,
        session: None,
        login: None,
        home: None,
        api_ms,
        five_hour: w(five, 1_792_063_851),
        seven_day: w(seven, 1_792_398_651),
        warm: None,
        cache_expires_at: None,
        version: None,
        model: None,
        ctx_left: None,
        dur_us: 0,
        row: None,
    }
}

/// THE TENTHS AND THE MODEL (the account-stats design §4.2, F1): Claude Code
/// hands `used_percentage` with one decimal (`42.6` in the lab, 2.1.301):
/// the reading holds the whole percent it rounds to, the file keeps the tenths
/// and the process's `model.id` beside it, and both read back; the row says
/// the week's whole percent alone.
#[test]
fn a_reading_keeps_the_vendors_tenths_and_its_model() {
    let text = r#"{"model":{"id":"claude-opus-5-5"},"cost":{"total_api_duration_ms":9},
"rate_limits":{"five_hour":{"used_percentage":42.6,"resets_at":1792062396},
"seven_day":{"used_percentage":61,"resets_at":1792397196}}}"#;
    let r = Reading::of_stdin(text, &vars(), 1_792_050_000).expect("a reading");
    let five = r.five_hour.expect("the five-hour window");
    assert_eq!((five.pct, five.pct_x10, five.x10()), (43, Some(426), 426));
    assert_eq!(
        r.week_words(
            1_792_050_000,
            Some(&super::super::outlook::Offsets::fixed(0))
        ),
        "week 61% used \u{b7} resets Mon 08:06"
    );
    assert_eq!(
        r.seven_day.and_then(|w| w.pct_x10),
        Some(610),
        "a whole percent"
    );
    assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
    let back = Reading::of_json(&r.to_json()).expect("reads back");
    assert_eq!(back, r);
    let old = r#"{"v":1,"at":5,"sid":"s-1","pid":7,"session":null,"login":null,"home":null,
"api_ms":3,"five_hour":{"pct":42,"resets_at":9},"seven_day":null,"cache":{},"version":null}"#;
    let old = Reading::of_json(old).expect("an older file reads");
    let w = old.five_hour.expect("its window");
    assert_eq!((w.pct_x10, w.x10()), (None, 420));
    assert_eq!(old.model, None);
    let odd = text.replace("claude-opus-5-5", "not a model; rm -rf");
    let r = Reading::of_stdin(&odd, &vars(), 1_792_050_000).expect("a reading");
    assert_eq!(r.model, None);
}

/// NO READING UNDER 100 IS SAID AS 100, AND A BOUND STAYS TRUE
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.1): the tenths of a `used_percentage`
/// round down, so `99.96` is 999 tenths, said `99%` now and `≥99%` once aged, and `62.999` is
/// 629, whose bound `≥62%` the reading meets; only a reading of 100 is 1000 and `100%`. The
/// whole-percent field the usage line and the survey print holds the same rule: half up from the
/// tenths (62.96 is 63), but 99 at 99.5, 99.96 and 99.999999999999999, and 100 only at 100. The
/// exact JSON lexeme decides the boundary: a decimal just below 63 stays below it even where
/// `f64` would round it to 63, while exact `63.0` stays 630.
/// NEGATIVE CONTROLS: rounding through hundredths makes 62.999 630; parsing
/// 62.999999999999999 through `f64` makes it 63. Both falsely say the aged bound is 63%.
/// Rounding the whole field plainly half up from the tenths (`(x10 + 5) / 10`) makes 99.96 a
/// whole 100 (run once, restored).
/// The earlier negative control (run once, restored): rounding the tenths half up
/// (`(f * 10.0).round()`) gives 99.96 1000 and fails at `99%`.
#[test]
fn no_reading_under_a_hundred_is_said_as_a_hundred() {
    use super::super::outlook::Figure;
    let now = 1_792_050_000;
    let reset = now + 86_400;
    let read = |pct: &str| {
        let text = format!(
            r#"{{"cost":{{"total_api_duration_ms":9}},"rate_limits":{{"seven_day":{{"used_percentage":{pct},"resets_at":{reset}}}}}}}"#
        );
        let week = Reading::of_stdin(&text, &vars(), now)
            .expect("a reading")
            .seven_day
            .expect("its week");
        let x10 = u16::try_from(week.x10()).expect("tenths");
        (
            week.pct,
            week.x10(),
            Figure::Exact(x10).words(),
            Figure::AtLeast(x10).words(),
        )
    };
    let said = |pct: u32, x10: u32, now: &str, aged: &str| (pct, x10, now.into(), aged.into());
    assert_eq!(read("99.96"), said(99, 999, "99%", "\u{2265}99%"));
    assert_eq!(read("99.5"), said(99, 995, "99%", "\u{2265}99%"));
    assert_eq!(read("99.49"), said(99, 994, "99%", "\u{2265}99%"));
    assert_eq!(read("62.96"), said(63, 629, "63%", "\u{2265}62%"));
    assert_eq!(read("62.999"), said(63, 629, "63%", "\u{2265}62%"));
    assert_eq!(
        read("62.999999999999999"),
        said(63, 629, "63%", "\u{2265}62%")
    );
    assert_eq!(
        read("6.2999999999999999e1"),
        said(63, 629, "63%", "\u{2265}62%")
    );
    assert_eq!(read("63.0"), said(63, 630, "63%", "\u{2265}63%"));
    assert_eq!(read("62.6"), said(63, 626, "63%", "\u{2265}62%"));
    assert_eq!(
        read("99.999999999999999"),
        said(99, 999, "99%", "\u{2265}99%")
    );
    assert_eq!(
        read("100.0"),
        said(100, 1000, "100%", "\u{2265}100%"),
        "CONTROL: a reading of 100 is 100"
    );
    assert_eq!(read("100"), said(100, 1000, "100%", "\u{2265}100%"));
    assert_eq!(read("1e2"), said(100, 1000, "100%", "\u{2265}100%"));
}

/// THE CONTEXT LEFT (Claude Code 2.1.311's status-line payload): `context_window` carries
/// `remaining_percentage`, Claude Code's own count of the window left, which the file keeps
/// rounded for the survey; before the first reply it is `null`, and an older file, which never
/// had it, reads as none.
#[test]
fn a_reading_keeps_the_context_left_claude_code_counts() {
    let text = r#"{"cost":{"total_api_duration_ms":9},"context_window":{"total_input_tokens":
71000,"context_window_size":200000,"used_percentage":35.4,"remaining_percentage":64.6},
"rate_limits":{"five_hour":{"used_percentage":7,"resets_at":1792062396}}}"#;
    let r = Reading::of_stdin(text, &vars(), 1_792_050_000).expect("a reading");
    assert_eq!(r.ctx_left, Some(65));
    assert_eq!(Reading::of_json(&r.to_json()).expect("reads back"), r);
    let fresh = text.replace("64.6}", "null}");
    let r = Reading::of_stdin(&fresh, &vars(), 1_792_050_000).expect("a reading");
    assert_eq!(r.ctx_left, None, "no reply yet");
    let wild = text.replace("64.6}", "164}");
    let r = Reading::of_stdin(&wild, &vars(), 1_792_050_000).expect("a reading");
    assert_eq!(r.ctx_left, None, "past 100 is no percent");
    let old = r#"{"v":1,"at":5,"sid":"s-1","pid":7,"session":null,"login":null,"home":null,
"api_ms":3,"five_hour":null,"seven_day":null,"cache":{},"version":null}"#;
    assert_eq!(
        Reading::of_json(old).expect("an older file reads").ctx_left,
        None
    );
}

/// WHAT A READING SAYS BESIDE THE LAST ADMITTED ([`Fresh::beside`]): the
/// same process read before is `seen`; a window is REFILLED, from the
/// figure that process read before, where it was served since (`api_ms`
/// rose) and reads lower by a whole point or more under the same reset —
/// the drop a limit reset used inside the window makes.
#[test]
fn a_refill_is_a_drop_in_one_process_served_since_under_one_reset() {
    let at = |pct_x10: u32, reset: i64| {
        Some(Window {
            pct: (pct_x10 + 5) / 10,
            resets_at: reset,
            pct_x10: Some(pct_x10),
        })
    };
    let read = |pid: u32, api_ms: u64, five: Option<Window>| Reading {
        five_hour: five,
        ..reading(pid, api_ms, None, Some(30))
    };
    let mut fresh = Fresh::default();
    assert!(fresh.admit(&read(7, 100, at(1000, 50))));
    let beside = |r: &Reading| fresh.beside(r);
    assert_eq!(
        beside(&read(7, 200, at(50, 50))),
        Beside {
            seen: true,
            refilled: [Some(1000), None],
        }
    );
    assert_eq!(
        beside(&read(7, 100, at(50, 50))).refilled,
        [None, None],
        "no API time since"
    );
    assert_eq!(
        beside(&read(7, 200, at(50, 80))).refilled,
        [None, None],
        "another reset"
    );
    assert_eq!(
        beside(&read(7, 200, at(995, 50))).refilled,
        [None, None],
        "a tenth's wobble"
    );
    assert_eq!(
        beside(&read(8, 200, at(50, 50))),
        Beside::default(),
        "another process"
    );
    let r = Reading {
        at: 77,
        ..read(7, 200, at(50, 50))
    };
    let t = r.tap_read(beside(&r), None);
    assert_eq!(
        t.five_hour,
        Some(accounts::WindowRead {
            pct_x10: 50,
            reset: 50,
            refilled: Some(1000),
        })
    );
    assert_eq!((t.at, t.seen_before), (77, true));
    assert_eq!(
        t.seven_day.map(|w| (w.pct_x10, w.refilled)),
        Some((300, None))
    );
}

/// FRESHNESS (design §3.2) ON THE LAB'S OWN SEQUENCES (Claude Code 2.1.300,
/// E1): `tap-refresh2` — an idle process re-reporting 75/43 every 2 s with
/// no API time — is fresh once, at its first figures, and stale after;
/// `tap-bgagent-refresh1` — a background agent's usage arriving on the timer
/// — is fresh at every change; a new process is fresh at its first figures,
/// whatever it repeats; a reading with no figures never is.
#[test]
fn freshness_admits_news_and_never_an_idle_repeat() {
    let refresh2 = [(0, None, None), (0, Some(75), Some(43))]
        .into_iter()
        .chain(std::iter::repeat_n((0, Some(75), Some(43)), 10));
    let mut fresh = Fresh::default();
    let admitted: Vec<bool> = refresh2
        .map(|(api, five, seven)| fresh.admit(&reading(9, api, five, seven)))
        .collect();
    assert_eq!(admitted.iter().filter(|a| **a).count(), 1, "{admitted:?}");
    assert!(admitted[1], "the first figures");
    let bgagent = [
        (0, None, Some(60)),
        (0, Some(58), Some(60)),
        (48, Some(64), Some(60)),
        (58, Some(66), Some(60)),
        (71, Some(68), Some(60)),
        (79, Some(70), Some(60)),
        (96, Some(74), Some(60)),
    ];
    let mut fresh = Fresh::default();
    for (api, five, seven) in bgagent {
        assert!(fresh.admit(&reading(11, api, five, seven)), "{five:?}");
    }
    assert!(!fresh.admit(&reading(11, 96, Some(74), Some(60))));
    assert!(fresh.admit(&reading(11, 97, Some(74), Some(60))));
    assert!(fresh.admit(&reading(12, 0, Some(74), Some(60))));
    assert!(!fresh.admit(&reading(12, 0, Some(74), Some(60))));
    assert!(!Fresh::default().admit(&reading(13, 50, None, None)));
}

/// A tap is marked silent, once, only for a process up at least 30 s that served a turn, whose
/// file is missing or older than 30 s at two points 30 s apart; heard again, it is unmarked.
#[test]
fn a_tap_is_silent_only_after_two_strikes_on_a_served_process() {
    let mut s = Silence::default();
    assert_eq!(s.step(100, 7, 60, true, None), Heard::Same, "first strike");
    assert_eq!(s.step(120, 7, 80, true, Some(90)), Heard::Same, "too soon");
    assert_eq!(s.step(130, 7, 90, true, None), Heard::Silent);
    assert!(s.silent());
    assert_eq!(s.step(200, 7, 160, true, None), Heard::Same, "once");
    assert_eq!(s.step(210, 7, 170, true, Some(3)), Heard::Again);
    assert!(!s.silent());
    for (up, served, age) in [
        (60, false, None),
        (10, true, None),
        (60, true, Some(5)),
        (60, true, Some(30)),
    ] {
        let mut s = Silence::default();
        for at in [0, 40, 80, 120] {
            assert_eq!(
                s.step(at, 7, up, served, age),
                Heard::Same,
                "{up} {served} {age:?}"
            );
        }
        assert!(!s.silent());
    }
    let mut s = Silence::default();
    assert_eq!(s.step(0, 7, 60, true, None), Heard::Same);
    assert_eq!(
        s.step(40, 8, 60, true, None),
        Heard::Same,
        "another process"
    );
    assert_eq!(s.step(80, 8, 100, true, None), Heard::Silent);
}

/// A silent mark ends, once ([`Heard::Again`]), at the first point of a relaunched process or when
/// the tab's agent carries no tap; the new process needs two strikes of its own.
#[test]
fn a_silent_mark_ends_with_its_process() {
    let marked = || {
        let mut s = Silence::default();
        assert_eq!(s.step(0, 7, 60, true, None), Heard::Same);
        assert_eq!(s.step(30, 7, 90, true, None), Heard::Silent);
        s
    };
    for age in [None, Some(3)] {
        let mut s = marked();
        assert_eq!(s.step(60, 8, 60, true, age), Heard::Again, "{age:?}");
        assert!(!s.silent());
        assert_eq!(s.step(70, 8, 70, true, age), Heard::Same, "once: {age:?}");
    }
    let mut s = marked();
    assert_eq!(s.step(60, 8, 60, true, None), Heard::Again);
    assert_eq!(s.step(90, 8, 90, true, None), Heard::Silent, "its own");
    let mut s = marked();
    assert_eq!(s.untapped(9), Heard::Again);
    assert!(!s.silent());
    assert_eq!(s.untapped(9), Heard::Same, "once");
    let mut s = marked();
    assert_eq!(s.step(60, 7, 120, true, None), Heard::Same, "still marked");
    assert!(s.silent());
    let mut s = Silence::default();
    assert_eq!(s.step(0, 7, 60, true, None), Heard::Same);
    assert_eq!(s.step(30, 8, 60, true, None), Heard::Same, "none marked");
    assert_eq!(s.untapped(9), Heard::Same, "none marked");
}

/// A sweep takes a gone process's file, an old one and a stale temp file,
/// and leaves a live process's file and anything not named as ours.
#[test]
fn a_sweep_takes_the_dead_and_the_old_and_nothing_else() {
    let root = scratch("sweep");
    let dir = root.path().to_path_buf();
    for name in [
        format!("{SID}-1.json"),
        format!("{SID}-2.json"),
        format!(".{SID}-3.json.tmp99"),
        "notes.txt".to_string(),
        "s-zz-4.json".to_string(),
    ] {
        std::fs::write(dir.join(name), "{}").expect("write");
    }
    let now = aterm_time::unix_secs()
        .and_then(|s| i64::try_from(s).ok())
        .unwrap_or(0);
    sweep(&dir, now + 60, &|pid| pid == 2);
    let mut left: Vec<String> = std::fs::read_dir(&dir)
        .expect("ls")
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["notes.txt", "s-60776b6e2ed1b73bd013-2.json", "s-zz-4.json"]
    );
    sweep(&dir, now + 2 * SWEEP_AFTER_S as i64, &|_| true);
    let left: Vec<String> = std::fs::read_dir(&dir)
        .expect("ls")
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    assert!(!left.contains(&format!("{SID}-2.json")), "an old one");
}

/// The facts of a line the gate admits: the switch on, the machine's own
/// stored login, a plain launch, every document there and quiet.
#[cfg(unix)]
fn all_clear<'a>(argv: &'a [String], settings: &'a [(String, Source)]) -> GateFacts<'a> {
    GateFacts {
        exe: Some(Path::new(EXE)),
        route: crate::harness::accounts::AuthRoute::Stored,
        login: None,
        lone_login: None,
        home: None,
        argv,
        settings,
        user_settings: Some("user"),
        profile_store: false,
    }
}

/// THE GATE TABLE: the all-clear row gets the tap, and EACH GUARD ALONE
/// turns it into none — the switch off, a path that cannot be named, a
/// process not on its stored login, a login or a home of its own, a profile
/// store, a person's `--settings` (either spelling) or `--bare`, an argv the
/// rewrite refuses, a `statusLine`, `disableAllHooks`, an `apiKeyHelper` or
/// a credential, profile or route in any document of the chain, a document
/// that cannot be read or parsed.
#[cfg(unix)]
#[test]
fn each_guard_alone_refuses_the_tap() {
    let argv: Vec<String> = ["claude", "--model", "opus"].map(str::to_owned).to_vec();
    let chain = |text: &str| -> Vec<(String, Source)> {
        vec![
            ("managed".to_string(), Source::Missing),
            ("user".to_string(), Source::Text("{}".to_string())),
            ("project".to_string(), Source::Text(text.to_string())),
        ]
    };
    let quiet = chain(r#"{"permissions":{"allow":["Bash(ls)"]},"env":{"FOO":"1"}}"#);
    let want = settings_value(Path::new(EXE));
    assert_eq!(gate(&all_clear(&argv, &quiet)), want);
    assert_eq!(
        gate(&GateFacts {
            login: Some(""),
            ..all_clear(&argv, &quiet)
        }),
        want
    );
    let renewed = "/Users//example/.aterm/logins/home-a.1";
    assert_eq!(
        gate(&GateFacts {
            login: Some(renewed),
            lone_login: Some(renewed),
            ..all_clear(&argv, &quiet)
        }),
        want
    );
    let tapped: Vec<String> = argv
        .iter()
        .cloned()
        .chain(["--settings".to_string(), want.clone().unwrap()])
        .collect();
    assert_eq!(gate(&all_clear(&tapped, &quiet)), want);
    let refused = |facts: GateFacts<'_>, why: &str| {
        assert_eq!(gate(&facts), None, "{why}");
    };
    refused(
        GateFacts {
            exe: None,
            ..all_clear(&argv, &quiet)
        },
        "the switch off",
    );
    refused(
        GateFacts {
            exe: Some(Path::new(
                "/Applications/aterm (dev).app/Contents/MacOS/aterm",
            )),
            ..all_clear(&argv, &quiet)
        },
        "a path with a space",
    );
    refused(
        GateFacts {
            exe: Some(Path::new("/Users//o'neil/aterm")),
            ..all_clear(&argv, &quiet)
        },
        "a path with a quote",
    );
    refused(
        GateFacts {
            route: crate::harness::accounts::AuthRoute::Outside,
            ..all_clear(&argv, &quiet)
        },
        "a credential, profile or route variable",
    );
    refused(
        GateFacts {
            login: Some("/Users//example/.claude-logins/b"),
            ..all_clear(&argv, &quiet)
        },
        "a named login",
    );
    refused(
        GateFacts {
            login: Some("/Users//example/.claude-logins/b"),
            lone_login: Some(renewed),
            ..all_clear(&argv, &quiet)
        },
        "a login beside the one account's",
    );
    refused(
        GateFacts {
            login: Some(renewed),
            lone_login: Some(renewed),
            home: Some("/Users//example/.claude-b"),
            ..all_clear(&argv, &quiet)
        },
        "the one account's login in a home of its own",
    );
    refused(
        GateFacts {
            home: Some("/Users//example/.claude-b"),
            ..all_clear(&argv, &quiet)
        },
        "CLAUDE_CONFIG_DIR",
    );
    refused(
        GateFacts {
            home: Some(""),
            ..all_clear(&argv, &quiet)
        },
        "CLAUDE_CONFIG_DIR set and empty",
    );
    refused(
        GateFacts {
            profile_store: true,
            ..all_clear(&argv, &quiet)
        },
        "a profile store config",
    );
    for extra in [
        vec!["--settings", r#"{"model":"x"}"#],
        vec![r#"--settings={"model":"x"}"#],
        vec!["--settings", "/Users//example/s.json"],
        vec!["--bare"],
        vec!["--print"],
        vec!["--no-such-flag"],
    ] {
        let mut a = argv.clone();
        a.extend(extra.iter().map(|s| (*s).to_string()));
        refused(all_clear(&a, &quiet), &format!("{extra:?}"));
    }
    for text in [
        r#"{"statusLine":{"type":"command","command":"echo mine"}}"#,
        r#"{"disableAllHooks":true}"#,
        r#"{"apiKeyHelper":"/usr/local/bin/key"}"#,
        r#"{"env":{"ANTHROPIC_API_KEY":"k"}}"#,
        r#"{"env":{"CLAUDE_CODE_OAUTH_TOKEN":"t"}}"#,
        r#"{"env":{"ANTHROPIC_PROFILE":"work"}}"#,
        r#"{"env":{"HTTPS_PROXY":"http://gw.example:8080"}}"#,
        r#"{"env":{"ANTHROPIC_BASE_URL":"http://gw.example"}}"#,
        r#"{"env":{"CLAUDE_SECURESTORAGE_CONFIG_DIR":""}}"#,
        r#"{"env":{"CLAUDE_CONFIG_DIR":"/x"}}"#,
        "{not json",
        "[1]",
    ] {
        refused(all_clear(&argv, &chain(text)), text);
    }
    for at in 0..3 {
        let mut c = quiet.clone();
        c[at].1 = Source::Text(r#"{"statusLine":{"type":"command","command":"x"}}"#.into());
        if c[at].0 == "user" {
            assert_eq!(
                gate(&all_clear(&argv, &c)),
                settings_value_carrying(Path::new(EXE), true),
                "the owner's own statusLine in the user's settings.json is carried (R-12)"
            );
        } else {
            refused(
                all_clear(&argv, &c),
                &format!("a status line in document {at}"),
            );
        }
        let mut c = quiet.clone();
        c[at].1 = Source::Unreadable;
        refused(all_clear(&argv, &c), &format!("document {at} unreadable"));
    }
}

/// The settings document a person's `~/.claude/settings.json` was on the measured machine
/// (2026-10-24, Claude Code 2.1.310): another app's statusLine hook, a plain command line, beside
/// hooks and an `env` block. The command is that file's, with its home replaced.
#[cfg(unix)]
const OWNER_SETTINGS_2_1_310: &str = include_str!("testdata/tap/claude-2.1.310-user-settings.json");

/// THE OWNER'S OWN STATUSLINE IS CARRIED, NOT A REFUSAL (owner ruling R-12, 2026-10-24: "THE
/// HARNESS MUST DO THIS STOP ASKING THE USER"). On the measured machine every Claude Code footer
/// read `weekly ? · /usage` because the person's `~/.claude/settings.json` held another app's
/// statusLine, and the gate refused the tap over any statusLine, so no line carried it and no
/// figure was ever read. The gate now carries that command (`--carry`, which names no command:
/// the tap reads it when it runs), the value is still exactly ours for every reader (`is_ours`,
/// `carried_by`, the relaunch's shedding), and every statusLine it cannot carry faithfully still
/// refuses: one in another document, two documents that set one, another shape, an over-long
/// command.
#[cfg(unix)]
#[test]
fn the_owners_own_statusline_is_carried_and_every_other_still_refuses() {
    let argv: Vec<String> = ["claude", "--dangerously-skip-permissions"]
        .map(str::to_owned)
        .to_vec();
    let owner = netwatch::settings_of(OWNER_SETTINGS_2_1_310)
        .and_then(|read| read.status_command)
        .expect("the measured statusLine is a plain command line");
    let chain = |user: &str, project: &str| -> Vec<(String, Source)> {
        vec![
            ("managed".to_string(), Source::Missing),
            ("project".to_string(), Source::Text(project.to_string())),
            ("user".to_string(), Source::Text(user.to_string())),
        ]
    };
    let measured = chain(OWNER_SETTINGS_2_1_310, "{}");
    let value = gate(&all_clear(&argv, &measured)).expect("the measured machine gets the tap");
    assert_eq!(
        value,
        settings_value_carrying(Path::new(EXE), true).expect("a carried value")
    );
    assert!(
        value.ends_with(r#" harness tap --carry","refreshInterval":10}}"#),
        "{value}"
    );
    assert!(
        !value.contains("orca"),
        "the line names no command of the owner's: the tap reads it when it runs: {value}"
    );
    assert_eq!(
        owner_command(OWNER_SETTINGS_2_1_310).as_deref(),
        Some(owner.as_str()),
        "the command the tap runs is the one the gate judged"
    );
    assert!(is_ours(&value));
    assert_eq!(carrying_of(&value), Some(true));
    let carried: Vec<String> = argv
        .iter()
        .cloned()
        .chain(["--settings".to_string(), value.clone()])
        .collect();
    assert!(
        carried_by(&carried),
        "the footer and the host know the line is tapped"
    );
    assert_eq!(
        gate(&all_clear(&carried, &measured)),
        Some(value.clone()),
        "a tapped line is relaunched with the same tap"
    );
    let refused = |c: &[(String, Source)], why: &str| {
        assert_eq!(gate(&all_clear(&argv, c)), None, "{why}");
    };
    let line = r#"{"statusLine":{"type":"command","command":"echo mine"}}"#;
    refused(&chain("{}", line), "a project's statusLine");
    refused(&chain(line, line), "two documents set one");
    refused(
        &[
            ("managed".to_string(), Source::Text(line.to_string())),
            ("user".to_string(), Source::Missing),
        ],
        "a managed statusLine",
    );
    assert_eq!(
        gate(&GateFacts {
            user_settings: None,
            ..all_clear(&argv, &chain(line, "{}"))
        }),
        None,
        "no document named the user's own"
    );
    let long = format!(
        r#"{{"statusLine":{{"type":"command","command":"{}"}}}}"#,
        "x".repeat(CHAIN_MAX + 1)
    );
    for odd in [
        r#"{"statusLine":{"type":"command","command":"echo mine","refreshInterval":5}}"#,
        r#"{"statusLine":{"type":"command","command":"echo mine","hideVimModeIndicator":true}}"#,
        r#"{"statusLine":{"type":"command","command":"echo mine","padding":"2"}}"#,
        r#"{"statusLine":{"type":"text","command":"echo mine"}}"#,
        r#"{"statusLine":{"type":"command","command":""}}"#,
        r#"{"statusLine":{"type":"command","command":["echo"]}}"#,
        r#"{"statusLine":{"type":"command"}}"#,
        r#"{"statusLine":"echo mine"}"#,
        r#"{"statusLine":{"type":"command","command":"echo mine"},"disableAllHooks":true}"#,
        long.as_str(),
    ] {
        refused(&chain(odd, "{}"), odd);
    }
    assert_eq!(
        gate(&all_clear(
            &argv,
            &chain(
                r#"{"statusLine":{"type":"command","command":"echo mine","padding":0}}"#,
                "{}"
            )
        )),
        settings_value_carrying(Path::new(EXE), true),
        "a padding beside the command is the owner's presentation, carried by Claude Code's merge"
    );
}

/// THE CARRIED VALUE IS EXACT AND SHORT: it names no command, so its length is the plain tap's
/// and [`CARRY_FLAG`]'s, whatever the owner's command (the measured one is 333 bytes, which the
/// hex-encoded value of 864de5a37b put on every relaunch line, past a Darwin tty's line cap);
/// every near miss is not ours, so the relaunch never sheds or keeps a value aterm did not write;
/// and the legacy `--then <hex>` value is still ours, so a relaunch sheds it.
#[cfg(unix)]
#[test]
fn the_carried_value_is_exact_and_only_it_is_ours() {
    let value = settings_value_carrying(Path::new(EXE), true).expect("a value");
    assert_eq!(
        value,
        r#"{"statusLine":{"type":"command","command":"/Applications/aterm.app/Contents/MacOS/aterm harness tap --carry","refreshInterval":10}}"#
    );
    let plain = settings_value(Path::new(EXE)).expect("a value");
    assert_eq!(value.len(), plain.len() + 1 + CARRY_FLAG.len());
    assert_eq!(carrying_of(&plain), Some(false));
    assert_eq!(carrying_of(&value), Some(true));
    let other = |from: &str, to: &str| value.replacen(from, to, 1);
    for near in [
        other(" --carry", " --cary"),
        other(" --carry", " --carry 6563686f"),
        other(" --carry", " --carry "),
        other(" --carry", "  --carry"),
        other(
            r#""refreshInterval":10"#,
            r#""refreshInterval":10,"padding":1"#,
        ),
        other(" harness tap --carry", " harness tap x --carry"),
        other("/Applications/aterm.app", "aterm.app"),
    ] {
        assert!(!is_ours(&near), "{near}");
    }
    let legacy = |hex: &str| other(" --carry", &format!(" --then {hex}"));
    assert_eq!(
        carrying_of(&legacy("6563686f206d696e65")),
        Some(true),
        "864de5a37b's value for `echo mine` is still ours, so a relaunch sheds it"
    );
    for near in [
        legacy("6563686F206D696E65"),
        legacy("6563686f206d696e6"),
        legacy("6563686f00206d696e65"),
        legacy(""),
        legacy("zz"),
        legacy(&"78".repeat(CHAIN_MAX + 1)),
    ] {
        assert!(!is_ours(&near), "{near}");
    }
}

/// THE CARRYING TAP RUNS THE COMMAND THE USER'S `settings.json` NAMES WHEN IT RUNS: the measured
/// 2.1.310 document's command; nothing (and the tap prints its own row) where that document now
/// sets no statusLine, another shape, an over-long command, or turns hooks off; and the file is
/// read under the user's home, never through a symlink.
#[cfg(unix)]
#[test]
fn the_carrying_tap_reads_the_owners_command_when_it_runs() {
    let owner = netwatch::settings_of(OWNER_SETTINGS_2_1_310)
        .and_then(|read| read.status_command)
        .expect("the measured statusLine");
    assert_eq!(owner_command(OWNER_SETTINGS_2_1_310), Some(owner.clone()));
    let long = format!(
        r#"{{"statusLine":{{"type":"command","command":"{}"}}}}"#,
        "x".repeat(CHAIN_MAX + 1)
    );
    for none in [
        "{}",
        r#"{"theme":"dark"}"#,
        r#"{"statusLine":{"type":"command","command":"echo mine","refreshInterval":5}}"#,
        r#"{"statusLine":{"type":"text","command":"echo mine"}}"#,
        r#"{"statusLine":{"type":"command","command":"echo mine"},"disableAllHooks":true}"#,
        "{not json",
        long.as_str(),
    ] {
        assert_eq!(owner_command(none), None, "{none}");
    }
    let home = scratch("owner-home");
    assert_eq!(read_owner_command(home.path()), None, "no settings.json");
    let dir = home.path().join(".claude");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("settings.json"), OWNER_SETTINGS_2_1_310).expect("write");
    assert_eq!(read_owner_command(home.path()), Some(owner));
    let elsewhere = home.path().join("elsewhere.json");
    std::fs::rename(dir.join("settings.json"), &elsewhere).expect("move");
    std::os::unix::fs::symlink(&elsewhere, dir.join("settings.json")).expect("link");
    assert_eq!(
        read_owner_command(home.path()),
        None,
        "a settings.json that is a symlink is not read"
    );
}

/// Claude Code 2.1.300's idle screen with the tap's row, at 110x32 and at 80x24 beside the
/// vendor's credits notice.
fn tap_screens() -> [Vec<String>; 2] {
    use aterm_phase::prompt::fixtures::screen;
    [
        screen(include_str!(
            "../../../aterm-phase/src/fixtures/claude-2.1.300-tap-row-110.txt"
        )),
        screen(include_str!(
            "../../../aterm-phase/src/fixtures/claude-2.1.300-tap-row-80-credits.txt"
        )),
    ]
}

/// `rows` with the row under the composer's bottom rule replaced by `row`.
fn with_tap_row(rows: &[String], row: &str) -> Vec<String> {
    let bottom = aterm_phase::phase::composer_bottom(rows).expect("the composer");
    let mut out = rows.to_vec();
    out[bottom + 1] = row.to_string();
    out
}

/// One day, seconds.
const DAY: i64 = 86_400;

/// A reading of the week `x10` used, resetting at `resets_at`, beside a five-hour window 42% used.
fn week(x10: u32, resets_at: i64) -> Reading {
    Reading {
        seven_day: Some(Window {
            pct: (x10 + 5) / 10,
            resets_at,
            pct_x10: Some(x10),
        }),
        ..reading(68537, 0, Some(42), None)
    }
}

/// THE TAP'S ROW IS SAFE FOR EVERY READER (the real Claude Code 2.1.300's
/// screens, inline at 110 columns and at 80 beside `Now using usage
/// credits`): with every form of line the tap prints in its row (the widest
/// among them), the phase reads idle with no wall, the footer's mode row and
/// the live REPL's row are found under it, the line holds no guarded string of
/// the vendor's, and it ends two cells or more before Claude Code's notice,
/// where [`parse_line`] reads it back. The rows the fixtures measured are the
/// five-hour-and-week words of builds before this one, which it refuses.
#[test]
fn the_taps_row_reads_idle_and_keeps_the_mode_row_for_every_reader() {
    use super::super::footer;
    let utc = super::super::outlook::Offsets::fixed(0);
    let lines = [
        week(610, NOW + 3600).week_words(NOW, Some(&utc)),
        week(1000, NOW + 2 * DAY).week_words(NOW, Some(&utc)),
        week(1000, NOW + 7 * DAY).week_words(NOW, Some(&utc)),
        week(0, NOW - 1).week_words(NOW, Some(&utc)),
        NO_WEEK.to_string(),
    ];
    assert_eq!(lines[2], "week 100% used \u{b7} resets Thu 22 Oct 08:07");
    for rows in tap_screens() {
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        let measured = &rows[bottom + 1];
        assert!(measured.starts_with("  5h "), "{measured:?}");
        assert_eq!(parse_line(&measured[2..]), None, "{measured:?}");
        let words_end = measured[2..].find("  ").map_or(measured.len(), |e| 2 + e);
        let notice = measured[words_end..].find(|c: char| c != ' ').map(|at| {
            (
                measured[..words_end + at].chars().count(),
                &measured[words_end + at..],
            )
        });
        for line in &lines {
            let width = line.chars().count();
            let mut row = format!("  {line}");
            if let Some((col, text)) = notice {
                assert!(2 + width + 2 <= col, "{line} reaches the notice at {col}");
                row.extend(std::iter::repeat_n(' ', col - 2 - width));
                row.push_str(text);
                assert_eq!(row.chars().count(), measured.chars().count());
            }
            assert_eq!(parse_line(&row[2..]), Some(width), "{row}");
            let screen = with_tap_row(&rows, &row);
            let read = aterm_phase::read(Some("claude"), &screen, None);
            assert_eq!(read.phase, aterm_phase::Phase::Idle, "{row}");
            assert!(read.wall.is_none(), "{row}");
            assert_eq!(footer::mode_row(&screen), Some(bottom + 2), "{row}");
            assert_eq!(footer::live_repl_row(&screen), Some(bottom + 2), "{row}");
            for anchor in aterm_phase::ANCHORS {
                assert!(!line.contains(anchor.text), "{line} holds {}", anchor.id);
            }
        }
        let walled = with_tap_row(&rows, "  ⚠ Usage limit reached · limit resets 12:23pm");
        let read = aterm_phase::read(Some("claude"), &walled, None);
        assert!(
            read.wall.is_some() || matches!(read.phase, aterm_phase::Phase::Limited { .. }),
            "the readers read that row: {read:?}"
        );
        let said = with_tap_row(
            &rows,
            "  You've hit your session limit · resets 12:23pm (America/Los_Angeles)",
        );
        let read = aterm_phase::read(Some("claude"), &said, None);
        assert!(read.wall.is_none(), "a status line's words: {read:?}");
        let blank = with_tap_row(&rows, "");
        assert_eq!(
            footer::mode_row(&blank),
            None,
            "a blank row hides the mode row"
        );
    }
    for capture in [
        include_str!("../../../aterm-phase/src/fixtures/claude-2.1.300-tap-row-silent-command.txt"),
        include_str!(
            "../../../aterm-phase/src/fixtures/claude-2.1.300-tap-row-failing-command.txt"
        ),
    ] {
        let rows = aterm_phase::prompt::fixtures::screen(capture);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        assert_eq!(footer::mode_row(&rows), Some(bottom + 1));
    }
}

/// One Claude Code 2.1.318 capture of the tap's row in its week form (a stand-in statusLine
/// command printing the tap's new words, `docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §5.2).
struct WeekRow {
    /// The capture's file name, for messages.
    name: &'static str,
    /// The capture.
    capture: &'static str,
    /// What the row under the composer's bottom rule shows from column 2 on; `None` where the
    /// command printed nothing.
    words: Option<&'static str>,
    /// Claude's right-aligned notice and its row, counted from the bottom rule.
    notice: Option<(&'static str, usize)>,
    /// Whether a turn is running.
    busy: bool,
}

/// The 2.1.318 captures, at a turn end, at launch and mid-turn.
fn week_rows() -> [WeekRow; 15] {
    const WEEK: &str = "week 63% used \u{b7} resets Sat 10:00";
    const CREDITS: &str = "Now using usage credits";
    macro_rules! row {
        ($name:literal, $words:expr, $notice:expr, $busy:expr) => {
            WeekRow {
                name: $name,
                capture: include_str!(concat!("../../../aterm-phase/src/fixtures/", $name)),
                words: $words,
                notice: $notice,
                busy: $busy,
            }
        };
    }
    [
        row!("claude-2.1.318-tap-row-week.txt", Some(WEEK), None, false),
        row!(
            "claude-2.1.318-tap-row-week-100col.txt",
            Some(WEEK),
            None,
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-70col.txt",
            Some(WEEK),
            None,
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-30col.txt",
            Some("week 63% used \u{b7} resets Sa\u{2026}"),
            None,
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-launch.txt",
            Some(WEEK),
            Some(("\u{25d0} medium \u{b7} /effort", 1)),
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-mid-turn.txt",
            Some(WEEK),
            None,
            true
        ),
        row!(
            "claude-2.1.318-tap-row-week-mid-turn-100col.txt",
            Some(WEEK),
            None,
            true
        ),
        row!(
            "claude-2.1.318-tap-row-week-mid-turn-70col.txt",
            Some(WEEK),
            None,
            true
        ),
        row!(
            "claude-2.1.318-tap-row-week-mid-tool.txt",
            Some(WEEK),
            None,
            true
        ),
        row!(
            "claude-2.1.318-tap-row-no-figure.txt",
            Some("week \u{b7} no figure yet"),
            None,
            false
        ),
        row!(
            "claude-2.1.318-tap-row-silent-command.txt",
            None,
            None,
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-credits.txt",
            Some(WEEK),
            Some((CREDITS, 1)),
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-credits-70col.txt",
            Some(WEEK),
            Some((CREDITS, 1)),
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-credits-50col.txt",
            Some(WEEK),
            Some((CREDITS, 3)),
            false
        ),
        row!(
            "claude-2.1.318-tap-row-week-credits-manual-60col.txt",
            Some(WEEK),
            Some((CREDITS, 1)),
            false
        ),
    ]
}

/// THE WEEK ROW IS CLAUDE CODE 2.1.318'S ROW UNDER THE BOTTOM RULE, FOR EVERY READER (the real
/// Claude Code's screens at 170, 100, 70 and 30 columns, at launch, at a turn end, while a reply
/// streams and while a tool runs, beside its credits and effort notices): the row directly under
/// the composer's bottom rule holds the tap's words from column 2, the mode row is the next row
/// (the one under the rule where the command printed nothing), the phase reads idle or busy as the
/// turn stands and never a wall, Claude's notice sits on the row's right two cells short of the
/// edge (on a row of its own under the mode row when both do not fit), nothing of Claude's reaches
/// the last two columns, and the words hold no guarded string of the vendor's.
#[test]
fn claude_2_1_318_keeps_the_week_row_under_the_bottom_rule_for_every_reader() {
    use super::super::footer;
    for case in week_rows() {
        let name = case.name;
        let rows = aterm_phase::prompt::fixtures::screen(case.capture);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        let cols = rows[bottom].chars().count();
        let mode = bottom + 1 + usize::from(case.words.is_some());
        assert_eq!(footer::mode_row(&rows), Some(mode), "{name}");
        assert_eq!(footer::live_repl_row(&rows), Some(mode), "{name}");
        let read = aterm_phase::read(Some("claude"), &rows, None);
        let phase = if case.busy {
            aterm_phase::Phase::Busy
        } else {
            aterm_phase::Phase::Idle
        };
        assert_eq!(read.phase, phase, "{name}");
        assert!(read.wall.is_none(), "{name}: {read:?}");
        for row in &rows[bottom + 1..] {
            assert!(row.chars().count() <= cols - 2, "{name}: {row:?}");
        }
        if let Some(words) = case.words {
            let row = &rows[bottom + 1];
            assert_eq!(
                row.strip_prefix("  ").map(|r| r.starts_with(words)),
                Some(true),
                "{name}: {row:?}"
            );
            for anchor in aterm_phase::ANCHORS {
                assert!(
                    !words.contains(anchor.text),
                    "{name}: {words} holds {}",
                    anchor.id
                );
            }
            if case.notice.is_none_or(|(_, at)| at != 1) {
                assert_eq!(row.trim_end(), format!("  {words}"), "{name}");
            }
        }
        if let Some((notice, at)) = case.notice {
            let row = &rows[bottom + at];
            assert!(row.ends_with(notice), "{name}: {row:?}");
            assert_eq!(row.chars().count(), cols - 2, "{name}: {row:?}");
        }
    }
}

/// THE ONE BLOCK THAT ARMS THE TAP ([`Armed::arm`]), for the window's background lane and
/// `aterm harness upgrade` alike: on when the switch is and there is an aterm to name, with
/// the lone login the store binds to a one-row roster, else every login a roster of several
/// names as the store binds it, each login a renewal moved a row away from too (the logins
/// the footer's `row_left` promises a restart for); off where the clock or required account
/// records cannot be read.
#[test]
fn the_tap_is_armed_by_one_block_with_the_rosters_lone_login() {
    let abs = |path: &str| {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_owned()
        }
    };
    let root = scratch("armed");
    let roster = root.path().join(accounts::ACCOUNTS_FILE);
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .expect("roster");
    let now = 1_792_050_000;
    let login = abs("/s/main.1");
    let login = login.as_str();
    let mut logins = accounts::Logins::default();
    accounts::note_renewed(
        &mut accounts::State::default(),
        &mut logins,
        "main",
        None,
        login,
        None,
        now,
    );
    let store = accounts::Store::at(&root.path().join("state"));
    store.save_logins(&logins).expect("logins");
    let exe = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
    let arm = |on: bool, exe: Option<PathBuf>, now: Option<i64>| {
        Armed::arm(on, || exe, Some(&roster), &store, now)
    };
    assert_eq!(
        arm(true, Some(exe.clone()), Some(now)),
        Some(Armed {
            exe: exe.clone(),
            lone_login: Some(login.to_owned()),
            listed: Vec::new(),
        }),
        "the lone login the store binds"
    );
    assert_eq!(
        arm(true, Some(exe.clone()), None),
        None,
        "no clock read: no tap"
    );
    let work = abs("/s/work");
    let work = work.as_str();
    let work_renewed = abs("/s/work.1");
    let work_renewed = work_renewed.as_str();
    std::fs::write(
        &roster,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{work}\"\n"
        ),
    )
    .expect("two rows");
    let several = arm(true, Some(exe.clone()), Some(now)).expect("armed");
    assert_eq!(
        (several.lone_login.as_deref(), several.listed.clone()),
        (None, vec![login.to_owned(), work.to_owned()]),
        "two rows: no lone login, and every login they name, the renewed one the store binds"
    );
    assert_eq!(several.lone_for(Some(work)), Some(work));
    assert_eq!(
        several.lone_for(Some("/s/elsewhere")),
        None,
        "a login no row names"
    );
    assert_eq!(several.lone_for(None), None, "the default login needs none");
    accounts::note_renewed(
        &mut accounts::State::default(),
        &mut logins,
        "work",
        Some(work),
        work_renewed,
        None,
        now,
    );
    store.save_logins(&logins).expect("logins");
    let renewed = arm(true, Some(exe.clone()), Some(now)).expect("armed");
    assert_eq!(
        renewed.listed,
        vec![login.to_owned(), work_renewed.to_owned(), work.to_owned()],
        "a row a renewal moved: its new login, and the one it left (`Row::was`), which the \
         footer's `row_left` promises a restart for"
    );
    assert_eq!(renewed.lone_for(Some(work_renewed)), Some(work_renewed));
    assert_eq!(
        renewed.lone_for(Some(work)),
        Some(work),
        "a session still on the login the renewal left is tapped as that account's"
    );
    let logins_file = store.logins_file();
    let kept = std::fs::read(&logins_file).expect("logins.json");
    std::fs::write(&logins_file, "{ torn").expect("torn");
    assert_eq!(
        arm(true, Some(exe.clone()), Some(now)).map(|a| (a.lone_login, a.listed)),
        None,
        "logins.json unread: no tap, as the host's attribution and the footer's gate refuse \
         that roster too"
    );
    std::fs::write(&logins_file, kept).expect("restored");
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"solo\"\nclass = \"dedicated\"\n",
    )
    .expect("one row, no login of its own");
    assert_eq!(
        arm(true, Some(exe.clone()), Some(now)).map(|a| (a.lone_login, a.listed)),
        Some((None, Vec::new())),
        "one row with no login of its own: only the default login is the machine's"
    );
    assert_eq!(
        Armed::arm(true, || Some(exe.clone()), None, &store, Some(now)).map(|a| a.lone_login),
        Some(None),
        "no roster: no login"
    );
    assert_eq!(
        arm(false, Some(exe.clone()), Some(now)),
        None,
        "the switch off"
    );
    assert_eq!(arm(true, None, Some(now)), None, "no aterm to name");
}

/// WHY A LINE IS REFUSED THE TAP, BY ITS ARGV: options no relaunch line can repeat (a flag this
/// build does not know, one that cannot resume) are `Argv`, never the `--settings` or `--bare`
/// of its own (`Line`) the footer's menu would otherwise name; aterm's own tap is no
/// `--settings` of its own.
#[test]
fn a_lines_refusal_names_its_argv_or_its_own_settings() {
    let argv = |words: &[&str]| -> Vec<String> {
        std::iter::once("claude")
            .chain(words.iter().copied())
            .map(str::to_owned)
            .collect()
    };
    let exe = if cfg!(windows) {
        r"C:\Users\u\AppData\Local\Programs\aterm\aterm.exe"
    } else {
        "/Applications/aterm.app/Contents/MacOS/aterm"
    };
    let own = settings_value(Path::new(exe)).expect("an admissible path");
    for (words, refusal) in [
        (vec![], None),
        (vec!["--verbose"], None),
        (vec!["--settings", own.as_str()], None),
        (vec!["--settings", "/work/own.json"], Some(Refusal::Line)),
        (vec!["--bare"], Some(Refusal::Line)),
        (vec!["--a-flag-of-a-newer-claude"], Some(Refusal::Argv)),
        (vec!["--no-session-persistence"], Some(Refusal::Argv)),
        (vec!["--print"], Some(Refusal::Argv)),
    ] {
        let line = argv(&words);
        assert_eq!(line_refusal(&line), refusal, "{words:?}");
        assert_eq!(line_admits(&line), refusal.is_none(), "{words:?}");
    }
}

/// A Claude Code 2.1.310 status-line payload, composed from that build's own statusLine input
/// (`workspace`, `context_window`, `prompt_cache`, `effort`, `thinking`, one-decimal
/// `rate_limits`), with example names.
const PAYLOAD_2_1_310: &str = include_str!("testdata/tap/claude-2.1.310-statusline.json");

/// A Claude Code 2.1.316 status-line payload for a login whose replies carry the week and no
/// five-hour window, with example names: that build puts `rate_limits.five_hour` in only while the
/// last reply's headers carried both `anthropic-ratelimit-unified-5h-utilization` and `-reset`
/// (its own statusLine docs: "present only while the API reports it"), and `rate_limits` itself
/// once any window is there. The week's figures are a real session's of 2026-10-31.
const PAYLOAD_2_1_316_WEEK_ONLY: &str =
    include_str!("testdata/tap/claude-2.1.316-statusline-week-only.json");

/// The instant the 2.1.316 week-only payload was captured: five minutes before its prompt cache's
/// `expires_at` (Sat 31 Oct 2026 20:03:13 UTC), an hour into a week that resets Sat 7 Nov 19:00.
const CAPTURED_2_1_316: i64 = 1_793_477_293 - 300;

/// THE TAP READS 2.1.316'S WEEK-ONLY PAYLOAD AS IT IS: the week (1%, its reset) and no five-hour
/// window, never a guessed one; the row says the week, and the reading has figures, which is what
/// tells `harness usage` the vendor reports no five-hour window rather than that nothing was read.
/// It runs at the payload's own instant ([`CAPTURED_2_1_316`]): at the tests' [`NOW`] its reset
/// lies 23 days ahead, further than a week's can, and the row would rightly say none.
#[test]
fn a_week_only_payload_reads_the_week_and_no_five_hour_window() {
    let dir = scratch("week-only");
    let mut out = Vec::new();
    let code = run(
        &mut PAYLOAD_2_1_316_WEEK_ONLY.as_bytes(),
        false,
        &vars(),
        Some(dir.path()),
        CAPTURED_2_1_316,
        utc,
        &mut out,
    );
    let line = String::from_utf8(out).expect("utf-8");
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(line, "week 1% used \u{b7} resets Sat 7 Nov 19:00\n");
    let (reading, _) = Reading::read(dir.path(), SID, 68537, 0).expect("the reading");
    assert_eq!(reading.five_hour, None);
    assert_eq!(
        reading.seven_day,
        Some(Window {
            pct: 1,
            resets_at: 1_794_078_000,
            pct_x10: Some(10),
        })
    );
    assert!(reading.has_figures());
    assert_eq!(reading.version.as_deref(), Some("2.1.316"));
    assert_eq!(reading.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(reading.api_ms, 219_320_877);
    assert_eq!(reading.ctx_left, Some(88));
}

/// THE CARRYING TAP WRITES THE READING AND HANDS ON THE PAYLOAD BYTE FOR BYTE (R-12): on Claude
/// Code 2.1.310's payload it records the week the footer reads (12.3% used, its reset) as the
/// plain tap does, prints nothing of its own, and returns exactly what Claude Code wrote for the
/// owner's command; where the owner's file names no command now, or the payload is over the cap,
/// it prints the plain tap's row instead, so a run never shows less than the plain tap.
#[test]
fn the_carrying_tap_records_the_week_and_hands_on_the_payload() {
    let dir = scratch("chain");
    let now = 1_792_867_260;
    let mut out = Vec::new();
    let carried = run_carrying(
        &mut PAYLOAD_2_1_310.as_bytes(),
        false,
        &vars(),
        Some(dir.path()),
        now,
        utc,
        || Some("echo mine".to_string()),
        &mut out,
    );
    assert_eq!(
        carried,
        Carried::Owner {
            command: "echo mine".to_string(),
            payload: PAYLOAD_2_1_310.as_bytes().to_vec(),
        }
    );
    assert!(out.is_empty(), "the owner's command prints the row");
    let (reading, _) = Reading::read(dir.path(), SID, 68537, now - 1).expect("the reading");
    assert_eq!(
        reading.seven_day,
        Some(Window {
            pct: 12,
            resets_at: 1_793_473_200,
            pct_x10: Some(123),
        })
    );
    assert_eq!(reading.five_hour.map(|w| w.x10()), Some(375));
    assert_eq!(reading.version.as_deref(), Some("2.1.310"));
    assert_eq!(reading.api_ms, 412_000);
    let mut out = Vec::new();
    let own = run_carrying(
        &mut PAYLOAD_2_1_310.as_bytes(),
        false,
        &vars(),
        Some(dir.path()),
        now,
        utc,
        || None,
        &mut out,
    );
    assert_eq!(own, Carried::Own);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "week 12% used \u{b7} resets Sat 31 Oct 19:00\n"
    );
    let over = vec![b' '; STDIN_MAX + 1];
    let mut out = Vec::new();
    let asked = std::cell::Cell::new(false);
    assert_eq!(
        run_carrying(
            &mut &over[..],
            false,
            &vars(),
            Some(dir.path()),
            now,
            utc,
            || {
                asked.set(true);
                Some("echo mine".to_string())
            },
            &mut out,
        ),
        Carried::Own
    );
    assert_eq!(String::from_utf8_lossy(&out), format!("{NO_WEEK}\n"));
    assert!(
        !asked.get(),
        "the owner's file is not read for a payload the tap cannot hand on"
    );
}

/// THE TAP NEVER PRINTS A FIVE-HOUR FIGURE (the design's §5.2): with the five-hour window at 42%
/// beside a week of 61%, alone, or full beside a week of 3%, the plain tap and the carrying one
/// print the week's words or [`NO_WEEK`], with no `5h` and no five-hour figure, while the file
/// still records the five-hour window (for walls, the tap clock and the journal). Negative
/// controls: printing the five-hour-and-week row (`5h 42% · 7d 61%`, before this slice) fails the
/// first assertion; recording only what is printed (the five-hour window dropped) fails the last.
#[test]
fn the_tap_never_prints_a_five_hour_figure() {
    let root = scratch("no-five");
    let cases = [
        (Some((42, 1_792_062_396)), Some((61, 1_792_397_196)), "42%"),
        (Some((42, 1_792_062_396)), None, "42%"),
        (Some((100, 1_792_062_396)), Some((3, 1_792_397_196)), "100%"),
    ];
    for (five, seven, figure) in cases {
        let stdin = payload(7, five, seven);
        let (_, line) = tap(stdin.as_bytes(), &vars(), Some(root.path()));
        let mut carried = Vec::new();
        let _ = run_carrying(
            &mut stdin.as_bytes(),
            false,
            &vars(),
            Some(root.path()),
            NOW,
            utc,
            || None,
            &mut carried,
        );
        for printed in [line, String::from_utf8(carried).expect("utf-8")] {
            assert!(
                printed.starts_with("week ")
                    && !printed.contains("5h")
                    && !printed.contains(figure),
                "{printed}"
            );
        }
        let (read, _) = Reading::read(root.path(), SID, 68537, 0).expect("the reading");
        assert_eq!(read.five_hour.map(|w| w.pct), five.map(|(pct, _)| pct));
    }
}

/// Run the plain tap and the carrying one whose owner names no command on `stdin`, into `dir`, on
/// the local clock `offset`: what each printed.
fn both_taps(
    stdin: &[u8],
    tty: bool,
    vars: &TapVars,
    dir: &Path,
    offset: fn(i64) -> Option<i64>,
) -> [String; 2] {
    let mut plain = Vec::new();
    let _ = run(
        &mut &stdin[..],
        tty,
        vars,
        Some(dir),
        NOW,
        offset,
        &mut plain,
    );
    let mut carried = Vec::new();
    let _ = run_carrying(
        &mut &stdin[..],
        tty,
        vars,
        Some(dir),
        NOW,
        offset,
        || None,
        &mut carried,
    );
    [plain, carried].map(|out| String::from_utf8(out).expect("utf-8"))
}

/// THE TAP ALWAYS PRINTS EXACTLY ONE LINE, NEVER AN EMPTY ONE (a command that prints nothing loses
/// the row: aterm-phase's `claude-2.1.300-tap-row-silent-command.txt`): a tty for stdin, an empty
/// payload, `{}`, non-JSON, bytes that are not UTF-8, an oversized payload, a week's payload with
/// no tab named, a week's, and weeks whose reset no week can have (`i64::MAX`, a month on) on a
/// clock east of UTC, each through the plain tap and the carrying one whose owner names no
/// command, print one non-empty line that [`parse_line`] reads whole; the impossible resets say
/// the week with no reset. Negative controls: a print kept for a reading only (an `if let
/// Some(reading)` around the write) fails it on the payload with no tab named; a reset said
/// whenever it is ahead of now (the believability bound dropped, as the slice before this fix
/// had it) overflows the local clock's sum at `i64::MAX` (a panic, so no line) and says a month on.
#[test]
fn the_tap_always_prints_one_line() {
    let root = scratch("one-line");
    let week = payload(9, None, Some((61, 1_792_397_196)));
    let over = vec![b' '; STDIN_MAX + 1];
    let own = vars();
    let no_tab = TapVars {
        sid: None,
        ..vars()
    };
    let inputs: [(&[u8], bool, &TapVars); 8] = [
        (week.as_bytes(), true, &own),
        (b"", false, &own),
        (b"{}", false, &own),
        (b"not json", false, &own),
        (b"\xff\xfe{}", false, &own),
        (&over, false, &own),
        (week.as_bytes(), false, &no_tab),
        (week.as_bytes(), false, &own),
    ];
    for (stdin, tty, vars) in inputs {
        for out in both_taps(stdin, tty, vars, root.path(), utc) {
            let line = out.strip_suffix('\n').expect("a line");
            assert!(!line.is_empty() && !line.contains('\n'), "{out:?}");
            assert_eq!(parse_line(line), Some(line.chars().count()), "{out:?}");
        }
    }
    let tokyo: fn(i64) -> Option<i64> = |_| Some(9 * 3600);
    for resets_at in [i64::MAX, NOW + 30 * DAY] {
        let stdin = payload(9, None, Some((5, resets_at)));
        for out in both_taps(stdin.as_bytes(), false, &own, root.path(), tokyo) {
            assert_eq!(out, "week 5% used\n", "a reset at {resets_at}");
            assert_eq!(parse_line(out.trim_end()), Some(12));
        }
    }
}

/// THE TAP SAYS NO RESET WHERE THE LOCAL ZONE DOES NOT READ (`aterm_uds::zone::local_offset_at`
/// answers `None` on a platform it cannot read): a week resetting Mon 19 Oct 08:06:36 UTC prints
/// `week 61% used`, through the plain tap and the carrying one, and so does a zone that fails at
/// one instant of the span it is asked over ([`super::super::outlook::Offsets::read_around`]),
/// rather than an instant said on UTC that the glass, on the local clock, says otherwise. The
/// control: a zone that reads (UTC, and UTC−8) says the reset on its own clock. Negative control:
/// a zone that falls back to UTC (`unwrap_or(0)`, the CLI's before this fix) prints `· resets Mon
/// 08:06` and fails the first assertion.
#[test]
fn the_tap_says_no_reset_where_the_local_zone_does_not_read() {
    let root = scratch("no-zone");
    let stdin = payload(4, None, Some((61, 1_792_397_196)));
    let unread: fn(i64) -> Option<i64> = |_| None;
    let gap: fn(i64) -> Option<i64> = |t| (t != NOW + 6 * 3600).then_some(0);
    for zone in [unread, gap] {
        for out in both_taps(stdin.as_bytes(), false, &vars(), root.path(), zone) {
            assert_eq!(out, "week 61% used\n");
            assert_eq!(parse_line(out.trim_end()), Some(13));
        }
    }
    let pacific: fn(i64) -> Option<i64> = |_| Some(-8 * 3600);
    for (zone, said) in [
        (utc as fn(i64) -> Option<i64>, WEEK_61),
        (pacific, "week 61% used \u{b7} resets Mon 00:06\n"),
    ] {
        for out in both_taps(stdin.as_bytes(), false, &vars(), root.path(), zone) {
            assert_eq!(out, said);
        }
    }
}

/// A CARRIED ROW IS RECORDED AS THE OWNER'S (the design's §5.2): the carrying tap asks for the
/// owner's command before it writes, so a run that hands the payload on writes `"row":"owner"`,
/// which is not claimable, and prints nothing. The control: a run whose owner names no command,
/// and the plain tap, write `"row":"own"`, which is. Negative controls: asking after the write
/// (every run `own`, as before this slice) fails the first half; a carrying tap that always writes
/// `owner` fails the control.
#[test]
fn a_carried_row_is_recorded_as_the_owners() {
    let root = scratch("carried-row");
    let dir = root.path();
    let stdin = payload(5, None, Some((61, 1_792_397_196)));
    let file = dir.join(file_name(SID, 68537).expect("a name"));
    let carry = |owner: Option<&str>| {
        let mut out = Vec::new();
        let carried = run_carrying(
            &mut stdin.as_bytes(),
            false,
            &vars(),
            Some(dir),
            NOW,
            utc,
            || owner.map(str::to_owned),
            &mut out,
        );
        let text = std::fs::read_to_string(&file).expect("the file");
        let (reading, _) = Reading::read(dir, SID, 68537, 0).expect("the reading");
        (
            carried,
            String::from_utf8(out).expect("utf-8"),
            text,
            reading,
        )
    };
    let (carried, out, text, reading) = carry(Some("echo mine"));
    assert!(matches!(carried, Carried::Owner { .. }), "{carried:?}");
    assert_eq!(out, "");
    assert!(text.contains(r#""row":"owner""#), "{text}");
    assert_eq!(reading.row, Some(Row::Owner));
    assert!(!reading.claimable());
    let (carried, out, text, reading) = carry(None);
    assert_eq!(carried, Carried::Own);
    assert_eq!(out, WEEK_61);
    assert!(text.contains(r#""row":"own""#), "{text}");
    assert_eq!(reading.row, Some(Row::Own));
    assert!(reading.claimable());
    let _ = carry(Some("echo mine"));
    let _ = tap(stdin.as_bytes(), &vars(), Some(dir));
    let (reading, _) = Reading::read(dir, SID, 68537, 0).expect("the reading");
    assert_eq!(
        reading.row,
        Some(Row::Own),
        "the plain tap's row is its own"
    );
}

/// A NEW READING KEEPS FORMAT ONE (the design's §5.2 and §8.3): the file a run writes says `"v":1`
/// beside the new `row` key, so a GUI or a tap of another build reads it; a file without the key
/// (every build before this one) still reads, its figures intact, and is not claimable; a row word
/// this build does not know, beside a key it does not know, reads as no row. The control: the
/// same file at `"v":2` is refused, as [`Reading::of_json`] refuses every other version. Negative
/// controls: bumping [`FORMAT_V`] fails the first assertion; an `of_json` that takes any version
/// fails the control.
#[test]
fn a_new_reading_keeps_format_one() {
    let root = scratch("format-one");
    let _ = tap(
        payload(3, None, Some((61, 1_792_397_196))).as_bytes(),
        &vars(),
        Some(root.path()),
    );
    let path = root.path().join(file_name(SID, 68537).expect("a name"));
    let text = std::fs::read_to_string(&path).expect("the file");
    assert!(
        text.contains("\"v\":1,") && text.contains(r#""row":"own","#),
        "{text}"
    );
    let read = Reading::of_json(&text).expect("it reads");
    assert_eq!(read.row, Some(Row::Own));
    assert!(read.claimable());
    let older = Reading::of_json(&text.replace(r#""row":"own","#, "")).expect("an older file");
    assert_eq!(older.row, None);
    assert!(!older.claimable());
    assert_eq!(
        (older.seven_day, older.at, older.api_ms),
        (read.seven_day, read.at, read.api_ms)
    );
    let later = Reading::of_json(&text.replace(r#""row":"own","#, r#""row":"glass","later":1,"#))
        .expect("a later word and key");
    assert_eq!(later.row, None);
    assert!(!later.claimable());
    assert_eq!(
        Reading::of_json(&text.replace("\"v\":1,", "\"v\":2,")),
        None,
        "another version is refused"
    );
}

/// PARSE_LINE READS EVERY LINE THE TAP PRINTS, AND ONLY THOSE (the design's §5.2): every week
/// figure from 0 to 100% in tenths, its reset later today, one to six days on and seven days on
/// (each form of [`super::super::outlook::when`]) on three local clocks, or not ahead, or a month
/// on (no week's reset, so none said), and [`NO_WEEK`], each read back at its width in cells,
/// alone, padded with blanks, beside a notice two blanks away and beside one a single blank away
/// (manual mode's, 2026-11-07: the words end where the tap's form does, never at a gap), and never
/// run on into a word. It refuses the five-hour-and-week rows of builds before this one (as
/// aterm-phase's 2.1.300 and 2.1.312 fixtures measured them), an owner's own text, and every row
/// Claude Code cuts with `…` at `cols − 3` in a pane too narrow for the words (as the lab's
/// stand-in draws them, aterm-phase's `-30col`), at every width and each form of the words.
/// Negative controls: a parser that takes any `week ` head fails the refusals; a printer that
/// says tenths (`63.4%`) fails the round trip; S2's parser, which ended the words only at a gap,
/// fails the one-blank notice; S5b's, which ended them at any blank, read the cuts `week 63% used
/// ·…` and `week 63% used …` (19 to 27 columns) as the bare form.
#[test]
fn parse_line_reads_every_line_the_tap_prints() {
    use super::super::outlook::Offsets;
    let clocks = [
        Offsets::fixed(0),
        Offsets::fixed(-7 * 3600),
        Offsets::fixed(5 * 3600 + 1800),
    ];
    let resets = [
        NOW - 1,
        NOW,
        NOW + 600,
        NOW + 3600,
        NOW + DAY,
        NOW + 3 * DAY,
        NOW + 6 * DAY,
        NOW + 7 * DAY,
        NOW + 7 * DAY + 13 * 3600,
        NOW + 30 * DAY,
    ];
    let mut forms = std::collections::BTreeSet::new();
    for clock in &clocks {
        for reset in resets {
            for x10 in 0..=1000 {
                let line = week(x10, reset).week_words(NOW, Some(clock));
                let width = line.chars().count();
                forms.insert(
                    line.split_once(" resets ")
                        .map_or(0, |(_, when)| when.split(' ').count()),
                );
                for text in [
                    line.clone(),
                    format!("{line}   "),
                    format!("{line}  Now using usage credits"),
                    format!("{line} Now using usage credits"),
                ] {
                    assert_eq!(parse_line(&text), Some(width), "{text}");
                }
                assert_eq!(parse_line(&format!("{line}credits")), None, "{line}");
            }
        }
    }
    assert_eq!(
        forms,
        std::collections::BTreeSet::from([0, 1, 2, 4]),
        "no reset, and each form of when"
    );
    assert_eq!(parse_line(NO_WEEK), Some(NO_WEEK.chars().count()));
    assert_eq!(
        parse_line(&format!("{NO_WEEK}     Now using usage credits")),
        Some(20)
    );
    assert_eq!(
        parse_line(&format!("{NO_WEEK} Now using usage credits")),
        Some(20),
        "a notice one blank after the words"
    );
    // An owner's command that prints the tap's head reads as its words: whose row it is, the
    // reading says (`Reading::claimable`), not the text.
    assert_eq!(
        parse_line("week 63% used \u{b7} resets Sat 10:00 \u{b7} mine"),
        Some(32)
    );
    for refused in [
        "5h 42% \u{b7} 7d 61%",
        "5h 100% \u{b7} 7d 61%",
        "5h \u{2013} \u{b7} 7d 42%",
        "5h \u{2013} \u{b7} 7d \u{2013}",
        "",
        " week 61% used",
        "week 101% used",
        "week 07% used",
        "week 63.5% used",
        "week 63% left",
        "week \u{2265}63% used",
        "week 63% used \u{b7} resets soon",
        "week 63% used \u{b7} resets 24:00",
        "week 63% used \u{b7} resets 23:60",
        "week 63% used \u{b7} resets Thx 05:00",
        "week 63% used \u{b7} resets Thu 32 Nov 05:00",
        "week 63% used \u{b7} resets Thu 05 Nov 05:00",
        "week 63% used \u{b7} resets Thu 5 Now 05:00",
        "week 63% used \u{b7} resets Sat 10:000",
        "week 63% used \u{b7} resets Sat 10:00\u{b7} mine",
        "week 63% usedX",
        "week \u{b7} no figure",
        "main \u{b7} Opus 5.5 \u{b7} 42%",
        "~/aterm main* opus",
    ] {
        assert_eq!(parse_line(refused), None, "{refused}");
    }
    // Claude's cut: in a pane `cols` wide the row keeps `cols − 5` cells of the words, then `…`,
    // wherever the words do not fit `cols − 4` (the lab stand-in's `status_row`).
    let mut cuts = 0;
    for line in [
        "week 63% used \u{b7} resets Sat 10:00",
        "week 7% used \u{b7} resets 15:00",
        "week 100% used \u{b7} resets Thu 12 Nov 05:00",
        NO_WEEK,
    ] {
        let n = line.chars().count();
        for cols in 5..n + 4 {
            let kept: String = line.chars().take(cols - 5).collect();
            let cut = format!("{kept}\u{2026}");
            assert_eq!(parse_line(&cut), None, "{cols} columns: {cut}");
            cuts += 1;
        }
    }
    assert_eq!(cuts, 31 + 26 + 39 + 19, "every cut width of each form");
}

/// THE OWNER'S COMMAND RUNS AS CLAUDE CODE RUNS IT: `/bin/sh -c`, the payload on its stdin
/// whatever its size, its own stdout and exit status (the tap execs it; here it is waited on), and
/// no file left behind for the payload.
#[cfg(unix)]
#[test]
fn the_owners_command_reads_the_payload_and_keeps_its_row_and_status() {
    let leftovers = || -> Vec<String> {
        let mine = format!(".aterm-tap-stdin-{}-", std::process::id());
        std::fs::read_dir(std::env::temp_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|name| name.starts_with(&mine))
                    .collect()
            })
            .unwrap_or_default()
    };
    let echoed = chain_command("\"$ATERM_TEST_CAT\"", PAYLOAD_2_1_310.as_bytes())
        .expect("a command")
        .env("ATERM_TEST_CAT", crate::test_program::kept("/bin/cat"))
        .output()
        .expect("it ran");
    assert!(echoed.status.success());
    assert_eq!(echoed.stdout, PAYLOAD_2_1_310.as_bytes());
    let big = "x".repeat(200 * 1024);
    let counted = chain_command(
        "\"$ATERM_TEST_WC\" -c | \"$ATERM_TEST_TR\" -d ' '",
        big.as_bytes(),
    )
    .expect("a command")
    .env("ATERM_TEST_WC", crate::test_program::kept("/usr/bin/wc"))
    .env("ATERM_TEST_TR", crate::test_program::kept("/usr/bin/tr"))
    .output()
    .expect("it ran");
    assert_eq!(String::from_utf8_lossy(&counted.stdout).trim(), "204800");
    let own = chain_command("printf 'mine\\n'; exit 3", b"{}")
        .expect("a command")
        .output()
        .expect("it ran");
    assert_eq!(own.stdout, b"mine\n");
    assert_eq!(
        own.status.code(),
        Some(3),
        "its exit status is Claude Code's to read"
    );
    assert_eq!(
        leftovers(),
        Vec::<String>::new(),
        "the payload's file is unlinked"
    );
}

/// THE LAUNCH GATE ([`launch_value`], what `aterm harness tap --launch` prints for the managed
/// twin's block): an interactive Claude Code started in an aterm tab, on the machine's own stored
/// login under the account's own `HOME`, gets [`gate`]'s value from its start, the owner's own
/// statusLine carried; EACH GUARD ALONE turns it into none — the switch off, no tab or one not
/// spelled as aterm's, `--no-harness`, a `HOME` of its own (or none, or a relative one), a line
/// already carrying the tap, a subcommand word where the prompt would be, a line [`gate`]
/// refuses (its own `--settings`), a credential variable, a login or a Claude home of its own,
/// and a settings chain [`gate`] refuses.
#[cfg(unix)]
#[test]
fn the_launch_gate_taps_a_plain_launch_in_a_tab_and_each_guard_alone_refuses() {
    let armed = Armed {
        exe: Path::new(EXE).to_path_buf(),
        lone_login: None,
        listed: Vec::new(),
    };
    let words = |w: &[&str]| -> Vec<String> { w.iter().map(|s| (*s).to_string()).collect() };
    let pairs = |p: &[(&str, &str)]| -> Vec<(String, String)> {
        p.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    };
    let env = pairs(&[("HOME", "/Users//example"), ("TERM_PROGRAM", "aterm")]);
    let argv = words(&["claude", "--model", "opus"]);
    let quiet: Vec<(String, Source)> = vec![
        ("managed".to_string(), Source::Missing),
        ("user".to_string(), Source::Text("{}".to_string())),
    ];
    fn facts<'a>(
        armed: &'a Armed,
        env: &'a [(String, String)],
        settings: &'a [(String, Source)],
        argv: &'a [String],
    ) -> LaunchFacts<'a> {
        let home = Path::new("/Users//example");
        LaunchFacts {
            armed: Some(armed),
            sid: Some(SID),
            no_harness: false,
            env,
            home: Some(home),
            account_home: Some(home),
            argv,
            settings,
            user_settings: Some("user"),
            profile_store: false,
        }
    }
    let facts = |argv| facts(&armed, &env, &quiet, argv);
    let want = settings_value(Path::new(EXE));
    assert!(want.is_some());
    assert_eq!(launch_value(&facts(&argv)), want);
    for typed in [
        &["claude"][..],
        &["claude", "fix the failing test"][..],
        &["claude", "--resume", "1b0c9a64-5bd3-4d8e-9a3c-0e1f2a3b4c5d"][..],
        &["claude", "-c"][..],
        &[
            "claude",
            "--dangerously-skip-permissions",
            "--model",
            "opus",
        ][..],
        &["claude", "Hello"][..],
    ] {
        let typed_argv = words(typed);
        assert_eq!(
            launch_value(&LaunchFacts {
                argv: &typed_argv,
                ..facts(&argv)
            }),
            want,
            "{typed:?}"
        );
    }
    let owner = vec![
        ("managed".to_string(), Source::Missing),
        (
            "user".to_string(),
            Source::Text(OWNER_SETTINGS_2_1_310.to_string()),
        ),
    ];
    let carried = launch_value(&LaunchFacts {
        settings: &owner,
        ..facts(&argv)
    })
    .expect("the owner's own statusLine is carried at launch too");
    assert_eq!(carrying_of(&carried), Some(true));
    assert_eq!(Some(carried), settings_value_carrying(Path::new(EXE), true));
    let refused = |f: LaunchFacts<'_>, why: &str| assert_eq!(launch_value(&f), None, "{why}");
    refused(
        LaunchFacts {
            armed: None,
            ..facts(&argv)
        },
        "the switch off",
    );
    refused(
        LaunchFacts {
            sid: None,
            ..facts(&argv)
        },
        "no tab",
    );
    for sid in ["", "s-", "60776b6e", "s-xyz", "s-../x"] {
        refused(
            LaunchFacts {
                sid: Some(sid),
                ..facts(&argv)
            },
            &format!("tab {sid:?}"),
        );
    }
    refused(
        LaunchFacts {
            no_harness: true,
            ..facts(&argv)
        },
        "--no-harness",
    );
    refused(
        LaunchFacts {
            home: Some(Path::new("/Users//example/sandbox")),
            ..facts(&argv)
        },
        "a HOME of its own",
    );
    refused(
        LaunchFacts {
            home: None,
            ..facts(&argv)
        },
        "no HOME",
    );
    refused(
        LaunchFacts {
            home: Some(Path::new("example")),
            account_home: Some(Path::new("example")),
            ..facts(&argv)
        },
        "a relative HOME",
    );
    refused(
        LaunchFacts {
            account_home: None,
            ..facts(&argv)
        },
        "the account's home unread",
    );
    let tapped: Vec<String> = argv
        .iter()
        .cloned()
        .chain(["--settings".to_string(), want.clone().unwrap()])
        .collect();
    refused(facts(&tapped), "already carrying the tap");
    for typed in [
        &["claude", "mcp", "list"][..],
        &["claude", "doctor"][..],
        &["claude", "update"][..],
        &["claude", "setup-token"][..],
        &["claude", "--model", "opus", "plugin"][..],
        &["claude", "hello"][..],
        &["claude", "--settings", "/Users//example/team.json"][..],
        &["claude", "--bare"][..],
        &["claude", "--no-such-flag"][..],
    ] {
        let typed_argv = words(typed);
        refused(
            LaunchFacts {
                argv: &typed_argv,
                ..facts(&argv)
            },
            &format!("{typed:?}"),
        );
    }
    for (key, value) in [
        ("ANTHROPIC_API_KEY", "k"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "t"),
        ("CLAUDE_CONFIG_DIR", "/Users//example/.claude-b"),
        (
            "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            "/Users//example/.claude-logins/b",
        ),
    ] {
        let env = pairs(&[("HOME", "/Users//example"), (key, value)]);
        refused(
            LaunchFacts {
                env: &env,
                ..facts(&argv)
            },
            key,
        );
    }
    let statusline = vec![
        ("managed".to_string(), Source::Missing),
        ("user".to_string(), Source::Text("{}".to_string())),
        (
            "project".to_string(),
            Source::Text(r#"{"statusLine":{"type":"command","command":"x"}}"#.to_string()),
        ),
    ];
    refused(
        LaunchFacts {
            settings: &statusline,
            ..facts(&argv)
        },
        "a project's statusLine",
    );
    refused(
        LaunchFacts {
            profile_store: true,
            ..facts(&argv)
        },
        "a profile store config",
    );
}
