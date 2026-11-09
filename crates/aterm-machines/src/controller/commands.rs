// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Controller CLI verbs.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

macro_rules! outln {
    ($($arg:tt)*) => {{
        #[cfg(not(test))]
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stdout(), $($arg)*);
        }
        #[cfg(test)]
        println!($($arg)*);
    }};
}

macro_rules! errln {
    ($($arg:tt)*) => {{
        #[cfg(not(test))]
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), $($arg)*);
        }
        #[cfg(test)]
        eprintln!($($arg)*);
    }};
}

use super::artifacts;
use super::enroll;
use super::inventory::{self, Host, HostState, Inventory, Paths};
use super::naming;
use super::screen;
use super::ssh::{self, Target};
use super::templates;
use super::text::shown;
use crate::envelope::{self, Action, ArtifactRef, Disrupt, Envelope, Exec, Signing, Upgrade};
use crate::model::{Effect, HostReport, ItemReport, Mode, Outcome, Read};
use crate::port::{Cmd, Ran, System};
use crate::protocol::{Hello, Line};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const HELLO_BUDGET: Duration = Duration::from_secs(30);

const APPLY_BUDGET: Duration = Duration::from_secs(4 * 3600);

const PUT_BUDGET: Duration = Duration::from_secs(3600);

const UPGRADE_BUDGET: Duration = Duration::from_secs(900);

const ENVELOPE_LIFETIME: u64 = 24 * 3600;

const AGENT_KEY_LIFETIME: &str = "3600";

/// What every verb needs.
pub struct Env<'a> {
    /// The port.
    pub sys: &'a dyn System,
    /// The controller directory.
    pub paths: Paths,
    /// Print NDJSON instead of tables.
    pub json: bool,
    /// Standard input and standard error are terminals (a person is there).
    pub tty: bool,
    /// Concurrent hosts (`--parallel`), over `[controller] parallel`.
    pub parallel: Option<usize>,
    /// This process's `SSH_AUTH_SOCK` for passphrase-protected signing keys.
    pub agent_sock: Option<OsString>,
}

/// Aggregate fleet command exit tally (`0` ok, `1` failed, `2` unreachable/refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    /// An item failed or could not be measured, or a host refused or broke.
    pub failed: bool,
    /// A host could not be reached.
    pub unreachable: bool,
}

impl Tally {
    /// The exit code.
    #[must_use = "handle or propagate the command's exit status"]
    pub fn code(self) -> ExitCode {
        if self.failed {
            ExitCode::FAILURE
        } else if self.unreachable {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// `aterm machines init [--passphrase]`: initialize controller keys, signers, and inventory.
#[must_use = "handle or propagate the command's exit status"]
pub fn init(env: &Env<'_>, passphrase: bool, out: &mut dyn std::io::Write) -> ExitCode {
    if passphrase && !env.tty {
        errln!(
            "aterm machines init: not run: --passphrase asks for the signing and recovery \
             keys' passphrases, and standard input or error is not a terminal"
        );
        return ExitCode::from(2);
    }
    let sys = env.sys;
    let root = &env.paths.root;
    if let Err(why) = env.paths.ensure(sys) {
        errln!("aterm machines init: {why}");
        return ExitCode::FAILURE;
    }
    if let Err(why) = env.paths.ensure_control_dir(sys) {
        errln!("aterm machines init: {why}");
        return ExitCode::FAILURE;
    }
    let host = controller_name(sys);
    let keys = [
        (
            env.paths.ssh_key(),
            format!("aterm-machines-fleet@{host}"),
            "the fleet SSH key",
            false,
        ),
        (
            env.paths.signing_key(),
            format!("signing@{host}"),
            "the signing key",
            true,
        ),
        (
            root.join("keys").join("recovery"),
            format!("recovery@{host}"),
            "the recovery key",
            true,
        ),
    ];
    for (path, comment, what, signer) in &keys {
        let asked = passphrase && *signer;
        if sys.stat(path).is_ok() || sys.stat(&path.with_extension("pub")).is_ok() {
            if asked && sys.stat(path).is_ok() {
                match envelope::needs_passphrase(sys, path) {
                    Read::Value(true) => {}
                    Read::Value(false) => {
                        let _ = writeln!(out, "{what} has no passphrase: ssh-keygen asks for one");
                        if let Err(why) = add_passphrase(sys, path) {
                            errln!("aterm machines init: ssh-keygen -p ({what}): {why}");
                            return ExitCode::FAILURE;
                        }
                        let _ = writeln!(out, "gave {what} a passphrase: {}", path.display());
                    }
                    Read::Absent => {
                        errln!("aterm machines init: {}: no key", path.display());
                        return ExitCode::FAILURE;
                    }
                    Read::Unusable(why) => {
                        errln!("aterm machines init: {}: {why}", path.display());
                        return ExitCode::FAILURE;
                    }
                }
            }
            continue;
        }
        if asked {
            let _ = writeln!(out, "{what}: ssh-keygen asks for its passphrase");
        }
        if let Err(why) = make_key(sys, path, comment, asked) {
            errln!("aterm machines init: ssh-keygen ({what}): {why}");
            return ExitCode::FAILURE;
        }
        let _ = writeln!(out, "made {what}: {}", path.display());
    }
    if sys.stat(&env.paths.allowed_signers()).is_err() {
        let mut signers = String::new();
        for (path, comment, _, _) in keys.iter().filter(|(_, _, _, signer)| *signer) {
            let Ok(public) = sys.read_file(&path.with_extension("pub")) else {
                errln!(
                    "aterm machines init: {} is missing",
                    path.with_extension("pub").display()
                );
                return ExitCode::FAILURE;
            };
            match envelope::signers_line(comment, String::from_utf8_lossy(&public).trim()) {
                Ok(line) => {
                    signers.push_str(&line);
                    signers.push('\n');
                }
                Err(why) => {
                    errln!(
                        "aterm machines init: {}: {why}",
                        path.with_extension("pub").display()
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
        if let Err(e) = sys.write_file(&env.paths.allowed_signers(), signers.as_bytes(), 0o644) {
            errln!("aterm machines init: allowed_signers: {e}");
            return ExitCode::FAILURE;
        }
    }
    for (path, text) in [
        (env.paths.inventory(), templates::INVENTORY),
        (env.paths.baseline("default"), templates::DEFAULT_BASELINE),
    ] {
        if sys.stat(&path).is_ok() {
            continue;
        }
        if let Err(e) = sys.write_file(&path, text.as_bytes(), 0o600) {
            errln!("aterm machines init: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        let _ = writeln!(out, "wrote {}", path.display());
    }
    let _ = writeln!(
        out,
        "The controller is ready in {}. Next: aterm machines help enroll",
        root.display()
    );
    ExitCode::SUCCESS
}

fn make_key(sys: &dyn System, path: &Path, comment: &str, asked: bool) -> Result<(), String> {
    let keygen = |args: &[&str]| {
        Cmd::new("/usr/bin/ssh-keygen")
            .args(args.iter().copied())
            .arg("-C")
            .arg(comment)
            .arg("-f")
            .arg(path.as_os_str())
    };
    if asked {
        return match sys.run_tty(&keygen(&["-t", "ed25519"]), &mut |_| Ok(())) {
            Ok(Some(0)) => require_passphrase(sys, path),
            Ok(code) => Err(format!("did not make the key (status {code:?})")),
            Err(e) => Err(e.to_string()),
        };
    }
    match sys.run(&keygen(&["-q", "-t", "ed25519", "-N", ""])) {
        Ok(ran) if ran.success() => Ok(()),
        Ok(ran) => Err(ran.describe()),
        Err(e) => Err(e.to_string()),
    }
}

fn add_passphrase(sys: &dyn System, path: &Path) -> Result<(), String> {
    match sys.run_tty(
        &Cmd::new("/usr/bin/ssh-keygen")
            .args(["-p", "-f"])
            .arg(path.as_os_str()),
        &mut |_| Ok(()),
    ) {
        Ok(Some(0)) => require_passphrase(sys, path),
        Ok(code) => Err(format!("did not change the passphrase (status {code:?})")),
        Err(e) => Err(e.to_string()),
    }
}

fn require_passphrase(sys: &dyn System, path: &Path) -> Result<(), String> {
    match envelope::needs_passphrase(sys, path) {
        Read::Value(true) => Ok(()),
        Read::Value(false) => Err("the key still has no passphrase".to_string()),
        Read::Absent => Err("the key's passphrase protection could not be read".to_string()),
        Read::Unusable(why) => Err(format!(
            "the key's passphrase protection could not be confirmed: {why}"
        )),
    }
}

fn controller_name(sys: &dyn System) -> String {
    sys.run(&Cmd::new("/usr/sbin/scutil").args(["--get", "LocalHostName"]))
        .ok()
        .filter(crate::port::Ran::success)
        .map(|r| r.out().trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "controller".to_owned())
}

/// `aterm machines ls`: the inventory, one host per row.
#[must_use = "handle or propagate the command's exit status"]
pub fn ls(env: &Env<'_>) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    if env.json {
        for host in &inv.hosts {
            outln!("{}", aterm_json::to_string(host).unwrap_or_default());
        }
        return ExitCode::SUCCESS;
    }
    if inv.hosts.is_empty() {
        outln!("no hosts yet: `aterm machines help enroll`");
        return ExitCode::SUCCESS;
    }
    outln!(
        "{:<16} {:<24} {:<12} {:<10} UUID",
        "NAME",
        "ADDRESS",
        "ADMIN",
        "BASELINE"
    );
    for host in &inv.hosts {
        outln!(
            "{:<16} {:<24} {:<12} {:<10} {}",
            host.name,
            host.address,
            host.user,
            host.baseline.as_deref().unwrap_or("default"),
            host.uuid
        );
    }
    ExitCode::SUCCESS
}

fn summary(host: &Host, state: &HostState, now: u64) -> String {
    let mut reach = match (state.unreachable_since, state.last_seen) {
        (Some(since), _) => format!("unreachable for {}", age(now.saturating_sub(since))),
        (None, 0) => "never reached".to_owned(),
        (None, seen) => format!("seen {} ago", age(now.saturating_sub(seen))),
    };
    if let Some((since, why)) = &state.contact_error {
        reach.push_str(&format!(
            "; failing for {}: {}",
            age(now.saturating_sub(*since)),
            shown(why)
        ));
    }
    let hello = saved_hello(state);
    let mut notes = String::new();
    if let Some((at, why)) = &state.last_refusal {
        notes.push_str(&format!(
            "; REFUSED {} ago: {}",
            age(now.saturating_sub(*at)),
            shown(why)
        ));
    }
    if let Some((was, is)) = os_changed(state, hello.as_ref()) {
        notes.push_str(&format!("; os-changed ({} → {})", shown(&was), shown(&is)));
    }
    if let Some(only) = &state.only {
        let rest = only.rest.map_or_else(
            || "the rest of the baseline not yet".to_owned(),
            |at| format!("the rest {} ago", age(now.saturating_sub(at))),
        );
        let tables: Vec<String> = only.tables.iter().map(|t| shown(t)).collect();
        notes.push_str(&format!("; {} only ({rest})", tables.join(",")));
    }
    let agent = hello
        .as_ref()
        .map(|h| format!("agent {}", shown(&h.agent_version)))
        .unwrap_or_default();
    let Some(report) = &state.report else {
        let agent = if agent.is_empty() {
            String::new()
        } else {
            format!("  [{agent}]")
        };
        return format!("{:<16} {reach}; no report yet{notes}{agent}", host.name);
    };
    let mut counts = String::new();
    for (outcome, word) in [
        (Outcome::Ok, "ok"),
        (Outcome::Changed, "changed"),
        (Outcome::Planned, "planned"),
        (Outcome::Failed, "failed"),
        (Outcome::Unknown, "unknown"),
        (Outcome::Person, "person"),
        (Outcome::Deferred, "deferred"),
        (Outcome::Blocked, "blocked"),
        (Outcome::NotApplicable, "n/a"),
    ] {
        let n = report.count(outcome);
        if n > 0 {
            counts.push_str(&format!(" {n} {word}"));
        }
    }
    let mode = match report.mode {
        Mode::Plan => "planned",
        Mode::Apply => "applied",
    };
    let refused = report
        .refused
        .as_ref()
        .map(|r| format!("; REFUSED: {}", shown(r)))
        .unwrap_or_default();
    let agent = if agent.is_empty() {
        String::new()
    } else {
        format!(", {agent}")
    };
    format!(
        "{:<16} {reach}; {mode} {} ago;{counts}{refused}{notes}  [macOS {} {}{agent}]",
        host.name,
        age(now.saturating_sub(report.finished)),
        shown(&report.facts.os_version),
        shown(&report.facts.os_build)
    )
}

fn saved_hello(state: &HostState) -> Option<Hello> {
    state
        .hello
        .as_deref()
        .and_then(|text| aterm_json::from_str(text).ok())
}

fn os_changed(state: &HostState, hello: Option<&Hello>) -> Option<(String, String)> {
    let was = state.applied_build.as_deref().filter(|b| !b.is_empty())?;
    let is = hello
        .map(|h| h.os_build.as_str())
        .filter(|b| !b.is_empty())?;
    (was != is).then(|| (was.to_owned(), is.to_owned()))
}

fn age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn read_state(env: &Env<'_>, host: &Host, tally: &mut Tally) -> Option<HostState> {
    match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(state) => Some(state),
        Err(why) => {
            tally.failed = true;
            outln!("{:<16} state unreadable: {why}", host.name);
            None
        }
    }
}

/// `aterm machines status`: one row per host, from `state/` only.
#[must_use = "handle or propagate the command's exit status"]
pub fn status(env: &Env<'_>, names: &[String], all: bool, by_item: bool) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match inventory::select(&inv, names, all || names.is_empty()) {
        Ok(h) => h,
        Err(e) => {
            errln!("aterm machines: {e}");
            return ExitCode::from(2);
        }
    };
    let now = env.sys.now();
    let mut tally = Tally::default();
    let mut items: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
    for host in hosts {
        let Some(state) = read_state(env, host, &mut tally) else {
            continue;
        };
        tally.unreachable |= state.unreachable_since.is_some();
        tally.failed |= state.contact_error.is_some() || state.last_refusal.is_some();
        if let Some(r) = &state.report {
            tally.failed |= r.failed();
            for row in &r.items {
                *items
                    .entry(row.id.clone())
                    .or_default()
                    .entry(outcome_word(row.outcome))
                    .or_default() += 1;
            }
        }
        if env.json {
            outln!(
                "{}",
                aterm_json::to_string(&aterm_json::json!({
                    "host": host.name, "uuid": host.uuid, "address": host.address,
                    "last_seen": state.last_seen, "unreachable_since": state.unreachable_since,
                    "contact_error": state.contact_error, "last_refusal": state.last_refusal,
                    "os_changed": os_changed(&state, saved_hello(&state).as_ref()),
                    "report": state.report, "only": state.only,
                }))
                .unwrap_or_default()
            );
        } else if !by_item {
            outln!("{}", summary(host, &state, now));
        }
    }
    if by_item && !env.json {
        for (id, counts) in items {
            let counts: Vec<String> = counts.iter().map(|(w, n)| format!("{n} {w}")).collect();
            outln!("{:<48} {}", shown(&id), counts.join(", "));
        }
    }
    tally.code()
}

/// `aterm machines rename <host> <new-name>`: set a host's display name in `machines.toml`.
#[must_use = "handle or propagate the command's exit status"]
pub fn rename(env: &Env<'_>, selector: &str, new_name: &str) -> ExitCode {
    match inventory::set_host_name(env.sys, &env.paths, selector, new_name) {
        Ok(tag_key) => {
            if env.json {
                outln!(
                    "{}",
                    aterm_json::to_string(&aterm_json::json!({
                        "selector": selector,
                        "key": tag_key,
                        "name": new_name,
                        "inventory": env.paths.inventory(),
                    }))
                    .unwrap_or_default()
                );
            } else {
                outln!(
                    "renamed {} -> {} in {}",
                    shown(selector),
                    shown(new_name),
                    env.paths.inventory().display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            errln!("aterm machines rename: {why}");
            ExitCode::from(2)
        }
    }
}

fn format_session_ai_status(s: &crate::protocol::HostedSession) -> String {
    let Some(word) = s.agent.as_deref().filter(|w| !w.is_empty()) else {
        return "-".to_owned();
    };
    let mut out = shown(word);
    if let Some(detail) = s.agent_detail.as_deref().filter(|d| !d.is_empty()) {
        out.push_str(": ");
        out.push_str(&shown(detail));
    }
    if let Some(ms) = s.agent_since_ms {
        out.push_str(&format!(" ({})", age(ms / 1000)));
    }
    out
}

fn describe_host_hardware(model_id: Option<&str>) -> String {
    model_id
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("hardware unknown")
        .to_owned()
}

/// The model and macOS major version a Mac's `_device-info._tcp` TXT record names (its
/// `model=` and `osxvers=` strings, `strings` as [`crate::port::bonjour::txt_strings`] reads
/// them).
fn device_info(strings: &[Vec<u8>]) -> (Option<String>, Option<u32>) {
    let mut model = None;
    let mut osxvers = None;
    for string in strings {
        let tok = String::from_utf8_lossy(string);
        if let Some(m) = tok.strip_prefix("model=") {
            let clean: String = m
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == ',')
                .collect();
            if !clean.is_empty() {
                model = Some(clean);
            }
        } else if let Some(v) = tok.strip_prefix("osxvers=") {
            let digits: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                osxvers = Some(n);
            }
        }
    }
    (model, osxvers)
}

fn resolve_ipv4_and_ssh_banner(hostname: &str, port: u16) -> (Option<String>, Option<String>) {
    let (ipv4, banner, _) = resolve_ipv4_banner_and_rtt(hostname, port);
    (ipv4, banner)
}

/// How long [`resolve_ipv4_banner_and_rtt`] lets a name resolve: the system resolver's own
/// wait for a `.local` name's mDNS answer (about five seconds), which the unbounded
/// `getaddrinfo` this replaced waited for too. The connect and the banner keep their own
/// [`BANNER_WITHIN`].
const RESOLVE_WITHIN: Duration = Duration::from_secs(5);

/// How long [`resolve_ipv4_banner_and_rtt`] gives the connect to a resolved address, and then
/// its SSH banner: a Mac on this LAN answers within it.
const BANNER_WITHIN: Duration = Duration::from_millis(120);

/// `hostname`'s IPv4 address, the SSH banner on its `port` and the connect's round trip: the
/// name resolved and the connection made through the root LAN broker on macOS
/// (`aterm_uds::lan`), the name within [`RESOLVE_WITHIN`] and the connect and banner each within
/// [`BANNER_WITHIN`]; whatever could not be learned (a resolve the broker could not make
/// included) is `None`.
fn resolve_ipv4_banner_and_rtt(
    hostname: &str,
    port: u16,
) -> (Option<String>, Option<String>, Option<u64>) {
    use std::net::{IpAddr, SocketAddr};
    let ipv4 = aterm_uds::lan::resolve(hostname, RESOLVE_WITHIN)
        .ok()
        .and_then(|addrs| {
            let v4s: Vec<std::net::Ipv4Addr> = addrs
                .into_iter()
                .filter_map(|a| match a {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                })
                .collect();
            v4s.iter()
                .copied()
                .find(|ip| !ip.is_loopback() && !ip.is_unspecified())
                .or_else(|| v4s.first().copied())
                .map(|ip| ip.to_string())
        });
    let target_ip = ipv4
        .as_deref()
        .and_then(|ip| ip.parse::<std::net::Ipv4Addr>().ok());
    let mut rtt_us = None;
    let banner = target_ip.and_then(|ip| {
        let addr = SocketAddr::from((ip, port));
        let t0 = std::time::Instant::now();
        let (stream, _) = aterm_uds::lan::connect(
            &aterm_uds::lan::Target::Addr(addr),
            port,
            BANNER_WITHIN,
            aterm_uds::lan::ConnectOptions::default(),
        )
        .ok()?;
        rtt_us = u64::try_from(t0.elapsed().as_micros().max(1)).ok();
        read_ssh_banner(&stream, std::time::Instant::now() + BANNER_WITHIN)
    });
    (ipv4, banner, rtt_us)
}

fn open_native_monitor_tab_in_aterm() -> Option<String> {
    let mut candidates = Vec::new();
    if let Some(sock) = std::env::var_os("ATERM_SOCKET") {
        candidates.push(PathBuf::from(sock));
    }
    if let Some(dir) = aterm_uds::control_socket_dir()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        let mut socks: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("aterm-") && n.ends_with(".sock"))
            })
            .map(|e| e.path())
            .collect();
        socks.sort();
        socks.reverse();
        for s in socks {
            if !candidates.contains(&s) {
                candidates.push(s);
            }
        }
    }
    open_native_monitor_tab_in_candidates(&candidates, Duration::from_millis(250))
}

/// Each candidate's write and its one-line answer wait at most `wait`: the opener's 250 ms, or a
/// test's one-minute hang detector where the test's own server always answers.
fn open_native_monitor_tab_in_candidates(candidates: &[PathBuf], wait: Duration) -> Option<String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    for sock_path in candidates {
        let Some(token) = crate::sessions::read_sock_token(sock_path) else {
            continue;
        };
        let Ok(mut stream) = UnixStream::connect(sock_path) else {
            continue;
        };
        if !crate::sessions::same_user_peer(&stream) {
            continue;
        }
        let _ = stream.set_read_timeout(Some(wait));
        let _ = stream.set_write_timeout(Some(wait));
        if stream
            .write_all(format!("AUTH {token}\nopen app settings /monitor\n").as_bytes())
            .and_then(|()| stream.flush())
            .is_err()
        {
            continue;
        }
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.by_ref().take(1024).read_line(&mut line).is_ok() && line.starts_with("OK") {
            return Some(sock_path.display().to_string());
        }
    }
    None
}

fn read_ssh_banner(
    mut stream: &std::net::TcpStream,
    deadline: std::time::Instant,
) -> Option<String> {
    use std::io::Read;
    let mut bytes = [0; 255];
    let mut filled = 0;
    while filled < bytes.len() {
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(remaining)).ok()?;
        let read = stream.read(&mut bytes[filled..]).ok()?;
        if read == 0 {
            return None;
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        if let Some(newline) = bytes[filled..filled + read]
            .iter()
            .position(|b| *b == b'\n')
        {
            let end = filled + newline;
            return std::str::from_utf8(&bytes[..=end])
                .ok()?
                .trim()
                .strip_prefix("SSH-2.0-")
                .filter(|banner| !banner.is_empty())
                .map(str::to_owned);
        }
        filled += read;
    }
    None
}

fn format_os_and_ssh(
    os_version: Option<&str>,
    os_build: Option<&str>,
    osxvers: Option<u32>,
    ssh_banner: Option<&str>,
) -> String {
    let os_part = if let Some(ver) = os_version.filter(|v| !v.is_empty()) {
        if let Some(build) = os_build.filter(|b| !b.is_empty()) {
            format!("macOS {ver} ({build})")
        } else {
            format!("macOS {ver}")
        }
    } else if let Some(d) = osxvers {
        format!("Darwin {d}")
    } else if let Some(build) = os_build.filter(|b| !b.is_empty()) {
        format!("OS build {build}")
    } else {
        "OS version unknown".to_owned()
    };
    let ssh_part = if let Some(b) = ssh_banner.filter(|b| !b.trim().is_empty()) {
        b.split_whitespace().next().unwrap_or(b).to_owned()
    } else {
        "SSH banner unknown".to_owned()
    };
    format!("{os_part} · {ssh_part}")
}

fn working_ai_sessions(sessions: &[crate::protocol::HostedSession]) -> usize {
    sessions
        .iter()
        .filter(|s| matches!(s.agent.as_deref(), Some("working" | "busy")))
        .count()
}

/// One aterm instance's line over its tabs, `dims` its grid: every word the host reported
/// ([`shown`]), so a window name cannot write to the owner's terminal.
fn instance_header(pid: u32, tabs: &[&crate::protocol::HostedSession], dims: &str) -> String {
    let win = tabs
        .first()
        .map(|t| t.window.as_str())
        .filter(|w| !w.is_empty() && *w != "-")
        .unwrap_or("0");
    let focused = tabs.iter().any(|t| t.focused);
    let sup = tabs
        .iter()
        .find_map(|t| t.supervisor.as_deref())
        .filter(|s| !s.is_empty() && *s != "-")
        .unwrap_or("unsupervised");
    let sess_ai = tabs
        .iter()
        .filter(|s| {
            s.agent.is_some()
                || matches!(
                    s.program.as_deref(),
                    Some("claude" | "codex" | "agy" | "muse")
                )
        })
        .count();
    format!(
        "  \u{2514}\u{2500} aterm-{pid} \u{b7} window {}{} \u{b7} grid {dims} \u{b7} supervisor {} \u{b7} {} open {} \u{b7} {sess_ai} AI CLIs",
        shown(win),
        if focused { " (focused)" } else { "" },
        shown(sup),
        tabs.len(),
        if tabs.len() == 1 { "tab" } else { "tabs" }
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionExtent {
    Unobserved,
    Partial,
    Complete,
}

fn session_extent(inventory: Option<&crate::protocol::SessionSnapshot>) -> SessionExtent {
    match inventory {
        Some(inventory) if inventory.complete => SessionExtent::Complete,
        Some(inventory) if !inventory.rows.is_empty() => SessionExtent::Partial,
        _ => SessionExtent::Unobserved,
    }
}

#[allow(clippy::too_many_arguments)]
fn print_host_tabs_rows(
    host_name: &str,
    status_word: &str,
    addr_display: &str,
    hw_display: &str,
    os_display: &str,
    services_display: &str,
    why_not: Option<&str>,
    vitals: Option<&crate::protocol::HostVitals>,
    inventory: Option<&crate::protocol::SessionSnapshot>,
) {
    let extent = session_extent(inventory);
    let sessions = inventory.map_or(&[][..], |inventory| inventory.rows.as_slice());
    let mut by_instance: BTreeMap<u32, Vec<&crate::protocol::HostedSession>> = BTreeMap::new();
    for s in sessions {
        by_instance.entry(s.instance_pid).or_default().push(s);
    }
    let inst_count = by_instance.len();
    let tab_count = sessions.len();
    let ai_count = sessions
        .iter()
        .filter(|s| {
            s.agent.is_some()
                || matches!(
                    s.program.as_deref(),
                    Some("claude" | "codex" | "agy" | "muse")
                )
        })
        .count();
    let active_ai = working_ai_sessions(sessions);
    let summary = if let Some(why) = why_not {
        format!("unreachable ({})", shown(why))
    } else if extent == SessionExtent::Unobserved {
        "sessions unobserved".to_owned()
    } else if tab_count == 0 {
        "0 aterm · 0 tabs · no AI CLI".to_owned()
    } else {
        let counts = format!(
            "{inst_count} aterm · {tab_count} {} · {ai_count} AI ({active_ai} working)",
            if tab_count == 1 { "tab" } else { "tabs" }
        );
        if extent == SessionExtent::Complete {
            counts
        } else {
            format!("observed {counts} · inventory incomplete")
        }
    };
    outln!(
        "{:<35}  {:<13}  {:<42}  {:<38}  {:<34}  {:<24}  {}",
        shown(host_name),
        shown(status_word),
        shown(addr_display),
        shown(hw_display),
        shown(os_display),
        shown(services_display),
        summary
    );
    if let Some(v) = vitals {
        let vitals_branch = if by_instance.is_empty() {
            "\u{2514}\u{2500}"
        } else {
            "\u{251c}\u{2500}"
        };
        outln!(
            "  {vitals_branch} vitals: {}",
            shown(&v.format_summary_line())
        );
    }
    for (pid, tabs) in by_instance {
        let dims = tabs
            .iter()
            .find_map(|t| t.cols.zip(t.rows))
            .map_or_else(|| "-".to_owned(), |(c, r)| format!("{c}\u{d7}{r}"));
        outln!("{}", instance_header(pid, &tabs, &dims));
        for (idx, s) in tabs.iter().enumerate() {
            let branch = if idx + 1 == tabs.len() {
                "\u{2514}\u{2500}"
            } else {
                "\u{251c}\u{2500}"
            };
            let tab = format!(
                "Tab #{}{}{}",
                s.local_id,
                if s.active { "*" } else { "" },
                if s.focused { "!" } else { "" }
            );
            let tab_dims = s
                .cols
                .zip(s.rows)
                .map_or_else(|| dims.clone(), |(c, r)| format!("{c}\u{d7}{r}"));
            let cwd = s.cwd.as_deref().map_or_else(|| "-".to_owned(), shown);
            let prog = s.program.as_deref().map_or_else(|| "-".to_owned(), shown);
            let ai = format_session_ai_status(s);
            let auto = s.autopilot.as_deref().map_or_else(|| "-".to_owned(), shown);
            let human = s
                .human_ms
                .map_or_else(|| "-".to_owned(), |ms| format!("{} ago", age(ms / 1000)));
            let mut title = shown(&s.title);
            if let Some(role) = s.role.as_deref().filter(|r| !r.is_empty()) {
                title.push_str(&format!(" [{}]", shown(role)));
            }
            if let Some(att) = s.attention.as_deref().filter(|a| !a.is_empty()) {
                title.push_str(&format!(" ! {}", shown(att)));
            }
            if let Some(task) = s.task.as_deref().filter(|t| !t.is_empty()) {
                title.push_str(&format!(" ({})", shown(task)));
            }
            outln!(
                "       {branch} {:<9}  {:<8}  {:<12}  program: {:<8}  AI: {:<22}  autopilot: {:<8}  human: {:<8}  title: {}",
                tab,
                tab_dims,
                cwd,
                prog,
                ai,
                auto,
                human,
                title
            );
        }
    }
}

/// `aterm machines monitor`: live continuous system monitoring across the fleet.
#[must_use = "handle or propagate the command's exit status"]
pub fn monitor(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    open: bool,
    once: bool,
    watch: bool,
    interval_secs: u64,
) -> ExitCode {
    let is_live = env.sys.own_uuid().is_some() && crate::sessions::is_live_account();
    if (open || (!once && !watch && !env.json)) && is_live {
        if let Some(sock) = open_native_monitor_tab_in_aterm() {
            outln!(
                "opened Live Fleet System Monitor tab (/monitor) in aterm ({})",
                shown(&sock)
            );
            if open {
                return ExitCode::SUCCESS;
            }
        } else if open {
            errln!(
                "aterm machines monitor: no running local aterm window answered; printing fleet telemetry snapshot"
            );
        }
    }
    if watch && !once && !env.json && is_live {
        loop {
            if env.tty {
                outln!("\x1b[2J\x1b[H");
            }
            let now_secs = env.sys.now();
            outln!(
                "=== ATERM FLEET LIVE SYSTEM MONITOR (every {}s \u{b7} unix {}s \u{b7} Ctrl-C to stop) ===",
                interval_secs,
                now_secs
            );
            let _ = tabs(env, names, all);
            std::thread::sleep(Duration::from_secs(interval_secs.max(1)));
        }
    }
    tabs(env, names, all)
}

/// `aterm machines tabs`: live table of hosts, `aterm` instances, tabs, and vitals.
#[must_use = "handle or propagate the command's exit status"]
pub fn tabs(env: &Env<'_>, names: &[String], all: bool) -> ExitCode {
    let inv = match inventory::load(env.sys, &env.paths) {
        Ok(inv) => inv,
        Err(why) => {
            if names.is_empty() && why.contains("run `aterm machines init`") {
                Inventory::default()
            } else {
                errln!("aterm machines: {why}");
                return ExitCode::FAILURE;
            }
        }
    };
    let hosts = if inv.hosts.is_empty() && names.is_empty() {
        Vec::new()
    } else {
        match inventory::select(&inv, names, all || names.is_empty()) {
            Ok(h) => h,
            Err(e) => {
                errln!("aterm machines tabs: {e}");
                return ExitCode::from(2);
            }
        }
    };
    let mut tally = Tally::default();
    let reached = if hosts.is_empty() {
        Vec::new()
    } else {
        hello_sweep(env, &hosts, parallel(env, &inv))
    };

    if !env.json {
        outln!(
            "{:<35}  {:<13}  {:<42}  {:<38}  {:<34}  {:<24}  SESSIONS / TABS / AI",
            "HOST",
            "STATUS",
            "ADDRESS & IPv4",
            "MODEL & HARDWARE",
            "OS & SSH",
            "SERVICES"
        );
    }

    let own_facts = crate::facts::gather_without_keyboards(env.sys);
    let self_in_hosts = !own_facts.uuid.is_empty()
        && hosts
            .iter()
            .any(|h| h.uuid.eq_ignore_ascii_case(&own_facts.uuid));
    let live_all = (names.is_empty() || all)
        && env.sys.own_uuid().is_some()
        && crate::sessions::is_live_account();
    if live_all && !self_in_hosts {
        let local_sessions = crate::sessions::scan_local_aterm_sessions();
        let local_vitals = crate::facts::gather_vitals(env.sys, &own_facts);
        let raw_self = if !own_facts.local_host_name.is_empty() {
            own_facts.local_host_name.clone()
        } else if !own_facts.computer_name.is_empty() {
            own_facts.computer_name.clone()
        } else {
            "this-mac".to_owned()
        };
        let self_name = inventory::canonical_host_name(&raw_self, &inv.names);
        let dot_local = format!("{raw_self}.local");
        let (ipv4, ssh_banner) = resolve_ipv4_and_ssh_banner(&dot_local, 22);
        let addr_display = match &ipv4 {
            Some(ip) => format!("{ip} ({dot_local})"),
            None => dot_local.clone(),
        };
        let hw_desc = crate::hardware::describe(&own_facts)
            .ok()
            .map(|d| d.parts().join(" · "))
            .unwrap_or_else(|| describe_host_hardware(None));
        let hw_display = if own_facts.model.is_empty() {
            hw_desc
        } else {
            format!("{} · {hw_desc}", own_facts.model)
        };
        let os_display = format_os_and_ssh(
            Some(&own_facts.os_version),
            Some(&own_facts.os_build),
            None,
            ssh_banner.as_deref(),
        );
        let local_services = ssh_banner.as_ref().map(|_| vec!["ssh"]);
        if env.json {
            outln!(
                "{}",
                aterm_json::to_string(&aterm_json::json!({
                    "host": self_name,
                    "raw_name": raw_self,
                    "uuid": own_facts.uuid,
                    "address": dot_local,
                    "ipv4": ipv4,
                    "reachable": true,
                    "status": "online (local)",
                    "model": own_facts.model,
                    "hardware": hw_display,
                    "os_version": own_facts.os_version,
                    "os_build": own_facts.os_build,
                    "os_ssh": os_display,
                    "services": local_services,
                    "vitals": local_vitals,
                    "sessions": local_sessions.rows,
                    "sessions_complete": local_sessions.complete,
                }))
                .unwrap_or_default()
            );
        } else {
            print_host_tabs_rows(
                &self_name,
                "online (local)",
                &addr_display,
                &hw_display,
                &os_display,
                if local_services.is_some() {
                    "ssh"
                } else {
                    "unobserved"
                },
                None,
                local_vitals.as_ref(),
                Some(&local_sessions),
            );
        }
    }

    for r in reached {
        let state = inventory::load_state(env.sys, &env.paths, &r.host.uuid).unwrap_or_default();
        let (reachable, why_not, hello_opt) = match r.hello {
            Ok(h) => (true, None, Some(h)),
            Err(why) => {
                if why.unreachable() {
                    tally.unreachable = true;
                } else {
                    tally.failed = true;
                }
                (false, Some(why.why()), saved_hello(&state))
            }
        };
        let inventory = hello_opt.as_ref().map(|h| &h.sessions);
        let vitals = hello_opt.as_ref().and_then(|h| h.vitals.as_ref());
        let host_name = inventory::canonical_host_name(&r.host.name, &inv.names);
        let hw_display = hello_opt
            .as_ref()
            .filter(|h| !h.hardware.is_empty())
            .map(|h| h.hardware.join(" · "))
            .unwrap_or_else(|| describe_host_hardware(None));
        let os_display = format_os_and_ssh(
            hello_opt.as_ref().map(|h| h.os_version.as_str()),
            hello_opt.as_ref().map(|h| h.os_build.as_str()),
            None,
            None,
        );
        if env.json {
            outln!(
                "{}",
                aterm_json::to_string(&aterm_json::json!({
                    "host": host_name,
                    "uuid": r.host.uuid,
                    "address": r.host.address,
                    "reachable": reachable,
                    "error": why_not,
                    "hardware": hw_display,
                    "os_version": hello_opt.as_ref().map(|h| h.os_version.as_str()).unwrap_or(""),
                    "os_build": hello_opt.as_ref().map(|h| h.os_build.as_str()).unwrap_or(""),
                    "vitals": vitals,
                    "sessions": inventory.map_or(&[][..], |inventory| inventory.rows.as_slice()),
                    "sessions_complete": inventory.is_some_and(|inventory| inventory.complete),
                }))
                .unwrap_or_default()
            );
        } else {
            print_host_tabs_rows(
                &host_name,
                if reachable { "online (ssh)" } else { "offline" },
                &r.host.address,
                &hw_display,
                &os_display,
                "ssh",
                why_not.as_deref(),
                vitals,
                inventory,
            );
        }
    }

    if live_all {
        let services = [
            super::discover::SSH_SERVICE,
            super::discover::SFTP_SERVICE,
            super::discover::RFB_SERVICE,
        ];
        let browsed = ssh::fan_out(&services, services.len(), |svc| {
            env.sys
                .bonjour(
                    &crate::port::Bonjour::Browse((*svc).to_owned()),
                    Duration::from_millis(200),
                )
                .map(|heard| super::discover::instances(&heard, svc))
                .unwrap_or_default()
        });
        let mut ssh_instances = browsed.first().cloned().unwrap_or_default();
        let sftp_instances = browsed.get(1).cloned().unwrap_or_default();
        let rfb_instances = browsed.get(2).cloned().unwrap_or_default();
        ssh_instances.sort_by_key(|inst| natural_host_sort_key(&inst.name));

        let filtered: Vec<_> = ssh_instances
            .into_iter()
            .filter(|inst| {
                let short: String = inst
                    .name
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join("-")
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if short.is_empty() {
                    return false;
                }
                if short.eq_ignore_ascii_case(&own_facts.local_host_name)
                    || inst.name.eq_ignore_ascii_case(&own_facts.computer_name)
                {
                    return false;
                }
                let address = format!("{short}.local");
                !inv.hosts.iter().any(|h| {
                    h.address.eq_ignore_ascii_case(&address)
                        || h.name.eq_ignore_ascii_case(&short)
                        || h.name.eq_ignore_ascii_case(&inst.name)
                })
            })
            .collect();

        let enriched = ssh::fan_out(&filtered, 32, |inst| {
            let canonical = inventory::canonical_host_name(&inst.name, &inv.names);
            let resolved = super::discover::resolve(
                env.sys,
                &inst.name,
                super::discover::SSH_SERVICE,
                Duration::from_millis(120),
            );
            let (dot_local, port) = resolved.first()?.clone();
            let txt = crate::port::Bonjour::Query {
                name: format!("{}._device-info._tcp.local.", inst.name),
                rrtype: 16,
            };
            let strings: Vec<Vec<u8>> = env
                .sys
                .bonjour(&txt, Duration::from_millis(120))
                .unwrap_or_default()
                .iter()
                .filter_map(|event| match event {
                    aterm_uds::lan::Event::Record { rdata, added, .. } if *added => {
                        Some(crate::port::bonjour::txt_strings(rdata))
                    }
                    _ => None,
                })
                .flatten()
                .collect();
            let (model_id, osxvers) = device_info(&strings);
            let (ipv4, ssh_banner, rtt_us) = resolve_ipv4_banner_and_rtt(&dot_local, port);
            let vitals = ssh_banner.is_some().then(|| crate::protocol::HostVitals {
                rtt_us,
                ..crate::protocol::HostVitals::default()
            });
            let has_sftp = sftp_instances.iter().any(|s| s.name == inst.name);
            let has_rfb = rfb_instances.iter().any(|s| s.name == inst.name);
            let mut svc_list = vec!["ssh"];
            if has_sftp {
                svc_list.push("sftp");
            }
            if has_rfb {
                svc_list.push("screen-sharing");
            }
            let addr_display = match &ipv4 {
                Some(ip) => format!("{ip} ({dot_local})"),
                None => dot_local.clone(),
            };
            let hw_display = describe_host_hardware(model_id.as_deref());
            let reachable = ssh_banner.as_ref().map(|_| true);
            let status_word = if ssh_banner.is_some() {
                "reachable (ssh)"
            } else {
                "advertised (lan)"
            };
            let os_display = format_os_and_ssh(None, None, osxvers, ssh_banner.as_deref());
            Some((
                canonical,
                inst.name.clone(),
                dot_local,
                ipv4,
                addr_display,
                model_id,
                hw_display,
                os_display,
                svc_list,
                status_word,
                reachable,
                vitals,
            ))
        });

        for (
            canonical,
            raw_name,
            dot_local,
            ipv4,
            addr_display,
            model_id,
            hw_display,
            os_display,
            svc_list,
            status_word,
            reachable,
            vitals,
        ) in enriched.into_iter().flatten()
        {
            if env.json {
                outln!(
                    "{}",
                    aterm_json::to_string(&aterm_json::json!({
                        "host": canonical,
                        "raw_name": raw_name,
                        "address": dot_local,
                        "ipv4": ipv4,
                        "reachable": reachable,
                        "status": status_word,
                        "enrolled": false,
                        "model": model_id,
                        "hardware": hw_display,
                        "os_ssh": os_display,
                        "services": svc_list,
                        "vitals": vitals,
                        "sessions": Option::<&[crate::protocol::HostedSession]>::None,
                        "sessions_complete": false,
                    }))
                    .unwrap_or_default()
                );
            } else {
                print_host_tabs_rows(
                    &canonical,
                    status_word,
                    &addr_display,
                    &hw_display,
                    &os_display,
                    &svc_list.join(", "),
                    None,
                    vitals.as_ref(),
                    None,
                );
            }
        }
    }
    tally.code()
}

fn natural_host_sort_key(name: &str) -> (u8, u64, String) {
    let trimmed = name.trim();
    if let Some(rest) = trimmed
        .strip_prefix('m')
        .or_else(|| trimmed.strip_prefix('M'))
    {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = digits.parse::<u64>() {
            return (0, num, trimmed.to_ascii_lowercase());
        }
    }
    (1, 0, trimmed.to_ascii_lowercase())
}

const fn outcome_word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Ok => "ok",
        Outcome::Planned => "planned",
        Outcome::Changed => "changed",
        Outcome::Person => "person",
        Outcome::Blocked => "blocked",
        Outcome::NotApplicable => "n/a",
        Outcome::Deferred => "deferred",
        Outcome::Unknown => "unknown",
        Outcome::Failed => "failed",
    }
}

/// `aterm machines outstanding`: fleet-wide list of unreached hosts, failures, and person steps.
#[must_use = "handle or propagate the command's exit status"]
pub fn outstanding(env: &Env<'_>) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let now = env.sys.now();
    let mut tally = Tally::default();
    let mut any = false;
    for host in &inv.hosts {
        let Some(state) = read_state(env, host, &mut tally) else {
            any = true;
            continue;
        };
        if let Some(since) = state.unreachable_since {
            tally.unreachable = true;
            any = true;
            outln!(
                "{}: unreachable for {}",
                host.name,
                age(now.saturating_sub(since))
            );
        }
        if let Some((since, why)) = &state.contact_error {
            tally.failed = true;
            any = true;
            outln!(
                "{}: failing for {}: {}",
                host.name,
                age(now.saturating_sub(*since)),
                shown(why)
            );
        }
        if let Some((at, why)) = &state.last_refusal {
            tally.failed = true;
            any = true;
            outln!(
                "{}: the agent refused the last run {} ago: {}",
                host.name,
                age(now.saturating_sub(*at)),
                shown(why)
            );
        }
        if let Some((was, is)) = os_changed(&state, saved_hello(&state).as_ref()) {
            any = true;
            outln!(
                "{}: os-changed ({} → {}): apply it again",
                host.name,
                shown(&was),
                shown(&is)
            );
        }
        let Some(report) = &state.report else {
            any = true;
            outln!("{}: never planned or applied", host.name);
            continue;
        };
        if let Some(todo) = only_todo(host, &state) {
            any = true;
            outln!("{todo}");
        }
        if let Some(why) = &report.refused {
            tally.failed = true;
            any = true;
            outln!(
                "{}: the agent refused the last run: {}",
                host.name,
                shown(why)
            );
        }
        for row in &report.items {
            let pending = match (row.outcome, row.effect) {
                (Outcome::Changed, Effect::Login) => Some("at the next login"),
                (Outcome::Changed, Effect::Restart) => Some("at the next restart"),
                _ => None,
            };
            if let Some(when) = pending {
                any = true;
                outln!("{}: takes effect {when}: {}", host.name, shown(&row.id));
                continue;
            }
            if matches!(
                row.outcome,
                Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
            ) {
                continue;
            }
            tally.failed |= row.outcome.is_failure();
            any = true;
            outln!(
                "{}: {} {} — {}{}",
                host.name,
                outcome_word(row.outcome),
                shown(&row.id),
                shown(&row.title),
                row.why
                    .as_ref()
                    .map(|w| format!(": {}", shown(w)))
                    .unwrap_or_default()
            );
        }
    }
    if !any {
        outln!("nothing outstanding");
    }
    tally.code()
}

fn only_todo(host: &Host, state: &HostState) -> Option<String> {
    let only = state.only.as_ref().filter(|o| o.rest.is_none())?;
    let tables: Vec<String> = only.tables.iter().map(|t| shown(t)).collect();
    Some(format!(
        "{name}: only its {} so far (--only); the rest of its baseline (apps, settings) has not \
         run yet: `aterm machines apply {name}` does it when you want",
        tables.join(", "),
        name = host.name
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelloError {
    Ssh(ssh::Failure),
    Run(String),
    Answer(String),
}

impl HelloError {
    const fn unreachable(&self) -> bool {
        matches!(self, Self::Ssh(f) if f.unreachable())
    }

    fn why(&self) -> String {
        match self {
            Self::Ssh(f) => f.why(),
            Self::Run(why) | Self::Answer(why) => why.clone(),
        }
    }
}

struct Reached<'h> {
    host: Cow<'h, Host>,
    hello: Result<Hello, HelloError>,
}

fn not_reached(tally: &mut Tally, host: &Host, why: &HelloError) {
    if why.unreachable() {
        tally.unreachable = true;
        outln!("{:<16} unreachable: {}", host.name, shown(&why.why()));
    } else {
        tally.failed = true;
        outln!("{:<16} failed: {}", host.name, shown(&why.why()));
    }
}

struct OwnAgent {
    exe: PathBuf,
    sha256: String,
}

impl OwnAgent {
    fn of(sys: &dyn System) -> Result<Self, String> {
        let exe = enroll::own_exe()?;
        let sha256 = sys
            .sha256(&exe)
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        Ok(Self { exe, sha256 })
    }

    fn differs(&self, hello: &Hello) -> bool {
        !hello.agent_sha256.eq_ignore_ascii_case(&self.sha256)
    }
}

struct AgentArchive<'s> {
    archive: enroll::Archive,
    kind: &'static str,
    _scratch: crate::port::Scratch<'s>,
}

struct Job<'h> {
    host: Cow<'h, Host>,
    hello: Hello,
    needed: Vec<artifacts::Needed>,
    upgrade: Option<Vec<u8>>,
    wire: Vec<u8>,
}

#[derive(Default)]
struct Resolved {
    aterm: artifacts::AtermMemo,
    ensured: BTreeSet<String>,
}

fn fleet_hosts<'i>(
    inv: &'i Inventory,
    names: &[String],
    all: bool,
    verb: &str,
) -> Result<Vec<&'i Host>, ExitCode> {
    match inventory::select(inv, names, all) {
        Ok(h) if !h.is_empty() => Ok(h),
        Ok(_) => {
            outln!("machines.toml has no hosts yet: `aterm machines help enroll`");
            Err(ExitCode::SUCCESS)
        }
        Err(e) => {
            errln!("aterm machines {verb}: {e}");
            Err(ExitCode::from(2))
        }
    }
}

/// `aterm machines plan|apply`, the whole baseline.
#[must_use = "handle or propagate the command's exit status"]
pub fn plan_or_apply(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    mode: Mode,
    now: bool,
) -> ExitCode {
    plan_or_apply_only(env, names, all, mode, now, &[])
}

/// `aterm machines plan|apply [--only <table>,…]`: plan or apply selected baseline tables.
#[must_use = "handle or propagate the command's exit status"]
pub fn plan_or_apply_only(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    mode: Mode,
    now: bool,
    only: &[String],
) -> ExitCode {
    let (action, verb) = match mode {
        Mode::Plan => (Action::Plan, "plan"),
        Mode::Apply => (Action::Apply, "apply"),
    };
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let all = all || (mode == Mode::Plan && names.is_empty());
    let hosts = match fleet_hosts(&inv, names, all, verb) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let parallel = parallel(env, &inv);
    let mut tally = Tally::default();
    let own = match OwnAgent::of(env.sys) {
        Ok(own) => own,
        Err(why) => {
            errln!("aterm machines: {why}");
            return ExitCode::FAILURE;
        }
    };

    let reached = hello_sweep(env, &hosts, parallel);
    if let Some(stop) = broker_wall(&reached) {
        errln!("{stop}");
        return ExitCode::FAILURE;
    }

    let principals = principals(&inv, env);
    let local_names = naming::wanted(env.sys, &env.paths, &inv);
    let mut resolved = Resolved::default();
    let mut agent_archive: Option<AgentArchive<'_>> = None;
    let mut built: Vec<(Cow<'_, Host>, Hello, Vec<artifacts::Needed>, bool)> = Vec::new();
    let mut bodies: Vec<Envelope> = Vec::new();
    for r in reached {
        let hello = match r.hello {
            Ok(h) => h,
            Err(why) => {
                not_reached(&mut tally, &r.host, &why);
                continue;
            }
        };
        if let Some(run) = &hello.busy {
            tally.failed = true;
            outln!(
                "{:<16} busy run={}: nothing was sent; `aterm machines attach {}` follows it",
                r.host.name,
                shown(run),
                r.host.name
            );
            continue;
        }
        if let Some(why) = local_names
            .get(&r.host.uuid.to_ascii_lowercase())
            .and_then(|local| naming::clash(&inv, &local_names, &r.host, local))
        {
            tally.failed = true;
            outln!("{:<16} not sent: {why}", r.host.name);
            continue;
        }
        let upgrade = mode == Mode::Apply && own.differs(&hello);
        if upgrade && agent_archive.is_none() {
            match build_agent_archive(env.sys, &own) {
                Ok(a) => agent_archive = Some(a),
                Err(why) => {
                    errln!("aterm machines: the agent upgrade archive: {why}");
                    return ExitCode::FAILURE;
                }
            }
        }
        let floor = floor_of(&hello, &principals);
        let mut host_bodies = Vec::new();
        if let (true, Some(a)) = (upgrade, &agent_archive) {
            match upgrade_body(env, &r.host, floor, a) {
                Ok(body) => host_bodies.push(body),
                Err(why) => {
                    tally.failed = true;
                    outln!("{:<16} not sent: {why}", r.host.name);
                    continue;
                }
            }
        }
        match baseline_body(
            env,
            &r.host,
            &hello,
            floor,
            action,
            now,
            &mut resolved,
            only,
        ) {
            Ok((body, needed)) => {
                if let Err(why) = remember_only(env, &r.host, body.serial, only) {
                    tally.failed = true;
                    outln!(
                        "{:<16} not sent: --only cannot be noted in its state ({}), and its \
                         report would read as the whole baseline's",
                        r.host.name,
                        shown(&why)
                    );
                    continue;
                }
                host_bodies.push(body);
                bodies.extend(host_bodies);
                built.push((r.host, hello, needed, upgrade));
            }
            Err(why) => {
                tally.failed = true;
                outln!("{:<16} not sent: {why}", r.host.name);
            }
        }
    }
    let mut wires = match seal_all(env, &bodies, &env.paths.signing_key()) {
        Ok(w) => w.into_iter(),
        Err(why) => {
            errln!("aterm machines: {why}");
            return ExitCode::FAILURE;
        }
    };
    let mut jobs: Vec<Job<'_>> = Vec::with_capacity(built.len());
    for (host, hello, needed, upgrade) in built {
        let upgrade = if upgrade { wires.next() } else { None };
        let Some(wire) = wires.next() else {
            break;
        };
        jobs.push(Job {
            host,
            hello,
            needed,
            upgrade,
            wire,
        });
    }

    let outcomes = ssh::fan_out(&jobs, parallel, |job| {
        send(env, job, agent_archive.as_ref().map(|a| &a.archive))
    });
    // Every host's key calls share one agh client, so agh paces the run's key writes.
    let key_calls = super::github::KeyCalls::new(env.sys, &env.paths);
    for (job, outcome) in jobs.iter().zip(outcomes) {
        match outcome {
            Ok(mut report) => {
                if mode == Mode::Plan && own.differs(&job.hello) {
                    report
                        .items
                        .insert(0, pending_upgrade_row(&job.hello, &own));
                }
                tally.failed |= report.failed();
                print_report(env, &job.host, &report);
                let _ = follow_rename(env, &job.host, &report);
                after_run_with(env, &inv, &job.host, report, Source::Baseline, &key_calls);
            }
            Err(why) => {
                tally.failed = true;
                outln!("{:<16} {}", job.host.name, shown(&why));
            }
        }
    }
    tally.code()
}

fn parallel(env: &Env<'_>, inv: &Inventory) -> usize {
    env.parallel.unwrap_or_else(|| inv.parallel()).max(1)
}

fn hello_sweep<'h>(env: &Env<'_>, hosts: &[&'h Host], parallel: usize) -> Vec<Reached<'h>> {
    let reached = ssh::fan_out(hosts, parallel, |host| reach(env, host));
    for (host, r) in hosts.iter().zip(&reached) {
        record_found(env, host, r);
    }
    reached
}

fn reach_one<'h>(env: &Env<'_>, host: &'h Host) -> Reached<'h> {
    let reached = reach(env, host);
    record_found(env, host, &reached);
    reached
}

fn reach<'h>(env: &Env<'_>, host: &'h Host) -> Reached<'h> {
    let mut answered = ask_hello(env, host);
    let mut at = Cow::Borrowed(host);
    let elsewhere = match &answered {
        Err(HelloError::Ssh(ssh::Failure::Unresolved | ssh::Failure::HostKeyChanged)) => {
            naming::renamed(env.sys, &env.paths, host)
        }
        Ok(hello) => naming::own_address(host, hello).map(|address| Host {
            address,
            ..host.clone()
        }),
        Err(_) => None,
    };
    if let Some(moved) = elsewhere
        && let Ok(found) = ask_hello(env, &moved)
    {
        answered = Ok(found);
        at = Cow::Owned(moved);
    }
    if let Err(HelloError::Ssh(failure)) = &answered
        && (failure.unreachable() || *failure == ssh::Failure::HostKeyChanged)
        && let Some(moved) = naming::named(env.sys, &env.paths, host)
        && let Ok(found) = ask_hello(env, &moved)
    {
        answered = Ok(found);
        at = Cow::Owned(moved);
    }
    record_hello(env, host, &answered);
    Reached {
        host: at,
        hello: answered,
    }
}

fn record_found(env: &Env<'_>, host: &Host, reached: &Reached<'_>) {
    if let Cow::Owned(moved) = &reached.host {
        let _ = move_address(env, host, &moved.address, &found_lead(host));
    }
}

fn found_lead(host: &Host) -> String {
    let how = if host.address.parse::<std::net::IpAddr>().is_ok() {
        "moved"
    } else {
        "renamed"
    };
    format!("{:<16} {how}: ", host.name)
}

fn hello(env: &Env<'_>, host: &Host) -> Result<Hello, HelloError> {
    let answered = ask_hello(env, host);
    record_hello(env, host, &answered);
    answered
}

fn record_hello(env: &Env<'_>, host: &Host, answered: &Result<Hello, HelloError>) {
    if let Ok(mut state) = inventory::load_state(env.sys, &env.paths, &host.uuid) {
        let now = env.sys.now();
        match answered {
            Ok(hello) => {
                state.last_seen = now;
                state.unreachable_since = None;
                state.contact_error = None;
                state.hello = aterm_json::to_string(hello).ok();
            }
            Err(why) if why.unreachable() => {
                if state.unreachable_since.is_none() {
                    state.unreachable_since = Some(now);
                }
            }
            Err(why) => {
                state.unreachable_since = None;
                let since = state
                    .contact_error
                    .as_ref()
                    .map_or(now, |(since, _)| *since);
                state.contact_error = Some((since, why.why()));
            }
        }
        let _ = inventory::save_state(env.sys, &env.paths, &host.uuid, &state);
    }
}

fn ask_hello(env: &Env<'_>, host: &Host) -> Result<Hello, HelloError> {
    let target = Target::of(host);
    let ran = ssh::request(env.sys, &env.paths, &target, "hello", None, HELLO_BUDGET)
        .map_err(|e| HelloError::Run(e.to_string()))?;
    if !ran.success() {
        return Err(
            ssh::classify(&ran).map_or_else(|| HelloError::Answer(ran.describe()), HelloError::Ssh)
        );
    }
    let hello: Hello = aterm_json::from_str(ran.out().trim())
        .map_err(|e| HelloError::Answer(format!("hello did not parse: {e}")))?;
    if !hello.uuid.eq_ignore_ascii_case(&host.uuid) {
        return Err(HelloError::Answer(format!(
            "this is not {}: it answers as {} (serial {})",
            host.name, hello.uuid, hello.serial_number
        )));
    }
    Ok(hello)
}

/// The one line a sweep stops on where no host answered and the LAN broker, which makes every
/// LAN connection of the controller's `ssh`, could not make one ([`ssh::Failure::Broker`]): the
/// broker's own words stand for every host, which says nothing of them.
fn broker_wall(reached: &[Reached<'_>]) -> Option<String> {
    let none_reached = reached.iter().all(|r| r.hello.is_err());
    let broker = reached.iter().find_map(|r| match &r.hello {
        Err(HelloError::Ssh(ssh::Failure::Broker(line))) => Some(format!("controller: {line}")),
        _ => None,
    });
    broker.filter(|_| none_reached)
}

const ONLY_SENT_KEPT: usize = 8;

fn remember_only(env: &Env<'_>, host: &Host, serial: u64, only: &[String]) -> Result<(), String> {
    if only.is_empty() {
        return Ok(());
    }
    let mut state = inventory::load_state(env.sys, &env.paths, &host.uuid)?;
    state.only_sent.push((serial, only.to_vec()));
    let excess = state.only_sent.len().saturating_sub(ONLY_SENT_KEPT);
    state.only_sent.drain(..excess);
    inventory::save_state(env.sys, &env.paths, &host.uuid, &state)
}

fn resolved_baseline(
    env: &Env<'_>,
    host: &Host,
    hello: &Hello,
    resolved: &mut Resolved,
    only: &[String],
) -> Result<String, String> {
    let name = host.baseline.as_deref().unwrap_or("default");
    if name.contains('/') || name.contains("..") {
        return Err(format!("baseline name {name:?} is not a plain name"));
    }
    let path = env.paths.baseline(name);
    let text = env
        .sys
        .read_file(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8(text).map_err(|_| format!("{} is not UTF-8", path.display()))?;
    let mut vars = naming::vars(host).to_vec();
    vars.push(("serial", hello.serial_number.as_str()));
    let text = crate::baseline::fill(&text, &vars)?;
    let text = default_users(&text, host)?;
    let text = crate::baseline::keep_only(&text, only)?;
    let text = artifacts::resolve_aterm_once(env.sys, &env.paths, &text, &mut resolved.aterm)?;
    crate::baseline::parse(&text)?;
    Ok(text)
}

fn default_users(text: &str, host: &Host) -> Result<String, String> {
    let mut doc: aterm_toml::Value =
        aterm_toml::from_str(text).map_err(|e| format!("baseline: {e}"))?;
    let Some(table) = doc.as_table_mut() else {
        return Err("baseline: not a table".into());
    };
    if table.get("users").is_none() {
        let users = if host.users.is_empty() {
            vec![host.user.clone()]
        } else {
            host.users.clone()
        };
        table.insert(
            "users".into(),
            aterm_toml::Value::Array(users.into_iter().map(aterm_toml::Value::String).collect()),
        );
    }
    aterm_toml::to_string(&doc).map_err(|e| format!("baseline: {e}"))
}

fn floor_of(hello: &Hello, principals: &[String]) -> u64 {
    hello
        .floors
        .iter()
        .filter(|f| principals.contains(&f.principal))
        .map(|f| f.serial)
        .max()
        .unwrap_or(0)
}

fn body(
    env: &Env<'_>,
    host: &Host,
    floor: u64,
    action: Action,
    disrupt: Disrupt,
) -> Result<Envelope, String> {
    let serial = inventory::next_serial(env.sys, &env.paths, floor)?;
    let issued = env.sys.now();
    Ok(Envelope {
        schema: crate::SCHEMA,
        host: host.uuid.clone(),
        host_name: host.name.clone(),
        serial,
        issued,
        expires: issued + ENVELOPE_LIFETIME,
        action,
        disrupt,
        idle_secs: 900,
        baseline: None,
        artifacts: Vec::new(),
        exec: None,
        upgrade: None,
        rekey: None,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "each is one input of the envelope"
)]
fn baseline_body(
    env: &Env<'_>,
    host: &Host,
    hello: &Hello,
    floor: u64,
    action: Action,
    now: bool,
    resolved: &mut Resolved,
    only: &[String],
) -> Result<(Envelope, Vec<artifacts::Needed>), String> {
    let text = resolved_baseline(env, host, hello, resolved, only)?;
    let baseline = crate::baseline::parse(&text)?;
    let needed = artifacts::needed(&baseline);
    for item in &needed {
        if !resolved.ensured.contains(&item.sha256) {
            artifacts::ensure(env.sys, &env.paths, item)?;
            resolved.ensured.insert(item.sha256.clone());
        }
    }
    let disrupt = if now { Disrupt::Now } else { Disrupt::WhenIdle };
    let mut envelope = body(env, host, floor, action, disrupt)?;
    envelope.baseline = Some(text);
    envelope.artifacts = needed
        .iter()
        .map(|n| ArtifactRef {
            sha256: n.sha256.clone(),
            name: n.name.clone(),
        })
        .collect();
    if action == Action::Apply && !only.is_empty() {
        envelope
            .artifacts
            .extend(kept_by_only(hello, &envelope.artifacts));
    }
    Ok((envelope, needed))
}

fn kept_by_only(hello: &Hello, named: &[ArtifactRef]) -> Vec<ArtifactRef> {
    let mut kept: Vec<ArtifactRef> = Vec::new();
    for sha in &hello.cached {
        let sha = sha.to_ascii_lowercase();
        let known = named.iter().chain(&kept).any(|a| a.sha256 == sha);
        if crate::agent::is_sha256(&sha) && !known {
            kept.push(ArtifactRef {
                sha256: sha,
                name: "cached, kept by --only".into(),
            });
        }
    }
    kept
}

fn build_agent_archive<'s>(
    sys: &'s dyn System,
    own: &OwnAgent,
) -> Result<AgentArchive<'s>, String> {
    let scratch = envelope::fresh_scratch(sys, "upgrade")?;
    let archive = enroll::build_archive(sys, &own.exe, scratch.path())?;
    let kind = upgrade_kind(sys, &own.exe, &archive);
    Ok(AgentArchive {
        archive,
        kind,
        _scratch: scratch,
    })
}

const APP_ZIP_REQUIREMENT: &str =
    "=anchor apple generic and certificate leaf[subject.OU] = \"A66A9P66Z7\"";

fn upgrade_kind(sys: &dyn System, exe: &Path, archive: &enroll::Archive) -> &'static str {
    if !archive.bundle {
        return "dev-zip";
    }
    let (bundle, _, _) = enroll::payload_of(exe);
    let verified = sys
        .run(
            &Cmd::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict", "-R", APP_ZIP_REQUIREMENT])
                .arg(bundle.as_os_str())
                .budget(Duration::from_secs(300)),
        )
        .is_ok_and(|ran| ran.success());
    if verified { "app-zip" } else { "dev-zip" }
}

fn upgrade_body(
    env: &Env<'_>,
    host: &Host,
    floor: u64,
    agent: &AgentArchive<'_>,
) -> Result<Envelope, String> {
    let mut envelope = body(env, host, floor, Action::Upgrade, Disrupt::Now)?;
    envelope.artifacts = vec![ArtifactRef {
        sha256: agent.archive.sha256.clone(),
        name: "aterm agent".into(),
    }];
    envelope.upgrade = Some(Upgrade {
        sha256: agent.archive.sha256.clone(),
        kind: agent.kind.into(),
        version: aterm_stamp::app_version().into(),
    });
    Ok(envelope)
}

fn build_name(version: &str, sha256: &str) -> String {
    match sha256.get(..12) {
        Some(short) => format!("{version} ({short})"),
        None => format!("{version} (build not reported)"),
    }
}

fn pending_upgrade_row(hello: &Hello, own: &OwnAgent) -> ItemReport {
    ItemReport {
        id: "agent.upgrade".into(),
        family: "agent".into(),
        title: "aterm's helper on this Mac, switched to the controller's build".into(),
        outcome: Outcome::Planned,
        from: Some(build_name(&hello.agent_version, &hello.agent_sha256)),
        to: Some(build_name(aterm_stamp::app_version(), &own.sha256)),
        why: Some("apply switches it first".into()),
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        essential: false,
        elapsed_ms: None,
    }
}

fn principals(inv: &Inventory, env: &Env<'_>) -> Vec<String> {
    if let Some(p) = &inv.controller.principal {
        return vec![p.clone()];
    }
    let public = public_half(&env.paths.signing_key());
    let mut names = Vec::new();
    for list in [env.paths.allowed_signers(), previous_signers(&env.paths)] {
        if let Ok(found) = principals_of_key(env.sys, &public, &list) {
            for name in found {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    if names.is_empty() {
        names.push(format!("signing@{}", controller_name(env.sys)));
    }
    names
}

fn public_half(key: &Path) -> PathBuf {
    let mut public = key.as_os_str().to_owned();
    public.push(".pub");
    PathBuf::from(public)
}

fn previous_signers(paths: &Paths) -> PathBuf {
    paths.root.join("allowed_signers.previous")
}

fn agent_holds(env: &Env<'_>, key: &Path) -> bool {
    let Some(sock) = &env.agent_sock else {
        return false;
    };
    env.sys
        .run(&ssh_add(sock, &["-T"]).arg(public_half(key).as_os_str()))
        .is_ok_and(|r| r.success())
}

fn signing(env: &Env<'_>, key: &Path) -> Result<Signing, String> {
    match envelope::needs_passphrase(env.sys, key) {
        Read::Value(false) => Ok(Signing::Unattended),
        Read::Value(true) if agent_holds(env, key) => Ok(Signing::Agent),
        Read::Value(true) if env.tty => Ok(Signing::Terminal),
        Read::Value(true) => Err(format!(
            "{} has a passphrase, ssh-agent does not hold it, and standard input is not a \
             terminal to ask it on",
            key.display()
        )),
        Read::Absent => Err(format!("{}: no key", key.display())),
        Read::Unusable(why) => Err(format!("{}: {why}", key.display())),
    }
}

fn seal_all(env: &Env<'_>, bodies: &[Envelope], key: &Path) -> Result<Vec<Vec<u8>>, String> {
    if bodies.is_empty() {
        return Ok(Vec::new());
    }
    let how = signing(env, key)?;
    seal_with(env, bodies, key, how)
}

/// Seal `bodies` as `how` says, an agent run reaching [`Env::agent_sock`].
fn seal_with(
    env: &Env<'_>,
    bodies: &[Envelope],
    key: &Path,
    how: Signing,
) -> Result<Vec<Vec<u8>>, String> {
    let scratch = envelope::fresh_scratch(env.sys, "seal")?;
    envelope::seal_batch(
        env.sys,
        bodies,
        key,
        how,
        scratch.path(),
        env.agent_sock.as_deref(),
    )
}

fn put(
    env: &Env<'_>,
    target: &Target,
    hello: &Hello,
    sha256: &str,
    name: &str,
    path: &Path,
) -> Result<(), String> {
    if hello.cached.iter().any(|c| c.eq_ignore_ascii_case(sha256)) {
        return Ok(());
    }
    let ran = ssh::request(
        env.sys,
        &env.paths,
        target,
        &format!("put {sha256}"),
        Some(path),
        PUT_BUDGET,
    )
    .map_err(|e| format!("upload of {name} failed: {e}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("upload of {name} failed: {}", ran.describe()))
    }
}

fn submit(env: &Env<'_>, host: &Host, wire: &[u8], budget: Duration) -> Result<HostReport, String> {
    let staged = crate::port::staging_file(env.sys, wire)
        .map_err(|e| format!("could not stage the envelope: {e}"))?;
    let ran = ssh::request(env.sys, &env.paths, &Target::of(host), "apply", Some(staged.path()), budget)
        .map_err(|e| format!("the run did not finish within this connection ({e}); if it started, `aterm machines attach {}` follows it", host.name))?;
    let (report, started) = read_stream(&ran.out(), env.sys.now());
    report.ok_or_else(|| match started {
        Some(run) => format!(
            "detached run={run} ({}); `aterm machines attach {} {run}` follows it",
            ran.describe(),
            host.name
        ),
        None => format!("the agent answered no report: {}", ran.describe()),
    })
}

fn send(
    env: &Env<'_>,
    job: &Job<'_>,
    archive: Option<&enroll::Archive>,
) -> Result<HostReport, String> {
    let target = Target::of(&job.host);
    let mut first: Vec<ItemReport> = Vec::new();
    if let (Some(wire), Some(archive)) = (&job.upgrade, archive) {
        put(
            env,
            &target,
            &job.hello,
            &archive.sha256,
            "the aterm agent",
            &archive.path,
        )?;
        let report = submit(env, &job.host, wire, UPGRADE_BUDGET).map_err(|why| {
            format!("the agent upgrade did not finish, so the baseline was not sent: {why}")
        })?;
        if let Some(why) = &report.refused {
            return Err(format!(
                "the agent upgrade was refused, so the baseline was not sent: {why}"
            ));
        }
        if report.failed() {
            let why: Vec<String> = report.items.iter().filter_map(|r| r.why.clone()).collect();
            return Err(format!(
                "the agent upgrade failed, so the baseline was not sent: {}",
                why.join("; ")
            ));
        }
        first = report.items;
        let _ = hello(env, &job.host);
    }
    for item in &job.needed {
        put(
            env,
            &target,
            &job.hello,
            &item.sha256,
            &item.name,
            &env.paths.cached(&item.sha256),
        )?;
    }
    let mut report = submit(env, &job.host, &job.wire, APPLY_BUDGET)?;
    if !first.is_empty() {
        first.append(&mut report.items);
        report.items = first;
    }
    Ok(report)
}

fn read_stream(out: &str, now: u64) -> (Option<HostReport>, Option<String>) {
    let mut items: Vec<ItemReport> = Vec::new();
    let mut started: Option<String> = None;
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        match aterm_json::from_str::<Line>(line) {
            Ok(Line::Report(report)) => return (Some(*report), started),
            Ok(Line::Item(row)) => items.push(*row),
            Ok(Line::Refused { why }) => {
                let report = HostReport {
                    schema: crate::SCHEMA,
                    mode: Mode::Plan,
                    serial: 0,
                    key_id: String::new(),
                    run_id: String::new(),
                    started: now,
                    finished: now,
                    facts: crate::model::HostFacts::default(),
                    items,
                    refused: Some(why),
                };
                return (Some(report), started);
            }
            Ok(Line::Started { run }) if crate::agent::is_run_id(&run) => started = Some(run),
            Ok(Line::Started { .. }) | Err(_) => {}
        }
    }
    (None, started)
}

fn print_report(env: &Env<'_>, host: &Host, report: &HostReport) {
    if env.json {
        outln!(
            "{}",
            aterm_json::to_string(&aterm_json::json!({"host": host.name, "report": report}))
                .unwrap_or_default()
        );
        return;
    }
    let state = HostState {
        report: Some(report.clone()),
        last_seen: env.sys.now(),
        ..HostState::default()
    };
    outln!("{}", summary(host, &state, env.sys.now()));
    for row in &report.items {
        if matches!(row.outcome, Outcome::Ok | Outcome::NotApplicable) {
            continue;
        }
        outln!("    {}", row_line(row));
    }
}

/// Format one item report row for `print_report` and `unenroll`.
pub(crate) fn row_line(row: &ItemReport) -> String {
    let change = match (&row.from, &row.to) {
        (Some(from), Some(to)) if row.outcome != Outcome::Person => {
            format!(": {} → {}", shown(from), shown(to))
        }
        _ => String::new(),
    };
    let why = row
        .why
        .as_ref()
        .map(|w| {
            let joint = if change.is_empty() { ": " } else { "; " };
            format!("{joint}{}", shown(w))
        })
        .unwrap_or_default();
    format!(
        "{:<9} {}{change}{why}",
        outcome_word(row.outcome),
        shown(&row.title)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Baseline,
    Followed,
}

fn follow_rename(env: &Env<'_>, host: &Host, report: &HostReport) -> Option<Host> {
    let address = naming::followed_address(host, report)?;
    move_address(env, host, &address, "    names     ")
}

fn move_address(env: &Env<'_>, host: &Host, address: &str, lead: &str) -> Option<Host> {
    let moved = Host {
        address: address.to_owned(),
        ..host.clone()
    };
    let (line, took) = match inventory::upsert_host(env.sys, &env.paths, &moved) {
        Ok(()) => (
            format!(
                "{lead}address {} -> {} in machines.toml",
                shown(&host.address),
                shown(address)
            ),
            true,
        ),
        Err(why) => (
            format!(
                "{lead}the Mac now answers at {}, but machines.toml still says {}: {}",
                shown(address),
                shown(&host.address),
                shown(&why)
            ),
            false,
        ),
    };
    if env.json {
        errln!("{line}");
    } else {
        outln!("{line}");
    }
    took.then_some(moved)
}

fn is_baseline_report(report: &HostReport) -> bool {
    report.items.is_empty()
        || !report
            .items
            .iter()
            .all(|r| r.family == "exec" || r.family == "agent")
}

fn after_run(env: &Env<'_>, inv: &Inventory, host: &Host, report: HostReport, source: Source) {
    after_run_with(
        env,
        inv,
        host,
        report,
        source,
        &super::github::KeyCalls::new(env.sys, &env.paths),
    );
}

/// `after_run`, its key calls made through `key_calls`, which a run shares across its hosts.
fn after_run_with(
    env: &Env<'_>,
    inv: &Inventory,
    host: &Host,
    report: HostReport,
    source: Source,
    key_calls: &super::github::KeyCalls<'_>,
) {
    let mut state = match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(s) => s,
        Err(why) => {
            errln!("aterm machines: {}'s report is not saved: {why}", host.name);
            return;
        }
    };
    let now = env.sys.now();
    state.last_seen = now;
    state.unreachable_since = None;
    state.contact_error = None;
    let only = state
        .only_sent
        .iter()
        .position(|(serial, _)| *serial == report.serial && report.serial != 0)
        .map(|at| state.only_sent.remove(at).1);
    if let Some(why) = &report.refused {
        state.last_refusal = Some((now, why.clone()));
    } else if source == Source::Baseline || is_baseline_report(&report) {
        state.last_refusal = None;
        if report.mode == Mode::Apply && only.is_none() {
            state.applied_build = Some(report.facts.os_build.clone());
        }
        if report.mode == Mode::Apply {
            if let Some(github) = &inv.github {
                register_keys(key_calls, github, host, &report, &mut state);
            } else if report
                .items
                .iter()
                .any(|r| r.id.starts_with("github.key@") && r.detail.is_some())
            {
                outln!(
                    "    github    keys made but not registered: machines.toml names no \
                     [github] account"
                );
            }
        }
        match only {
            Some(tables) => {
                let (merged, covers) =
                    merge_only(state.report.take(), state.only.take(), report, &tables);
                state.report = Some(merged);
                state.only = Some(covers);
            }
            None => {
                state.report = Some(report);
                state.only = None;
            }
        }
    }
    if let Err(e) = inventory::save_state(env.sys, &env.paths, &host.uuid, &state) {
        errln!("aterm machines: could not save {}'s state: {e}", host.name);
    }
}

fn merge_only(
    earlier: Option<HostReport>,
    covered: Option<inventory::Only>,
    report: HostReport,
    tables: &[String],
) -> (HostReport, inventory::Only) {
    let earlier = earlier.filter(|r| r.refused.is_none());
    let mut covers = match (&earlier, covered) {
        (Some(_), Some(covered)) => covered,
        (Some(whole), None) => inventory::Only {
            tables: Vec::new(),
            rest: Some(whole.finished),
        },
        (None, _) => inventory::Only::default(),
    };
    for table in tables {
        if !covers.tables.contains(table) {
            covers.tables.push(table.clone());
        }
    }
    let Some(earlier) = earlier else {
        return (report, covers);
    };
    let covered = |family: &str| tables.iter().any(|t| t == family);
    let mut items = Vec::with_capacity(earlier.items.len() + report.items.len());
    let mut fresh = Some(report.items);
    for row in earlier.items {
        if covered(&row.family) {
            if let Some(rows) = fresh.take() {
                items.extend(rows);
            }
        } else if row.family != "agent" {
            items.push(row);
        }
    }
    if let Some(rows) = fresh {
        items.extend(rows);
    }
    (HostReport { items, ..report }, covers)
}

fn key_text_of(state: &HostState, id: u64) -> Option<&str> {
    state
        .github_key_text
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, text)| text.as_str())
}

fn register_keys(
    key_calls: &super::github::KeyCalls<'_>,
    github: &inventory::Github,
    host: &Host,
    report: &HostReport,
    state: &mut HostState,
) {
    let mut reported: Vec<(String, String)> = Vec::new();
    for row in &report.items {
        let (Some(user), Some(line)) = (row.id.strip_prefix("github.key@"), &row.detail) else {
            continue;
        };
        let Some((kind, body)) = crate::items::github::key_material(line) else {
            outln!(
                "    github    {}'s key is not an OpenSSH public key; not registered",
                shown(user)
            );
            continue;
        };
        let text = format!("{kind} {body}");
        if state.github_revoked.contains(&text) {
            outln!(
                "    github    {}'s key was revoked (`aterm machines github revoke`); not \
                 registered again",
                shown(user)
            );
            continue;
        }
        reported.push((user.to_owned(), text));
    }
    let fresh: Vec<(String, String)> = reported
        .iter()
        .filter(|(user, text)| {
            !state
                .github_keys
                .iter()
                .any(|(u, id)| u == user && key_text_of(state, *id) == Some(text.as_str()))
        })
        .cloned()
        .collect();
    if !fresh.is_empty() {
        // Each key GitHub holds is recorded, whatever became of the others, so a later
        // revocation can find it.
        let registration = key_calls.register(github, &host.name, &fresh);
        for r in registration.done {
            let Some((_, text)) = fresh.iter().find(|(u, _)| *u == r.user) else {
                continue;
            };
            outln!(
                "    github    registered {}'s key on {} (id {})",
                shown(&r.user),
                github.account,
                r.id
            );
            if !state.github_keys.contains(&(r.user.clone(), r.id)) {
                state.github_keys.push((r.user.clone(), r.id));
            }
            state.github_key_text.retain(|(id, _)| *id != r.id);
            state.github_key_text.push((r.id, text.clone()));
        }
        if !registration.failed.is_empty() {
            outln!(
                "    github    not registered: {}",
                shown(&registration.failed.join("; "))
            );
        }
    }
    let stale: Vec<u64> = state
        .github_keys
        .iter()
        .filter(|(user, id)| {
            reported.iter().any(|(u, text)| {
                u == user
                    && key_text_of(state, *id) != Some(text.as_str())
                    && state
                        .github_keys
                        .iter()
                        .any(|(u2, id2)| u2 == u && key_text_of(state, *id2) == Some(text.as_str()))
            })
        })
        .map(|(_, id)| *id)
        .collect();
    if stale.is_empty() {
        return;
    }
    match key_calls.revoke(github, &stale) {
        Ok(_) => {
            outln!(
                "    github    deleted {} replaced key(s) from {}",
                stale.len(),
                github.account
            );
            state.github_keys.retain(|(_, id)| !stale.contains(id));
            state.github_key_text.retain(|(id, _)| !stale.contains(id));
        }
        Err(why) => outln!(
            "    github    the replaced key(s) are still registered: {}",
            shown(&why)
        ),
    }
}

fn one_host<'i>(inv: &'i Inventory, name: &str, verb: &str) -> Result<&'i Host, ExitCode> {
    match inventory::select(inv, &[name.to_owned()], false) {
        Ok(h) if h.len() == 1 => Ok(h[0]),
        Ok(_) => {
            errln!("aterm machines {verb}: {name:?} names more than one host");
            Err(ExitCode::from(2))
        }
        Err(e) => {
            errln!("aterm machines {verb}: {e}");
            Err(ExitCode::from(2))
        }
    }
}

/// `aterm machines attach <host> [<run>]`: follow a run that outlived its connection.
#[must_use = "handle or propagate the command's exit status"]
pub fn attach(env: &Env<'_>, name: &str, run: Option<&str>) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, "attach") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let reached = reach_one(env, host);
    let host: &Host = &reached.host;
    let hello = match reached.hello {
        Ok(h) => h,
        Err(why) => {
            errln!(
                "aterm machines attach: {}: {}",
                host.name,
                shown(&why.why())
            );
            return if why.unreachable() {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            };
        }
    };
    let run = match run {
        Some(r) => r.to_owned(),
        None => match hello.busy.or(hello.last_run) {
            Some(r) => r,
            None => {
                errln!("aterm machines attach: {} has no run to follow", host.name);
                return ExitCode::from(2);
            }
        },
    };
    if !run.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        errln!("aterm machines attach: {run:?} is not a run id");
        return ExitCode::from(2);
    }
    let ran = match ssh::request(
        env.sys,
        &env.paths,
        &Target::of(host),
        &format!("attach {run}"),
        None,
        APPLY_BUDGET,
    ) {
        Ok(r) => r,
        Err(e) => {
            errln!("aterm machines attach: {}: {e}", host.name);
            return ExitCode::FAILURE;
        }
    };
    let Some(report) = read_stream(&ran.out(), env.sys.now()).0 else {
        errln!(
            "aterm machines attach: {}: no report ({})",
            host.name,
            shown(&ran.describe())
        );
        return ExitCode::FAILURE;
    };
    let failed = report.failed();
    print_report(env, host, &report);
    let _ = follow_rename(env, host, &report);
    after_run(env, &inv, host, report, Source::Followed);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `aterm machines run <host…|--all> [--user <u>] -- <argv>`: run one signed command.
#[must_use = "handle or propagate the command's exit status"]
pub fn run(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    user: Option<&str>,
    argv: &[String],
) -> ExitCode {
    let Some(program) = argv.first() else {
        errln!("aterm machines run: name a command after --");
        return ExitCode::from(2);
    };
    if !program.starts_with('/') {
        errln!(
            "aterm machines run: {program:?} must be an absolute path (no shell, no PATH lookup)"
        );
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match fleet_hosts(&inv, names, all, "run") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let parallel = parallel(env, &inv);
    let reached = hello_sweep(env, &hosts, parallel);
    if let Some(stop) = broker_wall(&reached) {
        errln!("{stop}");
        return ExitCode::FAILURE;
    }
    let principals = principals(&inv, env);
    let mut tally = Tally::default();
    let mut targets: Vec<Cow<'_, Host>> = Vec::new();
    let mut bodies = Vec::new();
    for r in reached {
        let hello = match r.hello {
            Ok(h) => h,
            Err(why) => {
                not_reached(&mut tally, &r.host, &why);
                continue;
            }
        };
        let exec = Exec {
            argv: argv.to_vec(),
            user: user.map(str::to_owned),
            budget_secs: 3600,
        };
        match exec_body(env, &r.host, floor_of(&hello, &principals), exec) {
            Ok(b) => {
                bodies.push(b);
                targets.push(r.host);
            }
            Err(why) => {
                errln!("aterm machines run: {why}");
                return ExitCode::FAILURE;
            }
        }
    }
    let wires = match seal_all(env, &bodies, &env.paths.signing_key()) {
        Ok(w) => w,
        Err(why) => {
            errln!("aterm machines run: {why}");
            return ExitCode::FAILURE;
        }
    };
    let jobs: Vec<(Cow<'_, Host>, Vec<u8>)> = targets.into_iter().zip(wires).collect();
    let outcomes = ssh::fan_out(&jobs, parallel, |(host, wire)| {
        submit(env, host, wire, Duration::from_secs(3600 + 60))
    });
    for ((host, _), outcome) in jobs.iter().zip(outcomes) {
        match outcome {
            Ok(report) => {
                tally.failed |= report.failed();
                for line in exec_lines(&report) {
                    outln!("{:<16} {line}", host.name);
                }
            }
            Err(why) => {
                tally.failed = true;
                outln!("{:<16} {}", host.name, shown(&why));
            }
        }
    }
    tally.code()
}

fn exec_lines(report: &HostReport) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(why) = &report.refused {
        out.push(format!("refused: {}", shown(why)));
    }
    for row in &report.items {
        out.push(shown(row.to.as_deref().unwrap_or("did not run")));
        for line in row.detail.iter().flat_map(|d| d.lines()) {
            out.push(format!("  {}", shown(line)));
        }
        for line in row.why.iter().flat_map(|d| d.lines()) {
            out.push(format!("! {}", shown(line)));
        }
    }
    out
}

fn exec_body(env: &Env<'_>, host: &Host, floor: u64, exec: Exec) -> Result<Envelope, String> {
    let mut envelope = body(env, host, floor, Action::Exec, Disrupt::Now)?;
    envelope.idle_secs = 0;
    envelope.exec = Some(exec);
    Ok(envelope)
}

/// `aterm machines person [<host>…|--all]`: list outstanding interactive person steps.
#[must_use = "handle or propagate the command's exit status"]
pub fn person(env: &Env<'_>, names: &[String], all: bool) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match inventory::select(&inv, names, all || names.is_empty()) {
        Ok(h) => h,
        Err(e) => {
            errln!("aterm machines person: {e}");
            return ExitCode::from(2);
        }
    };
    let now = env.sys.now();
    let mut tally = Tally::default();
    let mut by_step: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut unknown: Vec<&str> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for host in hosts {
        let Some(state) = read_state(env, host, &mut tally) else {
            continue;
        };
        let Some(report) = &state.report else {
            unknown.push(&host.name);
            continue;
        };
        if let Some(why) = &report.refused {
            tally.failed = true;
            notes.push(format!(
                "{}: not measured (the last run was refused: {}); run `aterm machines plan {}`",
                host.name,
                shown(why),
                host.name
            ));
            continue;
        }
        if let Some((at, why)) = &state.last_refusal {
            tally.failed = true;
            notes.push(format!(
                "{}: the agent refused the newest run {} ago ({}); its steps are from the \
                 report before it",
                host.name,
                age(now.saturating_sub(*at)),
                shown(why)
            ));
        }
        for step in screen::prepare_steps(report) {
            let who = if step.measured {
                host.name.clone()
            } else {
                tally.failed = true;
                format!(
                    "{} (not measured: {})",
                    host.name,
                    shown(step.why.as_deref().unwrap_or("no reading"))
                )
            };
            by_step
                .entry(step.id())
                .or_insert_with(|| (step.how(), Vec::new()))
                .1
                .push(who);
        }
    }
    if by_step.is_empty() && unknown.is_empty() && notes.is_empty() {
        outln!("no person steps outstanding");
    }
    for (id, (how, hosts)) in &by_step {
        outln!("{id}: {how}");
        outln!("    {}", hosts.join(" "));
    }
    for note in &notes {
        outln!("{note}");
    }
    if !unknown.is_empty() {
        outln!(
            "never planned (run `aterm machines plan` first): {}",
            unknown.join(" ")
        );
    }
    if !by_step.is_empty() {
        outln!(
            "At the Mac: `aterm machines person <host> --here`. From here: `aterm machines \
             screen <host>`."
        );
    }
    tally.code()
}

struct AgentKey<'a> {
    sys: &'a dyn System,
    sock: OsString,
    key: PathBuf,
}

impl Drop for AgentKey<'_> {
    fn drop(&mut self) {
        let _ = self
            .sys
            .run(&ssh_add(&self.sock, &["-d"]).arg(self.key.as_os_str()));
    }
}

fn ssh_add(sock: &OsStr, args: &[&str]) -> Cmd {
    Cmd::new("/usr/bin/ssh-add")
        .args(args)
        .secret_env("SSH_AUTH_SOCK", sock.to_string_lossy())
}

fn person_signing<'a>(env: &Env<'a>) -> Result<(Signing, Option<AgentKey<'a>>), String> {
    let key = env.paths.signing_key();
    match envelope::needs_passphrase(env.sys, &key) {
        Read::Value(false) => return Ok((Signing::Unattended, None)),
        Read::Value(true) => {}
        Read::Absent => return Err(format!("{}: no key", key.display())),
        Read::Unusable(why) => return Err(format!("{}: {why}", key.display())),
    }
    let Some(sock) = env.agent_sock.clone() else {
        return Err(format!(
            "{} has a passphrase and no ssh-agent is running (SSH_AUTH_SOCK is not set): \
             every re-probe signs an envelope, so the key must be in an agent",
            key.display()
        ));
    };
    if agent_holds(env, &key) {
        return Ok((Signing::Agent, None));
    }
    errln!(
        "The signing key goes into ssh-agent for this walk (removed when it ends, or after an \
         hour if the walk is interrupted); ssh-add asks its passphrase:"
    );
    match env.sys.run_tty(
        &ssh_add(&sock, &["-t", AGENT_KEY_LIFETIME]).arg(key.as_os_str()),
        &mut |_| Ok(()),
    ) {
        Ok(Some(0)) => Ok((
            Signing::Agent,
            Some(AgentKey {
                sys: env.sys,
                sock,
                key,
            }),
        )),
        Ok(code) => Err(format!("ssh-add did not load the key (status {code:?})")),
        Err(e) => Err(format!("ssh-add: {e}")),
    }
}

struct LiveRemote<'a> {
    env: &'a Env<'a>,
    host: &'a Host,
    floor: u64,
    how: Signing,
}

impl screen::Remote for LiveRemote<'_> {
    fn exec(&self, exec: &Exec) -> Result<Ran, String> {
        let body = exec_body(self.env, self.host, self.floor, exec.clone())?;
        let wire = seal_with(
            self.env,
            std::slice::from_ref(&body),
            &self.env.paths.signing_key(),
            self.how,
        )?
        .pop()
        .ok_or_else(|| "ssh-keygen signed nothing".to_owned())?;
        let budget = Duration::from_secs(exec.budget_secs.saturating_add(60));
        let report = submit(self.env, self.host, &wire, budget)?;
        if let Some(why) = report.refused {
            return Err(format!("refused: {why}"));
        }
        let row = report
            .items
            .first()
            .ok_or_else(|| "the exec report has no row".to_owned())?;
        crate::agent::job::exec_ran(row)
    }
}

fn report_for(env: &Env<'_>, host: &Host) -> Result<HostReport, String> {
    let saved = inventory::load_state(env.sys, &env.paths, &host.uuid)?;
    if let Some(report) = saved.report.filter(|r| r.refused.is_none()) {
        return Ok(report);
    }
    outln!(
        "{}: no plan or apply report yet; planning it first",
        host.name
    );
    let _ = plan_or_apply(
        env,
        std::slice::from_ref(&host.uuid),
        false,
        Mode::Plan,
        false,
    );
    inventory::load_state(env.sys, &env.paths, &host.uuid)?
        .report
        .filter(|r| r.refused.is_none())
        .ok_or_else(|| format!("{} answered no report to plan", host.name))
}

fn waiting_for_aterm(
    report: &HostReport,
    steps: Vec<screen::Step>,
    host: &Host,
) -> (Vec<screen::Step>, Vec<screen::Walked>) {
    let missing = report.items.iter().any(|row| {
        row.id == "aterm.app"
            && row.from.as_deref() == Some("absent")
            && row.outcome != Outcome::Changed
    });
    if !missing {
        return (steps, Vec::new());
    }
    let (wait, walk): (Vec<screen::Step>, Vec<screen::Step>) = steps
        .into_iter()
        .partition(|s| s.step.bundle.as_deref() == Some(crate::items::person::APPS[0].1));
    let waiting = wait
        .into_iter()
        .map(|s| screen::Walked {
            step: s.id(),
            done: screen::Done::NotGranted(format!(
                "not attempted: aterm.app is not installed on {} yet; `aterm machines apply \
                 {}` first, then this again",
                host.name, host.name
            )),
        })
        .collect();
    (walk, waiting)
}

fn walk_steps(
    report: &HostReport,
    host: &Host,
    at_console: bool,
    step: Option<&str>,
) -> Result<(Vec<screen::Step>, Vec<screen::Walked>), String> {
    let mut steps = screen::prepare_steps(report);
    if !at_console && let Some(off) = screen::screen_sharing_outstanding(&steps) {
        return Err(screen::screen_sharing_off(&host.address, off));
    }
    if let Some(id) = step {
        steps = screen::only(steps, id)?;
    }
    Ok(waiting_for_aterm(report, steps, host))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Said {
    Step(String),
    Plan,
    Last(String),
}

fn end_walk(
    host: &Host,
    verb: &str,
    walked: &[screen::Walked],
    session: &screen::Session,
    stops: Box<dyn std::any::Any>,
    emit: &mut dyn FnMut(Said),
) -> ExitCode {
    let (failed, changed) = tell(host, walked, emit);
    if changed {
        emit(Said::Plan);
    }
    let code = if let Some(line) = session.unlocked_line(&host.name, &host.user) {
        emit(Said::Last(format!(
            "aterm machines {verb}: {}",
            shown(&line)
        )));
        ExitCode::FAILURE
    } else if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    drop(stops);
    code
}

fn tell(host: &Host, walked: &[screen::Walked], emit: &mut dyn FnMut(Said)) -> (bool, bool) {
    let mut failed = false;
    let mut changed = false;
    for w in walked {
        let line = match &w.done {
            screen::Done::AlreadyGranted => {
                changed = true;
                "already granted".to_owned()
            }
            screen::Done::Granted => {
                changed = true;
                "granted".to_owned()
            }
            screen::Done::AtConsole(how) => format!("at the Mac's keyboard: {how}"),
            screen::Done::NotGranted(why) => {
                failed = true;
                format!("not granted: {}", shown(why))
            }
        };
        emit(Said::Step(format!("{}: {}: {line}", host.name, w.step)));
    }
    (failed, changed)
}

/// `aterm machines person <host> --here` and `screen <host>`: walk pending person steps.
#[must_use = "handle or propagate the command's exit status"]
pub fn walk(
    env: &Env<'_>,
    name: &str,
    at_console: bool,
    step: Option<&str>,
    terminals: screen::Terminals,
    opener: &dyn screen::Opener,
) -> ExitCode {
    use std::io::Write as _;
    let verb = if at_console {
        "person --here"
    } else {
        "screen"
    };
    if let Err(why) = screen::require_terminals(verb, terminals.stdin, terminals.stderr) {
        errln!("{why}");
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, verb) {
        Ok(h) => h,
        Err(code) => return code,
    };
    let (how, agent_key) = match person_signing(env) {
        Ok(signing) => signing,
        Err(why) => {
            errln!("aterm machines {verb}: {}: {why}", host.name);
            return ExitCode::FAILURE;
        }
    };
    let walked = (|| {
        let report = report_for(env, host)?;
        let admin = screen::account(&report, &host.user)?;
        let (steps, waiting) = walk_steps(&report, host, at_console, step)?;
        let reached = reach_one(env, host);
        let host: &Host = &reached.host;
        let hello = reached.hello.map_err(|e| e.why())?;
        let remote = LiveRemote {
            env,
            host,
            floor: floor_of(&hello, &principals(&inv, env)),
            how,
        };
        let clock = screen::WallClock::new();
        let say = |line: &str| {
            let _ = writeln!(std::io::stderr(), "{}: {}", host.name, shown(line));
        };
        let flow = screen::Flow {
            sys: env.sys,
            remote: &remote,
            clock: &clock,
            say: &say,
            hang: screen::PERSON_HANG,
        };
        let nothing: Box<dyn std::any::Any> = Box::new(());
        let (mut walked, session, stops) = if steps.is_empty() && !waiting.is_empty() {
            (Vec::new(), screen::Session::Kept, nothing)
        } else if at_console {
            (
                screen::here(&flow, &admin, &steps, terminals)?,
                screen::Session::Kept,
                nothing,
            )
        } else {
            let screened = screen::screen(
                &flow,
                &env.paths,
                &Target::of(host),
                &admin,
                &steps,
                opener,
                terminals,
            )?;
            (screened.walked, screened.session, screened.stops)
        };
        walked.extend(waiting);
        Ok::<_, String>((walked, session, stops))
    })();
    let (walked, session, stops) = match walked {
        Ok(w) => w,
        Err(why) => {
            errln!("aterm machines {verb}: {}: {}", host.name, shown(&why));
            return ExitCode::FAILURE;
        }
    };
    let code = end_walk(
        host,
        verb,
        &walked,
        &session,
        stops,
        &mut |said| match said {
            Said::Step(line) => {
                let _ = writeln!(std::io::stdout(), "{line}");
            }
            Said::Plan => {
                let _ = plan_or_apply(
                    env,
                    std::slice::from_ref(&host.uuid),
                    false,
                    Mode::Plan,
                    false,
                );
            }
            Said::Last(line) => {
                let _ = writeln!(std::io::stderr(), "{line}");
            }
        },
    );
    drop(agent_key);
    code
}

/// `aterm machines github revoke <host>`: delete the host's registered GitHub SSH keys.
#[must_use = "handle or propagate the command's exit status"]
pub fn github_revoke(env: &Env<'_>, name: &str) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, "github revoke") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let Some(github) = &inv.github else {
        errln!("aterm machines github revoke: machines.toml names no [github] account");
        return ExitCode::from(2);
    };
    let mut state = match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(s) => s,
        Err(why) => {
            errln!("aterm machines github revoke: {why}");
            return ExitCode::FAILURE;
        }
    };
    if state.github_keys.is_empty() {
        outln!("{}: no GitHub keys recorded", host.name);
        return ExitCode::SUCCESS;
    }
    let ids: Vec<u64> = state.github_keys.iter().map(|(_, id)| *id).collect();
    let deleted = match super::github::revoke_via(env.sys, &env.paths, github, &ids) {
        Ok(deleted) => deleted,
        Err(why) => {
            errln!("aterm machines github revoke: {}: {why}", host.name);
            return ExitCode::FAILURE;
        }
    };
    let recorded = ids
        .iter()
        .filter_map(|id| key_text_of(&state, *id).map(str::to_owned));
    let revoked: Vec<String> = deleted.into_iter().chain(recorded).collect();
    for key in revoked {
        if !state.github_revoked.contains(&key) {
            state.github_revoked.push(key);
        }
    }
    outln!(
        "{}: deleted {} key(s) from {}; they are not registered again (a key the host makes \
         anew is)",
        host.name,
        ids.len(),
        github.account
    );
    state.github_keys.clear();
    state.github_key_text.clear();
    if let Err(why) = inventory::save_state(env.sys, &env.paths, &host.uuid, &state) {
        errln!("aterm machines github revoke: {why}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// `aterm machines github login`: prompt for the `[github]` token and store it in the
/// controller's `keys/github_token` (0600), never the login keychain.
#[must_use = "handle or propagate the command's exit status"]
pub fn github_login(env: &Env<'_>) -> ExitCode {
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let Some(github) = &inv.github else {
        errln!("aterm machines github login: machines.toml names no [github] account");
        return ExitCode::from(2);
    };
    if github.keys_via() == super::inventory::KeysVia::AghFleet {
        // Under 5b the key calls read agh.toml's fleet profile and never this token file, so
        // a token stored here would go unused while the login said otherwise.
        errln!(
            "aterm machines github login: keys = \"agh-fleet\" reads agh.toml's fleet profile, \
             not keys/github_token: agh auth add fleet --file PATH --tier dev (a token of \
             {}'s with the {} scope); to store a token for keys = \"gh\" or \"agh-inline\", \
             set that first",
            github.account,
            super::github::KEY_SCOPE
        );
        return ExitCode::from(2);
    }
    if !env.tty {
        errln!(
            "aterm machines github login: needs a terminal, where the token is typed with \
             its echo off"
        );
        return ExitCode::from(2);
    }
    let prompt = format!(
        "A GitHub token of {}'s with the {} scope (not echoed): ",
        github.account,
        super::github::KEY_SCOPE
    );
    let token = match env.sys.ask(&prompt, true) {
        Ok(Some(token)) => token,
        Ok(None) => {
            errln!("aterm machines github login: no token typed; nothing was stored");
            return ExitCode::FAILURE;
        }
        Err(why) => {
            errln!("aterm machines github login: {why}");
            return ExitCode::FAILURE;
        }
    };
    let token_file = env.paths.github_token();
    match super::github::login(env.sys, &token_file, &github.account, token) {
        Ok(()) => {
            outln!(
                "stored {}'s GitHub token in {} (0600); host keys register with it from the \
                 next apply",
                github.account,
                token_file.display()
            );
            ExitCode::SUCCESS
        }
        Err(why) => {
            errln!("aterm machines github login: {why}");
            ExitCode::FAILURE
        }
    }
}

/// `aterm machines unenroll <host> [--unreachable]`: remove a host from the fleet.
#[must_use = "handle or propagate the command's exit status"]
pub fn unenroll(env: &Env<'_>, name: &str, unreachable: bool) -> ExitCode {
    if !env.tty {
        errln!(
            "aterm machines unenroll: not run: it removes a Mac from the fleet, and standard \
             input or error is not a terminal"
        );
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let host = match one_host(&inv, name, "unenroll") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let host = if unreachable {
        host.clone()
    } else {
        reach_one(env, host).host.into_owned()
    };
    let principals = principals(&inv, env);
    let mut out = std::io::stdout().lock();
    match enroll::unenroll(
        env.sys,
        &env.paths,
        &principals,
        &host,
        unreachable,
        &mut out,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            errln!("aterm machines unenroll: {}", shown(&why));
            ExitCode::FAILURE
        }
    }
}

fn signer_entries(text: &str) -> Vec<(&str, &str, &str)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                [principal, _options, kind, blob] => Some((*principal, *kind, *blob)),
                _ => None,
            }
        })
        .collect()
}

fn principals_of_key(sys: &dyn System, public: &Path, list: &Path) -> Result<Vec<String>, String> {
    let key = sys
        .read_file(public)
        .map_err(|e| format!("{}: {e}", public.display()))?;
    let key = String::from_utf8_lossy(&key);
    let (kind, blob) = envelope::public_key_parts(key.trim())?;
    let signers = sys
        .read_file(list)
        .map_err(|e| format!("{}: {e}", list.display()))?;
    let signers = String::from_utf8_lossy(&signers);
    let names: Vec<String> = signer_entries(&signers)
        .into_iter()
        .filter(|(_, k, b)| *k == kind && *b == blob)
        .map(|(p, ..)| p.to_owned())
        .collect();
    if names.is_empty() {
        return Err(format!("{} is not in {}", public.display(), list.display()));
    }
    Ok(names)
}

fn public_of(env: &Env<'_>, key: &Path) -> Result<PathBuf, String> {
    let beside = public_half(key);
    if env.sys.stat(&beside).is_ok() {
        return Ok(beside);
    }
    let kept = key
        .file_name()
        .map(|name| public_half(&env.paths.keys().join(name)));
    match kept {
        Some(kept) if env.sys.stat(&kept).is_ok() => Ok(kept),
        Some(kept) => Err(format!(
            "no public key for {}: neither {} nor {} exists",
            key.display(),
            beside.display(),
            kept.display()
        )),
        None => Err(format!("{} names no key file", key.display())),
    }
}

/// `aterm machines rekey`: replace the `allowed_signers` file trusted by selected hosts.
#[must_use = "handle or propagate the command's exit status"]
pub fn rekey(
    env: &Env<'_>,
    names: &[String],
    all: bool,
    signers: &Path,
    key: Option<&Path>,
) -> ExitCode {
    let text = match env.sys.read_file(signers) {
        Ok(t) => String::from_utf8_lossy(&t).into_owned(),
        Err(e) => {
            errln!("aterm machines rekey: {}: {e}", signers.display());
            return ExitCode::from(2);
        }
    };
    if let Err(why) = envelope::check_signers(&text) {
        errln!("aterm machines rekey: {}: {why}", signers.display());
        return ExitCode::from(2);
    }
    let own = public_half(&env.paths.signing_key());
    let own_listed = env
        .sys
        .read_file(&own)
        .ok()
        .and_then(|k| {
            let k = String::from_utf8_lossy(&k).into_owned();
            envelope::public_key_parts(k.trim())
                .ok()
                .map(|(kind, blob)| {
                    signer_entries(&text)
                        .iter()
                        .any(|(_, k, b)| *k == kind && *b == blob)
                })
        })
        .unwrap_or(false);
    if !own_listed {
        errln!(
            "aterm machines rekey: {} does not list this controller's signing key ({}): the \
             fleet would stop accepting it",
            signers.display(),
            own.display()
        );
        return ExitCode::from(2);
    }
    let inv = match load(env) {
        Ok(inv) => inv,
        Err(code) => return code,
    };
    let hosts = match fleet_hosts(&inv, names, all, "rekey") {
        Ok(h) => h,
        Err(code) => return code,
    };
    let key = key.map_or_else(|| env.paths.signing_key(), Path::to_path_buf);
    let principals = match public_of(env, &key)
        .and_then(|public| principals_of_key(env.sys, &public, &env.paths.allowed_signers()))
    {
        Ok(p) => p,
        Err(why) => {
            errln!("aterm machines rekey: {why}");
            return ExitCode::from(2);
        }
    };
    let parallel = parallel(env, &inv);
    let mut tally = Tally::default();
    let mut targets = Vec::new();
    let mut bodies = Vec::new();
    let reached = hello_sweep(env, &hosts, parallel);
    if let Some(stop) = broker_wall(&reached) {
        errln!("{stop}");
        return ExitCode::FAILURE;
    }
    for r in reached {
        let hello = match r.hello {
            Ok(h) => h,
            Err(why) => {
                not_reached(&mut tally, &r.host, &why);
                continue;
            }
        };
        match body(
            env,
            &r.host,
            floor_of(&hello, &principals),
            Action::Rekey,
            Disrupt::Now,
        ) {
            Ok(mut b) => {
                b.rekey = Some(text.clone());
                bodies.push(b);
                targets.push(r.host);
            }
            Err(why) => {
                errln!("aterm machines rekey: {why}");
                return ExitCode::FAILURE;
            }
        }
    }
    let wires = match seal_all(env, &bodies, &key) {
        Ok(w) => w,
        Err(why) => {
            errln!("aterm machines rekey: {why}");
            return ExitCode::FAILURE;
        }
    };
    let jobs: Vec<(Cow<'_, Host>, Vec<u8>)> = targets.into_iter().zip(wires).collect();
    let outcomes = ssh::fan_out(&jobs, parallel, |(host, wire)| {
        submit(env, host, wire, Duration::from_secs(120))
    });
    let digest = text_sha256(&text);
    let mut took = 0;
    for ((host, _), outcome) in jobs.iter().zip(outcomes) {
        match outcome {
            Ok(r) if r.refused.is_none() && !r.failed() => {
                took += 1;
                outln!("{:<16} rekeyed", host.name);
                record_signers(env, host, &digest);
            }
            Ok(r) => {
                tally.failed = true;
                let why: Vec<String> = r
                    .refused
                    .into_iter()
                    .chain(r.items.into_iter().filter_map(|i| i.why))
                    .collect();
                outln!("{:<16} not rekeyed: {}", host.name, shown(&why.join("; ")));
            }
            Err(why) => {
                tally.failed = true;
                outln!("{:<16} not rekeyed: {}", host.name, shown(&why));
            }
        }
    }
    if took == 0 {
        outln!(
            "no host took the new list; the controller's {} is unchanged",
            env.paths.allowed_signers().display()
        );
        return tally.code();
    }
    if let Err(why) = replace_signers(env, &text) {
        tally.failed = true;
        errln!("aterm machines rekey: {why}");
        return tally.code();
    }
    outln!(
        "the controller's {} is now {} (the old list is {})",
        env.paths.allowed_signers().display(),
        signers.display(),
        previous_signers(&env.paths).display()
    );
    let behind: Vec<&str> = inv
        .hosts
        .iter()
        .filter(|h| {
            inventory::load_state(env.sys, &env.paths, &h.uuid)
                .ok()
                .and_then(|s| s.signers_sha256)
                .is_none_or(|d| d != digest)
        })
        .map(|h| h.name.as_str())
        .collect();
    if !behind.is_empty() {
        outln!(
            "not known to carry the new list yet (rekey them when they answer): {}",
            behind.join(" ")
        );
    }
    tally.code()
}

fn text_sha256(text: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(text.as_bytes()))
}

fn record_signers(env: &Env<'_>, host: &Host, digest: &str) {
    match inventory::load_state(env.sys, &env.paths, &host.uuid) {
        Ok(mut state) => {
            state.signers_sha256 = Some(digest.to_owned());
            if let Err(why) = inventory::save_state(env.sys, &env.paths, &host.uuid, &state) {
                errln!("aterm machines rekey: {why}");
            }
        }
        Err(why) => errln!("aterm machines rekey: {}: not recorded: {why}", host.name),
    }
}

fn replace_signers(env: &Env<'_>, text: &str) -> Result<(), String> {
    let current = env.paths.allowed_signers();
    match env.sys.read_file(&current) {
        Ok(old) if old == text.as_bytes() => return Ok(()),
        Ok(old) => env
            .sys
            .write_file(&previous_signers(&env.paths), &old, 0o644)
            .map_err(|e| format!("{}: {e}", previous_signers(&env.paths).display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}; not replaced", current.display())),
    }
    env.sys
        .write_file(&current, text.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", current.display()))
}

/// How `aterm machines here` speaks ([`here`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HereOutput {
    /// The report: the host's status line and each row that is not in place.
    Report,
    /// The report as JSON (`--json`).
    Json,
    /// Nothing but the items' own progress lines (`--plain`, `aterm init`'s root half for
    /// Apple's Command Line Tools): the run applies the tools' own baseline
    /// ([`crate::init_clt::CommandLineTools::baseline`]), reading no file the person's account
    /// could write, and ends on 0 when done, else on the code of why it did not finish
    /// ([`crate::init_clt::Unfinished::code`]), which `aterm init` says in plain words.
    Plain,
}

/// The baseline's authority: an explicit file or one of setup's compiled root plans.
#[derive(Clone, Copy, Debug)]
pub enum HereBaseline<'a> {
    /// A person's explicitly named baseline.
    File(&'a Path),
    /// Setup's compiled plan; no account-writable file is read.
    BuiltIn(crate::init_clt::RootPlan),
}

/// `aterm machines here plan|apply <baseline.toml>`: run a baseline directly on this Mac; with
/// [`HereOutput::Plain`], setup's compiled root plan; a file source is refused.
#[must_use = "handle or propagate the command's exit status"]
pub fn here(
    sys: &dyn System,
    mode: Mode,
    source: HereBaseline<'_>,
    name: Option<&str>,
    output: HereOutput,
) -> ExitCode {
    match here_report(sys, mode, source, name, output) {
        Ok(Some((host, report))) => {
            print_report(
                &Env {
                    sys,
                    paths: Paths {
                        root: std::path::PathBuf::from("/nonexistent"),
                    },
                    json: output == HereOutput::Json,
                    tty: false,
                    parallel: None,
                    agent_sock: None,
                },
                &host,
                &report,
            );
            if report.failed() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// [`here`]'s run: the host and its report to print, `None` when a plain run finished, or the
/// exit code it ends on (a plain run's class of why it did not finish). A plain run also turns
/// developer mode on and puts the rule in place that names the Mac after admission
/// ([`crate::root_steps`]), and with `name` (a [`crate::init_clt::plain_name`]) gives the Mac
/// that name; whether those rows changed or failed goes to the journal, and the exit says the
/// tools alone, so a step macOS refused never reads as tools that did not install.
fn here_report(
    sys: &dyn System,
    mode: Mode,
    source: HereBaseline<'_>,
    name: Option<&str>,
    output: HereOutput,
) -> Result<Option<(Host, HostReport)>, ExitCode> {
    let plain = output == HereOutput::Plain;
    let other = || ExitCode::from(crate::init_clt::Unfinished::Other.code().unwrap_or(u8::MAX));
    // A plain run says nothing of its own: the tab says why it did not finish.
    macro_rules! refuse {
        ($code:expr, $($why:tt)*) => {{
            if plain {
                return Err(other());
            }
            errln!($($why)*);
            return Err($code);
        }};
    }
    if mode == Mode::Apply && sys.euid() != 0 {
        refuse!(
            ExitCode::from(2),
            "aterm machines here apply: needs root: run it with sudo"
        );
    }
    // The root half trusts nothing the person's account can write: a plain run applies the
    // tools' own baseline, built in.
    let text = match (plain, source) {
        (true, HereBaseline::BuiltIn(plan)) => {
            crate::init_clt::CommandLineTools::baseline_named(plan, name)
        }
        (false, HereBaseline::File(path)) => match sys.read_file(path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => refuse!(
                ExitCode::FAILURE,
                "aterm machines here: {}: {e}",
                path.display()
            ),
        },
        _ => refuse!(
            ExitCode::from(2),
            "aterm machines here: takes plan|apply <baseline.toml>"
        ),
    };
    let mut facts = crate::facts::gather_without_keyboards(sys);
    let name = facts.local_host_name.clone();
    let text = match crate::baseline::fill(
        &text,
        &[
            ("name", &name),
            ("tag", crate::items::names::tag(&name)),
            ("serial", &facts.serial),
            ("uuid", &facts.uuid),
        ],
    ) {
        Ok(t) => t,
        Err(e) => refuse!(ExitCode::FAILURE, "aterm machines here: {e}"),
    };
    let mut baseline = match crate::baseline::parse(&text) {
        Ok(b) => b,
        Err(e) => refuse!(ExitCode::FAILURE, "aterm machines here: {e}"),
    };
    if plain && let Some(developer) = baseline.developer.as_mut() {
        developer.retained_root_run = true;
    }
    if plain && let Some(lan) = baseline.lan.as_mut() {
        lan.retained_root_run = true;
    }
    if baseline
        .keyboard
        .as_ref()
        .is_some_and(|table| table.caps_lock.is_some())
    {
        facts.keyboards = crate::facts::inventory_keyboards(sys);
    }
    let mut users = Vec::new();
    let wanted: Vec<String> = if baseline.users.is_empty() {
        facts.console_user.iter().cloned().collect()
    } else {
        baseline.users.clone()
    };
    for u in &wanted {
        match crate::facts::user(sys, u) {
            Ok(user) => users.push(user),
            Err(e) => refuse!(ExitCode::FAILURE, "aterm machines here: {e}"),
        }
    }
    let console = facts
        .console_user
        .as_ref()
        .and_then(|c| crate::facts::user(sys, c).ok());
    let ctx = crate::items::Ctx {
        cache_dir: Path::new(crate::AGENT_ROOT).join("cache"),
        disrupt: true,
        facts,
        users,
        console,
        host_name: name,
    };
    let items = match crate::items::expand(&baseline, &ctx) {
        Ok(i) => i,
        Err(e) => refuse!(ExitCode::FAILURE, "aterm machines here: {e}"),
    };
    let mut journal = match sys.journal() {
        Ok(journal) => journal,
        Err(why) => refuse!(ExitCode::FAILURE, "aterm machines here: {why}"),
    };
    let remedy = aterm_converge::Remedy::Command(match source {
        HereBaseline::BuiltIn(crate::init_clt::RootPlan::WithTools) => {
            "sudo aterm machines here apply --plain".into()
        }
        HereBaseline::BuiltIn(crate::init_clt::RootPlan::StepsOnly) => {
            "sudo aterm machines here apply --plain --root-steps-only".into()
        }
        HereBaseline::File(path) => format!("sudo aterm machines here apply {}", path.display()),
    });
    let started = sys.now();
    let rows = match crate::engine::run_with(
        &items,
        mode,
        sys,
        &ctx,
        &remedy,
        journal.as_mut(),
        &mut |_| {},
    ) {
        Ok(rows) => rows,
        Err(refused) => refuse!(ExitCode::FAILURE, "aterm machines here: {refused}"),
    };
    if plain {
        // The tools and an applicable broker must finish before accepted root custody
        // ends successfully. Optional naming and debugger failures remain journaled;
        // they cannot cause another automatic administrator episode.
        let unfinished = rows
            .iter()
            .filter(|row| row.id == crate::items::github::CLT_ITEM || row.id == "lan.broker")
            .find(|row| {
                !matches!(
                    row.outcome,
                    Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
                )
            })
            .map(|row| {
                items
                    .iter()
                    .find(|item| item.id() == row.id)
                    .and_then(|item| item.unfinished())
                    .unwrap_or(crate::init_clt::Unfinished::Other)
            });
        return match unfinished {
            None => Ok(None),
            Some(why) => Err(ExitCode::from(why.code().unwrap_or(u8::MAX))),
        };
    }
    let report = HostReport {
        schema: crate::SCHEMA,
        mode,
        serial: 0,
        key_id: String::new(),
        run_id: String::new(),
        started,
        finished: sys.now(),
        facts: ctx.facts.clone(),
        items: rows,
        refused: None,
    };
    let host = Host {
        name: ctx.host_name.clone(),
        uuid: ctx.facts.uuid.clone(),
        address: "localhost".into(),
        user: String::new(),
        baseline: None,
        users: Vec::new(),
    };
    Ok(Some((host, report)))
}

fn load(env: &Env<'_>) -> Result<Inventory, ExitCode> {
    set_up_if_new(env.sys, &env.paths);
    inventory::load(env.sys, &env.paths).map_err(|e| {
        errln!("aterm machines: {e}");
        ExitCode::FAILURE
    })
}

/// Initialize the controller directory on demand when `machines.toml` does not exist yet.
pub(crate) fn set_up_if_new(sys: &dyn System, paths: &Paths) {
    if matches!(
        sys.stat(&paths.inventory()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ) {
        let _ = init_quiet(sys, paths);
    }
    refresh_starter_baseline(sys, paths);
}

fn refresh_starter_baseline(sys: &dyn System, paths: &Paths) {
    let path = paths.baseline("default");
    let Ok(digest) = sys.sha256(&path) else {
        return;
    };
    if templates::EARLIER_DEFAULT_BASELINES.contains(&digest.as_str()) {
        let _ = sys.write_file(&path, templates::DEFAULT_BASELINE.as_bytes(), 0o600);
    }
}

fn init_quiet(sys: &dyn System, paths: &Paths) -> Result<(), ()> {
    paths.ensure(sys).map_err(|_| ())?;
    let _ = paths.ensure_control_dir(sys);
    let host = controller_name(sys);
    let keys = [
        (
            paths.ssh_key(),
            format!("aterm-machines-fleet@{host}"),
            false,
        ),
        (paths.signing_key(), format!("signing@{host}"), true),
        (
            paths.root.join("keys").join("recovery"),
            format!("recovery@{host}"),
            true,
        ),
    ];
    for (path, comment, _) in &keys {
        if sys.stat(path).is_ok() || sys.stat(&path.with_extension("pub")).is_ok() {
            continue;
        }
        make_key(sys, path, comment, false).map_err(|_| ())?;
    }
    if sys.stat(&paths.allowed_signers()).is_err() {
        let mut signers = String::new();
        for (path, comment, _) in keys.iter().filter(|(_, _, signer)| *signer) {
            let public = sys.read_file(&path.with_extension("pub")).map_err(|_| ())?;
            let line = envelope::signers_line(comment, String::from_utf8_lossy(&public).trim())
                .map_err(|_| ())?;
            signers.push_str(&line);
            signers.push('\n');
        }
        sys.write_file(&paths.allowed_signers(), signers.as_bytes(), 0o644)
            .map_err(|_| ())?;
    }
    for (path, text) in [
        (paths.inventory(), templates::INVENTORY),
        (paths.baseline("default"), templates::DEFAULT_BASELINE),
    ] {
        if sys.stat(&path).is_err() {
            sys.write_file(&path, text.as_bytes(), 0o600)
                .map_err(|_| ())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::Fake;

    const ROOT: &str = "/ctl/machines";
    const UUID_1: &str = "0A1B2C3D-0000-4000-8000-000000000001";
    const UUID_2: &str = "0A1B2C3D-0000-4000-8000-000000000002";
    const SIGNING_BLOB: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";
    const RECOVERY_BLOB: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao";

    const HOST_KEY_CHANGED: &str = "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\r\nHost key for 0a1b2c3d-0000-4000-8000-000000000001 has changed and you have requested strict checking.\r\nHost key verification failed.\r\n";

    fn paths() -> Paths {
        Paths {
            root: PathBuf::from(ROOT),
        }
    }

    fn env(sys: &dyn System) -> Env<'_> {
        Env {
            sys,
            paths: paths(),
            json: false,
            tty: false,
            parallel: Some(1),
            agent_sock: None,
        }
    }

    fn lab(n: u8, uuid: &str) -> Host {
        Host {
            name: format!("lab-0{n}"),
            uuid: uuid.into(),
            address: format!("192.168.64.{n}"),
            user: "admin".into(),
            baseline: None,
            users: Vec::new(),
        }
    }

    fn controller(hosts: &[&Host], github: bool) -> Fake {
        let sys = Fake::root();
        let github = if github {
            "[github]\naccount = \"fleetbot\"\n"
        } else {
            ""
        };
        names(&sys, hosts, github);
        sys.file(
            &enroll::own_exe().unwrap().to_string_lossy(),
            b"this controller's agent",
        );
        sys.file(
            &format!("{ROOT}/keys/signing.pub"),
            format!("ssh-ed25519 {SIGNING_BLOB} signing@old-name\n").as_bytes(),
        );
        sys.file(
            &format!("{ROOT}/allowed_signers"),
            format!(
                "signing@old-name namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n\
                 recovery@old-name namespaces=\"aterm-machines\" ssh-ed25519 {RECOVERY_BLOB}\n"
            )
            .as_bytes(),
        );
        sys
    }

    /// Writes `machines.toml` naming `hosts`, with `github`'s table text (or none).
    fn names(sys: &Fake, hosts: &[&Host], github: &str) {
        let mut inv = format!("schema = 1\n{github}");
        for h in hosts {
            inv.push_str(&format!(
                "[[host]]\nname = \"{}\"\nuuid = \"{}\"\naddress = \"{}\"\nuser = \"{}\"\n",
                h.name, h.uuid, h.address, h.user
            ));
        }
        sys.file(&format!("{ROOT}/machines.toml"), inv.as_bytes());
    }

    fn request(host: &Host, command: &str) -> String {
        ssh::request_cmd(&paths(), &Target::of(host), command)
            .unwrap()
            .display()
    }

    fn hello_of(host: &Host) -> Hello {
        Hello {
            schema: 1,
            agent_version: "0.106.0".into(),
            agent_sha256: "ab".repeat(32),
            uuid: host.uuid.clone(),
            serial_number: "ZNYVNTRJN5".into(),
            os_version: "27.0".into(),
            os_build: "26A428".into(),
            ..Hello::default()
        }
    }

    fn state_of(sys: &Fake, host: &Host) -> HostState {
        inventory::load_state(sys, &paths(), &host.uuid).unwrap()
    }

    fn row(id: &str, family: &str, outcome: Outcome) -> ItemReport {
        ItemReport {
            id: id.into(),
            family: family.into(),
            title: id.into(),
            outcome,
            from: None,
            to: None,
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            essential: false,
            elapsed_ms: None,
        }
    }

    fn report(mode: Mode, items: Vec<ItemReport>) -> HostReport {
        HostReport {
            schema: 1,
            mode,
            serial: 7,
            key_id: String::new(),
            run_id: "1800000000-7".into(),
            started: 1_799_999_000,
            finished: 1_799_999_900,
            facts: crate::model::HostFacts::default(),
            items,
            refused: None,
        }
    }

    fn refusal(why: &str) -> HostReport {
        let line = aterm_json::to_string(&Line::Refused { why: why.into() }).unwrap();
        read_stream(&format!("{line}\n"), 1_800_000_000).0.unwrap()
    }

    fn inventory_of(sys: &Fake) -> Inventory {
        inventory::load(sys, &paths()).unwrap()
    }

    fn inert(text: &str) -> bool {
        !text.chars().any(char::is_control)
    }

    #[test]
    fn the_starter_baseline_and_inventory_parse() {
        let b = crate::baseline::parse(templates::DEFAULT_BASELINE).unwrap();
        assert!(b.keyboard.is_some());
        assert!(b.homebrew.is_none() && b.apps.is_empty());
        let inv: Inventory = aterm_toml::from_str(templates::INVENTORY).unwrap();
        assert_eq!(inv.schema, 1);
        assert!(inv.hosts.is_empty());
    }

    fn uninitialized(protected: bool) -> Fake {
        let sys = Fake::root();
        sys.on("/usr/sbin/scutil --get LocalHostName", Ran::ok("ctl\n"));
        sys.stat_of(
            &format!("{ROOT}/keys/ctl"),
            crate::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o700,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        for (key, comment, blob, asked) in [
            (
                "ssh_ed25519",
                "aterm-machines-fleet@ctl",
                SIGNING_BLOB,
                false,
            ),
            ("signing", "signing@ctl", SIGNING_BLOB, protected),
            ("recovery", "recovery@ctl", RECOVERY_BLOB, protected),
        ] {
            let path = format!("{ROOT}/keys/{key}");
            let run = if asked {
                format!("tty /usr/bin/ssh-keygen -t ed25519 -C {comment} -f {path}")
            } else {
                format!("/usr/bin/ssh-keygen -q -t ed25519 -N '' -C {comment} -f {path}")
            };
            sys.on(&run, Ran::ok(""));
            sys.on_write(
                &run,
                Path::new(&format!("{path}.pub")),
                format!("ssh-ed25519 {blob} {comment}\n").as_bytes(),
            );
            if asked {
                sys.on(
                    &format!("/usr/bin/ssh-keygen -y -P '' -f {path}"),
                    Ran::exit(
                        255,
                        "Load key: incorrect passphrase supplied to decrypt private key\n",
                    ),
                );
            }
        }
        sys
    }

    #[test]
    fn init_makes_every_key_with_no_passphrase_asking_nothing_and_needing_no_terminal() {
        let sys = uninitialized(false);
        let mut out = Vec::new();
        assert_eq!(init(&env(&sys), false, &mut out), ExitCode::SUCCESS);
        let calls = sys.calls();
        for key in ["ssh_ed25519", "signing", "recovery"] {
            assert!(
                calls.iter().any(
                    |c| c.starts_with("/usr/bin/ssh-keygen -q -t ed25519 -N '' ")
                        && c.ends_with(&format!("-f {ROOT}/keys/{key}"))
                ),
                "{key}: {calls:?}"
            );
        }
        assert!(!calls.iter().any(|c| c.starts_with("tty ")), "{calls:?}");
        let signers =
            String::from_utf8(sys.written(&format!("{ROOT}/allowed_signers")).unwrap()).unwrap();
        assert!(signers.starts_with("signing@ctl ") && signers.contains("\nrecovery@ctl "));
        let out = String::from_utf8(out).unwrap();
        assert!(
            !out.lines().any(|l| {
                let l = l.trim_start();
                l.split_once(". ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            }),
            "{out}"
        );
        assert_eq!(
            out.lines().last(),
            Some(
                format!("The controller is ready in {ROOT}. Next: aterm machines help enroll")
                    .as_str()
            )
        );
        assert!(!out.contains("removable media"), "{out}");
        let before = sys.calls().len();
        assert_eq!(init(&env(&sys), false, &mut Vec::new()), ExitCode::SUCCESS);
        assert!(
            !sys.calls()[before..]
                .iter()
                .any(|c| c.contains("ssh-keygen")),
            "{:?}",
            &sys.calls()[before..]
        );
    }

    #[test]
    fn init_with_passphrase_asks_on_the_terminal_and_refuses_without_one() {
        let sys = uninitialized(true);
        assert_eq!(init(&env(&sys), true, &mut Vec::new()), ExitCode::from(2));
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
        let mut e = env(&sys);
        e.tty = true;
        assert_eq!(init(&e, true, &mut Vec::new()), ExitCode::SUCCESS);
        let calls = sys.calls();
        let asked: Vec<&String> = calls.iter().filter(|c| c.starts_with("tty ")).collect();
        assert_eq!(asked.len(), 2, "the signing and recovery keys: {calls:?}");
        assert!(asked.iter().all(|c| !c.contains("-N")), "{asked:?}");
        assert!(
            calls
                .iter()
                .any(|c| c.contains("-N '' -C aterm-machines-fleet@ctl"))
        );
    }

    #[test]
    fn init_with_passphrase_later_gives_one_to_a_signing_key_made_without() {
        let sys = uninitialized(false);
        assert_eq!(init(&env(&sys), false, &mut Vec::new()), ExitCode::SUCCESS);
        for key in ["ssh_ed25519", "signing"] {
            sys.file(&format!("{ROOT}/keys/{key}"), b"private");
        }
        let probe = format!("/usr/bin/ssh-keygen -y -P '' -f {ROOT}/keys/signing");
        sys.on(&probe, Ran::ok(&format!("ssh-ed25519 {SIGNING_BLOB}\n")));
        sys.on(
            &probe,
            Ran::exit(
                255,
                "Load key: incorrect passphrase supplied to decrypt private key\n",
            ),
        );
        let protect = format!("tty /usr/bin/ssh-keygen -p -f {ROOT}/keys/signing");
        sys.on(&protect, Ran::ok(""));
        let mut e = env(&sys);
        e.tty = true;
        let before = sys.calls().len();
        let mut out = Vec::new();
        assert_eq!(init(&e, true, &mut out), ExitCode::SUCCESS);
        let calls = sys.calls()[before..].to_vec();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("tty "))
                .collect::<Vec<_>>(),
            [&protect],
            "{calls:?}"
        );
        assert!(!calls.iter().any(|c| c.contains("-t ed25519")), "{calls:?}");
        assert!(
            !calls.iter().any(|c| c.contains("keys/ssh_ed25519")),
            "{calls:?}"
        );
        assert!(String::from_utf8(out).unwrap().contains(&format!(
            "gave the signing key a passphrase: {ROOT}/keys/signing"
        )));
        let before = sys.calls().len();
        assert_eq!(init(&e, true, &mut Vec::new()), ExitCode::SUCCESS);
        assert!(
            !sys.calls()[before..].iter().any(|c| c.starts_with("tty ")),
            "{:?}",
            &sys.calls()[before..]
        );
    }

    #[test]
    fn plan_with_no_host_plans_them_all_and_apply_with_none_is_refused() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = controller(&[&a, &b], false);
        for h in [&a, &b] {
            sys.on(
                &request(h, "hello"),
                Ran::exit(
                    255,
                    &format!(
                        "ssh: connect to host {} port 22: Connection refused\r\n",
                        h.address
                    ),
                ),
            );
        }
        let e = env(&sys);
        assert_eq!(
            plan_or_apply(&e, &[], false, Mode::Apply, false),
            ExitCode::from(2)
        );
        assert!(
            sys.calls().is_empty(),
            "apply named no host: {:?}",
            sys.calls()
        );
        assert_eq!(
            plan_or_apply(&e, &[], false, Mode::Plan, false),
            ExitCode::from(2)
        );
        let calls = sys.calls();
        for h in [&a, &b] {
            assert!(
                calls.contains(&request(h, "hello")),
                "{}: {calls:?}",
                h.name
            );
        }
    }

    #[test]
    fn exit_status_says_failed_before_unreachable() {
        assert_eq!(Tally::default().code(), ExitCode::SUCCESS);
        assert_eq!(
            Tally {
                failed: false,
                unreachable: true
            }
            .code(),
            ExitCode::from(2)
        );
        assert_eq!(
            Tally {
                failed: true,
                unreachable: true
            }
            .code(),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn users_default_to_the_inventory_and_an_explicit_list_is_kept() {
        let host = Host {
            name: "lab-07".into(),
            uuid: "U".into(),
            address: "lab-07.local".into(),
            user: "labadmin".into(),
            baseline: None,
            users: Vec::new(),
        };
        let filled = default_users("schema = 1\n", &host).unwrap();
        assert_eq!(crate::baseline::parse(&filled).unwrap().users, ["labadmin"]);
        let kept = default_users("schema = 1\nusers = [\"alice\"]\n", &host).unwrap();
        assert_eq!(crate::baseline::parse(&kept).unwrap().users, ["alice"]);
    }

    #[test]
    fn a_stream_ends_in_its_report_and_a_refusal_is_a_refused_report_stamped_now() {
        let r = refusal("replay principal=p serial=3 floor=9");
        assert!(r.refused.unwrap().starts_with("replay"));
        assert_eq!(r.finished, 1_800_000_000, "never read as decades ago");
        assert_eq!(read_stream("garbage\n", 1).0, None);
        let started = aterm_json::to_string(&Line::Started {
            run: "1800000000-7".into(),
        })
        .unwrap();
        assert_eq!(
            read_stream(&format!("{started}\n"), 1),
            (None, Some("1800000000-7".to_owned()))
        );
    }

    #[test]
    fn ages_read_in_the_largest_unit() {
        assert_eq!(age(5), "5s");
        assert_eq!(age(120), "2m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(3 * 86_400), "3d");
    }

    #[test]
    fn a_host_runs_another_agent_build_unless_it_reports_this_one() {
        let own = OwnAgent {
            exe: PathBuf::from("/x/aterm"),
            sha256: "ab".repeat(32),
        };
        let mut hello: Hello = aterm_json::from_str(
            r#"{"schema":1,"agent_version":"0.1.0","families":[],"uuid":"U","serial_number":"S",
                "computer_name":"c","local_host_name":"l","os_version":"27.0","os_build":"26A428",
                "arch":"arm64","floors":[],"cached":[]}"#,
        )
        .unwrap();
        assert!(own.differs(&hello));
        hello.agent_sha256 = "AB".repeat(32);
        assert!(!own.differs(&hello));
        hello.agent_sha256 = "cd".repeat(32);
        assert!(own.differs(&hello));
    }

    #[test]
    fn the_floor_is_the_highest_any_of_the_keys_names_has_and_zero_for_a_new_one() {
        let hello: Hello = aterm_json::from_str(
            r#"{"schema":1,"agent_version":"0.1.0","families":[],"uuid":"U","serial_number":"S",
                "computer_name":"c","local_host_name":"l","os_version":"27.0","os_build":"26A428",
                "arch":"arm64","floors":[{"principal":"signing@ctl","serial":41},
                {"principal":"signing@renamed","serial":12}],"cached":[]}"#,
        )
        .unwrap();
        let names = |n: &[&str]| n.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(floor_of(&hello, &names(&["signing@ctl"])), 41);
        assert_eq!(
            floor_of(&hello, &names(&["signing@renamed", "signing@ctl"])),
            41
        );
        assert_eq!(floor_of(&hello, &names(&["recovery@ctl"])), 0);
    }

    #[test]
    fn a_changed_host_key_fails_the_command_and_is_kept_with_its_reason_not_as_unreachable() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = controller(&[&a, &b], false);
        sys.on(&request(&a, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
        sys.on(
            &request(&b, "hello"),
            Ran::exit(
                255,
                "ssh: connect to host 192.168.64.2 port 22: Operation timed out\r\n",
            ),
        );
        let e = env(&sys);
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Apply, false),
            ExitCode::FAILURE,
            "a possible impostor is never the harmless exit 2"
        );
        let changed = state_of(&sys, &a);
        assert_eq!(changed.unreachable_since, None);
        let (since, why) = changed.contact_error.clone().unwrap();
        assert_eq!(since, sys.now);
        assert!(why.contains("host key is not the one pinned"), "{why}");
        assert_eq!(state_of(&sys, &b).unreachable_since, Some(sys.now));
        assert_eq!(state_of(&sys, &b).contact_error, None);
        assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
        assert_eq!(outstanding(&e), ExitCode::FAILURE);
        assert!(
            summary(&a, &changed, sys.now + 3600).contains("failing for 1h: its host key"),
            "{}",
            summary(&a, &changed, sys.now + 3600)
        );
    }

    #[test]
    fn hosts_that_only_could_not_be_reached_exit_2_and_another_mac_answering_fails() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = controller(&[&a, &b], false);
        sys.on(
            &request(&a, "hello"),
            Ran::exit(
                255,
                "ssh: connect to host 192.168.64.1 port 22: Connection refused\r\n",
            ),
        );
        sys.on(
            &request(&b, "hello"),
            Ran::exit(
                255,
                "ssh: connect to host 192.168.64.2 port 22: Operation timed out\r\n",
            ),
        );
        sys.on(
            &request(&b, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(&a)).unwrap()),
        );
        let e = env(&sys);
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Plan, false),
            ExitCode::from(2)
        );
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Plan, false),
            ExitCode::FAILURE
        );
        assert!(
            state_of(&sys, &b)
                .contact_error
                .unwrap()
                .1
                .contains("this is not lab-02")
        );
    }

    /// Where no host answered and the LAN broker made no connection, the sweep stops on the
    /// broker's words, once; a host that answered, or failures that are the hosts' own, keep
    /// the per-host rows. NEGATIVE CONTROL: a wall on any broker failure stops a sweep a host
    /// answered.
    #[test]
    fn the_broker_line_stands_for_the_hosts_only_when_none_answered() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let reached = |ha: Result<Hello, HelloError>, hb: Result<Hello, HelloError>| {
            broker_wall(&[
                Reached {
                    host: Cow::Borrowed(&a),
                    hello: ha,
                },
                Reached {
                    host: Cow::Borrowed(&b),
                    hello: hb,
                },
            ])
        };
        let said = format!(
            "{}{}",
            super::super::lan_proxy::BROKER_SAID,
            aterm_uds::lan::Unavailable::NotInstalled
        );
        let broker = || Err(HelloError::Ssh(ssh::Failure::Broker(said.clone())));
        assert_eq!(reached(Ok(hello_of(&a)), broker()), None);
        assert_eq!(
            reached(broker(), broker()),
            Some(format!("controller: {said}"))
        );
        assert_eq!(
            reached(broker(), Err(HelloError::Ssh(ssh::Failure::Refused))),
            Some(format!("controller: {said}"))
        );
        assert_eq!(
            reached(
                Err(HelloError::Ssh(ssh::Failure::NoRoute)),
                Err(HelloError::Ssh(ssh::Failure::TimedOut))
            ),
            None
        );
    }

    #[test]
    fn an_unreadable_inventory_is_a_failure_and_all_over_no_hosts_is_nothing_to_do() {
        let sys = Fake::root();
        sys.file(
            &format!("{ROOT}/machines.toml"),
            b"schema = 1\n[[host]]\nname = 3\n",
        );
        let e = env(&sys);
        assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
        assert_eq!(
            plan_or_apply(&e, &[], true, Mode::Plan, false),
            ExitCode::FAILURE
        );
        assert_eq!(outstanding(&e), ExitCode::FAILURE);
        let missing = Fake::root();
        assert_eq!(status(&env(&missing), &[], true, false), ExitCode::FAILURE);
        let empty = controller(&[], false);
        assert_eq!(
            plan_or_apply(&env(&empty), &[], true, Mode::Apply, false),
            ExitCode::SUCCESS
        );
        assert_eq!(
            plan_or_apply(&env(&empty), &[], false, Mode::Apply, false),
            ExitCode::from(2)
        );
    }

    #[test]
    fn two_macs_their_baseline_would_give_one_local_hostname_are_sent_nothing() {
        let m1 = Host {
            name: "m1".into(),
            ..lab(1, UUID_1)
        };
        let spare = Host {
            name: "m1-spare".into(),
            ..lab(2, UUID_2)
        };
        let sys = controller(&[&m1, &spare], false);
        sys.file(
            &format!("{ROOT}/baselines/default.toml"),
            b"schema = 1\n[names]\ncomputer_name = \"{tag} {hardware}\"\nlocal_host_name = \"{tag}-{hardware}\"\n",
        );
        for h in [&m1, &spare] {
            sys.on(
                &request(h, "hello"),
                Ran::ok(&aterm_json::to_string(&hello_of(h)).unwrap()),
            );
        }
        assert_eq!(
            plan_or_apply(&env(&sys), &[], true, Mode::Plan, false),
            ExitCode::FAILURE
        );
        for h in [&m1, &spare] {
            assert!(!sys.calls().contains(&request(h, "apply")));
        }
        assert!(
            sys.written(&format!("{ROOT}/serial")).is_none(),
            "no serial taken"
        );
    }

    #[test]
    fn a_mac_renamed_at_its_dot_local_address_is_found_there_next_time() {
        let mut m1 = lab(1, UUID_1);
        m1.address = "m1-Air-M2-24GB-1TB-2.local".into();
        let sys = controller(&[&m1], false);
        let mut renamed = row("names.local-host-name", "names", Outcome::Changed);
        renamed.from = Some("\"m1-Air-M2-24GB-1TB-2\"".into());
        renamed.to = Some("\"m1-MacBook-Air-M2-24GB-1TB\"".into());
        let moved = follow_rename(&env(&sys), &m1, &report(Mode::Apply, vec![renamed])).unwrap();
        assert_eq!(moved.address, "m1-MacBook-Air-M2-24GB-1TB.local");
        let inv = inventory_of(&sys);
        assert_eq!(inv.hosts[0].address, "m1-MacBook-Air-M2-24GB-1TB.local");
        assert_eq!(inv.hosts[0].name, "lab-01");
    }

    fn renamed_vm() -> (Fake, Host, Host) {
        let vm = Host {
            name: "lab-03".into(),
            address: "lab-03.local".into(),
            ..lab(3, UUID_1)
        };
        let sys = controller(&[&vm], false);
        sys.file(
            &format!("{ROOT}/baselines/default.toml"),
            b"schema = 1\n[names]\ncomputer_name = \"{tag} {hardware}\"\nlocal_host_name = \"{tag}-{hardware}\"\n",
        );
        let mut said = hello_of(&vm);
        said.hardware = ["Virtual Machine", "M5", "8GB", "90GB"]
            .map(String::from)
            .to_vec();
        let state = HostState {
            hello: Some(aterm_json::to_string(&said).unwrap()),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &vm.uuid, &state).unwrap();
        let moved = Host {
            address: "lab-03-Virtual-Machine-M5-8GB-90GB.local".into(),
            ..vm.clone()
        };
        (sys, vm, moved)
    }

    const UNRESOLVED_OLD: &str = "ssh: Could not resolve hostname lab-03.local: nodename nor servname provided, or not known\r\n";

    #[test]
    fn a_mac_renamed_by_a_run_whose_report_was_lost_is_found_at_the_name_its_baseline_gives() {
        let (sys, vm, moved) = renamed_vm();
        sys.on(&request(&vm, "hello"), Ran::exit(255, UNRESOLVED_OLD));
        let mut answer = hello_of(&vm);
        answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
        sys.on(
            &request(&moved, "hello"),
            Ran::ok(&aterm_json::to_string(&answer).unwrap()),
        );
        let finished = report(Mode::Apply, Vec::new());
        let stream = aterm_json::to_string(&Line::Report(Box::new(finished.clone()))).unwrap();
        sys.on(
            &request(&moved, "attach 1800000000-7"),
            Ran::ok(&format!("{stream}\n")),
        );
        let e = env(&sys);
        assert_eq!(
            attach(&e, "lab-03", Some("1800000000-7")),
            ExitCode::SUCCESS
        );
        assert_eq!(inventory_of(&sys).hosts[0].address, moved.address);
        assert!(!sys.calls().contains(&request(&vm, "attach 1800000000-7")));
        let state = state_of(&sys, &vm);
        assert_eq!((state.last_seen, state.unreachable_since), (sys.now, None));
        assert_eq!(state.report, Some(finished));
        let at_old = |sys: &Fake| {
            sys.calls()
                .iter()
                .filter(|c| **c == request(&vm, "hello"))
                .count()
        };
        let asked = at_old(&sys);
        let inv = inventory_of(&sys);
        let reached = reach_one(&e, &inv.hosts[0]);
        assert!(reached.hello.is_ok());
        assert_eq!(at_old(&sys), asked);
    }

    #[test]
    fn a_mac_still_reached_by_its_old_name_moves_to_the_one_it_says_it_has_once_that_answers() {
        let (sys, vm, moved) = renamed_vm();
        let mut answer = hello_of(&vm);
        answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
        let answer = Ran::ok(&aterm_json::to_string(&answer).unwrap());
        sys.on(&request(&vm, "hello"), answer.clone())
            .on(&request(&moved, "hello"), answer);
        let reached = reach_one(&env(&sys), &vm);
        assert!(reached.hello.is_ok());
        assert_eq!(reached.host.address, moved.address);
        assert_eq!(inventory_of(&sys).hosts[0].address, moved.address);
        assert_eq!(found_lead(&vm), format!("{:<16} renamed: ", vm.name));
        let (sys, vm, moved) = renamed_vm();
        let mut answer = hello_of(&vm);
        answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
        sys.on(
            &request(&vm, "hello"),
            Ran::ok(&aterm_json::to_string(&answer).unwrap()),
        )
        .on(
            &request(&moved, "hello"),
            Ran::exit(
                255,
                &UNRESOLVED_OLD.replace("lab-03.local", "lab-03-Virtual-Machine-M5-8GB-90GB.local"),
            ),
        );
        let reached = reach_one(&env(&sys), &vm);
        assert!(reached.hello.is_ok());
        assert!(matches!(reached.host, Cow::Borrowed(_)));
        assert_eq!(inventory_of(&sys).hosts[0].address, "lab-03.local");
        assert_eq!(state_of(&sys, &vm).contact_error, None);
    }

    #[test]
    fn another_mac_at_the_renamed_name_is_never_followed_and_nothing_else_is_tried() {
        let (sys, vm, moved) = renamed_vm();
        sys.on(&request(&vm, "hello"), Ran::exit(255, UNRESOLVED_OLD));
        let other = lab(9, UUID_2);
        sys.on(
            &request(&moved, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(&other)).unwrap()),
        )
        .on(&request(&moved, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
        let e = env(&sys);
        for _ in 0..2 {
            assert_eq!(
                attach(&e, "lab-03", Some("1800000000-7")),
                ExitCode::from(2)
            );
            assert_eq!(inventory_of(&sys).hosts[0].address, "lab-03.local");
            let state = state_of(&sys, &vm);
            assert_eq!(state.unreachable_since, Some(sys.now));
            assert_eq!(state.contact_error, None);
        }
        assert!(!sys.calls().iter().any(|c| c.contains("attach ")));
        let (sys, vm, moved) = renamed_vm();
        sys.on(
            &request(&vm, "hello"),
            Ran::exit(
                255,
                "ssh: connect to host lab-03.local port 22: Operation timed out\r\n",
            ),
        );
        let reached = reach_one(&env(&sys), &vm);
        assert!(matches!(reached.host, Cow::Borrowed(_)));
        assert!(!sys.calls().contains(&request(&moved, "hello")));
    }

    #[test]
    fn a_mac_whose_old_name_another_mac_took_is_found_at_the_name_its_baseline_gives() {
        let (sys, vm, moved) = renamed_vm();
        sys.on(&request(&vm, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
        let mut answer = hello_of(&vm);
        answer.local_host_name = "lab-03-Virtual-Machine-M5-8GB-90GB".into();
        sys.on(
            &request(&moved, "hello"),
            Ran::ok(&aterm_json::to_string(&answer).unwrap()),
        );
        let reached = reach_one(&env(&sys), &vm);
        assert!(reached.hello.is_ok());
        assert_eq!(reached.host.address, moved.address);
        assert_eq!(inventory_of(&sys).hosts[0].address, moved.address);
        let state = state_of(&sys, &vm);
        assert_eq!((state.contact_error, state.last_seen), (None, sys.now));
        let (sys, vm, moved) = renamed_vm();
        sys.on(&request(&vm, "hello"), Ran::exit(255, HOST_KEY_CHANGED))
            .on(
                &request(&moved, "hello"),
                Ran::exit(
                    255,
                    &UNRESOLVED_OLD
                        .replace("lab-03.local", "lab-03-Virtual-Machine-M5-8GB-90GB.local"),
                ),
            );
        let reached = reach_one(&env(&sys), &vm);
        assert_eq!(
            reached.hello,
            Err(HelloError::Ssh(ssh::Failure::HostKeyChanged))
        );
        assert!(matches!(reached.host, Cow::Borrowed(_)));
        assert_eq!(inventory_of(&sys).hosts[0].address, "lab-03.local");
        assert!(
            state_of(&sys, &vm)
                .contact_error
                .is_some_and(|(_, why)| why.contains("host key"))
        );
    }

    #[test]
    fn an_upgraded_agent_is_greeted_before_the_baseline_that_may_rename_the_mac() {
        let vm = Host {
            name: "lab-03".into(),
            address: "lab-03.local".into(),
            ..lab(3, UUID_1)
        };
        let sys = controller(&[&vm], false);
        let archive = enroll::Archive {
            path: PathBuf::from("/scratch/agent.zip"),
            sha256: "cd".repeat(32),
            exe: PathBuf::from("aterm"),
            bundle: false,
        };
        let mut old = hello_of(&vm);
        old.cached = vec![archive.sha256.clone()];
        let stream = |items: Vec<ItemReport>| {
            let line = Line::Report(Box::new(report(Mode::Apply, items)));
            Ran::ok(&format!("{}\n", aterm_json::to_string(&line).unwrap()))
        };
        sys.on(
            &request(&vm, "apply"),
            stream(vec![row("agent.upgrade", "agent", Outcome::Changed)]),
        )
        .on(
            &request(&vm, "apply"),
            stream(vec![row("names.local-host-name", "names", Outcome::Ok)]),
        );
        let mut new = hello_of(&vm);
        new.agent_sha256 = "ef".repeat(32);
        new.hardware = ["Virtual Machine", "M5", "8GB", "90GB"]
            .map(String::from)
            .to_vec();
        sys.on(
            &request(&vm, "hello"),
            Ran::ok(&aterm_json::to_string(&new).unwrap()),
        );
        let job = Job {
            host: Cow::Borrowed(&vm),
            hello: old,
            needed: Vec::new(),
            upgrade: Some(b"upgrade".to_vec()),
            wire: b"baseline".to_vec(),
        };
        let done = send(&env(&sys), &job, Some(&archive)).unwrap();
        assert_eq!(done.items.len(), 2);
        let calls = sys.calls();
        let at = |cmd: String| -> Vec<usize> {
            calls
                .iter()
                .enumerate()
                .filter(|(_, c)| **c == cmd)
                .map(|(i, _)| i)
                .collect()
        };
        let (applies, hellos) = (at(request(&vm, "apply")), at(request(&vm, "hello")));
        assert_eq!((applies.len(), hellos.len()), (2, 1), "{calls:?}");
        assert!(
            applies[0] < hellos[0] && hellos[0] < applies[1],
            "{calls:?}"
        );
        let kept: Hello =
            aterm_json::from_str(state_of(&sys, &vm).hello.as_deref().unwrap()).unwrap();
        assert_eq!(kept, new);
    }

    #[test]
    fn a_busy_host_is_sent_nothing_and_fails_the_command() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let mut busy = hello_of(&a);
        busy.busy = Some("1799999999-4".into());
        sys.on(
            &request(&a, "hello"),
            Ran::ok(&aterm_json::to_string(&busy).unwrap()),
        );
        assert_eq!(
            plan_or_apply(&env(&sys), &[], true, Mode::Apply, false),
            ExitCode::FAILURE
        );
        assert!(!sys.calls().contains(&request(&a, "apply")));
        assert!(
            sys.written(&format!("{ROOT}/serial")).is_none(),
            "no serial taken"
        );
    }

    #[test]
    fn an_only_envelope_carries_those_tables_and_resolves_nothing_for_the_others() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        sys.file(
            &format!("{ROOT}/baselines/default.toml"),
            templates::DEFAULT_BASELINE.as_bytes(),
        );
        let e = env(&sys);
        let text = resolved_baseline(
            &e,
            &a,
            &hello_of(&a),
            &mut Resolved::default(),
            &["names".to_owned()],
        )
        .unwrap();
        let b = crate::baseline::parse(&text).unwrap();
        assert!(b.names.is_some());
        assert!(b.aterm.is_none() && b.apps.is_empty() && b.homebrew.is_none());
        assert!(b.power.is_none() && b.keyboard.is_none() && b.developer.is_none());
        assert_eq!(
            b.users,
            std::slice::from_ref(&a.user),
            "the users stay for user-scope names"
        );
        assert!(
            sys.calls().is_empty(),
            "no release looked up: {:?}",
            sys.calls()
        );
        assert!(resolved_baseline(&e, &a, &hello_of(&a), &mut Resolved::default(), &[]).is_err());
    }

    #[test]
    fn an_only_apply_names_what_the_cache_holds_so_the_agent_prunes_none_of_it() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        sys.file(
            &format!("{ROOT}/baselines/default.toml"),
            templates::DEFAULT_BASELINE.as_bytes(),
        );
        let e = env(&sys);
        let (app, pin) = ("ab".repeat(32), "CD".repeat(32));
        let mut hello = hello_of(&a);
        hello.cached = vec![app.clone(), pin.clone(), "../x".into()];
        let only = ["names".to_owned()];
        let body = |action: Action| {
            baseline_body(
                &e,
                &a,
                &hello,
                0,
                action,
                false,
                &mut Resolved::default(),
                &only,
            )
            .unwrap()
        };
        let (apply, needed) = body(Action::Apply);
        assert!(needed.is_empty(), "nothing to upload for [names]");
        let named: Vec<&str> = apply.artifacts.iter().map(|x| x.sha256.as_str()).collect();
        assert_eq!(named, [app.as_str(), &pin.to_ascii_lowercase()]);
        let (plan, _) = body(Action::Plan);
        assert!(plan.artifacts.is_empty(), "{:?}", plan.artifacts);
    }

    #[test]
    fn an_only_run_whose_state_cannot_note_it_is_not_sent() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        sys.file(
            &format!("{ROOT}/baselines/default.toml"),
            templates::DEFAULT_BASELINE.as_bytes(),
        );
        sys.file(&format!("{ROOT}/state/{UUID_1}.json"), b"{\"report\":");
        sys.on(
            &request(&a, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(&a)).unwrap()),
        );
        let e = env(&sys);
        assert_eq!(
            plan_or_apply_only(&e, &[], true, Mode::Plan, false, &["names".to_owned()]),
            ExitCode::FAILURE
        );
        let calls = sys.calls();
        assert!(!calls.iter().any(|c| c.contains("ssh-keygen")), "{calls:?}");
        assert_eq!(
            sys.written(&format!("{ROOT}/state/{UUID_1}.json")),
            Some(b"{\"report\":".to_vec()),
            "the state is left as it was"
        );
    }

    fn whole_apply() -> HostReport {
        let mut brew = row("homebrew.pkg", "homebrew", Outcome::Changed);
        brew.effect = Effect::Restart;
        let mut r = report(
            Mode::Apply,
            vec![
                row("names.computer-name", "names", Outcome::Failed),
                row("power.ac.sleep", "power", Outcome::Failed),
                brew,
            ],
        );
        r.facts.os_build = "26A428".into();
        r
    }

    fn names_plan(serial: u64) -> HostReport {
        let mut r = report(
            Mode::Plan,
            vec![
                row("agent.upgrade", "agent", Outcome::Planned),
                row("names.computer-name", "names", Outcome::Planned),
                row("names.local-host-name", "names", Outcome::Planned),
            ],
        );
        r.serial = serial;
        r.finished = 1_800_000_000;
        r.facts.os_build = "26B5".into();
        r
    }

    #[test]
    fn an_only_report_replaces_its_tables_rows_and_leaves_the_others_as_their_run_left_them() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        after_run(&e, &inv, &a, whole_apply(), Source::Baseline);
        remember_only(&e, &a, 8, &["names".to_owned()]).unwrap();
        after_run(&e, &inv, &a, names_plan(8), Source::Baseline);
        let state = state_of(&sys, &a);
        let saved = state.report.as_ref().unwrap();
        let rows: Vec<(&str, Outcome)> = saved
            .items
            .iter()
            .map(|r| (r.id.as_str(), r.outcome))
            .collect();
        assert_eq!(
            rows,
            [
                ("agent.upgrade", Outcome::Planned),
                ("names.computer-name", Outcome::Planned),
                ("names.local-host-name", Outcome::Planned),
                ("power.ac.sleep", Outcome::Failed),
                ("homebrew.pkg", Outcome::Changed),
            ]
        );
        assert_eq!((saved.mode, saved.serial), (Mode::Plan, 8));
        assert_eq!(
            state.only,
            Some(inventory::Only {
                tables: vec!["names".into()],
                rest: Some(1_799_999_900),
            })
        );
        assert!(state.only_sent.is_empty(), "the serial is spent");
        remember_only(&e, &a, 9, &["names".to_owned()]).unwrap();
        let mut names_apply = names_plan(9);
        names_apply.mode = Mode::Apply;
        after_run(&e, &inv, &a, names_apply, Source::Baseline);
        let state = state_of(&sys, &a);
        assert_eq!(state.applied_build.as_deref(), Some("26A428"));
        assert_eq!(outstanding(&e), ExitCode::FAILURE);
        let line = summary(&a, &state, sys.now);
        assert!(line.contains("; names only (the rest "), "{line}");
        assert!(only_todo(&a, &state).is_none(), "the rest was run once");
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Plan, Vec::new()),
            Source::Baseline,
        );
        let state = state_of(&sys, &a);
        assert_eq!(state.only, None);
        assert!(state.report.unwrap().items.is_empty());
    }

    #[test]
    fn a_host_only_only_runs_reached_says_the_rest_was_never_run() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        remember_only(&e, &a, 8, &["names".to_owned()]).unwrap();
        after_run(&e, &inv, &a, names_plan(8), Source::Followed);
        let state = state_of(&sys, &a);
        assert_eq!(
            state.only,
            Some(inventory::Only {
                tables: vec!["names".into()],
                rest: None,
            })
        );
        assert_eq!(
            only_todo(&a, &state).as_deref(),
            Some(
                "lab-01: only its names so far (--only); the rest of its baseline (apps, \
                 settings) has not run yet: `aterm machines apply lab-01` does it when you want"
            )
        );
        let line = summary(&a, &state, sys.now);
        assert!(
            line.contains("; names only (the rest of the baseline not yet)"),
            "{line}"
        );
        remember_only(&e, &a, 9, &["power".to_owned()]).unwrap();
        let mut power = names_plan(9);
        power.items = vec![row("power.ac.sleep", "power", Outcome::Ok)];
        after_run(&e, &inv, &a, power, Source::Baseline);
        let state = state_of(&sys, &a);
        assert_eq!(
            state.only.map(|o| o.tables),
            Some(vec!["names".to_owned(), "power".to_owned()])
        );
        let ids: Vec<String> = state
            .report
            .unwrap()
            .items
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(
            ids,
            [
                "names.computer-name",
                "names.local-host-name",
                "power.ac.sleep"
            ]
        );
    }

    #[test]
    fn a_refused_run_is_kept_beside_the_last_report_never_in_its_place() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let applied = report(
            Mode::Apply,
            vec![row(
                "person.full_disk_access:aterm",
                "person",
                Outcome::Person,
            )],
        );
        after_run(&e, &inv, &a, applied.clone(), Source::Baseline);
        after_run(
            &e,
            &inv,
            &a,
            refusal("busy run=1800000000-9"),
            Source::Baseline,
        );
        let state = state_of(&sys, &a);
        assert_eq!(state.report.as_ref(), Some(&applied));
        assert_eq!(
            state.last_refusal,
            Some((sys.now, "busy run=1800000000-9".to_owned()))
        );
        assert!(summary(&a, &state, sys.now).contains("REFUSED 0s ago: busy run="));
        assert_eq!(status(&e, &[], true, false), ExitCode::FAILURE);
        assert_eq!(outstanding(&e), ExitCode::FAILURE);
        assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Plan, Vec::new()),
            Source::Baseline,
        );
        assert_eq!(state_of(&sys, &a).last_refusal, None);
    }

    #[test]
    fn an_exec_or_upgrade_report_attach_followed_leaves_the_hosts_report_alone() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let applied = report(
            Mode::Apply,
            vec![row("power.ac.sleep", "power", Outcome::Failed)],
        );
        after_run(&e, &inv, &a, applied.clone(), Source::Baseline);
        for job in [
            report(Mode::Apply, vec![row("exec", "exec", Outcome::Changed)]),
            report(
                Mode::Apply,
                vec![row("agent.upgrade", "agent", Outcome::Changed)],
            ),
        ] {
            after_run(&e, &inv, &a, job, Source::Followed);
            assert_eq!(state_of(&sys, &a).report.as_ref(), Some(&applied));
        }
        let later = report(
            Mode::Apply,
            vec![row("power.ac.sleep", "power", Outcome::Changed)],
        );
        after_run(&e, &inv, &a, later.clone(), Source::Followed);
        assert_eq!(state_of(&sys, &a).report, Some(later));
    }

    #[test]
    fn a_report_whose_os_build_moved_since_the_last_apply_reads_os_changed_with_the_agent() {
        let a = lab(1, UUID_1);
        let mut hello = hello_of(&a);
        hello.os_build = "26B5".into();
        let state = HostState {
            hello: aterm_json::to_string(&hello).ok(),
            last_seen: 1_800_000_000,
            report: Some(report(Mode::Apply, Vec::new())),
            applied_build: Some("26A428".into()),
            ..HostState::default()
        };
        let line = summary(&a, &state, 1_800_000_000);
        assert!(line.contains("os-changed (26A428 \u{2192} 26B5)"), "{line}");
        assert!(line.ends_with(", agent 0.106.0]"), "{line}");
        assert!(
            !line.contains("abababababab"),
            "the build's hash is --json's: {line}"
        );
        let unknown = HostState {
            applied_build: None,
            ..state.clone()
        };
        assert_eq!(os_changed(&unknown, saved_hello(&unknown).as_ref()), None);
        let same = HostState {
            applied_build: Some("26B5".into()),
            ..state
        };
        assert_eq!(os_changed(&same, saved_hello(&same).as_ref()), None);
    }

    #[test]
    fn a_report_row_reads_by_its_title_never_its_id() {
        let mut r = row("names.netbios-name", "names", Outcome::Changed);
        r.title = "Windows network name, the Computer Name cut to 15 characters".into();
        r.from = Some("\"VIRTUALMAC-6C5B\"".into());
        r.to = Some("\"M201-VIRTUAL-MA\"".into());
        assert_eq!(
            row_line(&r),
            "changed   Windows network name, the Computer Name cut to 15 characters: \
             \"VIRTUALMAC-6C5B\" → \"M201-VIRTUAL-MA\""
        );
        r.outcome = Outcome::Failed;
        r.why = Some("scutil: Permission denied.".into());
        assert!(
            row_line(&r).ends_with("→ \"M201-VIRTUAL-MA\"; scutil: Permission denied."),
            "{}",
            row_line(&r)
        );
        r.from = None;
        assert!(
            row_line(&r).ends_with("15 characters: scutil: Permission denied."),
            "{}",
            row_line(&r)
        );
    }

    #[test]
    fn what_a_host_sends_reaches_the_terminal_without_control_characters() {
        let osc52 = "\u{1b}]52;c;aGVsbG8=\u{7}";
        let mut r = row("defaults.user.x@alice", "defaults", Outcome::Planned);
        r.from = Some(format!("{{{osc52} = 1}}"));
        r.to = Some("{}".into());
        r.why = Some(format!("why{osc52}"));
        assert!(inert(&row_line(&r)), "{}", row_line(&r));
        let mut ran = report(Mode::Apply, vec![row("exec", "exec", Outcome::Changed)]);
        ran.items[0].to = Some("exit 0".into());
        ran.items[0].detail = Some(format!("file-{osc52}\r\nsecond\n"));
        ran.items[0].why = Some("\u{1b}[2J".to_owned());
        let lines = exec_lines(&ran);
        assert!(lines.iter().all(|l| inert(l)), "{lines:?}");
        assert_eq!(lines.len(), 4, "{lines:?}");
        let a = lab(1, UUID_1);
        let state = HostState {
            report: Some(refusal(&format!("busy {osc52}"))),
            last_refusal: Some((1, format!("busy {osc52}"))),
            contact_error: Some((1, osc52.to_owned())),
            ..HostState::default()
        };
        assert!(inert(&summary(&a, &state, 2)), "{}", summary(&a, &state, 2));
    }

    #[test]
    fn a_person_row_nobody_could_read_is_listed_unmeasured_and_fails_person() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        let e = env(&sys);
        let mut unread = row("person.screen_sharing", "person", Outcome::Unknown);
        unread.why = Some("launchctl print-disabled: timed out".into());
        let state = HostState {
            report: Some(report(Mode::Plan, vec![unread])),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
        assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
        let old = HostState {
            report: Some(refusal("expired issued=1 host-clock=2")),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &old).unwrap();
        assert_eq!(person(&e, &[], true), ExitCode::FAILURE);
        let done = HostState {
            report: Some(report(
                Mode::Plan,
                vec![row("person.screen_sharing", "person", Outcome::Ok)],
            )),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &done).unwrap();
        assert_eq!(person(&e, &[], true), ExitCode::SUCCESS);
    }

    fn person_report(aterm_app: Option<(&str, Outcome)>) -> HostReport {
        let mut items = vec![
            row("person.screen_sharing", "person", Outcome::Person),
            row("person.full_disk_access:aterm", "person", Outcome::Person),
            row("person.developer_tools:aterm", "person", Outcome::Person),
        ];
        if let Some((from, outcome)) = aterm_app {
            let mut app = row("aterm.app", "aterm", outcome);
            app.from = Some(from.into());
            app.to = Some("aterm 0.106.0 (team A66A9P66Z7), owned by admin".into());
            items.insert(0, app);
        }
        report(Mode::Plan, items)
    }

    #[test]
    fn screen_is_refused_while_screen_sharing_is_outstanding_whichever_step_is_named() {
        let a = lab(1, UUID_1);
        let r = person_report(None);
        let err = walk_steps(&r, &a, false, Some("developer_tools:aterm")).unwrap_err();
        assert!(err.contains("Screen Sharing is not on"), "{err}");
        let (steps, waiting) = walk_steps(&r, &a, true, None).unwrap();
        assert_eq!(steps.len(), 3);
        assert!(waiting.is_empty());
    }

    #[test]
    fn before_apply_installs_aterm_its_switches_wait_and_the_rest_are_walked() {
        let a = lab(1, UUID_1);
        let (steps, waiting) = walk_steps(
            &person_report(Some(("absent", Outcome::Planned))),
            &a,
            true,
            None,
        )
        .unwrap();
        assert_eq!(
            steps.iter().map(screen::Step::id).collect::<Vec<_>>(),
            ["screen_sharing"]
        );
        assert_eq!(waiting.len(), 2);
        for w in &waiting {
            assert!(
                matches!(&w.done, screen::Done::NotGranted(why) if why.contains("aterm machines apply lab-01")),
                "{w:?}"
            );
        }
        for installed in [
            ("absent", Outcome::Changed),
            ("aterm 0.105.0", Outcome::Planned),
        ] {
            let (steps, waiting) =
                walk_steps(&person_report(Some(installed)), &a, true, None).unwrap();
            assert_eq!(steps.len(), 3, "{installed:?}");
            assert!(waiting.is_empty());
        }
    }

    #[test]
    fn a_walk_plans_again_when_a_step_reads_granted_even_one_granted_before_it() {
        let a = lab(1, UUID_1);
        let walked = |done: screen::Done| {
            vec![screen::Walked {
                step: "full_disk_access:aterm".into(),
                done,
            }]
        };
        let told = |done| tell(&a, &walked(done), &mut |_| {});
        assert_eq!(told(screen::Done::AlreadyGranted), (false, true));
        assert_eq!(told(screen::Done::Granted), (false, true));
        assert_eq!(told(screen::Done::NotGranted("hang".into())), (true, false));
        assert_eq!(
            told(screen::Done::AtConsole("at the Mac".into())),
            (false, false)
        );
    }

    #[test]
    fn a_mac_left_unlocked_is_the_walks_last_line_and_fails_it_whatever_the_steps_did() {
        let a = lab(1, UUID_1);
        let walked = |done: screen::Done| {
            vec![screen::Walked {
                step: "full_disk_access:aterm".into(),
                done,
            }]
        };
        let ended = |walked: &[screen::Walked], session: &screen::Session| {
            let mut said = Vec::new();
            let code = end_walk(&a, "screen", walked, session, Box::new(()), &mut |s| {
                said.push(s);
            });
            (code, said)
        };
        let unlocked = |why: &str| screen::Session::Unlocked {
            why: why.into(),
            unread: None,
            watched: false,
        };
        let line = "aterm machines screen: lab-01: the Mac is still logged in as admin and \
                    unlocked at its display: the lock after the walk failed \
                    (SACLockScreenImmediate answered 22)";
        assert_eq!(
            ended(
                &walked(screen::Done::Granted),
                &unlocked("SACLockScreenImmediate answered 22")
            ),
            (
                ExitCode::FAILURE,
                vec![
                    Said::Step("lab-01: full_disk_access:aterm: granted".into()),
                    Said::Plan,
                    Said::Last(line.into()),
                ]
            )
        );
        let stopped = walked(screen::Done::NotGranted("stopped by SIGINT".into()));
        let (code, said) = ended(&stopped, &unlocked("SACLockScreenImmediate answered 22"));
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(said.last(), Some(&Said::Last(line.into())));
        assert_eq!(said.len(), 2);
        for session in [screen::Session::Locked, screen::Session::Kept] {
            assert_eq!(
                ended(&walked(screen::Done::Granted), &session),
                (
                    ExitCode::SUCCESS,
                    vec![
                        Said::Step("lab-01: full_disk_access:aterm: granted".into()),
                        Said::Plan,
                    ]
                )
            );
            assert_eq!(ended(&stopped, &session).0, ExitCode::FAILURE);
        }
        let (_, said) = ended(&stopped, &unlocked("\u{1b}]52;c;aGVsbG8=\u{7}"));
        assert!(
            matches!(said.last(), Some(Said::Last(l)) if inert(l)),
            "{said:?}"
        );
    }

    struct Held(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

    impl Drop for Held {
        fn drop(&mut self) {
            self.0.borrow_mut().push("stops let go".into());
        }
    }

    #[test]
    fn the_stops_screen_caught_are_let_go_only_after_the_last_line() {
        let a = lab(1, UUID_1);
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let walked = vec![screen::Walked {
            step: "full_disk_access:aterm".into(),
            done: screen::Done::Granted,
        }];
        let session = screen::Session::Unlocked {
            why: "SACLockScreenImmediate answered 22".into(),
            unread: None,
            watched: true,
        };
        let held = Box::new(Held(std::rc::Rc::clone(&log)));
        let code = end_walk(&a, "screen", &walked, &session, held, &mut |said| {
            log.borrow_mut().push(format!("{said:?}"));
        });
        assert_eq!(code, ExitCode::FAILURE);
        let log = log.borrow();
        assert_eq!(log.len(), 4, "{log:?}");
        assert!(log[1] == "Plan" && log[2].starts_with("Last("), "{log:?}");
        assert_eq!(log[3], "stops let go");
    }

    #[test]
    fn a_key_with_a_passphrase_that_ssh_agent_holds_signs_through_it_without_a_prompt() {
        let sys = Fake::root();
        let key = format!("{ROOT}/keys/signing");
        sys.on(
            &format!("/usr/bin/ssh-keygen -y -P '' -f {key}"),
            Ran::exit(
                255,
                "Load key: incorrect passphrase supplied to decrypt private key\n",
            ),
        );
        let held = format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -T {key}.pub");
        sys.on(&held, Ran::ok(""));
        sys.on(&held, Ran::exit(1, "Agent signature failed\n"));
        let mut e = env(&sys);
        e.agent_sock = Some("/tmp/agent.sock".into());
        assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Agent));
        e.tty = true;
        assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Terminal));
        e.tty = false;
        assert!(
            signing(&e, Path::new(&key))
                .unwrap_err()
                .contains("ssh-agent does not hold it")
        );
        e.agent_sock = None;
        e.tty = true;
        let before = sys.calls().len();
        assert_eq!(signing(&e, Path::new(&key)), Ok(Signing::Terminal));
        assert!(!sys.calls()[before..].iter().any(|c| c.contains("ssh-add")));
    }

    #[test]
    fn the_principal_comes_from_allowed_signers_by_key_before_the_local_host_name() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        sys.on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("renamed-mac\n"),
        );
        let e = env(&sys);
        let inv = inventory_of(&sys);
        assert_eq!(principals(&inv, &e), ["signing@old-name"]);
        sys.file(&format!("{ROOT}/serial"), b"5\n");
        let mut hello = hello_of(&a);
        hello.floors = vec![crate::protocol::Floor {
            principal: "signing@old-name".into(),
            serial: 41,
        }];
        let floor = floor_of(&hello, &principals(&inv, &e));
        let b = body(&e, &a, floor, Action::Plan, Disrupt::WhenIdle).unwrap();
        assert_eq!(b.serial, 42);
        sys.file(
            &format!("{ROOT}/allowed_signers.previous"),
            format!("signing@older namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n")
                .as_bytes(),
        );
        assert_eq!(principals(&inv, &e), ["signing@old-name", "signing@older"]);
        let unlisted = Fake::root();
        unlisted.on(
            "/usr/sbin/scutil --get LocalHostName",
            Ran::ok("renamed-mac\n"),
        );
        assert_eq!(
            principals(&Inventory::default(), &env(&unlisted)),
            ["signing@renamed-mac"]
        );
    }

    #[test]
    fn an_upgrade_is_an_app_zip_only_when_the_bundle_verifies_as_aterms_team_here() {
        let exe = Path::new("/Applications/aterm (dev).app/Contents/MacOS/aterm");
        let archive = |bundle: bool| enroll::Archive {
            path: "/s/aterm-agent.zip".into(),
            sha256: "ab".repeat(32),
            exe: "aterm (dev).app/Contents/MacOS/aterm".into(),
            bundle,
        };
        let check = format!(
            "/usr/bin/codesign --verify --deep --strict -R {} '/Applications/aterm (dev).app'",
            crate::port::shell_word(APP_ZIP_REQUIREMENT)
        );
        let sys = Fake::root();
        sys.on(&check, Ran::ok(""));
        assert_eq!(upgrade_kind(&sys, exe, &archive(true)), "app-zip");
        let sys = Fake::root();
        sys.on(
            &check,
            Ran::exit(
                3,
                "test-requirement: code failed to satisfy specified code requirement(s)\n",
            ),
        );
        assert_eq!(upgrade_kind(&sys, exe, &archive(true)), "dev-zip");
        let sys = Fake::root();
        assert_eq!(
            upgrade_kind(&sys, Path::new("/w/aterm-machines-dev"), &archive(false)),
            "dev-zip"
        );
        assert!(sys.calls().is_empty(), "a bare binary is not checked");
    }

    #[test]
    fn a_new_list_that_only_mentions_the_signing_key_in_a_comment_is_refused() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], false);
        sys.file(
            "/tmp/new_signers",
            format!(
                "# signing@old-name namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n\
                 recovery@old-name namespaces=\"aterm-machines\" ssh-ed25519 {RECOVERY_BLOB}\n"
            )
            .as_bytes(),
        );
        let code = rekey(&env(&sys), &[], true, Path::new("/tmp/new_signers"), None);
        assert_eq!(code, ExitCode::from(2));
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("/usr/bin/ssh ")),
            "no host asked"
        );
    }

    #[test]
    fn the_recovery_keys_public_half_is_found_beside_it_or_kept_on_the_controller() {
        let sys = Fake::root();
        let e = env(&sys);
        sys.file(
            &format!("{ROOT}/keys/recovery.pub"),
            b"ssh-ed25519 AAAA recovery@ctl\n",
        );
        assert_eq!(
            public_of(&e, Path::new("/Volumes/KEY/recovery")).unwrap(),
            PathBuf::from(format!("{ROOT}/keys/recovery.pub"))
        );
        sys.file(
            "/Volumes/KEY/recovery.pub",
            b"ssh-ed25519 AAAA recovery@ctl\n",
        );
        assert_eq!(
            public_of(&e, Path::new("/Volumes/KEY/recovery")).unwrap(),
            PathBuf::from("/Volumes/KEY/recovery.pub")
        );
        let err = public_of(&e, Path::new("/Volumes/KEY/other")).unwrap_err();
        assert!(
            err.contains("/Volumes/KEY/other.pub") && err.contains("keys/other.pub"),
            "{err}"
        );
    }

    #[test]
    fn the_controllers_list_is_replaced_keeping_the_old_one_and_hosts_behind_are_known() {
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = controller(&[&a, &b], false);
        let e = env(&sys);
        let old = sys.written(&format!("{ROOT}/allowed_signers")).unwrap();
        let new = format!("signing@new namespaces=\"aterm-machines\" ssh-ed25519 {SIGNING_BLOB}\n");
        record_signers(&e, &a, &text_sha256(&new));
        replace_signers(&e, &new).unwrap();
        assert_eq!(
            sys.written(&format!("{ROOT}/allowed_signers.previous"))
                .unwrap(),
            old
        );
        assert_eq!(
            sys.written(&format!("{ROOT}/allowed_signers")).unwrap(),
            new.as_bytes()
        );
        assert_eq!(state_of(&sys, &a).signers_sha256, Some(text_sha256(&new)));
        assert_eq!(state_of(&sys, &b).signers_sha256, None);
        replace_signers(&e, &new).unwrap();
        assert_eq!(
            sys.written(&format!("{ROOT}/allowed_signers.previous"))
                .unwrap(),
            old
        );
        assert_eq!(
            principals(&inventory_of(&sys), &e),
            ["signing@new", "signing@old-name"]
        );
    }

    const ALICE_OLD: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao";
    const ALICE_NEW: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";
    const MACHINE_TOKEN: &str = "fixture_machine_user_token_only";
    const KEYS_PAGE: &str = "/user/keys?per_page=100&page=1";

    fn rest(status: u16, body: &str) -> Result<crate::port::GithubResponse, String> {
        Ok(crate::port::GithubResponse {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        })
    }

    fn with_token(sys: &Fake) {
        super::super::github_keys_tests::holds_token(sys, &paths(), MACHINE_TOKEN);
        sys.on_github_rest(
            crate::port::RestMethod::Get,
            "/user",
            rest(200, r#"{"login":"fleetbot"}"#),
        );
    }

    fn lists(sys: &Fake, held: &[(u64, &str)]) {
        let keys: Vec<_> = held
            .iter()
            .map(|(id, body)| aterm_json::json!({"id": id, "key": format!("ssh-ed25519 {body}")}))
            .collect();
        sys.on_github_rest(
            crate::port::RestMethod::Get,
            KEYS_PAGE,
            rest(200, &aterm_json::to_string(&keys).unwrap()),
        );
    }

    fn posts(sys: &Fake, id: u64) {
        sys.on_github_rest(
            crate::port::RestMethod::Post,
            "/user/keys",
            rest(201, &format!(r#"{{"id":{id}}}"#)),
        );
    }

    fn deletes(sys: &Fake, id: u64) {
        sys.on_github_rest(
            crate::port::RestMethod::Delete,
            &format!("/user/keys/{id}"),
            rest(204, ""),
        );
    }

    fn post_key(body: &str) -> String {
        format!(
            r#"github POST /user/keys {{"title":"aterm-machines:lab-01:alice","key":"ssh-ed25519 {body}"}}"#
        )
    }

    fn key_row(body: &str) -> ItemReport {
        let mut r = row("github.key@alice", "github", Outcome::Ok);
        r.detail = Some(format!("ssh-ed25519 {body} aterm-machines:lab-01:alice"));
        r
    }

    #[test]
    fn a_users_new_key_is_registered_and_the_one_it_replaced_deleted() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], true);
        with_token(&sys);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let state = HostState {
            github_keys: vec![("alice".into(), 5)],
            github_key_text: vec![(5, format!("ssh-ed25519 {ALICE_OLD}"))],
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_OLD)]),
            Source::Baseline,
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with("github ") || c.contains("credential fill")),
            "{:?}",
            sys.calls()
        );
        lists(&sys, &[(5, ALICE_OLD)]);
        posts(&sys, 9);
        lists(&sys, &[(5, ALICE_OLD), (9, ALICE_NEW)]);
        lists(&sys, &[(9, ALICE_NEW)]);
        deletes(&sys, 5);
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_NEW)]),
            Source::Baseline,
        );
        let state = state_of(&sys, &a);
        assert_eq!(state.github_keys, [("alice".to_owned(), 9)]);
        assert_eq!(
            state.github_key_text,
            [(9, format!("ssh-ed25519 {ALICE_NEW}"))]
        );
        let calls = sys.calls();
        assert!(calls.contains(&post_key(ALICE_NEW)), "{calls:?}");
        assert!(calls.contains(&"github DELETE /user/keys/5".to_owned()));
        assert!(
            calls
                .iter()
                .all(|c| !c.contains(MACHINE_TOKEN) && !c.contains("/gh "))
        );
    }

    #[test]
    fn a_key_github_created_is_recorded_when_another_key_fails() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], true);
        with_token(&sys);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        lists(&sys, &[]);
        posts(&sys, 9);
        sys.on_github_rest(
            crate::port::RestMethod::Post,
            "/user/keys",
            rest(
                422,
                r#"{"message":"Validation Failed","errors":[{"message":"key is already in use"}]}"#,
            ),
        );
        let mut bob = row("github.key@bob", "github", Outcome::Ok);
        bob.detail = Some(format!("ssh-ed25519 {ALICE_NEW} aterm-machines:lab-01:bob"));
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_OLD), bob]),
            Source::Baseline,
        );
        let state = state_of(&sys, &a);
        assert_eq!(state.github_keys, [("alice".to_owned(), 9)]);
        assert_eq!(
            state.github_key_text,
            [(9, format!("ssh-ed25519 {ALICE_OLD}"))]
        );
        let posted = sys
            .calls()
            .iter()
            .filter(|c| c.starts_with("github POST /user/keys"))
            .count();
        assert_eq!(posted, 2);
    }

    #[test]
    fn a_revoked_key_is_never_registered_again_and_a_plan_registers_nothing() {
        let a = lab(1, UUID_1);
        let sys = controller(&[&a], true);
        with_token(&sys);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        let state = HostState {
            github_keys: vec![("alice".into(), 5)],
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
        lists(&sys, &[(5, ALICE_OLD)]);
        lists(&sys, &[]);
        deletes(&sys, 5);
        assert_eq!(github_revoke(&e, "lab-01"), ExitCode::SUCCESS);
        let state = state_of(&sys, &a);
        assert!(state.github_keys.is_empty());
        assert_eq!(state.github_revoked, [format!("ssh-ed25519 {ALICE_OLD}")]);
        let calls = sys.calls().len();
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_OLD)]),
            Source::Baseline,
        );
        assert!(!sys.calls()[calls..].iter().any(|c| c.contains("POST")));
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Plan, vec![key_row(ALICE_NEW)]),
            Source::Baseline,
        );
        assert!(!sys.calls()[calls..].iter().any(|c| c.contains("POST")));
        lists(&sys, &[]);
        posts(&sys, 11);
        after_run(
            &e,
            &inv,
            &a,
            report(Mode::Apply, vec![key_row(ALICE_NEW)]),
            Source::Baseline,
        );
        assert_eq!(state_of(&sys, &a).github_keys, [("alice".to_owned(), 11)]);
    }

    /// A controller of `hosts` whose `[github]` row sends the key calls over agh, inline, to
    /// `fake` on loopback (plan §17).
    fn over_agh(hosts: &[&Host], fake: &agh_fake::FakeGithub) -> Fake {
        let (sys, _token) =
            super::super::github_keys_tests::inline_on(controller(hosts, false), &paths(), fake);
        names(
            &sys,
            hosts,
            &format!(
                "[github]\naccount = \"{}\"\nkeys = \"agh-inline\"\n",
                super::super::github_keys_tests::LOGIN
            ),
        );
        sys
    }

    /// `METHOD path` of each request `fake` saw.
    fn agh_saw(fake: &agh_fake::FakeGithub) -> Vec<String> {
        super::super::github_keys_tests::requests(&fake.seen())
    }

    #[test]
    fn a_runs_hosts_share_one_agh_client_so_their_key_writes_are_paced() {
        let fake = agh_fake::FakeGithub::start();
        let (a, b) = (lab(1, UUID_1), lab(2, UUID_2));
        let sys = over_agh(&[&a, &b], &fake);
        let e = env(&sys);
        let inv = inventory_of(&sys);
        // `run`'s loop: every host's report through the one `KeyCalls` it opens.
        let key_calls = super::super::github::KeyCalls::new(&sys, &paths());
        for (host, body) in [(&a, ALICE_OLD), (&b, ALICE_NEW)] {
            after_run_with(
                &e,
                &inv,
                host,
                report(Mode::Apply, vec![key_row(body)]),
                Source::Baseline,
                &key_calls,
            );
        }
        // The second host's POST waits out agh's second after the first's, and the inline
        // token's login is bound once.
        assert_eq!(System::github_fx(&sys).slept(), [Duration::from_secs(1)]);
        let saw = agh_saw(&fake);
        assert_eq!(
            saw.iter().filter(|r| *r == "GET /user").count(),
            1,
            "{saw:?}"
        );
        assert_eq!(
            saw.iter().filter(|r| *r == "POST /user/keys").count(),
            2,
            "{saw:?}"
        );
        let held = fake.keys(super::super::github_keys_tests::LOGIN);
        for (host, body) in [(&a, ALICE_OLD), (&b, ALICE_NEW)] {
            let state = state_of(&sys, host);
            let [(user, id)] = state.github_keys.as_slice() else {
                panic!("{}: {:?}", host.name, state.github_keys);
            };
            assert_eq!(user, "alice");
            assert!(
                held.iter()
                    .any(|k| k.id == *id && k.key == format!("ssh-ed25519 {body}")),
                "{held:?}"
            );
        }
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("github ")),
            "no REST call: {:?}",
            sys.calls()
        );
    }

    #[test]
    fn github_revoke_deletes_over_agh_when_the_row_says() {
        let fake = agh_fake::FakeGithub::start();
        let a = lab(1, UUID_1);
        let sys = over_agh(&[&a], &fake);
        let id = fake.add_key(
            super::super::github_keys_tests::LOGIN,
            "aterm-machines:lab-01:alice",
            &format!("ssh-ed25519 {ALICE_OLD}"),
        );
        let state = HostState {
            github_keys: vec![("alice".into(), id)],
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &a.uuid, &state).unwrap();
        assert_eq!(github_revoke(&env(&sys), "lab-01"), ExitCode::SUCCESS);
        let saw = agh_saw(&fake);
        assert!(saw.contains(&format!("DELETE /user/keys/{id}")), "{saw:?}");
        assert!(fake.keys(super::super::github_keys_tests::LOGIN).is_empty());
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("github ")),
            "no REST call: {:?}",
            sys.calls()
        );
        let state = state_of(&sys, &a);
        assert!(state.github_keys.is_empty());
        assert_eq!(state.github_revoked, [format!("ssh-ed25519 {ALICE_OLD}")]);
    }

    #[test]
    fn github_login_asks_at_the_terminal_and_stores_what_github_vouches_for() {
        let a = lab(1, UUID_1);
        for (tty, github, code) in [
            (true, true, ExitCode::SUCCESS),
            (false, true, ExitCode::from(2)),
            (true, false, ExitCode::from(2)),
        ] {
            let sys = controller(&[&a], github);
            sys.answer(Some(MACHINE_TOKEN));
            sys.on_github_rest(
                crate::port::RestMethod::Get,
                "/user",
                Ok(crate::port::GithubResponse {
                    status: 200,
                    headers: vec![("X-OAuth-Scopes".into(), "admin:public_key".into())],
                    body: br#"{"login":"fleetbot"}"#.to_vec(),
                }),
            );
            let stores = format!("write {} 600", paths().github_token().display());
            let mut e = env(&sys);
            e.tty = tty;
            assert_eq!(github_login(&e), code, "tty {tty}, github {github}");
            let calls = sys.calls();
            assert_eq!(
                calls.contains(&stores),
                code == ExitCode::SUCCESS,
                "{calls:?}"
            );
            let asked: Vec<&String> = calls.iter().filter(|c| c.starts_with("ask")).collect();
            if code == ExitCode::SUCCESS {
                assert_eq!(asked.len(), 1, "{calls:?}");
                assert!(asked[0].starts_with("ask secret "), "{calls:?}");
            } else {
                assert!(asked.is_empty(), "{calls:?}");
            }
            assert!(
                calls.iter().all(|c| !c.contains(MACHINE_TOKEN)),
                "{calls:?}"
            );
        }
    }

    #[test]
    fn github_login_under_agh_fleet_stores_nothing_the_key_calls_never_read() {
        let a = lab(1, UUID_1);
        for (keys, code) in [
            ("agh-fleet", ExitCode::from(2)),
            ("agh-inline", ExitCode::SUCCESS),
        ] {
            let sys = controller(&[&a], false);
            names(
                &sys,
                &[&a],
                &format!("[github]\naccount = \"fleetbot\"\nkeys = \"{keys}\"\n"),
            );
            sys.answer(Some(MACHINE_TOKEN));
            sys.on_github_rest(
                crate::port::RestMethod::Get,
                "/user",
                Ok(crate::port::GithubResponse {
                    status: 200,
                    headers: vec![("X-OAuth-Scopes".into(), "admin:public_key".into())],
                    body: br#"{"login":"fleetbot"}"#.to_vec(),
                }),
            );
            let stores = format!("write {} 600", paths().github_token().display());
            let mut e = env(&sys);
            e.tty = true;
            assert_eq!(github_login(&e), code, "{keys}");
            let calls = sys.calls();
            let stored = code == ExitCode::SUCCESS;
            assert_eq!(calls.contains(&stores), stored, "{keys}: {calls:?}");
            assert_eq!(
                calls.iter().any(|c| c.starts_with("ask")),
                stored,
                "{keys}: {calls:?}"
            );
        }
    }

    #[test]
    fn init_keeps_the_key_and_refuses_an_unconfirmed_passphrase() {
        for (label, after, protected) in [
            (
                "protected",
                Ran::exit(
                    255,
                    "Load key: incorrect passphrase supplied to decrypt private key\n",
                ),
                true,
            ),
            ("empty", Ran::ok("ssh-ed25519 public\n"), false),
            ("unreadable", Ran::exit(1, "permission denied\n"), false),
            (
                "interrupted",
                Ran {
                    status: None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
                false,
            ),
        ] {
            let sys = uninitialized(false);
            assert_eq!(init(&env(&sys), false, &mut Vec::new()), ExitCode::SUCCESS);
            let key = format!("{ROOT}/keys/signing");
            let private = b"the same private-key identity";
            sys.file(&key, private);
            let query = format!("/usr/bin/ssh-keygen -y -P '' -f {key}");
            sys.on(&query, Ran::ok("ssh-ed25519 public\n"));
            sys.on(&query, after);
            let protect = format!("tty /usr/bin/ssh-keygen -p -f {key}");
            sys.on(&protect, Ran::ok(""));
            let mut e = env(&sys);
            e.tty = true;
            let before = sys.calls().len();
            let mut out = Vec::new();
            assert_eq!(
                init(&e, true, &mut out),
                if protected {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                },
                "INIT_PASSPHRASE_NEEDS_CONFIRMED_PROTECTION: {label}"
            );
            assert_eq!(sys.read_file(Path::new(&key)).unwrap(), private, "{label}");
            let calls = sys.calls()[before..].to_vec();
            assert!(calls.contains(&protect), "the actual change ran: {calls:?}");
            assert!(!calls.iter().any(|c| c.contains("-t ed25519")), "{calls:?}");
            let out = String::from_utf8(out).unwrap();
            assert_eq!(
                out.contains("gave the signing key a passphrase"),
                protected,
                "{label}: {out}"
            );
        }
    }

    #[test]
    fn a_new_controller_is_set_up_by_the_first_verb_that_needs_it_and_an_old_one_left() {
        let sys = uninitialized(false);
        set_up_if_new(&sys, &paths());
        assert!(sys.written(&format!("{ROOT}/machines.toml")).is_some());
        assert!(sys.written(&format!("{ROOT}/allowed_signers")).is_some());
        assert!(
            sys.calls()
                .iter()
                .any(|c| c.contains("ssh-keygen -q -t ed25519 -N ''")),
            "{:?}",
            sys.calls()
        );
        let sys = uninitialized(false);
        sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
        set_up_if_new(&sys, &paths());
        assert!(sys.written(&format!("{ROOT}/allowed_signers")).is_none());
        assert!(!sys.calls().iter().any(|c| c.contains("ssh-keygen")));
    }

    #[test]
    fn a_starter_baseline_nobody_edited_follows_the_starter_shipped_now() {
        const HOMEBREW_AND_APPS: &str = "[homebrew]\n\
             pkg = { url = \"https://github.com/Homebrew/brew/releases/download/7.0.9/Homebrew.pkg\", \
             sha256 = \"e021295947a4a9b7ca1a8eff310c9ea1db14d923c5acf7d79e325b60a81f1a15\", \
             team_id = \"927JGANW46\" }\n\
             formulae = [\"git\", \"ripgrep\", \"fd\", \"jq\", \"tmux\", \"fzf\", \"wget\", \
             \"htop\", \"coreutils\", \"git-lfs\"]\ncasks = []\n\n\
             [[app]]\nname = \"Emacs\"\nkind = \"dmg-app\"\n\
             url = \"https://emacsformacosx.com/emacs-builds/Emacs-31.1-universal.dmg\"\n\
             sha256 = \"f0383bcbf0104947d3a46266aae43e3dc9e38fd68eeb35f11ee6b9444277b8ec\"\n\
             team_id = \"5BRAQAFB8B\"\nbundle = \"Emacs.app\"\nversion = \"31.1\"\n\n\
             [[app]]\nname = \"Caffeine\"\nkind = \"dmg-app\"\n\
             url = \"https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg\"\n\
             sha256 = \"1ad34c3299a0c866873c4feb432c3e11186375adb6f43c5f481ae9d46d06d723\"\n\
             team_id = \"YD6LEYT6WZ\"\nbundle = \"Caffeine.app\"\nversion = \"1.1.4\"\n\n";
        const GH_ROW: &str = "# gh's installer package carries no installer signature; its binary is checked against\n\
             # GitHub's Team ID instead (design §8).\n[[app]]\nname = \"gh\"\nkind = \"pkg\"\n\
             url = \"https://github.com/cli/cli/releases/download/v2.102.0/gh_2.102.0_macOS_universal.pkg\"\n\
             sha256 = \"1787e65f36626245a6b3cee353a458f6ea4372008548ceb8fb5d5dfb55b006d0\"\n\
             team_id = \"VEKTX9H2N7\"\nunsigned = true\npkg_id = \"com.github.cli\"\n\
             binary = \"/usr/local/bin/gh\"\nversion = \"2.102.0\"\n\n";
        let pointer = "# Other software is yours to add: a [homebrew] table installs Homebrew and its \
             formulae,\n# an [[app]] row one signed app (docs/DESIGN-machines-2026-10-16.md §8). For \
             keeping a Mac\n# awake and editing a file, aterm has its own `aterm acaffeine` and \
             `aterm edit`.\n\n";
        assert!(templates::DEFAULT_BASELINE.contains(pointer));
        let before_rename =
            templates::DEFAULT_BASELINE.replace("`aterm acaffeine`", "`aterm acaffiene`");
        assert!(
            templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&before_rename).as_str())
        );
        let before_vault_daemon = before_rename.replace("vault_server = true\n", "");
        assert!(
            templates::EARLIER_DEFAULT_BASELINES
                .contains(&text_sha256(&before_vault_daemon).as_str())
        );
        let previous = before_vault_daemon.replace(
            &pointer.replace("acaffeine", "acaffiene"),
            HOMEBREW_AND_APPS,
        );
        assert!(
            templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&previous).as_str()),
            "the digest is of the starter as a6c1342e82 wrote it"
        );
        let anchor = "# A per-host GitHub key for each user";
        let earlier = previous.replace(anchor, &format!("{GH_ROW}{anchor}"));
        assert!(
            templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&earlier).as_str()),
            "the digest is of the starter as 326cdca677 wrote it"
        );
        let older = earlier.replace(
            "# host_name \"\" unsets a HostName set by hand, so `hostname`, shell prompts and \
             ssh show the\n# new Local hostname rather than the old name; a dotted name sets one \
             instead.\n",
            "",
        );
        let older = older.replace("host_name = \"\"\n", "");
        assert!(
            templates::EARLIER_DEFAULT_BASELINES.contains(&text_sha256(&older).as_str()),
            "the digest is of the starter as 6045a51374 wrote it"
        );
        assert!(
            !templates::EARLIER_DEFAULT_BASELINES
                .contains(&text_sha256(templates::DEFAULT_BASELINE).as_str()),
            "the starter shipped now is not an earlier one"
        );
        let default = format!("{ROOT}/baselines/default.toml");
        for (on_disk, after) in [
            (
                before_rename.clone(),
                templates::DEFAULT_BASELINE.to_owned(),
            ),
            (
                before_vault_daemon.clone(),
                templates::DEFAULT_BASELINE.to_owned(),
            ),
            (
                format!("{before_rename}# mine\n"),
                format!("{before_rename}# mine\n"),
            ),
            (
                format!("{before_vault_daemon}# mine\n"),
                format!("{before_vault_daemon}# mine\n"),
            ),
            (previous.clone(), templates::DEFAULT_BASELINE.to_owned()),
            (earlier.clone(), templates::DEFAULT_BASELINE.to_owned()),
            (format!("{previous}# mine\n"), format!("{previous}# mine\n")),
            (format!("{earlier}# mine\n"), format!("{earlier}# mine\n")),
            (
                templates::DEFAULT_BASELINE.to_owned(),
                templates::DEFAULT_BASELINE.to_owned(),
            ),
        ] {
            let sys = uninitialized(false);
            sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
            sys.file(&default, on_disk.as_bytes());
            set_up_if_new(&sys, &paths());
            assert_eq!(
                String::from_utf8(sys.written(&default).unwrap()).unwrap(),
                after
            );
        }
        let sys = uninitialized(false);
        sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
        set_up_if_new(&sys, &paths());
        assert!(sys.written(&default).is_none());
    }

    fn timed_out(host: &Host) -> Ran {
        Ran::exit(
            255,
            &format!(
                "ssh: connect to host {} port 22: Operation timed out\r\n",
                host.address
            ),
        )
    }

    fn dhcp_vm() -> (Fake, Host, Host) {
        let vm = lab(9, UUID_1);
        let sys = controller(&[&vm], false);
        let mut said = hello_of(&vm);
        said.local_host_name = "lab-09".into();
        let state = HostState {
            hello: Some(aterm_json::to_string(&said).unwrap()),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &vm.uuid, &state).unwrap();
        let named = Host {
            address: "lab-09.local".into(),
            ..vm.clone()
        };
        (sys, vm, named)
    }

    #[test]
    fn a_mac_whose_dhcp_address_moved_is_followed_to_its_local_hostname_once_its_key_answers() {
        let (sys, vm, named) = dhcp_vm();
        sys.on(&request(&vm, "hello"), timed_out(&vm)).on(
            &request(&named, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(&vm)).unwrap()),
        );
        let reached = reach_one(&env(&sys), &vm);
        assert!(reached.hello.is_ok());
        assert_eq!(reached.host.address, named.address);
        assert_eq!(inventory_of(&sys).hosts[0].address, named.address);
        assert_eq!(found_lead(&vm), format!("{:<16} moved: ", vm.name));
        let asked = request(&named, "hello");
        assert!(sys.calls().contains(&asked));
        assert!(asked.starts_with("/usr/bin/ssh "), "{asked}");
        assert!(asked.contains(&format!("HostKeyAlias={UUID_1}")), "{asked}");
        assert!(asked.contains("StrictHostKeyChecking=yes"), "{asked}");
        let state = state_of(&sys, &vm);
        assert_eq!((state.last_seen, state.unreachable_since), (sys.now, None));
        let (sys, vm, named) = dhcp_vm();
        sys.on(&request(&vm, "hello"), Ran::exit(255, HOST_KEY_CHANGED))
            .on(
                &request(&named, "hello"),
                Ran::ok(&aterm_json::to_string(&hello_of(&vm)).unwrap()),
            );
        assert!(reach_one(&env(&sys), &vm).hello.is_ok());
        assert_eq!(inventory_of(&sys).hosts[0].address, named.address);
    }

    #[test]
    fn another_mac_at_its_local_hostname_moves_nothing() {
        let (sys, vm, named) = dhcp_vm();
        sys.on(&request(&vm, "hello"), timed_out(&vm))
            .on(&request(&named, "hello"), Ran::exit(255, HOST_KEY_CHANGED));
        let reached = reach_one(&env(&sys), &vm);
        assert!(matches!(
            reached.hello,
            Err(HelloError::Ssh(ssh::Failure::TimedOut))
        ));
        assert_eq!(reached.host.address, vm.address);
        assert_eq!(inventory_of(&sys).hosts[0].address, vm.address);
        let state = state_of(&sys, &vm);
        assert_eq!(state.unreachable_since, Some(sys.now));
        assert_eq!(state.contact_error, None);
        let (sys, vm, named) = dhcp_vm();
        let mut other = hello_of(&vm);
        other.uuid = UUID_2.into();
        sys.on(&request(&vm, "hello"), timed_out(&vm)).on(
            &request(&named, "hello"),
            Ran::ok(&aterm_json::to_string(&other).unwrap()),
        );
        assert!(reach_one(&env(&sys), &vm).hello.is_err());
        assert_eq!(inventory_of(&sys).hosts[0].address, vm.address);
    }

    #[test]
    fn only_an_ip_address_that_does_not_answer_is_looked_for_by_name() {
        let (sys, vm, named) = dhcp_vm();
        sys.on(
            &request(&vm, "hello"),
            Ran::ok(&aterm_json::to_string(&hello_of(&vm)).unwrap()),
        );
        assert!(reach_one(&env(&sys), &vm).hello.is_ok());
        assert!(!sys.calls().contains(&request(&named, "hello")));
        let (sys, vm, named) = dhcp_vm();
        sys.on(
            &request(&vm, "hello"),
            Ran::exit(
                255,
                "admin@192.168.64.9: Permission denied (publickey).\r\n",
            ),
        );
        assert!(reach_one(&env(&sys), &vm).hello.is_err());
        assert!(!sys.calls().contains(&request(&named, "hello")));
        let vm = lab(9, UUID_1);
        let sys = controller(&[&vm], false);
        sys.on(&request(&vm, "hello"), timed_out(&vm));
        assert!(reach_one(&env(&sys), &vm).hello.is_err());
        let ssh_calls = |sys: &Fake| {
            sys.calls()
                .iter()
                .filter(|c| c.starts_with("/usr/bin/ssh "))
                .count()
        };
        assert_eq!(ssh_calls(&sys), 1, "{:?}", sys.calls());
        let state = HostState {
            hello: Some(aterm_json::to_string(&hello_of(&vm)).unwrap()),
            ..HostState::default()
        };
        inventory::save_state(&sys, &paths(), &vm.uuid, &state).unwrap();
        assert!(reach_one(&env(&sys), &vm).hello.is_err());
        assert_eq!(ssh_calls(&sys), 2, "{:?}", sys.calls());
        assert_eq!(inventory_of(&sys).hosts[0].address, vm.address);
    }

    #[test]
    fn the_verbs_that_read_state_send_nothing() {
        let (sys, vm, _) = dhcp_vm();
        let e = env(&sys);
        let _ = ls(&e);
        let _ = status(&e, &[], true, false);
        let _ = outstanding(&e);
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("/usr/bin/ssh ")),
            "{:?}",
            sys.calls()
        );
        let inv = inventory_of(&sys);
        assert_eq!(inv.hosts.len(), 1);
        assert_eq!(inv.hosts[0].address, vm.address);
    }

    #[test]
    fn session_extent_requires_a_complete_observation_before_claiming_an_empty_host() {
        use crate::protocol::SessionSnapshot;
        assert_eq!(session_extent(None), SessionExtent::Unobserved);
        let mut inventory = SessionSnapshot::default();
        assert_eq!(session_extent(Some(&inventory)), SessionExtent::Unobserved);
        inventory.rows.push(crate::protocol::HostedSession {
            instance_pid: 42,
            local_id: 1,
            sid: "s-observed".into(),
            ..crate::protocol::HostedSession::default()
        });
        assert_eq!(session_extent(Some(&inventory)), SessionExtent::Partial);
        inventory.complete = true;
        assert_eq!(session_extent(Some(&inventory)), SessionExtent::Complete);
        inventory.rows.clear();
        assert_eq!(session_extent(Some(&inventory)), SessionExtent::Complete);
    }

    #[test]
    fn hello_carries_partial_session_observations_without_promoting_missing_metadata() {
        let host = lab(1, UUID_1);
        let mut hello = hello_of(&host);
        hello.sessions.rows.push(crate::protocol::HostedSession {
            instance_pid: 42,
            local_id: 1,
            sid: "s-observed".into(),
            ..crate::protocol::HostedSession::default()
        });
        hello.sessions.complete = false;
        let mut value = aterm_json::to_value(&hello).unwrap();
        let observed: Hello = aterm_json::from_value(value.clone()).unwrap();
        assert_eq!(observed.sessions, hello.sessions);
        assert!(!observed.sessions.complete);
        value.as_object_mut().unwrap().remove("sessions_complete");
        let missing: Hello = aterm_json::from_value(value).unwrap();
        assert_eq!(missing.sessions.rows, hello.sessions.rows);
        assert!(!missing.sessions.complete);
        assert_eq!(
            session_extent(Some(&missing.sessions)),
            SessionExtent::Partial
        );
    }

    #[test]
    fn tabs_sweeps_live_hosts_and_prints_sessions_and_ai_cli_status() {
        let host = lab(1, UUID_1);
        let sys = controller(&[&host], false);
        let mut h = hello_of(&host);
        h.sessions.complete = true;
        h.sessions.rows.push(crate::protocol::HostedSession {
            instance_pid: 9001,
            local_id: 2,
            sid: "s-tab2".into(),
            state: "alive".into(),
            title: "Ship Fleet".into(),
            window: "0".into(),
            active: true,
            focused: true,
            program: Some("claude".into()),
            agent: Some("working".into()),
            agent_detail: Some("thinking".into()),
            agent_since_ms: Some(12_000),
            program_state: None,
            human_ms: Some(45_000),
            autopilot: Some("cruise".into()),
            role: Some("operator".into()),
            attention: None,
            supervisor: Some("aterm".into()),
            task: Some("finish lan".into()),
            cols: Some(211),
            rows: Some(70),
            cwd: Some("~/aterm".into()),
        });
        let hello_json = format!("{}\n", aterm_json::to_string(&h).unwrap());
        sys.on(&request(&host, "hello"), Ran::ok(&hello_json));

        let e = env(&sys);
        assert_eq!(tabs(&e, &[], false), ExitCode::SUCCESS);
        let saved = state_of(&sys, &host);
        let parsed: Hello = aterm_json::from_str(saved.hello.as_deref().unwrap()).unwrap();
        assert!(parsed.sessions.complete);
        assert_eq!(parsed.sessions.rows.len(), 1);
        assert_eq!(parsed.sessions.rows[0].program.as_deref(), Some("claude"));
        assert_eq!(
            format_session_ai_status(&parsed.sessions.rows[0]),
            "working: thinking (12s)"
        );
        assert_eq!(working_ai_sessions(&parsed.sessions.rows), 1);
        let mut quiet = parsed.sessions.rows[0].clone();
        for word in ["waiting", "unknown", "idle"] {
            quiet.agent = Some(word.into());
            assert_eq!(working_ai_sessions(std::slice::from_ref(&quiet)), 0);
        }
        quiet.agent = Some("busy".into());
        assert_eq!(working_ai_sessions(std::slice::from_ref(&quiet)), 1);
        assert_eq!(describe_host_hardware(None), "hardware unknown");
        assert_eq!(describe_host_hardware(Some("Mac17,4")), "Mac17,4");
        assert_eq!(
            format_os_and_ssh(None, None, None, None),
            "OS version unknown · SSH banner unknown"
        );
        assert_eq!(
            format_os_and_ssh(None, None, Some(u32::MAX), None),
            format!("Darwin {} · SSH banner unknown", u32::MAX)
        );
        assert_eq!(
            format_os_and_ssh(
                Some("27.0.1"),
                Some("26A1"),
                None,
                Some("OpenSSH_measured detail")
            ),
            "macOS 27.0.1 (26A1) · OpenSSH_measured"
        );
        let banner = |bytes: &[u8], expired| {
            use std::io::Write;
            use std::net::{Shutdown, TcpListener, TcpStream};
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut peer, _) = listener.accept().unwrap();
            peer.write_all(bytes).unwrap();
            peer.shutdown(Shutdown::Write).unwrap();
            let now = std::time::Instant::now();
            let deadline = if expired {
                now - Duration::from_secs(1)
            } else {
                now + Duration::from_secs(60)
            };
            read_ssh_banner(&client, deadline)
        };
        assert_eq!(
            banner(b"SSH-2.0-observed_peer\r\n", false).as_deref(),
            Some("observed_peer")
        );
        let mut oversized = b"SSH-2.0-".to_vec();
        oversized.extend([b'x'; 255]);
        oversized.push(b'\n');
        assert_eq!(banner(&oversized, false), None);
        assert_eq!(banner(b"SSH-2.0-partial", false), None);
        assert_eq!(banner(b"SSH-2.0-\xff\r\n", false), None);
        assert_eq!(banner(b"SSH-2.0-observed_peer\r\n", true), None);

        let summary = crate::protocol::HostVitals {
            gpu_name: Some("NVIDIA GB10".into()),
            gpu_temp_c: Some(45),
            rtt_us: Some(2_100),
            ..Default::default()
        }
        .format_summary_line();
        assert!(summary.contains("NVIDIA GB10"), "{summary}");
        assert!(summary.contains("45°C"), "{summary}");
        assert!(summary.contains("RTT 2.1 ms"), "{summary}");

        assert_eq!(
            monitor(&e, &[], false, false, true, false, 2),
            ExitCode::SUCCESS
        );
    }

    /// `here apply --plain`, `aterm init`'s root half for the Command Line Tools, prints no
    /// report and no line of its own and reads no baseline file (it applies the tools' own): it
    /// ends on 0 once that baseline is done, else on the code of why it did not finish
    /// ([`crate::init_clt::Unfinished::code`]), which `aterm init` says in plain words.
    #[test]
    fn here_apply_plain_says_nothing_and_exits_with_why_it_did_not_finish() {
        use crate::init_clt::Unfinished;
        let baseline = "/b/command-line-tools.toml";
        let mac = || {
            let sys = Fake::root();
            sys.born(std::process::id(), 123).running(&[]);
            sys.stat_of(
                "/dev/console",
                crate::port::Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            sys
        };
        let none_selected = || {
            Ran::exit(
                2,
                "xcode-select: error: Unable to get active developer directory.",
            )
        };
        let catalog = "/usr/sbin/softwareupdate -l";
        let plain = |sys: &Fake| {
            here_report(
                sys,
                Mode::Apply,
                HereBaseline::BuiltIn(crate::init_clt::RootPlan::WithTools),
                None,
                HereOutput::Plain,
            )
        };
        let class = |why: Unfinished| Some(ExitCode::from(why.code().unwrap()));

        let with_tools = |sys: &Fake| {
            for executable in [
                crate::items::developer::CLT_GIT,
                crate::items::developer::CLT_CLANG,
            ] {
                sys.stat_of(
                    executable,
                    crate::port::Stat {
                        uid: 0,
                        gid: 0,
                        mode: 0o755,
                        dir: false,
                        symlink: false,
                        len: 1,
                    },
                );
            }
            sys.on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            );
        };
        // Installed and selected already: done, and nothing to print.
        let done = mac();
        with_tools(&done);
        assert!(matches!(plain(&done), Ok(None)));

        // Independent work must not install or select CLT, even with no tools present.
        let independent = mac();
        assert!(matches!(
            here_report(
                &independent,
                Mode::Apply,
                HereBaseline::BuiltIn(crate::init_clt::RootPlan::StepsOnly),
                None,
                HereOutput::Plain
            ),
            Ok(None)
        ));
        assert!(
            independent
                .written(crate::items::developer::PLACEHOLDER)
                .is_none()
        );
        assert!(
            !independent
                .calls()
                .iter()
                .any(|call| call.contains("softwareupdate") || call.contains("xcode-select")),
            "{:?}",
            independent.calls()
        );

        // Apple's catalog out of reach waits in the same process until an explicit Stop.
        let unreached = mac();
        unreached
            .on("/usr/bin/xcode-select -p", none_selected())
            .on(catalog, Ran::exit(1, "catalog network unavailable"));
        unreached.on_sleep(|sys, _| {
            sys.stop_now();
        });
        assert!(plain(&unreached).err() == class(Unfinished::Stopped));

        // Another install holds the placeholder; the same authorized run waits until Stop.
        let busy = mac();
        busy.on("/usr/bin/xcode-select -p", none_selected())
            .running(&["/usr/sbin/softwareupdate"])
            .file(crate::items::developer::PLACEHOLDER, b"");
        busy.on_sleep(|sys, _| {
            sys.stop_now();
        });
        assert!(plain(&busy).err() == class(Unfinished::Stopped));
        assert_eq!(
            busy.written(crate::items::developer::PLACEHOLDER),
            Some(Vec::new()),
            "Stop preserves the other installer's marker"
        );

        // A baseline file the person's account wrote is never what the root half applies: one
        // that asks for nothing still installs the tools.
        let planted = mac();
        planted
            .file(baseline, b"schema = 1\n")
            .on("/usr/bin/xcode-select -p", none_selected())
            .on(catalog, Ran::exit(1, "catalog network unavailable"));
        planted.on_sleep(|sys, _| {
            sys.stop_now();
        });
        assert!(plain(&planted).err() == class(Unfinished::Stopped));

        // What the root half cannot even start is said as the other class, never in its words.
        let mut user = Fake::default();
        user.euid = 501;
        user.file(baseline, b"schema = 1\n");
        assert!(plain(&user).err() == class(Unfinished::Other));
        assert!(
            here_report(
                &user,
                Mode::Apply,
                HereBaseline::File(Path::new(baseline)),
                None,
                HereOutput::Report
            )
            .err()
                == Some(ExitCode::from(2)),
            "a person's own run keeps its usage refusal"
        );
        let unread = Fake::root();
        assert!(plain(&unread).err() == class(Unfinished::Other));

        // The accepted root episode must retain the broker after publishing
        // its files. Completed tools cannot turn a transient bootstrap failure
        // into a successful root transaction with no helper to reach the LAN.
        let source = Path::new("/Applications/aterm.app");
        let image = aterm_uds::lan::install::test_image(0x0100_000c, &[b"this build"]);
        let broker_case = || {
            let mut sys = mac();
            with_tools(&sys);
            sys.running_app = Some(source.to_path_buf());
            sys.file("/Applications/aterm.app/Contents/MacOS/aterm", &image);
            sys.stat_of(
                source.to_str().unwrap(),
                crate::port::Stat {
                    uid: 501,
                    gid: 20,
                    mode: 0o755,
                    dir: true,
                    symlink: false,
                    len: 0,
                },
            );
            sys.app_check(|_, _| Ok(()));
            for (path, ino) in [
                (aterm_uds::lan::IMAGE_BUNDLE, 1),
                (aterm_uds::lan::PLIST_PATH, 2),
            ] {
                sys.file_id_of(
                    Path::new(path),
                    crate::port::FileId {
                        dev: 1,
                        ino,
                        born_ns: None,
                    },
                );
            }
            sys
        };
        let broker = broker_case();
        const BROKER_PRINT: &str = "/bin/launchctl print system/systems.alab.aterm-lan";
        const BROKER_BOOTSTRAP: &str =
            "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist";
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        broker.simulate({
            let attempts = std::sync::Arc::clone(&attempts);
            move |_, command| match command.display().as_str() {
                BROKER_PRINT => Some(if attempts.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                    Ran::exit(113, "Could not find service")
                } else {
                    Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n")
                }),
                BROKER_BOOTSTRAP => Some(
                    if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        Ran::exit(5, "transient bootstrap failure")
                    } else {
                        Ran::ok("")
                    },
                ),
                _ => None,
            }
        });
        broker
            .on_lan_broker(Err("the published broker is still starting".into()))
            .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: aterm_stamp::build_id(),
                direct: Some("a direct path is not the installed helper".into()),
            }))
            .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "a valid newer source build".into(),
                direct: None,
            }))
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(
                "/bin/launchctl enable system/systems.alab.aterm-lan",
                Ran::ok(""),
            )
            .on(
                "/bin/launchctl kickstart system/systems.alab.aterm-lan",
                Ran::ok(""),
            );
        let ended = plain(&broker);
        let reported_success = matches!(ended, Ok(None));
        let published = crate::items::lan::reading(&broker).unwrap().unwrap();
        assert!(published.current());
        assert!(!published.owed());
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the same accepted root episode must retry bootstrap before reporting success={reported_success}; \
             files are current but the first bootstrap failed; journal={:?}",
            broker.written(Fake::JOURNAL)
        );
        assert!(reported_success);
        assert_eq!(
            broker
                .calls()
                .iter()
                .filter(|call| call.starts_with("clone "))
                .count(),
            1,
            "launch/readiness recovery keeps the already published image"
        );

        // Already healthy: neither image publication nor launchd mutation is owed.
        let healthy = broker_case();
        healthy
            .file(aterm_uds::lan::IMAGE_EXECUTABLE, &image)
            .file(
                aterm_uds::lan::PLIST_PATH,
                aterm_uds::lan::install::plist_text(source).as_bytes(),
            )
            .on(
                BROKER_PRINT,
                Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
            )
            .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "a valid already-serving build".into(),
                direct: None,
            }));
        assert!(matches!(plain(&healthy), Ok(None)));
        assert!(
            !healthy.calls().iter().any(|call| call.starts_with("clone ")
                || call.starts_with("/bin/launchctl bootstrap")
                || call.starts_with("/bin/launchctl bootout")
                || call.starts_with("/bin/launchctl kickstart"))
        );

        for replaced in [false, true] {
            let interrupted = broker_case();
            interrupted
                .on(BROKER_PRINT, Ran::exit(113, "Could not find service"))
                .on(
                    "/bin/launchctl enable system/systems.alab.aterm-lan",
                    Ran::ok(""),
                )
                .on(
                    BROKER_BOOTSTRAP,
                    Ran::exit(5, "bootstrap still unavailable"),
                );
            interrupted.on_sleep(move |sys, _| {
                if replaced {
                    sys.file(
                        aterm_uds::lan::IMAGE_EXECUTABLE,
                        &aterm_uds::lan::install::test_image(
                            0x0100_000c,
                            &[b"a newer root owner's image"],
                        ),
                    );
                } else {
                    sys.stop_now();
                }
            });
            assert!(
                plain(&interrupted).err()
                    == class(if replaced {
                        Unfinished::Other
                    } else {
                        Unfinished::Stopped
                    })
            );
            assert_eq!(
                interrupted
                    .calls()
                    .iter()
                    .filter(|call| *call == BROKER_BOOTSTRAP)
                    .count(),
                1
            );
            assert_eq!(
                interrupted
                    .calls()
                    .iter()
                    .filter(|call| call.starts_with("clone "))
                    .count(),
                1
            );
        }

        // Another root owner can publish after our final ownership operation,
        // before any post-publication read. It must never become our receipt.
        let overwritten = broker_case();
        let other_image =
            aterm_uds::lan::install::test_image(0x0100_000c, &[b"another root owner's code"]);
        let other_plist = aterm_uds::lan::install::plist_text(Path::new("/Applications/other.app"));
        overwritten.on_chown({
            let (other_image, other_plist) = (other_image.clone(), other_plist.clone());
            move |sys, path| {
                if path == Path::new(aterm_uds::lan::PLIST_PATH) {
                    sys.file(aterm_uds::lan::IMAGE_EXECUTABLE, &other_image)
                        .file(aterm_uds::lan::PLIST_PATH, other_plist.as_bytes());
                }
            }
        });
        overwritten
            .on(
                BROKER_PRINT,
                Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
            )
            .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
                protocol: aterm_uds::lan::PROTOCOL,
                build: "another-build".into(),
                direct: None,
            }))
            .on(
                "/bin/launchctl bootout system/systems.alab.aterm-lan",
                Ran::ok(""),
            )
            .on(
                "/bin/launchctl enable system/systems.alab.aterm-lan",
                Ran::ok(""),
            )
            .on(
                BROKER_BOOTSTRAP,
                Ran::exit(5, "the other owner's job must not be restarted"),
            );
        overwritten.on_sleep(|sys, _| {
            sys.stop_now();
        });
        let ended = plain(&overwritten);
        assert!(
            !overwritten
                .calls()
                .iter()
                .any(|call| call.starts_with("/bin/launchctl bootout")
                    || call.starts_with("/bin/launchctl bootstrap")),
            "another owner's publication is never adopted as our own: {:?}",
            overwritten.calls()
        );
        assert!(ended.err() == class(Unfinished::Other));
        assert_eq!(
            overwritten.written(aterm_uds::lan::IMAGE_EXECUTABLE),
            Some(other_image)
        );
        assert_eq!(
            overwritten.written(aterm_uds::lan::PLIST_PATH),
            Some(other_plist.into_bytes())
        );

        // Strict code refusal is terminal before either publication or launch.
        let refused = broker_case();
        refused.app_check(|_, _| {
            Err(aterm_keeper::identity::CheckError::Refused(
                "different designated identity".into(),
            ))
        });
        assert!(plain(&refused).err() == class(Unfinished::Other));
        assert!(refused.written(aterm_uds::lan::IMAGE_EXECUTABLE).is_none());
        assert!(
            !refused
                .calls()
                .iter()
                .any(|call| call.starts_with("exchange ")
                    || call.starts_with("/bin/launchctl bootstrap"))
        );

        let serves = |sys: &Fake| {
            sys.on(BROKER_PRINT, Ran::exit(113, "Could not find service"))
                .on(BROKER_PRINT, Ran::exit(113, "Could not find service"))
                .on(
                    BROKER_PRINT,
                    Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
                )
                .on(
                    "/bin/launchctl enable system/systems.alab.aterm-lan",
                    Ran::ok(""),
                )
                .on(BROKER_BOOTSTRAP, Ran::ok(""))
                .on(
                    "/bin/launchctl kickstart system/systems.alab.aterm-lan",
                    Ran::ok(""),
                )
                .on_lan_broker(Ok(aterm_uds::lan::BrokerHello {
                    protocol: aterm_uds::lan::PROTOCOL,
                    build: "a valid staged build".into(),
                    direct: None,
                }));
        };
        let payload = "Contents/Resources/payload";
        for unanswered in [false, true] {
            let recovering = broker_case();
            recovering.file(source.join(payload).to_str().unwrap(), b"initial");
            let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            recovering.app_check({
                let checks = std::sync::Arc::clone(&checks);
                move |sys, candidate| {
                    let first = checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                    if unanswered && first {
                        return Err(aterm_keeper::identity::CheckError::Unanswered(
                            "signing service silent".into(),
                        ));
                    }
                    if !unanswered
                        && sys.read_file(&candidate.join(payload)).unwrap() != b"repaired"
                    {
                        return Err(aterm_keeper::identity::CheckError::Unchecked(
                            "resource check incomplete (-67054)".into(),
                        ));
                    }
                    Ok(())
                }
            });
            recovering.on_sleep(move |sys, _| {
                if !unanswered {
                    sys.file(source.join(payload).to_str().unwrap(), b"repaired");
                }
            });
            serves(&recovering);
            assert!(matches!(plain(&recovering), Ok(None)));
            assert_eq!(
                recovering
                    .read_file(&source.join(aterm_uds::lan::install::BUNDLE_EXECUTABLE))
                    .unwrap(),
                image
            );
            assert_eq!(
                recovering.written(&format!("{}/{payload}", aterm_uds::lan::IMAGE_BUNDLE)),
                Some(if unanswered {
                    b"initial".to_vec()
                } else {
                    b"repaired".to_vec()
                })
            );
            assert_eq!(
                recovering
                    .calls()
                    .iter()
                    .filter(|call| call.starts_with("clone "))
                    .count(),
                if unanswered { 1 } else { 2 }
            );
            assert_eq!(checks.load(std::sync::atomic::Ordering::SeqCst), 2);
        }

        let moved_source = broker_case();
        let successor = aterm_uds::lan::install::test_image(0x0100_000c, &[b"valid successor"]);
        let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        moved_source.app_check({
            let checks = std::sync::Arc::clone(&checks);
            move |_, _| {
                if checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    Err(aterm_keeper::identity::CheckError::Unanswered(
                        "service silent while source updates".into(),
                    ))
                } else {
                    Ok(())
                }
            }
        });
        moved_source.on_sleep({
            let successor = successor.clone();
            move |sys, _| {
                sys.file(
                    source
                        .join(aterm_uds::lan::install::BUNDLE_EXECUTABLE)
                        .to_str()
                        .unwrap(),
                    &successor,
                );
            }
        });
        serves(&moved_source);
        assert!(matches!(plain(&moved_source), Ok(None)));
        assert_eq!(
            moved_source.written(aterm_uds::lan::IMAGE_EXECUTABLE),
            Some(successor)
        );
        assert_eq!(
            moved_source
                .calls()
                .iter()
                .filter(|call| call.starts_with("clone "))
                .count(),
            2
        );
        assert_eq!(
            checks.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the obsolete first candidate is not checked again"
        );

        // An unreadable prior code directory is repairable; it need not be valid
        // code merely to bind its inode before replacing the old bundle.
        let malformed = broker_case();
        malformed.stat_of(
            aterm_uds::lan::IMAGE_BUNDLE,
            crate::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o755,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        malformed.file(aterm_uds::lan::IMAGE_EXECUTABLE, b"malformed old image");
        serves(&malformed);
        assert!(matches!(plain(&malformed), Ok(None)));
        assert_eq!(
            malformed.written(aterm_uds::lan::IMAGE_EXECUTABLE),
            Some(image.clone())
        );

        // A foreign root publication during a slow code check must not be
        // overwritten by the now-obsolete unadmitted candidate.
        let overtaken = broker_case();
        let replacement = aterm_uds::lan::install::test_image(0x0100_000c, &[b"foreign root"]);
        overtaken.app_check({
            let replacement = replacement.clone();
            move |sys, _| {
                sys.stat_of(
                    aterm_uds::lan::IMAGE_BUNDLE,
                    crate::port::Stat {
                        uid: 0,
                        gid: 0,
                        mode: 0o755,
                        dir: true,
                        symlink: false,
                        len: 0,
                    },
                );
                sys.file(aterm_uds::lan::IMAGE_EXECUTABLE, &replacement)
                    .file(aterm_uds::lan::PLIST_PATH, b"foreign job");
                sys.file_id_of(
                    Path::new(aterm_uds::lan::IMAGE_BUNDLE),
                    crate::port::FileId {
                        dev: 1,
                        ino: 99,
                        born_ns: None,
                    },
                );
                sys.file_id_of(
                    Path::new(aterm_uds::lan::PLIST_PATH),
                    crate::port::FileId {
                        dev: 1,
                        ino: 100,
                        born_ns: None,
                    },
                );
                Ok(())
            }
        });
        assert!(plain(&overtaken).err() == class(Unfinished::Other));
        assert_eq!(
            overtaken.written(aterm_uds::lan::IMAGE_EXECUTABLE),
            Some(replacement)
        );
        assert_eq!(
            overtaken.written(aterm_uds::lan::PLIST_PATH),
            Some(b"foreign job".to_vec())
        );
        assert!(
            !overtaken
                .calls()
                .iter()
                .any(|call| call.starts_with("exchange ")
                    || call.starts_with("/bin/launchctl bootstrap"))
        );

        let stopped_check = broker_case();
        stopped_check.app_check(|sys, _| {
            sys.stop_now();
            Ok(())
        });
        assert!(plain(&stopped_check).err() == class(Unfinished::Stopped));
        assert!(
            stopped_check
                .written(aterm_uds::lan::IMAGE_EXECUTABLE)
                .is_none()
        );
    }

    /// `--name` gives the Mac its swarm label as Computer Name, Local hostname and host name
    /// under the one password the tools take, journaled; a name macOS refuses leaves the exit
    /// saying the tools alone. NEGATIVE CONTROLS: with no name the root half runs no `scutil
    /// --set`, and with the tools out of reach the exit is still theirs.
    #[test]
    fn here_apply_plain_names_the_mac_and_exits_on_the_tools_alone() {
        use crate::init_clt::Unfinished;
        let console = crate::port::Stat {
            uid: 0,
            gid: 0,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        };
        let tools = || {
            let sys = Fake::root();
            sys.stat_of("/dev/console", console);
            for executable in [
                crate::items::developer::CLT_GIT,
                crate::items::developer::CLT_CLANG,
            ] {
                sys.stat_of(
                    executable,
                    crate::port::Stat {
                        uid: 0,
                        gid: 0,
                        mode: 0o755,
                        dir: false,
                        symlink: false,
                        len: 1,
                    },
                );
            }
            sys.on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            );
            sys
        };
        let sets = |sys: &Fake| -> Vec<String> {
            sys.calls()
                .into_iter()
                .filter(|call| call.contains("scutil --set"))
                .collect()
        };
        let plain = |sys: &Fake, name| {
            here_report(
                sys,
                Mode::Apply,
                HereBaseline::BuiltIn(crate::init_clt::RootPlan::WithTools),
                name,
                HereOutput::Plain,
            )
        };
        let journal =
            |sys: &Fake| String::from_utf8(sys.written(Fake::JOURNAL).unwrap_or_default()).unwrap();

        // Read three times each: the Mac's facts, the probe, and the check after the set.
        let named = tools();
        named
            .on(
                "/usr/sbin/scutil --get ComputerName",
                Ran::ok("Andrew\u{2019}s MacBook Pro\n"),
            )
            .on(
                "/usr/sbin/scutil --get ComputerName",
                Ran::ok("Andrew\u{2019}s MacBook Pro\n"),
            )
            .on("/usr/sbin/scutil --get ComputerName", Ran::ok("m42\n"))
            .on(
                "/usr/sbin/scutil --get LocalHostName",
                Ran::ok("Andrews-MacBook-Pro\n"),
            )
            .on(
                "/usr/sbin/scutil --get LocalHostName",
                Ran::ok("Andrews-MacBook-Pro\n"),
            )
            .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"))
            .on(
                "/usr/sbin/scutil --get HostName",
                Ran::exit(1, "HostName: not set\n"),
            )
            .on("/usr/sbin/scutil --get HostName", Ran::ok("m42\n"))
            .on("/usr/sbin/scutil --set ComputerName m42", Ran::ok(""))
            .on("/usr/sbin/scutil --set LocalHostName m42", Ran::ok(""))
            .on("/usr/sbin/scutil --set HostName m42", Ran::ok(""));
        assert!(matches!(plain(&named, Some("m42")), Ok(None)));
        assert_eq!(
            sets(&named),
            [
                "/usr/sbin/scutil --set ComputerName m42",
                "/usr/sbin/scutil --set LocalHostName m42",
                "/usr/sbin/scutil --set HostName m42"
            ]
        );
        let said = journal(&named);
        assert!(said.contains("names.computer-name"), "{said}");
        assert!(said.contains("names.local-host-name"), "{said}");
        assert!(said.contains("names.host-name"), "{said}");

        // Already named: nothing is set again.
        let already = tools();
        already
            .on("/usr/sbin/scutil --get ComputerName", Ran::ok("m42\n"))
            .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"));
        assert!(matches!(plain(&already, Some("m42")), Ok(None)));
        assert_eq!(sets(&already), Vec::<String>::new());

        // macOS refuses the name: the tools' exit stands.
        let refused = tools();
        refused
            .on(
                "/usr/sbin/scutil --get ComputerName",
                Ran::ok("Andrew\u{2019}s MacBook Pro\n"),
            )
            .on(
                "/usr/sbin/scutil --get LocalHostName",
                Ran::ok("Andrews-MacBook-Pro\n"),
            )
            .on(
                "/usr/sbin/scutil --set ComputerName m42",
                Ran::exit(1, "SCPreferencesCommitChanges() failed"),
            )
            .on(
                "/usr/sbin/scutil --set LocalHostName m42",
                Ran::exit(1, "SCPreferencesCommitChanges() failed"),
            );
        assert!(matches!(plain(&refused, Some("m42")), Ok(None)));
        assert_eq!(sets(&refused).len(), 2, "{:?}", refused.calls());

        // NEGATIVE CONTROL: no name, no rename.
        let unnamed = tools();
        assert!(matches!(plain(&unnamed, None), Ok(None)));
        assert_eq!(sets(&unnamed), Vec::<String>::new());

        // An unavailable catalog retains custody beside the completed naming steps.
        let unreached = Fake::root();
        unreached.born(std::process::id(), 123).running(&[]);
        unreached
            .stat_of("/dev/console", console)
            .on(
                "/usr/bin/xcode-select -p",
                Ran::exit(
                    2,
                    "xcode-select: error: Unable to get active developer directory.",
                ),
            )
            .on(
                "/usr/sbin/softwareupdate -l",
                Ran::exit(1, "catalog network unavailable"),
            )
            .on("/usr/sbin/scutil --get ComputerName", Ran::ok("m42\n"))
            .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"));
        unreached.on_sleep(|sys, _| {
            sys.stop_now();
        });
        assert!(
            plain(&unreached, Some("m42")).err()
                == Some(ExitCode::from(Unfinished::Stopped.code().unwrap()))
        );
    }

    #[test]
    fn here_journals_each_change_in_the_journal_of_the_system_it_runs_on() {
        let journaled = |sys: &Fake| -> Vec<aterm_json::Value> {
            sys.written(Fake::JOURNAL)
                .map(|bytes| String::from_utf8(bytes).unwrap())
                .unwrap_or_default()
                .lines()
                .map(|l| aterm_json::from_str(l).unwrap())
                .collect()
        };
        let baseline = "/b/baseline.toml";
        let text: &[u8] = b"schema = 1\n[names]\nhost_name = \"lab-07.lab.example\"\n";
        let mac = || {
            let sys = Fake::root();
            sys.file(baseline, text).stat_of(
                "/dev/console",
                crate::port::Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            sys
        };
        let sys = mac();
        sys.on(
            "/usr/sbin/scutil --get HostName",
            Ran::ok("lab-07.lab.example\n"),
        );
        assert_eq!(
            here(
                &sys,
                Mode::Apply,
                HereBaseline::File(Path::new(baseline)),
                None,
                HereOutput::Json
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(
            journaled(&sys),
            Vec::<aterm_json::Value>::new(),
            "a line for a run that changed nothing"
        );
        assert!(
            !sys.calls().iter().any(|call| call.contains("hidutil")),
            "a baseline with no keyboard table must not search live devices: {:?}",
            sys.calls()
        );
        let sys = mac();
        sys.on(
            "/usr/sbin/scutil --get HostName",
            Ran::exit(1, "HostName: not set"),
        )
        .on(
            "/usr/sbin/scutil --get HostName",
            Ran::ok("lab-07.lab.example\n"),
        )
        .on(
            "/usr/sbin/scutil --set HostName lab-07.lab.example",
            Ran::ok(""),
        );
        assert_eq!(
            here(
                &sys,
                Mode::Apply,
                HereBaseline::File(Path::new(baseline)),
                None,
                HereOutput::Json
            ),
            ExitCode::SUCCESS
        );
        let lines = journaled(&sys);
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert_eq!(line["id"].as_str(), Some("names.host-name"));
        assert_eq!(line["from"].as_str(), Some("not set"));
        assert_eq!(line["to"].as_str(), Some("\"lab-07.lab.example\""));
        assert_eq!(
            line["revert"].as_str(),
            Some("/usr/sbin/scutil --set HostName ''")
        );
    }

    #[test]
    fn monitor_tab_opener_speaks_silent_auth_and_explicit_socket_tokens() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = aterm_tempfile::Builder::new()
            .prefix("at-mon-open-")
            .tempdir_in("/tmp")
            .unwrap();
        let sym_sock = dir.path().join("aterm-1111.sock");
        let real_tok = dir.path().join("real.token");
        std::fs::write(&real_tok, "secret1111\n").unwrap();
        std::os::unix::fs::symlink(&real_tok, dir.path().join("aterm-1111.token")).unwrap();
        let _unused = UnixListener::bind(&sym_sock).unwrap();

        let explicit_sock = dir.path().join("custom.sock");
        std::fs::write(dir.path().join("custom.sock.token"), "cafef00d\n").unwrap();
        let listener = UnixListener::bind(&explicit_sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut auth = String::new();
            reader.read_line(&mut auth).unwrap();
            assert_eq!(auth, "AUTH cafef00d\n");
            let mut verb = String::new();
            reader.read_line(&mut verb).unwrap();
            assert_eq!(verb, "open app settings /monitor\n");
            conn.write_all(b"OK\n").unwrap();
        });

        // The server above always answers: a hang detector, never the opener's 250 ms.
        let opened = open_native_monitor_tab_in_candidates(
            &[sym_sock, explicit_sock.clone()],
            Duration::from_secs(60),
        );
        server.join().unwrap();
        assert_eq!(opened.as_deref(), explicit_sock.to_str());
        assert_eq!(
            device_info(&[
                b"model=Mac16,1".to_vec(),
                b"osxvers=26".to_vec(),
                b"x".to_vec()
            ]),
            (Some("Mac16,1".to_owned()), Some(26))
        );
        assert_eq!(device_info(&[b"model=".to_vec()]), (None, None));
    }

    /// A window name a host reports reaches the owner's terminal escaped, as the supervisor
    /// beside it does. NEGATIVE CONTROL: a plain window name prints as it is.
    #[test]
    fn a_host_s_window_name_cannot_write_to_the_owner_s_terminal() {
        let tab = |window: &str| crate::protocol::HostedSession {
            instance_pid: 99,
            window: window.into(),
            focused: true,
            ..crate::protocol::HostedSession::default()
        };
        let hostile = tab("0\x1b[2J\x1b]52;c;cHdu\x07");
        let line = instance_header(99, &[&hostile], "80\u{d7}24");
        assert!(!line.contains('\x1b') && !line.contains('\x07'), "{line:?}");
        assert!(line.contains("window 0\\u{1b}[2J"), "{line:?}");
        let plain = tab("3");
        let line = instance_header(99, &[&plain], "80\u{d7}24");
        assert!(
            line.contains("aterm-99 \u{b7} window 3 (focused) \u{b7} grid 80\u{d7}24"),
            "{line:?}"
        );
    }

    /// A host that answers with its UUID in another case is the host its inventory names (the
    /// UUID is hex). NEGATIVE CONTROL: a host that answers with another UUID is not.
    #[test]
    fn hello_matches_the_host_s_uuid_in_either_case() {
        let host = lab(1, &UUID_1.to_ascii_lowercase());
        let sys = controller(&[&host], false);
        let mut hello = hello_of(&host);
        hello.uuid = UUID_1.to_owned();
        sys.on(
            &request(&host, "hello"),
            Ran::ok(&aterm_json::to_string(&hello).unwrap()),
        );
        assert_eq!(ask_hello(&env(&sys), &host).unwrap().uuid, UUID_1);

        let sys = controller(&[&host], false);
        hello.uuid = UUID_2.to_owned();
        sys.on(
            &request(&host, "hello"),
            Ran::ok(&aterm_json::to_string(&hello).unwrap()),
        );
        let refused = ask_hello(&env(&sys), &host).unwrap_err();
        assert!(
            matches!(&refused, HelloError::Answer(why) if why.contains(&format!("it answers as {UUID_2}"))),
            "{refused:?}"
        );
    }

    /// The agent socket the walk loads the key into, and every envelope it seals, is
    /// [`Env::agent_sock`], never this process's own `SSH_AUTH_SOCK`: each run is handed exactly
    /// the injected socket. NEGATIVE CONTROL: with no socket a walk's key with a passphrase is
    /// refused before `ssh-add` runs.
    #[test]
    fn a_walk_loads_and_seals_through_the_env_s_agent_socket() {
        let sys = Fake::root();
        let key = format!("{ROOT}/keys/signing");
        sys.on(
            &format!("/usr/bin/ssh-keygen -y -P '' -f {key}"),
            Ran::exit(
                255,
                "Load key: incorrect passphrase supplied to decrypt private key\n",
            ),
        );
        let held = format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -T {key}.pub");
        sys.on(&held, Ran::exit(1, ""));
        sys.on(
            &format!("tty SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -t 3600 {key}"),
            Ran::ok(""),
        );
        sys.on(
            &format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -d {key}"),
            Ran::ok(""),
        );
        let handed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&handed);
        sys.simulate(move |_, cmd| {
            seen.lock().unwrap().push((cmd.display(), cmd.env.clone()));
            None
        });
        let seen = std::sync::Arc::clone(&handed);
        sys.tty_model(move |_, cmd| {
            seen.lock()
                .unwrap()
                .push((format!("tty {}", cmd.display()), cmd.env.clone()));
            None
        });
        let mut e = env(&sys);
        e.agent_sock = Some("/private/walk-agent.sock".into());
        let (how, loaded) = person_signing(&e).unwrap();
        assert_eq!(how, Signing::Agent);
        assert!(loaded.is_some());
        let sign = format!(
            "SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-keygen -Y sign -U -f {key}.pub -n aterm-machines"
        );
        let body = Envelope {
            schema: crate::SCHEMA,
            host: UUID_1.into(),
            host_name: "lab-01".into(),
            serial: 1,
            issued: 10,
            expires: 10 + ENVELOPE_LIFETIME,
            action: Action::Plan,
            disrupt: Disrupt::WhenIdle,
            idle_secs: 900,
            baseline: None,
            artifacts: Vec::new(),
            exec: None,
            upgrade: None,
            rekey: None,
        };
        let unsigned = seal_with(&e, &[body], Path::new(&key), how).unwrap_err();
        assert!(unsigned.contains("did not sign"), "{unsigned}");
        drop(loaded);
        let handed = handed.lock().unwrap();
        let sock = vec![(
            "SSH_AUTH_SOCK".to_owned(),
            "/private/walk-agent.sock".to_owned(),
        )];
        for want in [
            held.clone(),
            format!("tty SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -t 3600 {key}"),
            sign,
            format!("SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -d {key}"),
        ] {
            let ran: Vec<_> = handed
                .iter()
                .filter(|(line, _)| line.starts_with(&want))
                .collect();
            assert!(!ran.is_empty(), "{want}: {handed:#?}");
            assert!(ran.iter().all(|(_, env)| *env == sock), "{want}: {ran:#?}");
        }

        let sys = Fake::root();
        sys.on(
            &format!("/usr/bin/ssh-keygen -y -P '' -f {key}"),
            Ran::exit(
                255,
                "Load key: incorrect passphrase supplied to decrypt private key\n",
            ),
        );
        let refused = person_signing(&env(&sys)).map(|(how, _)| how).unwrap_err();
        assert!(refused.contains("no ssh-agent"), "{refused}");
        assert!(
            !sys.calls().iter().any(|c| c.contains("ssh-add")),
            "{:?}",
            sys.calls()
        );
    }
}
