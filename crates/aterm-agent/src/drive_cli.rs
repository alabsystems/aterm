// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive`: CLI entry point for the window harness readers and tape tools.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::supervise::limit::tz_offset_s;
use crate::supervise::tape::acts::Candidate;
use crate::supervise::tape::{corpus, explain, replay, show, store};
use crate::supervise::{
    self, ANCHOR_REMOVED, LedgerFormat, LedgerHost, LedgerOpts, Session, SupervisorConfig,
    classify_command_with, render_phase_and_survey,
};
use crate::{CtlClient, DRIVE_HELP};

#[derive(Debug)]
struct Opts {
    socket: Option<String>,
    cmd: Vec<String>,
}

fn parse(argv: Vec<std::ffi::OsString>) -> Result<Opts, String> {
    let mut socket = None;
    let mut cmd = Vec::new();
    let mut it = argv.into_iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" | "--sock" => {
                socket = Some(it.next().ok_or("--socket needs a PATH")?);
            }
            "-h" | "--help" | "help" => {
                cmd = vec!["help".to_string()];
                break;
            }
            "--dial" | "--remote" => {
                return Err(format!(
                    "unknown option '{a}': read another Mac's sessions with `aterm ctl survey \
                     --host <h>`, drive them with `aterm ctl --host <h> …`"
                ));
            }
            other if other.starts_with("--") => {
                return Err(format!(
                    "unknown option '{other}' (only --socket PATH); run `aterm drive --help`"
                ));
            }
            _ => {
                cmd.push(a);
                cmd.extend(it.by_ref());
                if matches!(cmd.get(1).map(String::as_str), Some("-h" | "--help")) {
                    cmd = vec!["help".to_string()];
                }
                break;
            }
        }
    }
    Ok(Opts { socket, cmd })
}

/// Output text and exit code produced by a drive command.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub code: u8,
}

impl Reply {
    const fn text(text: String) -> Self {
        Self { text, code: 0 }
    }
}

/// The arguments after a verb: `[@sid]` then its flags.
#[derive(Debug, Default, PartialEq, Eq)]
struct SubArgs {
    sid: Option<String>,
    /// `answer --box TOKEN`: the question dialog's token the answer was
    /// decided on (`drive phase`'s `box` line).
    box_token: Option<String>,
    allow_python: Vec<String>,
    /// `ledger --journal FILE`: the supervisor's journal it reads back.
    journal: Option<PathBuf>,
    /// `ledger --format`.
    format: Option<LedgerFormat>,
    /// `ledger --out PATH`: write the ledger there instead of to stdout.
    out: Option<PathBuf>,
    /// `ledger --since`: Unix ms, from a time word.
    since_ms: Option<i64>,
    /// Positional words (the command text for `classify`, the answer for
    /// `answer`).
    rest: Vec<String>,
}

fn parse_sub(verb: &str, args: &[String]) -> Result<SubArgs, String> {
    let mut out = SubArgs::default();
    let words_after_first = verb == "classify";
    let mut it = args.iter();
    let need = |flag: &str, what: &str, v: Option<&String>| -> Result<String, String> {
        v.cloned()
            .ok_or_else(|| format!("{verb}: {flag} needs {what}"))
    };
    let ledgers = |flag: &str, what: &str| -> Result<(), String> {
        if verb == "ledger" {
            Ok(())
        } else {
            Err(format!("{verb}: {flag} is ledger's ({what})"))
        }
    };
    while let Some(a) = it.next() {
        if words_after_first && !out.rest.is_empty() {
            out.rest.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--" => {
                out.rest.extend(it.by_ref().cloned());
                break;
            }
            s if s.starts_with('@') && !words_after_first && out.sid.is_none() => {
                out.sid = Some(s.to_string());
            }
            "--allow-python" => out.allow_python.push(need(
                "--allow-python",
                "a GLOB (e.g. 'scripts/*report*.py')",
                it.next(),
            )?),
            "--since" => {
                ledgers("--since", "start time")?;
                let what = "a time: Unix milliseconds, or YYYY-MM-DD[THH:MM[:SS]][Z|±HH:MM]";
                let v = need("--since", what, it.next())?;
                out.since_ms = Some(
                    supervise::parse_since_local(
                        &v,
                        tz_offset_s(),
                        &supervise::limit::local_offset_at,
                    )
                    .ok_or_else(|| format!("{verb}: --since needs {what}"))?,
                );
            }
            s if s == "--source" || s.starts_with("--source=") => {
                ledgers("--source", "log")?;
                let what = "log, the one source";
                let v = match s.strip_prefix("--source=") {
                    Some(v) => v.to_string(),
                    None => need("--source", what, it.next())?,
                };
                match v.as_str() {
                    "log" => {}
                    "anchor" => {
                        return Err(format!(
                            "{verb}: --source anchor was removed on {ANCHOR_REMOVED}: the \
                             session log places every time; leave the flag out"
                        ));
                    }
                    _ => return Err(format!("{verb}: --source needs {what}")),
                }
            }
            "--journal" => {
                ledgers("--journal", "supervisor journal")?;
                out.journal = Some(PathBuf::from(need("--journal", "a FILE", it.next())?));
            }
            "--format" => {
                ledgers("--format", "text, md or html")?;
                let what = "text, md or html";
                let v = need("--format", what, it.next())?;
                out.format = Some(
                    LedgerFormat::parse(&v)
                        .ok_or_else(|| format!("{verb}: --format needs {what}"))?,
                );
            }
            "--out" => {
                ledgers("--out", "output path")?;
                out.out = Some(PathBuf::from(need("--out", "a PATH", it.next())?));
            }
            "--box" => {
                if verb != "answer" {
                    return Err(format!("{verb}: --box is answer's (question box token)"));
                }
                out.box_token = Some(need("--box", "a TOKEN from `drive phase`", it.next())?);
            }
            other if other.starts_with("--") => {
                return Err(format!(
                    "{verb}: unknown option '{other}'; run `aterm drive --help`"
                ));
            }
            other => out.rest.push(other.to_string()),
        }
    }
    Ok(out)
}

fn classify_verb(args: &[String]) -> Result<Reply, String> {
    let home = crate::supervise::policy::approval::advisory_home();
    let cwd = std::env::current_dir().ok();
    classify_verb_at(args, home.as_deref(), cwd.as_deref())
}

/// [`classify_verb`] under `home`, asked from `cwd`: the classifier's verdict, then the advisory
/// word's on the line (`approval::command_read_only_at`: the proof gaps, the blanks and the
/// default secret list, a relative path read from `cwd`, where the asking agent's line runs, and
/// a tree a reader descends walked on this machine's disk as the harness walks it,
/// `PolicyProbe::below_tree`), so the verb calls read-only no line the harness's rules refuse
/// there.
fn classify_verb_at(
    args: &[String],
    home: Option<&std::path::Path>,
    cwd: Option<&std::path::Path>,
) -> Result<Reply, String> {
    let sub = parse_sub("classify", args)?;
    let cmd = sub.rest.join(" ");
    if cmd.trim().is_empty() {
        return Err(
            "classify needs a shell command line, e.g. `aterm drive classify 'git status && git pull'`"
                .to_string(),
        );
    }
    let allow = python_allow(&sub);
    let v = classify_command_with(&cmd, &allow);
    let verdict = if v.read_only {
        supervise::policy::approval::command_read_only_at(
            &cmd,
            &allow,
            home,
            cwd,
            &|root, below, secrets| {
                use supervise::policy::PolicyProbe as _;
                supervise::policy::LiveProbe.below_tree(root, below, secrets, home)
            },
        )
    } else {
        Err(v.reason)
    };
    Ok(match verdict {
        Ok(()) => Reply::text("read-only\n".to_string()),
        Err(why) => Reply {
            text: format!("not-read-only {why}\n"),
            code: 1,
        },
    })
}

fn python_allow(sub: &SubArgs) -> Vec<String> {
    if sub.allow_python.is_empty() {
        supervise::DEFAULT_PYTHON_ALLOW
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        sub.allow_python.clone()
    }
}

fn no_positionals(verb: &str, sub: &SubArgs) -> Result<(), String> {
    if sub.rest.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{verb} takes only [@sid] and flags; unexpected: {}",
            sub.rest.join(" ")
        ))
    }
}

/// The whole drive CLI as a callable: `argv[1..]` in, exit code out.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let opts = match parse(argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("aterm drive: {e}");
            return ExitCode::FAILURE;
        }
    };
    match run(&opts) {
        Ok(Reply { text, code }) => aterm_uds::stdout::answer_exit("aterm drive", &text, code),
        Err(e) => {
            eprintln!("aterm drive: {e}");
            ExitCode::FAILURE
        }
    }
}

fn phase_reply(
    turn: &supervise::Turn,
    reader: &dyn aterm_phase::ScreenReader,
    allow: &[String],
) -> Reply {
    Reply::text(render_phase_and_survey(turn, reader, allow))
}

fn gone_to(verb: &str) -> Option<&'static str> {
    Some(match verb {
        "prompt" => "`aterm ctl @<sid> turn '<text>'`",
        "read" => "`aterm ctl @<sid> text`",
        "shot" => "`aterm ctl @<sid> image <name.png>`",
        "await" => "`aterm ctl @<sid> await <cond>`",
        "await-turn" => {
            "`aterm ctl @<sid> await agent idle,prompt,question,wall` (Claude Code, agy, Muse \
             Code); for a codex, `aterm ctl @<sid> await log kind=program \
             since=<log_seq> ep=<log_ep> base=<log_base>` (all three from `status`, read before \
             typing)"
        }
        "supervise" | "watch" => "the window's host (`aterm help harness`)",
        "task" => {
            "`aterm ctl @self spawn agent=claude -` then `aterm ctl await task @<sid> since=0`"
        }
        "report" => "`aterm ctl @<sid> result` (or `offscreen` / `text`)",
        _ => return None,
    })
}

fn run(opts: &Opts) -> Result<Reply, String> {
    let verb = opts.cmd.first().map(String::as_str).unwrap_or("help");
    if verb == "help" {
        return Ok(Reply::text(format!("{DRIVE_HELP}\n")));
    }
    if verb == "classify" {
        return classify_verb(&opts.cmd[1..]);
    }
    if let Some(instead) = gone_to(verb) {
        return Err(format!(
            "`{verb}` is not an `aterm drive` command; use {instead}"
        ));
    }
    if !matches!(verb, "phase" | "answer" | "ledger" | "tape" | "replay") {
        return Err(format!(
            "unknown command '{verb}' (classify | phase | answer | ledger | tape | replay); run \
             `aterm drive --help`"
        ));
    }
    if verb == "tape" {
        return Ok(tape_verb(&opts.cmd[1..]));
    }
    if verb == "replay" {
        return Ok(replay_verb(&opts.cmd[1..]));
    }

    if verb == "ledger" {
        return ledger_verb(opts, &opts.cmd[1..]);
    }

    let mut client = CtlClient::own_at(crate::own_exe()?, opts.socket.clone());

    if let Err(e) = client.run(&["cursor"]) {
        return Err(format!("cannot reach an aterm: {e}"));
    }

    if verb == "phase" {
        let sub = parse_sub(verb, &opts.cmd[1..])?;
        no_positionals(verb, &sub)?;
        let allow = python_allow(&sub);
        let mut session = Session::new(&mut client, sub.sid);
        let (turn, reader) = session.phase_now()?;
        return Ok(phase_reply(&turn, reader, &allow));
    }

    let sub = parse_sub(verb, &opts.cmd[1..])?;
    if sub.sid.is_none() {
        return Err(
            "answer needs the worker's @sid: `aterm drive answer @<sid> [--box TOKEN] <answer>`"
                .to_string(),
        );
    }
    let answer = sub.rest.join(" ");
    if answer.trim().is_empty() {
        return Err(
            "answer needs a choice: option number or label, `a, b`, `submit`, `recommended`, or `human`"
                .to_string(),
        );
    }
    let aopts = supervise::AnswerOpts {
        answer,
        box_token: sub.box_token,
        grace: Duration::from_secs(u64::from(harness_policy().human_grace_s)),
    };
    let mut session = Session::new(&mut client, sub.sid);
    let mut out: Vec<u8> = Vec::new();
    let code = session.answer(&aopts, &mut out)?;
    Ok(Reply {
        text: String::from_utf8_lossy(&out).into_owned(),
        code,
    })
}

fn ledger_verb(opts: &Opts, args: &[String]) -> Result<Reply, String> {
    let sub = parse_sub("ledger", args)?;
    no_positionals("ledger", &sub)?;
    let worker = match sub
        .sid
        .clone()
        .or_else(|| journal_sid(sub.journal.as_deref()))
    {
        Some(sid) => sid,
        None => {
            return Err(
                "ledger needs the worker: `aterm drive ledger @s-…` (or a single-session --journal)"
                    .to_string(),
            );
        }
    };
    let lopts = ledger_opts(&sub, worker);
    let mut client = CtlClient::own_at(crate::own_exe()?, opts.socket.clone());
    let host = LedgerHost {
        now_ms: supervise::journal::unix_ms(),
        tz_offset_s: tz_offset_s(),
    };
    let ledger = supervise::gather(&mut client, &lopts, &host);
    let text = supervise::render_ledger_local(
        &ledger,
        sub.format.unwrap_or_default(),
        &supervise::limit::local_offset_at,
    );
    match &sub.out {
        None => Ok(Reply::text(text)),
        Some(path) => {
            write_private(path, &text)?;
            Ok(Reply::text(format!("wrote {}\n", path.display())))
        }
    }
}

/// What `ledger` reads for `worker`.
fn ledger_opts(sub: &SubArgs, worker: String) -> LedgerOpts {
    LedgerOpts {
        worker,
        journal: sub.journal.clone(),
        since_ms: sub.since_ms,
    }
}

fn write_private(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write as _;

    let mut o = std::fs::OpenOptions::new();
    o.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut f = o
        .open(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    if !f
        .metadata()
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?
        .is_file()
    {
        return Err(format!(
            "cannot write {}: not a regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    f.set_len(0)
        .and_then(|()| f.write_all(text.as_bytes()))
        .and_then(|()| f.flush())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn journal_sid(path: Option<&std::path::Path>) -> Option<String> {
    let (records, _) = supervise::read_journal(path?).ok()?;
    let mut sids: Vec<&str> = records.iter().filter_map(|r| r.sid.as_deref()).collect();
    sids.sort_unstable();
    sids.dedup();
    match sids.as_slice() {
        [one] => Some((*one).to_string()),
        _ => None,
    }
}

fn tape_verb(args: &[String]) -> Reply {
    match tape_reply(args) {
        Ok((text, note)) => {
            if let Some(note) = note {
                eprintln!("aterm drive: tape: {note}");
            }
            Reply::text(text)
        }
        Err(e) => {
            eprintln!("aterm drive: {e}");
            Reply {
                text: String::new(),
                code: 2,
            }
        }
    }
}

fn replay_verb(args: &[String]) -> Reply {
    replay_reply(args).unwrap_or_else(|e| {
        eprintln!("aterm drive: replay: {e}");
        Reply {
            text: String::new(),
            code: 2,
        }
    })
}

/// [`replay_verb`]'s answer for every form (plain, counterfactual, `--why`, `--shadow`,
/// `--check`): the report and its exit code.
pub(crate) fn replay_reply(args: &[String]) -> Result<Reply, String> {
    replay_reply_with_limit(args, supervise::tape::recover::HELD_MAX as u64)
}

fn replay_reply_with_limit(args: &[String], read_limit: u64) -> Result<Reply, String> {
    let ReplayArgs {
        file,
        more,
        options,
        why,
        shadow,
        check,
    } = replay_args(args)?;
    let read = |file: &PathBuf| {
        read_tape_bytes(file, read_limit)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))
    };
    let refused = |file: &PathBuf, e: String| format!("{}: {}", file.display(), show::shown(&e));
    if check {
        let named: Vec<PathBuf> = std::iter::once(file).chain(more).collect();
        let tapes = corpus::tapes_in(&named)?;
        if tapes.is_empty() {
            return Err(format!(
                "--check found no tape (*.atape, *{}) under {}",
                corpus::TEXT_SUFFIX,
                named
                    .iter()
                    .map(|p| show::shown(&p.display().to_string()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let candidate = options.candidate.unwrap_or_default();
        let rows: Vec<corpus::Checked> = tapes
            .into_iter()
            .map(|path| {
                let outcome = read(&path).and_then(|bytes| {
                    corpus::check(&bytes, &candidate).map_err(|e| show::shown(&e))
                });
                corpus::Checked { path, outcome }
            })
            .collect();
        let codes: Vec<u8> = rows.iter().map(corpus::Checked::code).collect();
        return Ok(Reply {
            text: corpus::render(&rows),
            code: corpus::worst(&codes),
        });
    }
    if shadow {
        let candidate = options.candidate.unwrap_or_default();
        let shadow_one = |file: &PathBuf| {
            read(file).and_then(|bytes| {
                supervise::tape::shadow::shadow(&bytes, &candidate).map_err(|e| refused(file, e))
            })
        };
        if more.is_empty() {
            let shadow = shadow_one(&file)?;
            return Ok(Reply {
                text: shadow.render(),
                code: shadow.code(),
            });
        }
        let mut text = String::new();
        let mut codes = Vec::new();
        for file in std::iter::once(&file).chain(&more) {
            text.push_str(&format!("{}\n", show::shown(&file.display().to_string())));
            match shadow_one(file) {
                Ok(shadow) => {
                    text.push_str(&shadow.render());
                    codes.push(shadow.code());
                }
                Err(e) => {
                    text.push_str(&format!("  not shadowed: {e}\n"));
                    codes.push(2);
                }
            }
        }
        return Ok(Reply {
            text,
            code: worst_shadow_code(&codes),
        });
    }
    let bytes = read(&file)?;
    match why {
        None => replay::replay(&bytes, &options)
            .map(|report| Reply {
                text: report.render(),
                code: report.code(),
            })
            .map_err(|e| refused(&file, e)),
        Some((at, format)) => explain::why(&bytes, &at, &supervise::limit::local_offset_at)
            .map(|why| Reply::text(explain::render(&why, format)))
            .map_err(|e| refused(&file, e)),
    }
}

fn worst_shadow_code(codes: &[u8]) -> u8 {
    [1, 2, 3]
        .into_iter()
        .find(|code| codes.contains(code))
        .unwrap_or(0)
}

/// `replay`'s parsed arguments: the tape (more under `--shadow` and `--check`, directories
/// too under `--check`), the options with any `--set`/`--policy` candidate, and `--why`'s point.
#[derive(Debug)]
struct ReplayArgs {
    file: PathBuf,
    more: Vec<PathBuf>,
    options: replay::Options,
    why: Option<(explain::At, explain::Format)>,
    shadow: bool,
    check: bool,
}

const POLICY_INPUT_MAX: u64 = 1024 * 1024;

fn candidate_file(path: &str) -> Result<(String, supervise::SupervisorConfig), String> {
    candidate_file_with_limit(path, POLICY_INPUT_MAX)
}

fn candidate_file_with_limit(
    path: &str,
    limit: u64,
) -> Result<(String, supervise::SupervisorConfig), String> {
    let named = show::shown(path);
    let bytes = read_regular_bytes(std::path::Path::new(path), limit, "policy")
        .map_err(|e| format!("--policy {named}: cannot be read: {e}"))?;
    let text =
        std::str::from_utf8(&bytes).map_err(|e| format!("--policy {named}: not UTF-8: {e}"))?;
    let (policy, notes) = SupervisorConfig::from_aterm_toml(text);
    if !notes.is_empty() {
        return Err(format!(
            "--policy {named}: rejected by policy reader: {}",
            show::shown(&notes.join("; "))
        ));
    }
    Ok((path.to_string(), policy))
}

fn replay_args(args: &[String]) -> Result<ReplayArgs, String> {
    const USAGE: &str = "replay FILE [--until door=N] | replay FILE --set k=v… | --policy FILE \
                         [--set k=v…] [--until door=N] | replay FILE… --shadow [--set k=v…] \
                         [--policy FILE] | replay --check FILE|DIR… [--set k=v…] [--policy \
                         FILE] | replay FILE --why --at \
                         seq=N|door=N|[YYYY-MM-DDT]HH:MM[:SS]|\"<journal line>\" [--format \
                         text|md|html]";
    let mut files = Vec::new();
    let mut options = replay::Options::default();
    let (mut why, mut at, mut format) = (false, None, None);
    let (mut shadow, mut sets, mut policy) = (false, Vec::new(), None);
    let mut check = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--set" => {
                let kv = it
                    .next()
                    .ok_or_else(|| format!("--set needs k=v: {USAGE}"))?;
                let (k, v) = kv
                    .split_once('=')
                    .filter(|(k, _)| !k.trim().is_empty())
                    .ok_or_else(|| format!("--set takes k=v (a [harness] key), not {kv:?}"))?;
                sets.push((k.trim().to_string(), v.to_string()));
            }
            "--policy" => {
                let path = it
                    .next()
                    .ok_or_else(|| format!("--policy needs a FILE: {USAGE}"))?;
                if policy.is_some() {
                    return Err(format!("one --policy only: {USAGE}"));
                }
                policy = Some(candidate_file(path)?);
            }
            "--shadow" => shadow = true,
            "--check" => check = true,
            "--until" => {
                let at = it
                    .next()
                    .ok_or_else(|| format!("--until needs door=N: {USAGE}"))?;
                let n = at
                    .strip_prefix("door=")
                    .and_then(|n| crate::parse_uint::<u64>(n.strip_prefix('#').unwrap_or(n)))
                    .ok_or_else(|| {
                        format!("--until takes door=N (an effect's ordinal), not {at:?}")
                    })?;
                options.until = Some(n);
            }
            "--why" => why = true,
            "--at" => {
                let v = it
                    .next()
                    .ok_or_else(|| format!("--at needs a point: {USAGE}"))?;
                at = Some(explain::At::parse(v)?);
            }
            "--format" => {
                let v = it
                    .next()
                    .ok_or_else(|| format!("--format needs text, md or html: {USAGE}"))?;
                format = Some(
                    explain::Format::parse(v)
                        .ok_or_else(|| format!("--format needs text, md or html, not {v:?}"))?,
                );
            }
            s if s.starts_with("--") => return Err(format!("unknown option '{s}': {USAGE}")),
            s => files.push(PathBuf::from(s)),
        }
    }
    let mut files = files.into_iter();
    let file = files
        .next()
        .ok_or_else(|| format!("needs the tape's FILE: {USAGE}"))?;
    let more: Vec<PathBuf> = files.collect();
    if check {
        // A corpus check replays every tape whole, in acts mode: it names no
        // point, and decides nothing again.
        if why || at.is_some() || format.is_some() || shadow || options.until.is_some() {
            return Err(format!(
                "--check takes only FILEs, DIRs, and --set/--policy: {USAGE}"
            ));
        }
        if !sets.is_empty() || policy.is_some() {
            options.candidate = Some(Candidate { file: policy, sets });
        }
        return Ok(ReplayArgs {
            file,
            more,
            options,
            why: None,
            shadow: false,
            check: true,
        });
    }
    if let Some(also) = more.first().filter(|_| !shadow) {
        return Err(format!(
            "one FILE only, not also {:?} (--shadow and --check take more than one)",
            also.display().to_string()
        ));
    }
    let why = match (why, at) {
        (false, None) if format.is_none() => None,
        (false, _) => return Err(format!("--at and --format require --why: {USAGE}")),
        (true, None) => return Err(format!("--why needs --at: {USAGE}")),
        (true, Some(_)) if options.until.is_some() => {
            return Err(format!("--why takes no --until: {USAGE}"));
        }
        (true, Some(at)) => Some((at, format.unwrap_or_default())),
    };
    let counterfactual = shadow || !sets.is_empty() || policy.is_some();
    if why.is_some() && counterfactual {
        return Err(format!(
            "--why cannot be combined with --shadow, --set, or --policy: {USAGE}"
        ));
    }
    if shadow && options.until.is_some() {
        return Err(format!("--shadow takes no --until: {USAGE}"));
    }
    if counterfactual {
        options.candidate = Some(Candidate { file: policy, sets });
    }
    Ok(ReplayArgs {
        file,
        more,
        options,
        why,
        shadow,
        check: false,
    })
}

fn tape_reply(args: &[String]) -> Result<(String, Option<String>), String> {
    tape_reply_with_limit(args, supervise::tape::recover::HELD_MAX as u64)
}

fn tape_reply_with_limit(
    args: &[String],
    read_limit: u64,
) -> Result<(String, Option<String>), String> {
    const USAGE: &str = "tape cat FILE [--text] [--salvage] | tape show FILE --decisions [--salvage] | tape stats \
         FILE [--salvage] | tape ls [@sid] | tape forget FILE|@sid…";
    let Some((verb, rest)) = args.split_first() else {
        return Err(format!("tape needs a verb: {USAGE}"));
    };
    let verb = verb.as_str();
    if matches!(verb, "ls" | "forget") {
        let state = aterm_types::dirs::state_dir()
            .ok_or_else(|| format!("tape {verb}: no state directory resolves here"))?;
        let store = store::store_under(&state);
        return if verb == "ls" {
            store_ls(&store, rest, std::time::SystemTime::now())
        } else {
            store_forget(&store, rest)
        }
        .map(|text| (text, None));
    }
    if !matches!(verb, "cat" | "show" | "stats") {
        return Err(format!("tape: no verb {verb:?}: {USAGE}"));
    }
    let (mut file, mut as_text, mut salvage, mut decisions) = (None, false, false, false);
    for a in rest {
        match a.as_str() {
            "--text" if verb == "cat" => as_text = true,
            "--decisions" if verb == "show" => decisions = true,
            "--salvage" => salvage = true,
            s if s.starts_with("--") => {
                return Err(format!("tape {verb}: unknown option '{s}': {USAGE}"));
            }
            s if file.is_none() => file = Some(PathBuf::from(s)),
            s => return Err(format!("tape {verb}: one FILE only, not also {s:?}")),
        }
    }
    let Some(file) = file else {
        return Err(format!("tape {verb} needs the tape's FILE: {USAGE}"));
    };
    if verb == "show" && !decisions {
        return Err("tape show needs --decisions".to_string());
    }
    let bytes = read_tape_bytes(&file, read_limit)
        .map_err(|e| format!("tape {verb}: cannot read {}: {e}", file.display()))?;
    let tape = show::open(&bytes, salvage).map_err(|e| format!("tape {}: {e}", file.display()))?;
    let text = match verb {
        "cat" if as_text => supervise::tape::text::render(tape.header.as_deref(), &tape.items),
        "cat" => show::skim(&tape),
        "show" => show::render_decisions(&show::decisions(&tape)),
        _ => show::render_stats(&show::stats(&tape)),
    };
    Ok((text, show::tail_note(&tape)))
}

fn store_ls(
    store: &std::path::Path,
    args: &[String],
    now: std::time::SystemTime,
) -> Result<String, String> {
    let sid = match args {
        [] => None,
        [sid] if sid.starts_with('@') => Some(sid.as_str()),
        _ => return Err("tape ls takes one @sid at most".to_string()),
    };
    let tapes = store::try_list(store, sid).map_err(|e| format!("tape ls: {e}"))?;
    if tapes.is_empty() {
        return Ok(format!("no hosted tapes in {}\n", store.display()));
    }
    let mut out = String::new();
    for t in &tapes {
        let ago = now.duration_since(t.modified).unwrap_or_default().as_secs();
        let ago = match ago {
            s if s < 120 => format!("{s}s"),
            s if s < 7200 => format!("{}m", s / 60),
            s if s < 172_800 => format!("{}h", s / 3600),
            s => format!("{}d", s / 86_400),
        };
        out.push_str(&format!(
            "{:>10}  {ago:>4} ago  {}{}\n",
            t.bytes,
            show::shown(&t.path.display().to_string()),
            if t.kept { "  kept" } else { "" }
        ));
    }
    Ok(out)
}

fn store_forget(store: &std::path::Path, args: &[String]) -> Result<String, String> {
    if args.is_empty() {
        return Err("tape forget needs a FILE or an @sid".to_string());
    }
    let mut paths = Vec::new();
    for a in args {
        if a.starts_with('@') {
            let before = paths.len();
            paths.extend(
                store::try_list(store, Some(a))
                    .map_err(|e| format!("tape forget: {e}"))?
                    .into_iter()
                    .filter(|t| !t.kept)
                    .map(|t| t.path),
            );
            if paths.len() == before {
                return Err(format!(
                    "tape forget: {a} has no tape in {} (`tape ls` lists them)",
                    store.display()
                ));
            }
        } else {
            paths.push(PathBuf::from(a));
        }
    }
    let gone = store::forget(store, &paths).map_err(|e| format!("tape forget: {e}"))?;
    Ok(gone
        .iter()
        .map(|p| format!("forgot {}\n", show::shown(&p.display().to_string())))
        .collect())
}

fn read_tape_bytes(path: &std::path::Path, limit: u64) -> std::io::Result<Vec<u8>> {
    read_regular_bytes(path, limit, "tape")
}

fn read_regular_bytes(path: &std::path::Path, limit: u64, kind: &str) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;

    let (file, meta) = crate::harness::footer::open_regular_with_meta(path)
        .ok_or_else(|| std::io::Error::other("not a readable regular file"))?;
    let overflow = || std::io::Error::other(format!("{kind} input exceeds {limit} bytes"));
    if meta.len() > limit {
        return Err(overflow());
    }
    let lookahead = limit.checked_add(1).ok_or_else(overflow)?;
    let cap = usize::try_from(meta.len().min(limit).saturating_add(1)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(cap);
    file.take(lookahead).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(overflow());
    }
    Ok(bytes)
}

fn harness_policy() -> SupervisorConfig {
    let (policy, notes) =
        SupervisorConfig::from_path(crate::harness::cli::default_config_path().as_deref());
    for note in notes {
        eprintln!("aterm drive: {note}");
    }
    policy
}

#[cfg(test)]
#[path = "drive_cli_tests.rs"]
mod tests;
