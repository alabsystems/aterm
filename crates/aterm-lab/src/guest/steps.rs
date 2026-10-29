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
//!   OUT, then its process group killed and reaped, as `browse` runs each: the lane's resolve of
//!   an advert the guest heard (`-i <index> -L …`, `-i <index> -G v4 <host>`), which it reads to
//!   tell this Mac's own registration from the guest's (`mac_nux::advert`).
//! - `arrivals SEED JOINER FILE...`: for each FILE, `<file> <mtime s|none> <same|differs|absent>`:
//!   `JOINER/FILE`'s modification time in epoch seconds (to the nanosecond the file system keeps)
//!   and whether its bytes are `SEED/FILE`'s, compared here and never printed.
//! - `enrol HOME AUTHORITY`: the vault member under HOME, which holds the Commons control entry its
//!   approval shared with it and is still pending, enrolled as a Commons member with the admin
//!   under AUTHORITY: one control exchange between the two custodies over a loopback connection
//!   in this process, as the admin's Commons runtime and the new Mac's run it over the LAN
//!   (`lab swarm partition`'s plane does the same). It prints `enrolled <epoch ms>`.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_db::control::{Endpoint, Repair, Side};
use aterm_net::lan::{Connection, Scope};
use aterm_secrets::client::Paths;
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
        .parse()
        .map_err(|_| format!("browse: {secs} is no whole number of seconds"))?;
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
        .parse()
        .map_err(|_| format!("dns-sd: {secs} is no whole number of seconds"))?;
    if rest.is_empty() {
        return Err(usage.to_owned());
    }
    let child = listen(Path::new(out), rest).map_err(|e| format!("dns-sd: {e}"))?;
    std::thread::sleep(Duration::from_secs(secs));
    end(vec![child]);
    Ok(())
}

/// One `dns-sd ARGS...` under `script -q -t 0` into `file` (removed first), in a session of its
/// own so [`end`] kills its whole group.
fn listen(file: &Path, args: &[String]) -> std::io::Result<aterm_uds::spawnfd::Child> {
    let _ = std::fs::remove_file(file);
    aterm_uds::spawnfd::command("/usr/bin/script")
        .args(["-q", "-t", "0"])
        .arg(file)
        .arg("/usr/bin/dns-sd")
        .args(args)
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
}

/// Each of `running`'s process groups killed, and each child reaped.
fn end(running: Vec<aterm_uds::spawnfd::Child>) {
    for mut child in running {
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
        let line = match std::fs::metadata(&landed).and_then(|m| m.modified()) {
            Ok(at) => {
                let held = match (
                    std::fs::read(&landed),
                    std::fs::read(Path::new(seed).join(file)),
                ) {
                    (Ok(theirs), Ok(seeds)) if theirs == seeds => "same",
                    _ => "differs",
                };
                format!("{file} {} {held}", epoch_secs(at))
            }
            Err(_) => format!("{file} none absent"),
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

/// The Commons bootstrap under `paths`, its custody's lock within [`HANG`].
fn bootstrap(paths: &Paths) -> Result<Bootstrap, String> {
    Bootstrap::required_until(paths, Instant::now() + HANG)
        .map_err(|e| format!("{}: its Commons bootstrap: {e}", paths.home.display()))
}

/// The enrolled endpoint of the custody under `home`, or `None` while it is pending. Its Commons
/// config (`home/commons.toml`) is never written, so sharing reads as on.
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
fn exchange(initiator: &Endpoint, responder: &Endpoint) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let (mut mine, mut theirs) = (Repair::default(), Repair::default());
    let (served, dialed) = std::thread::scope(|s| {
        let server = s.spawn(|| {
            let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
            let conn = Connection::admit(stream, Scope::Loopback).map_err(|e| e.to_string())?;
            responder
                .exchange(conn, Side::Responder, &mut theirs)
                .map_err(|e| e.to_string())
        });
        let dialed = Connection::connect(addr, Scope::Loopback, HANG)
            .map_err(|e| e.to_string())
            .and_then(|conn| {
                initiator
                    .exchange(conn, Side::Initiator, &mut mine)
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
    dialed.map_err(|e| format!("the newcomer's side: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    /// A scratch directory under the test's temp dir, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "aterm-lab-guest-{tag}-{}.noindex",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("seed")).unwrap();
            std::fs::create_dir_all(dir.join("joiner")).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Each file reads as the joiner holds it: the seed's bytes, other bytes, or none; its time is
    /// the joiner's file's own, and no value reaches the output.
    #[test]
    fn arrivals_say_when_each_file_landed_and_whether_it_is_the_seeds() {
        let s = Scratch::new("arrivals");
        let (seed, joiner) = (s.0.join("seed"), s.0.join("joiner"));
        std::fs::write(seed.join("gh_access_token"), "nux-synthetic-github-1").unwrap();
        std::fs::write(joiner.join("gh_access_token"), "nux-synthetic-github-1").unwrap();
        std::fs::write(seed.join("ai-accounts.csv"), "claude,a@example.invalid\n").unwrap();
        std::fs::write(joiner.join("ai-accounts.csv"), "claude,b@example.invalid\n").unwrap();
        std::fs::write(seed.join("gh_access_token_alabsystems"), "nux-synthetic-p").unwrap();
        let mut out = Vec::new();
        arrivals(
            &args(&[
                seed.to_str().unwrap(),
                joiner.to_str().unwrap(),
                "gh_access_token",
                "gh_access_token_alabsystems",
                "ai-accounts.csv",
            ]),
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<Vec<&str>> = text.lines().map(|l| l.split(' ').collect()).collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!((lines[0][0], lines[0][2]), ("gh_access_token", "same"));
        assert_eq!(
            lines[1],
            ["gh_access_token_alabsystems", "none", "absent"],
            "{text}"
        );
        assert_eq!((lines[2][0], lines[2][2]), ("ai-accounts.csv", "differs"));
        let landed = std::fs::metadata(joiner.join("gh_access_token"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(lines[0][1], epoch_secs(landed));
        assert!(
            !text.contains("synthetic") && !text.contains("example"),
            "no value is printed: {text}"
        );
        assert!(arrivals(&args(&["/s", "/j"]), &mut Vec::new()).is_err());
    }

    #[test]
    fn a_files_time_is_epoch_seconds_to_the_nanosecond() {
        let at = UNIX_EPOCH + Duration::new(1_761_650_000, 5_000_000);
        assert_eq!(epoch_secs(at), "1761650000.005000000");
    }

    /// The enrolment names a home and its authority, and nothing else.
    #[test]
    fn enrol_names_a_home_and_its_authority() {
        assert!(enrol(&args(&["/m"]), &mut Vec::new()).is_err());
        assert!(enrol(&args(&["/m", "/s", "/x"]), &mut Vec::new()).is_err());
    }

    /// The browse's words are read before anything runs, and each transcript has its own file.
    #[test]
    fn a_browse_names_its_seconds_its_folder_and_at_least_one_service() {
        assert!(browse(&args(&["3"])).is_err());
        assert!(browse(&args(&["3", "/n"])).is_err());
        assert!(browse(&args(&["three", "/n", "_alab._tcp"])).is_err());
        assert!(dns_sd(&args(&["2", "/n/out.txt"])).is_err());
        assert!(
            dns_sd(&args(&[
                "two",
                "/n/out.txt",
                "-L",
                "x",
                "_alab._tcp",
                "local."
            ]))
            .is_err()
        );
        assert_eq!(
            transcript(Path::new("/n"), "_alab-vault._tcp"),
            PathBuf::from("/n/heard_alab-vault._tcp.txt")
        );
    }
}
