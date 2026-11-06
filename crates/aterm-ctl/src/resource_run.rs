// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A process-root registration followed by one exact, identity-preserving exec.

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

const USAGE: &str = "usage: aterm resource-run [--sock PATH] [--session @self|@<sid>|@<local-id>] [--ai WORD] --job LABEL [--cpu-units N --memory-bytes M [--timeout-ms N] [--status-fd N]] -- PROGRAM [ARGS...]\n       aterm resource-run [--sock PATH] [--session @self|@<sid>] --capabilities";

#[cfg(unix)]
#[path = "resource_run_admission.rs"]
mod admission;

#[cfg(any(unix, test))]
struct Resources {
    cpu_units: u32,
    memory_bytes: u64,
    timeout_ms: Option<u64>,
    status_fd: Option<i32>,
}

#[cfg(not(any(unix, test)))]
struct Resources;

impl Resources {
    #[cfg(any(unix, test))]
    fn validated(
        cpu_units: u32,
        memory_bytes: u64,
        timeout_ms: Option<u64>,
        status_fd: Option<i32>,
    ) -> Self {
        Self {
            cpu_units,
            memory_bytes,
            timeout_ms,
            status_fd,
        }
    }

    // Unsupported platforms retain successful argument validation, not a request
    // payload that no native admission transport can consume.
    #[cfg(not(any(unix, test)))]
    fn validated(_: u32, _: u64, _: Option<u64>, _: Option<i32>) -> Self {
        Self
    }
}

const ABOUT: &str = "Registers this process with the session's aterm as job LABEL, then execs PROGRAM in its\n\
                     place (same pid, descriptors and process group): its CPU and memory are charged to\n\
                     that tab. --ai names the agent the job runs for (default: the agent the tab\n\
                     runs, else `unknown`). `aterm help resource-run` says more.";

struct Args {
    sock: Option<String>,
    session: String,
    ai: Option<String>,
    job: String,
    command: Vec<OsString>,
    resources: Option<Resources>,
    capabilities: bool,
}

fn parse(argv: Vec<OsString>) -> io::Result<Option<Args>> {
    let mut args = argv.into_iter();
    let (mut sock, mut session, mut ai, mut job) = (None, None, None, None);
    let (mut cpus, mut memory, mut timeout, mut status) = (None, None, None, None);
    let mut capabilities = false;
    while let Some(arg) = args.next() {
        let Some(word) = arg.to_str() else {
            return Err(super::usage(USAGE));
        };
        if word == "--help" || word == "-h" {
            return Ok(None);
        }
        if word == "--capabilities" {
            if capabilities {
                return Err(super::usage(USAGE));
            }
            capabilities = true;
            continue;
        }
        if word == "--" {
            let resources = resources(cpus, memory, timeout, status)?;
            let command: Vec<_> = args.collect();
            if command.is_empty() || capabilities || (resources.is_some() && ai.is_some()) {
                return Err(super::usage(USAGE));
            }
            return Ok(Some(Args {
                sock,
                session: match session {
                    Some(session) => session,
                    None if resources.is_some() => "@self".into(),
                    None => return Err(super::usage(USAGE)),
                },
                ai,
                job: job.ok_or_else(|| super::usage(USAGE))?,
                command,
                resources,
                capabilities: false,
            }));
        }
        let slot = match word {
            "--sock" => &mut sock,
            "--session" => &mut session,
            "--ai" => &mut ai,
            "--job" => &mut job,
            "--cpu-units" => &mut cpus,
            "--memory-bytes" => &mut memory,
            "--timeout-ms" => &mut timeout,
            "--status-fd" => &mut status,
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
    if capabilities
        && ai.is_none()
        && job.is_none()
        && cpus.is_none()
        && memory.is_none()
        && timeout.is_none()
        && status.is_none()
    {
        return Ok(Some(Args {
            sock,
            session: session.unwrap_or_else(|| "@self".into()),
            ai,
            job: String::new(),
            command: Vec::new(),
            resources: None,
            capabilities: true,
        }));
    }
    Err(super::usage(USAGE))
}

fn resources(
    cpus: Option<String>,
    memory: Option<String>,
    timeout: Option<String>,
    status: Option<String>,
) -> io::Result<Option<Resources>> {
    fn positive(value: &str) -> io::Result<u64> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(super::usage(USAGE));
        }
        value
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| super::usage(USAGE))
    }
    match (cpus, memory) {
        (None, None) if timeout.is_none() && status.is_none() => Ok(None),
        (Some(cpus), Some(memory)) => Ok(Some(Resources::validated(
            u32::try_from(positive(&cpus)?).map_err(|_| super::usage(USAGE))?,
            positive(&memory)?,
            timeout.as_deref().map(positive).transpose()?,
            status
                .as_deref()
                .map(positive)
                .transpose()?
                .map(|fd| {
                    i32::try_from(fd)
                        .ok()
                        .filter(|fd| *fd > 2)
                        .ok_or_else(|| super::usage(USAGE))
                })
                .transpose()?,
        ))),
        _ => Err(super::usage(USAGE)),
    }
}

/// Register this process, then become the exact requested command.
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    let admitted = argv.iter().any(|word| {
        matches!(
            word.to_str(),
            Some(
                "--cpu-units"
                    | "--memory-bytes"
                    | "--timeout-ms"
                    | "--status-fd"
                    | "--capabilities"
            )
        )
    });
    match run_registered_command(argv) {
        Ok(code) => code,
        Err(error) => {
            let _ = crate::stderr_prefixed("aterm resource-run: ", &error.to_string());
            if admitted {
                ExitCode::from(75)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn run_registered_command(argv: Vec<OsString>) -> io::Result<ExitCode> {
    let Some(args) = parse(argv)? else {
        use std::io::Write;
        let stdout = io::stdout();
        let mut output = crate::StdoutSink::new(stdout.lock());
        writeln!(output, "{USAGE}\n{ABOUT}")?;
        return Ok(ExitCode::SUCCESS);
    };
    let self_sid = std::env::var(crate::SELF_SID_ENV)
        .ok()
        .filter(|sid| !sid.is_empty());
    let target_sid = match super::classify_selector(&args.session)? {
        super::SelTok::Sid(sid) => Some(sid.to_string()),
        _ => self_sid.clone(),
    };
    let (path, origin) = crate::resolve_target(args.sock.clone(), None, target_sid)?;
    let wire = super::ConnWire { path, origin };
    let sid = super::resolve_selector(
        &args.session,
        self_sid.as_deref(),
        &mut super::LazySessions::new(&wire),
    )?;
    if args.capabilities || args.resources.is_some() {
        #[cfg(unix)]
        return admission::run(&wire, &sid, &args);
        #[cfg(not(unix))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retained resource admission requires Unix",
        ));
    }
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
    fn retained_admission_requires_both_positive_resources_and_defaults_self() {
        let args = [
            "--cpu-units",
            "4",
            "--memory-bytes",
            "1024",
            "--timeout-ms",
            "90",
            "--status-fd",
            "7",
            "--job",
            "solve",
            "--",
            "ay",
            "input",
        ];
        let parsed = parse(args.map(OsString::from).to_vec()).unwrap().unwrap();
        assert_eq!(parsed.session, "@self");
        let resources = parsed.resources.unwrap();
        assert_eq!(
            (
                resources.cpu_units,
                resources.memory_bytes,
                resources.timeout_ms,
                resources.status_fd
            ),
            (4, 1024, Some(90), Some(7))
        );
        for prefix in [
            vec!["--cpu-units", "4"],
            vec!["--memory-bytes", "1024"],
            vec!["--timeout-ms", "90"],
            vec!["--cpu-units", "0", "--memory-bytes", "1024"],
            vec!["--cpu-units", "4", "--memory-bytes", "-1"],
            vec![
                "--cpu-units",
                "4",
                "--memory-bytes",
                "1024",
                "--ai",
                "ignored",
            ],
            vec![
                "--cpu-units",
                "4",
                "--memory-bytes",
                "1024",
                "--status-fd",
                "1",
            ],
        ] {
            let mut args = prefix;
            args.extend(["--job", "solve", "--", "ay"]);
            assert!(parse(args.into_iter().map(OsString::from).collect()).is_err());
        }
    }

    #[test]
    fn capability_query_accepts_no_execution_or_resource_arguments() {
        assert!(
            parse(vec!["--capabilities".into()])
                .unwrap()
                .unwrap()
                .capabilities
        );
        for args in [
            vec!["--capabilities", "--capabilities"],
            vec!["--capabilities", "--job", "job"],
            vec!["--capabilities", "--", "ay"],
            vec!["--capabilities", "--cpu-units", "2"],
        ] {
            assert!(parse(args.into_iter().map(OsString::from).collect()).is_err());
        }
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
