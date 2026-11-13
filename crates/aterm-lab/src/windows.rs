// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows runtime lane on this Mac.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_toolchain::named_lock::Wait;
use aterm_uds::spawnfd::Stdio;
use atpkg::admit_disk::Admit;

use crate::linux::{LaneLock, Out};
use crate::paths::paths;
use crate::windows_guest::{self as guest, Arch};
use crate::{Fail, LAB, Res};

#[path = "windows_observe.rs"]
mod observe;

/// The VM's name (QEMU's `-name`).
pub const VM: &str = "aterm-windows";

/// What the VM runs with: the owner's sizing.
pub const CPUS: u32 = 6;
pub const MEMORY_GIB: u32 = 8;
/// The disk is a qcow2 file that grows to what the guest has written.
pub const DISK_GIB: u32 = 128;

/// The qcow2 snapshot a provisioned guest boots from, and `down` returns to.
pub const SNAPSHOT: &str = "provisioned";

/// Exit status of a lane that could not run: never a pass.
pub const NOT_RUN: i32 = 3;

const PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];

fn tool_prefixes(native: Option<&Path>) -> Vec<PathBuf> {
    native
        .into_iter()
        .map(Path::to_path_buf)
        .chain(PREFIXES.map(PathBuf::from))
        .fold(Vec::new(), |mut roots, root| {
            if !roots.contains(&root) {
                roots.push(root);
            }
            roots
        })
}

/// Microsoft's download page for Windows 11 on Arm64.
pub const ISO_PAGE: &str = "https://www.microsoft.com/en-us/software-download/windows11arm64";

/// Language SKU and digest-table row for the ISO the lane installs.
pub const ISO_LANGUAGE: &str = "English";
pub const ISO_DIGEST_ROW: &str = "English 64-bit";

/// Pinned `virtio-win` version and `virtio-win-<version>.iso` SHA-256.
pub const VIRTIO: (&str, &str) = (
    "0.1.302",
    "303f7ae40dad495d6ae474fdc571df58958a4dbc5c37a522d80f9a203867949d",
);

/// Where NetKVM's driver is on the virtio-win ISO, and so on the answer media.
pub const NETKVM_DIR: &str = "NetKVM/w11/ARM64";

const INSTALL_WITHIN: Duration = Duration::from_secs(2 * 3600);
/// How long a boot (to SSH), or a shutdown whose state is kept, is waited for while the guest
/// keeps working ([`Progress`]): a backstop for a guest that works and never answers. A hang is
/// read sooner, at [`STALL_AFTER`]. At 15 minutes a boot on this Mac at a load average of
/// 118-133 (each of the guest's six vCPUs given about a quarter of a core) was given up while it
/// read and wrote its disk the whole time, and its Windows then shut down at the power button
/// (2026-10-30).
const BOOT_WITHIN: Duration = Duration::from_secs(60 * 60);
const STOP_WITHIN: Duration = Duration::from_secs(5 * 60);
const DESKTOP_WITHIN: Duration = Duration::from_secs(guest::DESKTOP_WITHIN_SECS);
/// How long Windows is given to take a shutdown asked for over SSH (`shutdown /s` returns at
/// once): a hang detector, past which ACPI's power button asks instead. A guest paused while
/// the ask's SSH session was open held it over three minutes (2026-10-29).
const ASK_WITHIN: Duration = Duration::from_secs(30);
/// How long a boot or a shutdown may show no progress (no answer on SSH, and no operation on the
/// VM's drives, [`Progress`]) before the guest is read as hung. A Windows that boots reads its
/// drivers and hives and writes its logs, and one that shuts down flushes, even starved: a boot
/// at a load average of 118-133 did about 50 operations a second (2026-10-30). Twice a restart
/// hung in the firmware at "Start boot option", one vCPU at 100 %, for the whole 15-minute hang
/// detector (2026-10-21, `docs/measured/windows-smoke-2026-10-21.md` and
/// `windows-wb2-2026-10-21.md`).
const STALL_AFTER: Duration = Duration::from_secs(3 * 60);
/// How long `ssh` is given to connect and finish its handshake for a command that must run: a
/// one-minute hang detector, not a latency budget. At 10 s a provisioning step failed on a loaded
/// Mac ("Connection timed out during banner exchange", 2026-10-30) nine seconds after the step
/// before it ran.
const CONNECT_WITHIN: Duration = Duration::from_secs(60);
/// The same for a poll that asks again ([`answers`] while a boot is waited for), so a guest whose
/// network is not up yet holds one poll no longer than this.
const POLL_CONNECT_WITHIN: Duration = Duration::from_secs(10);

/// How a wait for the guest gives up: past `within`, or, where `stall` is set, once the guest
/// has shown no progress for that long ([`Progress`]); it looks every `look`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Patience {
    within: Duration,
    stall: Option<Duration>,
    look: Duration,
}

impl Patience {
    /// Windows' install: no stall is read; Setup is given its two hours.
    const INSTALL: Patience = Patience {
        within: INSTALL_WITHIN,
        stall: None,
        look: Duration::from_secs(10),
    };
    /// A boot, to SSH.
    const BOOT: Patience = Patience {
        within: BOOT_WITHIN,
        stall: Some(STALL_AFTER),
        look: Duration::from_secs(10),
    };
    /// A shutdown that `down` or a run's end asks for.
    const STOP: Patience = Patience {
        within: STOP_WITHIN,
        stall: Some(STALL_AFTER),
        look: Duration::from_secs(2),
    };
    /// A shutdown whose state is kept: a restart's, on whose way down Windows may service a
    /// pending install, as long as a restart takes, and the provisioned guest's before its
    /// snapshot, which a stop by force would lose. It has a boot's time while it shows progress.
    const KEEP: Patience = Patience {
        within: BOOT_WITHIN,
        stall: Some(STALL_AFTER),
        look: Duration::from_secs(2),
    };
    /// A guest already read as hung: nothing is asked of it, and its QEMU is ended at once.
    const NOW: Patience = Patience {
        within: Duration::ZERO,
        stall: None,
        look: Duration::from_millis(200),
    };
}

const BOOT_KEYS_FOR: Duration = Duration::from_secs(12);

/// The verbs, as `windows help` prints them.
pub const USAGE: &str = "\
targo --unverified lab windows [--no-wait] [--admit wait|now] <verb> — the Windows runtime lane: a QEMU VM (hvf, Windows
11 arm64, Prism for x86_64). One run holds the VM at a time: a verb that finds it held waits for that run to end, naming it
(pid, verb, checkout, since when) as the wait starts and once a minute; --no-wait answers NOT RUN at once instead, naming
it. A verb that boots the VM first takes its turn to boot, waiting for a run that boots it to end (one run of the VM
claims its disk at a time), then claims the disk its boots write (what the last ones wrote, else 35 GiB less what the
VM's directory holds) and waits for room before it waits for the lane, a line a minute naming the need and the holders;
once it holds the lane, the boot claims the guest's 8 GiB and 6 vCPUs in this Mac's capacity ledger (`aterm pkg capacity`)
and waits while they do not fit with 8 GiB to spare beside them, naming the claims that hold the room once a minute (NOT
RUN at once only on a Mac too small for them); --admit now boots at once, saying what it lacks or would have waited for.
The guest's builds run as many jobs as cores the claim was granted (CARGO_BUILD_JOBS). A VM still running after the
one-shot run that booted it died is stopped as `down` stops it, and booted afresh; one an `up` left is adopted
  up [--vnc]               make the VM on first use (fetch Windows 11's arm64 ISO from Microsoft and virtio-win,
                           install unattended, provision Git, Visual Studio Build Tools and Rust {rust}, snapshot),
                           boot the snapshot; leaves it running. --vnc: a VNC server on 127.0.0.1 (debugging only)
  down                     shut Windows down and put the disk back to its snapshot (the copy and the build go with it)
  status                   QEMU, swtpm, the images, the VM, its snapshot and provisioning, disk use
  exec [--arch aarch64|x86_64] [--desktop] [--] CMD...
                           copy this checkout's working tree into the guest (what changed since its last copy) and run
                           CMD there in C:\\lane\\src with the arch's cargo environment (stock Rust {rust}, MSVC; x86_64:
                           CARGO_BUILD_TARGET=x86_64-pc-windows-msvc, run under Prism) through cmd.exe, stderr merged,
                           as the standard user lane: over SSH (session 0, sshd's Redirection Guard), or with --desktop in
                           lane's desktop session (WinSta0, a Task Scheduler task); a one-shot exec (it booted the VM)
                           shuts it down and reverts the disk when CMD ends. Interrupted, CMD goes on in the guest and a
                           VM the exec booted keeps running until `down` or the next run stops it
  attach --pid N --birth SECONDS:MICROSECONDS --plan HEX exec [exec options] CMD...
                           take exclusive custody of that exact already-kept VM and run exec's copy/command only.
                           Requires matching live kernel generation, booter, recorded and guest plan, runner and
                           active desktop. Never boot, provision, reap leftovers, change display, power off or revert;
                           missing/unknown/mismatched custody refuses without recovery. The repo's current plan is
                           reported separately; a held guest is never migrated by this command.
                           Instead of exec: settings --source SHA40 --gui-sha SHA256 --ctl-sha SHA256
                           --backend cpu|warp --out ABSOLUTE-NEW-DIRECTORY captures three owned Settings cases
                           without building or changing desktop scaling. App source binding requires retained build
                           evidence; supplied executable hashes are checked. Geometry/pixels needing review remain
                           observations, not an automatic declaration that the library layout laws passed.
                           Instead of exec: get C:/absolute/file --out ABSOLUTE-NEW-DIRECTORY retains one regular
                           guest file and before/after SHA-256 receipts without copying source or running a workload.
                           Instead of exec: observe reports bounded process identities and cumulative CPU/I/O facts.
                           Read-only observe takes no lane lock, admission or desktop custody; requires existing SSH
                           host trust and exact held/guest identity before and after, under one minute total.
  smoke [--arch aarch64|x86_64]...
                           the lane's acceptance per arch (both unless named): aterm, aterm-gui and aterm-ctl built,
                           `aterm --version` printing this checkout's build number, and one aterm-gui --headless driven over
                           its control socket (typed, echoed, resized, render=ok, a frame kept) that must draw on the
                           GPU on WARP, Windows' software Direct3D. Exit 0 pass, 1 a row failed, 3 NOT RUN.
                           Report and frames: $LAB_STATE/windows/
  window [--arch aarch64|x86_64]... [--watch] [--cpu]
                           WB.3's runtime smoke per arch (both unless named) in lane's desktop session: a windowed
                           aterm-gui on WARP driven by the VM's USB keyboard and tablet: launched (per-monitor DPI aware),
                           typed into, Microsoft's Japanese IME composing and committing, resized by its frame, display
                           scaling changed under it (100 -> 150 %), moved to the second monitor and back, closed (Alt+F4,
                           exit 0), launched at 150 %. --watch logs the IME row's input as the session saw it (focus,
                           keyboard layout, IME mode, keys held, windows shown). --cpu runs the rows on the CPU presenter
                           (aterm-gui --cpu), each holding the frame aterm presented to the window's pixels as its DC
                           holds them (GetDIBits). Exit 0 pass, 1 a row failed, 3 NOT RUN.
                           Report and screenshots: $LAB_STATE/windows/
  uia [--arch aarch64|x86_64]... [--record]
                           what a UI Automation client sees of a windowed aterm-gui per arch (both unless named), in
                           lane's desktop session through .NET's UIAutomationClient: the window's events while a line
                           is typed, the focus is taken away and given back, a second tab is opened and the first
                           selected through the client, and the find bar searches, then its raw view walked (every
                           element's properties and patterns, each text pattern read), held to the committed record
                           crates/aterm-lab/windows-uia/<arch>.txt; --record rewrites it. Exit 0 pass, 1 a row failed or
                           the record differs, 3 NOT RUN. Report and records: $LAB_STATE/windows/
  destroy                  stop the VM and delete it (disk, firmware variables, TPM, key); the images stay cached
";

/// `args` past the lane's own `--no-wait` and `--admit <wait|now>` (before the verb; one after
/// it is the verb's), whether a held lane is waited for, and how a boot's disk is admitted: `run`,
/// the run's `--admit` ([`crate::admission::mode`]), where the lane names none.
/// # Errors
/// A usage error: `--admit` with no value or another than `wait` or `now`.
fn lane_flags(args: &[String], run: Admit) -> Result<(&[String], Wait, Admit), Fail> {
    let mut args = args;
    let (mut wait, mut admit) = (Wait::Block, run);
    loop {
        if let Some((mode, used)) = crate::linux::admit_flag("windows", args)? {
            admit = mode;
            args = &args[used..];
        } else if let [flag, rest @ ..] = args
            && flag == "--no-wait"
        {
            wait = Wait::No;
            args = rest;
        } else {
            return Ok((args, wait, admit));
        }
    }
}

/// `windows <verb> …`.
/// # Errors
/// A usage error, or a step that failed.
pub fn cmd_windows(args: &[String]) -> Res {
    let (args, wait, admit) = lane_flags(args, crate::admission::mode())?;
    let verb = args.first().map_or("help", String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    crate::linux::name_run(format!("lab windows {verb}"), wait, admit);
    match verb {
        "up" => cmd_up(rest),
        "down" => cmd_down(),
        "status" => cmd_status(),
        "exec" => cmd_exec(rest),
        "attach" => cmd_attach(rest),
        "smoke" => crate::windows_smoke::cmd_smoke(rest),
        "window" => crate::windows_window::cmd_window(rest),
        "uia" => crate::windows_uia::cmd_uia(rest),
        "destroy" => cmd_destroy(),
        "help" | "-h" | "--help" => {
            print!("{}", USAGE.replace("{rust}", guest::RUST));
            Ok(0)
        }
        other => Err(Fail::usage(format!(
            "windows: unknown verb '{other}' ({LAB} windows help)"
        ))),
    }
}

/// A lane that cannot run on this machine: `aterm-lab: NOT RUN: <why>`, exit 3.
#[must_use]
pub fn not_run(why: impl std::fmt::Display) -> Fail {
    Fail::exit(NOT_RUN, format!("NOT RUN: {why}"))
}

/// Where the lane keeps its VM and its downloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The VM: disk, firmware variables, TPM state, key, answer media, run state.
    pub home: PathBuf,
    /// The images, which a new VM is made from again without a download.
    pub cache: PathBuf,
}

impl Layout {
    /// The lane's directories under the user's home `home`.
    #[must_use]
    pub fn under(home: &Path) -> Layout {
        Layout {
            home: home.join(".aterm/lab/windows-lane"),
            cache: home.join("Library/Caches/aterm/windows-lane"),
        }
    }

    /// The lane's directories under this user's home.
    /// # Errors
    /// No home.
    pub fn find() -> Result<Layout, Fail> {
        Ok(Layout::under(Path::new(
            paths().need_home().map_err(Fail::die)?,
        )))
    }

    #[must_use]
    pub fn disk(&self) -> PathBuf {
        self.home.join("disk.qcow2")
    }
    /// The UEFI variable store (a copy of the firmware's template).
    #[must_use]
    pub fn vars(&self) -> PathBuf {
        self.home.join("efivars.fd")
    }
    #[must_use]
    pub fn vars_saved(&self) -> PathBuf {
        self.home.join("efivars.provisioned.fd")
    }
    /// swtpm's state.
    #[must_use]
    pub fn tpm(&self) -> PathBuf {
        self.home.join("tpm")
    }
    #[must_use]
    pub fn tpm_saved(&self) -> PathBuf {
        self.home.join("tpm.provisioned")
    }
    /// The lane's SSH key (`.pub` beside it) and the guest's host key.
    #[must_use]
    pub fn key(&self) -> PathBuf {
        self.home.join("id_ed25519")
    }
    #[must_use]
    pub fn known_hosts(&self) -> PathBuf {
        self.home.join("known_hosts")
    }
    /// The guest account's password (its first logon is automatic; SSH takes the key alone).
    #[must_use]
    pub fn password(&self) -> PathBuf {
        self.home.join("password")
    }
    /// The answer media and the directory it is made from.
    #[must_use]
    pub fn media(&self) -> PathBuf {
        self.home.join("answers.iso")
    }
    #[must_use]
    pub fn media_dir(&self) -> PathBuf {
        self.home.join("answers")
    }
    /// Written once Windows answered on SSH after its install.
    #[must_use]
    pub fn installed(&self) -> PathBuf {
        self.home.join("installed")
    }
    /// The provisioning plan's fingerprint the snapshot holds.
    #[must_use]
    pub fn provisioned(&self) -> PathBuf {
        self.home.join("provisioned")
    }
    /// Present from a boot of the snapshot until the disk, the firmware variables and the TPM
    /// state are reverted to it (or a new snapshot is taken): they may hold writes it does not.
    #[must_use]
    pub fn unreverted(&self) -> PathBuf {
        self.home.join("unreverted")
    }
    /// One run at a time; a run that boots the VM takes its turn to boot beside it first
    /// (`.boot.lock`, [`crate::linux::boot_turn_path`]).
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.home.join(".lane.lock")
    }
    /// A running VM's state: sockets, pid files, its SSH port, its screen.
    #[must_use]
    pub fn run(&self) -> PathBuf {
        self.home.join("run")
    }
    #[must_use]
    pub fn qmp(&self) -> PathBuf {
        self.run().join("qmp.sock")
    }
    #[must_use]
    pub fn tpm_sock(&self) -> PathBuf {
        self.run().join("tpm.sock")
    }
    #[must_use]
    pub fn qemu_pid(&self) -> PathBuf {
        self.run().join("qemu.pid")
    }
    #[must_use]
    pub fn tpm_pid(&self) -> PathBuf {
        self.run().join("swtpm.pid")
    }
    #[must_use]
    pub fn port(&self) -> PathBuf {
        self.run().join("ssh-port")
    }
    /// The VM's screen (ramfb's, the firmware's), taken while the install runs, while a boot or
    /// a shutdown is waited for, and when a wait fails.
    #[must_use]
    pub fn screen(&self) -> PathBuf {
        self.run().join("screen.png")
    }
    /// The virtio-gpu's screen ([`GPU_ID`]), taken with [`Layout::screen`] while a boot or a
    /// shutdown is waited for.
    #[must_use]
    pub fn screen_gpu(&self) -> PathBuf {
        self.run().join("screen-gpu.png")
    }
    /// The run that booted the VM ([`crate::vm_admit::Booter`]).
    #[must_use]
    pub fn booter(&self) -> PathBuf {
        self.run().join("booter")
    }
}

/// QEMU, `qemu-img`, swtpm and the firmware, from the native package prefix or an existing install.
#[derive(Clone, Debug)]
pub struct Tools {
    pub qemu: PathBuf,
    pub qemu_img: PathBuf,
    pub swtpm: PathBuf,
    /// edk2's aarch64 firmware (read-only) and its variable store template.
    pub code: PathBuf,
    pub vars: PathBuf,
    /// QEMU's `tpm-tis-device` has a `ppi` switch.
    pub tpm_ppi: bool,
}

impl Tools {
    /// QEMU and swtpm from the native package prefix, then existing install prefixes.
    /// # Errors
    /// Not Apple silicon, or a tool missing: the lane cannot run here, and the NOT RUN
    /// names what is missing and where it was looked for.
    pub fn find() -> Result<Tools, Fail> {
        if !cfg!(target_arch = "aarch64") {
            return Err(not_run(
                "the lane needs Apple silicon (QEMU's hvf accelerator runs an arm64 Windows)",
            ));
        }
        let native = atpkg::abrew::prefix::Prefix::resolve();
        let enabled = native.root.is_absolute() && !native.is_disabled();
        let prefixes = tool_prefixes(enabled.then_some(native.root.as_path()));
        Tools::find_in(&prefixes.iter().map(PathBuf::as_path).collect::<Vec<_>>())
    }

    /// [`Tools::find`] under `prefixes` (the first that has each tool), on any architecture.
    /// # Errors
    /// As [`Tools::find`], past its architecture check.
    pub fn find_in(prefixes: &[&Path]) -> Result<Tools, Fail> {
        let tools = Self::locate_in(prefixes)?;
        let tpm = run(
            &tools.qemu,
            &strings(&["-device", "tpm-tis-device,help"]),
            None,
        );
        Ok(Tools {
            tpm_ppi: tpm.text.contains("ppi="),
            ..tools
        })
    }

    fn locate_in(prefixes: &[&Path]) -> Result<Tools, Fail> {
        let ok = |p: &Path| crate::sys::executable(&p.to_string_lossy());
        let prefix = prefixes
            .iter()
            .find(|p| ok(&p.join("bin/qemu-system-aarch64")))
            .ok_or_else(|| not_run(missing("qemu-system-aarch64", "QEMU", prefixes)))?;
        let tools = Tools {
            qemu: prefix.join("bin/qemu-system-aarch64"),
            qemu_img: prefix.join("bin/qemu-img"),
            swtpm: prefixes
                .iter()
                .map(|p| p.join("bin/swtpm"))
                .find(|p| ok(p))
                .ok_or_else(|| not_run(missing("swtpm", "swtpm", prefixes)))?,
            code: prefix.join("share/qemu/edk2-aarch64-code.fd"),
            vars: prefix.join("share/qemu/edk2-arm-vars.fd"),
            tpm_ppi: false,
        };
        for f in [&tools.code, &tools.vars] {
            if !f.is_file() {
                return Err(not_run(format!(
                    "QEMU's firmware {} is missing",
                    f.display()
                )));
            }
        }
        if !ok(&tools.qemu_img) {
            return Err(not_run(format!("no {}", tools.qemu_img.display())));
        }
        Ok(tools)
    }
}

fn missing(bin: &str, what: &str, prefixes: &[&Path]) -> String {
    let looked: Vec<String> = prefixes
        .iter()
        .map(|p| p.join("bin").display().to_string())
        .collect();
    format!(
        "no {bin} in {}: the Windows lane runs on your own {what}, and aterm provides none \
         (no check set runs this lane)",
        looked.join(" or ")
    )
}

/// Run `bin ARGS` (in `dir` when given), capturing what it says.
#[must_use]
pub fn run(bin: &Path, args: &[String], dir: Option<&Path>) -> Out {
    let t0 = Instant::now();
    let mut cmd = aterm_uds::spawnfd::command(bin);
    cmd.args(args).stdin(Stdio::null());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output();
    let secs = t0.elapsed().as_secs_f64();
    match out {
        Ok(o) => Out {
            code: o.status.code(),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
            secs,
            signal: None,
            not_started: None,
            lost: None,
        },
        Err(e) => Out {
            code: None,
            text: format!("{} could not start: {e}", bin.display()),
            secs,
            signal: None,
            not_started: None,
            lost: None,
        },
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0";

const SDS_PROFILE: &str = "606624d44113";
const OV_DF_INSTANCE: &str = "560dc9f3-1aa5-4a2f-b63c-9e18f8d0e175";

fn curl_text(url: &str, referer: Option<&str>) -> Result<String, String> {
    let mut args = strings(&["-fsS", "--max-time", "60", "-A", USER_AGENT]);
    if let Some(r) = referer {
        args.extend(["-e".to_string(), r.to_string()]);
    }
    args.push(url.to_string());
    let out = run(Path::new("/usr/bin/curl"), &args, None);
    if out.ok() {
        Ok(out.text)
    } else {
        Err(format!("GET {url}: {}", out.tail(3)))
    }
}

/// Fetch `url` to `dest` (through `dest.part`, renamed when whole); `browser` sends a browser's
/// user agent.
/// # Errors
/// The download failed, or the rename.
pub(crate) fn curl_file(url: &str, dest: &Path, browser: bool) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let part = dest.with_extension("part");
    let mut args = strings(&["-fsSL", "--retry", "2"]);
    if browser {
        args.extend(["-A".to_string(), USER_AGENT.to_string()]);
    }
    args.extend(["-o".to_string(), lossy(&part), url.to_string()]);
    let out = run(Path::new("/usr/bin/curl"), &args, None);
    if !out.ok() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download failed: {}", out.tail(3)));
    }
    std::fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// The SHA-256 of the file at `path`, lowercase hex.
/// # Errors
/// It does not read.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let out = run(
        Path::new("/usr/bin/openssl"),
        &["dgst".into(), "-sha256".into(), "-r".into(), lossy(path)],
        None,
    );
    let hex = out.text.split_whitespace().next().unwrap_or("");
    if out.ok() && aterm_codec::hex::is_lower(hex, 64) {
        Ok(hex.to_string())
    } else {
        Err(format!(
            "{}: openssl dgst failed: {}",
            path.display(),
            out.tail(3)
        ))
    }
}

/// The product edition the page offers: its first `<option value="N">Windows…`.
#[must_use]
pub fn edition_id(page: &str) -> Option<String> {
    page.match_indices("<option value=\"").find_map(|(i, m)| {
        let rest = &page[i + m.len()..];
        let (id, tail) = rest.split_once("\">")?;
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && tail.starts_with("Windows"))
            .then(|| id.to_string())
    })
}

fn page_text(page: &str) -> String {
    let mut text = String::with_capacity(page.len());
    let mut in_tag = false;
    for c in page.chars() {
        match c {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase SHA-256 published in the page's digest table for `row`.
#[must_use]
pub fn published_sha256(page: &str, row: &str) -> Option<String> {
    let text = page_text(page);
    text.match_indices(row).find_map(|(i, m)| {
        let next = text[i + m.len()..].split(' ').find(|w| !w.is_empty())?;
        (next.len() == 64 && next.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| next.to_ascii_lowercase())
    })
}

/// Fingerprint request URL prefix from Microsoft's `mdt.js` without the timestamp suffix.
#[must_use]
pub fn fingerprint_request(mdt_js: &str) -> Option<(String, String)> {
    let start = mdt_js.find("i=\"https://ov-df.microsoft.com/?")? + 3;
    let url = &mdt_js[start..];
    let url = &url[..url.find('"')?];
    let ticks = mdt_js.split("\"&rticks=\"+").nth(1)?;
    let ticks: String = ticks.chars().take_while(char::is_ascii_digit).collect();
    (!ticks.is_empty()).then(|| (url.to_string(), ticks))
}

/// The SKU the download service lists for `language`.
#[must_use]
pub fn sku_id(json: &str, language: &str) -> Option<String> {
    let v = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    v.get("Skus")?.as_array()?.iter().find_map(|s| {
        (s.get("Language")?.as_str()? == language)
            .then(|| s.get("Id")?.as_str().map(str::to_string))?
    })
}

/// The ISO's link the download service answered, or the error it gave.
/// # Errors
/// An answer that is not JSON, the service's own error, or an answer with no link.
pub fn download_link(json: &str) -> Result<String, String> {
    let v =
        aterm_json::from_str::<aterm_json::Value>(json).map_err(|e| format!("not JSON: {e}"))?;
    if let Some(e) = v
        .get("Errors")
        .and_then(aterm_json::Value::as_array)
        .and_then(|a| a.first())
    {
        return Err(e
            .get("Value")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("an error")
            .to_string());
    }
    v.get("ProductDownloadOptions")
        .and_then(aterm_json::Value::as_array)
        .and_then(|a| {
            a.iter()
                .find_map(|o| o.get("Uri")?.as_str().map(str::to_string))
        })
        .ok_or_else(|| "no download link in the answer".to_string())
}

/// The file name a download link names (its path's last segment), when it is a plain `.iso` name.
#[must_use]
pub fn iso_name(link: &str) -> Option<String> {
    let path = link.split('?').next()?;
    let name = path.rsplit('/').next()?;
    (name.ends_with(".iso")
        && name.len() > 4
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
    .then(|| name.to_string())
}

fn uuid4() -> Result<String, String> {
    let mut b = [0u8; 16];
    aterm_uds::rand::fill(&mut b).map_err(|e| format!("entropy: {e}"))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = aterm_codec::hex::encode_lower(&b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

fn iso_record(l: &Layout) -> PathBuf {
    l.cache.join("windows-iso")
}

/// The cached Windows ISO and its digest, when there is one.
#[must_use]
pub fn cached_iso(l: &Layout) -> Option<(PathBuf, String)> {
    let text = crate::util::read_regular_text(&iso_record(l)).ok()?;
    let mut lines = text.lines();
    let (name, sha) = (lines.next()?, lines.next()?);
    let path = l.cache.join(name);
    path.is_file().then(|| (path, sha.to_string()))
}

fn fetch_windows_iso(l: &Layout) -> Result<(PathBuf, String), String> {
    let page = curl_text(ISO_PAGE, None)?;
    let edition = edition_id(&page).ok_or("the download page names no edition")?;
    let want = published_sha256(&page, ISO_DIGEST_ROW)
        .ok_or_else(|| format!("the download page publishes no '{ISO_DIGEST_ROW}' digest"))?;
    let session = uuid4()?;
    let mdt = curl_text(
        &format!(
            "https://ov-df.microsoft.com/mdt.js?instanceId={OV_DF_INSTANCE}&pageId=si&session_id={session}"
        ),
        Some(ISO_PAGE),
    )?;
    let (fp, ticks) = fingerprint_request(&mdt).ok_or("mdt.js named no fingerprint request")?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    curl_text(&format!("{fp}&mdt={now_ms}&rticks={ticks}"), Some(ISO_PAGE))?;
    curl_text(
        &format!("https://vlscppe.microsoft.com/tags?org_id=y6jn8c31&session_id={session}"),
        Some(ISO_PAGE),
    )?;
    let api = "https://www.microsoft.com/software-download-connector/api";
    let skus = curl_text(
        &format!(
            "{api}/getskuinformationbyproductedition?profile={SDS_PROFILE}&ProductEditionId={edition}\
             &SKU=undefined&friendlyFileName=undefined&Locale=en-US&sessionID={session}"
        ),
        Some(ISO_PAGE),
    )?;
    let sku = sku_id(&skus, ISO_LANGUAGE).ok_or_else(|| format!("no {ISO_LANGUAGE} SKU listed"))?;
    let links = curl_text(
        &format!(
            "{api}/GetProductDownloadLinksBySku?profile={SDS_PROFILE}&productEditionId=undefined\
             &SKU={sku}&friendlyFileName=undefined&Locale=en-US&sessionID={session}"
        ),
        Some(ISO_PAGE),
    )?;
    let link = download_link(&links).map_err(|e| format!("Microsoft gave no link: {e}"))?;
    let name = iso_name(&link).ok_or_else(|| format!("the link names no ISO: {link}"))?;
    let dest = l.cache.join(&name);
    let mut have = if dest.is_file() {
        sha256_file(&dest)?
    } else {
        String::new()
    };
    if have != want {
        println!("windows: downloading {name} from Microsoft …");
        curl_file(&link, &dest, true)?;
        have = sha256_file(&dest)?;
    }
    if have != want {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "{name} has SHA-256 {have}, not the {want} Microsoft publishes: removed"
        ));
    }
    crate::util::atomic_write(&iso_record(l), format!("{name}\n{want}\n").as_bytes())
        .map_err(|e| format!("{}: {e}", iso_record(l).display()))?;
    println!("windows: {name} matches Microsoft's published SHA-256 {want}");
    Ok((dest, want))
}

fn virtio_iso(l: &Layout) -> Result<PathBuf, String> {
    let (version, want) = VIRTIO;
    let dest = l.cache.join(format!("virtio-win-{version}.iso"));
    if !dest.is_file() {
        println!("windows: downloading virtio-win {version} …");
        curl_file(
            &format!(
                "https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/\
                 virtio-win-{version}-1/virtio-win-{version}.iso"
            ),
            &dest,
            false,
        )?;
    }
    let have = sha256_file(&dest)?;
    if have != want {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "{} has SHA-256 {have}, not the pinned {want}: removed",
            dest.display()
        ));
    }
    Ok(dest)
}

/// What one boot attaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Boot {
    /// The Windows ISO and the answer media, for the install's boot.
    pub install: Option<(PathBuf, PathBuf)>,
    /// The host port forwarded to the guest's SSH (127.0.0.1 only).
    pub port: u16,
    /// A VNC display on 127.0.0.1 (`up --vnc`), for a person debugging.
    pub vnc: Option<u16>,
}

/// The VM's second display adapter.
pub const VIRTIO_GPU: &str = "virtio-gpu-pci,id=gpu1,xres=1920,yres=1200";
pub const GPU_ID: &str = "gpu1";

/// QEMU's arguments for `boot`.
#[must_use]
pub fn qemu_args(l: &Layout, t: &Tools, boot: &Boot) -> Vec<String> {
    let mut a = strings(&["-name", VM, "-machine", "virt,accel=hvf", "-cpu", "host"]);
    a.extend([
        "-smp".into(),
        CPUS.to_string(),
        "-m".into(),
        format!("{MEMORY_GIB}G"),
        "-drive".into(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            lossy(&t.code)
        ),
        "-drive".into(),
        format!("if=pflash,format=raw,unit=1,file={}", lossy(&l.vars())),
    ]);
    a.extend(strings(&[
        "-device",
        "ramfb",
        "-device",
        VIRTIO_GPU,
        "-device",
        "qemu-xhci,id=xhci",
        "-device",
        "usb-kbd",
        "-device",
        "usb-tablet",
    ]));
    a.extend([
        "-drive".into(),
        format!(
            "if=none,id=disk,format=qcow2,discard=unmap,detect-zeroes=unmap,file={}",
            lossy(&l.disk())
        ),
        "-device".into(),
        "nvme,drive=disk,serial=aterm-lane,bootindex=1".into(),
    ]);
    if let Some((iso, media)) = &boot.install {
        a.extend([
            "-drive".into(),
            format!(
                "if=none,id=install,media=cdrom,readonly=on,file={}",
                lossy(iso)
            ),
            "-device".into(),
            "usb-storage,drive=install,bootindex=0".into(),
            "-drive".into(),
            format!(
                "if=none,id=answers,media=cdrom,readonly=on,file={}",
                lossy(media)
            ),
            "-device".into(),
            "usb-storage,drive=answers".into(),
        ]);
    }
    a.extend([
        "-netdev".into(),
        format!("user,id=net,hostfwd=tcp:127.0.0.1:{}-:22", boot.port),
        "-device".into(),
        "virtio-net-pci,netdev=net".into(),
        "-chardev".into(),
        format!("socket,id=tpmchr,path={}", lossy(&l.tpm_sock())),
        "-tpmdev".into(),
        "emulator,id=tpm0,chardev=tpmchr".into(),
        "-device".into(),
        if t.tpm_ppi {
            "tpm-tis-device,tpmdev=tpm0,ppi=off".into()
        } else {
            "tpm-tis-device,tpmdev=tpm0".into()
        },
        "-qmp".into(),
        format!("unix:{},server=on,wait=off", lossy(&l.qmp())),
        "-display".into(),
        "none".into(),
    ]);
    if let Some(port) = boot.vnc {
        a.extend([
            "-vnc".into(),
            format!("127.0.0.1:{}", port.saturating_sub(5900)),
        ]);
    }
    a.extend(strings(&[
        "-serial",
        "none",
        "-monitor",
        "none",
        "-parallel",
        "none",
        "-daemonize",
        "-pidfile",
    ]));
    a.push(lossy(&l.qemu_pid()));
    a
}

/// Arguments for `swtpm` backing the lane's TPM 2.0 emulator socket for QEMU.
#[must_use]
pub fn swtpm_args(l: &Layout) -> Vec<String> {
    vec![
        "socket".into(),
        "--tpm2".into(),
        "--tpmstate".into(),
        format!("dir={}", lossy(&l.tpm())),
        "--ctrl".into(),
        format!("type=unixio,path={}", lossy(&l.tpm_sock())),
        "--terminate".into(),
        "--daemon".into(),
        "--pid".into(),
        format!("file={}", lossy(&l.tpm_pid())),
        "--log".into(),
        format!("file={},level=1", lossy(&l.run().join("swtpm.log"))),
    ]
}

/// Which daemon belongs to this layout. Argument words bind the pid file to
/// the VM's own resources; a command-name prefix alone does not.
#[derive(Clone, Copy, Debug)]
enum Daemon {
    Qemu,
    Tpm,
}

impl Daemon {
    fn pid_file(self, l: &Layout) -> PathBuf {
        match self {
            Self::Qemu => l.qemu_pid(),
            Self::Tpm => l.tpm_pid(),
        }
    }
}

/// The observed process, never just the reusable number in its pid file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DaemonProcess {
    pid: i32,
    birth: aterm_uds::process::ProcessBirth,
}

fn daemon_words(l: &Layout, kind: Daemon, argv: &[OsString]) -> bool {
    let Some(program) = argv.first().and_then(|a| Path::new(a).file_name()) else {
        return false;
    };
    let named = match kind {
        Daemon::Qemu => program
            .to_str()
            .is_some_and(|n| n.starts_with("qemu-system-")),
        Daemon::Tpm => program == OsStr::new("swtpm"),
    };
    if !named {
        return false;
    }
    let pair = |key: &str, value: &str| {
        argv.windows(2)
            .any(|w| w[0] == OsStr::new(key) && w[1] == OsStr::new(value))
    };
    match kind {
        Daemon::Qemu => {
            pair("-name", VM)
                && pair("-pidfile", &lossy(&l.qemu_pid()))
                && pair(
                    "-qmp",
                    &format!("unix:{},server=on,wait=off", lossy(&l.qmp())),
                )
                && pair(
                    "-drive",
                    &format!(
                        "if=none,id=disk,format=qcow2,discard=unmap,detect-zeroes=unmap,file={}",
                        lossy(&l.disk())
                    ),
                )
        }
        Daemon::Tpm => {
            pair("--pid", &format!("file={}", lossy(&l.tpm_pid())))
                && pair(
                    "--ctrl",
                    &format!("type=unixio,path={}", lossy(&l.tpm_sock())),
                )
                && pair("--tpmstate", &format!("dir={}", lossy(&l.tpm())))
        }
    }
}

fn captured_daemon(
    l: &Layout,
    kind: Daemon,
    pid: i32,
    before: Option<aterm_uds::process::ProcessBirth>,
    argv: &[OsString],
    after: Option<aterm_uds::process::ProcessBirth>,
) -> Option<DaemonProcess> {
    let birth = before?;
    (pid > 1 && after == Some(birth) && daemon_words(l, kind, argv))
        .then_some(DaemonProcess { pid, birth })
}

/// The daemon `pid` runs, read as this layout's ([`captured_daemon`]). `None` where no process
/// holds `pid`, or one that has exited and is not yet reaped: a daemon's parent is launchd,
/// which reaps it when it can, and until then signal zero still finds it while its start and
/// words no longer read. It runs nothing again, so a stop that waits on it is over.
/// # Errors
/// A live `pid` that does not read as this VM's daemon, or one whose presence cannot be read:
/// either is preserved.
fn observe_daemon(l: &Layout, kind: Daemon, pid: i32) -> Result<Option<DaemonProcess>, String> {
    use aterm_uds::process::ProcessPresence;
    let before = aterm_uds::process::read_process_birth(pid);
    let argv = aterm_uds::process::read_process_argv(pid);
    let after = aterm_uds::process::read_process_birth(pid);
    if let Some(found) = argv
        .as_deref()
        .and_then(|a| captured_daemon(l, kind, pid, before, a, after))
    {
        return Ok(Some(found));
    }
    match aterm_uds::process::process_presence(pid) {
        ProcessPresence::Absent | ProcessPresence::Exited => Ok(None),
        ProcessPresence::Present | ProcessPresence::Unknown => Err(format!(
            "{kind:?} pid {pid} has no stable identity and arguments for this VM; preserved"
        )),
    }
}

fn daemon(l: &Layout, kind: Daemon) -> Result<Option<DaemonProcess>, String> {
    let file = kind.pid_file(l);
    let text = match crate::util::read_regular_text(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let pid: i32 = text
        .trim()
        .parse()
        .ok()
        .filter(|&pid| pid > 1)
        .ok_or_else(|| format!("{} records no valid daemon pid; preserved", file.display()))?;
    observe_daemon(l, kind, pid)
}

impl DaemonProcess {
    fn still_here(self, l: &Layout, kind: Daemon) -> Result<bool, String> {
        if let Some(birth) = aterm_uds::process::read_process_birth(self.pid)
            && birth != self.birth
        {
            return Ok(false);
        }
        Ok(observe_daemon(l, kind, self.pid)? == Some(self))
    }

    fn signal(self, l: &Layout, kind: Daemon, sig: i32) -> Result<(), String> {
        if self.still_here(l, kind)? {
            crate::sys::kill(self.pid, sig)
                .map_err(|e| format!("{kind:?} pid {}: {e}", self.pid))?;
        }
        Ok(())
    }
}

/// The running VM's QEMU, if any; unreadable or foreign state is refused.
pub fn running(l: &Layout) -> Result<Option<i32>, String> {
    Ok(daemon(l, Daemon::Qemu)?.map(|p| p.pid))
}

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("no free port: {e}"))?;
    l.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

fn read_port(l: &Layout) -> Option<u16> {
    crate::util::read_regular_text(&l.port())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn start(l: &Layout, t: &Tools, boot: &Boot) -> Result<(), String> {
    if running(l)?.is_some() {
        return Err("this VM already has a running QEMU; preserved".into());
    }
    stop_swtpm(l)?;
    let run_dir = l.run();
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    for f in [
        l.qmp(),
        l.tpm_sock(),
        l.qemu_pid(),
        l.tpm_pid(),
        l.port(),
        l.screen(),
        l.screen_gpu(),
    ] {
        let _ = std::fs::remove_file(f);
    }
    if snapshot_plan(l).is_some() {
        crate::util::atomic_write(&l.unreverted(), b"a boot of the snapshot\n")
            .map_err(|e| format!("{}: {e}", l.unreverted().display()))?;
    }
    std::fs::create_dir_all(l.tpm()).map_err(|e| format!("{}: {e}", l.tpm().display()))?;
    let out = run(&t.swtpm, &swtpm_args(l), None);
    if !out.ok() {
        return Err(format!("swtpm did not start: {}", out.tail(5)));
    }
    let t0 = Instant::now();
    while !l.tpm_sock().exists() {
        if t0.elapsed() > Duration::from_secs(60) {
            stop_swtpm(l)?;
            return Err("swtpm's socket did not appear within a minute".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = run(&t.qemu, &qemu_args(l, t, boot), None);
    if !out.ok() {
        stop_swtpm(l)?;
        return Err(format!("QEMU did not start: {}", out.tail(8)));
    }
    crate::util::atomic_write(&l.port(), format!("{}\n", boot.port).as_bytes())
        .map_err(|e| format!("{}: {e}", l.port().display()))
}

fn stop_swtpm(l: &Layout) -> Result<(), String> {
    let Some(process) = daemon(l, Daemon::Tpm)? else {
        return Ok(());
    };
    process.signal(l, Daemon::Tpm, libc::SIGTERM)?;
    let t0 = Instant::now();
    while process.still_here(l, Daemon::Tpm)? {
        if t0.elapsed() >= Duration::from_secs(60) {
            return Err("swtpm did not end within a minute; its state is preserved".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Send each of `commands` (QMP JSON) to the running VM, the answers in order.
/// # Errors
/// No QMP socket, or an answer that did not come or did not parse.
pub fn qmp(l: &Layout, commands: &[String]) -> Result<Vec<aterm_json::Value>, String> {
    let mut sock = std::os::unix::net::UnixStream::connect(l.qmp())
        .map_err(|e| format!("{}: {e}", l.qmp().display()))?;
    sock.set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(sock.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    (&mut reader)
        .take(1024 * 1024)
        .read_line(&mut line)
        .map_err(|e| format!("QMP greeting: {e}"))?;
    let mut answers = Vec::new();
    let all = std::iter::once("{\"execute\":\"qmp_capabilities\"}".to_string())
        .chain(commands.iter().cloned());
    for (i, c) in all.enumerate() {
        sock.write_all(format!("{c}\n").as_bytes())
            .map_err(|e| format!("QMP: {e}"))?;
        loop {
            line.clear();
            if (&mut reader)
                .take(1024 * 1024)
                .read_line(&mut line)
                .map_err(|e| format!("QMP: {e}"))?
                == 0
            {
                return Err("QMP closed".into());
            }
            let v = aterm_json::from_str::<aterm_json::Value>(line.trim())
                .map_err(|e| format!("QMP said {}: {e}", line.trim()))?;
            if v.get("return").is_some() || v.get("error").is_some() {
                if i > 0 {
                    answers.push(v);
                }
                break;
            }
        }
    }
    Ok(answers)
}

fn screendump(l: &Layout) {
    let _ = qmp(
        l,
        &[crate::windows_desk::screendump_command(
            &lossy(&l.screen()),
            None,
        )],
    );
}

/// Both of the VM's screens written where [`screens_said`] names them ([`Layout::screen`],
/// [`Layout::screen_gpu`]): what the guest shows as a wait says how it goes.
fn screendumps(l: &Layout) {
    let _ = qmp(
        l,
        &[
            crate::windows_desk::screendump_command(&lossy(&l.screen()), None),
            crate::windows_desk::screendump_command(&lossy(&l.screen_gpu()), Some(GPU_ID)),
        ],
    );
}

/// The operations the VM's drives have done since its QEMU started, as QMP's `query-blockstats`
/// counts them: every drive's reads, writes and flushes (the disk's, the firmware variables'),
/// summed. `None` where QMP did not answer.
fn disk_operations(l: &Layout) -> Option<u64> {
    let answers = qmp(l, &["{\"execute\":\"query-blockstats\"}".to_string()]).ok()?;
    let drives = answers.first()?.get("return")?.as_array()?;
    Some(
        drives
            .iter()
            .filter_map(|d| d.get("stats"))
            .flat_map(|s| {
                ["rd_operations", "wr_operations", "flush_operations"]
                    .map(|k| s.get(k).and_then(aterm_json::Value::as_u64).unwrap_or(0))
            })
            .sum(),
    )
}

/// The guest's work as a wait last read it ([`disk_operations`]), and since when it has read so:
/// a guest whose drives do nothing shows no progress. Its screens are no sign of it: at "Start
/// boot option", where both hung restarts stood, Windows' boot spinner turns, and it turned
/// through a boot that read and wrote its disk for 15 minutes (2026-10-30).
#[derive(Debug)]
struct Progress {
    last: Option<u64>,
    since: Instant,
}

impl Progress {
    /// No look taken yet: the guest counts as idle from `now`.
    fn from(now: Instant) -> Progress {
        Progress {
            last: None,
            since: now,
        }
    }

    /// The guest's drives had done `ops` operations at `now` (`None`: QMP did not say, read as
    /// no change): how long they have done none.
    fn saw(&mut self, ops: Option<u64>, now: Instant) -> Duration {
        match (self.last, ops) {
            (Some(last), Some(now_ops)) if now_ops != last => {
                self.last = ops;
                self.since = now;
            }
            (None, Some(_)) => self.last = ops,
            _ => {}
        }
        now.saturating_duration_since(self.since)
    }

    /// [`Progress::saw`] the guest's drives as they read now ([`disk_operations`]).
    fn look(&mut self, l: &Layout) -> Duration {
        let ops = disk_operations(l);
        self.saw(ops, Instant::now())
    }

    /// The operations the drives had done at the last look that read them.
    fn done(&self) -> Option<u64> {
        self.last
    }
}

/// `its screens: <ramfb's>, <the virtio-gpu's>`.
fn screens_said(l: &Layout) -> String {
    format!(
        "its screens: {}, {}",
        l.screen().display(),
        l.screen_gpu().display()
    )
}

fn press_through_boot_prompt(l: &Layout) {
    let key = "{\"execute\":\"send-key\",\"arguments\":{\"keys\":[{\"type\":\"qcode\",\"data\":\"ret\"}]}}";
    let t0 = Instant::now();
    while t0.elapsed() < BOOT_KEYS_FOR {
        let _ = qmp(l, &[key.to_string()]);
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The lane's SSH to the guest.
#[derive(Clone, Debug)]
pub struct Ssh {
    pub key: PathBuf,
    pub known_hosts: PathBuf,
    pub port: u16,
    /// [`guest::USER`] (the administrator: install, provisioning, shutdown) or [`guest::RUNNER`].
    pub user: &'static str,
    /// `ssh`'s `ConnectTimeout`: [`CONNECT_WITHIN`], or [`POLL_CONNECT_WITHIN`] for a poll.
    pub connect_within: Duration,
}

impl Ssh {
    /// As the guest's administrator, [`guest::USER`].
    #[must_use]
    pub fn new(l: &Layout, port: u16) -> Ssh {
        Ssh {
            key: l.key(),
            known_hosts: l.known_hosts(),
            port,
            user: guest::USER,
            connect_within: CONNECT_WITHIN,
        }
    }

    /// The same connection as the standard user [`guest::RUNNER`].
    #[must_use]
    pub fn runner(&self) -> Ssh {
        Ssh {
            user: guest::RUNNER,
            ..self.clone()
        }
    }

    /// The same connection for a poll that asks again: given [`POLL_CONNECT_WITHIN`] to connect.
    #[must_use]
    pub fn polling(&self) -> Ssh {
        Ssh {
            connect_within: POLL_CONNECT_WITHIN,
            ..self.clone()
        }
    }

    fn options(&self) -> Vec<String> {
        self.options_for(false)
    }

    fn options_for(&self, existing_host: bool) -> Vec<String> {
        let mut options = vec![
            "-F".into(),
            "/dev/null".into(),
            "-i".into(),
            lossy(&self.key),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            format!("UserKnownHostsFile={}", lossy(&self.known_hosts)),
            "-o".into(),
            if existing_host {
                "StrictHostKeyChecking=yes"
            } else {
                "StrictHostKeyChecking=accept-new"
            }
            .into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            format!("ConnectTimeout={}", self.connect_within.as_secs()),
            "-o".into(),
            "ServerAliveInterval=15".into(),
            "-o".into(),
            "ServerAliveCountMax=8".into(),
            "-o".into(),
            "LogLevel=ERROR".into(),
        ];
        if existing_host {
            options.extend([
                "-o".into(),
                "UpdateHostKeys=no".into(),
                "-o".into(),
                "GlobalKnownHostsFile=/dev/null".into(),
            ]);
        }
        options
    }

    /// `ssh`'s argv for `command` (a cmd.exe line in the guest).
    #[must_use]
    pub fn argv(&self, command: &str) -> Vec<String> {
        let mut a = self.options();
        a.extend([
            "-p".into(),
            self.port.to_string(),
            format!("{}@127.0.0.1", self.user),
            command.to_string(),
        ]);
        a
    }

    /// Run `command` in the guest, captured.
    #[must_use]
    pub fn run(&self, command: &str) -> Out {
        run(Path::new("/usr/bin/ssh"), &self.argv(command), None)
    }

    /// Whether `command` ran in the guest and exited 0 within `within`: ssh is ended at the
    /// limit, so a guest that hangs while it is asked (its SSH session open, nothing answering)
    /// holds the asker no longer than that.
    #[must_use]
    pub fn ran_within(&self, command: &str, within: Duration) -> bool {
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/ssh");
        cmd.args(self.argv(command));
        aterm_uds::deadline::output_within(&mut cmd, within).is_ok_and(|o| o.status.success())
    }

    /// Run the PowerShell `script` in the guest, captured, with no progress records.
    #[must_use]
    pub fn ps(&self, script: &str) -> Out {
        self.run(&guest::powershell_command(&format!(
            "$ProgressPreference = 'SilentlyContinue'\n{script}"
        )))
    }

    /// Run `command` in the guest, streaming and retaining each output line.
    #[must_use]
    pub fn streamed(&self, command: &str, prefix: &str) -> Out {
        let t0 = Instant::now();
        let mut child = match aterm_uds::spawnfd::command("/usr/bin/ssh")
            .args(self.argv(command))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return Out {
                    code: None,
                    text: format!("ssh could not start: {e}"),
                    secs: 0.0,
                    signal: None,
                    not_started: None,
                    lost: None,
                };
            }
        };
        let err_reader = child.stderr.take().map(|e| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = e.take(16 * 1024 * 1024).read_to_string(&mut s);
                s
            })
        });
        let mut text = String::new();
        if let Some(out) = child.stdout.take() {
            let stdout = std::io::stdout();
            for line in BufReader::new(out.take(16 * 1024 * 1024)).split(b'\n') {
                let Ok(line) = line else { break };
                let line = String::from_utf8_lossy(&line);
                let line = line.trim_end_matches('\r');
                {
                    let mut lock = stdout.lock();
                    let _ = writeln!(lock, "{prefix}{line}");
                }
                text.push_str(line);
                text.push('\n');
            }
        }
        if let Some(h) = err_reader {
            let err = h.join().unwrap_or_default();
            if !err.is_empty() {
                eprint!("{err}");
            }
            text.push_str(&err);
        }
        let status = child.wait();
        Out {
            code: status.ok().and_then(|s| s.code()),
            text,
            secs: t0.elapsed().as_secs_f64(),
            signal: None,
            not_started: None,
            lost: None,
        }
    }

    /// Copy the file `local` to the guest path `remote` (forward slashes).
    #[must_use]
    pub fn put(&self, local: &Path, remote: &str) -> Out {
        let mut a = self.options();
        a.extend([
            "-P".into(),
            self.port.to_string(),
            "-q".into(),
            lossy(local),
            format!("{}@127.0.0.1:{remote}", self.user),
        ]);
        run(Path::new("/usr/bin/scp"), &a, None)
    }

    /// Copy the guest file `remote` (forward slashes) to `local` on this Mac.
    #[must_use]
    pub fn get(&self, remote: &str, local: &Path) -> Out {
        let mut a = self.options();
        a.extend([
            "-P".into(),
            self.port.to_string(),
            "-q".into(),
            format!("{}@127.0.0.1:{remote}", self.user),
            lossy(local),
        ]);
        run(Path::new("/usr/bin/scp"), &a, None)
    }
}

fn answers(ssh: &Ssh) -> bool {
    let out = ssh.run("echo lane-ok");
    out.ok() && out.text.contains("lane-ok")
}

/// `d` as the lane says a wait: whole minutes as `N min`, else seconds.
fn span(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 && s.is_multiple_of(60) {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

/// Wait until `answered` (the guest answers on SSH) while `what`, within `p`. Where `p` reads a
/// stall, a guest that shows no progress for it (no answer, and no operation on its drives) is
/// hung, as twice in the firmware: it is started afresh through `relaunch` once, a step of the
/// boot and not a retry of a check, and at a second stall its QEMU is ended at once. Once a
/// minute it says how long it has waited and, reading a stall, how many operations the guest's
/// drives did in that minute.
/// # Errors
/// QEMU exited, the guest did not answer within `p.within`, it stalled again after its relaunch
/// (its QEMU then ended), or the relaunch failed.
fn wait_answer(
    l: &Layout,
    p: Patience,
    what: &str,
    answered: &mut dyn FnMut() -> bool,
    relaunch: &mut dyn FnMut() -> Result<(), String>,
) -> Result<f64, String> {
    let t0 = Instant::now();
    let mut progress = Progress::from(t0);
    let mut relaunched = false;
    let mut said = Duration::ZERO;
    let mut said_done = None;
    loop {
        if answered() {
            return Ok(t0.elapsed().as_secs_f64());
        }
        if running(l)?.is_none() {
            return Err(format!("QEMU exited while {what}"));
        }
        let waited = t0.elapsed();
        if waited > p.within {
            screendumps(l);
            return Err(format!(
                "the guest did not answer on SSH within {} while {what}; {}",
                span(p.within),
                screens_said(l)
            ));
        }
        if let Some(stall) = p.stall {
            let still = progress.look(l);
            if still >= stall {
                screendumps(l);
                let hung = format!(
                    "no answer on SSH and no operation on the VM's drives for {} s while {what} \
                     ({}): the guest is hung, as twice in its firmware",
                    still.as_secs(),
                    screens_said(l)
                );
                if relaunched {
                    let ended = stop(l, None, Patience::NOW).map_or_else(
                        |e| format!("its QEMU did not end: {e}"),
                        |s| format!("the VM {s}"),
                    );
                    return Err(format!(
                        "{hung}, again after it was started afresh; {ended}"
                    ));
                }
                println!(
                    "windows:   {hung}; it is ended and booted cold again, once (a step of the boot)"
                );
                relaunch().map_err(|e| format!("{hung}; started afresh, it did not start: {e}"))?;
                relaunched = true;
                progress = Progress::from(Instant::now());
                said_done = None;
                continue;
            }
        }
        if waited >= said + Duration::from_secs(60) {
            said = waited;
            let work = match (said_done, progress.done()) {
                (Some(then), Some(now)) if p.stall.is_some() => {
                    format!(
                        "{} operations on its drives in the last minute; ",
                        now.saturating_sub(then)
                    )
                }
                _ => String::new(),
            };
            said_done = progress.done();
            screendump(l);
            println!(
                "windows:   {what}: {} min ({work}screen: {})",
                waited.as_secs() / 60,
                l.screen().display()
            );
        }
        std::thread::sleep(p.look);
    }
}

/// How [`stop`] left the VM.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stopped {
    /// No QEMU of this VM's ran.
    NotRunning,
    /// Windows shut down as it was asked.
    ShutDown,
    /// Its captured QEMU was ended from outside (QMP's `quit`, then SIGKILL) and is gone: why.
    Forced(String),
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stopped::NotRunning => f.write_str("was not running"),
            Stopped::ShutDown => f.write_str("stopped (Windows shut down)"),
            Stopped::Forced(why) => write!(f, "stopped by force ({why})"),
        }
    }
}

/// Stop the VM. Windows is asked to shut down (over SSH as its administrator when `ssh` answers,
/// else by ACPI's power button through QMP) and given `p.within`, or less where it shows no
/// progress for `p.stall` ([`Progress`]: a guest hung in its firmware ignores the button); with
/// [`Patience::NOW`] nothing is asked. A guest that did not shut down is ended through QMP's
/// `quit`, then SIGKILL of its captured QEMU, which must then be gone within a minute (a hang
/// detector). swtpm ends with QEMU's connection.
/// # Errors
/// QEMU or swtpm does not read as this VM's, or QEMU outlived its SIGKILL: their state is
/// preserved.
fn stop(l: &Layout, ssh: Option<&Ssh>, p: Patience) -> Result<Stopped, String> {
    let Some(process) = daemon(l, Daemon::Qemu)? else {
        stop_swtpm(l)?;
        return Ok(Stopped::NotRunning);
    };
    let mut forced = None;
    if p.within.is_zero() {
        forced = Some("ended at once".to_string());
    } else {
        // The drives as they were before Windows was asked: a guest that shows no progress
        // from here on, the ask's own time included, is hung.
        let t0 = Instant::now();
        let mut progress = Progress::from(t0);
        if p.stall.is_some() {
            progress.look(l);
        }
        let asked = ssh.is_some_and(|s| s.ran_within("shutdown /s /t 0 /f", ASK_WITHIN));
        if !asked {
            let _ = qmp(l, &["{\"execute\":\"system_powerdown\"}".to_string()]);
        }
        // A QEMU that ends at once is seen to end at once; a guest that takes its time is
        // looked at every `p.look`.
        let mut step = Duration::from_millis(25).min(p.look);
        while process.still_here(l, Daemon::Qemu)? {
            if t0.elapsed() >= p.within {
                screendumps(l);
                forced = Some(format!(
                    "Windows did not shut down within {}; {}",
                    span(p.within),
                    screens_said(l)
                ));
                break;
            }
            if let Some(stall) = p.stall {
                let still = progress.look(l);
                if still >= stall {
                    screendumps(l);
                    forced = Some(format!(
                        "Windows showed no sign of shutting down for {} s, no operation on its \
                         drives; {}",
                        still.as_secs(),
                        screens_said(l)
                    ));
                    break;
                }
            }
            std::thread::sleep(step);
            step = step.saturating_mul(2).min(p.look);
        }
    }
    if let Some(why) = &forced {
        if process.still_here(l, Daemon::Qemu)? {
            if p.within.is_zero() {
                screendumps(l);
            }
            let _ = qmp(l, &["{\"execute\":\"quit\"}".to_string()]);
            let t0 = Instant::now();
            while process.still_here(l, Daemon::Qemu)? && t0.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(100));
            }
            process.signal(l, Daemon::Qemu, libc::SIGKILL)?;
            let t0 = Instant::now();
            while process.still_here(l, Daemon::Qemu)? {
                if t0.elapsed() >= Duration::from_secs(60) {
                    return Err(format!(
                        "{why}; its QEMU (pid {}) outlived its SIGKILL by a minute; state preserved",
                        process.pid
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        } else if !p.within.is_zero() {
            forced = None;
        }
    }
    stop_swtpm(l)?;
    Ok(forced.map_or(Stopped::ShutDown, Stopped::Forced))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file() {
            crate::util::copy_regular_file(entry.path(), to.join(entry.file_name()))
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn snapshot(l: &Layout, t: &Tools, fingerprint: &str) -> Result<(), String> {
    let disk = lossy(&l.disk());
    let _ = run(
        &t.qemu_img,
        &strings(&["snapshot", "-d", SNAPSHOT, &disk]),
        None,
    );
    let out = run(
        &t.qemu_img,
        &strings(&["snapshot", "-c", SNAPSHOT, &disk]),
        None,
    );
    if !out.ok() {
        return Err(format!("qemu-img snapshot -c failed: {}", out.tail(5)));
    }
    crate::util::copy_regular_file(l.vars(), l.vars_saved())
        .map_err(|e| format!("{}: {e}", l.vars().display()))?;
    copy_dir(&l.tpm(), &l.tpm_saved())?;
    crate::util::atomic_write(&l.provisioned(), format!("{fingerprint}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    reverted(l)
}

fn revert(l: &Layout, t: &Tools) -> Result<(), String> {
    let out = run(
        &t.qemu_img,
        &strings(&["snapshot", "-a", SNAPSHOT, &lossy(&l.disk())]),
        None,
    );
    if !out.ok() {
        return Err(format!("qemu-img snapshot -a failed: {}", out.tail(5)));
    }
    crate::util::copy_regular_file(l.vars_saved(), l.vars())
        .map_err(|e| format!("{}: {e}", l.vars_saved().display()))?;
    copy_dir(&l.tpm_saved(), &l.tpm())?;
    reverted(l)
}

/// The VM's state is its snapshot's: [`Layout::unreverted`] goes.
fn reverted(l: &Layout) -> Result<(), String> {
    match std::fs::remove_file(l.unreverted()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", l.unreverted().display()))
        }
        _ => Ok(()),
    }
}

/// Revert a stopped VM whose state may hold writes no run reverted ([`Layout::unreverted`]: a
/// revert that failed after its stop, or a Mac that went down under a running VM) to its
/// snapshot, so no run boots them. Whether it did.
/// # Errors
/// The revert's.
fn revert_unreverted(l: &Layout, t: &Tools) -> Result<bool, String> {
    if !l.unreverted().exists() || snapshot_plan(l).is_none() {
        return Ok(false);
    }
    revert(l, t)?;
    Ok(true)
}

fn snapshot_plan(l: &Layout) -> Option<String> {
    crate::util::read_regular_text(&l.provisioned())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn secret_file(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !f
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?
        .is_file()
    {
        return Err(format!("{}: not a regular file", path.display()));
    }
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.set_len(0)
        .and_then(|()| f.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn lane_key(l: &Layout) -> Result<String, String> {
    if !l.key().is_file() {
        let out = run(
            Path::new("/usr/bin/ssh-keygen"),
            &strings(&[
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "aterm-windows-lane",
                "-f",
                &lossy(&l.key()),
            ]),
            None,
        );
        if !out.ok() {
            return Err(format!("ssh-keygen failed: {}", out.tail(3)));
        }
    }
    crate::util::read_regular_text(&l.key().with_extension("pub"))
        .map_err(|e| format!("the lane's public key: {e}"))
}

fn lane_password(l: &Layout) -> Result<String, String> {
    if let Ok(p) = crate::util::read_regular_text(&l.password())
        && !p.trim().is_empty()
    {
        return Ok(p.trim().to_string());
    }
    let mut b = [0u8; 16];
    aterm_uds::rand::fill(&mut b).map_err(|e| format!("entropy: {e}"))?;
    let p = aterm_codec::hex::encode_lower(&b);
    secret_file(&l.password(), &p)?;
    Ok(p)
}

fn write_media(l: &Layout, virtio: &Path, public_key: &str, password: &str) -> Result<(), String> {
    let dir = l.media_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    secret_file(
        &dir.join("autounattend.xml"),
        &guest::autounattend(password),
    )?;
    crate::util::atomic_write(
        &dir.join(guest::FIRST_LOGON_FILE),
        guest::first_logon_script().as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    crate::util::atomic_write(
        &dir.join("administrators_authorized_keys"),
        public_key.as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let drivers = dir.join(NETKVM_DIR);
    std::fs::create_dir_all(&drivers).map_err(|e| format!("{}: {e}", drivers.display()))?;
    let inf = iso_member(virtio, &format!("{NETKVM_DIR}/{NETKVM_INF}"))?;
    let sources = inf_sources(&String::from_utf8_lossy(&inf));
    if !sources.iter().any(|f| f.eq_ignore_ascii_case("netkvm.sys")) {
        return Err(format!(
            "{NETKVM_DIR}/{NETKVM_INF} names no netkvm.sys in [SourceDisksFiles]"
        ));
    }
    crate::util::atomic_write(&drivers.join(NETKVM_INF), &inf).map_err(|e| e.to_string())?;
    for file in sources.iter().map(String::as_str).chain([NETKVM_CAT]) {
        let bytes = iso_member(virtio, &format!("{NETKVM_DIR}/{file}"))?;
        crate::util::atomic_write(&drivers.join(file), &bytes).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(l.media());
    let out = run(
        Path::new("/usr/bin/hdiutil"),
        &[
            "makehybrid".into(),
            "-quiet".into(),
            "-iso".into(),
            "-joliet".into(),
            "-default-volume-name".into(),
            guest::MEDIA_LABEL.into(),
            "-o".into(),
            lossy(&l.media()),
            lossy(&dir),
        ],
        None,
    );
    if !out.ok() {
        return Err(format!("hdiutil makehybrid failed: {}", out.tail(3)));
    }
    let _ = std::fs::remove_dir_all(&dir);
    aterm_uds::regular_file::open(&l.media())
        .and_then(|(f, _)| f.set_permissions(std::fs::Permissions::from_mode(0o600)))
        .map_err(|e| e.to_string())
}

/// NetKVM's INF and catalog.
pub const NETKVM_INF: &str = "netkvm.inf";
pub const NETKVM_CAT: &str = "netkvm.cat";

/// The files an INF's `[SourceDisksFiles]` sections name.
#[must_use]
pub fn inf_sources(inf: &str) -> Vec<String> {
    let mut inside = false;
    let mut files = Vec::new();
    for line in inf.lines() {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.starts_with('[') {
            let section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_ascii_lowercase();
            inside = section == "sourcedisksfiles" || section.starts_with("sourcedisksfiles.");
        } else if inside && let Some((name, _)) = line.split_once('=') {
            let name = name.trim();
            if !name.is_empty() {
                files.push(name.to_string());
            }
        }
    }
    files
}

/// The hard link an ISO listing line (`tar -tv`) names, when it is one.
#[must_use]
pub fn link_target(listing: &str) -> Option<&str> {
    listing
        .lines()
        .next()?
        .split_once(" link to ")
        .map(|(_, t)| t.trim())
}

fn iso_member(iso: &Path, member: &str) -> Result<Vec<u8>, String> {
    let listed = run(
        Path::new("/usr/bin/tar"),
        &["-tvf".into(), lossy(iso), member.into()],
        None,
    );
    if !listed.ok() {
        return Err(format!(
            "{member} is not on {}: {}",
            iso.display(),
            listed.tail(3)
        ));
    }
    let real = link_target(&listed.text).unwrap_or(member).to_string();
    let out = aterm_uds::spawnfd::command("/usr/bin/tar")
        .args(["-xOf", &lossy(iso), &real])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!(
            "{real} did not come off {}: {}",
            iso.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn new_machine(l: &Layout, t: &Tools) -> Result<(), String> {
    for f in [
        l.disk(),
        l.vars(),
        l.vars_saved(),
        l.installed(),
        l.provisioned(),
        l.known_hosts(),
    ] {
        let _ = std::fs::remove_file(f);
    }
    for d in [l.tpm(), l.tpm_saved()] {
        let _ = std::fs::remove_dir_all(d);
    }
    let out = run(
        &t.qemu_img,
        &strings(&[
            "create",
            "-q",
            "-f",
            "qcow2",
            &lossy(&l.disk()),
            &format!("{DISK_GIB}G"),
        ]),
        None,
    );
    if !out.ok() {
        return Err(format!("qemu-img create failed: {}", out.tail(3)));
    }
    crate::util::copy_regular_file(&t.vars, l.vars())
        .map_err(|e| format!("{}: {e}", t.vars.display()))?;
    aterm_uds::regular_file::open(&l.vars())
        .and_then(|(f, _)| f.set_permissions(std::fs::Permissions::from_mode(0o600)))
        .map_err(|e| e.to_string())
}

fn install(l: &Layout, t: &Tools, timings: &mut Vec<(String, f64)>) -> Result<Ssh, Fail> {
    let (iso, _) = match cached_iso(l) {
        Some(found) => found,
        None => {
            let t0 = Instant::now();
            let got = fetch_windows_iso(l).map_err(|e| not_run(format!("the Windows ISO: {e}")))?;
            timings.push(("Windows ISO fetched".into(), t0.elapsed().as_secs_f64()));
            got
        }
    };
    let virtio = virtio_iso(l).map_err(|e| not_run(format!("virtio-win: {e}")))?;
    let public_key = lane_key(l).map_err(not_run)?;
    let password = lane_password(l).map_err(not_run)?;
    write_media(l, &virtio, &public_key, &password).map_err(not_run)?;
    new_machine(l, t).map_err(not_run)?;
    let port = free_port().map_err(not_run)?;
    println!(
        "windows: installing {} unattended ({CPUS} CPUs, {MEMORY_GIB} GiB, disk <= {DISK_GIB} GiB) …",
        iso.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    );
    start(
        l,
        t,
        &Boot {
            install: Some((iso, l.media())),
            port,
            vnc: None,
        },
    )
    .map_err(not_run)?;
    press_through_boot_prompt(l);
    let ssh = Ssh::new(l, port);
    match wait_answer(
        l,
        Patience::INSTALL,
        "installing",
        &mut || answers(&ssh.polling()),
        &mut || Ok(()),
    ) {
        Ok(secs) => timings.push(("install and first logon".into(), secs)),
        Err(e) => {
            let _ = stop(l, None, Patience::STOP);
            return Err(not_run(e));
        }
    }
    let done = ssh.run(&format!("type {}", guest::FIRST_LOGON_DONE));
    if !done.ok() {
        let log = ssh.run(&format!("type {}", guest::FIRST_LOGON_LOG));
        let _ = stop(l, Some(&ssh), Patience::STOP);
        return Err(not_run(format!(
            "the first logon did not finish:\n{}",
            log.tail(20)
        )));
    }
    crate::util::atomic_write(&l.installed(), b"ok\n").map_err(|e| Fail::die(e.to_string()))?;
    Ok(ssh)
}

/// Restart Windows cold: shut down from inside with a restart's patience ([`stop`],
/// [`Patience::KEEP`]), its QEMU gone, then booted again on the same port ([`start`]) and
/// waited for to SSH and to `lane`'s desktop. Both restarts that hung (2026-10-21) were warm, a
/// `shutdown /r` that resets the machine inside QEMU; no cold boot recorded has hung. `booted`
/// runs whenever a new QEMU runs (the run's claim and its lane's record credit it).
/// # Errors
/// Windows did not shut down, the VM did not start or answer ([`wait_answer`]), or `lane` had no
/// desktop.
fn reboot(l: &Layout, t: &Tools, ssh: &Ssh, booted: &mut dyn FnMut()) -> Result<(), String> {
    match stop(l, Some(ssh), Patience::KEEP)? {
        Stopped::ShutDown => {}
        other => {
            return Err(format!(
                "Windows did not shut down to restart: the VM {other}"
            ));
        }
    }
    let boot = Boot {
        install: None,
        port: ssh.port,
        vnc: None,
    };
    start(l, t, &boot)?;
    booted();
    wait_answer(
        l,
        Patience::BOOT,
        "restarting",
        &mut || answers(&ssh.polling()),
        &mut || relaunch(l, t, &boot, booted),
    )?;
    wait_desktop(ssh)
}

/// Start the VM afresh on `boot`: its QEMU ended at once ([`Patience::NOW`]), swtpm with it, both
/// started again cold, and `booted` run.
/// # Errors
/// The stop's or the start's.
fn relaunch(l: &Layout, t: &Tools, boot: &Boot, booted: &mut dyn FnMut()) -> Result<(), String> {
    stop(l, None, Patience::NOW)?;
    start(l, t, boot)?;
    booted();
    Ok(())
}

fn wait_desktop(ssh: &Ssh) -> Result<(), String> {
    let t0 = Instant::now();
    loop {
        if ssh
            .ps(&guest::desktop_check())
            .text
            .contains(guest::DESKTOP_UP)
        {
            return Ok(());
        }
        if t0.elapsed() > DESKTOP_WITHIN {
            return Err(format!(
                "{} has no desktop session {} s after boot (it is logged on automatically)",
                guest::RUNNER,
                DESKTOP_WITHIN.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn stage_display_driver(l: &Layout, ssh: &Ssh) -> Result<(), String> {
    let iso = virtio_iso(l)?;
    let made = ssh.run(&format!(
        "if not exist {d} mkdir {d}",
        d = guest::VIOGPUDO_GUEST
    ));
    if !made.ok() {
        return Err(format!("mkdir {}: {}", guest::VIOGPUDO_GUEST, made.tail(3)));
    }
    for (file, want) in guest::VIOGPUDO_FILES {
        let bytes = iso_member(&iso, &format!("{}/{file}", guest::VIOGPUDO_DIR))?;
        let local = l.run().join(file);
        crate::util::atomic_write(&local, &bytes)
            .map_err(|e| format!("{}: {e}", local.display()))?;
        let have = sha256_file(&local);
        if have.as_deref() != Ok(want) {
            let _ = std::fs::remove_file(&local);
            return Err(format!(
                "{}/{file} has SHA-256 {have:?}, not the pinned {want}",
                guest::VIOGPUDO_DIR
            ));
        }
        let put = ssh.put(
            &local,
            &format!("{}/{file}", guest::VIOGPUDO_GUEST.replace('\\', "/")),
        );
        let _ = std::fs::remove_file(&local);
        if !put.ok() {
            return Err(format!("{file} did not copy: {}", put.tail(3)));
        }
    }
    Ok(())
}

/// This Mac's `web2` to the guest's [`guest::web2_path`], whose digest the plan's `web2` step
/// checks.
fn stage_web2(ssh: &Ssh) -> Result<(), String> {
    let web2 = guest::web2_path();
    let (dir, _) = web2
        .rsplit_once('\\')
        .unwrap_or_else(|| unreachable!("{web2} names a folder"));
    let made = ssh.run(&format!("if not exist {dir} mkdir {dir}"));
    if !made.ok() {
        return Err(format!("mkdir {dir}: {}", made.tail(3)));
    }
    let put = ssh.put(
        Path::new(crate::linux_guest::WEB2),
        &web2.replace('\\', "/"),
    );
    if put.ok() { Ok(()) } else { Err(put.tail(3)) }
}

/// Provision the guest with the plan ([`guest::provision_steps`]) unless it holds it already:
/// how long it took. `booted` runs whenever a restart starts a new QEMU.
/// # Errors
/// A step failed, or a restart did.
fn provision(
    l: &Layout,
    t: &Tools,
    ssh: &Ssh,
    booted: &mut dyn FnMut(),
) -> Result<Option<f64>, String> {
    let steps = guest::provision_steps();
    let want = guest::fingerprint(&steps);
    let have = ssh.run(&format!("type {}", guest::STAMP));
    if have.ok() && have.text.trim() == want {
        return Ok(None);
    }
    println!("windows: provisioning the guest (plan {want}) …");
    let t0 = Instant::now();
    stage_display_driver(l, ssh)
        .map_err(|e| format!("virtio-win's display driver did not copy: {e}"))?;
    if steps.iter().any(|s| s.label == "web2") {
        stage_web2(ssh).map_err(|e| format!("web2 did not copy: {e}"))?;
    }
    let runner = ssh.runner();
    for s in &steps {
        println!("windows:   {} …", s.label);
        let by = if s.runner { &runner } else { ssh };
        let out = by.streamed(&guest::powershell_command(&s.script), "windows:     ");
        if !out.ok() {
            return Err(format!(
                "provisioning step '{}' failed:\n{}",
                s.label,
                out.tail(15)
            ));
        }
        if s.reboot {
            println!("windows:   restarting Windows cold (shut down, then booted) …");
            reboot(l, t, ssh, booted).map_err(|e| format!("after '{}': {e}", s.label))?;
        }
    }
    let wrote = ssh.ps(&format!(
        "Set-Content -Path {} -Value {}",
        guest::ps_quote(guest::STAMP),
        guest::ps_quote(&want)
    ));
    if !wrote.ok() {
        return Err(format!(
            "could not write {}: {}",
            guest::STAMP,
            wrote.tail(5)
        ));
    }
    Ok(Some(t0.elapsed().as_secs_f64()))
}

/// The running VM, booted and provisioned, and whether this call booted it.
pub struct Up {
    pub layout: Layout,
    pub tools: Tools,
    /// As [`guest::RUNNER`], the standard user.
    pub ssh: Ssh,
    /// As [`guest::USER`], the administrator, which shuts Windows down.
    pub admin: Ssh,
    pub booted_here: bool,
    /// What it took, in order.
    pub timings: Vec<(String, f64)>,
    /// This run's claim on the VM ([`crate::vm_admit`]), held until the run ends, and
    /// `CARGO_BUILD_JOBS` for the guest's builds ([`Up::cargo_env`]).
    admitted: crate::vm_admit::Admitted,
    /// The booter record of the VM this run booted, which its end settles.
    booter: Option<PathBuf>,
}

/// Explicit custody of an already running VM. Neither an absent observation nor a
/// PID alone identifies a held VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldVm {
    pid: i32,
    birth: aterm_uds::process::ProcessBirth,
    plan: String,
}

impl HeldVm {
    fn matches(
        &self,
        actual: Option<DaemonProcess>,
        booter: &crate::vm_admit::Booter,
        plan: Option<&str>,
    ) -> bool {
        actual
            == Some(DaemonProcess {
                pid: self.pid,
                birth: self.birth,
            })
            && booter.kept
            && booter.vm_pid == u32::try_from(self.pid).ok()
            && booter.vm_birth == Some((self.birth.seconds, self.birth.microseconds))
            && plan == Some(self.plan.as_str())
    }

    pub(crate) fn check(&self, l: &Layout) -> Result<(), Fail> {
        let booter = crate::vm_admit::Booter::read(&l.booter())
            .ok_or_else(|| not_run("held VM has no readable booter identity; preserved"))?;
        let actual = daemon(l, Daemon::Qemu).map_err(not_run)?;
        if !self.matches(actual, &booter, snapshot_plan(l).as_deref()) {
            return Err(not_run(
                "held VM PID, generation, kept record or provisioned plan differs; preserved",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "windows_held_vm_tests.rs"]
mod held_vm_tests;

/// Attach under the caller's lane lock, without any lifecycle recovery or cleanup.
/// The deliberately retained guest need not have today's provisioning plan.
fn attach_existing(l: Layout, held: &HeldVm) -> Result<Up, Fail> {
    held.check(&l)?;
    let tools = Tools::find()?;
    let admitted = crate::vm_admit::admit(&crate::vm_admit::WINDOWS, false)?;
    held.check(&l)?;
    let port = read_port(&l).ok_or_else(|| not_run("held VM recorded no SSH port; preserved"))?;
    let admin = Ssh::new(&l, port);
    let ssh = admin.runner();
    let identity = ssh.ps(&guest_identity_script(&held.plan, true));
    if !identity.ok() || !identity.text.lines().any(|s| s.trim() == guest::DESKTOP_UP) {
        return Err(not_run(format!(
            "held guest runner, plan or interactive desktop is unavailable; preserved: {}",
            identity.tail(5)
        )));
    }
    held.check(&l)?;
    println!(
        "windows: attached held QEMU pid {} birth {}:{} plan {}; lifecycle and display unchanged",
        held.pid, held.birth.seconds, held.birth.microseconds, held.plan
    );
    println!(
        "windows: current source provisioning plan {} (observed only)",
        guest::fingerprint(&guest::provision_steps())
    );
    Ok(Up {
        layout: l,
        tools,
        ssh,
        admin,
        booted_here: false,
        timings: Vec::new(),
        admitted,
        booter: None,
    })
}

/// The fixed runner and provisioned-plan check shared by held operations. Observation does
/// not need an interactive desktop; mutative attachment retains that additional requirement.
fn guest_identity_script(plan: &str, desktop: bool) -> String {
    format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; try {{ if ($env:USERNAME -ine {}) {{ exit 125 }}; \
         if ((Get-Content -Raw {}).Trim() -cne {}) {{ exit 125 }}; {} }} catch {{ exit 125 }}",
        guest::ps_quote(guest::RUNNER),
        guest::ps_quote(guest::STAMP),
        guest::ps_quote(plan),
        if desktop {
            guest::desktop_check()
        } else {
            "'lane-identity-ok'".into()
        }
    )
}

fn parse_attach(args: &[String]) -> Result<(HeldVm, &[String]), Fail> {
    let usage = || {
        Fail::usage(
            "windows attach --pid N --birth SECONDS:MICROSECONDS --plan HEX exec|settings|get|observe ...",
        )
    };
    let [pid_flag, pid, birth_flag, birth, plan_flag, plan, rest @ ..] = args else {
        return Err(usage());
    };
    if pid_flag != "--pid" || birth_flag != "--birth" || plan_flag != "--plan" {
        return Err(usage());
    }
    let pid = pid
        .parse::<i32>()
        .ok()
        .filter(|p| *p > 1)
        .ok_or_else(usage)?;
    let (seconds, microseconds) = birth.split_once(':').ok_or_else(usage)?;
    let seconds = seconds.parse::<u64>().map_err(|_| usage())?;
    let microseconds = microseconds
        .parse::<u64>()
        .ok()
        .filter(|us| *us < 1_000_000)
        .ok_or_else(usage)?;
    if plan.len() != 16 || !plan.bytes().all(|b| b.is_ascii_hexdigit()) || rest.is_empty() {
        return Err(usage());
    }
    Ok((
        HeldVm {
            pid,
            birth: aterm_uds::process::ProcessBirth {
                seconds,
                microseconds,
            },
            plan: plan.clone(),
        },
        rest,
    ))
}

fn cmd_attach(args: &[String]) -> Res {
    let (held, rest) = parse_attach(args)?;
    // Parse before taking custody; never pass an unknown verb to a lifecycle route.
    let [verb, args @ ..] = rest else {
        unreachable!()
    };
    if !matches!(verb.as_str(), "exec" | "settings" | "get" | "observe") {
        return Err(Fail::usage(
            "windows attach supports exec|settings|get|observe",
        ));
    }
    if verb == "observe" {
        return observe::run(&held, args);
    }
    if verb == "exec" {
        let _ = parse_exec(args)?;
    } else if verb == "settings" {
        crate::windows_settings::validate_args(args)?;
    } else {
        let _ = parse_get(args)?;
    }
    let l = Layout::find()?;
    if !l.home.is_dir() {
        return Err(not_run("held VM layout is absent; preserved"));
    }
    held.check(&l)?;
    let mut lane = lane_lock(&l)?;
    let up = attach_existing(l, &held)?;
    lane.credit(&vm_procs(&up.layout));
    held.check(&up.layout)?;
    if verb == "exec" {
        exec_on(up, parse_exec(args)?)
    } else if verb == "settings" {
        crate::windows_settings::run(&up, &held, args)
    } else {
        let (remote, out) = parse_get(args)?;
        get_held_artifact(&up, &held, &remote, &out)
            .map(|()| 0)
            .map_err(Fail::die)
    }
}

fn parse_get(args: &[String]) -> Result<(String, PathBuf), Fail> {
    let usage = || {
        Fail::usage("windows attach <custody> get C:/absolute/file --out NEW-ABSOLUTE-DIRECTORY")
    };
    let [remote, flag, out] = args else {
        return Err(usage());
    };
    // SCP receives one literal drive-absolute path, never a wildcard or shell expression.
    let remote = remote.replace('\\', "/");
    let bytes = remote.as_bytes();
    if flag != "--out"
        || !Path::new(out).is_absolute()
        || bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || &bytes[1..3] != b":/"
        || remote
            .chars()
            .any(|c| c.is_control() || "*?[]`$\"'<>|;".contains(c))
        || remote[3..].contains(':')
    {
        return Err(usage());
    }
    Ok((remote, out.into()))
}

#[derive(Debug, PartialEq, Eq)]
struct GuestArtifact {
    path: String,
    bytes: u64,
    sha256: String,
}

fn artifact_metadata(
    up: &Up,
    held: &HeldVm,
    remote: &str,
    out: &Path,
    label: &str,
) -> Result<GuestArtifact, String> {
    held.check(&up.layout).map_err(|e| format!("{e:?}"))?;
    let raw = up.ssh.ps(&format!(
        "$ErrorActionPreference='Stop'; $p={}; $f=Get-Item -LiteralPath $p -Force; if($f.PSIsContainer -or ($f.Attributes -band [IO.FileAttributes]::ReparsePoint)){{throw 'not a regular artifact'}}; [ordered]@{{path=$p;bytes=[uint64]$f.Length;sha256=(Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant()}} | ConvertTo-Json -Compress",
        guest::ps_quote(remote)
    ));
    artifact_receipt(
        out,
        label,
        &format!("ok={} code={:?}\n{}", raw.ok(), raw.code, raw.text),
    )?;
    let value: aterm_json::Value = aterm_json::from_str(raw.text.trim())
        .map_err(|e| format!("artifact metadata: {e}; {}", raw.tail(3)))?;
    let object = value
        .as_object()
        .filter(|v| v.len() == 3)
        .ok_or("artifact metadata is not a three-field object")?;
    let metadata = GuestArtifact {
        path: object
            .get("path")
            .and_then(aterm_json::Value::as_str)
            .ok_or("artifact path absent")?
            .into(),
        bytes: object
            .get("bytes")
            .and_then(aterm_json::Value::as_u64)
            .ok_or("artifact size absent")?,
        sha256: object
            .get("sha256")
            .and_then(aterm_json::Value::as_str)
            .ok_or("artifact hash absent")?
            .into(),
    };
    if !raw.ok() || metadata.path != remote || !aterm_codec::hex::is_lower(&metadata.sha256, 64) {
        return Err(format!("invalid guest artifact metadata: {}", raw.tail(3)));
    }
    Ok(metadata)
}

fn artifact_receipt(out: &Path, name: &str, text: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out.join(name))
        .map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes())
        .and_then(|()| aterm_durable::durable(&file))
        .and_then(|()| aterm_durable::Flush::Durable.dir(out))
        .map_err(|e| e.to_string())
}

fn get_held_artifact(up: &Up, held: &HeldVm, remote: &str, out: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(out)
        .map_err(|e| format!("new private artifact directory: {e}"))?;
    aterm_durable::Flush::Durable
        .dir(out.parent().ok_or("artifact directory has no parent")?)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        held.check(&up.layout).map_err(|e| format!("{e:?}"))?;
        artifact_receipt(
            out,
            "custody.json",
            &aterm_json::json!({
                "pid": held.pid, "birth_seconds": held.birth.seconds,
                "birth_microseconds": held.birth.microseconds, "plan": held.plan,
                "guest_path": remote, "source_sync": false
            })
            .to_string(),
        )?;
        let before = artifact_metadata(up, held, remote, out, "before.txt")?;
        let dest = out.join("artifact");
        // Reserve a private new file before the transport opens it. The enclosing directory is new.
        let reserved = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&dest)
            .map_err(|e| e.to_string())?;
        drop(reserved);
        held.check(&up.layout).map_err(|e| format!("{e:?}"))?;
        let copied = up.ssh.get(remote, &dest);
        artifact_receipt(
            out,
            "transport.txt",
            &format!("ok={} code={:?}\n{}", copied.ok(), copied.code, copied.text),
        )?;
        if !std::fs::symlink_metadata(&dest)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_file()
        {
            return Err("transport destination is not a regular artifact".into());
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&dest)
            .map_err(|e| e.to_string())?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        aterm_durable::durable(&file)
            .and_then(|()| aterm_durable::Flush::Durable.dir(out))
            .map_err(|e| e.to_string())?;
        if !copied.ok() {
            return Err(format!("artifact copy failed: {}", copied.tail(3)));
        }
        held.check(&up.layout).map_err(|e| format!("{e:?}"))?;
        let after = artifact_metadata(up, held, remote, out, "after.txt")?;
        let bytes = file.metadata().map_err(|e| e.to_string())?.len();
        let sha256 = sha256_file(&dest)?;
        if before != after || bytes != before.bytes || sha256 != before.sha256 {
            return Err("artifact changed or copied bytes differ from guest metadata".into());
        }
        held.check(&up.layout).map_err(|e| format!("{e:?}"))?;
        artifact_receipt(
            out,
            "VERIFIED.json",
            &aterm_json::json!({"path":before.path,"bytes":before.bytes,"sha256":before.sha256})
                .to_string(),
        )?;
        println!(
            "windows: retained {bytes} bytes sha256={sha256} at {} (held VM identity rechecked; no source sync)",
            dest.display()
        );
        Ok(())
    })();
    if let Err(reason) = &result {
        artifact_receipt(out, "REFUSED.txt", reason)?;
    }
    result
}

impl Up {
    /// The environment the guest's `cargo` gets for `arch` ([`guest::cargo_env`]), its job count
    /// the cores this run was admitted (`CARGO_BUILD_JOBS`).
    #[must_use]
    pub fn cargo_env(&self, arch: Arch) -> Vec<(String, String)> {
        let mut env = guest::cargo_env(arch);
        env.push(("CARGO_BUILD_JOBS".into(), self.admitted.jobs.to_string()));
        env
    }

    /// Shut down and revert the VM when this run booted it, saying how.
    /// # Errors
    /// [`shut_and_revert`]'s.
    pub fn release(self) -> Result<(), String> {
        if self.booted_here {
            let shut =
                shut_and_revert(&self.layout, &self.tools, Some(&self.admin), Patience::STOP);
            crate::vm_admit::settle(self.booter.as_deref(), false, shut.is_ok());
            println!("windows: VM {} (this run booted it)", shut?);
        }
        Ok(())
    }

    /// `up`'s end: the VM left running for later runs, which adopt it.
    pub fn keep(self) {
        crate::vm_admit::settle(self.booter.as_deref(), true, false);
    }
}

/// `down` on a running VM: Windows shut down within `p` (over SSH as its administrator where
/// the VM recorded its port, else ACPI's power button through QMP), or ended from outside, and
/// the disk put back to its snapshot either way ([`shut_and_revert`]); its booter record goes
/// once it has stopped. How it left the VM.
/// # Errors
/// The VM did not stop, or the disk did not revert.
fn down(l: &Layout, t: &Tools, p: Patience) -> Result<Shut, String> {
    let ssh = read_port(l).map(|port| Ssh::new(l, port));
    let shut = shut_and_revert(l, t, ssh.as_ref(), p)?;
    crate::vm_admit::clear(&l.booter());
    Ok(shut)
}

/// The running VM a run adopts, if any (its QEMU's pid). One whose one-shot booter has died is
/// that run's leftover ([`crate::vm_admit::leftover`]): it is stopped here, as `down` stops it,
/// and `None` comes back, so this run boots afresh.
/// # Errors
/// NOT RUN: the VM's state could not be read, or a leftover did not stop.
fn adopted(l: &Layout, t: &Tools, say: &mut dyn FnMut(&str)) -> Result<Option<i32>, Fail> {
    let Some(pid) = running(l).map_err(not_run)? else {
        return Ok(None);
    };
    let Some(gone) = crate::vm_admit::leftover(&l.booter()) else {
        return Ok(Some(pid));
    };
    say(&format!(
        "stopping {VM} left by dead run {} (QEMU pid {pid})",
        gone.describe()
    ));
    let shut = down(l, t, Patience::STOP).map_err(|e| {
        not_run(format!(
            "{VM}, left by dead run pid {}, did not stop: {e}",
            gone.pid
        ))
    })?;
    say(&format!("{VM} {shut}"));
    Ok(None)
}

/// What a Windows VM boot claims of its volume where no boot was measured: 35 GiB, less what
/// the VM's directory holds already (`aterm_capacity::Footprints::need`), so a VM on disk
/// claims nothing until a boot is measured. MEASURED 2026-10-25 on the Mac the lane was made on:
/// its directory holds 35 GiB (`du -sh`), the install (2026-10-20), the snapshot and every run
/// since included, and a qcow2 only grows, so a fresh install writes no more than that.
pub const BOOT_COLD_DISK: u64 = 35 << 30;

/// [`crate::linux::boot_run`] for the Windows lane: the VM's directory, with what the last boots
/// wrote kept in it.
fn windows_boot_run(l: &Layout) -> atpkg::admit_disk::Run {
    crate::linux::boot_run(
        "windows",
        &l.home,
        l.home.clone(),
        l.home.join(".boot-footprint.json"),
        BOOT_COLD_DISK,
    )
}

/// How [`shut_and_revert`] left the VM: stopped as [`Stopped`] says, and reverted to its snapshot
/// unless it has none.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shut {
    stopped: Stopped,
    reverted: bool,
}

impl std::fmt::Display for Shut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.stopped)?;
        if self.reverted {
            f.write_str(" and reverted to its snapshot")
        } else {
            f.write_str("; it has no snapshot to revert to")
        }
    }
}

/// Stop the VM within `p` ([`stop`]) and put its disk, firmware variables and TPM state back to
/// their snapshot, a stop that had to end QEMU from outside included, once that QEMU is gone.
/// # Errors
/// The stop's, with nothing reverted, or the revert's: the VM is then stopped and not reverted,
/// and the next run reverts it before it boots ([`revert_unreverted`]).
fn shut_and_revert(l: &Layout, t: &Tools, ssh: Option<&Ssh>, p: Patience) -> Result<Shut, String> {
    let stopped = stop(l, ssh, p)?;
    let reverted = snapshot_plan(l).is_some();
    if reverted {
        revert(l, t).map_err(|e| {
            format!("{stopped}, not reverted: {e}; the next run reverts it before it boots")
        })?;
    }
    Ok(Shut { stopped, reverted })
}

/// What a failed boot or provisioning left: [`shut_and_revert`]'s outcome, said.
fn left(shut: &Result<Shut, String>) -> String {
    match shut {
        Ok(shut) => format!("the VM {shut}"),
        Err(e) => format!("the VM: {e}"),
    }
}

fn boot(l: &Layout, t: &Tools, vnc: bool, timings: &mut Vec<(String, f64)>) -> Result<Ssh, Fail> {
    let port = free_port().map_err(not_run)?;
    let vnc = if vnc {
        Some(free_port().map_err(not_run)?.max(5900))
    } else {
        None
    };
    println!("windows: booting {VM} …");
    let boot = Boot {
        install: None,
        port,
        vnc,
    };
    start(l, t, &boot).map_err(not_run)?;
    if let Some(p) = vnc {
        println!("windows: VNC on 127.0.0.1:{p}");
    }
    let ssh = Ssh::new(l, port);
    match wait_answer(
        l,
        Patience::BOOT,
        "booting",
        &mut || answers(&ssh.polling()),
        &mut || relaunch(l, t, &boot, &mut || {}),
    ) {
        Ok(secs) => timings.push(("boot".into(), secs)),
        Err(e) => {
            let shut = shut_and_revert(l, t, None, Patience::STOP);
            return Err(not_run(format!("{e}; {}", left(&shut))));
        }
    }
    Ok(ssh)
}

fn end_leftovers(admin: &Ssh) {
    let out = admin.ps(&guest::end_leftovers());
    let ended = out
        .text
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("ended "))
        .filter(|_| out.ok());
    match ended {
        Some("0") => {}
        Some(n) => println!("windows: ended {n} processes an earlier run left in the guest"),
        None => eprintln!(
            "aterm-lab: windows: what an earlier run left in the guest was not ended: {}",
            out.tail(5)
        ),
    }
}

/// Make the VM if there is none, boot it, and provision it when its snapshot is not the plan's,
/// under `lane`, which this run holds: each VM it finds or boots is named in the lane's record
/// ([`LaneLock::credit`]), before any guest step.
/// # Errors
/// NOT RUN: a tool or an image missing, or the VM could not be made, booted or provisioned.
pub fn bring_up(lane: &mut LaneLock, vnc: bool) -> Result<Up, Fail> {
    let tools = Tools::find()?;
    let l = Layout::find()?;
    let mut timings = Vec::new();
    if let Some(pid) = adopted(&l, &tools, &mut |line| println!("windows: {line}"))? {
        let admitted = crate::vm_admit::admit(&crate::vm_admit::WINDOWS, false)?;
        let port = read_port(&l).ok_or_else(|| not_run("the running VM recorded no SSH port"))?;
        let admin = Ssh::new(&l, port);
        let ssh = admin.runner();
        if !answers(&ssh) {
            return Err(not_run(format!(
                "the running VM (QEMU pid {pid}) does not answer on SSH as {}; `{LAB} windows down`",
                guest::RUNNER
            )));
        }
        lane.credit(&vm_procs(&l));
        end_leftovers(&admin);
        return Ok(Up {
            layout: l,
            tools,
            ssh,
            admin,
            booted_here: false,
            timings,
            admitted,
            booter: None,
        });
    }
    let mut admitted = crate::vm_admit::admit(&crate::vm_admit::WINDOWS, true)?;
    let booter = crate::vm_admit::record_boot("windows", &l.booter());
    stop_swtpm(&l).map_err(not_run)?;
    if revert_unreverted(&l, &tools).map_err(not_run)? {
        println!(
            "windows: {VM}'s disk held writes no run reverted (a revert that failed, or a Mac \
             that went down under the VM): reverted to its snapshot before it boots"
        );
    }
    let want = guest::fingerprint(&guest::provision_steps());
    if snapshot_plan(&l).as_deref() != Some(want.as_str()) {
        let ssh = if l.installed().is_file() && l.disk().is_file() {
            boot(&l, &tools, false, &mut timings)?
        } else {
            install(&l, &tools, &mut timings)?
        };
        let procs = vm_procs(&l);
        admitted.credit("windows", &procs);
        lane.credit(&procs);
        match provision(&l, &tools, &ssh, &mut || {
            let procs = vm_procs(&l);
            admitted.credit("windows", &procs);
            lane.credit(&procs);
        }) {
            Ok(Some(secs)) => timings.push(("provision".into(), secs)),
            Ok(None) => {}
            Err(e) => {
                let shut = shut_and_revert(&l, &tools, Some(&ssh), Patience::STOP);
                return Err(not_run(format!("{e}; {}", left(&shut))));
            }
        }
        let t0 = Instant::now();
        match stop(&l, Some(&ssh), Patience::KEEP).map_err(not_run)? {
            Stopped::Forced(why) => {
                // `start` marks the disk unreverted only when an earlier snapshot exists.
                let next = if l.unreverted().exists() && snapshot_plan(&l).is_some() {
                    "the next run reverts the disk to its earlier snapshot before it boots"
                } else {
                    "with no earlier snapshot to revert to, the next run provisions this disk again"
                };
                return Err(not_run(format!(
                    "the provisioned guest did not shut down for its snapshot ({why}): no snapshot \
                     was taken, and {next}"
                )));
            }
            Stopped::NotRunning | Stopped::ShutDown => {}
        }
        timings.push(("shut down".into(), t0.elapsed().as_secs_f64()));
        let t0 = Instant::now();
        snapshot(&l, &tools, &want).map_err(not_run)?;
        timings.push(("snapshot".into(), t0.elapsed().as_secs_f64()));
    }
    let admin = boot(&l, &tools, vnc, &mut timings)?;
    let procs = vm_procs(&l);
    admitted.credit("windows", &procs);
    lane.credit(&procs);
    crate::vm_admit::record_vm(booter.as_deref(), qemu(&l));
    let ssh = admin.runner();
    if !answers(&ssh) {
        let shut = shut_and_revert(&l, &tools, Some(&admin), Patience::STOP);
        return Err(not_run(format!(
            "the guest answers its administrator but not {} on SSH (made by the first logon: `{LAB} windows destroy` \
             and `up` again); {}",
            guest::RUNNER,
            left(&shut)
        )));
    }
    Ok(Up {
        layout: l,
        tools,
        ssh,
        admin,
        booted_here: true,
        timings,
        admitted,
        booter,
    })
}

/// The running VM's QEMU, if any.
fn qemu(l: &Layout) -> Option<u32> {
    running(l)
        .ok()
        .flatten()
        .and_then(|p| u32::try_from(p).ok())
}

/// The running VM's processes ([`crate::vm_admit::vm_procs`]): its QEMU, which daemonizes away
/// from this run, and whatever holds its disk.
fn vm_procs(l: &Layout) -> Vec<u32> {
    crate::vm_admit::vm_procs(&l.disk(), qemu(l).as_slice())
}

/// [`lane_lock`] for a run that boots the VM: QEMU and swtpm found, the VM's turn to boot taken
/// ([`LaneLock::turn`]), then the boot's disk claimed ([`crate::linux::claim_boot_disk`]) before
/// the lane is waited for, so no run holds the lane while it waits for disk, and runs of the VM
/// queued behind one that boots it claim no disk until it is over. The turn and the claim are
/// held, and the VM's directory measured, until the lock is let go at the run's end.
/// # Errors
/// A tool missing, the turn's or the claim's NOT RUN, or [`lane_lock`]'s.
pub fn boot_lane(l: &Layout) -> Result<LaneLock, Fail> {
    Tools::find()?;
    home(l)?;
    LaneLock::boot_with(
        "windows",
        || LaneLock::turn(&l.lock(), "windows"),
        || crate::linux::claim_boot_disk("windows", windows_boot_run(l)),
        || lane_lock(l),
    )
}

/// The one lane run of this Mac, waiting for the run that holds it ([`LaneLock::at`]).
/// # Errors
/// `--no-wait` and another run holds it (NOT RUN, naming it), an interrupt while waiting, or the
/// lock file cannot be made.
pub fn lane_lock(l: &Layout) -> Result<LaneLock, Fail> {
    home(l)?;
    LaneLock::at(&l.lock(), "windows")
}

/// The VM's directory, private, where its locks are.
fn home(l: &Layout) -> Result<(), Fail> {
    crate::util::create_private_dir_all(&l.home)
        .map_err(|e| Fail::die(format!("{}: {e}", l.home.display())))
}

/// One file of the working tree: its path (forward slashes), size and modification time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub mtime: String,
}

/// The working tree's files.
/// # Errors
/// git failed.
pub fn tree_files(repo: &Path, prefix: &str) -> Result<Vec<Entry>, String> {
    let out = run(
        Path::new("/usr/bin/git"),
        &strings(&[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ]),
        Some(repo),
    );
    if !out.ok() {
        return Err(format!(
            "git ls-files in {}: {}",
            repo.display(),
            out.tail(3)
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut entries = Vec::new();
    for name in out.text.split('\0').filter(|n| !n.is_empty()) {
        if !seen.insert(name.to_string()) {
            continue;
        }
        let path = repo.join(name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if path.join(".git").exists() {
                entries.extend(tree_files(&path, &format!("{prefix}{name}/"))?);
            }
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(String::new, |d| {
                format!("{}.{:09}", d.as_secs(), d.subsec_nanos())
            });
        entries.push(Entry {
            path: format!("{prefix}{name}"),
            size: meta.len(),
            mtime,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// The manifest the guest keeps of what it was sent: `size mtime path` lines.
#[must_use]
pub fn manifest(entries: &[Entry]) -> String {
    let mut s = String::new();
    for e in entries {
        s.push_str(&format!("{} {} {}\n", e.size, e.mtime, e.path));
    }
    s
}

/// A manifest read back: path to `(size, mtime)`.
#[must_use]
pub fn parse_manifest(text: &str) -> BTreeMap<String, (u64, String)> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.trim_end_matches('\r').splitn(3, ' ');
            let size = parts.next()?.parse().ok()?;
            let mtime = parts.next()?.to_string();
            let path = parts.next()?.to_string();
            Some((path, (size, mtime)))
        })
        .collect()
}

/// Files to send (new or changed against the guest manifest) and files to remove.
#[must_use]
pub fn plan_copy(
    here: &[Entry],
    there: &BTreeMap<String, (u64, String)>,
) -> (Vec<String>, Vec<String>) {
    let send = here
        .iter()
        .filter(|e| there.get(&e.path) != Some(&(e.size, e.mtime.clone())))
        .map(|e| e.path.clone())
        .collect();
    let mine: std::collections::BTreeSet<&str> = here.iter().map(|e| e.path.as_str()).collect();
    let remove = there
        .keys()
        .filter(|p| !mine.contains(p.as_str()))
        .cloned()
        .collect();
    (send, remove)
}

/// The directories removing `remove` empties: each folder above a removed path that holds no
/// path of `here`, deepest first. Git keeps no empty folder, so the guest's copy keeps none
/// either: cargo reads an empty folder under a workspace glob (`crates/*`) as a member with
/// no manifest.
#[must_use]
pub fn emptied_dirs(here: &[Entry], remove: &[String]) -> Vec<String> {
    let mut kept = std::collections::BTreeSet::new();
    for e in here {
        let mut path = e.path.as_str();
        while let Some((dir, _)) = path.rsplit_once('/') {
            if !kept.insert(dir) {
                break;
            }
            path = dir;
        }
    }
    let mut gone = std::collections::BTreeSet::new();
    for r in remove {
        let mut path = r.as_str();
        while let Some((dir, _)) = path.rsplit_once('/') {
            if kept.contains(dir) || !gone.insert(dir) {
                break;
            }
            path = dir;
        }
    }
    let mut dirs: Vec<String> = gone.into_iter().map(str::to_string).collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    dirs
}

const GUEST_MANIFEST: &str = r"C:\lane\manifest";

/// Bring the guest's copy of `repo` up to its working tree.
/// # Errors
/// git, tar or a copy failed.
pub fn copy_tree(l: &Layout, ssh: &Ssh, repo: &Path) -> Result<(usize, usize), String> {
    let here = tree_files(repo, "")?;
    let there = ssh.run(&format!("type {GUEST_MANIFEST}"));
    let there = if there.ok() {
        parse_manifest(&there.text)
    } else {
        BTreeMap::new()
    };
    let (send, remove) = plan_copy(&here, &there);
    if send.is_empty() && remove.is_empty() {
        return Ok((0, 0));
    }
    let run_dir = l.run();
    if !send.is_empty() {
        let list = run_dir.join("copy.list");
        let tar = run_dir.join("copy.tar");
        crate::util::write_fresh(&list, send.join("\0")).map_err(|e| e.to_string())?;
        let out = aterm_uds::spawnfd::command("/usr/bin/tar")
            .args([
                "-c",
                "-f",
                &lossy(&tar),
                "--no-mac-metadata",
                "--no-xattrs",
                "--no-acls",
                "--null",
                "-T",
                &lossy(&list),
            ])
            .current_dir(repo)
            .env("COPYFILE_DISABLE", "1")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("tar: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "tar failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let put = ssh.put(&tar, "C:/lane/copy.tar");
        let _ = std::fs::remove_file(&tar);
        if !put.ok() {
            return Err(format!("scp of the copy failed: {}", put.tail(3)));
        }
        let unpacked = ssh.ps(&format!(
            "$ErrorActionPreference = 'Stop'\n\
             New-Item -ItemType Directory -Force -Path {src} | Out-Null\n\
             tar.exe -xmf {tar} -C {src}\n\
             if ($LASTEXITCODE -ne 0) {{ throw \"tar exited $LASTEXITCODE\" }}\n\
             Remove-Item -LiteralPath {tar}\n",
            src = guest::ps_quote(guest::SRC),
            tar = guest::ps_quote(r"C:\lane\copy.tar"),
        ));
        if !unpacked.ok() {
            return Err(format!("the copy did not unpack: {}", unpacked.tail(5)));
        }
    }
    if !remove.is_empty() {
        // Files first, then each folder they emptied (a trailing `/`), deepest first; a folder
        // the guest put anything else in stays.
        let mut lines = remove.clone();
        lines.extend(emptied_dirs(&here, &remove).into_iter().map(|d| d + "/"));
        let gone = run_dir.join("copy.remove");
        crate::util::write_fresh(&gone, lines.join("\n")).map_err(|e| e.to_string())?;
        let put = ssh.put(&gone, "C:/lane/copy.remove");
        let out = ssh.ps(&format!(
            "Get-Content -LiteralPath 'C:\\lane\\copy.remove' | ForEach-Object {{\n\
             $p = Join-Path {} $_.TrimEnd('/')\n\
             if (-not $_.EndsWith('/')) {{ Remove-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue }} \
             elseif ((Test-Path -LiteralPath $p -PathType Container) -and -not (Get-ChildItem -LiteralPath $p -Force)) {{ \
             Remove-Item -LiteralPath $p -Force }}\n\
             }}",
            guest::ps_quote(guest::SRC)
        ));
        if !put.ok() || !out.ok() {
            return Err(format!("removals failed: {}{}", put.tail(3), out.tail(3)));
        }
    }
    let m = run_dir.join("manifest");
    crate::util::write_fresh(&m, manifest(&here)).map_err(|e| e.to_string())?;
    let put = ssh.put(&m, "C:/lane/manifest");
    if !put.ok() {
        return Err(format!("the manifest did not copy: {}", put.tail(3)));
    }
    Ok((send.len(), remove.len()))
}

/// The build facts `crates/aterm/build.rs` takes pinned instead of asking git, in its `commit`
/// mode (`aterm_stamp::input`).
#[must_use]
pub fn build_facts(repo: &Path) -> Vec<(String, String)> {
    let git = |args: &[&str]| {
        let out = run(Path::new("/usr/bin/git"), &strings(args), Some(repo));
        out.ok()
            .then(|| out.text.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let (Some(full), Some(short)) = (
        git(&["rev-parse", "HEAD"]),
        git(&["rev-parse", "--short=12", "HEAD"]),
    ) else {
        return Vec::new();
    };
    let dirty = git(&[
        &["status", "--porcelain"][..],
        &crate::lab_build::stamp_paths::PATHSPEC,
    ]
    .concat())
    .is_some();
    let dev = git(&["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"])
        .and_then(|tag| git(&["rev-list", "--count", &format!("{tag}..HEAD")]))
        .unwrap_or_else(|| "0".into());
    use aterm_stamp::input;
    let mut facts = vec![(input::MODE.to_string(), "commit".to_string())];
    if let Some(epoch) = git(&["show", "-s", "--format=%ct", "HEAD"]) {
        facts.push((input::EPOCH.to_string(), epoch));
    }
    facts.extend([
        (
            input::COMMIT.into(),
            if dirty {
                format!("{short}-dirty")
            } else {
                short
            },
        ),
        (input::FULL_COMMIT.into(), full),
        (input::DEV_COMMITS.into(), dev),
    ]);
    facts
}

fn cmd_up(args: &[String]) -> Res {
    let vnc = match args {
        [] => false,
        [v] if v == "--vnc" => true,
        _ => {
            return Err(Fail::usage(format!(
                "windows up [--vnc] ({LAB} windows help)"
            )));
        }
    };
    let l = Layout::find()?;
    let mut lane = boot_lane(&l)?;
    if vnc
        && running(&l).map_err(Fail::die)?.is_some()
        && crate::vm_admit::leftover(&l.booter()).is_none()
    {
        return Err(Fail::usage(
            "windows up --vnc: the VM is already running without VNC; `windows down` first",
        ));
    }
    let up = bring_up(&mut lane, vnc)?;
    for (name, secs) in &up.timings {
        println!("windows: {name} {secs:.1} s");
    }
    println!(
        "windows: {VM} is up (SSH on 127.0.0.1:{}); `{LAB} windows exec -- CMD` runs in it, `{LAB} windows down` stops it",
        up.ssh.port
    );
    up.keep();
    Ok(0)
}

fn cmd_down() -> Res {
    let tools = Tools::find()?;
    let l = Layout::find()?;
    let _lock = lane_lock(&l)?;
    if running(&l).map_err(Fail::die)?.is_none() {
        stop_swtpm(&l).map_err(Fail::die)?;
        if revert_unreverted(&l, &tools).map_err(Fail::die)? {
            println!(
                "windows: {VM} was not running; its disk held writes no run reverted: reverted to its snapshot"
            );
        } else if snapshot_plan(&l).is_some() {
            println!("windows: {VM} is not running, and holds its snapshot");
        } else {
            println!("windows: {VM} is not running");
        }
        return Ok(0);
    }
    let shut = down(&l, &tools, Patience::STOP).map_err(Fail::die)?;
    println!("windows: {VM} {shut}");
    Ok(0)
}

fn cmd_destroy() -> Res {
    let l = Layout::find()?;
    let _lock = lane_lock(&l)?;
    let ssh = read_port(&l).map(|p| Ssh::new(&l, p));
    stop(&l, ssh.as_ref(), Patience::STOP).map_err(Fail::die)?;
    for f in [
        l.disk(),
        l.vars(),
        l.vars_saved(),
        l.key(),
        l.key().with_extension("pub"),
        l.known_hosts(),
        l.password(),
        l.media(),
        l.installed(),
        l.provisioned(),
        l.unreverted(),
    ] {
        let _ = std::fs::remove_file(f);
    }
    for d in [l.tpm(), l.tpm_saved(), l.media_dir(), l.run()] {
        let _ = std::fs::remove_dir_all(d);
    }
    println!(
        "windows: {VM} deleted ({}); the images stay in {}",
        l.home.display(),
        l.cache.display()
    );
    Ok(0)
}

fn allocated(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|m| m.blocks() * 512)
}

fn cmd_status() -> Res {
    let tools = Tools::find()?;
    let l = Layout::find()?;
    let v = run(&tools.qemu, &strings(&["--version"]), None);
    println!(
        "qemu      {} ({})",
        tools.qemu.display(),
        v.text.lines().next().unwrap_or("")
    );
    println!("swtpm     {}", tools.swtpm.display());
    match cached_iso(&l) {
        Some((iso, sha)) => println!("image     {} (SHA-256 {sha}, Microsoft's)", iso.display()),
        None => println!("image     none (the first `{LAB} windows up` fetches it)"),
    }
    let want = guest::fingerprint(&guest::provision_steps());
    if !l.disk().is_file() {
        println!("vm        none (`{LAB} windows up` makes it)");
        return Ok(0);
    }
    let qemu = running(&l).map_err(Fail::die)?;
    println!(
        "vm        {VM} {} — {CPUS} CPUs, {MEMORY_GIB} GiB, disk <= {DISK_GIB} GiB",
        qemu.map_or_else(
            || "stopped".to_string(),
            |pid| format!("running (QEMU pid {pid})")
        )
    );
    if qemu.is_some()
        && let Some(line) = crate::vm_admit::status_line(&l.booter())
    {
        println!("run       {line}");
    }
    println!(
        "snapshot  {}",
        match snapshot_plan(&l) {
            Some(have) if have == want =>
                format!("{SNAPSHOT}, provisioned with the current plan ({want})"),
            Some(have) =>
                format!("{SNAPSHOT}, plan {have}; the plan is {want}: the next `up` provisions"),
            None if l.installed().is_file() =>
                "none: installed, not provisioned (the next `up` provisions)".into(),
            None => "none: not installed (the next `up` installs)".into(),
        }
    );
    if qemu.is_none() && l.unreverted().exists() && snapshot_plan(&l).is_some() {
        println!("disk      holds writes no run reverted: `down` or the next run reverts them");
    }
    if let Some(bytes) = allocated(&l.disk()) {
        println!(
            "host disk {} allocated in {}",
            crate::linux::gib(bytes),
            l.disk().display()
        );
    }
    if running(&l).map_err(Fail::die)?.is_some()
        && let Some(port) = read_port(&l)
    {
        println!(
            "ssh       127.0.0.1:{port} ({}@ runs, {}@ administers; key {})",
            guest::RUNNER,
            guest::USER,
            l.key().display()
        );
        let ssh = Ssh::new(&l, port);
        let df = ssh.ps("$d = Get-PSDrive C; '{0} {1}' -f $d.Used, ($d.Used + $d.Free)");
        if let Some((used, size)) = df
            .text
            .split_whitespace()
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .and_then(|v| Some((*v.first()?, *v.get(1)?)))
        {
            println!(
                "guest C:  {} used of {}",
                crate::linux::gib(used),
                crate::linux::gib(size)
            );
        }
    }
    Ok(0)
}

/// What `exec` was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct ExecArgs {
    pub arch: Arch,
    /// In the runner's desktop session, not over SSH.
    pub desktop: bool,
    pub argv: Vec<String>,
}

/// `exec [--arch A] [--desktop] [--] CMD...`.
/// # Errors
/// A usage error.
pub fn parse_exec(args: &[String]) -> Result<ExecArgs, Fail> {
    let mut arch = Arch::Aarch64;
    let mut desktop = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--arch" => {
                let w = args.get(i + 1).map_or("", String::as_str);
                arch = Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("windows exec: --arch {w}: aarch64 or x86_64"))
                })?;
                i += 2;
            }
            "--desktop" => {
                desktop = true;
                i += 1;
            }
            "--" => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    let argv: Vec<String> = args.get(i..).unwrap_or_default().to_vec();
    if argv.is_empty() {
        return Err(Fail::usage(
            "windows exec: no command (… exec [--arch A] [--desktop] -- CMD)",
        ));
    }
    Ok(ExecArgs {
        arch,
        desktop,
        argv,
    })
}

/// Run `cmd.exe` `line` in the runner's desktop session under scheduled task `name`.
pub fn run_on_desktop(up: &Up, name: &str, line: &str, how: guest::Desktop, stream: bool) -> Out {
    let failed = |text: String| {
        eprintln!("aterm-lab: windows: {text}");
        Out {
            code: None,
            text,
            secs: 0.0,
            signal: None,
            not_started: None,
            lost: None,
        }
    };
    let run_dir = up.layout.run();
    let tasks = guest::TASKS.replace('\\', "/");
    for (ext, text) in [
        ("cmd", guest::desktop_batch(line, name)),
        ("ps1", guest::desktop_runner(name, how)),
    ] {
        let local = run_dir.join(format!("{name}.{ext}"));
        if let Err(e) = crate::util::write_fresh(&local, text) {
            return failed(format!("{}: {e}", local.display()));
        }
        let put = up.admin.put(&local, &format!("{tasks}/{name}.{ext}"));
        let _ = std::fs::remove_file(&local);
        if !put.ok() {
            return failed(format!("the {ext} did not copy: {}", put.tail(5)));
        }
    }
    let controller = format!(
        "powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File {}\\{name}.ps1",
        guest::TASKS
    );
    if stream {
        up.admin.streamed(&controller, "")
    } else {
        up.admin.run(&controller)
    }
}

/// A task name no other run of this Mac's uses: `aterm-lane-<what>-<pid>-<ms>`.
#[must_use]
pub fn task_name(what: &str) -> String {
    format!(
        "aterm-lane-{what}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    )
}

fn cmd_exec(args: &[String]) -> Res {
    let ask = parse_exec(args)?;
    let l = Layout::find()?;
    let mut lane = boot_lane(&l)?;
    let up = bring_up(&mut lane, false)?;
    exec_on(up, ask)
}

fn exec_on(up: Up, ask: ExecArgs) -> Res {
    let repo = crate::paths::physical(&paths().repo);
    let t0 = Instant::now();
    match copy_tree(&up.layout, &up.ssh, &repo) {
        Ok((0, 0)) => println!("windows: the guest's copy of {} is current", repo.display()),
        Ok((sent, removed)) => println!(
            "windows: copied {} into {} ({sent} sent, {removed} removed, {:.1} s)",
            repo.display(),
            guest::SRC,
            t0.elapsed().as_secs_f64()
        ),
        Err(e) => {
            up.release().map_err(Fail::die)?;
            return Err(not_run(format!("the checkout did not copy: {e}")));
        }
    }
    let mut env = up.cargo_env(ask.arch);
    env.extend(build_facts(&repo));
    println!(
        "windows: {} ({}, {}) in {}, {}: {}",
        ask.arch.name(),
        ask.arch.triple(),
        ask.arch.runs(),
        guest::SRC,
        if ask.desktop {
            "in lane's desktop session"
        } else {
            "over SSH"
        },
        ask.argv.join(" ")
    );
    let line = guest::exec_command(guest::SRC, &env, &ask.argv);
    let out = if ask.desktop {
        run_on_desktop(
            &up,
            &task_name("exec"),
            &line,
            guest::Desktop::Console,
            true,
        )
    } else {
        up.ssh.streamed(&line, "")
    };
    released(out.code.unwrap_or(1), up.release())
}

/// A completed command is no pass when its required cleanup failed.
pub fn released(code: i32, cleanup: Result<(), String>) -> Res {
    cleanup.map_err(Fail::die)?;
    Ok(code)
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
