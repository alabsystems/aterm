// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The guest steps of `lab mac nux` that are more than one command: each a verb of the guest
//! helper, so the guest side is Trust code the lane's tests run, never logic in a shell line.
//!
//! - `browse SECS DIR SERVICE...`: one `dns-sd -B <service> local.` per service for SECS seconds,
//!   each under `script -q -t 0` into `DIR/heard<service>.txt`, then each one's process group
//!   killed and reaped. `-t 0` flushes the transcript at each write: `script`'s default holds its
//!   output up to 30 s, so a kill before then left an empty transcript, which read as "nothing
//!   heard" (measured 2026-10-28). The lane reads the transcripts (`mac_nux::guest_heard`).
//! - `dns-sd SECS OUT ARG...`: one `dns-sd ARG...` for SECS seconds, under `script -q -t 0` into
//!   OUT, then on until a resolve has answered (`dns_sd_text::answered`, within the one-minute
//!   hang detector), then its process group killed and reaped, as `browse` runs each: the lane's
//!   resolve of an advert the guest heard (`-i <index> -L …`, `-i <index> -G v4v6 <host>`), which
//!   it reads to tell this Mac's own registration from the guest's (`mac_nux::advert`).
//! - `arrivals SEED JOINER FILE...`: for each FILE, `<file> <mtime s|none> <same|differs|absent>`:
//!   `JOINER/FILE`'s modification time in epoch seconds (to the nanosecond the file system keeps)
//!   and whether its bytes are `SEED/FILE`'s, compared here and never printed.
//! - `enrol HOME AUTHORITY`: the vault member under HOME, which holds the Commons control entry its
//!   approval shared with it and is still pending, enrolled as a Commons member with the admin
//!   under AUTHORITY: one control exchange between the two custodies over a loopback connection
//!   in this process, as the admin's Commons runtime and the new Mac's run it over the LAN
//!   (`lab swarm partition`'s plane does the same). It prints `enrolled <epoch ms>`.
//! - `genesis HOME OUT`: the vault the admin under HOME made signs its membership log's Genesis v2
//!   into the new file OUT, seeding the log's first audience epochs (`[everyone]` among them)
//!   wrapped to each Mac its vault and Commons list, as a lab's founder does
//!   (`aterm_secrets::client::seed_vault_genesis`; `lab mesh churn` and `lab commons-lan` found
//!   theirs the same way). `secrets create` mints no `[everyone]` epoch of its own, and a table's
//!   digest is keyed by one. It prints `genesis <epoch ms> members=<n> digest=<8 hex>`.
//! - `repair HOME PEER ROUNDS [AUTHORITY]`: ROUNDS Commons repair rounds (1 to [`MOST_ROUNDS`]),
//!   HOME's custody dialing PEER's over a fresh loopback connection in this process each round,
//!   one `Repair` per side kept across them, as the Commons runtime repairs a peer it heard over
//!   the LAN: control history, then one table a round. Two homes in one guest share its address,
//!   and the runtime drops every beacon from its own addresses, so no runtime of theirs would
//!   ever hear the other: this is how `vault-phrase`'s joiner gets the lab's tables. A HOME still
//!   pending in Commons is first enrolled with AUTHORITY (as `enrol`); without one it is refused.
//!   It prints `repaired <epoch ms> standing=<enrolled|enrolled-now> rounds=<n> control=<records
//!   HOME took> rows=<table operations HOME took> keys=<historical keys HOME took>`, counts only.

#[cfg(unix)]
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// The Commons verbs (`enrol`, `genesis`, `repair`) run on the Unix-only custody and LAN modules.
#[cfg(unix)]
use aterm_db::control::{Endpoint, Repair, Report, Side};
#[cfg(unix)]
use aterm_net::lan::{Connection, Scope};
#[cfg(unix)]
use aterm_secrets::client::Paths;
#[cfg(unix)]
use aterm_secrets::commons::bootstrap::Bootstrap;

/// The hang detector on each wait a step needs to end: a browse's group gone after its kill, a
/// custody's lock, the enrolment's dial.
const HANG: Duration = Duration::from_secs(60);

/// `browse SECS DIR SERVICE...` (the module docs).
///
/// # Errors
/// A missing word, a SECS that is no whole number, or a browse that could not start.
pub(crate) fn browse(args: &[String]) -> Result<(), String> {
    let usage = "browse SECS DIR SERVICE...";
    let [secs, dir, services @ ..] = args else {
        return Err(usage.to_owned());
    };
    let secs: u64 = secs
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| secs.parse().ok())
        .flatten()
        .ok_or_else(|| format!("browse: {secs} is no whole number of seconds"))?;
    if services.is_empty() {
        return Err(usage.to_owned());
    }
    let mut running = Vec::new();
    for service in services {
        let file = transcript(Path::new(dir), service);
        let args = ["-B", service.as_str(), "local."].map(str::to_owned);
        running.push(listen(&file, &args).map_err(|e| format!("browse {service}: {e}"))?);
    }
    std::thread::sleep(Duration::from_secs(secs));
    end(running);
    Ok(())
}

/// `dns-sd SECS OUT ARG...` (the module docs).
///
/// # Errors
/// A missing word, a SECS that is no whole number, or a `dns-sd` that could not start.
pub(crate) fn dns_sd(args: &[String]) -> Result<(), String> {
    let usage = "dns-sd SECS OUT ARG...";
    let [secs, out, rest @ ..] = args else {
        return Err(usage.to_owned());
    };
    let secs: u64 = secs
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| secs.parse().ok())
        .flatten()
        .ok_or_else(|| format!("dns-sd: {secs} is no whole number of seconds"))?;
    if rest.is_empty() {
        return Err(usage.to_owned());
    }
    let child = listen(Path::new(out), rest).map_err(|e| format!("dns-sd: {e}"))?;
    std::thread::sleep(Duration::from_secs(secs));
    // A resolve of an advert that was heard must answer: listen on until it has, within the
    // hang detector (`dns_sd_text`'s module docs).
    let hang = Instant::now() + HANG;
    while Instant::now() < hang
        && !crate::dns_sd_text::answered(
            rest,
            &String::from_utf8_lossy(&read_regular_file(Path::new(out)).unwrap_or_default()),
        )
    {
        std::thread::sleep(ANSWER_POLL);
    }
    end(vec![child]);
    Ok(())
}

/// How often `dns-sd` looks at its transcript for a resolve's answer once its window ended.
const ANSWER_POLL: Duration = Duration::from_millis(100);

/// One `dns-sd ARGS...` under `script -q -t 0` into `file` (removed first), in a session of its
/// own so [`end`] kills its whole group. Both are macOS's commands: off Unix there is no session
/// to start, and the spawn finds no `/usr/bin/script`.
fn listen(file: &Path, args: &[String]) -> std::io::Result<aterm_uds::spawnfd::Child> {
    let _ = std::fs::remove_file(file);
    let mut script = aterm_uds::spawnfd::command("/usr/bin/script");
    script
        .args(["-q", "-t", "0"])
        .arg(file)
        .arg("/usr/bin/dns-sd")
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    #[cfg(unix)]
    script.setsid(true);
    script.spawn()
}

/// Each of `running`'s process groups killed (a Unix session's), and each child reaped.
fn end(running: Vec<aterm_uds::spawnfd::Child>) {
    for mut child in running {
        #[cfg(unix)]
        if let Ok(group) = i32::try_from(child.id()) {
            let _ = aterm_uds::process::kill_group_until_empty(group);
        }
        aterm_uds::deadline::kill_within(&mut child, HANG);
    }
}

/// Where `browse` writes `service`'s transcript under `dir`.
pub(crate) fn transcript(dir: &Path, service: &str) -> PathBuf {
    dir.join(format!("heard{service}.txt"))
}

pub(crate) fn read_regular_file(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if meta.len() > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds 16 MiB limit",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds 16 MiB limit",
        ));
    }
    Ok(bytes)
}

/// `arrivals SEED JOINER FILE...` (the module docs), written to `out`.
///
/// # Errors
/// A missing word, or `out` could not be written.
pub(crate) fn arrivals(args: &[String], out: &mut dyn std::io::Write) -> Result<(), String> {
    let usage = "arrivals SEED JOINER FILE...";
    let [seed, joiner, files @ ..] = args else {
        return Err(usage.to_owned());
    };
    if files.is_empty() {
        return Err(usage.to_owned());
    }
    for file in files {
        let landed = Path::new(joiner).join(file);
        let line = match std::fs::symlink_metadata(&landed)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|m| m.modified().ok())
        {
            Some(at) => {
                let held = match (
                    read_regular_file(&landed),
                    read_regular_file(&Path::new(seed).join(file)),
                ) {
                    (Ok(theirs), Ok(seeds)) if theirs == seeds => "same",
                    _ => "differs",
                };
                format!("{file} {} {held}", epoch_secs(at))
            }
            None => format!("{file} none absent"),
        };
        writeln!(out, "{line}").map_err(|e| format!("arrivals: {e}"))?;
    }
    Ok(())
}

/// `at` as epoch seconds with nine decimals (`0` for a time before the epoch).
fn epoch_secs(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:09}", since.as_secs(), since.subsec_nanos())
}

/// `enrol HOME AUTHORITY` (the module docs), its line written to `out`.
///
/// # Errors
/// A missing or stray word; HOME is no pending Commons member (no control entry yet, or it is
/// enrolled already); AUTHORITY is not enrolled itself; or the exchange left HOME pending.
#[cfg(unix)]
pub(crate) fn enrol(args: &[String], out: &mut dyn std::io::Write) -> Result<(), String> {
    let [home, authority] = args else {
        return Err("enrol HOME AUTHORITY".to_owned());
    };
    let (home, authority) = (Path::new(home), Path::new(authority));
    let newcomer = Paths::for_home(home);
    let pending = Endpoint::pending_from_bootstrap(&bootstrap(&newcomer)?, newcomer.clone())
        .map_err(|e| format!("{}: pending: {e}", home.display()))?;
    let owner = endpoint(authority)?
        .ok_or_else(|| format!("{} is not enrolled itself", authority.display()))?;
    exchange(&pending, &owner)?;
    if endpoint(home)?.is_none() {
        return Err(format!(
            "{} is still pending after the exchange",
            home.display()
        ));
    }
    writeln!(out, "enrolled {}", super::now_ms()).map_err(|e| format!("enrol: {e}"))
}

/// `genesis HOME OUT` (the module docs).
///
/// # Errors
/// A missing or extra word, or the genesis could not be signed or seeded.
#[cfg(unix)]
pub(crate) fn genesis(args: &[String], out: &mut dyn std::io::Write) -> Result<(), String> {
    let [home, file] = args else {
        return Err("genesis HOME OUT".to_owned());
    };
    let signed = aterm_secrets::client::seed_vault_genesis(
        &Paths::for_home(Path::new(home)),
        Path::new(file),
    )
    .map_err(|e| format!("{home}: genesis: {e}"))?;
    let digest = aterm_codec::hex::encode_lower(&signed.digest);
    writeln!(
        out,
        "genesis {} members={} digest={}",
        super::now_ms(),
        signed.members,
        digest.get(..8).unwrap_or_default()
    )
    .map_err(|e| format!("genesis: {e}"))
}

/// The Commons bootstrap under `paths`, its custody's lock within [`HANG`].
#[cfg(unix)]
fn bootstrap(paths: &Paths) -> Result<Bootstrap, String> {
    Bootstrap::required_until(paths, Instant::now() + HANG)
        .map_err(|e| format!("{}: its Commons bootstrap: {e}", paths.home.display()))
}

/// The enrolled endpoint of the custody under `home`, or `None` while it is pending. Its Commons
/// config (`home/commons.toml`) is never written, so sharing reads as on.
#[cfg(unix)]
fn endpoint(home: &Path) -> Result<Option<Endpoint>, String> {
    let paths = Paths::for_home(home);
    let b = bootstrap(&paths)?;
    match Endpoint::from_bootstrap(&b).map_err(|e| format!("{}: {e}", home.display()))? {
        Some(e) => e
            .with_commons(paths, home.join("commons.toml"))
            .map(Some)
            .map_err(|e| format!("{}: {e}", home.display())),
        None => Ok(None),
    }
}

/// One exchange: `initiator` dials `responder` on a loopback port of this process.
#[cfg(unix)]
fn exchange(initiator: &Endpoint, responder: &Endpoint) -> Result<(), String> {
    let (mut mine, mut theirs) = (Repair::default(), Repair::default());
    round(initiator, responder, &mut mine, &mut theirs).map(|_| ())
}

/// The most rounds one `repair` runs: a fresh registry is its catalog and a few batches of 16.
#[cfg(unix)]
pub(crate) const MOST_ROUNDS: usize = 32;

/// What HOME took over a `repair`'s rounds, summed from its side's reports.
#[cfg(unix)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Took {
    /// Control (membership) records acknowledged into HOME's history.
    pub control: usize,
    /// Table operations HOME ingested.
    pub rows: usize,
    /// Historical key handoffs HOME received.
    pub keys: usize,
}

#[cfg(unix)]
impl Took {
    /// One round's reports added in: the control exchange's, and its table round's where it
    /// completed one.
    fn add(&mut self, control: Report, table: Option<aterm_db::replica::Report>) {
        self.control += control.received;
        if let Some(t) = table {
            self.rows += t.received + t.conflicts_received;
            self.keys += t.keys_received;
        }
    }
}

/// `repair HOME PEER ROUNDS [AUTHORITY]`'s words: the two homes, the rounds, the authority.
///
/// # Errors
/// A missing or stray word, or ROUNDS outside 1 to [`MOST_ROUNDS`].
#[cfg(unix)]
fn repair_words(args: &[String]) -> Result<(&Path, &Path, usize, Option<&Path>), String> {
    let usage = || format!("repair HOME PEER ROUNDS [AUTHORITY] (ROUNDS 1 to {MOST_ROUNDS})");
    let (home, peer, rounds, authority) = match args {
        [h, p, r] => (h, p, r, None),
        [h, p, r, a] => (h, p, r, Some(Path::new(a))),
        _ => return Err(usage()),
    };
    let rounds = rounds
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=MOST_ROUNDS).contains(n))
        .ok_or_else(usage)?;
    Ok((Path::new(home), Path::new(peer), rounds, authority))
}

/// `repair HOME PEER ROUNDS [AUTHORITY]` (the module docs), its line written to `out`.
///
/// # Errors
/// Its words ([`repair_words`]); HOME pending with no AUTHORITY, or its enrolment failed; PEER is
/// not enrolled; or a round failed, its table round included (its words, never a value).
#[cfg(unix)]
pub(crate) fn repair(args: &[String], out: &mut dyn std::io::Write) -> Result<(), String> {
    let (home, peer, rounds, authority) = repair_words(args)?;
    let standing = if endpoint(home)?.is_some() {
        "enrolled"
    } else {
        let authority = authority.ok_or_else(|| {
            format!(
                "{} is pending in Commons and no AUTHORITY was named to enrol it",
                home.display()
            )
        })?;
        let words = [home, authority].map(|p| p.to_string_lossy().into_owned());
        enrol(&words, &mut std::io::sink())?;
        "enrolled-now"
    };
    let me = endpoint(home)?
        .ok_or_else(|| format!("{} is still pending after its enrolment", home.display()))?;
    let them =
        endpoint(peer)?.ok_or_else(|| format!("{} is not enrolled itself", peer.display()))?;
    let (mut mine, mut theirs) = (Repair::default(), Repair::default());
    let mut took = Took::default();
    for at in 1..=rounds {
        let control =
            round(&me, &them, &mut mine, &mut theirs).map_err(|e| format!("round {at}: {e}"))?;
        if let Some(e) = mine.application_error().or(theirs.application_error()) {
            return Err(format!("round {at}'s table round: {e}"));
        }
        took.add(control, mine.application());
    }
    writeln!(
        out,
        "repaired {} standing={standing} rounds={rounds} control={} rows={} keys={}",
        super::now_ms(),
        took.control,
        took.rows,
        took.keys
    )
    .map_err(|e| format!("repair: {e}"))
}

/// One round on a fresh loopback connection of this process: `initiator` dials `responder`,
/// each side's `Repair` carried in; the initiator's report.
#[cfg(unix)]
fn round(
    initiator: &Endpoint,
    responder: &Endpoint,
    mine: &mut Repair,
    theirs: &mut Repair,
) -> Result<Report, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let (served, dialed) = std::thread::scope(|s| {
        let server = s.spawn(|| {
            let (stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .map_err(|e| e.to_string())?;
            let conn = Connection::admit(stream, Scope::Loopback).map_err(|e| e.to_string())?;
            responder
                .exchange(conn, Side::Responder, theirs)
                .map_err(|e| e.to_string())
        });
        let dialed = Connection::connect(addr, Scope::Loopback, HANG)
            .map_err(|e| e.to_string())
            .and_then(|conn| {
                initiator
                    .exchange(conn, Side::Initiator, mine)
                    .map_err(|e| e.to_string())
            });
        if dialed.is_err() {
            // Unblock the responder's accept, which no dial will reach.
            let _ = TcpStream::connect(addr);
        }
        let served = server
            .join()
            .unwrap_or_else(|_| Err("the responder panicked".to_owned()));
        (served, dialed)
    });
    served.map_err(|e| format!("the authority's side: {e}"))?;
    dialed.map_err(|e| format!("the newcomer's side: {e}"))
}

#[cfg(test)]
#[path = "steps_tests.rs"]
mod tests;
