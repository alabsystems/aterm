// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disposable macOS guest VM lane via `tart` ([`IMAGE`], [`ROW`]): `dmg` (the disk-image
//! acceptance), `nux` (the first-run measurements, [`crate::mac_nux`]), `runtime`
//! (release-product measurements, [`crate::mac_runtime`]) and `clean`.
//!
//! A run clones the prepared base of its image ([`base_name`]: the image with SIP turned on,
//! made once by the first run that needs it and kept stopped), boots it headless except for
//! runtime's requested display, and
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
pub(crate) const GUEST_AGENT: &str = "/opt/homebrew/bin/tart-guest-agent";

/// Where a person base keeps the guest helper (`aterm-lab-guest`), the path its Accessibility and
/// event-post grants name.
pub(crate) const PERSON_HELPER: &str = "/usr/local/libexec/aterm-lab-guest";

/// The code identifier the guest helper is signed with (ad hoc), and the one thing a person base's
/// grants require of it: each run puts its own build at [`PERSON_HELPER`] signed with it
/// ([`person_install`]), so a new build keeps the base. A designated requirement would name the
/// build's cdhash, which every checkout's build changes (its debug info names the build folder).
pub(crate) const PERSON_ID: &str = "aterm-lab-guest";

/// What a prepared base holds besides SIP on ([`prepared_base_with`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Prep {
    /// Full Disk Access granted to the guest agent.
    pub fda: bool,
    /// The Command Line Tools removed and `xcode-select --reset`: a Mac that never had them, so
    /// `xcode-select -p` fails.
    pub no_clt: bool,
    /// The guest helper put at [`PERSON_HELPER`] and granted Accessibility and the event-post
    /// grant by its identifier ([`PERSON_ID`]): its file in the share.
    pub person: Option<String>,
    /// macOS's Local Network answer of Allow, for the image's account, for the ad-hoc aterm nux
    /// installs ([`NUX_APP_EXE`]), written into nehelper's record while SIP is off
    /// ([`lan_allow`]): a Mac whose aterm a person once allowed.
    pub lan_allow: bool,
}

/// The executable nux installs and launches (`/Applications/aterm.app`): what a
/// [`Prep::lan_allow`] base's answer names.
pub(crate) const NUX_APP_EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

/// The share's copy of the rewritten nehelper record ([`lan_allow`]).
#[cfg(any(test, debug_assertions))]
const LAN_ALLOWED_FILE: &str = "nehelper-allowed.plist";

/// The name of the prepared base for `image` and `prep`: [`base_name`], then `-noclt`,
/// `-lanallow` and `-personid` (grants that require [`PERSON_ID`]) as `prep` asks. Not
/// `-person-<…>`: a lab from before identifier grants named its base `-person-<digest>` and
/// deletes every other base under that prefix when it prepares its own.
#[must_use]
pub(crate) fn prep_name(image: &str, prep: &Prep) -> String {
    let mut name = base_name(image, prep.fda);
    if prep.no_clt {
        name.push_str("-noclt");
    }
    if prep.lan_allow {
        name.push_str("-lanallow");
    }
    if prep.person.is_some() {
        name.push_str("-personid");
    }
    name
}

pub const USAGE: &str = "\
targo --unverified lab mac <verb> — the macOS guest lane: a disposable tart VM (Apple's Virtualization framework)
  provision     locate Tart or fetch the pinned version when absent; no app build, image pull, VM clone or boot
  commons-startup --aterm FILE --record NEW_DIR [--image REF]
                the existing guarded Commons startup component in one disposable guest; see `mac commons-startup --help`
  dmg [--image REF] [--fda] [--keep]
                the disk-image acceptance in a fresh guest: atpkg's, aterm-containment's, aterm-uds' and
                aterm-update's dmg_acceptance benches, built here with --no-run (nothing is attached on this
                Mac) while the guest boots; a clone of REF's prepared base ($LAB_MAC_IMAGE, else the macOS 27
                base image; tart pulls it first, ~41 GB, ~9 min): REF with SIP turned on (csrutil clear, the
                guest's own administrator) and, --fda, Full Disk Access granted to the guest agent, made once by
                the first run that needs it and kept stopped; booted with no window, each bench run in the guest
                with tccd's log streamed; prints every row, the removable-volume requests tccd saw and a
                ~/Desktop control; the clone deleted (--keep: stopped and kept as aterm-mac-dmg-kept-<pid>-<instance>; a
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
        Some("provision") => cmd_provision(&args[1..]),
        Some("commons-startup") => crate::mac_commons_startup::cmd(&args[1..]),
        Some("dmg") => cmd_dmg(&args[1..]),
        Some("nux") => crate::mac_nux::cmd_nux(&args[1..]),
        Some("runtime") => crate::mac_runtime::run(&args[1..]),
        Some("count-ranks") => crate::mac_runtime::count_ranks(&args[1..]),
        Some("settle-ranks") => crate::mac_runtime::settle_ranks(&args[1..]),
        Some("wait-ranks") => crate::mac_runtime::wait_ranks(&args[1..]),
        Some("clean") => cmd_clean(&args[1..]),
        None | Some("help" | "-h" | "--help") => {
            print!(
                "{USAGE}{}{}{}{CLEAN_USAGE}",
                crate::mac_nux::USAGE,
                crate::mac_runtime::USAGE,
                crate::mac_runtime::COUNT_USAGE
            );
            Ok(0)
        }
        Some(other) => Err(Fail::usage(format!(
            "mac: unknown verb '{other}' ({LAB} mac help)"
        ))),
    }
}

fn cmd_provision(args: &[String]) -> Res {
    if let Some(extra) = args.first() {
        return Err(Fail::usage(format!(
            "mac provision: unexpected '{extra}' ({LAB} mac help)"
        )));
    }
    let tart = tart_to_run()?;
    println!("mac provision: Tart resolved at {tart}; no VM images pulled or VMs started");
    Ok(0)
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
    crate::util::create_private_dir_all(cache)
        .map_err(|error| fail(format!("{}: {error}", cache.display())))?;
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
            .suffix(".noindex")
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
        .filter(|name| lane_owner(name).is_some_and(|pid| pid != me && !alive(pid)))
        .map(str::to_owned)
        .collect()
}

/// The name every lane clone is made under, before its owner pid and instance number.
const LANE_VM: &str = "aterm-mac-dmg-";

/// One clone's identity. Allocation always reads this process's actual owner pid.
#[derive(Clone, Copy)]
struct VmIdentity {
    owner: u32,
    instance: u64,
}

impl VmIdentity {
    fn allocate() -> Result<Self, Fail> {
        static NEXT: std::sync::Mutex<u64> = std::sync::Mutex::new(1);
        let instance = {
            let mut next = NEXT
                .lock()
                .map_err(|_| Fail::die("the macOS guest instance allocator is poisoned"))?;
            let instance = *next;
            *next = instance
                .checked_add(1)
                .ok_or_else(|| Fail::die("the macOS guest instance identities are exhausted"))?;
            instance
        };
        Ok(Self {
            owner: std::process::id(),
            instance,
        })
    }

    fn running_name(self) -> String {
        format!("{LANE_VM}{}-{}", self.owner, self.instance)
    }

    fn kept_name(self) -> String {
        format!("{LANE_VM}kept-{}-{}", self.owner, self.instance)
    }
}

/// The owner of an old bare-pid clone or a current pid-instance clone. This is only a
/// candidate for reporting; a name never grants custody or permission to remove a VM.
fn lane_owner(name: &str) -> Option<i32> {
    let mut parts = name.strip_prefix(LANE_VM)?.split('-');
    let owner = parts.next()?;
    let positive = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u64>().is_ok_and(|number| number > 0)
    };
    if !positive(owner) {
        return None;
    }
    if let Some(instance) = parts.next()
        && !positive(instance)
    {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }
    owner.parse().ok()
}

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
    identity: VmIdentity,
    share: PathBuf,
    log: PathBuf,
    run: Option<Child>,
    keep: bool,
    /// Renamed to a prepared base ([`Vm::promote`]): neither deleted nor kept here.
    promoted: bool,
    slot: Option<crate::mac_slots::Slot>,
    display_slot: Option<crate::mac_slots::Slot>,
    pub(crate) graphics: bool,
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
        let identity = VmIdentity::allocate()?;
        let name = identity.running_name();
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
            identity,
            run: None,
            keep,
            promoted: false,
            slot: None,
            display_slot: None,
            graphics: false,
        })
    }

    /// Boot it with the selected display, the share read-only, and wait for the guest agent ([`ATTEMPT`] at a
    /// time).
    pub(crate) fn boot(&mut self) -> Result<(), Fail> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let display_slot = if self.graphics {
            Some(crate::mac_slots::Slot::display()?)
        } else {
            None
        };
        let slot = crate::mac_slots::Slot::take()?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.log)
            .and_then(|f| {
                if f.metadata()?.is_file() {
                    Ok(f)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "not a regular file",
                    ))
                }
            })
            .map_err(|e| format!("{}: {e}", self.log.display()))?;
        let err = log.try_clone().map_err(|e| format!("tart run log: {e}"))?;
        let mut command = aterm_uds::spawnfd::command(&self.tart);
        let mut guards = vec![slot.inherited()?];
        if let Some(display) = &display_slot {
            guards.push(display.inherited()?);
        }
        command
            .inherit_guards(guards)
            .map_err(|e| format!("mac guest slot inheritance: {e}"))?;
        command.args(["run", "--no-audio", "--no-clipboard"]);
        if !self.graphics {
            command.arg("--no-graphics");
        }
        let child = command
            .arg(format!("--dir=lane:{}:ro", self.share.display()))
            .arg(&self.name)
            .setsid(true)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| not_run(format!("tart run: {e}")))?;
        self.run = Some(child);
        self.slot = Some(slot);
        self.display_slot = display_slot;
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

    /// Complete stdout up to 16 MiB; oversized or incomplete reads fail instead of retaining
    /// a tail. Stderr keeps the existing bounded-tail policy.
    pub(crate) fn exec_complete(
        &self,
        script: &str,
        limit: Duration,
    ) -> Result<std::process::Output, String> {
        aterm_uds::deadline::output_until_complete_stdout_with_limit(
            aterm_uds::spawnfd::command(&self.tart)
                .args(["exec", &self.name, "/bin/sh", "-c", script]),
            Instant::now() + limit,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|e| format!("tart exec complete: {e}"))
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
            let _ = aterm_uds::deadline::output_until(
                aterm_uds::spawnfd::command(&self.tart)
                    .args(["stop", &self.name])
                    .stdin(Stdio::null()),
                Instant::now() + STEP,
            );
            reap_within(&mut child, STEP);
        }
        self.slot = None;
        self.display_slot = None;
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.run.as_ref().map(Child::id)
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
            let kept = self.identity.kept_name();
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

/// Reuse an already prepared base without booting or changing an image to prepare one.
///
/// # Errors
/// Tart's local image list could not be read.
pub(crate) fn existing_prepared_base(tart: &str, image: &str) -> Result<Option<String>, Fail> {
    let base = prep_name(image, &Prep::default());
    Ok(local_vms(tart)?
        .lines()
        .any(|name| name.trim() == base)
        .then_some(base))
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
    let prep = Prep {
        fda,
        ..Prep::default()
    };
    prepared_base_with(tart, image, &prep, run, share, between)
}

/// [`prepared_base`] for `prep` ([`prep_name`]): the grants and the Local Network answer written
/// while the image's SIP is off, then SIP turned on, then the Command Line Tools removed
/// (`no_clt`, held to `xcode-select -p` failing). A new person base deletes the image's older
/// person bases of the same kind.
///
/// # Errors
/// As [`prepared_base`]; a grant or answer asked of an image with SIP on, or an answer the
/// image's record cannot take (NOT RUN); a CLT-less base whose `xcode-select -p` still answers.
pub(crate) fn prepared_base_with(
    tart: &str,
    image: &str,
    prep: &Prep,
    run: &Path,
    share: &Path,
    between: &mut dyn FnMut() -> Result<(), Fail>,
) -> Result<String, Fail> {
    let fda = prep.fda;
    let base = prep_name(image, prep);
    if local_vms(tart)?.lines().any(|n| n.trim() == base) {
        println!("mac: cloning the prepared {base}");
        return Ok(base);
    }
    println!(
        "mac: preparing {base} from {image}, once (tart pulls the image first when this Mac has no copy)"
    );
    let mut prep_vm = Vm::clone_from(tart, image, run, share, false)?;
    prep_vm.boot()?;
    between()?;
    let vm = &mut prep_vm;
    if vm.sh("csrutil status", STEP)?.contains("disabled") {
        if fda {
            vm.sh(&fda_grant(), STEP)?;
            println!("mac: Full Disk Access granted to the guest agent");
        }
        if let Some(helper) = &prep.person {
            vm.sh(&person_grant(helper), STEP)?;
            println!(
                "mac: the guest helper at {PERSON_HELPER} granted Accessibility and event posting"
            );
        }
        if prep.lan_allow {
            lan_allow(vm, share)?;
            println!("mac: nehelper's record answers Allow for {NUX_APP_EXE} on the local network");
        }
        vm.sh(SIP_ON, STEP)?;
    } else if fda || prep.person.is_some() || prep.lan_allow {
        return Err(not_run(
            "a grant (--fda, the person) or a Local Network answer needs an image with SIP off: it is written to a SIP-protected file before SIP is turned on",
        ));
    }
    if prep.no_clt {
        vm.sh(NO_CLT, STEP)?;
        println!("mac: the Command Line Tools removed (xcode-select -p fails)");
    }
    // What the guest has not written out is lost when `tart stop` outlasts its graceful wait and
    // cuts the power: a base's removal of the Command Line Tools came back in its clones.
    vm.sh("/bin/sync", STEP)?;
    if !vm.promote(&base) && !local_vms(tart)?.lines().any(|n| n.trim() == base) {
        return Err(not_run(format!(
            "tart rename {} {base} did not take",
            vm.name
        )));
    }
    if prep.person.is_some() {
        let kind = prep_name(
            image,
            &Prep {
                person: None,
                ..prep.clone()
            },
        );
        let older = format!("{kind}-person-");
        for name in local_vms(tart)?.lines().map(str::trim) {
            if name.starts_with(&older) && name != base {
                let _ = aterm_uds::spawnfd::command(tart)
                    .args(["delete", name])
                    .stdin(Stdio::null())
                    .status();
                println!("mac: deleted the older person base {name}");
            }
        }
    }
    Ok(base)
}

/// Write the answer an Allow press writes for the ad-hoc aterm at [`NUX_APP_EXE`], for the
/// image's account, into nehelper's record (`aterm_lan::grant::allow`), while SIP is off: the
/// record is read out, rewritten here and staged in the share, and copied over nehelper's with
/// nehelper stopped before and after, so neither its old self nor a relaunch writes its own
/// state over it. The press is a person's and the lab never fakes it; what it measures is what
/// follows the answer. The copy is read back through the reader the app itself uses; whether
/// nehelper keeps it across the boots that follow is the row's to read, in each clone
/// (`lan-allowed` is NOT RUN on a clone whose record no longer allows the app).
///
/// # Errors
/// NOT RUN: an account with no directory UUID, a record the writer refuses (no configuration
/// for the account, no rule to take the shape of), or a copy that does not read as the answer.
#[cfg(any(test, debug_assertions))]
fn lan_allow(vm: &Vm, share: &Path) -> Result<(), Fail> {
    // The account `tart exec` runs as in the image, whose Local Network configuration the app
    // nux opens answers from.
    const IMAGE_ACCOUNT: &str = "admin";
    let plist = aterm_lan::grant::NEHELPER_PLIST;
    let said = vm.sh(
        &format!("dscl . -read /Users/{IMAGE_ACCOUNT} GeneratedUID"),
        STEP,
    )?;
    let user = said
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| not_run(format!("{IMAGE_ACCOUNT}'s GeneratedUID: {said}")))?;
    let code = aterm_lan::grant::Code {
        identifier: "com.aterm.aterm".to_owned(),
        team: None,
        executable: NUX_APP_EXE.to_owned(),
        user: user.to_owned(),
    };
    let record = |when: &str| -> Result<Vec<u8>, Fail> {
        let read = vm.exec(&format!("/bin/cat {plist}"), STEP)?;
        if read.status.success() {
            Ok(read.stdout)
        } else {
            Err(not_run(format!(
                "{plist} {when}: {}",
                String::from_utf8_lossy(&read.stderr)
            )))
        }
    };
    let allowed = aterm_lan::grant::allow(&record("as the image holds it")?, &code)
        .map_err(|why| not_run(format!("the image's Local Network record: {why}")))?;
    let staged = share.join(LAN_ALLOWED_FILE);
    crate::util::write_fresh(&staged, &allowed)
        .map_err(|e| Fail::die(format!("{}: {e}", staged.display())))?;
    vm.sh(
        &format!(
            "set -e; sudo -n killall nehelper || true;              sudo -n cp '{GUEST_SHARE}/{LAN_ALLOWED_FILE}' {plist};              sudo -n chown root:wheel {plist}; sudo -n chmod 644 {plist}; /bin/sync;              sudo -n killall nehelper || true"
        ),
        STEP,
    )?;
    aterm_lan::grant::allowed(&record("after the copy")?, &code).map_err(|why| {
        not_run(format!(
            "the copied record does not read as the answer: {why}"
        ))
    })?;
    Ok(())
}

/// A release lab has no writer of macOS's record (`aterm_lan::grant::allow` is a debug-only
/// seam); `targo --unverified lab` is a debug build.
#[cfg(not(any(test, debug_assertions)))]
fn lan_allow(_: &Vm, _: &Path) -> Result<(), Fail> {
    Err(not_run(
        "a release lab has no writer of macOS's Local Network record: run the debug lab",
    ))
}

/// Remove the Command Line Tools and reset the developer directory, then hold `xcode-select -p`
/// to failing (an image whose Xcode answers it cannot make a CLT-less base).
const NO_CLT: &str = "sudo -n rm -rf /Library/Developer/CommandLineTools && sudo -n xcode-select --reset; \
                      if p=$(xcode-select -p 2>/dev/null); then echo \"xcode-select -p still answers: $p\"; exit 1; fi";

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
        let out = crate::util::create_stdio_file(&json)
            .map_err(|e| format!("{}: {e}", json.display()))?;
        let mut args = vec!["--unverified", "bench", "--quiet", "--profile", "test"];
        args.extend(BENCHES.iter().flat_map(|(package, _)| ["-p", *package]));
        args.extend([
            "--bench",
            "dmg_acceptance",
            "--no-run",
            "--message-format=json-render-diagnostics",
        ]);
        let claim = crate::util::claim_build(&args, None, &crate::paths::paths().repo)
            .map_err(|e| Fail::die(format!("the benches' build: {e}")))?;
        let started = Instant::now();
        // Its own process group: a lane that ends early stops the whole build ([`Drop`]).
        let (mut child, metered) = crate::util::spawn_claimed(
            crate::util::targo(&args)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::from(out))
                .stderr(Stdio::inherit()),
            claim,
        )
        .map_err(|e| Fail::die(format!("targo: {e}")))?;
        let pgid = child.id();
        let waiter = std::thread::spawn(move || {
            crate::util::wait_claimed(&mut child, metered)
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
        let lines = crate::util::read_regular_text(&self.json)
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

pub(crate) fn sip_on(status: &str) -> bool {
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

/// The `sed -n` program that keeps the requirement from `codesign -d -r-`'s `designated => <req>`
/// line, which macOS 27 prints as `# designated => cdhash H"…"` for an implicit one (an ad hoc or
/// linker-signed binary's, as the guest helper and agent are).
const DESIGNATED: &str = "s/^#* *designated => //p";

/// Put the guest helper at `from` (a guest path) at [`PERSON_HELPER`], signed ad hoc with
/// [`PERSON_ID`]: what a person base's grants require of it. The script runs as the image's
/// administrator, whose `sudo` needs no password.
#[must_use]
pub(crate) fn person_install(from: &str) -> String {
    format!(
        "set -e; sudo -n mkdir -p {dir}; sudo -n cp '{from}' {PERSON_HELPER}; sudo -n chmod 755 {PERSON_HELPER}; \
         sudo -n codesign --force --sign - --identifier {PERSON_ID} {PERSON_HELPER}",
        dir = PERSON_HELPER
            .rsplit_once('/')
            .map_or("/usr/local/libexec", |(d, _)| d),
    )
}

/// How long a grant's write waits on the TCC database while another writer holds it (sqlite's
/// busy timeout): tccd writes it in the first minutes after a boot, and a write that met its lock
/// failed at once (`database is locked`, a person base's preparation, 2026-10-31).
const TCC_BUSY_MS: u32 = 30_000;

/// The code requirement a person base's grants name: the helper's identifier alone.
fn person_requirement() -> String {
    format!("identifier \"{PERSON_ID}\"")
}

/// The guest helper copied from the share's `helper` to [`PERSON_HELPER`] ([`person_install`])
/// and granted, by its identifier ([`person_requirement`]), Accessibility (to read a dialog's
/// controls) and event posting (to post the HID events that press them): rows in the TCC
/// database, written while SIP is off.
fn person_grant(helper: &str) -> String {
    format!(
        "{install}; \
         echo '{requirement}' | csreq -r- -b /tmp/person.csreq; \
         hex=$(xxd -p /tmp/person.csreq | tr -d '\\n'); \
         for s in kTCCServiceAccessibility kTCCServicePostEvent; do \
         sudo -n sqlite3 -cmd '.timeout {TCC_BUSY_MS}' '/Library/Application Support/com.apple.TCC/TCC.db' \
         \"INSERT OR REPLACE INTO access (service,client,client_type,auth_value,auth_reason,auth_version,csreq,flags) \
         VALUES ('$s','{PERSON_HELPER}',1,2,4,1,X'$hex',0);\"; done",
        install = person_install(&format!("{GUEST_SHARE}/{helper}")),
        requirement = person_requirement(),
    )
}

fn fda_grant() -> String {
    format!(
        "set -e; a=$(readlink -f {GUEST_AGENT}); \
         codesign -d -r- \"$a\" 2>&1 | sed -n '{DESIGNATED}' | csreq -r- -b /tmp/agent.csreq; \
         hex=$(xxd -p /tmp/agent.csreq | tr -d '\\n'); \
         sudo -n sqlite3 -cmd '.timeout {TCC_BUSY_MS}' '/Library/Application Support/com.apple.TCC/TCC.db' \
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
        .suffix(".noindex")
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
        crate::util::copy_regular_file(exe, share.join(name))
            .map_err(|e| format!("{}: {e}", exe.display()))?;
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
#[path = "mac_tests.rs"]
mod tests;
