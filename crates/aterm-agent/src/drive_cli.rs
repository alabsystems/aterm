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
                    supervise::parse_since(&v, tz_offset_s())
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
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    classify_verb_at(args, home.as_deref())
}

/// [`classify_verb`] under `home`: the classifier's verdict, then the default secret list's.
fn classify_verb_at(args: &[String], home: Option<&std::path::Path>) -> Result<Reply, String> {
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
        supervise::policy::approval::command_reads_no_secret(&cmd, home)
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
    let text = supervise::render_ledger(&ledger, sub.format.unwrap_or_default());
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
    o.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    f.write_all(text.as_bytes())
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
                    .and_then(|n| n.strip_prefix('#').unwrap_or(n).parse::<u64>().ok())
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

    let file = crate::harness::footer::open_regular(path)
        .ok_or_else(|| std::io::Error::other("not a readable regular file"))?;
    let overflow = || std::io::Error::other(format!("{kind} input exceeds {limit} bytes"));
    if file.metadata()?.len() > limit {
        return Err(overflow());
    }
    let lookahead = limit.checked_add(1).ok_or_else(overflow)?;
    let mut bytes = Vec::new();
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
mod tests {
    use super::*;
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn os(v: &[&str]) -> Vec<std::ffi::OsString> {
        v.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn classify_parses_the_command_and_exits_on_the_verdict() {
        let r = classify_verb(&args(&["git status --short && git pull | tail"])).expect("ok");
        assert_eq!(
            r,
            Reply {
                text: "not-read-only git pull\n".to_string(),
                code: 1
            }
        );
        let r = classify_verb(&args(&["git", "log", "-3"])).expect("joined words");
        assert_eq!(
            r,
            Reply {
                text: "read-only\n".to_string(),
                code: 0
            }
        );
        let r = classify_verb(&args(&["git", "log", "--oneline"])).expect("a flag in the command");
        assert_eq!(r.code, 0, "{r:?}");
        let sub = parse_sub("classify", &args(&["git", "log", "--oneline", "-5"])).expect("parses");
        assert_eq!(sub.rest, args(&["git", "log", "--oneline", "-5"]));
        let sub = parse_sub("classify", &args(&["--", "--weird", "x"])).expect("-- ends the flags");
        assert_eq!(sub.rest, args(&["--weird", "x"]));
        let sub = parse_sub("classify", &args(&["@foo", "bar"])).expect("no sid for classify");
        assert_eq!((sub.sid, sub.rest), (None, args(&["@foo", "bar"])));
        let sub = parse_sub("answer", &args(&["@s-1", "--", "--weird"])).expect("parses");
        assert_eq!(
            (sub.sid.as_deref(), sub.rest),
            (Some("@s-1"), args(&["--weird"]))
        );
        let r = classify_verb(&args(&["python3 tools/audit.py"])).expect("ok");
        assert_eq!(r.code, 1);
        let r = classify_verb(&args(&[
            "--allow-python",
            "tools/*.py",
            "python3 tools/audit.py",
        ]))
        .expect("ok");
        assert_eq!(r.code, 0, "{r:?}");
        assert!(classify_verb(&args(&[])).is_err(), "no command");
        let o = parse(vec!["classify".into(), "ls -la".into()]).expect("parses");
        assert_eq!(o.cmd, args(&["classify", "ls -la"]));
        assert_eq!(run(&o).expect("pure verb needs no host").code, 0);
    }

    #[test]
    fn classify_refuses_a_read_of_a_secret_under_the_default_list() {
        let home = Some(std::path::Path::new("/Users/_owner"));
        for line in [
            "cat ~/.ssh/id_rsa",
            "head -3 .env",
            "awk 'NR<3' ~/.aws/credentials",
            "cat /Users/_owner/.claude.json",
            "grep -m 1 key ~/.config/gh/hosts.yml",
            "git show HEAD:.env",
        ] {
            let r = classify_verb_at(&args(&[line]), home).expect("ok");
            assert_eq!(r.code, 1, "{line}: {r:?}");
            assert!(
                r.text.starts_with("not-read-only a read of a secret"),
                "{line}: {r:?}"
            );
        }
        // NEGATIVE CONTROLS
        for line in [
            "grep -rn credentials src/",
            "grep -m 1 credentials src/",
            "wc -l src/credentials.rs",
            "ls -la ~/.ssh",
            "cat README.md",
        ] {
            let r = classify_verb_at(&args(&[line]), home).expect("ok");
            assert_eq!(r, Reply::text("read-only\n".to_string()), "{line}");
        }
        let unknown_home =
            classify_verb_at(&args(&["cat /Users/_owner/.claude.json"]), None).expect("ok");
        assert_eq!(unknown_home.code, 0, "{unknown_home:?}");
        // The classifier's own refusal still comes first.
        let r = classify_verb_at(&args(&["cat ~/.ssh/id_rsa > out"]), home).expect("ok");
        assert!(!r.text.contains("a read of a secret"), "{r:?}");
        assert_eq!(r.code, 1);
    }

    #[test]
    fn phase_ends_with_the_context_left() {
        let rule = "─".repeat(120);
        let indicator = format!("{}1% until auto-compact", " ".repeat(97));
        let body = [
            "⏺ Done.",
            "",
            "✻ Cogitated for 4s · done 2:41 PM",
            "",
            "● How is Claude doing this session? (optional)",
            "  1: Bad    2: Fine   3: Good   0: Dismiss",
            &indicator,
            &rule,
            "❯",
            &rule,
            "  ? for shortcuts",
        ];
        let turn = |rows: Vec<String>| supervise::Turn {
            phase: supervise::worker_phase(&rows),
            screen: supervise::screen::Screen {
                rows,
                ..supervise::screen::Screen::default()
            },
            timed_out: false,
        };
        let claude = aterm_phase::Program::Claude.reader();
        let rows: Vec<String> = body.iter().map(|r| r.to_string()).collect();
        let r = phase_reply(&turn(rows.clone()), claude, &[]);
        assert_eq!(
            (r.text.as_str(), r.code),
            ("idle\nsurvey 0\ncontext 1%\n", 0)
        );
        let no_survey: Vec<String> = rows
            .iter()
            .filter(|r| !r.contains("How is Claude doing") && !r.contains("0: Dismiss"))
            .cloned()
            .collect();
        assert_eq!(
            phase_reply(&turn(no_survey.clone()), claude, &[]).text,
            "idle\ncontext 1%\n"
        );
        let plain: Vec<String> = no_survey
            .into_iter()
            .filter(|r| !r.contains("until auto-compact"))
            .collect();
        assert_eq!(phase_reply(&turn(plain), claude, &[]).text, "idle\n");
    }

    #[test]
    fn a_verb_followed_by_help_asks_for_help_and_never_acts() {
        for verb in ["classify", "phase", "answer", "ledger", "tape", "replay"] {
            for flag in ["--help", "-h"] {
                let o = parse(os(&[verb, flag])).expect("parses");
                assert_eq!(o.cmd, args(&["help"]), "`{verb} {flag}` must ask for help");
                let o = parse(os(&[verb, flag, "more"])).expect("parses");
                assert_eq!(o.cmd, args(&["help"]), "`{verb} {flag} more`");
            }
        }
        let o = parse(os(&["--socket", "/s.sock", "answer", "--help"])).expect("parses");
        assert_eq!(
            (o.socket.as_deref(), o.cmd),
            (Some("/s.sock"), args(&["help"]))
        );
        let o = parse(os(&["answer", "@s-1", "--help"])).expect("parses");
        assert_eq!(o.cmd, args(&["answer", "@s-1", "--help"]));
        let o = parse(os(&["phase", "--help"])).expect("parses");
        let text = match run(&o) {
            Ok(r) => r.text,
            Err(e) => panic!("help must not fail: {e}"),
        };
        assert!(text.contains("aterm drive"), "{text}");
    }

    #[test]
    fn the_one_option_is_the_socket() {
        let o = parse(os(&["--sock", "/run/a.sock", "phase", "@s-1"])).expect("parses");
        assert_eq!(o.socket.as_deref(), Some("/run/a.sock"));
        assert_eq!(o.cmd, args(&["phase", "@s-1"]));
        for gone in ["--dial", "--remote", "--timeout", "--idle", "--ready"] {
            let err = parse(os(&[gone, "x", "phase"])).expect_err(gone);
            assert!(
                err.starts_with(&format!("unknown option '{gone}'")),
                "{err}"
            );
        }
        let err = parse(os(&["--socket"])).expect_err("no path");
        assert_eq!(err, "--socket needs a PATH");
    }

    #[test]
    fn a_removed_verb_is_refused_before_any_host() {
        for verb in [
            "prompt",
            "read",
            "shot",
            "await",
            "await-turn",
            "supervise",
            "watch",
            "task",
            "report",
        ] {
            let o = Opts {
                socket: Some("/nonexistent/aterm.sock".to_string()),
                cmd: args(&[verb, "@s-1", "x"]),
            };
            let err = run(&o).expect_err(verb);
            assert!(
                err.starts_with(&format!("`{verb}` is not an `aterm drive` command; use ")),
                "{err}"
            );
            assert!(gone_to(verb).is_some());
        }
        for word in [
            "classify", "phase", "answer", "ledger", "tape", "replay", "bogus",
        ] {
            assert_eq!(gone_to(word), None, "{word}");
        }
        let err = run(&Opts {
            socket: Some("/nonexistent/aterm.sock".to_string()),
            cmd: args(&["bogus"]),
        })
        .expect_err("not a verb");
        assert!(err.starts_with("unknown command 'bogus'"), "{err}");
    }

    #[test]
    fn the_ledgers_flags_belong_to_it() {
        let sub = parse_sub(
            "ledger",
            &args(&[
                "@s-1",
                "--journal",
                "j.jsonl",
                "--format",
                "md",
                "--out",
                "out.md",
                "--since",
                "1789344000000",
            ]),
        )
        .expect("parses");
        assert!(no_positionals("ledger", &sub).is_ok());
        assert_eq!(
            (
                sub.sid.as_deref(),
                sub.journal.as_deref(),
                sub.format,
                sub.out.as_deref(),
                sub.since_ms
            ),
            (
                Some("@s-1"),
                Some(std::path::Path::new("j.jsonl")),
                Some(LedgerFormat::Md),
                Some(std::path::Path::new("out.md")),
                Some(1_789_344_000_000)
            )
        );
        for (word, want) in [
            ("text", LedgerFormat::Text),
            ("md", LedgerFormat::Md),
            ("markdown", LedgerFormat::Md),
            ("html", LedgerFormat::Html),
        ] {
            let sub = parse_sub("ledger", &args(&["--format", word])).expect(word);
            assert_eq!(sub.format, Some(want), "{word}");
        }
        for bad in ["", "HTML", "pdf", "json"] {
            let err = parse_sub("ledger", &args(&["--format", bad])).expect_err(bad);
            assert_eq!(err, "ledger: --format needs text, md or html", "{bad}");
        }
        let err = parse_sub("ledger", &args(&["--format"])).expect_err("no value");
        assert!(err.contains("--format needs"), "{err}");
        let sub = parse_sub("ledger", &args(&["--since", "2026-09-14T10:30:00Z"])).expect("a time");
        assert_eq!(sub.since_ms, Some(1_789_381_800_000));
        for bad in ["7730:1291", "yesterday", "2026-13-01"] {
            let err = parse_sub("ledger", &args(&["--since", bad])).expect_err(bad);
            assert!(
                err.starts_with("ledger: --since needs a time:"),
                "{bad}: {err}"
            );
        }
        for words in [&["--source", "log"][..], &["--source=log"][..]] {
            let sub = parse_sub("ledger", &args(words)).expect("the one source");
            assert_eq!(sub, parse_sub("ledger", &[]).expect("none"), "{words:?}");
        }
        for bad in [
            &["--source", "clock"][..],
            &["--source="][..],
            &["--source"][..],
        ] {
            let err = parse_sub("ledger", &args(bad)).expect_err("no source");
            assert!(err.starts_with("ledger: --source needs"), "{bad:?}: {err}");
        }
        let sub = parse_sub("answer", &args(&["@s-1", "--box", "q7", "2"])).expect("parses");
        assert_eq!(
            (sub.box_token.as_deref(), sub.rest),
            (Some("q7"), args(&["2"]))
        );
        for (verb, flag, says) in [
            ("phase", "--journal", "--journal is ledger's"),
            ("answer", "--since", "--since is ledger's"),
            ("phase", "--format", "--format is ledger's"),
            ("answer", "--out", "--out is ledger's"),
            ("phase", "--source", "--source is ledger's"),
            ("phase", "--box", "--box is answer's"),
            ("ledger", "--box", "--box is answer's"),
        ] {
            let err = parse_sub(verb, &args(&[flag, "x"])).expect_err(flag);
            assert!(
                err.starts_with(&format!("{verb}: {says}")),
                "{verb} {flag}: {err}"
            );
        }
        for gone in ["--final", "--messages", "--mail", "--max-rows", "--report"] {
            let err = parse_sub("ledger", &args(&[gone])).expect_err(gone);
            assert!(err.contains(&format!("unknown option '{gone}'")), "{err}");
        }
    }

    /// THE REMOVED SOURCE IS A USAGE ERROR: `ledger --source anchor`, either spelling, is
    /// refused before any host with the day it was removed and what to do instead;
    /// `--source log` reads as no flag; NEGATIVE CONTROL: the anchor parsed as a source again
    /// is caught here.
    #[test]
    fn a_removed_source_is_a_usage_error_naming_its_date() {
        for words in [
            &["@s-1", "--source", "anchor"][..],
            &["@s-1", "--source=anchor"][..],
        ] {
            let err = parse_sub("ledger", &args(words)).expect_err("removed");
            assert_eq!(
                err,
                format!(
                    "ledger: --source anchor was removed on {ANCHOR_REMOVED}: the session log \
                     places every time; leave the flag out"
                ),
                "{words:?}"
            );
        }
        let err = ledger_verb(
            &Opts {
                socket: Some("/nonexistent/aterm.sock".to_string()),
                cmd: args(&["ledger"]),
            },
            &args(&["@s-1", "--source", "anchor"]),
        )
        .expect_err("refused before any host");
        assert!(err.contains("was removed on"), "{err}");
        assert!(
            DRIVE_HELP.contains(&format!("was removed on {ANCHOR_REMOVED}.")),
            "the help names the same day"
        );
        let sub = parse_sub("ledger", &args(&["@s-1", "--source", "log"])).expect("parses");
        assert_eq!(
            ledger_opts(&sub, "@s-1".to_string()),
            ledger_opts(
                &parse_sub("ledger", &args(&["@s-1"])).expect("parses"),
                "@s-1".to_string()
            )
        );
    }

    #[test]
    fn the_ledger_takes_its_worker_from_the_journal_when_none_is_named() {
        let dir = std::env::temp_dir().join(format!("aterm-drive-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("journal.jsonl");
        let line = |sid: &str| {
            crate::supervise::journal::JournalRecord::of_line(1, Some(sid), "TIMEOUT", None)
                .to_json()
        };
        std::fs::write(&path, format!("{}\n{}\n", line("s-work"), line("@s-work"))).expect("write");
        assert_eq!(journal_sid(Some(&path)).as_deref(), Some("s-work"));
        std::fs::write(&path, format!("{}\n{}\n", line("s-work"), line("s-other"))).expect("write");
        assert_eq!(journal_sid(Some(&path)), None);
        assert_eq!(journal_sid(None), None);
        assert_eq!(journal_sid(Some(&dir.join("nothing.jsonl"))), None);
        let err = ledger_verb(
            &Opts {
                socket: None,
                cmd: vec!["ledger".to_string()],
            },
            &args(&["--journal", &path.to_string_lossy()]),
        )
        .expect_err("no worker");
        assert!(err.starts_with("ledger needs the worker:"), "{err}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(dir);
    }

    /// `tape ls` lists the hosted store newest first, a session's alone with
    /// `@sid` (the kept ones beside it), and says an empty store; `tape
    /// forget` removes the tapes named — a FILE, or a session's every tape
    /// but its kept ones — and refuses anything that is no tape in the store
    /// before a tape goes.
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    #[test]
    fn tape_ls_lists_the_store_and_forget_trims_it() {
        use std::os::unix::fs::DirBuilderExt as _;
        use std::time::{Duration, SystemTime};
        let guard = crate::supervise::ScratchDir::new("drive-tape", "store");
        let state = guard.canonicalize().expect("owned canonical fixture");
        let store = store::store_under(&state);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(state.join("drive"))
            .expect("private drive");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&store)
            .expect("private store");
        let now = SystemTime::now();
        assert!(
            store_ls(&store, &[], now)
                .expect("lists")
                .starts_with("no hosted tapes in "),
            "an empty store says so"
        );
        let put = |dir: &str, name: &str, ago: u64| {
            let path = store.join(dir).join(name);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(path.parent().expect("dir"))
                .expect("private session directory");
            use std::io::Write as _;
            store::create(&path)
                .expect("exclusive tape")
                .write_all(b"tape")
                .expect("write");
            std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("open")
                .set_modified(now - Duration::from_secs(ago))
                .expect("mtime");
            path
        };
        let a = put("s-1", "a.atape", 30);
        let b = put("s-2", "b.atape", 7200);
        let k = put(store::KEPT, "k.atape", 4 * 86_400);
        let all = store_ls(&store, &[], now).expect("lists");
        let lines: Vec<&str> = all.lines().collect();
        assert_eq!(lines.len(), 3, "{all}");
        assert!(
            lines[0].contains("30s ago") && lines[0].ends_with("a.atape"),
            "{all}"
        );
        assert!(
            lines[1].contains("2h ago") && lines[1].ends_with("b.atape"),
            "{all}"
        );
        assert!(
            lines[2].contains("4d ago") && lines[2].ends_with("k.atape  kept"),
            "{all}"
        );
        let one = store_ls(&store, &args(&["@s-2"]), now).expect("lists");
        assert_eq!(one.lines().count(), 2, "s-2's and the kept one: {one}");
        assert!(
            store_ls(&store, &args(&["s-2"]), now).is_err(),
            "a session is @sid"
        );

        let outside = state.join("elsewhere.atape");
        std::fs::write(&outside, b"tape").expect("write");
        let err = store_forget(
            &store,
            &args(&[a.to_str().expect("utf-8"), "/nonexistent.atape"]),
        )
        .expect_err("refused");
        assert!(err.contains("no tape"), "{err}");
        assert!(a.exists(), "nothing went before the refusal");
        assert!(store_forget(&store, &args(&[outside.to_str().expect("utf-8")])).is_err());
        assert!(store_forget(&store, &[]).is_err());
        let err = store_forget(&store, &args(&["@s-12", a.to_str().expect("utf-8")]))
            .expect_err("refused");
        assert!(err.contains("@s-12 has no tape"), "{err}");
        assert!(a.exists(), "nothing went before the refusal");
        let gone =
            store_forget(&store, &args(&["@s-1", b.to_str().expect("utf-8")])).expect("forgotten");
        assert_eq!(gone.lines().count(), 2, "{gone}");
        assert!(!a.exists() && !b.exists());
        assert!(k.exists(), "a kept tape goes only by name");
        assert!(outside.exists());
    }

    #[test]
    fn tape_reads_only_its_file_and_refuses_with_two() {
        use crate::supervise::tape::codec::{End, Item};
        use crate::supervise::tape::frame::TapeWriter;
        let dir = crate::supervise::ScratchDir::new("drive-tape", "verb");
        let path = dir.join("run.atape");
        {
            let file = store::create(&path).expect("made");
            let mut w = TapeWriter::start(
                Box::new(file),
                "run.atape",
                "atape 1\ntape=t\n",
                Box::new(std::io::sink()),
            );
            w.append(&Item::End(End::Returned {
                how: "exit 0".to_string(),
            }));
        }
        let file = path.to_string_lossy().into_owned();
        for (line, says) in [
            (vec!["cat", file.as_str()], "end: returned exit 0"),
            (
                vec!["cat", file.as_str(), "--text"],
                "atape-text 1\nheader\n",
            ),
            (vec!["show", file.as_str(), "--decisions"], "no decisions"),
            (vec!["stats", file.as_str()], "end returned exit 0"),
        ] {
            let reply = tape_verb(&args(&line));
            assert_eq!(reply.code, 0, "{line:?}");
            assert!(reply.text.contains(says), "{line:?}: {}", reply.text);
        }
        for line in [
            vec![],
            vec!["play", file.as_str()],
            vec!["cat"],
            vec!["show", file.as_str()],
            vec!["cat", file.as_str(), "--decisions"],
            vec!["cat", file.as_str(), file.as_str()],
            vec!["stats", "/nonexistent/run.atape"],
        ] {
            assert_eq!(tape_verb(&args(&line)).code, 2, "{line:?}");
        }
        std::fs::write(dir.join("not.atape"), b"hello").expect("write");
        let not = dir.join("not.atape").to_string_lossy().into_owned();
        assert_eq!(tape_verb(&args(&["cat", &not])).code, 2, "no tape");
    }

    #[test]
    fn replay_reads_only_its_file_and_refuses_with_two() {
        use crate::supervise::tape::codec::{End, Item};
        use crate::supervise::tape::frame::TapeWriter;
        let dir = crate::supervise::ScratchDir::new("drive-replay", "verb");
        let path = dir.join("run.atape");
        {
            let file = store::create(&path).expect("made");
            let mut w = TapeWriter::start(
                Box::new(file),
                "run.atape",
                "not a header\n",
                Box::new(std::io::sink()),
            );
            w.append(&Item::End(End::Returned {
                how: "exit 0".to_string(),
            }));
        }
        let file = path.to_string_lossy().into_owned();
        for line in [
            vec![],
            vec!["--until", "door=3"],
            vec![file.as_str(), "--until"],
            vec![file.as_str(), "--until", "3"],
            vec![file.as_str(), "--until", "door=x"],
            vec![file.as_str(), "--fast"],
            vec![file.as_str(), file.as_str()],
            vec!["/nonexistent/run.atape"],
            vec![file.as_str()],
        ] {
            assert_eq!(replay_verb(&args(&line)).code, 2, "{line:?}");
        }
        for (until, door) in [
            ("door=7", 7),
            ("door=#7", 7),
            ("door=#0", 0),
            ("door=12", 12),
        ] {
            let ReplayArgs {
                file: named,
                options,
                why,
                ..
            } = replay_args(&args(&["--until", until, file.as_str()])).expect("parses");
            assert_eq!(named, path, "{until}");
            assert!(why.is_none(), "{until}");
            assert_eq!(options.until, Some(door), "{until}");
            assert_eq!(options.base, None, "{until}");
            let err = replay_reply(&args(&[file.as_str(), "--until", until]))
                .expect_err("no loop to rebuild");
            assert!(err.contains("not a tape header"), "{until}: {err}");
        }
        let ReplayArgs { options, why, .. } = replay_args(&args(&[file.as_str()])).expect("parses");
        assert_eq!(options.until, None, "no --until, no stop");
        assert!(why.is_none(), "no --why, a replay");
        for bad in ["door=#", "door=-1", "door=#x", "#7", "door= 7"] {
            assert!(
                replay_args(&args(&[file.as_str(), "--until", bad])).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn why_is_an_option_of_replay_and_refuses_with_two() {
        use crate::supervise::tape::codec::{End, Item};
        use crate::supervise::tape::frame::TapeWriter;
        let dir = crate::supervise::ScratchDir::new("drive-replay", "why");
        let path = dir.join("run.atape");
        {
            let file = store::create(&path).expect("made");
            let mut w = TapeWriter::start(
                Box::new(file),
                "run.atape",
                "not a header\n",
                Box::new(std::io::sink()),
            );
            w.append(&Item::End(End::Returned {
                how: "exit 0".to_string(),
            }));
        }
        let file = path.to_string_lossy().into_owned();
        for line in [
            vec![file.as_str(), "--why"],
            vec![file.as_str(), "--at", "seq=1"],
            vec![file.as_str(), "--format", "md"],
            vec![file.as_str(), "--why", "--at"],
            vec![file.as_str(), "--why", "--at", "seq=x"],
            vec![file.as_str(), "--why", "--at", "seq=1", "--format"],
            vec![file.as_str(), "--why", "--at", "seq=1", "--format", "pdf"],
            vec![file.as_str(), "--why", "--at", "seq=1", "--until", "door=3"],
            vec!["--why", "--at", "seq=1"],
            vec!["/nonexistent/run.atape", "--why", "--at", "seq=1"],
            vec![file.as_str(), "--why", "--at", "seq=1"],
        ] {
            let reply = replay_verb(&args(&line));
            assert_eq!(reply.code, 2, "{line:?}");
            assert!(reply.text.is_empty(), "{line:?}");
        }
        let ReplayArgs { why, options, .. } = replay_args(&args(&[
            "--why",
            "--at",
            "door=#17",
            file.as_str(),
            "--format",
            "html",
        ]))
        .expect("parses");
        assert_eq!(
            why,
            Some((explain::At::Door(17), explain::Format::Html)),
            "the point and the rendering"
        );
        assert_eq!(options.until, None);
        let ReplayArgs { why, .. } =
            replay_args(&args(&[file.as_str(), "--why", "--at", "seq=3"])).expect("parses");
        assert_eq!(why, Some((explain::At::Seq(3), explain::Format::Text)));
        let err = replay_reply(&args(&[file.as_str(), "--why", "--at", "door=3"]))
            .expect_err("no decision");
        assert!(err.contains("no decisions"), "{err}");
        let err = run(&Opts {
            socket: None,
            cmd: args(&["why", file.as_str(), "--at", "seq=1"]),
        })
        .expect_err("no `why` verb");
        assert!(err.contains("unknown command 'why'"), "{err}");
    }

    #[test]
    fn counterfactuals_are_options_of_replay_and_refuse_with_two() {
        use crate::supervise::tape::codec::{End, Item};
        use crate::supervise::tape::frame::TapeWriter;
        let dir = crate::supervise::ScratchDir::new("drive-replay", "counterfactual");
        let path = dir.join("run.atape");
        {
            let file = store::create(&path).expect("made");
            let mut w = TapeWriter::start(
                Box::new(file),
                "run.atape",
                "not a header\n",
                Box::new(std::io::sink()),
            );
            w.append(&Item::End(End::Returned {
                how: "exit 0".to_string(),
            }));
        }
        let file = path.to_string_lossy().into_owned();
        let policy = dir.join("aterm.toml");
        std::fs::write(&policy, "[harness]\napprove = \"none\"\n").expect("written");
        let policy = policy.to_string_lossy().into_owned();
        let typo = dir.join("typo.toml");
        std::fs::write(&typo, "[harness]\napprove = \"saf\"\n").expect("written");
        let typo = typo.to_string_lossy().into_owned();
        let absent = dir.join("absent.toml");
        let absent = absent.to_string_lossy().into_owned();
        let len =
            u64::try_from(b"[harness]\napprove = \"none\"\n".len()).expect("small policy fixture");
        let (_, exact) = candidate_file_with_limit(&policy, len).expect("exact policy limit");
        assert_eq!(exact.approve, supervise::config::Approve::None);
        let over = candidate_file_with_limit(&policy, len - 1).expect_err("one byte over");
        assert!(over.contains("exceeds"), "{over}");
        let directory = dir.to_string_lossy().into_owned();
        let kind = candidate_file(&directory).expect_err("a directory is not policy input");
        assert!(kind.contains("not a readable regular file"), "{kind}");
        let non_utf8 = dir.join("non-utf8.toml");
        std::fs::write(&non_utf8, b"[harness]\n# \xff\n").expect("owned non-UTF8 policy");
        assert!(
            candidate_file(&non_utf8.to_string_lossy())
                .expect_err("the policy bytes must be UTF-8")
                .contains("not UTF-8")
        );
        let controls = dir.join("controls.toml");
        std::fs::write(
            &controls,
            "[harness]\n\"\\u001b]52;c;YQ==\\u0007\" = true\n",
        )
        .expect("owned policy with a control-bearing unknown key");
        let refusal = candidate_file(&controls.to_string_lossy()).expect_err("unknown policy key");
        assert!(!refusal.chars().any(char::is_control), "{refusal:?}");
        for line in [
            vec![file.as_str(), "--set"],
            vec![file.as_str(), "--set", "approve"],
            vec![file.as_str(), "--set", "=safe"],
            vec![file.as_str(), "--policy"],
            vec![file.as_str(), "--policy", absent.as_str()],
            vec![
                file.as_str(),
                "--policy",
                policy.as_str(),
                "--policy",
                policy.as_str(),
            ],
            vec![file.as_str(), file.as_str(), "--set", "approve=safe"],
            vec![file.as_str(), "--shadow", "--until", "door=3"],
            vec![
                file.as_str(),
                "--why",
                "--at",
                "seq=1",
                "--set",
                "approve=safe",
            ],
            vec![file.as_str(), "--why", "--at", "seq=1", "--shadow"],
            vec![file.as_str(), "--set", "approve=safe"],
            vec![file.as_str(), "--shadow"],
        ] {
            let reply = replay_verb(&args(&line));
            assert_eq!(reply.code, 2, "{line:?}");
            assert!(reply.text.is_empty(), "{line:?}");
        }
        let reply = replay_verb(&args(&[file.as_str(), file.as_str(), "--shadow"]));
        assert_eq!(reply.code, 2, "{}", reply.text);
        assert_eq!(
            reply.text.matches("not shadowed: ").count(),
            2,
            "{}",
            reply.text
        );
        let ReplayArgs {
            options, shadow, ..
        } = replay_args(&args(&[
            file.as_str(),
            "--set",
            "harness.approve=safe",
            "--set",
            "continue=false",
            "--until",
            "door=9",
        ]))
        .expect("parses");
        assert!(!shadow);
        assert_eq!(options.until, Some(9));
        let candidate = options.candidate.expect("a counterfactual");
        assert_eq!(
            candidate.sets,
            [
                ("harness.approve".to_string(), "safe".to_string()),
                ("continue".to_string(), "false".to_string())
            ]
        );
        assert!(candidate.file.is_none());
        let ReplayArgs {
            more,
            options,
            shadow,
            ..
        } = replay_args(&args(&[
            "--shadow",
            file.as_str(),
            file.as_str(),
            "--policy",
            policy.as_str(),
        ]))
        .expect("parses");
        assert!(shadow);
        assert_eq!(more.len(), 1, "two tapes shadowed");
        let (named, read) = options
            .candidate
            .expect("a candidate")
            .file
            .expect("the policy file");
        assert_eq!(named, policy);
        assert_eq!(read.approve, supervise::config::Approve::None);
        for line in [
            vec![file.as_str(), "--policy", typo.as_str()],
            vec![file.as_str(), "--shadow", "--policy", typo.as_str()],
        ] {
            let err = replay_args(&args(&line)).expect_err("a policy the reader refuses");
            assert!(
                err.contains("harness.approve") && err.contains("saf"),
                "{err}"
            );
        }
        let ReplayArgs { options, .. } = replay_args(&args(&[file.as_str()])).expect("parses");
        assert!(options.candidate.is_none(), "no candidate, a strict replay");
        let err = run(&Opts {
            socket: None,
            cmd: args(&["shadow", file.as_str()]),
        })
        .expect_err("no `shadow` verb");
        assert!(err.contains("unknown command 'shadow'"), "{err}");
    }

    #[test]
    fn a_corpus_check_is_an_option_of_replay() {
        let dir = crate::supervise::ScratchDir::new("drive-replay", "check");
        std::fs::create_dir_all(dir.join("empty")).expect("made");
        std::fs::create_dir_all(dir.join("corpus").join("deep")).expect("made");
        let bad = dir.join("corpus").join("deep").join("bad.atape.txt");
        std::fs::write(&bad, "atape-text 1\nnot a record\n").expect("written");
        let corpus = dir.join("corpus").to_string_lossy().into_owned();
        let empty = dir.join("empty").to_string_lossy().into_owned();
        for line in [
            vec!["--check"],
            vec!["--check", corpus.as_str(), "--shadow"],
            vec!["--check", corpus.as_str(), "--until", "door=3"],
            vec!["--check", corpus.as_str(), "--why", "--at", "seq=1"],
            vec!["--check", empty.as_str()],
            vec!["--check", "/nonexistent/corpus"],
        ] {
            let reply = replay_verb(&args(&line));
            assert_eq!(reply.code, 2, "{line:?}");
            assert!(reply.text.is_empty(), "{line:?}");
        }
        let reply = replay_verb(&args(&["--check", corpus.as_str()]));
        assert_eq!(reply.code, 2, "{}", reply.text);
        assert!(
            reply.text.starts_with("NOT REPLAYED ") && reply.text.contains("bad.atape.txt"),
            "{}",
            reply.text
        );
        assert!(
            reply.text.ends_with("1 tape(s): 1 NOT REPLAYED\n"),
            "{}",
            reply.text
        );
        let ReplayArgs {
            file,
            options,
            check,
            ..
        } = replay_args(&args(&[
            "--check",
            corpus.as_str(),
            "--set",
            "approve=safe",
        ]))
        .expect("parses");
        assert!(check);
        assert_eq!(file, PathBuf::from(&corpus));
        assert_eq!(
            options.candidate.expect("a candidate").sets,
            [("approve".to_string(), "safe".to_string())]
        );
        let err = run(&Opts {
            socket: None,
            cmd: args(&["check", corpus.as_str()]),
        })
        .expect_err("no `check` verb");
        assert!(err.contains("unknown command 'check'"), "{err}");
    }

    /// A replay reads its tape as the `tape` verbs do: a regular file at
    /// most the read budget long — one byte over is refused, never replayed
    /// as a shorter tape — and a directory or a FIFO refused at once (a FIFO
    /// with no writer would park a blocking open for ever: the one-minute
    /// wait below is the hang detector), each exit 2.
    #[cfg(unix)]
    #[test]
    fn replay_reads_its_tape_bounded_and_regular() {
        use crate::supervise::tape::codec::{End, Item};
        use crate::supervise::tape::frame::TapeWriter;
        let dir = crate::supervise::ScratchDir::new("drive-replay", "bounded");
        let path = dir.join("run.atape");
        {
            let file = store::create(&path).expect("made");
            let mut w = TapeWriter::start(
                Box::new(file),
                "run.atape",
                "not a header\n",
                Box::new(std::io::sink()),
            );
            w.append(&Item::End(End::Returned {
                how: "exit 0".to_string(),
            }));
        }
        let len = std::fs::metadata(&path).expect("the tape").len();
        let file = path.to_string_lossy().into_owned();
        let err = replay_reply_with_limit(&args(&[file.as_str()]), len).expect_err("no header");
        assert!(err.contains("not a tape header"), "read whole: {err}");
        let err = replay_reply_with_limit(&args(&[file.as_str()]), len - 1).expect_err("over");
        assert!(err.contains("exceeds"), "one byte over its budget: {err}");

        let directory = dir.to_string_lossy().into_owned();
        let err = replay_reply(&args(&[directory.as_str()])).expect_err("a directory");
        assert!(err.contains("not a readable regular file"), "{err}");

        use std::os::unix::ffi::OsStrExt as _;
        let fifo = dir.join("run.fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("a C path");
        // SAFETY: one NUL-terminated, owned scratch path; no pointer is kept.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let fifo = fifo.to_string_lossy().into_owned();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let tape_code = replay_verb(&args(&[fifo.as_str()])).code;
            let policy_refused = candidate_file(&fifo).is_err();
            let _ = tx.send((tape_code, policy_refused));
        });
        let refused = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the tape/policy FIFO admission did not come back: it blocked at the open");
        assert_eq!(refused, (2, true));
    }

    #[test]
    fn a_replay_refusal_shows_the_tapes_control_bytes_as_text() {
        use crate::supervise::tape::codec::{End, Item};
        use crate::supervise::tape::frame::TapeWriter;
        const OSC: &str = "\x1b]52;c;cHduZWQ=\x07\x1b]0;title\x07";
        let dir = crate::supervise::ScratchDir::new("drive-replay", "shown");
        let twice = dir.join("twice.atape");
        {
            let file = store::create(&twice).expect("made");
            let mut w = TapeWriter::start(
                Box::new(file),
                "twice.atape",
                &format!("atape 1\n[{OSC}]\nk\nk\n"),
                Box::new(std::io::sink()),
            );
            w.append(&Item::End(End::Returned {
                how: "exit 0".to_string(),
            }));
        }
        let file = twice.to_string_lossy().into_owned();
        let err = replay_reply(&args(&[file.as_str()])).expect_err("refused");
        assert!(err.contains("said twice"), "{err}");
        assert!(
            err.contains("\\x1b]52;c;"),
            "the sequence said as text: {err}"
        );
        assert!(
            !err.chars().any(|c| c.is_control()),
            "no control byte reaches the terminal: {err:?}"
        );
    }

    /// A tape reader admits ordinary binary and text files, including a link
    /// to one, at the exact input limit; an overflow is refused, never parsed
    /// as a shorter tape.
    #[cfg(unix)]
    #[test]
    fn tape_file_reads_refuse_overflow_and_keep_regular_symlink_inputs() {
        use crate::supervise::tape::codec::{End, Item};
        use crate::supervise::tape::frame::TapeWriter;
        use crate::supervise::tape::frame_tests::{Memory, Said};

        let dir = crate::supervise::ScratchDir::new("tape-read", "bounded");
        let end = Item::End(End::Returned {
            how: "exit 0".to_string(),
        });
        let text = crate::supervise::tape::text::render(None, std::slice::from_ref(&end));
        let sink = Memory::default();
        let mut writer = TapeWriter::start(
            Box::new(sink.clone()),
            "memory.atape",
            "atape 1\n",
            Box::new(Said::default()),
        );
        writer.append(&end);
        drop(writer);
        let files = [
            ("text.atape", text.into_bytes()),
            ("binary.atape", sink.bytes()),
        ];
        let mut oversized_admissions = Vec::new();
        for (name, bytes) in &files {
            let file = dir.join(name);
            std::fs::write(&file, bytes).expect("owned tape file");
            let cap = u64::try_from(bytes.len()).expect("small fixture");
            let words = vec![
                "cat".to_string(),
                file.to_string_lossy().into_owned(),
                "--text".to_string(),
            ];
            let (rendered, note) = tape_reply_with_limit(&words, cap).expect("exact-limit tape");
            assert!(rendered.contains("exit 0"));
            assert!(note.is_none());
            let link = dir.join(format!("{name}.link"));
            std::os::unix::fs::symlink(&file, &link).expect("owned regular-file link");
            let linked = vec![
                "cat".to_string(),
                link.to_string_lossy().into_owned(),
                "--text".to_string(),
            ];
            assert_eq!(
                tape_reply_with_limit(&linked, cap).expect("regular-file symlink"),
                (rendered, note)
            );
            if tape_reply_with_limit(&words, cap - 1).is_ok() {
                oversized_admissions.push(*name);
            }
            assert_eq!(std::fs::read(&file).expect("original remains"), *bytes);
        }
        let directory = vec!["cat".to_string(), dir.to_string_lossy().into_owned()];
        assert!(tape_reply_with_limit(&directory, 1024).is_err());
        assert!(
            oversized_admissions.is_empty(),
            "raw tape input over its read limit was admitted: {oversized_admissions:?}"
        );
    }
}
