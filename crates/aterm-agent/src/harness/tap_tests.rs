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

/// Run the verb on `stdin`, into `dir`: its exit and the one line it printed.
fn tap(stdin: &[u8], vars: &TapVars, dir: Option<&Path>) -> (std::process::ExitCode, String) {
    let mut out = Vec::new();
    let code = run(&mut &stdin[..], false, vars, dir, 1_792_051_660, &mut out);
    (code, String::from_utf8(out).expect("utf-8"))
}

/// THE VALUE IS aterm's TAP BYTE FOR BYTE, and only that value is ours: the
/// round trip holds for an admissible path, and every near miss — a key more,
/// another command, an argument after `tap`, a relative path, a path with a
/// space or a quote — is another's, carried verbatim. NEGATIVE CONTROL: the
/// same value with its refresh changed is not ours.
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
    // Carried in either spelling, and never past `--`.
    let argv = |rest: &[&str]| -> Vec<String> {
        std::iter::once("claude")
            .chain(rest.iter().copied())
            .map(str::to_owned)
            .collect()
    };
    assert!(carried_by(&argv(&["--settings", &value])));
    assert!(carried_by(&argv(&[&format!("--settings={value}")])));
    assert!(!carried_by(&argv(&["--", "--settings", &value])));
    assert!(!carried_by(&argv(&["--settings", r#"{"model":"x"}"#])));
    assert!(!carried_by(&argv(&[])));
}

/// ON WINDOWS THE VALUE NAMES A DRIVE PATH WITH `/` SEPARATORS. Claude Code runs a status-line
/// command through Git Bash (else PowerShell), where a `\` is an escape: the path the window's
/// canonical `current_exe` gives (`\\?\C:\…`) is spelled `C:/…`, which Git Bash, pwsh 7 and
/// Windows PowerShell 5.1 each ran (measured 2026-10-17), and the value round-trips as ours.
/// Every path refused before still is (the census found every Windows path refused: a drive
/// colon and backslashes), and so are a UNC path, a drive-relative one and a value spelled with
/// backslashes.
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
/// console `aterm.exe` beside it is named, and no tap where there is none. Elsewhere, and for
/// the console image itself, the running image.
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

/// THE VERB, ON EVERY STDIN: a v1 file of mode 0600 in a 0700 directory,
/// and one non-empty line, for a payload with figures; the dash line for
/// one with none, for text that is not JSON, for an empty stdin and for one
/// over the bound (drained); nothing written but the line where no tab or
/// process is named or the directory cannot be made — and exit 0 every
/// time. NEGATIVE CONTROL: the figures the line shows are the payload's.
#[test]
fn the_verb_writes_its_reading_and_always_prints_one_line_and_exits_zero() {
    let root = scratch("verb");
    let dir = root.path().to_path_buf();
    let rl = dir.join("rl");
    let five = Some((42, 1_792_062_396));
    let seven = Some((61, 1_792_397_196));
    let (code, line) = tap(payload(18, five, seven).as_bytes(), &vars(), Some(&rl));
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(line, "5h 42% \u{b7} 7d 61%\n");
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
    // No temp file is left beside it.
    let names: Vec<_> = std::fs::read_dir(&rl)
        .expect("ls")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");

    // An occupied candidate is borrowed, so a failed write leaves it and
    // the last published reading alone. The verb still prints its line.
    let candidate = rl.join(format!(".{SID}-68537.json.tmp{}", std::process::id()));
    std::fs::write(&candidate, "borrowed sentinel").expect("candidate");
    let (_, collision_line) = tap(payload(19, five, seven).as_bytes(), &vars(), Some(&rl));
    assert_eq!(collision_line, "5h 42% \u{b7} 7d 61%\n");
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
        assert_eq!(collision_line, "5h 42% \u{b7} 7d 61%\n");
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
        let (code, line) = tap(&stdin, &vars(), Some(&rl));
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert_eq!(line, format!("{NO_FIGURES}\n"));
        assert!(!line.trim().is_empty());
    }
    // A window the vendor left out is a dash on its side alone.
    let (_, line) = tap(payload(0, five, None).as_bytes(), &vars(), Some(&rl));
    assert_eq!(line, "5h 42% \u{b7} 7d \u{2013}\n");
    // No tab, no process: the line, nothing written.
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
        assert_eq!(line, format!("{NO_FIGURES}\n"));
        assert!(!nowhere.exists(), "{vars:?}");
    }
    // A directory that cannot be made: the line all the same.
    let blocked = dir.join("blocked");
    std::fs::write(&blocked, "a file where the directory would go").expect("write");
    let (code, line) = tap(
        payload(1, five, seven).as_bytes(),
        &vars(),
        Some(&blocked.join("rl")),
    );
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(line, "5h 42% \u{b7} 7d 61%\n");
    // No state root: the line all the same.
    let (code, line) = tap(payload(1, five, seven).as_bytes(), &vars(), None);
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    assert_eq!(line, "5h 42% \u{b7} 7d 61%\n");
}

/// NOTHING OF STDIN IS WRITTEN AS IT CAME, and no variable but the four is
/// read: a payload carrying a token-shaped value in a field the tap does not
/// keep, and a process whose environment carries two credentials, leave
/// neither in the file nor on the line. Run as the real front door
/// (`aterm harness tap`, [`super::super::cli::main_entry`]) in a child of
/// this test binary, so the environment is a real one. NEGATIVE CONTROL:
/// the child did write its reading (the figures are there). The seam names
/// the state root the scratch `HOME` resolves to, so a build that compiles no
/// seam (an optimized test build) keeps its reading where a development build
/// does.
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
    assert!(stdout.contains("5h 7% \u{b7} 7d 9%"), "{stdout}");
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
/// reset however late the host reads it. Session B's 100%, first reported
/// before session A's refill at `E`, re-reported after it and read then:
/// the account keeps A's 3%, and B's next served reply (2%, dropped from
/// that 100%) opens no second epoch. NEGATIVE CONTROLS: a request served
/// since (`api_ms` rose) or a figure that moved is a new report, stamped by
/// its own run; and the same late reading stamped by its late write — what
/// the tap wrote before — is taken as served after the reset, so 100%
/// stands again and B's next reply writes a second `reset` row.
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
    // The account: A's 100% (served at 990), A's refill to 3% at E = 1100,
    // then B's late reading, then B's next reply.
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
    // NEGATIVE CONTROL: stamped by its late write, it stands again.
    let rewritten = TapRead {
        at: 1366,
        ..late.tap_read(beside, None)
    };
    assert_eq!(account(rewritten), (1000, 2));
    // NEGATIVE CONTROLS: served since; a figure that moved.
    assert_eq!(report(31, 100, 1376).at, 1376, "served since");
    assert_eq!(report(31, 99, 1386).at, 1386, "a figure moved");
    // Both windows compare their served values, including a tenth that
    // moves without changing the whole-percent row.
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

/// A reading is read only from its own process's regular file: tab and pid in name and text,
/// written since the process started. Negative controls: another tab's, another pid's, an older
/// one, a symlink and another format version are each refused.
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
    // The same text under another pid's name names the wrong process.
    std::fs::copy(dir.join(&name), dir.join(format!("{SID}-1.json"))).expect("copy");
    assert!(Reading::read(&dir, SID, 1, 0).is_none());
    // A symlink is never followed.
    #[cfg(unix)]
    {
        let link_dir = dir.join("links");
        std::fs::create_dir_all(&link_dir).expect("mkdir");
        std::os::unix::fs::symlink(dir.join(&name), link_dir.join(&name)).expect("symlink");
        assert!(Reading::read(&link_dir, SID, 68537, 0).is_none());
    }
    // Another format version is not read.
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
        dur_us: 0,
    }
}

/// THE TENTHS AND THE MODEL (the account-stats design §4.2, F1): Claude Code
/// hands `used_percentage` with one decimal (`42.6` in the lab, 2.1.301):
/// the row prints the whole percent it rounds to, the file keeps the tenths
/// and the process's `model.id` beside it, and both read back. A file an
/// older tap wrote — no `pct_x10`, no `model` — still reads, with neither
/// (`FORMAT_V` stays 1). A `model.id` that is no model id is not kept.
/// NEGATIVE CONTROL: a whole-percent payload has whole tenths, and the
/// figure in tenths of an old file is its whole percent times ten.
#[test]
fn a_reading_keeps_the_vendors_tenths_and_its_model() {
    let text = r#"{"model":{"id":"claude-opus-5-5"},"cost":{"total_api_duration_ms":9},
"rate_limits":{"five_hour":{"used_percentage":42.6,"resets_at":1792062396},
"seven_day":{"used_percentage":61,"resets_at":1792397196}}}"#;
    let r = Reading::of_stdin(text, &vars(), 1_792_050_000).expect("a reading");
    let five = r.five_hour.expect("the five-hour window");
    assert_eq!((five.pct, five.pct_x10, five.x10()), (43, Some(426), 426));
    assert_eq!(r.line(), "5h 43% \u{b7} 7d 61%");
    assert_eq!(
        r.seven_day.and_then(|w| w.pct_x10),
        Some(610),
        "a whole percent"
    );
    assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
    let back = Reading::of_json(&r.to_json()).expect("reads back");
    assert_eq!(back, r);
    // An older tap's file: no tenths, no model.
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

/// WHAT A READING SAYS BESIDE THE LAST ADMITTED ([`Fresh::beside`]): the
/// same process read before is `seen`; a window is REFILLED, from the
/// figure that process read before, where it was served since (`api_ms`
/// rose) and reads lower by a whole point or more under the same reset —
/// the drop a limit reset used inside the window makes. NEGATIVE CONTROLS: the same drop with no API time since
/// (an idle repeat), another process, another reset, and a tenth's wobble
/// are no refill; and another process is not `seen`.
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
    // `tap_read` carries what was said, in tenths, at the file's time.
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
    // tap-refresh2: (api_ms, 5h, 7d) as the tap saw them.
    let refresh2 = [(0, None, None), (0, Some(75), Some(43))]
        .into_iter()
        .chain(std::iter::repeat_n((0, Some(75), Some(43)), 10));
    let mut fresh = Fresh::default();
    let admitted: Vec<bool> = refresh2
        .map(|(api, five, seven)| fresh.admit(&reading(9, api, five, seven)))
        .collect();
    assert_eq!(admitted.iter().filter(|a| **a).count(), 1, "{admitted:?}");
    assert!(admitted[1], "the first figures");
    // tap-bgagent-refresh1: 58 → 64 → 66 → 68 → 70 → 74 on the timer.
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
    // The same figures again with no request served: stale; a request
    // served (api_ms rose) with the same figures: fresh.
    assert!(!fresh.admit(&reading(11, 96, Some(74), Some(60))));
    assert!(fresh.admit(&reading(11, 97, Some(74), Some(60))));
    // A relaunch: its first figures are fresh even where they repeat.
    assert!(fresh.admit(&reading(12, 0, Some(74), Some(60))));
    assert!(!fresh.admit(&reading(12, 0, Some(74), Some(60))));
    assert!(!Fresh::default().admit(&reading(13, 50, None, None)));
}

/// A tap is marked silent, once, only for a process up at least 30 s that served a turn, whose
/// file is missing or older than 30 s at two points 30 s apart; heard again, it is unmarked.
/// Negative controls: no turn served, up for less, a fresh file, strikes too close together.
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
    // Negative controls.
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
    // A new process starts over.
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
/// the tab's agent carries no tap; the new process needs two strikes of its own. Negative
/// controls: the marked process stays marked and quiet; an unmarked process replaced says nothing.
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
    // Negative controls.
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
        stored_login: true,
        login: None,
        lone_login: None,
        home: None,
        argv,
        settings,
        profile_store: false,
    }
}

/// THE GATE TABLE: the all-clear row gets the tap, and EACH GUARD ALONE
/// turns it into none — the switch off, a path that cannot be named, a
/// process not on its stored login, a login or a home of its own, a profile
/// store, a person's `--settings` (either spelling) or `--bare`, an argv the
/// rewrite refuses, a `statusLine`, `disableAllHooks`, an `apiKeyHelper` or
/// a credential, profile or route in any document of the chain, a document
/// that cannot be read or parsed. NEGATIVE CONTROLS: aterm's own tap on the
/// argv is shed, not refused; a set-and-empty login is the default one; the
/// login a renewal bound the machine's one account to is the machine's own
/// too ([`Armed::lone_login`]) — but not another login beside it, nor it in
/// a home of its own; a quiet document and one with a harmless `env` key let
/// it through.
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
    // Set and empty is the default login named.
    assert_eq!(
        gate(&GateFacts {
            login: Some(""),
            ..all_clear(&argv, &quiet)
        }),
        want
    );
    // The one account's renewed login is the machine's own.
    let renewed = "/Users//example/.aterm/logins/home-a.1";
    assert_eq!(
        gate(&GateFacts {
            login: Some(renewed),
            lone_login: Some(renewed),
            ..all_clear(&argv, &quiet)
        }),
        want
    );
    // aterm's own tap on the argv is shed: the next line gets it again.
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
            stored_login: false,
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
    // Each place in the chain counts: a status line in any one document.
    for at in 0..3 {
        let mut c = quiet.clone();
        c[at].1 = Source::Text(r#"{"statusLine":{"type":"command","command":"x"}}"#.into());
        refused(
            all_clear(&argv, &c),
            &format!("a status line in document {at}"),
        );
        let mut c = quiet.clone();
        c[at].1 = Source::Unreadable;
        refused(all_clear(&argv, &c), &format!("document {at} unreadable"));
    }
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

/// THE TAP'S ROW IS SAFE FOR EVERY READER (the real Claude Code 2.1.300's
/// screens, inline at 110 columns and at 80 beside `Now using usage
/// credits`): with every line the tap prints in its row, the phase reads
/// idle with no wall, the footer's mode row and the live REPL's row are
/// found under it, and the line holds no guarded string of the vendor's.
/// NEGATIVE CONTROLS: Claude Code's own usage-limit banner in that row
/// reads as a wall (the readers do read the row), and a blank row there hides the mode row from the
/// footer — which a tap that prints nothing would cost (on 2.1.300 a silent
/// or failing command takes no row at all: the `tap-row-silent-command`
/// capture).
#[test]
fn the_taps_row_reads_idle_and_keeps_the_mode_row_for_every_reader() {
    use super::super::footer;
    let lines = [
        line_of(
            Some(Window {
                pct: 42,
                resets_at: 1,
                pct_x10: None,
            }),
            Some(Window {
                pct: 61,
                resets_at: 2,
                pct_x10: None,
            }),
        ),
        line_of(
            Some(Window {
                pct: 100,
                resets_at: 1,
                pct_x10: None,
            }),
            Some(Window {
                pct: 100,
                resets_at: 2,
                pct_x10: None,
            }),
        ),
        line_of(
            Some(Window {
                pct: 0,
                resets_at: 1,
                pct_x10: None,
            }),
            None,
        ),
        NO_FIGURES.to_string(),
    ];
    for rows in tap_screens() {
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        assert!(
            rows[bottom + 1].starts_with("  5h "),
            "{:?}",
            rows[bottom + 1]
        );
        for line in &lines {
            assert!(line.chars().count() <= 17, "{line}");
            let at = rows[bottom + 1].find("5h ").expect("the tap");
            let mut row = rows[bottom + 1].clone();
            let end = row[at..].find("  ").map_or(row.len(), |e| at + e);
            row.replace_range(at..end, line);
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
        // Negative controls. The vendor's banner there (its `⚠` first, as
        // 2.1.304 draws it above a status line) is read; a status line's
        // own words naming a limit are not (KI-1015-14: the footer reads
        // only where Claude Code draws its notices).
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
    // A command that prints nothing, or fails, takes no row on 2.1.300: the
    // mode row is the one under the rule.
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
