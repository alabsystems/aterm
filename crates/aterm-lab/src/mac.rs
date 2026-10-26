// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disposable macOS guest VM lane via `tart` ([`IMAGE`], [`ROW`]): `dmg` (the disk-image
//! acceptance), `nux` (the first-run measurements, [`crate::mac_nux`]) and `clean`.
//!
//! A run clones the prepared base of its image ([`base_name`]: the image with SIP turned on,
//! made once by the first run that needs it and kept stopped), boots it with no window and
//! drives it only through `tart exec`, never a connection to the guest's address. tart is
//! Homebrew's, `$LAB_TART`, or [`TART_VERSION`] fetched into `~/Library/Caches/aterm/mac-lane`
//! ([`fetch_tart`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};

use crate::instance::{take_flag, take_value};
use crate::paths::paths;
use crate::{Fail, LAB, Res, stop};

/// Exit status of a lane that could not run: never a pass.
pub const NOT_RUN: i32 = 3;

/// The image a clone is made from: macOS 27 with the Tart Guest Agent (`tart exec`) and no Xcode.
pub const IMAGE: &str = "ghcr.io/cirruslabs/macos-golden-gate-base:latest";

/// Where tart lives when `$LAB_TART` names none (Homebrew's prefixes).
const TART_PATHS: [&str; 2] = ["/opt/homebrew/bin/tart", "/usr/local/bin/tart"];

/// The tart the lane fetches when this Mac has none: Cirrus Labs' release archive, held to
/// [`TART_SHA256`] and signed by [`TART_TEAM`]. Homebrew's `cirruslabs/cli/tart` formula stopped
/// installing on Homebrew 7.0.10 (2026-10-21), and homebrew-core has no tart.
pub const TART_VERSION: &str = "2.40.1";
const TART_SHA256: &str = "363e2701154a8155cbc1bb6d845430c9b42697d2a186bc49574471ca2877db46";
/// Cirrus Labs' Developer ID team, which signs `tart.app`.
const TART_TEAM: &str = "9M2P8L4D89";

/// Each bench: its package, and the name it is shared and run by in the guest.
pub const BENCHES: [(&str, &str); 4] = [
    ("atpkg", "atpkg-dmg"),
    ("aterm-containment", "containment-dmg"),
    ("aterm-uds", "uds-dmg"),
    ("aterm-update", "update-dmg"),
];

/// A boot until the guest agent answers: a hang detector (34 to 54 s measured at load 15 to 40).
pub(crate) const BOOT: Duration = Duration::from_secs(300);
/// One `tart exec` asking a booting guest whether its agent answers: one started before the agent
/// is up neither answers nor fails for about 30 s, so each is cut here and the next starts at once.
const ATTEMPT: Duration = Duration::from_secs(3);
pub(crate) const ROW: Duration = Duration::from_secs(300);
pub(crate) const STEP: Duration = Duration::from_secs(120);
/// How long the control's `~/Desktop` read may take, when tccd shows no dialog for it, before it
/// counts as parked on a consent question: a hang detector ([`desktop_prompted`] ends it sooner).
const CONTROL: Duration = Duration::from_secs(15);
/// The pause between two looks at something the lane waits on.
pub(crate) const POLL: Duration = Duration::from_millis(250);

pub(crate) const GUEST_SHARE: &str = "/Volumes/My Shared Files/lane";
const GUEST_DIR: &str = "/tmp/aterm-mac-lane";

/// The guest agent's executable, the subject tccd names for what `tart exec` runs.
const GUEST_AGENT: &str = "/opt/homebrew/bin/tart-guest-agent";

pub const USAGE: &str = "\
targo --unverified lab mac <verb> — the macOS guest lane: a disposable tart VM (Apple's Virtualization framework)
  dmg [--image REF] [--fda] [--keep]
                the disk-image acceptance in a fresh guest: atpkg's, aterm-containment's, aterm-uds' and
                aterm-update's dmg_acceptance benches, built here with --no-run (nothing is attached on this
                Mac) while the guest boots; a clone of REF's prepared base ($LAB_MAC_IMAGE, else the macOS 27
                base image; tart pulls it first, ~41 GB, ~9 min): REF with SIP turned on (csrutil clear, the
                guest's own administrator) and, --fda, Full Disk Access granted to the guest agent, made once by
                the first run that needs it and kept stopped; booted with no window, each bench run in the guest
                with tccd's log streamed; prints every row, the removable-volume requests tccd saw and a
                ~/Desktop control; the clone deleted (--keep: stopped and kept as aterm-mac-dmg-kept-<pid>; a
                killed run's clone is reported by the next and retained). No tart here: it fetches tart
                2.40.1 (SHA-256 and Developer ID checked) into ~/Library/Caches/aterm/mac-lane, or
                $LAB_TART=/path/to/tart. ~2.5 min with the image and a base here. Exit 0 every row passed, 1 a
                row failed, 3 NOT RUN (a row the guest refused, a guest that did not boot, no tart)
";

/// `mac clean`'s usage.
pub const CLEAN_USAGE: &str = "\
  clean         delete every VM the lane keeps (the prepared bases and the kept clones); the image stays in
                tart's cache (tart delete REF, or tart prune, takes it)
";

/// `mac <verb>`.
///
/// # Errors
/// Usage, or the lane could not run.
pub fn cmd_mac(args: &[String]) -> Res {
    match args.first().map(String::as_str) {
        Some("dmg") => cmd_dmg(&args[1..]),
        Some("nux") => crate::mac_nux::cmd_nux(&args[1..]),
        Some("clean") => cmd_clean(&args[1..]),
        None | Some("help" | "-h" | "--help") => {
            print!("{USAGE}{}{CLEAN_USAGE}", crate::mac_nux::USAGE);
            Ok(0)
        }
        Some(other) => Err(Fail::usage(format!(
            "mac: unknown verb '{other}' ({LAB} mac help)"
        ))),
    }
}

pub(crate) fn not_run(why: impl std::fmt::Display) -> Fail {
    Fail::exit(NOT_RUN, format!("NOT RUN: {why}"))
}

/// Where the `tart` to run comes from.
#[derive(Debug, PartialEq, Eq)]
pub enum TartSource {
    /// This executable.
    At(String),
    /// None here: fetch [`TART_VERSION`] ([`fetch_tart`]).
    Fetch,
}

/// The `tart` to run: `named` (`$LAB_TART`) alone when it is given, else the first of Homebrew's
/// paths that `executable` accepts, else the copy the lane fetched before (`fetched`), else none
/// yet ([`TartSource::Fetch`]).
///
/// # Errors
/// A relative `named` (usage), or a named one that does not run (NOT RUN).
pub fn pick_tart(
    named: Option<&str>,
    fetched: &str,
    executable: impl Fn(&str) -> bool,
) -> Result<TartSource, Fail> {
    match named {
        Some(p) if !p.starts_with('/') => Err(Fail::usage(format!(
            "LAB_TART={p} must be an absolute path"
        ))),
        Some(p) if executable(p) => Ok(TartSource::At(p.to_owned())),
        Some(p) => Err(not_run(format!("LAB_TART={p} is not an executable tart"))),
        None => Ok(TART_PATHS
            .into_iter()
            .chain([fetched])
            .find(|p| executable(p))
            .map_or(TartSource::Fetch, |p| TartSource::At(p.to_owned()))),
    }
}

/// Where the lane keeps the tart it fetches: `<cache>/tart-<version>/tart.app`.
fn fetched_tart(cache: &Path) -> PathBuf {
    cache.join(format!("tart-{TART_VERSION}/tart.app/Contents/MacOS/tart"))
}

/// The team a `codesign -dv` report names (`TeamIdentifier=<team>`), when it names one.
#[must_use]
pub fn signing_team(report: &str) -> Option<&str> {
    report
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set")
}

/// Fetch [`TART_VERSION`] into `cache` and return its executable: the release archive (about
/// 23 MB), held to [`TART_SHA256`], unpacked into a fresh folder beside its home, whose `tart.app`
/// must verify (`codesign --verify --strict`) as [`TART_TEAM`]'s, then renamed into place. Nothing
/// is installed system-wide, and a file curl wrote carries no quarantine.
///
/// # Errors
/// The download, or a digest or signature that does not match (NOT RUN, what it wrote removed).
fn fetch_tart(cache: &Path) -> Result<String, Fail> {
    let fail = |why: String| not_run(format!("fetching tart {TART_VERSION}: {why}"));
    let url =
        format!("https://github.com/cirruslabs/tart/releases/download/{TART_VERSION}/tart.tar.gz");
    println!("mac: no tart here: fetching {url} into {}", cache.display());
    let archive = cache.join(format!("tart-{TART_VERSION}.tar.gz"));
    crate::windows::curl_file(&url, &archive, false).map_err(fail)?;
    let held = (|| {
        let digest = crate::windows::sha256_file(&archive)?;
        if digest != TART_SHA256 {
            return Err(format!(
                "the archive's SHA-256 is {digest}, not {TART_SHA256}"
            ));
        }
        let unpacked = aterm_tempfile::Builder::new()
            .prefix(".tart-")
            .tempdir_in(cache)
            .map_err(|e| format!("{}: {e}", cache.display()))?;
        let tar = crate::windows::run(
            Path::new("/usr/bin/tar"),
            &[
                "-xzf".into(),
                archive.to_string_lossy().into_owned(),
                "-C".into(),
                unpacked.path().to_string_lossy().into_owned(),
            ],
            None,
        );
        if !tar.ok() {
            return Err(format!("tar: {}", tar.tail(3)));
        }
        let app = unpacked
            .path()
            .join("tart.app")
            .to_string_lossy()
            .into_owned();
        let codesign = Path::new("/usr/bin/codesign");
        let verify = crate::windows::run(
            codesign,
            &["--verify".into(), "--strict".into(), app.clone()],
            None,
        );
        if !verify.ok() {
            return Err(format!("tart.app does not verify: {}", verify.tail(3)));
        }
        let report = crate::windows::run(codesign, &["-dv".into(), app], None);
        let team = signing_team(&report.text);
        if team != Some(TART_TEAM) {
            return Err(format!(
                "tart.app is signed by team {}, not {TART_TEAM}",
                team.unwrap_or("none")
            ));
        }
        let home = cache.join(format!("tart-{TART_VERSION}"));
        // A copy cut short before (no executable tart in it, or this run would not fetch) would
        // hold the name for good.
        if home.exists() && !crate::sys::executable(&fetched_tart(cache).to_string_lossy()) {
            let _ = std::fs::remove_dir_all(&home);
        }
        let unpacked = unpacked.keep();
        // A concurrent lane that put its copy first wins; this one is dropped.
        if std::fs::rename(&unpacked, &home).is_err() {
            let _ = std::fs::remove_dir_all(&unpacked);
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&archive);
    held.map_err(fail)?;
    let tart = fetched_tart(cache);
    if crate::sys::executable(&tart.to_string_lossy()) {
        Ok(tart.to_string_lossy().into_owned())
    } else {
        Err(fail(format!("{} is not executable", tart.display())))
    }
}

/// The lane's cache folder: `~/Library/Caches/aterm/mac-lane`.
fn lane_cache() -> Result<PathBuf, Fail> {
    Ok(Path::new(paths().need_home().map_err(Fail::die)?).join("Library/Caches/aterm/mac-lane"))
}

/// The `tart` this run uses ([`pick_tart`]), fetched first when there is none.
///
/// # Errors
/// As [`pick_tart`] and [`fetch_tart`]; no home to fetch into.
pub(crate) fn tart_to_run() -> Result<String, Fail> {
    let cache = lane_cache()?;
    let named = std::env::var("LAB_TART").ok().filter(|p| !p.is_empty());
    match pick_tart(
        named.as_deref(),
        &fetched_tart(&cache).to_string_lossy(),
        crate::sys::executable,
    )? {
        TartSource::At(tart) => Ok(tart),
        TartSource::Fetch => fetch_tart(&cache),
    }
}

/// The benches' executables, by package, from cargo's `--message-format=json` lines.
#[must_use]
pub fn bench_executables(json_lines: &str) -> BTreeMap<String, PathBuf> {
    json_lines
        .lines()
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .filter(|v| {
            v.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact")
        })
        .filter(|v| {
            v.get("target")
                .and_then(|t| t.get("name"))
                .and_then(aterm_json::Value::as_str)
                == Some("dmg_acceptance")
        })
        .filter_map(|v| {
            let exe = v.get("executable").and_then(aterm_json::Value::as_str)?;
            let id = v.get("package_id").and_then(aterm_json::Value::as_str)?;
            let (path, fragment) = id.split_once('#')?;
            let package = match fragment.split_once('@') {
                Some((name, _)) => name,
                None => path.rsplit('/').next()?,
            };
            Some((package.to_owned(), PathBuf::from(exe)))
        })
        .collect()
}

/// A bench's verdict in the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every row it planned passed.
    Pass,
    /// A row failed or the bench ended without its `measure result:` line.
    Fail,
    /// No row failed, but the guest refused one (`COULD NOT RUN`).
    NotRun,
}

const REFUSED_PANIC: &str = "measurement panicked: aterm-gate: COULD NOT RUN";

/// Parse a bench's output into displayed lines and a [`Verdict`].
#[must_use]
pub fn read_report(out: &str) -> (Vec<&str>, Verdict) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Row {
        Pass,
        Fail,
        Refused,
    }
    let mut shown = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut ended = false;
    let (mut detail, mut first) = (false, false);
    for line in out.lines() {
        if let Some(row) = line.strip_prefix("measure row: ") {
            let status = if row.ends_with(" PASS") {
                Row::Pass
            } else if row.ends_with(" COULD NOT RUN") {
                Row::Refused
            } else {
                Row::Fail
            };
            rows.push(status);
            shown.push(line);
            (detail, first) = (status != Row::Pass, status == Row::Fail);
        } else if line.starts_with("measure result: ") {
            ended = true;
            shown.push(line);
            detail = false;
        } else if line.starts_with("measure ") {
            shown.push(line);
        } else if detail {
            if first
                && line.starts_with(REFUSED_PANIC)
                && let Some(last) = rows.last_mut()
            {
                *last = Row::Refused;
            }
            first = false;
            shown.push(line);
        }
    }
    let verdict = if !ended || rows.contains(&Row::Fail) {
        Verdict::Fail
    } else if rows.is_empty() || rows.contains(&Row::Refused) {
        Verdict::NotRun
    } else {
        Verdict::Pass
    };
    (shown, verdict)
}

/// Names that may belong to an ended lane.
#[must_use]
pub fn ended_lane_candidates(names: &str, me: i32, alive: impl Fn(i32) -> bool) -> Vec<String> {
    names
        .lines()
        .map(str::trim)
        .filter(|name| {
            name.strip_prefix(LANE_VM)
                .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|pid| pid.parse::<i32>().ok())
                .is_some_and(|pid| pid != me && !alive(pid))
        })
        .map(str::to_owned)
        .collect()
}

/// The name every lane clone is made under, before its lane's pid.
const LANE_VM: &str = "aterm-mac-dmg-";

/// The name of the prepared base a run clones for `image` (`--fda`: the one with the grant):
/// [`LANE_VM`]`base-`, the image's name with every character but a letter or digit made a `-`,
/// and `-fda`. A base stays until `mac clean`; a later `tart pull` of the image does not refresh it.
#[must_use]
pub fn base_name(image: &str, fda: bool) -> String {
    let slug: String = image
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("{LANE_VM}base-{slug}{}", if fda { "-fda" } else { "" })
}

/// Whether tccd's log shows it putting up the consent dialog for a `~/Desktop` read
/// (`AUTHREQ_PROMPTING: … service=kTCCServiceSystemPolicyDesktopFolder`): the read then waits for
/// good, as nobody answers in the guest.
#[must_use]
pub fn desktop_prompted(log: &str) -> bool {
    log.lines().any(|l| {
        event_message(l)
            .split_once("AUTHREQ_PROMPTING:")
            .is_some_and(|(_, rest)| rest.contains("service=kTCCServiceSystemPolicyDesktopFolder"))
    })
}

/// The message of one `log stream --style ndjson` line: its `eventMessage`, never the
/// `formatString` beside it, which macOS 27 writes first and whose `%{public}@` stands where the
/// service is. A line that is no such event reads as itself.
#[must_use]
pub fn event_message(line: &str) -> String {
    aterm_json::from_str::<aterm_json::Value>(line)
        .ok()
        .and_then(|v| {
            v.get("eventMessage")
                .and_then(aterm_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| line.to_owned())
}

/// What tccd's log says, from its `Handling access request to <service>, from Sub:{…}` lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TccSeen {
    pub removable: Vec<String>,
    pub desktop: usize,
}

/// [`TccSeen`] over the log's lines (ndjson, one event per line).
#[must_use]
pub fn tcc_seen(log: &str) -> TccSeen {
    const HANDLING: &str = "Handling access request to kTCCService";
    let mut seen = TccSeen::default();
    for line in log.lines() {
        let line = event_message(line);
        let Some(at) = line.find(HANDLING) else {
            continue;
        };
        let rest = &line[at + HANDLING.len()..];
        if rest.starts_with("SystemPolicyRemovableVolumes") {
            let subject = rest
                .split_once("from Sub:{")
                .and_then(|(_, s)| s.split_once('}'))
                .map_or("?", |(s, _)| s);
            seen.removable.push(subject.replace("\\/", "/"));
        } else if rest.starts_with("SystemPolicyDesktopFolder") {
            seen.desktop += 1;
        }
    }
    seen
}

/// Whether the `~/Desktop` control read confirms TCC enforcement or, with `fda`, the grant.
pub const fn control_verdict(
    fda: bool,
    desktop_read_passed: bool,
    seen: &TccSeen,
) -> Result<&'static str, &'static str> {
    match (fda, desktop_read_passed, seen.desktop > 0) {
        (false, false, true) => Ok(
            "a ~/Desktop read was asked of tccd and waits on its question: file consent is enforced",
        ),
        (true, true, _) => Ok("a ~/Desktop read passed on the Full Disk Access grant"),
        (false, _, false) => Err(
            "tccd saw no request for a ~/Desktop read: file consent is not enforced here, so no count of requests means anything",
        ),
        (false, true, true) => Err("a ~/Desktop read passed without a grant"),
        (true, false, _) => Err(
            "a ~/Desktop read did not pass under the Full Disk Access grant: the grant did not take",
        ),
    }
}

pub(crate) struct Vm {
    pub(crate) tart: String,
    pub(crate) name: String,
    share: PathBuf,
    log: PathBuf,
    run: Option<Child>,
    keep: bool,
    /// Renamed to a prepared base ([`Vm::promote`]): neither deleted nor kept here.
    promoted: bool,
}

impl Vm {
    /// `tart clone <source> <name>` (tart pulls an image first when this Mac has none), booted
    /// later with `share` read-only and its log in `run`.
    pub(crate) fn clone_from(
        tart: &str,
        source: &str,
        run: &Path,
        share: &Path,
        keep: bool,
    ) -> Result<Vm, Fail> {
        let name = format!("{LANE_VM}{}", std::process::id());
        let status = aterm_uds::spawnfd::command(tart)
            .args(["clone", source, &name])
            .stdin(Stdio::null())
            .status()
            .map_err(|e| not_run(format!("tart clone: {e}")))?;
        if !status.success() {
            return Err(not_run(format!("tart clone {source} {name}: {status}")));
        }
        Ok(Vm {
            tart: tart.to_owned(),
            log: run.join("tart-run.log"),
            share: share.to_path_buf(),
            name,
            run: None,
            keep,
            promoted: false,
        })
    }

    /// Boot it with no window, the share read-only, and wait for the guest agent ([`ATTEMPT`] at a
    /// time).
    pub(crate) fn boot(&mut self) -> Result<(), Fail> {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|e| format!("{}: {e}", self.log.display()))?;
        let err = log.try_clone().map_err(|e| format!("tart run log: {e}"))?;
        let child = aterm_uds::spawnfd::command(&self.tart)
            .args(["run", "--no-graphics", "--no-audio", "--no-clipboard"])
            .arg(format!("--dir=lane:{}:ro", self.share.display()))
            .arg(&self.name)
            .setsid(true)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| not_run(format!("tart run: {e}")))?;
        self.run = Some(child);
        let start = Instant::now();
        while start.elapsed() < BOOT {
            stop::check()?;
            match self.exec("true", ATTEMPT) {
                Ok(o) if o.status.success() => return Ok(()),
                // Not running yet, or refused: asked again shortly.
                Ok(_) => stop::sleep(POLL)?,
                // Cut at ATTEMPT: the agent is not up yet.
                Err(_) => {}
            }
        }
        Err(not_run(format!(
            "the guest agent did not answer within {} s of boot (an image without the Tart Guest Agent cannot run the lane; tart run's log: {})",
            BOOT.as_secs(),
            self.log.display()
        )))
    }

    pub(crate) fn exec(
        &self,
        script: &str,
        limit: Duration,
    ) -> Result<std::process::Output, String> {
        aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(&self.tart)
                .args(["exec", &self.name, "/bin/sh", "-c", script]),
            Instant::now() + limit,
        )
        .map_err(|e| format!("tart exec: {e}"))
    }

    pub(crate) fn sh(&self, script: &str, limit: Duration) -> Result<String, Fail> {
        let out = self.exec(script, limit)?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            Ok(text)
        } else {
            Err(Fail::die(format!(
                "guest `{script}`: {}: {text}{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(mut child) = self.run.take() {
            let _ = aterm_uds::spawnfd::command(&self.tart)
                .args(["stop", &self.name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.wait();
        }
    }

    /// Stop it and rename it `base`. `false` when the rename did not take (a concurrent lane's
    /// base already holds the name): it is deleted as usual then.
    fn promote(&mut self, base: &str) -> bool {
        self.stop();
        self.promoted = aterm_uds::spawnfd::command(&self.tart)
            .args(["rename", &self.name, base])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        self.promoted
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        self.stop();
        if self.promoted {
            return;
        }
        if self.keep {
            let kept = format!("{LANE_VM}kept-{}", std::process::id());
            let renamed = aterm_uds::spawnfd::command(&self.tart)
                .args(["rename", &self.name, &kept])
                .stdin(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            let name = if renamed { &kept } else { &self.name };
            println!("mac: kept the stopped VM {name} (tart delete {name})");
        } else {
            let _ = aterm_uds::spawnfd::command(&self.tart)
                .args(["delete", &self.name])
                .stdin(Stdio::null())
                .status();
        }
    }
}

/// The local VMs' names (`tart list --source local --quiet`).
///
/// # Errors
/// tart did not answer.
fn local_vms(tart: &str) -> Result<String, Fail> {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(tart)
            .args(["list", "--source", "local", "--quiet"])
            .stdin(Stdio::null()),
        Instant::now() + STEP,
    )
    .map_err(|e| not_run(format!("tart list: {e}")))?;
    if !out.status.success() {
        return Err(not_run(format!("tart list: {}", out.status)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The prepared base for `image` ([`base_name`]): the one here, else made now from a clone of the
/// image (SIP on and, `fda`, the grant first) and kept stopped. `between` is called between the
/// steps, so a build the caller runs meanwhile that failed ends the lane at once.
///
/// # Errors
/// The clone or boot could not run, a guest step failed, `--fda` on an image with SIP on, or
/// `between` failed.
pub(crate) fn prepared_base(
    tart: &str,
    image: &str,
    fda: bool,
    run: &Path,
    share: &Path,
    between: &mut dyn FnMut() -> Result<(), Fail>,
) -> Result<String, Fail> {
    let base = base_name(image, fda);
    if local_vms(tart)?.lines().any(|n| n.trim() == base) {
        println!("mac: cloning the prepared {base}");
        return Ok(base);
    }
    println!(
        "mac: preparing {base} from {image}, once (tart pulls the image first when this Mac has no copy)"
    );
    let mut prep = Vm::clone_from(tart, image, run, share, false)?;
    prep.boot()?;
    between()?;
    if prep.sh("csrutil status", STEP)?.contains("disabled") {
        if fda {
            prep.sh(&fda_grant(), STEP)?;
            println!("mac: Full Disk Access granted to the guest agent");
        }
        prep.sh(SIP_ON, STEP)?;
    } else if fda {
        return Err(not_run(
            "--fda needs an image with SIP off: the grant is written to the TCC database before SIP is turned on",
        ));
    }
    if !prep.promote(&base) && !local_vms(tart)?.lines().any(|n| n.trim() == base) {
        return Err(not_run(format!(
            "tart rename {} {base} did not take",
            prep.name
        )));
    }
    Ok(base)
}

/// Boot `vm` (a clone of the prepared `base`) and hold it to SIP on: a base whose clone booted
/// with SIP not on is deleted, and the next run makes it again.
///
/// # Errors
/// The boot, the status read, or SIP not on (NOT RUN).
pub(crate) fn boot_sip_on(vm: &mut Vm, base: &str) -> Result<(), Fail> {
    vm.boot()?;
    let sip = vm.sh("csrutil status", STEP)?;
    println!("mac: {} {}", vm.name, sip.trim());
    if sip_on(&sip) {
        return Ok(());
    }
    let _ = aterm_uds::spawnfd::command(&vm.tart)
        .args(["delete", base])
        .stdin(Stdio::null())
        .status();
    Err(not_run(format!(
        "SIP is not on in the guest: {} (its base {base} is deleted; the next run makes it again)",
        sip.trim()
    )))
}

/// The benches' build here, run while the guest gets ready: `--no-run` (nothing is attached on
/// this Mac), in the shim's cache (`crate::util::targo`), its artifact lines into a file.
struct Build {
    /// The build's process group (it leads its own).
    pgid: u32,
    /// The thread that reaps it: how it ended, and how long after the start.
    waiter: Option<std::thread::JoinHandle<Result<(std::process::ExitStatus, Duration), String>>>,
    ended: Option<Result<(std::process::ExitStatus, Duration), String>>,
    json: PathBuf,
}

impl Build {
    fn start(run: &Path) -> Result<Build, Fail> {
        let json = run.join("build.json");
        let out = std::fs::File::create(&json).map_err(|e| format!("{}: {e}", json.display()))?;
        let mut args = vec!["--unverified", "bench", "--quiet", "--profile", "test"];
        args.extend(BENCHES.iter().flat_map(|(package, _)| ["-p", *package]));
        args.extend([
            "--bench",
            "dmg_acceptance",
            "--no-run",
            "--message-format=json-render-diagnostics",
        ]);
        let started = Instant::now();
        // Its own process group: a lane that ends early stops the whole build ([`Drop`]).
        let mut child = crate::util::targo(&args)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Fail::die(format!("targo: {e}")))?;
        let pgid = child.id();
        let waiter = std::thread::spawn(move || {
            child
                .wait()
                .map(|status| (status, started.elapsed()))
                .map_err(|e| format!("targo: {e}"))
        });
        Ok(Build {
            pgid,
            waiter: Some(waiter),
            ended: None,
            json,
        })
    }

    /// Whether it has ended (reaped by its thread).
    fn ended(&mut self) -> bool {
        if self.ended.is_none() && self.waiter.as_ref().is_some_and(|w| w.is_finished()) {
            self.ended = self.waiter.take().map(|w| {
                w.join()
                    .unwrap_or_else(|_| Err("the build's reaper panicked".into()))
            });
        }
        self.ended.is_some()
    }

    /// A build that failed already ends the lane now.
    fn check(&mut self) -> Result<(), Fail> {
        if !self.ended() {
            return Ok(());
        }
        match &self.ended {
            Some(Ok((status, _))) if !status.success() => {
                Err(Fail::die(format!("the benches did not build: {status}")))
            }
            Some(Err(why)) => Err(Fail::die(format!("the benches did not build: {why}"))),
            _ => Ok(()),
        }
    }

    /// Wait for it: the benches' executables by package.
    fn finish(&mut self) -> Result<BTreeMap<String, PathBuf>, Fail> {
        let waited = Instant::now();
        while !self.ended() {
            stop::sleep(POLL)?;
        }
        self.check()?;
        let built = match &self.ended {
            Some(Ok((_, at))) => *at,
            _ => Duration::ZERO,
        };
        println!(
            "mac dmg: the benches built here in {:.1} s, {:.1} s of it after the guest was ready",
            built.as_secs_f64(),
            waited.elapsed().as_secs_f64()
        );
        let lines = std::fs::read_to_string(&self.json)
            .map_err(|e| format!("{}: {e}", self.json.display()))?;
        Ok(bench_executables(&lines))
    }
}

impl Drop for Build {
    fn drop(&mut self) {
        if !self.ended() {
            if let Ok(pgid) = i32::try_from(self.pgid) {
                let _ = crate::sys::kill_group(pgid, libc::SIGTERM);
            }
            if let Some(waiter) = self.waiter.take() {
                let _ = waiter.join();
            }
        }
    }
}

/// Report possible leftovers without mutating VMs this run did not create.
pub(crate) fn report_ended_lanes(tart: &str) {
    let Ok(names) = local_vms(tart) else {
        return;
    };
    let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    for name in ended_lane_candidates(&names, me, crate::sys::alive) {
        println!("mac: retained {name}; its lane pid is absent, but its ownership is unconfirmed");
    }
}

fn sip_on(status: &str) -> bool {
    status.contains("status: enabled") && !status.contains("disabled")
}

pub(crate) fn reap_within(child: &mut Child, limit: Duration) {
    let start = Instant::now();
    while start.elapsed() < limit {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

const SIP_ON: &str = "/usr/bin/expect -c 'set timeout 30; spawn sudo -n csrutil clear; \
                      expect \"user:\"; send \"admin\\r\"; expect \"assword\"; send \"admin\\r\"; expect eof'";

/// The control's `~/Desktop` read in `vm`, tccd's log streamed to `tccd_log`: whether it passed. It ends at the read's own end, once
/// tccd shows the read's consent dialog ([`desktop_prompted`]: it then waits for good), or at
/// [`CONTROL`].
///
/// # Errors
/// The read could not start, or the lane was stopped.
pub(crate) fn desktop_read(vm: &Vm, tccd_log: &str) -> Result<bool, Fail> {
    let mut read = aterm_uds::spawnfd::command(&vm.tart)
        .args(["exec", &vm.name, "/bin/sh", "-c", "ls ~/Desktop >/dev/null"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Fail::die(format!("tart exec: {e}")))?;
    let start = Instant::now();
    let ended = loop {
        if let Ok(Some(status)) = read.try_wait() {
            break Some(status);
        }
        let prompted = vm
            .exec(
                &format!("grep -h AUTHREQ_PROMPTING {tccd_log} || true"),
                STEP,
            )
            .is_ok_and(|o| desktop_prompted(&String::from_utf8_lossy(&o.stdout)));
        if prompted || start.elapsed() >= CONTROL {
            break read.try_wait().ok().flatten();
        }
        stop::sleep(POLL)?;
    };
    if ended.is_none() {
        let _ = read.kill();
        let _ = read.wait();
    }
    Ok(ended.is_some_and(|status| status.success()))
}

fn fda_grant() -> String {
    format!(
        "set -e; a=$(readlink -f {GUEST_AGENT}); \
         codesign -d -r- \"$a\" 2>&1 | sed -n 's/^designated => //p' | csreq -r- -b /tmp/agent.csreq; \
         hex=$(xxd -p /tmp/agent.csreq | tr -d '\\n'); \
         sudo -n sqlite3 '/Library/Application Support/com.apple.TCC/TCC.db' \
         \"INSERT OR REPLACE INTO access (service,client,client_type,auth_value,auth_reason,auth_version,csreq,flags) \
         VALUES ('kTCCServiceSystemPolicyAllFiles','$a',1,2,4,1,X'$hex',0);\""
    )
}

/// `mac dmg`: the `dmg_acceptance` benches, built here and run in a fresh guest with SIP on and
/// tccd's log streamed, then a `~/Desktop` read as the control.
///
/// # Errors
/// Usage, or the lane could not run.
pub fn cmd_dmg(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let image = take_value(&mut a, "--image")?
        .or_else(|| {
            std::env::var("LAB_MAC_IMAGE")
                .ok()
                .filter(|i| !i.is_empty())
        })
        .unwrap_or_else(|| IMAGE.to_owned());
    let fda = take_flag(&mut a, "--fda");
    let keep = take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "mac dmg: unexpected '{extra}' ({LAB} mac help)"
        )));
    }
    let tart = tart_to_run()?;
    report_ended_lanes(&tart);
    let runs = paths().state.join("mac-dmg");
    std::fs::create_dir_all(&runs).map_err(|e| format!("{}: {e}", runs.display()))?;
    let run_guard = aterm_tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&runs)
        .map_err(|e| format!("{}: {e}", runs.display()))?;
    let run = run_guard.path();
    // The share holds only what the guest runs.
    let share = run.join("share");
    std::fs::create_dir(&share).map_err(|e| format!("{}: {e}", share.display()))?;

    // 1. The benches, built here and never run here, while the guest gets ready.
    let mut build = Build::start(run)?;

    // 2. The guest: a clone of the prepared base (made first when this Mac has none).
    let base = prepared_base(&tart, &image, fda, run, &share, &mut || build.check())?;
    let mut vm = Vm::clone_from(&tart, &base, run, &share, keep)?;
    boot_sip_on(&mut vm, &base)?;

    // 3. The benches into the share (out of `deps/`: a binary run from a huge directory outruns
    // WindowServer's TCC preflight watchdog), then into the guest.
    let exes = build.finish()?;
    for (package, name) in BENCHES {
        let exe = exes.get(package).ok_or_else(|| {
            Fail::die(format!(
                "the build named no dmg_acceptance bench of {package}"
            ))
        })?;
        std::fs::copy(exe, share.join(name)).map_err(|e| format!("{}: {e}", exe.display()))?;
    }
    vm.sh(
        &format!(
            "rm -rf {GUEST_DIR} && mkdir -p {GUEST_DIR} && cp '{GUEST_SHARE}'/*-dmg {GUEST_DIR}/"
        ),
        STEP,
    )?;
    let mut stream = aterm_uds::spawnfd::command(&tart)
        .args(["exec", &vm.name, "sudo", "-n", "/bin/sh", "-c"])
        .arg(format!(
            "exec /usr/bin/log stream --info --style ndjson --predicate 'process == \"tccd\"' > {GUEST_DIR}/tccd.ndjson 2>&1"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Fail::die(format!("tccd's log: {e}")))?;
    // From its first line on (what it filters, about 1.6 s in), so no request goes unseen.
    let started = Instant::now();
    while !vm
        .exec(&format!("test -s {GUEST_DIR}/tccd.ndjson"), STEP)
        .is_ok_and(|o| o.status.success())
    {
        if started.elapsed() >= STEP {
            return Err(Fail::die(format!(
                "tccd's log stream wrote nothing within {} s",
                STEP.as_secs()
            )));
        }
        stop::sleep(POLL)?;
    }
    let (mut failed, mut refused) = (0, 0);
    for (_, name) in BENCHES {
        stop::check()?;
        let out = vm.exec(&format!("cd {GUEST_DIR} && ./{name}"), ROW);
        let text = match &out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(why) => why.clone(),
        };
        let (shown, verdict) = read_report(&text);
        for line in shown {
            println!("mac dmg: {line}");
        }
        match verdict {
            Verdict::Pass => {}
            Verdict::NotRun => refused += 1,
            Verdict::Fail => {
                failed += 1;
                if !text.lines().any(|l| l.starts_with("measure result: ")) {
                    println!("mac dmg: {name} did not run to its end: {}", tail(&text));
                }
            }
        }
    }

    let desktop_read_passed = desktop_read(&vm, &format!("{GUEST_DIR}/tccd.ndjson"))?;
    let _ = vm.exec("sudo -n pkill -INT -f '^/usr/bin/log stream'", STEP);
    reap_within(&mut stream, STEP);
    let log = vm.sh(
        &format!("grep -h 'Handling access request to kTCCServiceSystemPolicy' {GUEST_DIR}/tccd.ndjson || true"),
        STEP,
    )?;
    let seen = tcc_seen(&log);
    println!(
        "mac dmg: tccd: {} SystemPolicyRemovableVolumes request(s){}",
        seen.removable.len(),
        seen.removable
            .iter()
            .map(|s| format!("\n  from {s}"))
            .collect::<String>()
    );
    let counted = match control_verdict(fda, desktop_read_passed, &seen) {
        Ok(why) => {
            println!("mac dmg: control: {why}");
            ""
        }
        Err(why) => {
            println!("mac dmg: control FAILED: {why}");
            "; the tccd count proves nothing (control FAILED)"
        }
    };
    drop(vm);
    let benches = BENCHES.len();
    if failed > 0 {
        println!("mac dmg: FAIL ({failed} of {benches} benches){counted}");
        Ok(1)
    } else if refused > 0 {
        println!("mac dmg: NOT RUN ({refused} of {benches} benches refused by the guest){counted}");
        Ok(NOT_RUN)
    } else {
        println!("mac dmg: PASS ({benches} benches){counted}");
        Ok(0)
    }
}

/// `mac clean`: delete the VMs the lane keeps (each prepared base and kept clone), never a run's
/// clone or an ended lane's (reported instead).
///
/// # Errors
/// Usage, or tart did not answer.
pub fn cmd_clean(args: &[String]) -> Res {
    if let Some(extra) = args.first() {
        return Err(Fail::usage(format!(
            "mac clean: unexpected '{extra}' ({LAB} mac help)"
        )));
    }
    let named = std::env::var("LAB_TART").ok().filter(|p| !p.is_empty());
    let TartSource::At(tart) = pick_tart(
        named.as_deref(),
        &fetched_tart(&lane_cache()?).to_string_lossy(),
        crate::sys::executable,
    )?
    else {
        println!("mac clean: no tart here, so no VM of the lane");
        return Ok(0);
    };
    let names = local_vms(&tart)?;
    let mut failed = 0;
    for name in kept_by_lane(&names) {
        let deleted = aterm_uds::spawnfd::command(&tart)
            .args(["delete", name])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if deleted {
            println!("mac clean: deleted {name}");
        } else {
            failed += 1;
            println!("mac clean: could not delete {name}");
        }
    }
    report_ended_lanes(&tart);
    Ok(i32::from(failed > 0))
}

/// The VMs among `names` that the lane keeps on purpose: its prepared bases and kept clones.
#[must_use]
pub fn kept_by_lane(names: &str) -> Vec<&str> {
    names
        .lines()
        .map(str::trim)
        .filter(|n| {
            n.strip_prefix(LANE_VM)
                .is_some_and(|rest| rest.starts_with("base-") || rest.starts_with("kept-"))
        })
        .collect()
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tart_is_the_named_one_alone_else_homebrews_else_the_fetched_one() {
        let none = |_: &str| false;
        let all = |_: &str| true;
        let fetched = "/c/tart-x/tart.app/Contents/MacOS/tart";
        let at = |p: &str| TartSource::At(p.to_owned());
        assert_eq!(
            pick_tart(Some("/x/tart"), fetched, all).unwrap(),
            at("/x/tart")
        );
        assert_eq!(
            pick_tart(Some("rel/tart"), fetched, all).unwrap_err().code,
            2
        );
        // A named tart that does not run is NOT RUN, never another in its place.
        assert_eq!(
            pick_tart(Some("/x/tart"), fetched, none).unwrap_err().code,
            NOT_RUN
        );
        assert_eq!(pick_tart(None, fetched, all).unwrap(), at(TART_PATHS[0]));
        assert_eq!(
            pick_tart(None, fetched, |p: &str| p == TART_PATHS[1]).unwrap(),
            at(TART_PATHS[1])
        );
        // Homebrew's first; then the copy fetched before; then a fetch.
        assert_eq!(
            pick_tart(None, fetched, |p: &str| p == fetched).unwrap(),
            at(fetched)
        );
        assert_eq!(pick_tart(None, fetched, none).unwrap(), TartSource::Fetch);
    }

    #[test]
    fn only_a_named_team_is_a_signing_team() {
        let report = "Executable=/t/tart.app/Contents/MacOS/tart\n\
                      Identifier=com.github.cirruslabs.tart\n\
                      TeamIdentifier=9M2P8L4D89\n";
        assert_eq!(signing_team(report), Some(TART_TEAM));
        // Ad hoc: codesign says so instead of a team.
        assert_eq!(
            signing_team("Signature=adhoc\nTeamIdentifier=not set\n"),
            None
        );
        assert_eq!(signing_team("Identifier=x\n"), None);
    }

    #[test]
    fn a_base_is_named_by_its_image_and_grant_and_is_never_a_lanes_clone() {
        let plain = base_name(IMAGE, false);
        let fda = base_name(IMAGE, true);
        assert_eq!(
            plain,
            "aterm-mac-dmg-base-ghcr-io-cirruslabs-macos-golden-gate-base-latest"
        );
        assert_eq!(fda, format!("{plain}-fda"));
        assert_ne!(
            base_name("ghcr.io/x:1", false),
            base_name("ghcr.io/x:2", false)
        );
        // A base is kept by the lane on purpose, never taken for an ended lane's clone.
        let names = format!("{plain}\n{fda}\naterm-mac-dmg-kept-7\naterm-mac-dmg-8\nother\n");
        assert_eq!(
            kept_by_lane(&names),
            [plain.as_str(), fda.as_str(), "aterm-mac-dmg-kept-7"]
        );
        assert_eq!(
            ended_lane_candidates(&names, 1, |_| false),
            ["aterm-mac-dmg-8"]
        );
    }

    #[test]
    fn only_tccds_dialog_for_a_desktop_read_ends_the_controls_wait() {
        // tccd's lines for a `~/Desktop` read with no grant, 2026-10-21 (macOS 27.0 guest).
        let prompting = [
            r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyDesktopFolder, from Sub:{\/opt\/homebrew\/Cellar\/tart-guest-agent\/0.15.0\/bin\/tart-guest-agent}Resp:{…}"}"#,
            r#"{"eventMessage":"AUTHREQ_PROMPTING: msgID=158.27, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{\/opt\/homebrew\/Cellar\/tart-guest-agent\/0.15.0\/bin\/tart-guest-agent}Resp:{…}"}"#,
        ]
        .join("\n");
        assert!(desktop_prompted(&prompting));
        // A request alone, or a dialog for another service, is no dialog for the read.
        let asked = prompting.lines().next().unwrap();
        assert!(!desktop_prompted(asked));
        assert!(!desktop_prompted(
            r#"{"eventMessage":"AUTHREQ_PROMPTING: msgID=9.1, service=kTCCServiceSystemPolicyDocumentsFolder"}"#
        ));
        assert!(!desktop_prompted(
            r#"{"eventMessage":"AUTHREQ_CTX: msgID=158.27, function=TCCAccessRequest, service=kTCCServiceSystemPolicyDesktopFolder"}"#
        ));
        assert!(!desktop_prompted(""));
    }

    #[test]
    fn a_tccd_event_is_read_by_its_message_never_its_format_string() {
        // A line as a macOS 27.0 guest's `log stream --style ndjson` writes it (2026-10-25), cut.
        let line = r#"{"timezoneName":"","formatString":"AUTHREQ_PROMPTING: msgID=%{public}@, service=%{public}@, subject=%{public}@, ","eventMessage":"AUTHREQ_PROMPTING: msgID=158.53, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{\/opt\/homebrew\/bin\/tart-guest-agent}Resp:{…}","processID":95}"#;
        assert_eq!(
            event_message(line),
            "AUTHREQ_PROMPTING: msgID=158.53, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{/opt/homebrew/bin/tart-guest-agent}Resp:{…}"
        );
        // The format string alone names no service, so it is no dialog for the read.
        assert!(!desktop_prompted(
            r#"{"formatString":"AUTHREQ_PROMPTING: msgID=%{public}@, service=kTCCServiceSystemPolicyDesktopFolder","eventMessage":"AUTHREQ_CTX: msgID=1.1"}"#
        ));
        assert!(desktop_prompted(line));
        assert_eq!(event_message("not json"), "not json");
    }

    #[test]
    fn only_the_dmg_acceptance_executables_are_taken_by_package() {
        let lines = [
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/atpkg#0.107.0","target":{"name":"dmg_acceptance"},"executable":"/b/deps/dmg_acceptance-1"}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/atpkg#0.107.0","target":{"name":"atpkg"},"executable":"/b/deps/atpkg-9"}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/aterm-uds#0.107.0","target":{"name":"dmg_acceptance"},"executable":null}"#,
            r#"{"reason":"build-finished","success":true}"#,
            "not json",
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/aterm-update#0.107.0","target":{"name":"dmg_acceptance"},"executable":"/b/deps/dmg_acceptance-4"}"#,
        ]
        .join("\n");
        let exes = bench_executables(&lines);
        assert_eq!(
            exes.into_iter().collect::<Vec<_>>(),
            [
                (
                    "aterm-update".to_owned(),
                    PathBuf::from("/b/deps/dmg_acceptance-4")
                ),
                (
                    "atpkg".to_owned(),
                    PathBuf::from("/b/deps/dmg_acceptance-1")
                ),
            ]
        );
    }

    #[test]
    fn a_package_is_named_by_its_id_whatever_its_folder() {
        let line = |id: &str| {
            format!(
                r#"{{"reason":"compiler-artifact","package_id":"{id}","target":{{"name":"dmg_acceptance"}},"executable":"/b/x"}}"#
            )
        };
        let exes = bench_executables(&line("path+file:///w/crates/uds#aterm-uds@0.107.0"));
        assert_eq!(exes.keys().collect::<Vec<_>>(), ["aterm-uds"]);
    }

    #[test]
    fn a_bench_passes_only_on_its_own_complete_report_and_says_why_a_row_did_not() {
        let pass = "measure row: x::a PASS\nall ran\nmeasure row: x::b PASS\nok\n\
                    measure result: x planned=2 passed=2 failed=0 refused=0\n";
        let (shown, verdict) = read_report(pass);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(
            shown,
            [
                "measure row: x::a PASS",
                "measure row: x::b PASS",
                "measure result: x planned=2 passed=2 failed=0 refused=0"
            ]
        );

        let fail = "measure row: x::a FAIL\nmeasurement panicked: left 1 right 2\nat x.rs\n\
                    measure row: x::b PASS\nok\nmeasure result: x planned=2 passed=1 failed=1 refused=0\n";
        let (shown, verdict) = read_report(fail);
        assert_eq!(verdict, Verdict::Fail);
        assert_eq!(
            shown[..3],
            [
                "measure row: x::a FAIL",
                "measurement panicked: left 1 right 2",
                "at x.rs"
            ]
        );

        let typed = "measure row: x::a COULD NOT RUN\nno hdiutil\n\
                     measure result: x planned=1 passed=0 failed=0 refused=1\n";
        assert_eq!(read_report(typed).1, Verdict::NotRun);
        assert_eq!(read_report(typed).0[1], "no hdiutil");
        let panicked = "measure row: x::a FAIL\n\
                        measurement panicked: aterm-gate: COULD NOT RUN — trustd did not answer\n\
                        measure result: x planned=1 passed=0 failed=1 refused=0\n";
        assert_eq!(read_report(panicked).1, Verdict::NotRun);
        let both = format!(
            "{}{fail}",
            typed.replace(
                "measure result: x planned=1 passed=0 failed=0 refused=1\n",
                ""
            )
        );
        assert_eq!(read_report(&both).1, Verdict::Fail);
        let later = "measure row: x::a FAIL\nleft != right\n\
                     measurement panicked: aterm-gate: COULD NOT RUN — quoted\n\
                     measure result: x planned=1 passed=0 failed=1 refused=0\n";
        assert_eq!(read_report(later).1, Verdict::Fail);

        assert_eq!(read_report("measure row: x::a PASS\n").1, Verdict::Fail);
        assert_eq!(read_report("tart exec: timed out").1, Verdict::Fail);
        let empty = "measure result: x planned=0 passed=0 failed=0 refused=0\n";
        assert_eq!(read_report(empty).1, Verdict::NotRun);
    }

    #[test]
    fn ended_lane_candidates_exclude_live_and_kept_names() {
        let names = "aterm-mac-dmg-100\naterm-mac-dmg-200\naterm-mac-dmg-300\n\
                     aterm-mac-dmg-kept-100\naterm-mac-dmg-\naterm-mac-dmg-1x\nother-vm\n";
        let alive = |pid: i32| pid == 200;
        assert_eq!(
            ended_lane_candidates(names, 300, alive),
            ["aterm-mac-dmg-100"]
        );
        assert_eq!(ended_lane_candidates("", 300, alive), Vec::<String>::new());
    }

    #[test]
    fn sip_is_on_only_when_wholly_enabled() {
        assert!(sip_on("System Integrity Protection status: enabled.\n"));
        assert!(!sip_on("System Integrity Protection status: disabled.\n"));
        assert!(!sip_on(
            "System Integrity Protection status: enabled (Custom Configuration).\n\n\
             Configuration:\n\tFilesystem Protections: disabled\n"
        ));
    }

    #[test]
    fn tccd_lines_count_removable_requests_by_subject_and_desktop_requests() {
        let log = [
            r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyRemovableVolumes, from Sub:{com.aterm.aterm}Resp:{TCCDProcess: …}"}"#,
            r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyDesktopFolder, from Sub:{\/opt\/homebrew\/bin\/tart-guest-agent}Resp:{…}"}"#,
            r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyAllFiles, from Sub:{com.aterm.aterm}Resp:{…}"}"#,
            r#"{"eventMessage":"AUTHREQ_CTX: service=kTCCServiceSystemPolicyRemovableVolumes"}"#,
        ]
        .join("\n");
        assert_eq!(
            tcc_seen(&log),
            TccSeen {
                removable: vec!["com.aterm.aterm".to_owned()],
                desktop: 1,
            }
        );
        assert_eq!(tcc_seen(""), TccSeen::default());
    }

    #[test]
    fn a_count_means_something_only_where_the_control_shows_consent_enforced() {
        let asked = TccSeen {
            removable: Vec::new(),
            desktop: 1,
        };
        let silent = TccSeen::default();
        assert!(control_verdict(false, false, &asked).is_ok());
        assert!(control_verdict(true, true, &silent).is_ok());
        assert!(control_verdict(false, true, &silent).is_err());
        assert!(control_verdict(false, false, &silent).is_err());
        assert!(control_verdict(false, true, &asked).is_err());
        assert!(control_verdict(true, false, &asked).is_err());
    }
}
