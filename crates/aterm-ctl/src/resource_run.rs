// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A process-root registration followed by one exact, identity-preserving
//! exec. A shared launcher is never registered as this job's root.

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

const USAGE: &str = "aterm resource-run [--sock PATH] --session @self|@<sid> [--ai LABEL] --job LABEL -- PROGRAM [ARGS...]";

struct Args {
    sock: Option<String>,
    session: String,
    ai: Option<String>,
    job: String,
    command: Vec<OsString>,
}

fn parse(argv: Vec<OsString>) -> io::Result<Option<Args>> {
    let mut args = argv.into_iter();
    let (mut sock, mut session, mut ai, mut job) = (None, None, None, None);
    while let Some(arg) = args.next() {
        let Some(word) = arg.to_str() else {
            return Err(super::usage(USAGE));
        };
        if word == "--help" || word == "-h" {
            return Ok(None);
        }
        if word == "--" {
            let command: Vec<_> = args.collect();
            if command.is_empty() {
                return Err(super::usage(USAGE));
            }
            return Ok(Some(Args {
                sock,
                session: session.ok_or_else(|| super::usage(USAGE))?,
                ai,
                job: job.ok_or_else(|| super::usage(USAGE))?,
                command,
            }));
        }
        let slot = match word {
            "--sock" => &mut sock,
            "--session" => &mut session,
            "--ai" => &mut ai,
            "--job" => &mut job,
            _ => return Err(super::usage(USAGE)),
        };
        if slot.is_some() {
            return Err(super::usage(USAGE));
        }
        *slot = Some(
            args.next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| super::usage(USAGE))?,
        );
    }
    Err(super::usage(USAGE))
}

/// Register this process, then become the exact requested command.
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    match run_registered_command(argv) {
        Ok(code) => code,
        Err(error) => {
            let _ = crate::stderr_prefixed("aterm resource-run: ", &error.to_string());
            ExitCode::FAILURE
        }
    }
}

fn run_registered_command(argv: Vec<OsString>) -> io::Result<ExitCode> {
    let Some(args) = parse(argv)? else {
        use std::io::Write;
        let stdout = io::stdout();
        let mut output = crate::StdoutSink::new(stdout.lock());
        writeln!(output, "{USAGE}")?;
        return Ok(ExitCode::SUCCESS);
    };
    let self_sid = std::env::var(crate::SELF_SID_ENV)
        .ok()
        .filter(|sid| !sid.is_empty());
    let target_sid = match super::classify_selector(&args.session)? {
        super::SelTok::Sid(sid) => Some(sid.to_string()),
        _ => self_sid.clone(),
    };
    let (path, origin) = crate::resolve_target(args.sock, None, target_sid)?;
    let wire = super::ConnWire { path, origin };
    let sid = super::resolve_selector(
        &args.session,
        self_sid.as_deref(),
        &mut super::LazySessions::new(&wire),
    )?;
    let own = std::process::id();
    let start = aterm_uds::process::read_process_start(i32::try_from(own).unwrap_or(-1))
        .ok_or_else(|| io::Error::other("kernel process identity unavailable"))?
        .boot
        .0;
    let mut request = vec![
        format!("@{sid}"),
        "resources".into(),
        "register".into(),
        format!("start={start}"),
        format!("job={}", aterm_codec::pct::encode(&args.job)),
    ];
    if let Some(ai) = args.ai {
        request.push(format!("ai={}", aterm_codec::pct::encode(&ai)));
    }
    let reply = super::require_ok(wire.request(&request)?)?;
    if !registered(&reply.status, own, start, &sid) {
        return Err(io::Error::other(
            "host did not confirm this process's resource registration",
        ));
    }
    exec_command(&args.command)
}

fn exec_command(command: &[OsString]) -> io::Result<ExitCode> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Exec keeps the registered PID and start record, the launcher's
        // process group, limits, cwd, environment and every inherited fd.
        // Its wait therefore observes the actual command's exit, even when
        // that command catches a group signal and takes time to finish.
        #[expect(
            clippy::disallowed_types,
            reason = "a registered pass-through becomes its command; exec preserves all caller descriptors and process identity"
        )]
        let error = std::process::Command::new(&command[0])
            .args(&command[1..])
            .exec();
        Err(error)
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "resource-run requires a Unix process identity",
        ))
    }
}

fn registered(status: &str, pid: u32, start: u64, sid: &str) -> bool {
    let words: Vec<_> = status.split_whitespace().collect();
    if words.get(..2) != Some(&["OK", "resources"]) {
        return false;
    }
    let one = |name: &str, value: &str| {
        let prefix = format!("{name}=");
        let values: Vec<_> = words
            .iter()
            .filter_map(|word| word.strip_prefix(&prefix))
            .collect();
        values == [value]
    };
    one("version", "1")
        && one("root_pid", &pid.to_string())
        && one("start", &start.to_string())
        && one("session", sid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn non_utf8_command_arguments_remain_exact_os_strings() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![0xff, 0x80]);
        let mut argv = ["--session", "@self", "--job", "build", "--", "tool"]
            .map(OsString::from)
            .to_vec();
        argv.push(raw.clone());
        assert_eq!(parse(argv).unwrap().unwrap().command[1], raw);
    }

    #[test]
    fn arguments_after_separator_are_preserved_including_empty_words() {
        let argv = [
            "--session",
            "@self",
            "--job",
            "work:tool",
            "--",
            "tool",
            "",
            "--ai",
            "private words",
        ]
        .map(OsString::from)
        .to_vec();
        let parsed = parse(argv).unwrap().unwrap();
        assert_eq!(
            parsed.command,
            ["tool", "", "--ai", "private words"].map(OsString::from)
        );
        assert!(parsed.ai.is_none());
    }

    #[test]
    fn confirmation_requires_one_matching_identity_and_session() {
        assert!(registered(
            "OK resources version=1 root_pid=42 start=77 session=s-a",
            42,
            77,
            "s-a"
        ));
        for status in [
            "OK resources version=1 root_pid=43 start=77 session=s-a",
            "OK resources version=1 root_pid=42 start=78 session=s-a",
            "OK resources version=1 root_pid=42 start=77 session=s-b",
            "OK resources version=1 root_pid=42 root_pid=42 start=77 session=s-a",
        ] {
            assert!(!registered(status, 42, 77, "s-a"));
        }
    }
}

#[cfg(all(test, unix))]
#[path = "resource_run_tests.rs"]
mod process_tests;
