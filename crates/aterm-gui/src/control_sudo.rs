// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `sudo` control verb: aterm starts `/usr/bin/sudo` itself on the caller's exact words and
//! stdio. macOS never shows an unprivileged process a setuid sudo's argv (measured:
//! `docs/measured/sudo-argv-2026-11-02.md`), so the words the askpass sheet shows and the debugger
//! check judges are the ones aterm hands to exec, bound to the pid before sudo runs an instruction
//! (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.10).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_sysprobe::cpu_policy::Identity;
use aterm_uds::CtlStream;
use aterm_uds::spawnfd::{Command, Stdio};

use crate::askpass::AskpassTicket;
use crate::control::Scope;

const SUDO: &str = "/usr/bin/sudo";
/// `-k`: this password authorizes these words only, and sudo neither reads nor writes the record
/// it would otherwise key on aterm's own pid, shared by every session.
const SUDO_ARGS: [&str; 3] = ["-k", "-A", "--"];
const STDIO_WAIT: Duration = Duration::from_secs(10);

static BOUND: Mutex<Vec<Arc<Bound>>> = Mutex::new(Vec::new());

/// One sudo aterm started: its birth-proven identity and the exact words it was given.
#[derive(Debug)]
pub(crate) struct Bound {
    identity: Identity,
    pub(crate) command: String,
    pub(crate) purpose: Option<String>,
    pub(crate) client_pid: i32,
    tickets: Mutex<Option<Arc<AskpassTicket>>>,
}

impl Bound {
    /// Tie the open password request to this sudo: its caller's hang-up withdraws the sheet.
    pub(crate) fn watch(&self, ticket: &Arc<AskpassTicket>) {
        *self.tickets.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(ticket));
    }

    fn withdraw(&self) {
        if let Some(ticket) = self
            .tickets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            ticket.cancel();
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        crate::cpu_policy_authorization::read_identity(self.identity.pid) == Some(self.identity)
    }
}

/// The sudo aterm started under `sudo_pid`, when that pid still names it.
pub(crate) fn bound(sudo_pid: i32) -> Option<Arc<Bound>> {
    let pid = u32::try_from(sudo_pid).ok()?;
    BOUND
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|bound| bound.identity.pid == pid)
        .filter(|bound| bound.is_live())
        .map(Arc::clone)
}

struct Registered(Arc<Bound>);

impl Registered {
    fn new(bound: Arc<Bound>) -> Self {
        BOUND
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&bound));
        Self(bound)
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        BOUND
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|bound| !Arc::ptr_eq(bound, &self.0));
    }
}

/// The refusal for these words, or the argv aterm passes to sudo. The program must be absolute
/// (the client resolved it), and the check that judges a sudo's argv judges this one.
fn admit(words: &[String]) -> Result<Vec<String>, &'static str> {
    if !words
        .first()
        .is_some_and(|program| program.starts_with('/'))
    {
        return Err("ERR sudo refused command-unreadable: the program is not an absolute path\n");
    }
    let argv: Vec<String> = SUDO_ARGS
        .iter()
        .map(|arg| (*arg).to_string())
        .chain(words.iter().cloned())
        .collect();
    let mut judged = vec![SUDO.to_string()];
    judged.extend_from_slice(&argv);
    match crate::control_askpass::sudo_target_is_debugger(Some(&judged)) {
        Ok(false) => Ok(argv),
        Ok(true) => Err("ERR sudo refused debugger-as-root\n"),
        Err(()) => Err("ERR sudo refused command-unreadable: the target cannot be classified\n"),
    }
}

/// The exact words as a shell would need them, so `a b` never reads as two words. Then escaped so
/// no control or bidi byte can forge the sheet.
fn shown(words: &[String]) -> String {
    let quoted: Vec<String> = words
        .iter()
        .map(|word| {
            let plain = !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_alphanumeric() || "-_./:=@%+,".contains(c));
            if plain {
                word.clone()
            } else {
                format!("'{}'", word.replace('\'', "'\\''"))
            }
        })
        .collect();
    crate::askpass::escape_display(&quoted.join(" "))
}

/// What the sheet shows of the run: the exact words ([`shown`]) and the folder they run in
/// (`cwd`, escaped the same way), since root resolves every relative word there and the person
/// typing the password must see what `.` or `./ssh` names.
fn sheet_text(cwd: &str, words: &[String]) -> String {
    format!(
        "{}\n\nIn the folder:\n{}",
        shown(words),
        crate::askpass::escape_display(cwd)
    )
}

/// A purpose derived from exact, bounded operations, never a caller-supplied description.
fn rename_purpose(words: &[String]) -> Option<String> {
    let [program, verb, field, number] = words else {
        return None;
    };
    let scutil = program == "/usr/sbin/scutil"
        && verb == "--set"
        && matches!(
            field.as_str(),
            "ComputerName" | "LocalHostName" | "HostName"
        );
    let own = std::env::current_exe().ok();
    let batch = own.as_deref() == Some(std::path::Path::new(program))
        && verb == "name"
        && field == "apply-system";
    if !scutil && !batch {
        return None;
    }
    let number = aterm_types::machine_name::Number::parse(number).ok()?;
    Some(format!(
        "Enter your system sudo password to change your system name to {number}."
    ))
}

/// The caller's stdin, stdout and stderr, sent as one message once it read the admission.
fn receive_stdio(stream: &CtlStream) -> Option<[Stdio; 3]> {
    let deadline = Instant::now() + STDIO_WAIT;
    let _ = stream.set_read_timeout(Some(STDIO_WAIT));
    loop {
        let mut byte = [0u8; 1];
        match aterm_uds::fdpass::recv_with_fds(stream, &mut byte, 3) {
            Ok(received) => {
                let [stdin, stdout, stderr]: [_; 3] = received.fds.try_into().ok()?;
                return Some([stdin.into(), stdout.into(), stderr.into()]);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

fn exit_line(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("OK sudo exit={code}\n"),
        (None, Some(signal)) => format!("OK sudo signal={signal}\n"),
        (None, None) => "ERR sudo ended with no status\n".to_string(),
    }
}

/// Start `command` stopped, bind its pid to `sheet` (the words and folder the password sheet
/// shows, [`sheet_text`]) before it runs an instruction, and see it out.
/// A caller that hangs up has its sheet withdrawn and sudo sent `SIGTERM`, which sudo relays to the
/// command. aterm (uid 501) cannot signal the root command itself, and `SIGKILL` to sudo would only
/// end the relay and orphan the command, so a command that outlives the signal by `grace` runs on to
/// its own end: the lane stops waiting, and the reaper collects sudo when it exits.
fn run_bound(
    mut command: Command,
    sheet: String,
    purpose: Option<String>,
    client_pid: i32,
    grace: Duration,
    hung_up: &mut dyn FnMut() -> bool,
) -> String {
    command.start_suspended(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return format!("ERR sudo could not start: {error}\n"),
    };
    let Some(identity) = crate::cpu_policy_authorization::read_identity(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return "ERR sudo refused its process identity is unreadable\n".to_string();
    };
    let bound = Arc::new(Bound {
        identity,
        command: sheet,
        purpose,
        client_pid,
        tickets: Mutex::new(None),
    });
    let _registered = Registered::new(Arc::clone(&bound));
    if let Err(error) = child.resume() {
        let _ = child.kill();
        let _ = child.wait();
        return format!("ERR sudo could not start: {error}\n");
    }
    let started = Instant::now();
    let mut terminated: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return exit_line(status),
            Ok(None) => {}
            Err(error) => return format!("ERR sudo lost its process: {error}\n"),
        }
        if hung_up() {
            match terminated {
                None => {
                    bound.withdraw();
                    if let Err(error) = child.terminate() {
                        return format!(
                            "ERR sudo hung up: SIGTERM to sudo failed ({error}); the command runs on to its own end\n"
                        );
                    }
                    terminated = Some(Instant::now());
                }
                Some(at) if at.elapsed() >= grace => {
                    return "ERR sudo hung up: the command outlived SIGTERM and runs on to its own end\n"
                        .to_string();
                }
                Some(_) => {}
            }
        }
        // Quick commands answer quickly; a long one costs five wakes a second.
        std::thread::sleep(
            if started.elapsed() < Duration::from_secs(2) || terminated.is_some() {
                Duration::from_millis(10)
            } else {
                crate::control::HANGUP_POLL
            },
        );
    }
}

/// Serve one `sudo` request; the reply is the verb's last line.
pub(crate) fn serve(
    stream: &CtlStream,
    rest: &str,
    scope: Scope,
    store: &crate::session_store::Store,
) -> String {
    use std::io::Write as _;
    if scope != Scope::Owner {
        return "ERR sudo refused owner-only\n".to_string();
    }
    if let Some(refusal) = crate::fabric::app_halt_refusal(store, "sudo") {
        return refusal;
    }
    if crate::askpass::relay_off() {
        return "ERR sudo refused off\n".to_string();
    }
    let Some((cwd, words)) = aterm_uds::sudo::decode_command(rest) else {
        return "ERR usage: aterm ctl sudo -- <command> [args...]\n".to_string();
    };
    // The folder is shown on the sheet as the client sent it, so it must name one folder
    // whatever aterm's own directory is.
    if !cwd.starts_with('/') {
        return "ERR sudo refused the working directory is not an absolute path\n".to_string();
    }
    let argv = match admit(&words) {
        Ok(argv) => argv,
        Err(refusal) => return refusal.to_string(),
    };
    let Some(client_pid) = aterm_uds::fdpass::peer_pid(stream)
        .ok()
        .and_then(|pid| i32::try_from(pid).ok())
    else {
        return "ERR sudo refused the caller is unreadable\n".to_string();
    };
    if crate::spare::askpass_refused(client_pid) {
        return "ERR sudo refused no such session\n".to_string();
    }
    let helper = match crate::cpu_policy_authorization::fixed_paths() {
        Ok((_, helper)) => helper,
        Err(why) => return format!("ERR sudo unavailable: {why}\n"),
    };
    let Some(sock) = crate::proxy::self_sock_path() else {
        return "ERR sudo unavailable: this instance has no control socket\n".to_string();
    };
    if (&*stream).write_all(b"OK sudo stdio\n").is_err() {
        return String::new();
    }
    let Some([stdin, stdout, stderr]) = receive_stdio(stream) else {
        return "ERR sudo refused the caller sent no stdio\n".to_string();
    };
    let mut command = aterm_uds::spawnfd::command(SUDO);
    command
        .args(&argv)
        .current_dir(&cwd)
        .env("SUDO_ASKPASS", helper)
        .env(aterm_uds::sudo::ASKPASS_SOCK_ENV, sock)
        .env_remove("ATERM_PARENT_SESSION_ID")
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    run_bound(
        command,
        sheet_text(&cwd, &words),
        rename_purpose(&words),
        client_pid,
        aterm_uds::process::END_GRACE,
        &mut || crate::control::caller_hung_up(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_string()).collect()
    }

    #[test]
    fn rename_summary_requires_a_fixed_operation_and_valid_number() {
        for field in ["ComputerName", "LocalHostName", "HostName"] {
            assert!(
                rename_purpose(&words(&["/usr/sbin/scutil", "--set", field, "m59"]))
                    .unwrap()
                    .ends_with("m59.")
            );
        }
        for args in [
            words(&["/tmp/scutil", "--set", "HostName", "m59"]),
            words(&["/usr/sbin/scutil", "--get", "HostName", "m59"]),
            words(&["/usr/sbin/scutil", "--set", "HostName", "m59; do more"]),
            words(&["/usr/sbin/scutil", "--set", "HostName", "m59", "extra"]),
        ] {
            assert_eq!(rename_purpose(&args), None);
        }
        let exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(rename_purpose(&words(&[&exe, "name", "apply-system", "m59"])).is_some());
        assert_eq!(
            rename_purpose(&words(&["/tmp/aterm", "name", "apply-system", "m59"])),
            None
        );
    }

    #[test]
    fn the_check_judges_the_words_aterm_will_exec() {
        assert_eq!(
            admit(&words(&["/usr/sbin/scutil", "--get", "ComputerName"])).expect("ordinary"),
            words(&[
                "-k",
                "-A",
                "--",
                "/usr/sbin/scutil",
                "--get",
                "ComputerName"
            ])
        );
        // An argument named like a debugger is not the target.
        assert!(admit(&words(&["/bin/echo", "lldb"])).is_ok());
        for debugger in [
            "/usr/bin/lldb",
            "/tmp/private tools/gdb",
            "/usr/sbin/dtrace",
        ] {
            assert_eq!(
                admit(&words(&[debugger, "x"])),
                Err("ERR sudo refused debugger-as-root\n"),
                "{debugger}"
            );
        }
        for unjudgeable in [&["lldb"][..], &["FOO=bar", "/usr/bin/lldb"], &[""], &[]] {
            assert!(
                admit(&words(unjudgeable))
                    .is_err_and(|refusal| refusal.contains("command-unreadable")),
                "{unjudgeable:?}"
            );
        }
        assert!(admit(&words(&["/odd=path/tool"])).is_err());
    }

    /// What the sheet shows tells one word holding a space from two words, which is exactly what
    /// the `ps` line cannot do. NEGATIVE CONTROL: the two forms differ.
    #[test]
    fn the_sheet_shows_word_boundaries_and_no_control_bytes() {
        assert_eq!(shown(&words(&["/bin/rm", "a b"])), "/bin/rm 'a b'");
        assert_ne!(
            shown(&words(&["/bin/rm", "a b"])),
            shown(&words(&["/bin/rm", "a", "b"]))
        );
        assert_eq!(shown(&words(&["/bin/x", ""])), "/bin/x ''");
        assert_eq!(shown(&words(&["/bin/x", "it's"])), "/bin/x 'it'\\''s'");
        assert!(!shown(&words(&["/bin/x", "a\nb\u{202e}"])).contains('\n'));
        assert!(!shown(&words(&["/bin/x", "a\nb\u{202e}"])).contains('\u{202e}'));
        for forger in ['\u{2028}', '\u{2029}', '\u{061c}'] {
            let line = shown(&words(&["/bin/x", &format!("a{forger}Asked by:")]));
            assert!(!line.contains(forger), "{forger:?} reached the sheet");
        }
    }

    /// The sheet names the folder the words run in, where root resolves a relative word, and
    /// no folder name can forge the sheet's other lines.
    /// NEGATIVE CONTROL: the same words in two folders read differently.
    #[test]
    fn the_sheet_names_the_folder_the_command_runs_in() {
        let chmod = words(&["/bin/chmod", "-R", "777", "."]);
        let at_root = sheet_text("/", &chmod);
        assert_eq!(at_root, "/bin/chmod -R 777 .\n\nIn the folder:\n/");
        assert_ne!(at_root, sheet_text("/Users//a/project", &chmod));
        let forged = sheet_text("/tmp/x\n\nAsked by:\nyou", &chmod);
        assert_eq!(forged.matches('\n').count(), 3, "{forged}");
    }

    fn stand_in(script: &str) -> Command {
        let mut command = aterm_uds::spawnfd::command(aterm_uds::os_path!("sh"));
        command.args(["-c", script]);
        command
    }

    /// The binding exists while the process runs and is gone when it ends, under the pid aterm
    /// spawned and no other; the command's stdout is the caller's and its code comes back.
    #[test]
    fn a_started_command_is_bound_for_its_life_and_reports_its_exit() {
        let (reader, writer) = std::io::pipe().expect("pipe");
        let mut command = stand_in("echo ran; exit 7");
        command.stdout(writer);
        let marker = "/bin/bound-for-its-life".to_owned();
        let mut seen: Option<u32> = None;
        let line = run_bound(
            command,
            marker,
            None,
            4242,
            Duration::from_secs(60),
            &mut || {
                let table = BOUND.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(bound) = table
                    .iter()
                    .find(|b| b.command == "/bin/bound-for-its-life")
                {
                    seen = Some(bound.identity.pid);
                    assert_eq!(bound.client_pid, 4242);
                }
                false
            },
        );
        assert_eq!(line, "OK sudo exit=7\n");
        let output = std::io::read_to_string(reader).expect("the command's stdout");
        assert_eq!(output, "ran\n");
        let pid = seen.expect("bound while running");
        assert!(bound(i32::try_from(pid).unwrap()).is_none(), "gone at exit");
        assert!(bound(i32::MAX).is_none());
    }

    /// A caller's hang-up ends the command and withdraws the open sheet's ticket.
    /// NEGATIVE CONTROL: until the hang-up the command is running and the ticket live.
    #[test]
    fn a_caller_hang_up_ends_the_command_and_withdraws_the_sheet() {
        let marker = "/bin/hang-up-case".to_owned();
        let ticket = Arc::new(AskpassTicket::new(
            1,
            Instant::now() + Duration::from_secs(60),
        ));
        let mut watched = false;
        let line = run_bound(
            stand_in("exec sleep 60"),
            marker,
            None,
            4243,
            Duration::from_secs(60),
            &mut || {
                if watched {
                    return true;
                }
                let table = BOUND.lock().unwrap_or_else(PoisonError::into_inner);
                let found = table.iter().find(|b| b.command == "/bin/hang-up-case");
                if let Some(bound) = found {
                    bound.watch(&ticket);
                    assert!(bound.is_live());
                    assert!(ticket.is_live(Instant::now()));
                    watched = true;
                }
                false
            },
        );
        assert_eq!(line, "OK sudo signal=15\n");
        assert!(!ticket.is_live(Instant::now()));
    }

    /// A command that ignores `SIGTERM` is not killed behind the relay (aterm cannot reach a root
    /// command, and `SIGKILL` to sudo would orphan it): the lane stops waiting and says so.
    /// NEGATIVE CONTROL: the process is still alive when the verb answers.
    #[test]
    fn a_command_that_ignores_the_hang_up_runs_on_and_frees_the_lane() {
        let marker = "/bin/ignores-term".to_owned();
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-sudo-term-")
            .tempdir()
            .expect("temp dir");
        let armed = scratch.path().join("armed");
        let mut command = stand_in("trap '' TERM; : > \"$1\"; sleep 3; :");
        command.arg("sh").arg(&armed);
        let mut pid: Option<u32> = None;
        let line = run_bound(
            command,
            marker,
            None,
            4244,
            Duration::from_millis(200),
            &mut || {
                let table = BOUND.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(bound) = table.iter().find(|b| b.command == "/bin/ignores-term") {
                    pid = Some(bound.identity.pid);
                }
                // Hang up only once the shell ignores SIGTERM, which it shows by the file.
                pid.is_some() && armed.exists()
            },
        );
        assert_eq!(
            line,
            "ERR sudo hung up: the command outlived SIGTERM and runs on to its own end\n"
        );
        let pid = pid.expect("bound");
        assert!(
            crate::cpu_policy_authorization::read_identity(pid).is_some(),
            "still running"
        );
    }

    #[test]
    fn stdio_is_exactly_three_descriptors() {
        use std::os::fd::AsFd as _;
        let (client, server) = CtlStream::pair().expect("pair");
        let (read, write) = std::io::pipe().expect("pipe");
        aterm_uds::fdpass::send_with_fds(
            &client,
            b"s",
            &[read.as_fd(), write.as_fd(), write.as_fd()],
        )
        .expect("send");
        assert!(receive_stdio(&server).is_some());
        aterm_uds::fdpass::send_with_fds(&client, b"s", &[write.as_fd()]).expect("send one");
        assert!(receive_stdio(&server).is_none(), "one is not stdio");
        drop(client);
        assert!(receive_stdio(&server).is_none(), "hung up");
    }
}
