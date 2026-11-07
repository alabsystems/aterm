// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native-image Claude stand-in for explicitly owned private host fixtures.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// The version of this managed copy, shared by its probe and session record.
fn managed_version() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::read_to_string(exe.with_file_name("fixture-version")).ok())
        .map(|version| version.trim().to_owned())
}

/// Whether this run was the harness probing a managed copy without a fixture root.
fn answered_build_probe() -> bool {
    let Some(version) = managed_version() else {
        return false;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("{} (Claude Code)", version.trim());
        return true;
    }
    args == ["--help"]
}

fn append(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the private fixture journal");
    file.write_all(bytes)
        .expect("append the private fixture journal");
}

fn start_word(pid: u32) -> String {
    aterm_agent::harness::restart_stamp::kernel_start(pid)
        .expect("the native fixture's kernel birth")
}

struct Terminal(libc::termios);
impl Terminal {
    fn enter() -> Self {
        // SAFETY: both termios values are initialized and descriptor 0 is this fixture's PTY.
        unsafe {
            let mut old: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(0, &mut old), 0);
            let mut raw = old;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 1;
            assert_eq!(libc::tcsetattr(0, libc::TCSANOW, &raw), 0);
            Self(old)
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: restore the terminal state read on this process's descriptor 0.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

fn paint(root: &Path, screen: &str, clear: bool) {
    let text =
        std::fs::read_to_string(root.join(format!("sc/{screen}.txt"))).expect("the fixture screen");
    let (row, column) = text
        .lines()
        .enumerate()
        .filter_map(|(row, line)| {
            line.strip_prefix("❯ ").map(|draft| {
                let column = if screen == "restored" {
                    3 + draft.chars().count()
                } else {
                    3
                };
                (row + 1, column)
            })
        })
        .last()
        .unwrap_or((1, 3));
    let mut out = std::io::stdout().lock();
    if clear {
        out.write_all(b"\x1b[2J\x1b[H").unwrap();
    }
    out.write_all(text.as_bytes()).unwrap();
    write!(out, "\x1b[{row};{column}H").unwrap();
    out.flush().unwrap();
}

pub(super) fn run_if_selected() -> bool {
    let selected = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(|name| name == "claude"))
        .unwrap_or(false);
    if !selected {
        return false;
    }
    if answered_build_probe() {
        return true;
    }
    let Some(root) = std::env::var_os("ATERM_CLAUDE_FIXTURE_ROOT").map(PathBuf::from) else {
        return false;
    };
    assert!(
        root.is_absolute() && root.is_dir(),
        "explicit private fixture root"
    );
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("2.1.281 (Claude Code)");
        return true;
    }
    let pid = std::process::id();
    // SAFETY: getppid observes this synthetic child process's parent without dereferencing data.
    let parent = unsafe { libc::getppid() };
    std::fs::write(root.join("relaunch-parent"), parent.to_string()).unwrap();
    let mut recorded_argv = Vec::new();
    for arg in &args {
        recorded_argv.extend_from_slice(arg.as_bytes());
        recorded_argv.push(0);
    }
    std::fs::write(root.join(format!("launch-{pid}.argv")), recorded_argv).unwrap();
    append(
        &root.join("launches.log"),
        format!("{pid} {}\n", args.join(" ")).as_bytes(),
    );
    if let Some(probe) = std::env::var_os("ATERM_RESTART_ENV_PROBE") {
        append(
            &root.join("launch-env.log"),
            format!(
                "{pid} {} {} {}\n",
                u8::from(probe == "outer"),
                u8::from(probe == "agent"),
                u8::from(probe == "helper")
            )
            .as_bytes(),
        );
    }
    let conversation = args
        .windows(2)
        .find(|pair| pair[0] == "--resume")
        .map_or("0badf00d-1111-2222-3333-444455556666", |pair| {
            pair[1].as_str()
        });
    assert!(
        !conversation.contains(['/', '\\', '\0']),
        "fixture conversation is a filename"
    );
    // SAFETY: the signal handler only sets a process-local atomic flag.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
    }
    let terminal = Terminal::enter();
    if args.iter().any(|arg| arg == "--resume") && root.join("hold-register").exists() {
        while !root.join("register").exists() {
            if STOP.load(Ordering::Relaxed) {
                drop(terminal);
                std::process::exit(143);
            }
            if root.join("quit").exists() {
                std::fs::remove_file(root.join("quit")).unwrap();
                return true;
            }
            if root.join("die").exists() {
                std::fs::remove_file(root.join("die")).unwrap();
                aterm_uds::process::kill_self_now();
            }
            let mut bytes = [0_u8; 4096];
            match std::io::stdin().read(&mut bytes) {
                Ok(0) => {}
                Ok(count) => append(&root.join(format!("typed-{pid}.log")), &bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("fixture registration input: {error}"),
            }
        }
    }
    let home = PathBuf::from(std::env::var_os("HOME").expect("fixture HOME"));
    let sessions = home.join(".claude/sessions");
    let project = home.join(".claude/projects/-w");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    append(
        &project.join(format!("{conversation}.jsonl")),
        b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let record = aterm_json::json!({"pid":pid,"sessionId":conversation,
        "cwd":std::env::current_dir().unwrap().to_string_lossy().into_owned(),
        "version":managed_version().unwrap_or_else(|| "2.1.281".into()),
        "status":"idle","statusUpdatedAt":u64::try_from(now).unwrap(),
        "procStart":start_word(pid),"kind":"interactive","entrypoint":"cli"});
    let session = sessions.join(format!("{pid}.json"));
    std::fs::write(&session, record.to_string()).unwrap();
    print!("\x1b[?1049h\x1b[?1004h");
    let mut restored =
        args.iter().any(|arg| arg == "--resume") && root.join("sc/restored.txt").is_file();
    paint(&root, if restored { "restored" } else { "idle" }, true);
    let mut pending = Vec::new();
    while !STOP.load(Ordering::Relaxed) {
        if root.join("quit").exists() {
            std::fs::remove_file(root.join("quit")).unwrap();
            break;
        }
        if root.join("die").exists() {
            std::fs::remove_file(root.join("die")).unwrap();
            aterm_uds::process::kill_self_now();
        }
        if restored && root.join("undraft").exists() {
            std::fs::remove_file(root.join("undraft")).unwrap();
            paint(&root, "cleared", true);
        }
        let mut bytes = [0_u8; 4096];
        let count = match std::io::stdin().read(&mut bytes) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("fixture terminal read: {error}"),
        };
        if count == 0 {
            continue;
        }
        let bytes = &bytes[..count];
        append(&root.join(format!("typed-{pid}.log")), bytes);
        pending.extend_from_slice(bytes);
        if bytes.iter().any(|byte| matches!(byte, b'\r' | b'\n')) {
            let text = String::from_utf8_lossy(&pending).replace(['\r', '\n'], "");
            append(
                &root.join("typed.log"),
                format!("{pid} {text}\n").as_bytes(),
            );
            pending.clear();
            if text == "/exit" {
                break;
            }
            if !text.is_empty() {
                // This owned stand-in answers the submitted turn, not its startup.
                // Explicit launch selection wins; claude-test is the same synthetic
                // default used by the other private host fixture.
                let model = aterm_agent::harness::upgrade::launch_model(&args)
                    .unwrap_or_else(|| "claude-test".into());
                let response = aterm_json::json!({
                    "type": "assistant",
                    "version": managed_version().unwrap_or_else(|| "2.1.281".into()),
                    "message": {
                        "role": "assistant", "model": model,
                        "content": [{"type": "text", "text": "fixture response"}],
                    },
                });
                append(
                    &project.join(format!("{conversation}.jsonl")),
                    format!("{response}\n").as_bytes(),
                );
            }
            restored = false;
            paint(&root, "idle", true);
        } else if bytes[0] != 0x1b {
            let screen = std::fs::read_to_string(root.join("sc/idle.txt")).unwrap();
            let row = screen
                .lines()
                .enumerate()
                .filter_map(|(row, line)| line.starts_with('❯').then_some(row + 1))
                .last()
                .unwrap_or(1);
            print!("\x1b[{row};3H\x1b[K[Pasted text #1]");
            std::io::stdout().flush().unwrap();
        }
    }
    std::fs::remove_file(session).unwrap();
    drop(terminal);
    if STOP.load(Ordering::Relaxed) {
        std::process::exit(143);
    }
    true
}
