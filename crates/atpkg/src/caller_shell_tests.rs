// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn the_parent_shell_wins_and_the_login_shell_is_the_fallback() {
    assert_eq!(
        invoking_shell_from(Some("fish"), Some("zsh")).as_deref(),
        Some("fish")
    );
    assert_eq!(
        invoking_shell_from(Some("bash"), Some("zsh")).as_deref(),
        Some("bash")
    );
    assert_eq!(
        invoking_shell_from(Some("sudo"), Some("zsh")).as_deref(),
        Some("zsh")
    );
    assert_eq!(invoking_shell_from(Some("python3"), None).as_deref(), None);
    assert_eq!(
        invoking_shell_from(None, Some("fish")).as_deref(),
        Some("fish")
    );
    assert_eq!(
        invoking_shell_from(Some("-zsh"), Some("bash")).as_deref(),
        Some("zsh")
    );
    assert_eq!(
        invoking_shell_from(Some("pwsh.exe"), None).as_deref(),
        Some("pwsh")
    );
    assert_eq!(
        invoking_shell_from(Some("make"), Some("nu")).as_deref(),
        Some("nu")
    );
    assert_eq!(
        invoking_shell_from(Some("make"), Some("weird")).as_deref(),
        Some("weird")
    );
    assert_eq!(invoking_shell_from(None, None), None);
}

/// The pid reader names this test binary and a spawned `sleep` by executable, and
/// answers `None` for a pid nobody has.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_pid_reader_names_a_process_by_its_executable() {
    let me = process_exe_name(std::process::id()).expect("our own name");
    let exe = std::env::current_exe().unwrap();
    let own = exe.file_name().unwrap().to_string_lossy();
    assert!(
        own.len() > 15,
        "the truncation this asserts away needs a name past 15 bytes, not {own:?}"
    );
    assert_eq!(me, own, "{me}");
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("/bin/sleep");
    aterm_uds::deadline::execd(child.id() as i32, std::path::Path::new("/bin/sleep"));
    let name = process_exe_name(child.id());
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(name.as_deref(), Some("sleep"));
    assert_eq!(process_exe_name(u32::MAX), None);
}

/// End to end through a real shell: `sh`/`zsh`/`bash`/`fish -c` each spawn this test
/// binary to print what [`invoking_shell`] answers, with `$SHELL` pointed elsewhere; the
/// parent shell is what comes back.
#[cfg(unix)]
#[test]
fn a_real_parent_shell_is_named_over_the_login_shell() {
    if probe_prints_the_invoking_shell_if_asked() {
        return;
    }
    let scratch = aterm_tempfile::tempdir().expect("a scratch home");
    let exe = std::env::current_exe().unwrap();
    let mut lanes = vec![(std::path::PathBuf::from("/bin/sh"), "sh|bash|dash|zsh")];
    let mut absent = Vec::new();
    for shell in ["zsh", "bash", "fish"] {
        match aterm_spec::host::test_shell(shell) {
            Some(path) => lanes.push((path, shell)),
            None => absent.push(shell),
        }
    }
    for (shell, expect) in &lanes {
        let shell = shell.display();
        let script = format!(
            "'{}' --exact --nocapture --quiet {}; true",
            exe.display(),
            "caller_shell::tests::a_real_parent_shell_is_named_over_the_login_shell"
        );
        let out = aterm_spec::host::shell_command(shell.to_string(), scratch.path())
            .arg("-c")
            .arg(&script)
            .env(PROBE_ENV, "1")
            .env("SHELL", "/usr/bin/nowhere/elvish")
            .output()
            .expect("spawn the shell");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find_map(|l| l.strip_prefix("invoking_shell="))
            .unwrap_or_else(|| panic!("{shell}: no probe line in {stdout:?}"));
        assert!(
            expect.split('|').any(|e| e == line),
            "{shell}: expected one of {expect}, got {line}: {stdout}"
        );
    }
    if !absent.contains(&"fish") {
        assert!(
            scratch.path().join(".config/fish").is_dir(),
            "fish wrote its first-run state somewhere other than the scratch home"
        );
    }
    aterm_spec::host::could_not_run_without(&absent);
}

/// The probe half of the test above: the same test binary, run as the child.
#[cfg(unix)]
const PROBE_ENV: &str = "ATPKG_CALLER_SHELL_PROBE";

#[cfg(unix)]
fn probe_prints_the_invoking_shell_if_asked() -> bool {
    if std::env::var_os(PROBE_ENV).is_none() {
        return false;
    }
    println!(
        "invoking_shell={}",
        invoking_shell().unwrap_or_else(|| "-".into())
    );
    true
}

#[test]
fn procargs2_parses_argv_and_env_and_refuses_a_short_buffer() {
    let mut buf = 3i32.to_ne_bytes().to_vec();
    buf.extend_from_slice(
        b"/x/claude\0\0\0\0claude\0--add-dir\0/a b\0HOME=/h\0ATERM_PARENT_SESSION_ID=s-1\0\0junk",
    );
    let got = parse_procargs2(&buf).expect("a whole buffer");
    assert_eq!(got.exec_path, "/x/claude");
    assert_eq!(got.argv, vec!["claude", "--add-dir", "/a b"]);
    assert_eq!(got.env, vec!["HOME=/h", "ATERM_PARENT_SESSION_ID=s-1"]);
    assert_eq!(got.env_var("ATERM_PARENT_SESSION_ID"), Some("s-1"));
    assert_eq!(got.env_var("ATERM_PARENT"), None, "a prefix is not a key");
    let mut short = 5i32.to_ne_bytes().to_vec();
    short.extend_from_slice(b"/x\0a\0b\0");
    assert_eq!(parse_procargs2(&short), None, "fewer argv than argc");
    assert_eq!(parse_procargs2(&[1, 0]), None);
}

#[test]
fn procargs2_admits_an_argc_past_65535_and_bounds_it_by_the_buffer() {
    let n = 70_000_usize;
    let mut buf = i32::try_from(n).unwrap().to_ne_bytes().to_vec();
    buf.extend_from_slice(b"/bin/zsh\0\0\0\0");
    for _ in 0..n {
        buf.extend_from_slice(b"x\0");
    }
    buf.extend_from_slice(b"K=v\0\0");
    let got = parse_procargs2(&buf).expect("70 000 argv is a whole buffer");
    assert_eq!(got.exec_path, "/bin/zsh");
    assert_eq!(got.argv.len(), n);
    assert_eq!(got.env_var("K"), Some("v"));
    assert_eq!(exe_name_of_procargs2(&buf).as_deref(), Some("zsh"));
    let mut lie = i32::MAX.to_ne_bytes().to_vec();
    lie.extend_from_slice(b"/x\0\0a\0");
    assert_eq!(
        parse_procargs2(&lie),
        None,
        "more argv than the buffer has bytes"
    );
}

#[test]
fn the_exe_name_reads_the_exec_path_alone() {
    let mut short = 5i32.to_ne_bytes().to_vec();
    short.extend_from_slice(b"/opt/homebrew/bin/fish\0\0a\0b\0");
    assert_eq!(parse_procargs2(&short), None, "fewer argv than argc");
    assert_eq!(exe_name_of_procargs2(&short).as_deref(), Some("fish"));
}

#[test]
fn a_nul_list_keeps_an_empty_word_and_drops_only_the_terminator() {
    let cmdline = b"claude\0--append-system-prompt\0\0--dangerously-skip-permissions\0";
    assert_eq!(
        split_nul_list(cmdline),
        vec![
            "claude",
            "--append-system-prompt",
            "",
            "--dangerously-skip-permissions"
        ]
    );
    assert_eq!(
        split_nul_list(b"claude\0--model\0\0"),
        vec!["claude", "--model", ""],
        "an empty LAST word, then the terminator"
    );
    assert_eq!(
        split_nul_list(b"a\0b"),
        vec!["a", "b"],
        "a rewritten title, no final NUL"
    );
    assert_eq!(split_nul_list(b"\0"), vec![""], "one word, and it is empty");
    assert_eq!(split_nul_list(b""), Vec::<String>::new(), "no words at all");
    let mut buf = 4i32.to_ne_bytes().to_vec();
    buf.extend_from_slice(b"/x/claude\0\0");
    buf.extend_from_slice(cmdline);
    buf.extend_from_slice(b"K=v\0\0");
    assert_eq!(
        parse_procargs2(&buf).expect("whole").argv,
        split_nul_list(cmdline)
    );
}

/// A live process started with an empty argument reads it back in place (`/proc` on
/// Linux, `KERN_PROCARGS2` on macOS), and on Linux its environment with it, though procfs
/// `stat`s `environ` as empty. (`KERN_PROCARGS2` gave no environment for this `/bin/sh`
/// on macOS 27.0.1.)
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn process_args_keeps_an_empty_argument_of_a_live_process() {
    let tail = [
        "--append-system-prompt",
        "",
        "--dangerously-skip-permissions",
    ];
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read -r _", "sh"])
        .args(tail)
        .env("ATPKG_PROBE_VAR", "a=b c")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("/bin/sh");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let read = loop {
        let read = process_args(child.id());
        if read
            .as_ref()
            .and_then(|a| a.argv.last())
            .is_some_and(|w| w == tail[2])
            || std::time::Instant::now() >= deadline
        {
            break read;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    let read = read.unwrap_or_default();
    let argv = &read.argv;
    assert!(
        argv.last().is_some_and(|w| w == tail[2]),
        "the shell never read back as exec'd within a minute ({argv:?}) — the spawn failed, \
         which is not what this case is about"
    );
    assert!(
        argv.ends_with(&tail.map(String::from)),
        "the empty argument must read back in place: {argv:?}"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        read.env_var("ATPKG_PROBE_VAR"),
        Some("a=b c"),
        "its environment reads back with it"
    );
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[test]
fn a_windows_command_line_splits_as_the_c_runtime_splits_it() {
    let split = |line: &str| split_command_line(&wide(line), &wide(r"C:\x\prog.exe"));
    for (line, want) in [
        (r#"prog "abc" d e"#, &["prog", "abc", "d", "e"][..]),
        (r#"prog a\\b d"e f"g h"#, &["prog", r"a\\b", "de fg", "h"]),
        (r#"prog a\\\"b c d"#, &["prog", r#"a\"b"#, "c", "d"]),
        (r#"prog a\\\\"b c" d e"#, &["prog", r"a\\b c", "d", "e"]),
        (r#"prog a"b"" c d"#, &["prog", r#"ab" c d"#]),
        (r#"claude --x"y"" z"#, &["claude", r#"--xy" z"#]),
        (r#"claude "--q r"""s t"#, &["claude", r#"--q r"s"#, "t"]),
        (r#"claude --w\\"v u""#, &["claude", r"--w\v u"]),
        (
            r#""C:\Program Files\x\claude.exe" --y"#,
            &[r"C:\Program Files\x\claude.exe", "--y"],
        ),
        (r#"C:\a\"b c"\d.exe e"#, &[r"C:\a\b c\d.exe", "e"]),
        (r#""a"b c"#, &["ab", "c"]),
        ("\tprog", &["", "prog"]),
        (r#"prog "" x """#, &["prog", "", "x", ""]),
        (r#"prog "a b"#, &["prog", "a b"]),
        (r#"prog ""#, &["prog", ""]),
        ("prog\t a  \tb ", &["prog", "a", "b"]),
        (r"prog \\srv\share\ x", &["prog", r"\\srv\share\", "x"]),
        (r#"prog "C:\x\\" y"#, &["prog", r"C:\x\", "y"]),
        ("prog a\0b c", &["prog", "a"]),
    ] {
        assert_eq!(split(line), want, "{line:?}");
    }
    assert_eq!(
        split_command_line(&[], &wide(r"C:\x\prog.exe")),
        [r"C:\x\prog.exe"]
    );
    assert_eq!(
        split_command_line(&wide("\0junk"), &wide(r"C:\Program Files\p.exe")),
        [r"C:\Program", r"Files\p.exe"],
        "the C runtime splits its own path at a space"
    );
}

#[test]
fn a_windows_environment_block_reads_whole_or_not_at_all() {
    let block = wide("=C:=C:\\work\0=D:=D:\\\0Path=C:\\bin;C:\\x\0K=v=w\0\0junk\0");
    let env = split_environment(&block).expect("a whole block");
    assert_eq!(
        env,
        ["=C:=C:\\work", "=D:=D:\\", "Path=C:\\bin;C:\\x", "K=v=w"]
    );
    let args = ProcArgs {
        env,
        ..ProcArgs::default()
    };
    assert_eq!(args.env_var("=C:"), Some("C:\\work"));
    assert_eq!(args.env_var("K"), Some("v=w"));
    assert_eq!(args.env_var("C:"), None, "the name is `=C:`");
    assert_eq!(split_environment(&wide("A=1\0B=2\0")), None, "cut short");
    assert_eq!(split_environment(&wide("A=1\0B=2")), None, "cut mid-string");
    assert_eq!(split_environment(&[]), None);
    assert_eq!(split_environment(&wide("\0")), Some(Vec::new()));
}

#[test]
fn a_variable_is_found_by_its_whole_name_and_on_windows_blind_to_case() {
    let upper = |c: char| {
        let mut up = c.to_uppercase();
        match (up.next(), up.next()) {
            (Some(u), None) => u,
            _ => c,
        }
    };
    let exact: fn(char) -> char = std::convert::identity;
    assert_eq!(env_value("Path=C:\\bin", "PATH", upper), Some("C:\\bin"));
    assert_eq!(env_value("Path=C:\\bin", "PATH", exact), None);
    assert_eq!(env_value("Path=C:\\bin", "Path", exact), Some("C:\\bin"));
    assert_eq!(env_value("PATHEXT=.COM", "PATH", upper), None, "longer");
    assert_eq!(env_value("PAT=x", "PATH", upper), None, "shorter");
    assert_eq!(env_value("PATH", "PATH", upper), None, "no `=`");
    assert_eq!(env_value("ıd=x", "ID", upper), Some("x"));
    assert_eq!(env_value("=C:=C:\\x", "=c:", upper), Some("C:\\x"));
    let args = ProcArgs {
        env: vec!["Path=C:\\bin".into(), "PATH=second".into()],
        ..ProcArgs::default()
    };
    assert_eq!(args.env_var("Path"), Some("C:\\bin"));
    assert_eq!(
        args.env_var("PATH"),
        Some(if cfg!(windows) { "C:\\bin" } else { "second" })
    );
}

/// The probe half of [`process_args_reads_a_live_process_as_its_own_runtime_split_it`]:
/// this test binary, run as the child.
#[cfg(windows)]
const ARGS_PROBE_ENV: &str = "ATPKG_PROCESS_ARGS_PROBE";

/// `s` as the hex of its UTF-8, one line's worth whatever it holds.
#[cfg(windows)]
fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// [`hex`]'s inverse.
#[cfg(windows)]
fn unhex(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect();
    String::from_utf8(bytes).expect("UTF-8")
}

/// Run as the child (with [`ARGS_PROBE_ENV`] set): print the argv this process's own
/// runtime split from its command line (Rust's std: the C runtime's post-2008 rules) and
/// its environment as std reads it (`GetEnvironmentStringsW`), then wait for stdin to
/// close, so the parent reads it alive.
#[cfg(windows)]
#[test]
fn probe_prints_its_own_argv_and_environment_when_asked() {
    use std::io::Read as _;
    if std::env::var_os(ARGS_PROBE_ENV).is_none() {
        return;
    }
    for arg in std::env::args() {
        println!("arg={}", hex(&arg));
    }
    for (k, v) in std::env::vars() {
        println!("env={}", hex(&format!("{k}={v}")));
    }
    println!("probe=ready");
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

/// A LIVE PROCESS READS BACK AS ITS OWN RUNTIME SPLIT IT, ITS ENVIRONMENT WHOLE: this test
/// binary, run as a child with each raw command line below (the C runtime's hard cases,
/// the three `claude.exe` was measured on), prints the argv and the environment its own
/// std read, and `process_args` of it — its process environment block, read from here —
/// names the same words, the same `KEY=value` strings in the same order, its image as
/// its exec path, and a variable set for it, found by its name in another case.
#[cfg(windows)]
#[test]
fn process_args_reads_a_live_process_as_its_own_runtime_split_it() {
    use std::io::BufRead as _;
    use std::os::windows::process::CommandExt as _;
    if std::env::var_os(ARGS_PROBE_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    for raw in [
        r#"--x"y"" z"#,
        r#""--q r"""s t"#,
        r#"--w\\"v u""#,
        r#"a\\\\"b c" "" d"e f"g \\srv\x\ "C:\y\\""#,
    ] {
        let mut child = aterm_uds::spawnfd::command(&exe)
            .args([
                "--exact",
                "--nocapture",
                "--quiet",
                "--test-threads=1",
                "caller_shell::tests::probe_prints_its_own_argv_and_environment_when_asked",
                "--",
            ])
            .raw_arg(raw)
            .env(ARGS_PROBE_ENV, "1")
            .env("ATPKG_Probe_Var", "a=b c")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the probe");
        let (mut argv, mut env, mut ready) = (Vec::new(), Vec::new(), false);
        let out = std::io::BufReader::new(child.stdout.take().expect("stdout"));
        for line in out.lines() {
            let line = line.expect("a line");
            if line == "probe=ready" {
                ready = true;
                break;
            }
            if let Some(h) = line.strip_prefix("arg=") {
                argv.push(unhex(h));
            } else if let Some(h) = line.strip_prefix("env=") {
                env.push(unhex(h));
            }
        }
        let read = process_args(child.id());
        drop(child.stdin.take());
        let _ = child.wait();
        assert!(ready, "{raw}: the probe never said it was ready: {argv:?}");
        let read = read.unwrap_or_else(|| panic!("{raw}: the probe's block did not read"));
        assert_eq!(read.argv, argv, "{raw}");
        assert_eq!(read.env, env, "{raw}");
        assert_eq!(read.env_var("ATPKG_PROBE_VAR"), Some("a=b c"), "{raw}");
        assert_eq!(
            std::fs::canonicalize(&read.exec_path).ok(),
            std::fs::canonicalize(&exe).ok(),
            "{raw}: {}",
            read.exec_path
        );
    }
}

/// A 32-BIT PROCESS UNDER WOW64 IS READ THROUGH ITS OWN 32-BIT BLOCK, the one its
/// runtime writes: the system's 32-bit `cmd.exe` (`SysWOW64`), started with a quoted
/// word and a variable in a scratch folder, then `cd`ing into `sub` and `set`ting a
/// second variable itself, reads back its words, both variables, its image and `sub` as
/// its working directory.
#[cfg(windows)]
#[test]
fn a_wow64_process_is_read_through_its_32_bit_block() {
    use std::os::windows::process::CommandExt as _;
    let root = std::env::var_os("SystemRoot").expect("SystemRoot");
    let cmd = std::path::Path::new(&root).join("SysWOW64").join("cmd.exe");
    assert!(cmd.is_file(), "no 32-bit cmd.exe at {}", cmd.display());
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("sub");
    let tail = r#"echo "a b" c & cd sub & set ATPKG_LATER=1"#;
    let mut child = aterm_uds::spawnfd::command(&cmd)
        .args(["/d", "/q", "/k"])
        .raw_arg(tail)
        .env("ATPKG_WOW64_PROBE", "x y=z")
        .current_dir(dir.path())
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the 32-bit cmd.exe");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let (read, cwd) = loop {
        let read = process_args(child.id());
        let later = read.as_ref().and_then(|r| r.env_var("ATPKG_LATER"));
        if later.is_some() || std::time::Instant::now() >= deadline {
            break (read, aterm_sysprobe::process_cwd(child.id()));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    let read = read.expect("the 32-bit process's block read within a minute");
    assert_eq!(
        read.env_var("ATPKG_LATER"),
        Some("1"),
        "the variable it set itself was never read"
    );
    assert_eq!(read.env_var("ATPKG_WOW64_PROBE"), Some("x y=z"));
    assert_eq!(
        read.argv,
        [
            &cmd.display().to_string(),
            "/d",
            "/q",
            "/k",
            "echo",
            "a b",
            "c",
            "&",
            "cd",
            "sub",
            "&",
            "set",
            "ATPKG_LATER=1"
        ]
    );
    assert!(
        read.exec_path
            .to_ascii_lowercase()
            .ends_with(r"\syswow64\cmd.exe"),
        "{}",
        read.exec_path
    );
    let cwd = cwd.expect("its working directory");
    assert_eq!(
        std::fs::canonicalize(&cwd).ok(),
        std::fs::canonicalize(&sub).ok(),
        "{}",
        cwd.display()
    );
}

/// A MEASUREMENT, run by hand (`-- --ignored --nocapture
/// process_args_of_every_process_of_this_account`), never a gate: every process of this
/// account read as `process_args` reads it, how many read and how long it took; each
/// that did not read named by its image; how far each environment block runs past its
/// end (`EnvironmentSize` against the empty string).
#[cfg(windows)]
#[test]
#[ignore = "a measurement, run by hand"]
fn process_args_of_every_process_of_this_account() {
    let pids = aterm_uds::holders::uid_pids().expect("this account's processes");
    let started = std::time::Instant::now();
    let mut read = 0;
    let mut refused = Vec::new();
    for &pid in &pids {
        match process_args(pid) {
            Some(_) => read += 1,
            None => refused.push(pid),
        }
    }
    let took = started.elapsed();
    eprintln!(
        "{} processes: {read} read, {} not, in {took:?}",
        pids.len(),
        refused.len()
    );
    for pid in refused {
        eprintln!("  not read: {pid} {:?}", aterm_uds::holders::exe_of(pid));
    }
    let mut tails = std::collections::BTreeMap::new();
    for &pid in &pids {
        if let Some(p) = aterm_sysprobe::peb::parameters(pid) {
            let end = p.environment.windows(2).position(|w| w == [0, 0]);
            let tail = end.map(|e| p.environment.len() - (e + 2));
            *tails.entry(tail).or_insert(0) += 1;
        }
    }
    eprintln!("units past the block's end (None: no end): {tails:?}");
}
