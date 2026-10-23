// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows runtime lane on this Mac.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::linux::{LaneLock, Out};
use crate::paths::paths;
use crate::windows_guest::{self as guest, Arch};
use crate::{Fail, LAB, Res};

/// The VM's name (QEMU's `-name`).
pub const VM: &str = "aterm-windows";

/// What the VM runs with: the owner's sizing (2026-10-20).
pub const CPUS: u32 = 6;
pub const MEMORY_GIB: u32 = 8;
/// The disk is a qcow2 file that grows to what the guest has written.
pub const DISK_GIB: u32 = 128;

/// The qcow2 snapshot a provisioned guest boots from, and `down` returns to.
pub const SNAPSHOT: &str = "provisioned";

/// Exit status of a lane that could not run: never a pass.
pub const NOT_RUN: i32 = 3;

const PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];

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
const BOOT_WITHIN: Duration = Duration::from_secs(15 * 60);
const STOP_WITHIN: Duration = Duration::from_secs(5 * 60);
const GONE_WITHIN: Duration = Duration::from_secs(5 * 60);
const DESKTOP_WITHIN: Duration = Duration::from_secs(guest::DESKTOP_WITHIN_SECS);

const BOOT_KEYS_FOR: Duration = Duration::from_secs(12);

/// The verbs, as `windows help` prints them.
pub const USAGE: &str = "\
targo --unverified lab windows <verb> — the Windows runtime lane: a QEMU VM (hvf, Windows 11 arm64, Prism for x86_64)
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
                           VM the exec booted keeps running: `down` stops it
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
  destroy                  stop the VM and delete it (disk, firmware variables, TPM, key); the images stay cached
";

/// `windows <verb> …`.
/// # Errors
/// A usage error, or a step that failed.
pub fn cmd_windows(args: &[String]) -> Res {
    let verb = args.first().map_or("help", String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    match verb {
        "up" => cmd_up(rest),
        "down" => cmd_down(),
        "status" => cmd_status(),
        "exec" => cmd_exec(rest),
        "smoke" => crate::windows_smoke::cmd_smoke(rest),
        "window" => crate::windows_window::cmd_window(rest),
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
    /// One run at a time.
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
    /// The VM's screen, taken while the install runs and when a wait fails.
    #[must_use]
    pub fn screen(&self) -> PathBuf {
        self.run().join("screen.png")
    }
}

/// QEMU, `qemu-img`, swtpm and the firmware, from the person's own install ([`PREFIXES`]).
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
    /// QEMU and swtpm under the first of [`PREFIXES`] that has them.
    /// # Errors
    /// Not Apple silicon, or a tool missing: the lane cannot run here, and the NOT RUN
    /// names what is missing and where it was looked for.
    pub fn find() -> Result<Tools, Fail> {
        if !cfg!(target_arch = "aarch64") {
            return Err(not_run(
                "the lane needs Apple silicon (QEMU's hvf accelerator runs an arm64 Windows)",
            ));
        }
        Tools::find_in(&PREFIXES.map(Path::new))
    }

    /// [`Tools::find`] under `prefixes` (the first that has each tool), on any architecture.
    /// # Errors
    /// As [`Tools::find`], past its architecture check.
    pub fn find_in(prefixes: &[&Path]) -> Result<Tools, Fail> {
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
        },
        Err(e) => Out {
            code: None,
            text: format!("{} could not start: {e}", bin.display()),
            secs,
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

fn curl_file(url: &str, dest: &Path, browser: bool) -> Result<(), String> {
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
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
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
    let text = std::fs::read_to_string(iso_record(l)).ok()?;
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
    std::fs::write(iso_record(l), format!("{name}\n{want}\n"))
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

fn absent(pid: i32) -> bool {
    crate::sys::signal_probe(pid, 0).is_err_and(|e| e.raw_os_error() == Some(libc::ESRCH))
}

fn observe_daemon(l: &Layout, kind: Daemon, pid: i32) -> Result<Option<DaemonProcess>, String> {
    let before = aterm_uds::process::read_process_birth(pid);
    let argv = aterm_uds::process::read_process_argv(pid);
    let after = aterm_uds::process::read_process_birth(pid);
    if let Some(found) = argv
        .as_deref()
        .and_then(|a| captured_daemon(l, kind, pid, before, a, after))
    {
        return Ok(Some(found));
    }
    if absent(pid) {
        return Ok(None);
    }
    Err(format!(
        "{kind:?} pid {pid} has no stable identity and arguments for this VM; preserved"
    ))
}

fn daemon(l: &Layout, kind: Daemon) -> Result<Option<DaemonProcess>, String> {
    let file = kind.pid_file(l);
    let text = match std::fs::read_to_string(&file) {
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
    std::fs::read_to_string(l.port()).ok()?.trim().parse().ok()
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
    ] {
        let _ = std::fs::remove_file(f);
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
    std::fs::write(l.port(), format!("{}\n", boot.port))
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
    reader
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
            if reader
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
    let cmd = format!(
        "{{\"execute\":\"screendump\",\"arguments\":{{\"filename\":{},\"format\":\"png\"}}}}",
        aterm_json::to_string(&aterm_json::Value::String(lossy(&l.screen()))).unwrap_or_default()
    );
    let _ = qmp(l, &[cmd]);
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

    fn options(&self) -> Vec<String> {
        vec![
            "-F".into(),
            "/dev/null".into(),
            "-i".into(),
            lossy(&self.key),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            format!("UserKnownHostsFile={}", lossy(&self.known_hosts)),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            "ServerAliveInterval=15".into(),
            "-o".into(),
            "ServerAliveCountMax=8".into(),
            "-o".into(),
            "LogLevel=ERROR".into(),
        ]
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
                };
            }
        };
        let err_reader = child.stderr.take().map(|mut e| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = e.read_to_string(&mut s);
                s
            })
        });
        let mut text = String::new();
        if let Some(out) = child.stdout.take() {
            let stdout = std::io::stdout();
            for line in BufReader::new(out).split(b'\n') {
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

fn wait_ssh(l: &Layout, ssh: &Ssh, limit: Duration, what: &str) -> Result<f64, String> {
    let t0 = Instant::now();
    let mut said = Duration::ZERO;
    loop {
        if answers(ssh) {
            return Ok(t0.elapsed().as_secs_f64());
        }
        if running(l)?.is_none() {
            return Err(format!("QEMU exited while {what}"));
        }
        let waited = t0.elapsed();
        if waited > limit {
            screendump(l);
            return Err(format!(
                "the guest did not answer on SSH within {} min while {what}; its screen: {}",
                limit.as_secs() / 60,
                l.screen().display()
            ));
        }
        if waited >= said + Duration::from_secs(60) {
            said = waited;
            screendump(l);
            println!(
                "windows:   {what}: {} min (screen: {})",
                waited.as_secs() / 60,
                l.screen().display()
            );
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn stop(l: &Layout, ssh: Option<&Ssh>) -> Result<(), String> {
    let Some(process) = daemon(l, Daemon::Qemu)? else {
        return stop_swtpm(l);
    };
    let asked = ssh.is_some_and(|s| s.run("shutdown /s /t 0 /f").ok());
    if !asked {
        let _ = qmp(l, &["{\"execute\":\"system_powerdown\"}".to_string()]);
    }
    let t0 = Instant::now();
    while process.still_here(l, Daemon::Qemu)? && t0.elapsed() < STOP_WITHIN {
        std::thread::sleep(Duration::from_secs(2));
    }
    let mut forced = None;
    if process.still_here(l, Daemon::Qemu)? {
        screendump(l);
        let _ = qmp(l, &["{\"execute\":\"quit\"}".to_string()]);
        std::thread::sleep(Duration::from_secs(5));
        process.signal(l, Daemon::Qemu, libc::SIGKILL)?;
        forced = Some(format!(
            "Windows did not shut down within {} min; a stop was attempted only for its captured QEMU identity (screen: {}); state preserved",
            STOP_WITHIN.as_secs() / 60,
            l.screen().display()
        ));
    }
    stop_swtpm(l)?;
    forced.map_or(Ok(()), Err)
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name()))
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
    std::fs::copy(l.vars(), l.vars_saved()).map_err(|e| format!("{}: {e}", l.vars().display()))?;
    copy_dir(&l.tpm(), &l.tpm_saved())?;
    std::fs::write(l.provisioned(), format!("{fingerprint}\n")).map_err(|e| e.to_string())
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
    std::fs::copy(l.vars_saved(), l.vars())
        .map_err(|e| format!("{}: {e}", l.vars_saved().display()))?;
    copy_dir(&l.tpm_saved(), &l.tpm())
}

fn snapshot_plan(l: &Layout) -> Option<String> {
    std::fs::read_to_string(l.provisioned())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn secret_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
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
    std::fs::read_to_string(l.key().with_extension("pub"))
        .map_err(|e| format!("the lane's public key: {e}"))
}

fn lane_password(l: &Layout) -> Result<String, String> {
    if let Ok(p) = std::fs::read_to_string(l.password())
        && !p.trim().is_empty()
    {
        return Ok(p.trim().to_string());
    }
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
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
    std::fs::write(
        dir.join(guest::FIRST_LOGON_FILE),
        guest::first_logon_script(),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(dir.join("administrators_authorized_keys"), public_key)
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
    std::fs::write(drivers.join(NETKVM_INF), &inf).map_err(|e| e.to_string())?;
    for file in sources.iter().map(String::as_str).chain([NETKVM_CAT]) {
        let bytes = iso_member(virtio, &format!("{NETKVM_DIR}/{file}"))?;
        std::fs::write(drivers.join(file), bytes).map_err(|e| e.to_string())?;
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
    std::fs::set_permissions(l.media(), std::fs::Permissions::from_mode(0o600))
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
    std::fs::copy(&t.vars, l.vars()).map_err(|e| format!("{}: {e}", t.vars.display()))?;
    std::fs::set_permissions(l.vars(), std::fs::Permissions::from_mode(0o600))
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
    match wait_ssh(l, &ssh, INSTALL_WITHIN, "installing") {
        Ok(secs) => timings.push(("install and first logon".into(), secs)),
        Err(e) => {
            let _ = stop(l, None);
            return Err(not_run(e));
        }
    }
    let done = ssh.run(&format!("type {}", guest::FIRST_LOGON_DONE));
    if !done.ok() {
        let log = ssh.run(&format!("type {}", guest::FIRST_LOGON_LOG));
        let _ = stop(l, Some(&ssh));
        return Err(not_run(format!(
            "the first logon did not finish:\n{}",
            log.tail(20)
        )));
    }
    std::fs::write(l.installed(), "ok\n").map_err(|e| Fail::die(e.to_string()))?;
    Ok(ssh)
}

fn reboot(l: &Layout, ssh: &Ssh) -> Result<(), String> {
    if !ssh.run("shutdown /r /t 0 /f").ok() {
        return Err("`shutdown /r` failed".into());
    }
    let t0 = Instant::now();
    while answers(ssh) {
        if t0.elapsed() > GONE_WITHIN {
            return Err(format!(
                "Windows still answered on SSH {} min after `shutdown /r`",
                GONE_WITHIN.as_secs() / 60
            ));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    wait_ssh(l, ssh, BOOT_WITHIN, "restarting")?;
    wait_desktop(ssh)
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
        std::fs::write(&local, &bytes).map_err(|e| format!("{}: {e}", local.display()))?;
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

fn provision(l: &Layout, ssh: &Ssh) -> Result<Option<f64>, Fail> {
    let steps = guest::provision_steps();
    let want = guest::fingerprint(&steps);
    let have = ssh.run(&format!("type {}", guest::STAMP));
    if have.ok() && have.text.trim() == want {
        return Ok(None);
    }
    println!("windows: provisioning the guest (plan {want}) …");
    let t0 = Instant::now();
    stage_display_driver(l, ssh)
        .map_err(|e| not_run(format!("virtio-win's display driver did not copy: {e}")))?;
    let runner = ssh.runner();
    for s in &steps {
        println!("windows:   {} …", s.label);
        let by = if s.runner { &runner } else { ssh };
        let out = by.streamed(&guest::powershell_command(&s.script), "windows:     ");
        if !out.ok() {
            return Err(not_run(format!(
                "provisioning step '{}' failed:\n{}",
                s.label,
                out.tail(15)
            )));
        }
        if s.reboot {
            println!("windows:   restarting Windows …");
            reboot(l, ssh).map_err(|e| not_run(format!("after '{}': {e}", s.label)))?;
        }
    }
    let wrote = ssh.ps(&format!(
        "Set-Content -Path {} -Value {}",
        guest::ps_quote(guest::STAMP),
        guest::ps_quote(&want)
    ));
    if !wrote.ok() {
        return Err(not_run(format!(
            "could not write {}: {}",
            guest::STAMP,
            wrote.tail(5)
        )));
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
}

impl Up {
    /// Shut down and revert the VM when this run booted it.
    pub fn release(self) -> Result<(), String> {
        if self.booted_here {
            shut_and_revert(&self.layout, &self.tools, Some(&self.admin))?;
            println!("windows: VM stopped and reverted to its snapshot (this run booted it)");
        }
        Ok(())
    }
}

fn shut_and_revert(l: &Layout, t: &Tools, ssh: Option<&Ssh>) -> Result<(), String> {
    stop(l, ssh)?;
    if snapshot_plan(l).is_some() {
        revert(l, t)?;
    }
    Ok(())
}

fn boot(l: &Layout, t: &Tools, vnc: bool, timings: &mut Vec<(String, f64)>) -> Result<Ssh, Fail> {
    let port = free_port().map_err(not_run)?;
    let vnc = if vnc {
        Some(free_port().map_err(not_run)?.max(5900))
    } else {
        None
    };
    println!("windows: booting {VM} …");
    start(
        l,
        t,
        &Boot {
            install: None,
            port,
            vnc,
        },
    )
    .map_err(not_run)?;
    if let Some(p) = vnc {
        println!("windows: VNC on 127.0.0.1:{p}");
    }
    let ssh = Ssh::new(l, port);
    match wait_ssh(l, &ssh, BOOT_WITHIN, "booting") {
        Ok(secs) => timings.push(("boot".into(), secs)),
        Err(e) => {
            let _ = shut_and_revert(l, t, None);
            return Err(not_run(e));
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

/// Make the VM if there is none, boot it, and provision it when its snapshot is not the plan's.
/// # Errors
/// NOT RUN: a tool or an image missing, or the VM could not be made, booted or provisioned.
pub fn bring_up(vnc: bool) -> Result<Up, Fail> {
    let tools = Tools::find()?;
    let l = Layout::find()?;
    let mut timings = Vec::new();
    if let Some(pid) = running(&l).map_err(not_run)? {
        let port = read_port(&l).ok_or_else(|| not_run("the running VM recorded no SSH port"))?;
        let admin = Ssh::new(&l, port);
        let ssh = admin.runner();
        if !answers(&ssh) {
            return Err(not_run(format!(
                "the running VM (QEMU pid {pid}) does not answer on SSH as {}; `{LAB} windows down`",
                guest::RUNNER
            )));
        }
        end_leftovers(&admin);
        return Ok(Up {
            layout: l,
            tools,
            ssh,
            admin,
            booted_here: false,
            timings,
        });
    }
    stop_swtpm(&l).map_err(not_run)?;
    let want = guest::fingerprint(&guest::provision_steps());
    if snapshot_plan(&l).as_deref() != Some(want.as_str()) {
        let ssh = if l.installed().is_file() && l.disk().is_file() {
            boot(&l, &tools, false, &mut timings)?
        } else {
            install(&l, &tools, &mut timings)?
        };
        match provision(&l, &ssh) {
            Ok(Some(secs)) => timings.push(("provision".into(), secs)),
            Ok(None) => {}
            Err(f) => {
                let _ = shut_and_revert(&l, &tools, Some(&ssh));
                return Err(f);
            }
        }
        let t0 = Instant::now();
        stop(&l, Some(&ssh)).map_err(not_run)?;
        timings.push(("shut down".into(), t0.elapsed().as_secs_f64()));
        let t0 = Instant::now();
        snapshot(&l, &tools, &want).map_err(not_run)?;
        timings.push(("snapshot".into(), t0.elapsed().as_secs_f64()));
    }
    let admin = boot(&l, &tools, vnc, &mut timings)?;
    let ssh = admin.runner();
    if !answers(&ssh) {
        let _ = shut_and_revert(&l, &tools, Some(&admin));
        return Err(not_run(format!(
            "the guest answers its administrator but not {} on SSH (made by the first logon: `{LAB} windows destroy` \
             and `up` again)",
            guest::RUNNER
        )));
    }
    Ok(Up {
        layout: l,
        tools,
        ssh,
        admin,
        booted_here: true,
        timings,
    })
}

/// The one lane run of this Mac.
/// # Errors
/// Another run holds it (NOT RUN), or the lock file cannot be made.
pub fn lane_lock(l: &Layout) -> Result<LaneLock, Fail> {
    std::fs::create_dir_all(&l.home)
        .map_err(|e| Fail::die(format!("{}: {e}", l.home.display())))?;
    let _ = std::fs::set_permissions(&l.home, std::fs::Permissions::from_mode(0o700));
    LaneLock::at(&l.lock(), "windows")
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
        std::fs::write(&list, send.join("\0")).map_err(|e| e.to_string())?;
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
        let gone = run_dir.join("copy.remove");
        std::fs::write(&gone, remove.join("\n")).map_err(|e| e.to_string())?;
        let put = ssh.put(&gone, "C:/lane/copy.remove");
        let out = ssh.ps(&format!(
            "Get-Content -LiteralPath 'C:\\lane\\copy.remove' | ForEach-Object {{ \
             Remove-Item -LiteralPath (Join-Path {} $_) -Force -ErrorAction SilentlyContinue }}",
            guest::ps_quote(guest::SRC)
        ));
        if !put.ok() || !out.ok() {
            return Err(format!("removals failed: {}{}", put.tail(3), out.tail(3)));
        }
    }
    let m = run_dir.join("manifest");
    std::fs::write(&m, manifest(&here)).map_err(|e| e.to_string())?;
    let put = ssh.put(&m, "C:/lane/manifest");
    if !put.ok() {
        return Err(format!("the manifest did not copy: {}", put.tail(3)));
    }
    Ok((send.len(), remove.len()))
}

/// The build facts `crates/aterm/build.rs` takes pinned instead of asking git.
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
    let dirty = git(&["status", "--porcelain"]).is_some();
    let dev = git(&["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"])
        .and_then(|tag| git(&["rev-list", "--count", &format!("{tag}..HEAD")]))
        .unwrap_or_else(|| "0".into());
    let mut facts = Vec::new();
    if let Some(epoch) = git(&["show", "-s", "--format=%ct", "HEAD"]) {
        facts.push(("SOURCE_DATE_EPOCH".to_string(), epoch));
    }
    facts.extend([
        (
            "ATERM_BUILD_GIT_COMMIT".into(),
            if dirty {
                format!("{short}-dirty")
            } else {
                short
            },
        ),
        ("ATERM_BUILD_GIT_COMMIT_FULL".into(), full),
        ("ATERM_BUILD_DEV_COMMITS".into(), dev),
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
    let _lock = lane_lock(&l)?;
    if vnc && running(&l).map_err(Fail::die)?.is_some() {
        return Err(Fail::usage(
            "windows up --vnc: the VM is already running without VNC; `windows down` first",
        ));
    }
    let up = bring_up(vnc)?;
    for (name, secs) in &up.timings {
        println!("windows: {name} {secs:.1} s");
    }
    println!(
        "windows: {VM} is up (SSH on 127.0.0.1:{}); `{LAB} windows exec -- CMD` runs in it, `{LAB} windows down` stops it",
        up.ssh.port
    );
    Ok(0)
}

fn cmd_down() -> Res {
    let tools = Tools::find()?;
    let l = Layout::find()?;
    let _lock = lane_lock(&l)?;
    if running(&l).map_err(Fail::die)?.is_none() {
        stop_swtpm(&l).map_err(Fail::die)?;
        println!("windows: {VM} is not running");
        return Ok(0);
    }
    let ssh = read_port(&l).map(|p| Ssh::new(&l, p));
    shut_and_revert(&l, &tools, ssh.as_ref()).map_err(Fail::die)?;
    println!("windows: {VM} stopped and reverted to its snapshot");
    Ok(0)
}

fn cmd_destroy() -> Res {
    let l = Layout::find()?;
    let _lock = lane_lock(&l)?;
    let ssh = read_port(&l).map(|p| Ssh::new(&l, p));
    stop(&l, ssh.as_ref()).map_err(Fail::die)?;
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
    println!(
        "vm        {VM} {} — {CPUS} CPUs, {MEMORY_GIB} GiB, disk <= {DISK_GIB} GiB",
        running(&l).map_err(Fail::die)?.map_or_else(
            || "stopped".to_string(),
            |pid| format!("running (QEMU pid {pid})")
        )
    );
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
        }
    };
    let run_dir = up.layout.run();
    let tasks = guest::TASKS.replace('\\', "/");
    for (ext, text) in [
        ("cmd", guest::desktop_batch(line, name)),
        ("ps1", guest::desktop_runner(name, how)),
    ] {
        let local = run_dir.join(format!("{name}.{ext}"));
        if let Err(e) = std::fs::write(&local, text) {
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
    let _lock = lane_lock(&l)?;
    let up = bring_up(false)?;
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
    let mut env = guest::cargo_env(ask.arch);
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
mod tests {
    use super::*;

    /// A missing QEMU or swtpm is named with where the lane looked, never with an install
    /// command: aterm provides neither (init design §6.1 row 26). The lookup's own NOT RUN
    /// says it, over scratch prefixes: no QEMU in either, then QEMU in the second and no
    /// swtpm. NEGATIVE CONTROL: with both found, the lookup goes on to the firmware, which
    /// the scratch prefix lacks, and runs nothing.
    #[cfg(unix)]
    #[test]
    fn a_missing_qemu_or_swtpm_is_named_without_an_install_command() {
        use std::os::unix::fs::PermissionsExt as _;
        let default: Vec<String> = PREFIXES.iter().map(|p| format!("{p}/bin")).collect();
        for (bin, what) in [("qemu-system-aarch64", "QEMU"), ("swtpm", "swtpm")] {
            let why = missing(bin, what, &PREFIXES.map(Path::new));
            assert!(
                why.starts_with(&format!("no {bin} in {}: ", default.join(" or ")))
                    && why.contains("aterm provides none"),
                "{why}"
            );
            assert!(
                !why.contains("install"),
                "no remedy installs anything: {why}"
            );
        }
        let tmp = aterm_tempfile::tempdir().unwrap();
        let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
        let prefixes = [one.as_path(), two.as_path()];
        let put = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"#!/bin/sh\nexit 9\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let refusal = |prefixes: &[&Path]| match Tools::find_in(prefixes) {
            Err(fail) => (fail.code, fail.msg.unwrap_or_default()),
            Ok(tools) => panic!("{tools:?}"),
        };
        let looked = format!(
            "{} or {}",
            one.join("bin").display(),
            two.join("bin").display()
        );
        let (code, msg) = refusal(&prefixes);
        assert_eq!(code, NOT_RUN);
        assert_eq!(
            msg,
            format!(
                "NOT RUN: {}",
                missing("qemu-system-aarch64", "QEMU", &prefixes)
            )
        );
        assert!(msg.contains(&looked) && !msg.contains("brew"), "{msg}");
        put(&two.join("bin/qemu-system-aarch64"));
        let (_, msg) = refusal(&prefixes);
        assert_eq!(
            msg,
            format!("NOT RUN: {}", missing("swtpm", "swtpm", &prefixes))
        );
        assert!(!msg.contains("brew"), "{msg}");
        put(&one.join("bin/swtpm"));
        let (code, msg) = refusal(&prefixes);
        assert_eq!(code, NOT_RUN);
        assert!(
            msg.starts_with("NOT RUN: QEMU's firmware ")
                && msg.contains(&two.join("share/qemu").display().to_string()),
            "{msg}"
        );
    }

    #[test]
    fn the_page_names_its_edition_and_each_isos_digest() {
        let page = "<select><option value=\"\" selected>Select</option>\
                    <option value=\"3816\">Windows 11 (multi-edition ISO for Arm64)</option></select>\
                    <table><tr><td>English International 64-bit</td><td>500BCB6F272B7C455156470EA06816F558F13B885723F84EE9EAB7534C7DEE61</td></tr>\
                    <tr><td>English 64-bit</td>\n  <td>8992F2D2CFD2FA647E579E9EB31FC21F34DA8333C29B6170FDE17D2ED6623E9C</td></tr></table>";
        assert_eq!(edition_id(page).as_deref(), Some("3816"));
        assert_eq!(
            published_sha256(page, ISO_DIGEST_ROW).as_deref(),
            Some("8992f2d2cfd2fa647e579e9eb31fc21f34da8333c29b6170fde17d2ed6623e9c")
        );
        assert_eq!(
            published_sha256("<td>English 64-bit</td><td>soon</td>", ISO_DIGEST_ROW),
            None
        );
        assert_eq!(edition_id("<option value=\"12\">Other</option>"), None);
    }

    #[test]
    fn the_fingerprint_request_is_read_from_mdt_js() {
        let js = "window.dfp={};i=\"https://ov-df.microsoft.com/?session_id=s&CustomerId=c&PageId=si&w=8DF2\";\
                  i+=\"&mdt=\"+r;i+=\"&rticks=\"+1792536204805;false&&(i+=1)";
        assert_eq!(
            fingerprint_request(js),
            Some((
                "https://ov-df.microsoft.com/?session_id=s&CustomerId=c&PageId=si&w=8DF2".into(),
                "1792536204805".into()
            ))
        );
        assert_eq!(fingerprint_request("nothing here"), None);
    }

    #[test]
    fn the_download_service_answers_a_sku_and_a_link_or_its_error() {
        let skus =
            r#"{"Skus":[{"Id":"27163","Language":"Arabic"},{"Id":"27162","Language":"English"}]}"#;
        assert_eq!(sku_id(skus, "English").as_deref(), Some("27162"));
        assert_eq!(sku_id(skus, "Klingon"), None);
        let link = r#"{"ProductDownloadOptions":[{"Uri":"https://software.download.prss.microsoft.com/dbazure/Win11_arm64.iso?t=x&P1=2"}]}"#;
        assert_eq!(
            download_link(link).as_deref(),
            Ok("https://software.download.prss.microsoft.com/dbazure/Win11_arm64.iso?t=x&P1=2")
        );
        let refused = r#"{"Errors":[{"Key":"ErrorSettings.SentinelReject","Value":"Sentinel marked this request as rejected.","Type":8}]}"#;
        assert_eq!(
            download_link(refused),
            Err("Sentinel marked this request as rejected.".into())
        );
        assert_eq!(
            iso_name("https://x/y/Win11_arm64.iso?t=1").as_deref(),
            Some("Win11_arm64.iso")
        );
        assert_eq!(iso_name("https://x/y/../evil?t=1"), None);
        assert_eq!(iso_name("https://x/y/a b.iso"), None);
    }

    fn layout() -> Layout {
        Layout::under(Path::new("/Users//someone"))
    }

    fn tools() -> Tools {
        Tools {
            qemu: "/opt/homebrew/bin/qemu-system-aarch64".into(),
            qemu_img: "/opt/homebrew/bin/qemu-img".into(),
            swtpm: "/opt/homebrew/bin/swtpm".into(),
            code: "/opt/homebrew/share/qemu/edk2-aarch64-code.fd".into(),
            vars: "/opt/homebrew/share/qemu/edk2-arm-vars.fd".into(),
            tpm_ppi: false,
        }
    }

    #[test]
    fn a_download_makes_the_cache_folder_a_first_up_has_not_got() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let source = root.path().join("source.iso");
        std::fs::write(&source, b"the image").expect("source");
        let dest = root.path().join("Library/Caches/aterm/windows-lane/w.iso");
        let url = format!("file://{}", source.display());
        assert_eq!(curl_file(&url, &dest, false), Ok(()));
        assert_eq!(std::fs::read(&dest).expect("downloaded"), b"the image");
        assert!(!dest.with_extension("part").exists());
    }

    #[test]
    fn the_vm_listens_on_loopback_alone_and_shows_nothing() {
        let l = layout();
        let a = qemu_args(
            &l,
            &tools(),
            &Boot {
                install: None,
                port: 52222,
                vnc: None,
            },
        );
        let joined = a.join(" ");
        assert!(joined.contains("hostfwd=tcp:127.0.0.1:52222-:22"));
        assert!(joined.contains("-display none"));
        assert!(!joined.contains("-vnc"));
        assert!(joined.contains("-machine virt,accel=hvf -cpu host -smp 6 -m 8G"));
        assert!(!joined.contains("id=install"));
        assert!(
            joined.contains(
                "-daemonize -pidfile /Users//someone/.aterm/lab/windows-lane/run/qemu.pid"
            )
        );
        assert!(joined.contains("-device tpm-tis-device,tpmdev=tpm0 "));
        let ppi = Tools {
            tpm_ppi: true,
            ..tools()
        };
        let boot = Boot {
            install: None,
            port: 1,
            vnc: None,
        };
        assert!(
            qemu_args(&l, &ppi, &boot)
                .join(" ")
                .contains("-device tpm-tis-device,tpmdev=tpm0,ppi=off "),
            "a QEMU with the switch turns the TPM's physical-presence RAM off"
        );
        let with = qemu_args(
            &l,
            &tools(),
            &Boot {
                install: Some(("/c/win.iso".into(), l.media())),
                port: 1,
                vnc: Some(5901),
            },
        )
        .join(" ");
        assert!(with.contains("-vnc 127.0.0.1:1"));
        assert!(with.contains("file=/c/win.iso -device usb-storage,drive=install,bootindex=0"));
        assert!(with.contains("usb-storage,drive=answers"));
        assert!(lossy(&l.qmp()).len() < 104);
        assert!(lossy(&l.tpm_sock()).len() < 104);
        assert!(swtpm_args(&l).join(" ").contains("--terminate"));
        let birth = aterm_uds::process::ProcessBirth {
            seconds: 12,
            microseconds: 34,
        };
        let later = aterm_uds::process::ProcessBirth {
            seconds: 12,
            microseconds: 35,
        };
        let argv = |bin: &Path, args: &[String]| {
            std::iter::once(bin.as_os_str().to_os_string())
                .chain(args.iter().map(OsString::from))
                .collect::<Vec<_>>()
        };
        let qa = argv(&tools().qemu, &a);
        let found = captured_daemon(&l, Daemon::Qemu, 123, Some(birth), &qa, Some(birth));
        assert_eq!(found, Some(DaemonProcess { pid: 123, birth }));
        assert_eq!(
            captured_daemon(&l, Daemon::Qemu, 123, Some(birth), &qa, Some(later)),
            None
        );
        assert_eq!(
            captured_daemon(&l, Daemon::Qemu, 123, None, &qa, None),
            None
        );
        assert_eq!(
            captured_daemon(&l, Daemon::Qemu, 1, Some(birth), &qa, Some(birth)),
            None
        );
        let foreign = Layout::under(Path::new("/Users//another"));
        assert_eq!(
            captured_daemon(&foreign, Daemon::Qemu, 123, Some(birth), &qa, Some(birth)),
            None
        );
        let mut wrong = qa.clone();
        let qmp = wrong
            .iter()
            .position(|w| w == OsStr::new("-qmp"))
            .expect("QMP argument");
        wrong[qmp + 1] = OsString::from("unix:/another/qmp.sock,server=on,wait=off");
        assert_eq!(
            captured_daemon(&l, Daemon::Qemu, 123, Some(birth), &wrong, Some(birth)),
            None
        );
        let ta = argv(&tools().swtpm, &swtpm_args(&l));
        assert!(captured_daemon(&l, Daemon::Tpm, 124, Some(birth), &ta, Some(birth)).is_some());
        assert_eq!(
            captured_daemon(&foreign, Daemon::Tpm, 124, Some(birth), &ta, Some(birth)),
            None
        );
        assert_eq!(
            captured_daemon(&l, Daemon::Tpm, 124, Some(birth), &ta, Some(later)),
            None
        );
    }

    #[test]
    fn a_copy_sends_what_changed_and_removes_what_went() {
        let e = |p: &str, size: u64, t: &str| Entry {
            path: p.into(),
            size,
            mtime: t.into(),
        };
        let here = vec![
            e("a.rs", 1, "10.000000000"),
            e("b.rs", 2, "20.000000000"),
            e("d/c.rs", 3, "30.5"),
        ];
        let there = parse_manifest(&manifest(&[
            e("a.rs", 1, "10.000000000"),
            e("b.rs", 2, "19.000000000"),
            e("gone.rs", 9, "1.0"),
        ]));
        let (send, remove) = plan_copy(&here, &there);
        assert_eq!(send, vec!["b.rs".to_string(), "d/c.rs".to_string()]);
        assert_eq!(remove, vec!["gone.rs".to_string()]);
        assert_eq!(plan_copy(&here, &BTreeMap::new()).0.len(), 3);
        assert_eq!(
            parse_manifest("1 10.0 a b.rs\r\n"),
            BTreeMap::from([("a b.rs".to_string(), (1, "10.0".to_string()))])
        );
    }

    #[test]
    fn an_inf_installs_what_its_source_disks_files_name() {
        let inf = "[Version]\r\nSignature = \"$WINDOWS NT$\"\r\n\r\n[SourceDisksFiles]\r\n\
                   netkvm.sys  = 1,,\r\nnetkvmp.exe = 1,, ; the config tool\r\n\r\n\
                   [SourceDisksFiles.arm64]\r\nother.sys = 1\r\n[Strings]\r\nx = y\r\n";
        assert_eq!(
            inf_sources(inf),
            ["netkvm.sys", "netkvmp.exe", "other.sys"].map(String::from)
        );
        assert!(inf_sources("[Strings]\nnetkvm.sys = 1\n").is_empty());
    }

    #[test]
    fn a_hard_linked_iso_member_is_read_through_its_target() {
        assert_eq!(
            link_target(
                "-r-xr-xr-x  2 0  0  0 Jul 22 17:49 NetKVM/w11/ARM64/netkvm.inf link to NetKVM/2k25/ARM64/netkvm.inf\n"
            ),
            Some("NetKVM/2k25/ARM64/netkvm.inf")
        );
        assert_eq!(
            link_target("-r-xr-xr-x  1 0  0  18403 Jul 22 17:48 NetKVM/w10/ARM64/netkvm.inf\n"),
            None
        );
    }

    #[test]
    fn exec_takes_its_arch_and_then_the_command() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let a = parse_exec(&s(&["--arch", "x64", "--", "cargo", "--arch"]))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(a.arch, Arch::X86_64);
        assert!(!a.desktop);
        assert_eq!(a.argv, s(&["cargo", "--arch"]));
        assert_eq!(
            parse_exec(&s(&["cargo"])).map(|a| a.arch).ok(),
            Some(Arch::Aarch64)
        );
        let d = parse_exec(&s(&["--desktop", "--arch", "x86_64", "cargo", "--desktop"]))
            .unwrap_or_else(|_| unreachable!());
        assert!(d.desktop);
        assert_eq!(d.arch, Arch::X86_64);
        assert_eq!(d.argv, s(&["cargo", "--desktop"]));
        assert!(parse_exec(&s(&["--arch", "mips", "x"])).is_err());
        assert!(parse_exec(&s(&["--"])).is_err());
        assert_eq!(released(0, Ok(())).ok(), Some(0));
        assert_eq!(released(7, Ok(())).ok(), Some(7));
        let failed = released(0, Err("snapshot restore failed".into()))
            .expect_err("cleanup is part of the command result");
        assert_eq!(failed.code, 1);
        assert_eq!(failed.msg.as_deref(), Some("snapshot restore failed"));
    }
}
