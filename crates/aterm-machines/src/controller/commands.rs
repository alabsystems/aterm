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
use aterm_capacity::host_vitals::HostVitals;
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
    vitals: Option<&HostVitals>,
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
            let vitals = ssh_banner.is_some().then(|| HostVitals {
                rtt_us,
                ..HostVitals::default()
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
        waits_on: None,
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

/// What a plain run of [`here`] (`aterm init`'s root half) takes beside its built-in baseline
/// ([`HereBaseline::BuiltIn`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Plain<'a> {
    /// The generated names to give the Mac (`--name`, a [`crate::init_clt::plain_name`], and
    /// `--nickname`).
    pub name: Option<&'a aterm_types::machine_name::GeneratedNames>,
    /// Leave developer mode as it reads (`--leave-developer-mode`,
    /// [`crate::init_clt::LEAVE_DEVELOPER_MODE`]): the baseline carries no `[developer]
    /// debugger`.
    pub leave_developer_mode: bool,
}

/// `aterm machines here plan|apply <baseline.toml>`: run a baseline directly on this Mac; with
/// [`HereOutput::Plain`], setup's compiled root plan, narrowed as `plain` says; a file source is
/// refused.
#[must_use = "handle or propagate the command's exit status"]
pub fn here(
    sys: &dyn System,
    mode: Mode,
    source: HereBaseline<'_>,
    plain: Plain<'_>,
    output: HereOutput,
) -> ExitCode {
    match here_report(sys, mode, source, plain, output) {
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
/// developer mode on (unless `given` says to leave it) and puts the rule in place that names the
/// Mac after admission ([`crate::root_steps`]), and with `given`'s name (a
/// [`crate::init_clt::plain_name`]) gives the Mac that name; whether those rows changed or failed
/// goes to the journal, and the exit says the tools alone, so a step macOS refused never reads
/// as tools that did not install.
/// The explicit naming-permission plan instead has one decisive naming-rule row.
fn here_report(
    sys: &dyn System,
    mode: Mode,
    source: HereBaseline<'_>,
    given: Plain<'_>,
    output: HereOutput,
) -> Result<Option<(Host, HostReport)>, ExitCode> {
    let name = given.name;
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
    let name_rule_only = matches!(
        source,
        HereBaseline::BuiltIn(crate::init_clt::RootPlan::NameRuleOnly)
    );
    let permission_console = if name_rule_only {
        match crate::facts::console_account(sys) {
            Ok(user) => Some(user),
            Err(why) => refuse!(ExitCode::FAILURE, "aterm machines here: {why}"),
        }
    } else {
        None
    };
    let mut facts = match &permission_console {
        Some(user) => crate::model::HostFacts {
            console_user: Some(user.name.clone()),
            ..crate::model::HostFacts::default()
        },
        None => crate::facts::gather_without_keyboards(sys),
    };
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
        if given.leave_developer_mode {
            developer.debugger = None;
        }
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
        if let Some(user) = permission_console.as_ref().filter(|user| &user.name == u) {
            users.push(user.clone());
            continue;
        }
        match crate::facts::user(sys, u) {
            Ok(user) => users.push(user),
            Err(e) => refuse!(ExitCode::FAILURE, "aterm machines here: {e}"),
        }
    }
    let console = permission_console.or_else(|| {
        facts
            .console_user
            .as_ref()
            .and_then(|c| crate::facts::user(sys, c).ok())
    });
    let ctx = crate::items::Ctx {
        cache_dir: Path::new(crate::AGENT_ROOT).join("cache"),
        disrupt: true,
        facts,
        users,
        console,
        host_name: name,
    };
    let mut items = if name_rule_only {
        vec![Box::new(crate::root_steps::NameRule::refresh()) as Box<dyn crate::items::Item>]
    } else {
        // The built-in naming operation has narrower authority than a general machine
        // baseline: ComputerName must not implicitly add SMB preference writes.
        let root_names = if plain { baseline.names.take() } else { None };
        let expanded = (|| {
            let mut items = match root_names {
                Some(ref table) => crate::items::names::expand_system_names(table, &ctx)?,
                None => Vec::new(),
            };
            items.extend(crate::items::expand(&baseline, &ctx)?);
            Ok::<_, String>(items)
        })();
        match expanded {
            Ok(i) => i,
            Err(e) => refuse!(ExitCode::FAILURE, "aterm machines here: {e}"),
        }
    };
    // The compiled setup transaction disables data-volume indexing alongside its other
    // root work. Refresh the naming permission too, so matching bytes with an old read
    // ACL are repaired within that authorization. User baselines keep their policy.
    if matches!(source, HereBaseline::BuiltIn(_)) && !name_rule_only {
        items.push(Box::new(crate::init_spotlight::Indexing));
        for item in &mut items {
            if item.id() == "names.admitted-name-rule" {
                *item = Box::new(crate::root_steps::NameRule::refresh());
            }
        }
    }
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
        HereBaseline::BuiltIn(crate::init_clt::RootPlan::NameRuleOnly) => {
            "sudo aterm machines here apply --plain --name-rule-only".into()
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
        if name_rule_only {
            return match rows.as_slice() {
                [row]
                    if row.id == "names.admitted-name-rule"
                        && matches!(row.outcome, Outcome::Ok | Outcome::Changed) =>
                {
                    Ok(None)
                }
                _ => Err(other()),
            };
        }
        // The tools, an applicable broker and Spotlight repair must finish before root custody
        // ends successfully. Optional naming and debugger failures remain journaled;
        // they cannot cause another automatic administrator episode.
        let unfinished = rows
            .iter()
            .filter(|row| {
                row.id == crate::items::github::CLT_ITEM
                    || row.id == "lan.broker"
                    || row.id == "init.spotlight.indexing"
            })
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
#[path = "commands_tests.rs"]
mod tests;
