// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `local-signins`: one-mesh E4, sign-ins checked on each Mac, end to end
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.15).
//!
//! Two private headless aterms (A and B) run on loopback, each in a scratch world whose HOME
//! holds its own logins for Claude Code, Codex, Grok and agy: one signed in, one signed out and
//! one expired per CLI, laid out as each CLI and aterm's own records keep them. Every aterm
//! process (both instances and every client this lane runs in their worlds) runs under a jail
//! that kills any process that resolves a name, connects off loopback, execs `open` or
//! `osascript`, or asks LaunchServices to open a URL, so one attempt is one death and zero deaths
//! is zero attempts. A black-hole proxy every world names counts each request a proxy-honouring
//! client makes. The CLIs are stand-ins that note each run and fail. The lane then runs the
//! survey, the harness's account report and the accounts upkeep on both, and the fleet survey
//! of each by the other, and judges:
//!
//! 1. every row's state as the files say (signed in, signed out, expired);
//! 2. nothing contacted a provider (no death, no proxy request, no stand-in run, no probe),
//!    while a person's ask does take a `Use` and its probe is recorded;
//! 3. nothing opened a URL or a browser, and nothing signed a row in;
//! 4. each Mac reports only its own rows, and the other's reach it only as that Mac's report;
//! 5. agy's three states are read from local files alone.
//!
//! The positive controls show each witness bites: a jailed connect and a jailed `open` die, a
//! proxied request is counted, the open recorder logs, and an asked agy read runs and is noted.
//! No value of any fixture is printed or logged; files are compared by SHA-256.
//!
//! What the witnesses do not see: the jail kills whichever process asked, and only the two
//! instances' and this lane's own clients' deaths are read, so a helper an instance spawns that
//! dies in the jail is not counted by itself. A provider CLI it spawned is a stand-in, noted
//! before it could do anything, and a fetcher that honours the proxy variables (curl, a CLI's
//! HTTP stack) is counted at the black hole. The `open` control dies at the exec rule; the
//! LaunchServices lookup rule is a backstop no control reaches.

use crate::instance::{BOOT, Instance, Launch, World};
use crate::{Fail, Res};
use aterm_agent::ai::connector::Cli;
use aterm_agent::harness::accounts::{self, Auth, Store};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The hang detector for every wait and client run.
const HANG: Duration = Duration::from_secs(60);

/// How long both Macs stay up after the passes before the zero counts are read: an exposure
/// window past the upkeep's 60-second fallback wake (`harness_host`'s `WAKE_FALLBACK`), so at
/// least one pass that no pass of this lane kicked falls inside what is counted. Not a budget:
/// nothing is waited for.
const HOLD: Duration = Duration::from_secs(70);

/// The jail (SBPL; the last matching rule wins). A name lookup (the resolver's socket), an IP
/// connection to anything but loopback, an exec of `open` or `osascript`, or a lookup of the
/// LaunchServices open services kills the process that asked, at once. The resolver's rule comes
/// first: after the loopback allowance it would also kill an established loopback connection
/// (macOS 27), as `crates/aterm/tests/accounts_upkeep_live_headless.rs`'s `NETWORK_JAIL` notes.
pub(crate) const JAIL: &str = concat!(
    "(version 1)(allow default)",
    "(deny network-outbound (literal \"/private/var/run/mDNSResponder\") ",
    "(with send-signal SIGKILL))",
    "(deny network-outbound (remote ip) (with send-signal SIGKILL))",
    "(allow network-outbound (remote ip \"localhost:*\"))",
    "(deny process-exec (literal \"/usr/bin/open\") (literal \"/usr/bin/osascript\") ",
    "(with send-signal SIGKILL))",
    "(deny mach-lookup (global-name \"com.apple.lsd.open\") ",
    "(global-name \"com.apple.lsd.openurl\") (global-name \"com.apple.coreservices.uiagent\") ",
    "(with send-signal SIGKILL))",
);

/// What `SSH_CONNECTION` says in each world: agy keeps its grant in a file there (its file
/// backend), so a signed-out agy is one whose grant file is absent.
const SSH_CONNECTION: &str = "127.0.0.1 1 127.0.0.1 22";

/// The file each world's stand-ins note their runs in, under the world's directory.
const RAN: &str = "standin-ran.log";

/// The three states a login is laid out in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Laid {
    /// Signed in: its credential file holds a live login.
    In,
    /// Signed out: no credential file.
    Out,
    /// Expired: its credential file is here, and the CLI's or aterm's own record says it lapsed.
    Expired,
}

impl Laid {
    pub const ALL: [Laid; 3] = [Laid::In, Laid::Out, Laid::Expired];

    const fn word(self) -> &'static str {
        match self {
            Laid::In => "in",
            Laid::Out => "out",
            Laid::Expired => "exp",
        }
    }
}

/// The CLIs this lane lays out, with the letter their labels carry.
pub const CLIS: [(Cli, char); 4] = [
    (Cli::Claude, 'c'),
    (Cli::Codex, 'x'),
    (Cli::Grok, 'g'),
    (Cli::Agy, 'a'),
];

/// A row's label: the Mac's side, the CLI's letter, the state (`ac-in`, `bx-exp`).
#[must_use]
pub fn label(side: char, letter: char, laid: Laid) -> String {
    format!("{side}{letter}-{}", laid.word())
}

/// The condition words a report may give a row laid out `laid`: a signed-in row is `unseen`
/// (nothing ran on it yet) or `usable`; a signed-out row was never made here (`not-enrolled`);
/// an expired row is `signed-out`.
#[must_use]
pub fn expected(laid: Laid) -> &'static [&'static str] {
    match laid {
        Laid::In => &["unseen", "usable"],
        Laid::Out => &["not-enrolled"],
        Laid::Expired => &["signed-out"],
    }
}

/// One report row: its pool, label and condition word.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Row {
    pub pool: String,
    pub label: String,
    pub condition: String,
}

/// The rows of a survey reply (`ctl survey --json`, one host or many): each host's
/// `accounts[]`, keyed by the host's `instance.node` (empty when absent).
#[must_use]
pub fn survey_rows(reply: &aterm_json::Value) -> BTreeMap<String, BTreeSet<Row>> {
    let mut out: BTreeMap<String, BTreeSet<Row>> = BTreeMap::new();
    let hosts: Vec<&aterm_json::Value> = match reply.as_array() {
        Some(hosts) => hosts.iter().collect(),
        None => vec![reply],
    };
    for host in hosts {
        let node = host["instance"]["node"].as_str().unwrap_or("").to_string();
        let rows = out.entry(node).or_default();
        for row in host["accounts"].as_array().into_iter().flatten() {
            if let (Some(pool), Some(id), Some(condition)) = (
                row["pool"].as_str(),
                row["id"].as_str(),
                row["condition"].as_str(),
            ) {
                rows.insert(Row {
                    pool: pool.into(),
                    label: id.into(),
                    condition: condition.into(),
                });
            }
        }
    }
    out
}

/// The rows of `aterm harness accounts --cli all --json`: every object that names a `label`
/// and a `condition`, under the `cli` its nearest enclosing object names.
#[must_use]
pub fn report_rows(report: &aterm_json::Value) -> BTreeSet<Row> {
    fn walk(v: &aterm_json::Value, cli: &str, out: &mut BTreeSet<Row>) {
        if let Some(items) = v.as_array() {
            for item in items {
                walk(item, cli, out);
            }
            return;
        }
        let Some(object) = v.as_object() else {
            return;
        };
        let cli = v["cli"].as_str().unwrap_or(cli).to_string();
        if let (Some(label), Some(condition)) = (v["label"].as_str(), v["condition"].as_str()) {
            out.insert(Row {
                pool: cli.clone(),
                label: label.into(),
                condition: condition.into(),
            });
        }
        for child in object.values() {
            walk(child, &cli, out);
        }
    }
    let mut out = BTreeSet::new();
    walk(report, "", &mut out);
    out
}

/// Each expected row of `side` judged against `rows`: `(label, laid, got)` for every row whose
/// condition is not one `expected` allows (`got` is `missing` for an absent row).
#[must_use]
pub fn wrong_rows(side: char, rows: &BTreeSet<Row>) -> Vec<(String, Laid, String)> {
    let mut wrong = Vec::new();
    for (_, letter) in CLIS {
        for laid in Laid::ALL {
            let want = label(side, letter, laid);
            let got = rows
                .iter()
                .find(|r| r.label == want)
                .map_or("missing", |r| r.condition.as_str());
            if !expected(laid).contains(&got) {
                wrong.push((want, laid, got.to_string()));
            }
        }
    }
    wrong
}

/// Labels in `rows` that belong to another side than `side`.
#[must_use]
pub fn foreign(side: char, rows: &BTreeSet<Row>) -> Vec<String> {
    rows.iter()
        .filter(|r| {
            let mut chars = r.label.chars();
            let (Some(s), Some(l)) = (chars.next(), chars.next()) else {
                return false;
            };
            s != side && CLIS.iter().any(|(_, letter)| *letter == l) && r.label.contains('-')
        })
        .map(|r| r.label.clone())
        .collect()
}

/// The median of `ms` (`None` when empty).
#[must_use]
pub fn median(ms: &[u128]) -> Option<u128> {
    let mut sorted = ms.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied()
}

/// The host a proxied request names (`CONNECT host:port` or an absolute-form request line),
/// never its path or query.
#[must_use]
pub fn proxied_host(first_line: &str) -> String {
    let mut parts = first_line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return "?".into();
    };
    if method.eq_ignore_ascii_case("CONNECT") {
        return target.to_string();
    }
    target
        .split_once("://")
        .map_or(target, |(_, rest)| rest)
        .split(['/', '?'])
        .next()
        .unwrap_or("?")
        .to_string()
}

/// A loopback proxy that refuses every request with `502` and counts it by host.
struct BlackHole {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl BlackHole {
    fn start() -> Result<Self, String> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let url = format!(
            "http://{}",
            listener.local_addr().map_err(|e| e.to_string())?
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let noted = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let host = proxied_host(head.lines().next().unwrap_or(""));
                noted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(host);
                let _ = stream.write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nx-lab-blackhole: 1\r\ncontent-length: 0\r\n\
                      connection: close\r\n\r\n",
                );
            }
        });
        Ok(Self { url, seen })
    }

    fn hosts(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// One Mac of the pair: its world, its environment, its instance and its fleet port.
/// Its instance is declared before its world, so on any return the instance stops before the
/// world it runs in is removed (fields drop in declaration order).
struct Mac {
    side: char,
    instance: Option<Instance>,
    world: World,
    env: Vec<(String, String)>,
    port: u16,
    /// Fixture files and their SHA-256, as laid.
    digests: BTreeMap<PathBuf, String>,
}

/// What one client run did.
struct Ran {
    code: Option<i32>,
    signal: Option<i32>,
    stdout: String,
    stderr: String,
    ms: u128,
}

impl Ran {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    let bytes = crate::util::read_regular_file(path).ok()?;
    Some(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(&bytes),
    ))
}

fn put(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        crate::util::create_private_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn b64url(bytes: &[u8]) -> String {
    let mut out = String::new();
    let _ = aterm_codec::base64::encode_url_into(&mut out, bytes);
    out.truncate(out.trim_end_matches('=').len());
    out
}

/// An unsigned JWT (`alg: none`) whose claims a Codex login file carries.
fn codex_jwt(label: &str, exp: i64, now: i64) -> String {
    let claims = format!(
        "{{\"exp\":{exp},\"iat\":{now},\"email\":\"{label}@example.test\",\
         \"https://api.openai.com/profile\":{{\"email\":\"{label}@example.test\"}},\
         \"https://api.openai.com/auth\":{{\"chatgpt_plan_type\":\"plus\",\
         \"chatgpt_user_id\":\"user-{label}\",\"user_id\":\"user-{label}\",\
         \"chatgpt_account_id\":\"acct-{label}\",\"chatgpt_account_user_id\":\"user-{label}__acct-{label}\"}}}}"
    );
    format!(
        "{}.{}.",
        b64url(b"{\"alg\":\"none\",\"typ\":\"JWT\"}"),
        b64url(claims.as_bytes())
    )
}

/// Lay out `cli`'s login for `label` in `dir` as `laid`, returning the files written.
fn lay_login(
    cli: Cli,
    dir: &Path,
    label: &str,
    laid: Laid,
    now: i64,
) -> Result<Vec<PathBuf>, String> {
    crate::util::create_private_dir_all(dir).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    if laid == Laid::Out {
        return Ok(files);
    }
    let mut write = |rel: &str, text: String| -> Result<(), String> {
        let path = dir.join(rel);
        put(&path, &text)?;
        files.push(path);
        Ok(())
    };
    match cli {
        Cli::Claude => {
            let creds = if laid == Laid::Expired {
                crate::claude_login_proof::DEAD_LOGIN.to_string()
            } else {
                format!(
                    "{{\"claudeAiOauth\":{{\"accessToken\":\"lab-access-{label}\",\
                     \"refreshToken\":\"lab-refresh-{label}\",\"expiresAt\":{},\
                     \"scopes\":[\"user:inference\",\"user:profile\"],\"subscriptionType\":\"max\"}}}}",
                    (now + 30 * 86_400) * 1000
                )
            };
            write(".credentials.json", creds)?;
            write(
                ".claude.json",
                format!(
                    "{{\"oauthAccount\":{{\"accountUuid\":\"uuid-{label}\",\
                     \"emailAddress\":\"{label}@example.test\",\"organizationType\":\"claude_max\"}},\
                     \"hasCompletedOnboarding\":true}}"
                ),
            )?;
        }
        Cli::Codex => {
            let exp = if laid == Laid::Expired {
                now - 86_400
            } else {
                now + 30 * 86_400
            };
            let jwt = codex_jwt(label, exp, now);
            write(
                "config.toml",
                "cli_auth_credentials_store = \"file\"\nmcp_oauth_credentials_store = \"file\"\n"
                    .to_string(),
            )?;
            write(
                "auth.json",
                format!(
                    "{{\"auth_mode\":\"chatgpt\",\"OPENAI_API_KEY\":null,\"tokens\":{{\
                     \"id_token\":\"{jwt}\",\"access_token\":\"{jwt}\",\
                     \"refresh_token\":\"lab-refresh-{label}\",\"account_id\":\"acct-{label}\"}},\
                     \"last_refresh\":\"2026-11-01T00:00:00Z\"}}"
                ),
            )?;
        }
        Cli::Grok => {
            write("auth.json", format!("{{\"lab\":\"{label}\"}}"))?;
            if laid == Laid::Expired {
                // aterm's own record that grok refused this login (`grok_signin::mark_refused`):
                // a marker at least as new as the login file.
                write("auth.json.refused", String::new())?;
            }
        }
        Cli::Agy => {
            write(
                ".gemini/antigravity-cli/antigravity-oauth-token",
                "{}".to_string(),
            )?;
        }
        Cli::Kimi => {}
    }
    Ok(files)
}

/// The harness state directory of `world` (`<state>/harness/claude-harness`).
fn harness_state(world: &World) -> PathBuf {
    world
        .state()
        .join("harness")
        .join(aterm_agent::harness::cli::HARNESS)
}

/// `cli`'s accounts store in `world`.
fn store(world: &World, cli: Cli) -> Store {
    match cli {
        Cli::Claude => Store::at(&harness_state(world)),
        _ => Store::at_for(&harness_state(world), cli, &world.home),
    }
}

/// Lay out side `side`'s rosters and logins in `world`; record each expired row's lapse in the
/// CLI's store as a session here that saw its login wall would (`accounts::note_auth`).
fn seed(world: &World, side: char, now: i64) -> Result<BTreeMap<PathBuf, String>, String> {
    let logins = world.home.join("lab-logins");
    let mut digests = BTreeMap::new();
    for (cli, letter) in CLIS {
        let mut roster = String::new();
        for laid in Laid::ALL {
            let label = label(side, letter, laid);
            let dir = logins.join(cli.id()).join(&label);
            for file in lay_login(cli, &dir, &label, laid, now)? {
                let digest = sha256_file(&file).ok_or("unreadable fixture")?;
                digests.insert(file, digest);
            }
            roster.push_str(&format!(
                "[[account]]\nlabel = \"{label}\"\nclass = \"dedicated\"\ndir = \"{}\"\n\n",
                dir.display()
            ));
        }
        put(
            &world.cfg.join("aterm").join(cli.accounts().roster_file),
            &roster,
        )?;
        let expired = label(side, letter, Laid::Expired);
        let st = store(world, cli);
        match cli {
            // aterm's own record of grok's refusal is the marker laid beside its login.
            Cli::Grok => {}
            // Codex keeps its auth word under the login's credential context: recorded as a
            // read that met its wall records it.
            Cli::Codex => {
                let parsed =
                    accounts::Roster::parse_for(cli, &roster).map_err(|e| e.to_string())?;
                let row = parsed.row(&expired).ok_or("no expired row")?;
                st.record_lapse_for_lab(row, now)
                    .map_err(|e| format!("codex store: {e}"))?;
            }
            _ => {
                let mut note = None;
                st.transact(now, &mut note, |state| {
                    Ok(accounts::note_auth(state, &expired, Auth::SignedOut, now))
                })
                .map_err(|e| format!("{} store: {e}", cli.id()))?;
            }
        }
    }
    Ok(digests)
}

/// The marker beside a stand-in's link: the file its runs are noted in.
const STANDIN_MARK: &str = "e4-standin";

/// Put each provider CLI's stand-in where the harness finds it in `world`: a link named for the
/// CLI to this lab binary (`<world>/standins/<cli>/<cli>`, beside a [`STANDIN_MARK`] that names
/// the world's run log), exec'd by a managed twin (`#!/bin/sh`, `exec '<link>' "$@"`, as atpkg
/// writes it) in the atpkg prefix's `agents/` and in the world's `shim/` on `PATH`.
fn standins(world: &World) -> Result<(), String> {
    let lab = std::env::current_exe().map_err(|e| e.to_string())?;
    let agents = atpkg::store::default_prefix(&world.home).join("agents");
    let shim = world.dir.join("shim");
    for dir in [&agents, &shim] {
        crate::util::create_private_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let ran = world.dir.join(RAN);
    for name in ["claude", "codex", "grok", "agy"] {
        let dir = world.dir.join("standins").join(name);
        crate::util::create_private_dir_all(&dir).map_err(|e| e.to_string())?;
        let link = dir.join(name);
        std::os::unix::fs::symlink(&lab, &link).map_err(|e| e.to_string())?;
        put(&dir.join(STANDIN_MARK), &ran.to_string_lossy())?;
        let twin = format!("#!/bin/sh\nexec '{}' \"$@\"\n", link.display());
        for at in [agents.join(name), shim.join(name)] {
            crate::sys::put(&at, 0o755, &crate::sys::PutSrc::Bytes(twin.as_bytes()))
                .map_err(|e| format!("{}: {e}", at.display()))?;
        }
    }
    Ok(())
}

/// Whether `argv0` is one of this lane's stand-ins ([`standins`]): a path whose directory holds
/// the [`STANDIN_MARK`].
#[must_use]
pub fn is_standin(argv0: &str) -> bool {
    Path::new(argv0)
        .parent()
        .is_some_and(|dir| dir.join(STANDIN_MARK).is_file())
}

/// A stand-in provider CLI ([`standins`]): note which CLI ran and its verb words (never a value:
/// only short words of letters, `-` and `/`), then fail.
#[must_use]
pub fn standin_main(argv0: &str, args: &[String]) -> i32 {
    let path = Path::new(argv0);
    let cli = path
        .file_name()
        .map_or_else(|| "?".into(), |n| n.to_string_lossy().into_owned());
    let Some(ran) = path
        .parent()
        .and_then(|dir| crate::util::read_regular_text(&dir.join(STANDIN_MARK)).ok())
        .map(PathBuf::from)
    else {
        return 1;
    };
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| {
            a.len() <= 24
                && a.chars()
                    .all(|c| c.is_ascii_alphabetic() || matches!(c, '-' | '/'))
        })
        .take(6)
        .collect();
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&ran)
    {
        let _ = writeln!(f, "{cli} {}", words.join(" "));
    }
    eprintln!("{cli}: a lab stand-in; this world has no provider");
    1
}

/// The world's whole environment: [`World::env`], the loopback guard, the live upkeep, the
/// black hole, the open recorder, agy's file backend, `USER` and `LOGNAME`.
fn environment(world: &World, proxy: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = world
        .env()
        .into_iter()
        .filter(|(k, _)| k != "PATH")
        .collect();
    let shim = world.dir.join("shim");
    let s = |p: &Path| p.to_string_lossy().into_owned();
    env.extend([
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", s(&shim)),
        ),
        (aterm_net::loopback::SEAM.into(), "1".into()),
        (
            aterm_agent::harness::accounts_sync::UPKEEP_SEAM.into(),
            "1".into(),
        ),
        ("BROWSER".into(), s(&shim.join("open"))),
        ("LAB_OPEN_LOG".into(), s(&world.dir.join("open.log"))),
        ("SSH_CONNECTION".into(), SSH_CONNECTION.into()),
        ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
        ("no_proxy".into(), "127.0.0.1,localhost".into()),
    ]);
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        env.push((name.into(), proxy.into()));
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "lab".into());
    env.push(("USER".into(), user.clone()));
    env.push(("LOGNAME".into(), user));
    env
}

/// Run `program args` in `env` from `cwd`, under the jail when `jailed`, within [`HANG`].
fn run(env: &[(String, String)], cwd: &Path, jailed: bool, program: &Path, args: &[&str]) -> Ran {
    let mut cmd = if jailed {
        let mut c = aterm_uds::spawnfd::command(Path::new("/usr/bin/sandbox-exec"));
        c.args(["-p", JAIL]).arg(program);
        c
    } else {
        aterm_uds::spawnfd::command(program)
    };
    cmd.env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(cwd)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let started = Instant::now();
    match aterm_uds::deadline::output_within(&mut cmd, HANG) {
        Ok(out) => Ran {
            code: out.status.code(),
            signal: out.status.signal(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            ms: started.elapsed().as_millis(),
        },
        Err(e) => Ran {
            code: None,
            signal: None,
            stdout: String::new(),
            stderr: format!("{e}"),
            ms: started.elapsed().as_millis(),
        },
    }
}

impl Mac {
    fn new(side: char, proxy: &str, keep: bool, now: i64) -> Result<Mac, Fail> {
        let config = crate::instance::with_harness_keys(
            &crate::instance::config_after(crate::instance::CONFIG_OFF, true, &[]),
            &["probe_api = false".into()],
        );
        let mut world = World::new(&config).map_err(Fail::die)?;
        if keep {
            world.keep();
        }
        let digests = seed(&world, side, now).map_err(Fail::die)?;
        standins(&world).map_err(Fail::die)?;
        let shim = world.dir.join("shim");
        let lab = std::env::current_exe().map_err(|e| Fail::die(e.to_string()))?;
        std::os::unix::fs::symlink(&lab, shim.join("open"))
            .map_err(|e| Fail::die(e.to_string()))?;
        let env = environment(&world, proxy);
        Ok(Mac {
            side,
            world,
            env,
            port: 0,
            instance: None,
            digests,
        })
    }

    fn cli(&self, aterm: &Path, args: &[&str]) -> Ran {
        run(&self.env, &self.world.home, true, aterm, args)
    }

    fn ctl(&self, aterm: &Path, args: &[&str]) -> Ran {
        let sock = self.world.sock.to_string_lossy().into_owned();
        let mut argv = vec!["ctl", "--sock", sock.as_str(), "--timeout", "60"];
        argv.extend_from_slice(args);
        self.cli(aterm, &argv)
    }

    fn start(&mut self, aterm: &Path) -> Result<(), Fail> {
        let mut instance = Instance::start(
            &self.world,
            Launch {
                aterm: aterm.to_owned(),
                headless: true,
                args: vec!["-e".into(), "/bin/sh".into()],
                env: self.env.clone(),
                cwd: self.world.home.clone(),
                sandbox: Some(JAIL.into()),
                reroute: false,
            },
        )
        .map_err(Fail::die)?;
        instance.await_socket(BOOT)?;
        self.instance = Some(instance);
        Ok(())
    }

    fn alive(&mut self) -> bool {
        self.instance.as_mut().is_some_and(Instance::running)
    }

    fn log(&self) -> String {
        crate::util::read_regular_text(&self.world.dir.join("aterm.log")).unwrap_or_default()
            + &std::fs::read_dir(self.world.state().join("logs"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| crate::util::read_regular_text(&e.path()).ok())
                .collect::<String>()
    }

    /// The probe records in its journal spool (`<state>/alab/`), by `why`.
    fn probes(&self) -> Vec<String> {
        let dir = self.world.state().join("alab");
        let mut out = Vec::new();
        for name in ["spool.jsonl", "spool.taken", "outbox.jsonl"] {
            let Ok(text) = crate::util::read_regular_text(&dir.join(name)) else {
                continue;
            };
            for line in text.lines() {
                let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
                    continue;
                };
                if v["k"].as_str() == Some("probe") {
                    out.push(format!(
                        "{} {}",
                        v["c"].as_str().unwrap_or("?"),
                        v["why"].as_str().unwrap_or("?")
                    ));
                }
            }
        }
        out
    }

    /// Sign-in evidence in its stores: ledger sign-in rows and spacing files.
    fn sign_ins(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (cli, _) in CLIS {
            let st = store(&self.world, cli);
            if let Ok(text) = crate::util::read_regular_text(&st.ledger_path()) {
                for line in text.lines() {
                    if let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line)
                        && v["kind"].as_str().is_some_and(|k| k.contains("sign-in"))
                    {
                        out.push(format!(
                            "{} ledger {}",
                            cli.id(),
                            v["account"].as_str().unwrap_or("?")
                        ));
                    }
                }
            }
            if st.shared_dir().join("signin-spacing").exists() {
                out.push(format!("{} signin-spacing", cli.id()));
            }
        }
        out
    }

    fn ran(&self) -> Vec<String> {
        crate::util::read_regular_text(&self.world.dir.join(RAN))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn opened(&self) -> Vec<String> {
        crate::util::read_regular_text(&self.world.dir.join("open.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn changed_fixtures(&self) -> usize {
        self.digests
            .iter()
            .filter(|(path, digest)| sha256_file(path).as_ref() != Some(*digest))
            .count()
    }

    /// Labels of the other side found anywhere in its state directory's files.
    fn foreign_in_state(&self, other: char) -> Vec<String> {
        let mut found = BTreeSet::new();
        let labels: Vec<String> = CLIS
            .iter()
            .flat_map(|(_, l)| Laid::ALL.map(|laid| label(other, *l, laid)))
            .collect();
        let mut stack = vec![self.world.state()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    stack.push(entry.path());
                } else if kind.is_file() {
                    let path = entry.path();
                    if let Ok(bytes) = crate::util::read_regular_file(&path) {
                        for l in &labels {
                            if bytes.windows(l.len() + 2).any(|w| {
                                w[0] == b'"'
                                    && w[l.len() + 1] == b'"'
                                    && &w[1..=l.len()] == l.as_bytes()
                            }) {
                                found.insert(format!("{l} in {}", path.display()));
                            }
                        }
                    }
                }
            }
        }
        found.into_iter().collect()
    }

    /// Stop this Mac's instance; a failed stop keeps its world and says why.
    fn stop(&mut self) -> Result<(), String> {
        match self.instance.as_mut() {
            Some(instance) => instance.stop().map(|()| self.instance = None),
            None => Ok(()),
        }
    }
}

/// One judged line of the record.
struct Line {
    check: &'static str,
    pass: bool,
    detail: String,
}

fn line(check: &'static str, pass: bool, detail: impl Into<String>) -> Line {
    Line {
        check,
        pass,
        detail: detail.into(),
    }
}

fn parse(text: &str) -> Option<aterm_json::Value> {
    aterm_json::from_str(text).ok()
}

/// `local-signins [--aterm PATH] [--n N] [--hold S] [--keep] [--out FILE]`.
/// # Errors
/// Usage, a world or instance that cannot be made or started, or interruption. A failed check is
/// a printed `FAIL` line and exit status 1, never an error.
pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let aterm = crate::instance::take_value(&mut args, "--aterm")?;
    let n = match crate::instance::take_value(&mut args, "--n")? {
        Some(v) => crate::instance::count("--n", &v)?.max(1) as usize,
        None => 5,
    };
    let hold = match crate::instance::take_value(&mut args, "--hold")? {
        Some(v) => Duration::from_secs(u64::from(crate::instance::count("--hold", &v)?)),
        None => HOLD,
    };
    let keep = crate::instance::take_flag(&mut args, "--keep");
    let out = crate::instance::take_value(&mut args, "--out")?;
    if !args.is_empty() {
        return Err(Fail::usage(
            "local-signins [--aterm PATH] [--n N] [--hold S] [--keep] [--out FILE]",
        ));
    }
    crate::stop::install();
    let aterm = crate::instance::resolve_aterm(aterm).map_err(Fail::die)?;
    let now = crate::util::now_secs();
    let proxy = BlackHole::start().map_err(Fail::die)?;
    let mut lines: Vec<Line> = Vec::new();
    let mut timings: Vec<(String, Vec<u128>)> = Vec::new();

    let mut a = Mac::new('a', &proxy.url, keep, now)?;
    let mut b = Mac::new('b', &proxy.url, keep, now)?;
    println!(
        "worlds: a={} b={}",
        a.world.dir.display(),
        b.world.dir.display()
    );

    // The controls: each witness bites.
    let nc = run(
        &a.env,
        &a.world.home,
        true,
        Path::new("/usr/bin/nc"),
        &["-z", "-G", "3", "192.0.2.1", "443"],
    );
    let open = run(
        &a.env,
        &a.world.home,
        true,
        Path::new("/usr/bin/open"),
        &["x-lab-control://selftest"],
    );
    let curl = run(
        &a.env,
        &a.world.home,
        true,
        Path::new("/usr/bin/curl"),
        &["-s", "-o", "/dev/null", "https://control.example.invalid/"],
    );
    let control_hosts = proxy.hosts();
    let recorder_log = a.world.dir.join("open-control.log");
    let mut recorder_env = a.env.clone();
    recorder_env.retain(|(k, _)| k != "LAB_OPEN_LOG");
    recorder_env.push((
        "LAB_OPEN_LOG".into(),
        recorder_log.to_string_lossy().into_owned(),
    ));
    let _ = run(
        &recorder_env,
        &a.world.home,
        true,
        &a.world.dir.join("shim/open"),
        &["x-lab-control://selftest"],
    );
    let recorded = crate::util::read_regular_text(&recorder_log).unwrap_or_default();
    lines.push(line(
        "control",
        nc.signal == Some(libc::SIGKILL)
            && open.signal == Some(libc::SIGKILL)
            && control_hosts.len() == 1
            && recorded.contains("x-lab-control://selftest"),
        format!(
            "jailed connect signal={:?}; jailed open signal={:?}; proxied requests={} ({}); recorder logged={}",
            nc.signal,
            open.signal,
            control_hosts.len(),
            control_hosts.join(","),
            recorded.contains("x-lab-control://selftest")
        ),
    ));
    let _ = curl;

    // The stand-ins' controls: each CLI's twin on the world's PATH runs its stand-in, which notes
    // the run, and the harness finds each CLI installed (so a run it started would be one).
    let mut noted = Vec::new();
    let mut uninstalled = Vec::new();
    for mac in [&a, &b] {
        for name in ["claude", "codex", "grok", "agy"] {
            let _ = run(
                &mac.env,
                &mac.world.home,
                true,
                &mac.world.dir.join("shim").join(name),
                &["--version"],
            );
        }
        noted.push(mac.ran().len());
        let report = mac.cli(&aterm, &["harness", "accounts", "--cli", "all"]);
        for name in ["Claude Code", "Codex CLI", "Grok Build", "Antigravity CLI"] {
            if report.stdout.contains(&format!("{name} is not installed")) {
                uninstalled.push(format!("{} {name}", mac.side));
            }
        }
    }
    lines.push(line(
        "control: stand-ins",
        noted == [4, 4] && uninstalled.is_empty(),
        format!(
            "each twin run noted a={} b={} of 4; not installed per the harness: {:?}",
            noted[0], noted[1], uninstalled
        ),
    ));

    // The fleet on loopback: A founds, B joins A.
    let ports = crate::fleet_world::free_ports(2).map_err(|e| Fail::die(e.to_string()))?;
    a.port = ports[0];
    b.port = ports[1];
    let (pa, pb) = (a.port.to_string(), b.port.to_string());
    let init = a.cli(
        &aterm,
        &["fleet", "init", "--host", "127.0.0.1", "--port", &pa],
    );
    if !init.ok() {
        return Err(Fail::die(format!("fleet init: {}", init.stderr)));
    }
    let started = Instant::now();
    a.start(&aterm)?;
    timings.push(("start".into(), vec![started.elapsed().as_millis()]));
    let code = a.world.state().join(aterm_net::fleet_member::CODE_FILE);
    let code = code.to_string_lossy().into_owned();
    let at_a = format!("127.0.0.1:{pa}");
    let join = b.cli(
        &aterm,
        &[
            "fleet",
            "join",
            "--at",
            &at_a,
            "--host",
            "127.0.0.1",
            "--port",
            &pb,
            "--code-file",
            &code,
        ],
    );
    if !join.ok() {
        let stopped = a.stop().err().map(|e| format!("; A's stop: {e}"));
        return Err(Fail::die(format!(
            "fleet join: {}{}",
            join.stderr,
            stopped.unwrap_or_default()
        )));
    }
    let started = Instant::now();
    b.start(&aterm)?;
    timings[0].1.push(started.elapsed().as_millis());
    let at_b = format!("127.0.0.1:{pb}");

    // The upkeep: each Mac's must come up live (`unconfigured` or `configured`: it runs its
    // passes; `inert` and `off` run none), within the hang detector.
    let deadline = Instant::now() + HANG;
    let live = |log: &str| {
        log.contains("accounts-upkeep=unconfigured") || log.contains("accounts-upkeep=configured")
    };
    let mut upkept = [false, false];
    while Instant::now() < deadline && !upkept.iter().all(|u| *u) {
        upkept = [live(&a.log()), live(&b.log())];
        crate::stop::sleep(Duration::from_millis(200))?;
    }

    // The passes.
    let mut survey_ms = Vec::new();
    let mut report_ms = Vec::new();
    let mut fleet_ms = Vec::new();
    let mut reported: BTreeMap<char, BTreeSet<Row>> = BTreeMap::new();
    let mut client_signals = Vec::new();
    let mut failures = Vec::new();
    let mut wrong: BTreeSet<String> = BTreeSet::new();
    let mut own: BTreeSet<String> = BTreeSet::new();
    for pass in 1..=n {
        let mut local: BTreeMap<char, BTreeSet<Row>> = BTreeMap::new();
        let mut seen_by: BTreeMap<char, BTreeSet<Row>> = BTreeMap::new();
        reported.clear();
        for (mac, other, other_at) in [(&a, 'b', &at_b), (&b, 'a', &at_a)] {
            let s = mac.ctl(&aterm, &["survey", "--json"]);
            survey_ms.push(s.ms);
            let r = mac.cli(&aterm, &["harness", "accounts", "--cli", "all", "--json"]);
            report_ms.push(r.ms);
            let f = mac.cli(&aterm, &["ctl", "survey", "--host", other_at, "--json"]);
            fleet_ms.push(f.ms);
            for (what, ran) in [("survey", &s), ("report", &r), ("fleet survey", &f)] {
                if let Some(sig) = ran.signal {
                    client_signals.push(format!("{} {what}: signal {sig}", mac.side));
                }
                if !ran.ok() {
                    failures.push(format!(
                        "pass {pass} {} {what}: exit {:?}: {}",
                        mac.side,
                        ran.code,
                        ran.stderr.lines().next().unwrap_or("")
                    ));
                }
            }
            for (what, ran, into, key) in [
                ("survey", &s, &mut local, mac.side),
                ("fleet survey", &f, &mut seen_by, other),
            ] {
                match parse(&ran.stdout) {
                    Some(v) => {
                        into.insert(key, survey_rows(&v).into_values().flatten().collect());
                    }
                    None => failures.push(format!("pass {pass} {} {what}: no JSON", mac.side)),
                }
            }
            match parse(&r.stdout) {
                Some(v) => {
                    reported.insert(mac.side, report_rows(&v));
                }
                None => failures.push(format!("pass {pass} {} report: no JSON", mac.side)),
            }
        }
        // Check 1 and 4, judged on every pass.
        for side in ['a', 'b'] {
            let other = if side == 'a' { 'b' } else { 'a' };
            for (what, rows) in [
                ("survey", local.get(&side)),
                ("report", reported.get(&side)),
            ] {
                let rows = rows.cloned().unwrap_or_default();
                for (l, laid, got) in wrong_rows(side, &rows) {
                    wrong.insert(format!("{side} {what} {l} ({laid:?}) said {got}"));
                }
                for l in foreign(side, &rows) {
                    own.insert(format!("{side}'s {what} carries {l}"));
                }
            }
            let theirs = local.get(&other).cloned().unwrap_or_default();
            let as_seen = seen_by.get(&other).cloned().unwrap_or_default();
            if theirs.is_empty() || theirs != as_seen {
                own.insert(format!(
                    "pass {pass}: {side}'s view of {other} ({} rows) is not {other}'s own report \
                     ({} rows)",
                    as_seen.len(),
                    theirs.len()
                ));
            }
        }
    }
    timings.push(("survey (local, ctl)".into(), survey_ms));
    timings.push(("harness accounts --cli all --json".into(), report_ms));
    timings.push(("survey --host (the other Mac)".into(), fleet_ms));
    let wrong: Vec<String> = wrong.into_iter().collect();
    lines.push(line(
        "1 state from local files",
        wrong.is_empty() && failures.is_empty(),
        if wrong.is_empty() && failures.is_empty() {
            format!("24 rows x survey+report on both Macs as laid, on each of n={n} passes")
        } else {
            [wrong.clone(), failures.clone()].concat().join("; ")
        },
    ));
    for (side, other, mac) in [('a', 'b', &a), ('b', 'a', &b)] {
        for l in mac.foreign_in_state(other) {
            own.insert(format!("{side}'s state holds {l}"));
        }
    }
    let own: Vec<String> = own.into_iter().collect();

    // The exposure window, then the zero counts, before the asked control.
    let held = Instant::now();
    while held.elapsed() < hold {
        crate::stop::sleep(Duration::from_millis(500))?;
    }
    timings.push((
        "held after the passes".into(),
        vec![held.elapsed().as_millis()],
    ));
    let alive = [a.alive(), b.alive()];
    let hosts: Vec<String> = proxy
        .hosts()
        .into_iter()
        .skip(control_hosts.len())
        .collect();
    let probes = [a.probes(), b.probes()];
    let ran: [Vec<String>; 2] = [
        a.ran().into_iter().skip(noted[0]).collect(),
        b.ran().into_iter().skip(noted[1]).collect(),
    ];
    let opened = [a.opened(), b.opened()];
    let sign_ins = [a.sign_ins(), b.sign_ins()];
    let changed = [a.changed_fixtures(), b.changed_fixtures()];
    let auto_sign_in: Vec<String> = [a.log(), b.log()]
        .iter()
        .flat_map(|log| {
            log.lines()
                .filter(|l| l.contains("automatic sign-in") || l.contains("a sign-in started"))
                .map(|l| l.chars().take(200).collect::<String>())
                .collect::<Vec<_>>()
        })
        .collect();

    lines.push(line(
        "2 no provider contacted",
        upkept == [true, true]
            && alive == [true, true]
            && client_signals.is_empty()
            && hosts.is_empty()
            && probes.iter().all(Vec::is_empty)
            && ran.iter().all(Vec::is_empty),
        format!(
            "upkeep live a={} b={} (upkeep log lines a={} b={}, held {} s); instances alive a={} b={}; jailed client deaths={}; \
             proxied requests={}{}; probes a={} b={}; stand-in runs a={:?} b={:?}",
            upkept[0], upkept[1], a.log().matches("accounts-upkeep").count(),
            b.log().matches("accounts-upkeep").count(), hold.as_secs(),
            alive[0], alive[1], client_signals.len(), hosts.len(),
            if hosts.is_empty() { String::new() } else { format!(" ({})", hosts.join(",")) },
            probes[0].len(), probes[1].len(), ran[0], ran[1]
        ),
    ));
    lines.push(line(
        "3 no browser, no sign-in by itself",
        opened.iter().all(Vec::is_empty)
            && sign_ins.iter().all(Vec::is_empty)
            && auto_sign_in.is_empty()
            && changed == [0, 0],
        format!(
            "opens a={} b={}; sign-in records a={:?} b={:?}; upkeep sign-in lines={}; fixtures changed a={} b={}",
            opened[0].len(), opened[1].len(), sign_ins[0], sign_ins[1], auto_sign_in.len(), changed[0], changed[1]
        ),
    ));
    lines.push(line(
        "4 each Mac reports only its own",
        own.is_empty(),
        if own.is_empty() {
            "neither survey nor report carries the other's rows; neither state holds them; each sees the other's rows exactly as that Mac reports them".to_string()
        } else {
            own.join("; ")
        },
    ));

    // Check 5: agy, read locally. The report's agy rows (check 1's), and a person's ask on the
    // default home: signed out (no grant file) it answers at once and runs nothing; with its
    // grant it runs agy (the stand-in) under a `Use`, and that probe is recorded.
    let agy_rows: Vec<String> = ['a', 'b']
        .iter()
        .flat_map(|side| {
            Laid::ALL.map(|laid| {
                let l = label(*side, 'a', laid);
                let got = reported
                    .get(side)
                    .and_then(|rows| rows.iter().find(|r| r.label == l))
                    .map_or("missing".to_string(), |r| r.condition.clone());
                format!("{l}={got}")
            })
        })
        .collect();
    let agy_rows_ok = wrong.iter().all(|w| !w.contains("a-"));
    let ran_before = a.ran().len();
    let asked_out = a.cli(&aterm, &["harness", "agy", "--refresh", "--json"]);
    let out_reason =
        parse(&asked_out.stdout).and_then(|v| v["reason"].as_str().map(str::to_string));
    let ran_after_out = a.ran().len();
    let grant = a
        .world
        .home
        .join(".gemini/antigravity-cli/antigravity-oauth-token");
    put(&grant, "{}").map_err(Fail::die)?;
    let probes_before = a.probes().len();
    let asked_in = a.cli(&aterm, &["harness", "agy", "--refresh", "--json"]);
    let ran_after_in = a.ran().len();
    let probes_after = a.probes();
    let asked_probe = probes_after.len() > probes_before
        && probes_after
            .iter()
            .skip(probes_before)
            .any(|p| p.ends_with("asked"));
    lines.push(line(
        "5 agy read locally",
        agy_rows_ok
            && out_reason.as_deref() == Some("signed-out")
            && ran_after_out == ran_before
            && ran_after_in > ran_after_out
            && asked_probe
            && asked_in.signal.is_none(),
        format!(
            "rows {}; ask signed out: reason={:?}, agy runs +{}; ask with grant: agy runs +{}, \
             asked probe recorded={asked_probe}, exit {:?}",
            agy_rows.join(" "),
            out_reason,
            ran_after_out - ran_before,
            ran_after_in - ran_after_out,
            asked_in.code
        ),
    ));
    lines.push(line(
        "2b a read that goes out takes a Use and is recorded",
        asked_probe,
        format!(
            "the asked agy read noted {} probe(s): {:?}",
            probes_after.len() - probes_before,
            &probes_after[probes_before..]
        ),
    ));

    let stops = [("A", a.stop()), ("B", b.stop())];
    let unstopped: Vec<String> = stops
        .iter()
        .filter_map(|(mac, r)| r.as_ref().err().map(|e| format!("{mac}: {e}")))
        .collect();
    lines.push(line(
        "cleanup both instances stopped",
        unstopped.is_empty(),
        if unstopped.is_empty() {
            "both stopped within the deadline".to_string()
        } else {
            unstopped.join("; ")
        },
    ));

    let mut text = String::new();
    let version = run(&a.env, &a.world.home, true, &aterm, &["--version"]);
    let build = version.stdout.lines().next().unwrap_or("").to_string();
    text.push_str(&format!("build: {build}\n"));
    for l in &lines {
        text.push_str(&format!(
            "{} {}: {}\n",
            if l.pass { "PASS" } else { "FAIL" },
            l.check,
            l.detail
        ));
    }
    for (what, ms) in &timings {
        text.push_str(&format!(
            "time {what}: median {} ms, n={}\n",
            median(ms).map_or("-".into(), |m| m.to_string()),
            ms.len()
        ));
    }
    print!("{text}");
    if let Some(out) = out {
        let out = PathBuf::from(crate::paths::from_caller(&out));
        crate::util::atomic_write(&out, text.as_bytes()).map_err(|e| Fail::die(e.to_string()))?;
    }
    Ok(i32::from(!lines.iter().all(|l| l.pass)))
}

#[cfg(test)]
#[path = "local_signins_tests.rs"]
mod tests;
