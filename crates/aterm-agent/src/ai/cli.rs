// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE COMMAND: `aterm ai` (design §7–§9).
//!
//! * `aterm ai` — the doctor, read-only: one row per AI CLI and the health. Exit 0.
//! * `aterm ai doctor [--read-only] [--json] [--on <host>,… | --fleet]` — looks, then on a
//!   terminal a person is at signs in each CLI that needs it through the vendor's own flow,
//!   one after another, and looks again. Exit 1 when a wanted, measured CLI still needs a
//!   person.
//! * `aterm ai login <id>` — one CLI's sign-in.
//! * `aterm ai review […]` and `aterm ai review --show <id>` — reviews across AIs.

use std::ffi::OsString;
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use aterm_json::{Map, Number, Value};

use super::change::{self, Base};
use super::config::{self, FleetState, Settings};
use super::connector::{self, Connector};
use super::facts::{self, Facts, Job, LiveRunner, Machine, Runner};
use super::login::{self, Done};
use super::review;
use super::state::{self, Mark, Row, State};
use super::store::Store;

/// The usage text.
pub const USAGE: &str = "\
aterm ai — this machine's AI CLIs (claude, codex, agy, muse): one working login each,
a doctor that signs in what needs it, and reviews one AI asks of another.

  aterm ai                      the doctor, read-only (exit 0)
  aterm ai doctor [--read-only] [--json] [--on <host>,… | --fleet]
                                look, then on your terminal sign in each CLI that needs it,
                                through its vendor's own flow; --on and --fleet do the same
                                on other machines over ssh ([harness] hosts)
  aterm ai login <cli>          one CLI's sign-in
  aterm ai review [--with <cli>,… | --all] [--from <cli>] [--base <ref> | --uncommitted]
                  [--timeout <s>] [--json] [--] [<what to look for> | -]
                                hand this directory's change to the CLIs listed in
                                [ai] reviewers, read-only, and print their answers
  aterm ai review --show <id> [--json]
                                a review's answers again (kept a day)

Settings ▸ Harness ▸ AI CLIs, and `aterm help ai`, say more. Configuration: [ai] in
aterm.toml, and the fleet document (ai-fleet.toml beside it).";

/// Everything the verbs read of the process, once.
#[derive(Debug, Clone)]
pub struct Env {
    /// Unix seconds at the start.
    pub now: i64,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// The whole environment.
    pub vars: Vec<(String, String)>,
    /// `aterm.toml`.
    pub config: Option<PathBuf>,
    /// aterm's state root.
    pub aterm_state: Option<PathBuf>,
    /// The harness state root (accounts v2's store is under it).
    pub harness_state: Option<PathBuf>,
    /// atpkg's prefix.
    pub pkg: Option<atpkg::store::Layout>,
    /// `[packages] exclude`.
    pub exclude: Vec<String>,
    /// The working directory.
    pub cwd: PathBuf,
    /// Whether stdin is a terminal.
    pub stdin_tty: bool,
    /// This program.
    pub exe: Option<PathBuf>,
}

impl Env {
    /// The process's own.
    #[must_use]
    pub fn from_process() -> Env {
        use std::io::IsTerminal as _;
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute());
        let aterm_state = crate::operator::default_state_root().ok();
        let harness_state = match std::env::var_os("ATERM_HARNESS_STATE") {
            Some(dir) if Path::new(&dir).is_absolute() => Some(PathBuf::from(dir)),
            Some(_) => None,
            None => aterm_state
                .as_ref()
                .map(|s| s.join("harness").join(crate::harness::cli::HARNESS)),
        };
        Env {
            now: crate::supervise::limit::unix_now(),
            home,
            vars,
            config: crate::harness::cli::default_config_path(),
            aterm_state,
            harness_state,
            pkg: atpkg::store::resolve_configured(),
            exclude: atpkg::config::load().exclude().to_vec(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            stdin_tty: io::stdin().is_terminal(),
            exe: std::env::current_exe().ok(),
        }
    }

    /// One variable.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<String> {
        self.vars
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// The AI store.
    #[must_use]
    pub fn store(&self) -> Option<Store> {
        self.aterm_state.as_deref().map(Store::at)
    }
}

/// One look at every CLI.
#[derive(Debug, Clone)]
pub struct Look {
    /// The configuration, resolved.
    pub settings: Settings,
    /// The facts, in `VENDORS` order.
    pub facts: Vec<Facts>,
    /// The rows, the preferred CLI first.
    pub rows: Vec<Row>,
    /// Why the AI store's record was not used, if it was not.
    pub store_note: Option<String>,
}

/// Looks at every CLI.
#[must_use]
pub fn look(env: &Env) -> Look {
    let settings = config::load(env.config.as_deref());
    let store = env.store();
    let (records, store_note) = match store.as_ref().map(Store::load) {
        Some(Ok(r)) => (r, None),
        Some(Err(why)) => (Default::default(), Some(format!("{why:?}"))),
        None => (
            Default::default(),
            Some("aterm's state directory is unknown".to_string()),
        ),
    };
    let var = |k: &str| env.var(k);
    let machine = Machine {
        now: env.now,
        home: env.home.clone(),
        var: &var,
        pkg: env.pkg.clone(),
        exclude: env.exclude.clone(),
        records,
        harness_state: env.harness_state.clone(),
        roster: env
            .config
            .as_deref()
            .and_then(crate::harness::accounts::roster_beside),
    };
    let facts: Vec<Facts> = connector::connections()
        .map(|c| facts::gather(&machine, c))
        .collect();
    let mut settings = settings;
    if let Some(why) = facts
        .iter()
        .find_map(|f| f.accounts.as_ref().and_then(|a| a.refused.clone()))
    {
        settings.notes.push(format!(
            "accounts.toml is refused ({why}), so Claude reads as having no roster; \
             `aterm harness accounts` says how to fix it"
        ));
    }
    let mut rows: Vec<Row> = facts
        .iter()
        .map(|f| state::assess(f, settings.wants(f.connector.id), env.now))
        .collect();
    let preferred = settings.preferred.0.id;
    rows.sort_by_key(|r| r.connector.id != preferred);
    Look {
        settings,
        facts,
        rows,
        store_note,
    }
}

const LABEL_W: usize = 12;
/// The most of what the asker wants checked, read from stdin (`review -`).
const WORDS_MAX: usize = 16 * 1024;
const STATE_W: usize = 15;

fn pad(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}

/// The doctor's text.
pub fn render_text(out: &mut dyn Write, look: &Look, fixes: bool, next: &str) {
    let s = &look.settings;
    let from = |f: config::From| match f {
        config::From::Default => String::new(),
        other => format!(" ({})", other.word()),
    };
    let _ = writeln!(
        out,
        "{}{}{}",
        pad("preferred:", LABEL_W),
        s.preferred.0.id,
        from(s.preferred.1)
    );
    for row in &look.rows {
        let fact = review::sanitize_block(&row.fact, 400).replace('\n', " ");
        let _ = writeln!(
            out,
            "{}{}{fact}",
            pad(&format!("{}:", row.connector.id), LABEL_W),
            pad(row.state.word(), STATE_W)
        );
        if fixes
            && row.mark >= Mark::Warn
            && let Some(fix) = &row.fix
        {
            let _ = writeln!(out, "{}fix: {fix}", " ".repeat(LABEL_W));
        }
    }
    let reviewers: Vec<&str> = s.reviewers.0.iter().map(|c| c.id).collect();
    if reviewers.is_empty() {
        let _ = writeln!(
            out,
            "{}off: list in [ai] reviewers the CLIs whose vendors may receive this machine's code",
            pad("reviews:", LABEL_W)
        );
    } else {
        let _ = writeln!(
            out,
            "{}{}{} · {} per review · {} s",
            pad("reviews:", LABEL_W),
            reviewers.join(", "),
            from(s.reviewers.1),
            s.review_count.0,
            s.review_timeout_s.0
        );
    }
    let fleet = match &s.fleet {
        FleetState::None => "none".to_string(),
        FleetState::Read { path, row } => format!(
            "{}{} in {}",
            s.machine.as_deref().unwrap_or("no [ai] machine"),
            if *row { "" } else { " (no row of its own)" },
            path.display()
        ),
        FleetState::Refused { path, why } => format!("refused ({why}): {}", path.display()),
    };
    let _ = writeln!(out, "{}{fleet}", pad("fleet:", LABEL_W));
    for note in s.notes.iter().chain(look.store_note.iter()) {
        let _ = writeln!(
            out,
            "{}{}",
            pad("note:", LABEL_W),
            review::sanitize_block(note, 300).replace('\n', " ")
        );
    }
    let h = state::health(&look.rows);
    let _ = writeln!(out, "\nhealth: {}", h.line(next));
}

fn int(n: i64) -> Value {
    Value::Number(Number::from(n))
}

fn opt_int(n: Option<i64>) -> Value {
    n.map_or(Value::Null, int)
}

fn rfc(n: Option<i64>) -> Value {
    n.and_then(|t| u64::try_from(t).ok())
        .map_or(Value::Null, |t| {
            Value::from(aterm_types::rfc3339::format_rfc3339(t))
        })
}

/// The doctor's one JSON object.
#[must_use]
pub fn render_json(look: &Look, now: i64, next: &str) -> String {
    let s = &look.settings;
    let mut clis = Vec::new();
    for row in &look.rows {
        let use_from = s
            .uses
            .iter()
            .find(|(c, _, _)| c.id == row.connector.id)
            .map_or("default", |(_, _, f)| f.word());
        let mut o = Map::new();
        let text = |v: &Option<String>| v.as_ref().map_or(Value::Null, |s| Value::from(s.as_str()));
        o.insert("id".to_string(), Value::from(row.connector.id));
        o.insert("product".to_string(), Value::from(row.connector.product));
        o.insert("use".to_string(), Value::Bool(row.wanted));
        o.insert("use_from".to_string(), Value::from(use_from));
        o.insert("state".to_string(), Value::from(row.state.word()));
        o.insert("mark".to_string(), Value::from(row.mark.word()));
        o.insert("measured".to_string(), Value::Bool(row.connector.measured));
        o.insert("version".to_string(), text(&row.version));
        o.insert("plan".to_string(), text(&row.plan));
        o.insert("used_at".to_string(), opt_int(row.used.map(|u| u.0)));
        o.insert("used_at_rfc3339".to_string(), rfc(row.used.map(|u| u.0)));
        o.insert(
            "used_how".to_string(),
            row.used.map_or(Value::Null, |u| Value::from(u.1)),
        );
        o.insert("renew_by".to_string(), opt_int(row.renew_by));
        o.insert("renew_by_rfc3339".to_string(), rfc(row.renew_by));
        o.insert("limited_until".to_string(), opt_int(row.limited_until));
        o.insert("limited_until_rfc3339".to_string(), rfc(row.limited_until));
        o.insert("fact".to_string(), Value::from(row.fact.as_str()));
        o.insert("fix".to_string(), text(&row.fix));
        clis.push(Value::Object(o));
    }
    let h = state::health(&look.rows);
    let mut health = Map::new();
    health.insert("word".to_string(), Value::from(h.word));
    health.insert(
        "problems".to_string(),
        Value::Array(h.failing.iter().map(|i| Value::from(*i)).collect()),
    );
    health.insert(
        "next".to_string(),
        if h.word == "OK" {
            Value::Null
        } else {
            Value::from(next)
        },
    );
    let mut reviews = Map::new();
    reviews.insert(
        "reviewers".to_string(),
        Value::Array(s.reviewers.0.iter().map(|c| Value::from(c.id)).collect()),
    );
    reviews.insert("from".to_string(), Value::from(s.reviewers.1.word()));
    reviews.insert("count".to_string(), int(i64::from(s.review_count.0)));
    reviews.insert(
        "timeout_s".to_string(),
        int(i64::from(s.review_timeout_s.0)),
    );
    let mut fleet = Map::new();
    fleet.insert("state".to_string(), Value::from(s.fleet.word()));
    match &s.fleet {
        FleetState::None => {}
        FleetState::Read { path, .. } => {
            fleet.insert("path".to_string(), Value::from(path.display().to_string()));
        }
        FleetState::Refused { path, why } => {
            fleet.insert("path".to_string(), Value::from(path.display().to_string()));
            fleet.insert("why".to_string(), Value::from(why.as_str()));
        }
    }
    let mut o = Map::new();
    o.insert("schema".to_string(), int(1));
    o.insert("kind".to_string(), Value::from("ai"));
    o.insert("as_of".to_string(), int(now));
    o.insert("as_of_rfc3339".to_string(), rfc(Some(now)));
    o.insert(
        "machine".to_string(),
        s.machine.as_deref().map_or(Value::Null, Value::from),
    );
    o.insert("preferred".to_string(), Value::from(s.preferred.0.id));
    o.insert(
        "preferred_from".to_string(),
        Value::from(s.preferred.1.word()),
    );
    o.insert("fleet".to_string(), Value::Object(fleet));
    o.insert("clis".to_string(), Value::Array(clis));
    o.insert("reviews".to_string(), Value::Object(reviews));
    o.insert(
        "notes".to_string(),
        Value::Array(s.notes.iter().map(|n| Value::from(n.as_str())).collect()),
    );
    o.insert("health".to_string(), Value::Object(health));
    aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
}

/// Whether this run may fix: a person at a terminal, not an agent, not `--read-only`.
fn may_fix(env: &Env, read_only: bool) -> Result<(), String> {
    if read_only {
        return Err("--read-only".to_string());
    }
    if !env.stdin_tty {
        return Err("stdin is not a terminal".to_string());
    }
    if let Some(m) = login::agent_marker(&|k| env.var(k)) {
        return Err(format!("{m} is set (an agent runs this)"));
    }
    Ok(())
}

fn ask_line(question: &str) -> Option<String> {
    let mut err = io::stderr();
    let _ = write!(err, "{question}");
    let _ = err.flush();
    let mut line = String::new();
    io::stdin().read_line(&mut line).ok()?;
    Some(line)
}

fn sign_in_one(
    env: &Env,
    runner: &dyn Runner,
    store: &Store,
    facts: &Facts,
    row: &Row,
    out: &mut dyn Write,
) -> Done {
    let var = |k: &str| env.var(k);
    let now = || crate::supervise::limit::unix_now();
    let exe = env.exe.clone().unwrap_or_else(|| PathBuf::from("aterm"));
    let host = login::Host {
        runner,
        var: &var,
        vars: &env.vars,
        aterm: &exe,
        store,
        now: &now,
        ask: &ask_line,
    };
    login::sign_in(&host, facts, row, out)
}

fn done_line(c: &Connector, done: &Done) -> String {
    match done {
        Done::SignedIn(Some(m)) => format!("{}: signed in ({m})", c.id),
        Done::SignedIn(None) => format!("{}: signed in", c.id),
        Done::NotChecked => format!(
            "{}: the sign-in ended; nothing reads whether it worked",
            c.id
        ),
        Done::SignedOut => format!("{}: still signed out", c.id),
        Done::Failed(why) => format!(
            "{}: did not sign in: {}",
            c.id,
            crate::harness::one_line(why, 160)
        ),
        Done::Refused(why) => format!(
            "{}: not started: {}",
            c.id,
            crate::harness::one_line(why, 160)
        ),
    }
}

/// The one act the health line names.
fn next_act(env: &Env, read_only: bool) -> &'static str {
    match may_fix(env, read_only) {
        Ok(()) => "aterm ai doctor",
        Err(_) if login::agent_marker(&|k| env.var(k)).is_some() => {
            "ask a person to run `aterm ai doctor`"
        }
        Err(_) => "aterm ai doctor",
    }
}

/// Installs `id` with `aterm pkg install`, on this terminal.
fn install(env: &Env, runner: &dyn Runner, id: &str) -> Result<(), String> {
    let exe = env.exe.clone().ok_or("this program's path is unknown")?;
    let args = vec!["pkg".to_string(), "install".to_string(), id.to_string()];
    match runner.terminal(&exe, &args, &env.vars, None) {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!("`aterm pkg install {id}` exited {code}")),
        Err(why) => Err(why),
    }
}

/// `aterm ai doctor` on this machine.
fn doctor_here(
    env: &Env,
    runner: &dyn Runner,
    read_only: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let first = look(env);
    let fix = may_fix(env, read_only);
    let needs: Vec<usize> = first
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.wanted && (r.mark == Mark::Fail || matches!(r.state, State::Due(_))))
        .map(|(i, _)| i)
        .collect();
    if json || fix.is_err() || needs.is_empty() {
        let next = next_act(env, read_only);
        if json {
            let _ = writeln!(out, "{}", render_json(&first, env.now, next));
        } else {
            render_text(out, &first, true, next);
        }
        return exit_of(&first.rows);
    }
    render_text(out, &first, true, "signing in below");
    let Some(store) = env.store() else {
        let _ = writeln!(
            err,
            "aterm ai doctor: aterm's state directory is unknown; nothing was run"
        );
        return ExitCode::from(1);
    };
    for i in needs {
        let row = &first.rows[i];
        let Some(f) = first
            .facts
            .iter()
            .find(|f| f.connector.id == row.connector.id)
        else {
            continue;
        };
        let _ = writeln!(out, "\n── {} ──", row.connector.product);
        let _ = out.flush();
        if matches!(row.state, State::NotInstalled) {
            if let Err(why) = install(env, runner, row.connector.id) {
                let _ = writeln!(out, "{}: not installed: {why}", row.connector.id);
                continue;
            }
            // The install laid the copy: read again, then sign in.
            let again = look(env);
            let (Some(r), Some(f)) = (
                again
                    .rows
                    .iter()
                    .find(|r| r.connector.id == row.connector.id),
                again
                    .facts
                    .iter()
                    .find(|f| f.connector.id == row.connector.id),
            ) else {
                continue;
            };
            if r.mark == Mark::Fail {
                let done = sign_in_one(env, runner, &store, f, r, out);
                let _ = writeln!(out, "{}", done_line(f.connector, &done));
            }
            continue;
        }
        let done = sign_in_one(env, runner, &store, f, row, out);
        let _ = writeln!(out, "{}", done_line(f.connector, &done));
    }
    let after = Env {
        now: crate::supervise::limit::unix_now(),
        ..env.clone()
    };
    let last = look(&after);
    let _ = writeln!(out);
    render_text(out, &last, true, "aterm ai doctor");
    exit_of(&last.rows)
}

fn exit_of(rows: &[Row]) -> ExitCode {
    if !rows.iter().any(|r| r.mark == Mark::Fail) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The machines `--on` / `--fleet` name, validated.
fn machines(env: &Env, on: Option<&[String]>) -> Result<Vec<(String, String)>, String> {
    let hosts = match on {
        Some(on) => on.to_vec(),
        None => {
            crate::supervise::config::SupervisorConfig::from_path(env.config.as_deref())
                .0
                .hosts
        }
    };
    if hosts.is_empty() {
        return Err(
            "no machines: name them with --on <host>,… or list them in [harness] hosts".to_string(),
        );
    }
    crate::harness::cli::machine_entries(&hosts)
}

fn ssh_program() -> PathBuf {
    PathBuf::from("/usr/bin/ssh")
}

/// The closed state words a remote doctor's JSON gives, by CLI: never its free text.
fn remote_states(json: &str) -> Option<Vec<(String, String)>> {
    let v: Value = aterm_json::from_str(json).ok()?;
    let mut out = Vec::new();
    for cli in v.get("clis")?.as_array()? {
        let id = cli.get("id")?.as_str()?;
        let word = cli.get("state")?.as_str()?;
        let known = connector::connector(id).is_some()
            && [
                "ok",
                "idle",
                "limited",
                "due",
                "unproven",
                "signed-out",
                "not-installed",
                "off",
            ]
            .contains(&word);
        if known {
            if out.iter().any(|(seen, _)| seen == id) {
                return None;
            }
            out.push((id.to_string(), word.to_string()));
        }
    }
    Some(out)
}

/// One machine's closed state words, by CLI id.
type States = Vec<(String, String)>;

/// The words that count a CLI as working on a machine.
const WORKING: &[&str] = &["ok", "idle", "limited", "due"];

/// Reads every machine read-only: this one's look, and each host's closed state words over
/// `ssh -T -o BatchMode=yes`; then the coverage per CLI. Signs in nowhere.
fn fleet_poll(
    env: &Env,
    runner: &dyn Runner,
    hosts: &[(String, String)],
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let here_look = look(env);
    let mut machines: Vec<(String, Option<States>)> = vec![(
        "this machine".to_string(),
        Some(
            here_look
                .rows
                .iter()
                .map(|r| (r.connector.id.to_string(), r.state.word().to_string()))
                .collect(),
        ),
    )];
    for (host, aterm) in hosts {
        let args: Vec<String> = [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
            aterm,
            "ai",
            "doctor",
            "--json",
            "--read-only",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let ran = runner.run(&Job {
            program: &ssh_program(),
            args: &args,
            env: &env.vars,
            stdin: None,
            cwd: None,
            budget: Duration::from_secs(30),
        });
        // An answer over 64 KiB is no doctor's: it is not read at all.
        let states = ran
            .ok()
            .filter(|r| r.stdout.len() <= 64 * 1024)
            .and_then(|r| remote_states(&r.stdout));
        machines.push((host.clone(), states));
    }
    let mut cover: Vec<(&'static str, usize, usize)> =
        connector::connections().map(|c| (c.id, 0, 0)).collect();
    for (_, states) in &machines {
        for (id, word) in states.iter().flatten() {
            if let Some(w) = cover.iter_mut().find(|w| w.0 == id.as_str())
                && word != "off"
            {
                w.2 += 1;
                if WORKING.contains(&word.as_str()) {
                    w.1 += 1;
                }
            }
        }
    }
    let none: Vec<&str> = cover
        .iter()
        .filter(|(_, ok, of)| *of > 0 && *ok == 0)
        .map(|(id, _, _)| *id)
        .collect();
    if json {
        let mut list = Vec::new();
        for (host, states) in &machines {
            let mut m = Map::new();
            m.insert("host".to_string(), Value::from(host.as_str()));
            m.insert(
                "clis".to_string(),
                states.as_ref().map_or(Value::Null, |st| {
                    let mut o = Map::new();
                    for (id, word) in st {
                        o.insert(id.clone(), Value::from(word.as_str()));
                    }
                    Value::Object(o)
                }),
            );
            list.push(Value::Object(m));
        }
        let mut coverage = Map::new();
        for (id, ok, of) in &cover {
            let mut c = Map::new();
            c.insert("working".to_string(), int(i64::try_from(*ok).unwrap_or(0)));
            c.insert("of".to_string(), int(i64::try_from(*of).unwrap_or(0)));
            coverage.insert((*id).to_string(), Value::Object(c));
        }
        let mut o = Map::new();
        o.insert("schema".to_string(), int(1));
        o.insert("kind".to_string(), Value::from("ai-fleet"));
        o.insert("machines".to_string(), Value::Array(list));
        o.insert("coverage".to_string(), Value::Object(coverage));
        o.insert(
            "no_machine_works".to_string(),
            Value::Array(none.iter().map(|i| Value::from(*i)).collect()),
        );
        let _ = writeln!(
            out,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    } else {
        render_text(out, &here_look, true, next_act(env, true));
        for (host, states) in machines.iter().skip(1) {
            let _ = writeln!(out, "\n{host}:");
            match states {
                Some(st) => {
                    for (id, word) in st {
                        let _ = writeln!(out, "  {}{word}", pad(&format!("{id}:"), LABEL_W));
                    }
                }
                None => {
                    let _ = writeln!(
                        out,
                        "  could not be read (unreachable, or no aterm with `ai` there)"
                    );
                }
            }
        }
        let line: Vec<String> = cover
            .iter()
            .map(|(id, ok, of)| format!("{id} {ok}/{of}"))
            .collect();
        let _ = writeln!(out, "\ncoverage: {}", line.join(" · "));
        if !none.is_empty() {
            let _ = writeln!(
                out,
                "health: FAIL — no machine has a working {}",
                none.join(", ")
            );
        }
    }
    let _ = err;
    if none.is_empty() && !here_look.rows.iter().any(|r| r.mark == Mark::Fail) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `aterm ai doctor --on <hosts>` or `--fleet`.
fn doctor_away(
    env: &Env,
    runner: &dyn Runner,
    on: Option<&[String]>,
    read_only: bool,
    json: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let hosts = match machines(env, on) {
        Ok(h) => h,
        Err(why) => {
            let _ = writeln!(err, "aterm ai doctor: {why}");
            return ExitCode::from(2);
        }
    };
    // A poll reads every machine and signs in nowhere: asked for JSON, read-only, with no
    // terminal, or under an agent. Otherwise each machine's own doctor runs on this terminal.
    if json || may_fix(env, read_only).is_err() {
        return fleet_poll(env, runner, &hosts, json, out, err);
    }
    let here = doctor_here(env, runner, false, false, out, err);
    // A person at this terminal: each machine's own doctor, on this terminal, one deep.
    let mut code = here;
    for (host, aterm) in &hosts {
        let _ = writeln!(out, "\n══ {host} ══");
        let _ = out.flush();
        let args: Vec<String> = [
            "-t",
            "-L",
            "1455:127.0.0.1:1455",
            "--",
            host,
            aterm,
            "ai",
            "doctor",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        match runner.terminal(&ssh_program(), &args, &env.vars, None) {
            Ok(0) => {}
            Ok(_) => code = ExitCode::from(1),
            Err(why) => {
                let _ = writeln!(err, "aterm ai doctor: ssh {host}: {why}");
                code = ExitCode::from(1);
            }
        }
    }
    code
}

fn csv(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `aterm ai login <id>`.
fn login_verb(
    env: &Env,
    runner: &dyn Runner,
    id: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    if connector::connector(id).is_none() {
        let _ = writeln!(
            err,
            "aterm ai login: `{}` is no AI CLI ({})",
            crate::harness::one_line(id, 40),
            connector::ids().join(", ")
        );
        return ExitCode::from(2);
    }
    if !env.stdin_tty {
        let _ = writeln!(
            err,
            "aterm ai login: not run: stdin is not a terminal (a sign-in opens a browser and waits); run `aterm ai login {id}` in a terminal"
        );
        return ExitCode::from(2);
    }
    let Some(store) = env.store() else {
        let _ = writeln!(err, "aterm ai login: aterm's state directory is unknown");
        return ExitCode::from(1);
    };
    let l = look(env);
    let (Some(row), Some(f)) = (
        l.rows.iter().find(|r| r.connector.id == id),
        l.facts.iter().find(|f| f.connector.id == id),
    ) else {
        let _ = writeln!(
            err,
            "aterm ai login: `{id}` is not a connection in this build"
        );
        return ExitCode::from(2);
    };
    let done = sign_in_one(env, runner, &store, f, row, out);
    let _ = writeln!(out, "{}", done_line(f.connector, &done));
    match done {
        Done::SignedIn(_) | Done::NotChecked => ExitCode::SUCCESS,
        Done::Refused(_) => ExitCode::from(2),
        Done::SignedOut | Done::Failed(_) => ExitCode::from(1),
    }
}

/// Parsed `aterm ai review` arguments.
#[derive(Debug, Default)]
struct ReviewArgs {
    with: Option<Vec<String>>,
    all: bool,
    from: Option<String>,
    base: Option<Base>,
    timeout: Option<u32>,
    json: bool,
    show: Option<String>,
    words: Option<String>,
    words_stdin: bool,
}

fn parse_review(args: &[String]) -> Result<ReviewArgs, String> {
    let mut r = ReviewArgs::default();
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    let mut words: Vec<String> = Vec::new();
    while i < args.len() {
        match args[i].as_str() {
            "--with" => r.with = Some(csv(&value(&mut i, "--with")?)),
            "--all" => r.all = true,
            "--from" => r.from = Some(value(&mut i, "--from")?),
            "--base" => r.base = Some(Base::Ref(value(&mut i, "--base")?)),
            "--uncommitted" => r.base = Some(Base::Uncommitted),
            "--timeout" => {
                let v = value(&mut i, "--timeout")?;
                r.timeout = Some(
                    v.parse::<u32>()
                        .ok()
                        .filter(|n| (config::TIMEOUT_MIN_S..=config::TIMEOUT_MAX_S).contains(n))
                        .ok_or_else(|| {
                            format!(
                                "--timeout takes {} to {} seconds",
                                config::TIMEOUT_MIN_S,
                                config::TIMEOUT_MAX_S
                            )
                        })?,
                );
            }
            "--json" => r.json = true,
            "--show" => r.show = Some(value(&mut i, "--show")?),
            "-" => r.words_stdin = true,
            "--" => {
                words.extend(args[i + 1..].iter().cloned());
                break;
            }
            a if a.starts_with('-') => {
                return Err(format!(
                    "`{}` is not a flag of `aterm ai review`",
                    crate::harness::one_line(a, 40)
                ));
            }
            a => words.push(a.to_string()),
        }
        i += 1;
    }
    if r.with.is_some() && r.all {
        return Err("--with and --all are one or the other".to_string());
    }
    if !words.is_empty() {
        r.words = Some(words.join(" "));
    }
    Ok(r)
}

/// `aterm ai review`.
fn review_verb(
    env: &Env,
    runner: &dyn Runner,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let refuse = |err: &mut dyn Write, why: &str| {
        let _ = writeln!(err, "aterm ai review: {why}");
        ExitCode::from(2)
    };
    let a = match parse_review(args) {
        Ok(a) => a,
        Err(why) => return refuse(err, &why),
    };
    let Some(store) = env.store() else {
        return refuse(err, "aterm's state directory is unknown");
    };
    if let Some(id) = &a.show {
        let Some(json) = review::kept(&store, id) else {
            return refuse(
                err,
                &format!(
                    "no kept review `{}` (reviews are kept a day)",
                    crate::harness::one_line(id, 40)
                ),
            );
        };
        if a.json {
            let _ = writeln!(out, "{json}");
        } else if let Some((id, dir, answers)) = review::answers_of(&json) {
            review::render(out, &id, &dir, &answers);
        }
        return ExitCode::SUCCESS;
    }
    if env.var(review::REVIEW_ENV).is_some_and(|v| !v.is_empty()) {
        return refuse(err, "a reviewer may not start reviews");
    }
    if env.var("CODEX_SANDBOX").is_some_and(|v| !v.is_empty()) {
        return refuse(
            err,
            "it needs the network, which Codex's sandbox blocks: run `aterm ai review …` outside your sandbox",
        );
    }
    let l = look(env);
    let s = &l.settings;
    if s.reviewers.0.is_empty() {
        return refuse(
            err,
            "no reviewers: list in [ai] reviewers (aterm.toml) the CLIs whose vendors may receive this machine's code",
        );
    }
    let reviewer_ids: Vec<&str> = s.reviewers.0.iter().map(|c| c.id).collect();
    let asked: Option<Vec<&'static str>> = if a.all {
        Some(s.reviewers.0.iter().map(|c| c.id).collect())
    } else if let Some(with) = &a.with {
        let mut ids = Vec::new();
        for w in with {
            match s.reviewers.0.iter().find(|c| c.id == w.as_str()) {
                Some(c) => ids.push(c.id),
                None => {
                    return refuse(
                        err,
                        &format!(
                            "`{}` is not in [ai] reviewers ({})",
                            crate::harness::one_line(w, 40),
                            reviewer_ids.join(", ")
                        ),
                    );
                }
            }
        }
        Some(ids)
    } else {
        None
    };
    let asker = match &a.from {
        Some(f) if connector::connector(f).is_some() => f.clone(),
        Some(f) => {
            return refuse(
                err,
                &format!("--from `{}` is no AI CLI", crate::harness::one_line(f, 40)),
            );
        }
        None if env.var("CLAUDECODE").is_some() => "claude".to_string(),
        None if env.var("CODEX_THREAD_ID").is_some() => "codex".to_string(),
        None => s.preferred.0.id.to_string(),
    };
    let words = if a.words_stdin {
        let mut buf = Vec::new();
        let _ = io::stdin().take(WORDS_MAX as u64 + 1).read_to_end(&mut buf);
        if buf.len() > WORDS_MAX {
            return refuse(
                err,
                &format!("what to look for is over {} KiB", WORDS_MAX / 1024),
            );
        }
        match String::from_utf8(buf) {
            Ok(text) => Some(text),
            Err(_) => return refuse(err, "what to look for is not UTF-8 text"),
        }
    } else {
        a.words.clone()
    };
    let var = |k: &str| env.var(k);
    let change = match change::collect(
        runner,
        &var,
        &env.cwd,
        a.base.as_ref().unwrap_or(&Base::Branch),
        env.home.as_deref(),
    ) {
        Ok(c) => c,
        Err(change::Refused(why)) => return refuse(err, &why),
    };
    let cands = review::candidates(&s.reviewers.0, asked.as_deref(), &asker, &l.rows, env.now);
    let count = if asked.is_some() {
        cands.len()
    } else {
        usize::from(s.review_count.0)
    };
    let budget = Duration::from_secs(u64::from(a.timeout.unwrap_or(s.review_timeout_s.0)));
    let id = review::new_id();
    let now = || crate::supervise::limit::unix_now();
    let host = review::Host {
        runner,
        var: &var,
        store: &store,
        now: &now,
    };
    let dir = review::reviews_dir(&store).join(&id);
    let _ = writeln!(
        err,
        "aterm ai review {id}: {} — asking {}",
        change.describes,
        {
            let names: Vec<&str> = cands
                .iter()
                .filter(|(_, s)| s.is_none())
                .map(|(r, _)| r.connector.id)
                .collect();
            if names.is_empty() {
                "nobody usable".to_string()
            } else {
                names.join(", ")
            }
        }
    );
    let ask = review::Ask {
        change: &change,
        asker: &asker,
        words: words.as_deref(),
        count,
        budget,
    };
    let answers = review::run_review(&host, &cands, &l.facts, &ask);
    let json = review::json(&id, &dir, &asker, &change, &answers, env.now);
    if let Err(e) = review::keep(&store, &id, &json) {
        let _ = writeln!(err, "aterm ai review: the answers could not be kept: {e}");
    }
    if a.json {
        let _ = writeln!(out, "{json}");
    } else {
        review::render(out, &id, &dir, &answers);
    }
    if answers.iter().any(|x| x.verdict == review::Verdict::Served) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Runs `aterm ai <args>` against `env` and `runner`.
pub fn run(
    env: &Env,
    runner: &dyn Runner,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let flag = |f: &str| args.iter().any(|a| a == f);
    match args.first().map(String::as_str) {
        None | Some("status") => {
            let l = look(env);
            if flag("--json") {
                let _ = writeln!(out, "{}", render_json(&l, env.now, next_act(env, true)));
            } else {
                render_text(out, &l, false, next_act(env, true));
            }
            ExitCode::SUCCESS
        }
        Some("--json") => {
            let l = look(env);
            let _ = writeln!(out, "{}", render_json(&l, env.now, next_act(env, true)));
            ExitCode::SUCCESS
        }
        Some("doctor") => {
            let rest = &args[1..];
            let mut on: Option<Vec<String>> = None;
            let mut fleet = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--read-only" | "--json" => {}
                    "--fleet" => fleet = true,
                    "--on" => {
                        i += 1;
                        match rest.get(i) {
                            Some(v) => on = Some(csv(v)),
                            None => {
                                let _ = writeln!(err, "aterm ai doctor: --on needs <host>,…");
                                return ExitCode::from(2);
                            }
                        }
                    }
                    other => {
                        let _ = writeln!(
                            err,
                            "aterm ai doctor: `{}` is not a flag\n\n{USAGE}",
                            crate::harness::one_line(other, 40)
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 1;
            }
            let read_only = flag("--read-only");
            let json = flag("--json");
            if fleet || on.is_some() {
                doctor_away(env, runner, on.as_deref(), read_only, json, out, err)
            } else {
                doctor_here(env, runner, read_only, json, out, err)
            }
        }
        Some("login") => match args.get(1) {
            Some(id) if args.len() == 2 => login_verb(env, runner, id, out, err),
            _ => {
                let _ = writeln!(
                    err,
                    "usage: aterm ai login <cli> ({})",
                    connector::ids().join(", ")
                );
                ExitCode::from(2)
            }
        },
        Some("review") => review_verb(env, runner, &args[1..], out, err),
        Some("help" | "--help" | "-h") => {
            let _ = writeln!(out, "{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            let _ = writeln!(
                err,
                "aterm ai: `{}` is not a verb\n\n{USAGE}",
                crate::harness::one_line(other, 40)
            );
            ExitCode::from(2)
        }
    }
}

/// `aterm ai …`, from the front door.
#[must_use]
pub fn main_entry(argv: Vec<OsString>) -> ExitCode {
    let args: Vec<String> = argv
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let env = Env::from_process();
    let mut out = io::stdout().lock();
    let mut err = io::stderr();
    run(&env, &LiveRunner, &args, &mut out, &mut err)
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
