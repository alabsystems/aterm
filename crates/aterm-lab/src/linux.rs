// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux runtime lane on macOS: a VM reached through a [`Driver`] (lima's `limactl` by
//! default; the lane's own VM through `aterm-vz-host` under `LAB_LINUX_DRIVER=vz`).

use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use aterm_toolchain::named_lock::{self, Refused, Wait};
use aterm_uds::spawnfd::Stdio;
use atpkg::admit_disk::Admit;

use crate::linux_driver::{Driver, Kind};
use crate::linux_guest::{self as guest, Arch, Display};
use crate::linux_owned as owned;
use crate::paths::paths;
use crate::{Fail, LAB, Res};

/// The VM's lima name.
pub const VM: &str = "aterm-linux";

/// What the VM is made with (`limactl create`), the same size on every host.
pub const CPUS: u32 = 8;
pub const MEMORY_GIB: u32 = 16;
pub const DISK_GIB: u32 = 60;

/// lima's template for the guest image (the image digest is pinned by the lima release that ships the template).
pub const TEMPLATE: &str = "template:ubuntu-24.04";

/// The VZ guest's build directory under its user's home. Lima reserves a distinct guest directory and retains its creation receipt instead.
pub const GUEST_DIR: &str = "aterm-lane";

/// The file in the selected guest build root that names the checkout whose output it
/// holds. Cargo hashes a path package by its path inside the workspace and
/// keeps its dep-info relative to it, so the output another checkout left
/// (`--keep-build`, or an `up` never taken `down`) reads as fresh to this
/// one's build, which then links that checkout's units.
pub const BUILT_FROM: &str = "built-from";

/// Exit status of a lane that could not run (no lima, no VM, no display): never a pass.
pub const NOT_RUN: i32 = 3;

/// The verbs, as `linux help` prints them.
pub const USAGE: &str = "\
targo --unverified lab linux [--driver lima|vz] [--gpu] [--no-wait] [--admit wait|now] <verb> — the Linux runtime lane: an
Ubuntu 24.04 aarch64 VM (Rosetta for x86_64), lima's by default; --driver vz or LAB_LINUX_DRIVER=vz runs every verb on the
lane's own VM instead (aterm-vz-host, the guest agent, the state in LAB_VZ_STATE or ~/Library/Caches/aterm/vz.noindex; it
needs a console session, since the framework's VM process opens a WindowServer connection at each boot); --gpu runs every
verb on the lane's GPU instance instead, lima's aterm-linux-gpu on krunkit (krunkit from Homebrew's slp/krun tap,
KosmicKrisp placed by `gpu-host`): the lane's provisioning for aarch64 (krunkit has no Rosetta, so --arch x86_64 is
refused), then the mainline 7.2.6 kernel and Mesa 26.3's Venus, and before any verb runs the guest's Vulkan must be Venus on
this Mac's GPU and its GL zink over it (else NOT RUN); cargo and aterm-gui run with Venus as the only Vulkan driver and zink
as GL (the display servers stay on llvmpipe). One run holds a VM at a time: a verb that finds it held waits for that run to
end, naming it (pid, verb, checkout, since when) as the wait starts and once a minute; --no-wait answers NOT RUN at once
instead, naming it. A verb that boots the VM first claims the disk its boots write (what the last ones wrote, else 10 GiB
less what the VM's directory holds) and waits for room before it waits for the lane, a line a minute naming the need and the
holders; once it holds the lane, the boot claims the guest's 16 GiB and 8 vCPUs in this Mac's capacity ledger (`aterm pkg
capacity`) and waits while they do not fit with 8 GiB to spare beside them, naming the claims that hold the room once a
minute (NOT RUN at once only on a Mac too small for them); --admit now boots at once, saying what it lacks or would have
waited for. The guest's builds run as many jobs as cores the claim was granted (CARGO_BUILD_JOBS). A lima VM still running
after the one-shot run that booted it died is stopped as `down` stops it, and booted afresh; one an `up` or a --keep-up run
left is adopted. Lima uses ~/Library/Caches/aterm/lima.noindex (or explicit LIMA_HOME), with creation
receipts for its namespace and each VM. An existing namespace or VM without matching receipts is
retained and refused. The owner's default ~/.lima is never adopted. Lima build output uses a
reserved guest directory; historical ~/aterm-lane directories are retained.
  up                       make the VM on first use, mount this checkout read-only, boot, provision; leaves it running
  down                     remove the guest's build output and stop the VM (it is kept; the next run boots it in ~10 s)
  status                   lima, the VM, its mount, provisioning and disk use (guest and host)
  exec [--arch aarch64|x86_64] [--display xvfb|weston|mutter|xwayland|weston-x11] [--] CMD...
                           run CMD in the guest at this checkout's path with the arch's cargo environment (stock
                           Rust {rust}; x86_64: CARGO_BUILD_TARGET and the cross linker, run under Rosetta), under
                           the display when one is named, with the session bus and at-spi2-core up; the build
                           output is kept for the next `exec` under `up`, else removed when CMD ends
  proof [--arch A]... [--display D]... [--weston-renderer gl|pixman] [--keep-build] [--keep-up]
                           the lane's proof: aterm-uds, aterm-pty and aterm-libc's tests, aterm-render's
                           library tests, the libc oracle's conformance cells, and the windowing crates' and
                           the winit fork's tests with the displays up, on each arch; then aterm-gui under each
                           display (and --headless):
                           a session over its control socket, typed, resized, `status` read, a frame kept;
                           then, under xvfb and weston, the display killed under a running aterm-gui, and a
                           person's quit as their control (`gone`); then the accessibility runtime (`a11y`);
                           then the stats Deck's layout rows (`deck`); then LW.4's baseline replayed (below)
                           under every screen of those displays (weston's include weston-x11, the same weston on
                           an Xvfb window). Every session but the Deck's holds the Deck off ([sys] stats_bars =
                           \"off\"): its figures redraw every second, which races a frame held to the server's.
                           Exit 0 pass, 1 a row failed, 3 NOT RUN. Report and frames: $LAB_STATE/linux/
  gone [--arch A]... [--display D]... [--weston-renderer gl|pixman] [--keep-build] [--keep-up]
                           the proof's display-gone rows alone (xvfb and weston unless named): the display's
                           servers killed with SIGKILL under a running aterm-gui, at rest, mid-resize (the
                           server frozen until aterm's requests wait in it) and, on X11, mid-capture (mid-resize
                           on a turn that owes the crash journal a capture, which asks the window's position),
                           after which aterm-gui must exit by
                           itself, nonzero and without a panic, say that its display connection ended, and
                           leave session.toml holding its tab's folder and no crash journal; and a person's
                           quit (`invoke Quit`, the display up), which must exit 0, say nothing about the
                           display and save the same layout
  cpu [--arch A]... [--display D]... [--weston-renderer gl|pixman] [--keep-build] [--keep-up]
                           the proof's CPU presenter rows alone (every display unless named): aterm-gui --cpu
                           typed, resized and photographed twice (after the resize, and after one more line typed),
                           each frame held to the server's own pixels under xvfb, xvfb without MIT-SHM, xwayland
                           (lane-bus xgrab), weston (weston-screenshooter, weston started with --debug) and mutter
                           (lane-bus cast: a ScreenCast frame of its monitor through PipeWire, the frame found on
                           it); --weston-renderer pixman starts every weston of the run (proof, gone, cpu, deck)
                           with its pixman renderer, the VM and kiosk case, instead of GL
  deck [--arch A]... [--display D]... [--weston-renderer gl|pixman] [--keep-build] [--keep-up]
                           the proof's stats Deck rows alone, one per display family (xvfb, weston, mutter,
                           xwayland, weston-x11; weston's include weston-x11), aterm-gui with the Deck on, on the
                           GPU and with --cpu, its layout read and never its figures: the Deck's height in the
                           head and its place at the head's top, the head less the Deck with the Deck turned off,
                           the strip band under the Deck pixel-identical to the band without it, its buttons
                           drawn, a click on the strip under the Deck selecting a tab, maximize and restore
                           clicked where aterm draws the frame, and a window not whole cells (maximized, or sized
                           by xdotool on X11) held to the server's pixels with its remainder bands continuing the
                           Deck's rows, on the CPU
  a11y [--arch A]... [--keep-build] [--keep-up]
                           the proof's accessibility runtime rows alone: aterm-dbus, aterm-update-core and
                           aterm-a11y's tests with every ignored one; aterm-a11y's live_atspi bench on at-spi2-core
                           (an AT reads a window; its bus killed, and frozen with and without the write deadline);
                           aterm-gui's a11y suites and its Settings portal read (xdg-desktop-portal); then under xvfb the
                           accessibility bus's dbus-daemon killed (SIGKILL) and frozen (SIGSTOP, a flood on
                           screen) under a running aterm-gui, which must say so, rejoin and be read again with
                           no panic; and Orca (speech-dispatcher's dummy module, whose log of queued messages is
                           the sink) under xvfb, and under headless mutter with aterm-gui on its Xwayland and as
                           a Wayland client (keys from the RemoteDesktop seat), which must speak the frame, the
                           terminal's text and a typed line's output; as a Wayland client, Orca hears no key,
                           and an output it leaves unspoken must be for want of a cause, as its debug log says
  edges [--arch A]... [--edge E]... [--keep-build] [--keep-up]
                           the proof's rows for the edges LW.5-LW.7 left untested, each beside its control:
                           a ~/.Xresources whose #include sets Xft.dpi (no RESOURCE_MANAGER); an X server
                           without RANDR (xvfb -extension RANDR), XInput 2 or XKB (lane-bus xhide, a proxy that
                           answers them absent), which aterm-gui must refuse by name, exit 1, no panic, and one
                           with XInput 1 alone (the proxy refusing XIQueryVersion as such a server does); under
                           sway: server-side decorations, cursor-shape-v1 over a virtual pointer (lane-wl), an
                           output plugged and unplugged, a seat's pointer and a second seat, wl_compositor 5 with
                           no fractional scale (lane-bus wlhide), window_padding = 0 under the pointer, the
                           shadow margin outside a client-decorated window (grim's pixels); and under mutter,
                           the shadow margin's resize handle (a seat's press on it and past it), fullscreen at
                           tab_strip_rows = 0 and GNOME's button-layout through the Settings portal (both ends,
                           GNOME's default, and no portal); and under weston's pixman renderer, aterm-gui --cpu
                           resized seven times, each frame held to weston's output and weston alive after each
                           (pixman whatever a run names: edges takes no --weston-renderer)
  baseline [--arch A]... [--screen S]... [--record] [--keep-build] [--keep-up]
                           record a scripted aterm-gui session under each screen (xvfb, xvfb-dpi144, weston,
                           weston-scale2, mutter, mutter-scale1.5, xwayland, weston-x11): dims and scale, resizes
                           and cast drift, clipboard and primary, key repeat, compose, XIM (a dead key, and a
                           pinyin composition through ibus-libpinyin: its preedit, spot and commit) and an XDND
                           drop (X11), a Wayland drop and the frame's gestures (a seat: Mutter's RemoteDesktop, or
                           XTEST into weston's x11 backend), aterm-gui --cpu typed and resized on weston-x11's seat
                           with each frame held to weston's output, the AT-SPI tree and event stream, present
                           percentiles and the item the slowest present ended in, and the stack under it; replay
                           it against the recordings committed in crates/aterm-lab/linux-baseline/ (exit 0 same
                           behaviour, 1 it moved, 3 NOT RUN), or with --record write them there
  browser [--arch A]... [--keep-build] [--keep-up]
                           the browser lane in the guest: both web modules built and bound on this Mac (`verify
                           browser-site`), then on each arch aterm-verify built in the guest, its pinned browsers
                           fetched and `verify browser-drive` run on that site, every GPU frame held to the CPU
                           frames and the wgpu-WebGL recording within the CPU tolerance: x86_64 (Rosetta)
                           chrome-headless-shell linux64 on SwiftShader, aarch64 Firefox under Xvfb.
                           Exit 0 pass, 1 a row failed, 3 NOT RUN. Report and frames: $LAB_STATE/linux/
  vz-agent                 cross-build the vz guest agent (crates/aterm-vz-agent; STOCK EXCEPTION: rustup stable,
                           static aarch64-unknown-linux-musl, linked by its rust-lld) and place it at
                           ~/Library/Caches/aterm/vz.noindex/agent/<sha256>/; prints the sha256. NOT RUN without
                           the musl std (targo --unverified verify provision --linux)
  vz-host [place]          build the vz helper (crates/aterm-vz's aterm-vz-host, the dev profile) and place its copy,
                           signed ad hoc with com.apple.security.virtualization only, under the vz state's host/<sha256>/;
                           prints the sha256
  vz-smoke [--keep-up]     the vz arm end to end, whatever LAB_LINUX_DRIVER says: the bring-up (a bake where no baked
                           disk is sealed: the seed's first boot, provisioning through the agent, cloud-init disabled,
                           a trim, the power-off and the seal, then a clone), each Exec outcome kept apart (exit,
                           signal, not started), stdin, the merged order, the lane user's environment, copy_out,
                           copy_dir_out and copy_dir_in, the read-only checkout, a share swap and rebind, and Rosetta's
                           registration and translation cache. Exit 0 pass, 1 a check failed, 3 NOT RUN
  gpu-host                 place the LunarG Vulkan SDK's loader and KosmicKrisp (1.4.363.0, held to its sha256, installed
                           headless into a scratch root that is then removed) under ~/Library/Caches/aterm/tooling.noindex,
                           where --gpu's krunkit loads them in place of MoltenVK, which lacks zink's nullDescriptor
  clean                    remove the guest's build output and trim the VM disk (its blocks go back to this Mac)
  destroy                  delete the VM and its disk (`limactl delete`; vz: Stop, then the live and baked disks, base
                           kept); the next `up` makes it again
";

/// `linux <verb> …`.
/// # Errors
/// A usage error, or a step that failed.
pub fn cmd_linux(args: &[String]) -> Res {
    let mut args = args;
    let mut wait = Wait::Block;
    let mut admit = crate::admission::mode();
    loop {
        if let Some((mode, used)) = admit_flag("linux", args)? {
            admit = mode;
            args = &args[used..];
            continue;
        }
        match args {
            [flag, word, rest @ ..] if flag == "--driver" => {
                crate::linux_driver::choose(word)?;
                args = rest;
            }
            [flag] if flag == "--driver" => {
                return Err(Fail::usage(format!(
                    "linux --driver needs lima or vz ({LAB} linux help)"
                )));
            }
            [flag, rest @ ..] if flag == "--gpu" => {
                crate::linux_gpu::choose();
                args = rest;
            }
            [flag, rest @ ..] if flag == "--no-wait" => {
                wait = Wait::No;
                args = rest;
            }
            _ => break,
        }
    }
    if crate::linux_gpu::on() && crate::linux_driver::kind()? == Kind::Vz {
        return Err(Fail::usage(
            "linux --gpu: the GPU instance is lima's (krunkit); the vz arm has no GPU",
        ));
    }
    let verb = args.first().map_or("help", String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    let gpu = if crate::linux_gpu::on() { " --gpu" } else { "" };
    name_run(format!("lab linux{gpu} {verb}"), wait, admit);
    match verb {
        "up" => cmd_up(),
        "down" => cmd_down(),
        "status" => cmd_status(),
        "exec" => cmd_exec(rest),
        "proof" => crate::linux_proof::cmd_proof(rest),
        "baseline" => crate::linux_baseline::cmd_baseline(rest),
        "gone" => crate::linux_proof::cmd_gone(rest),
        "cpu" => crate::linux_proof::cmd_cpu(rest),
        "a11y" => crate::linux_proof::cmd_a11y(rest),
        "edges" => crate::linux_proof::cmd_edges(rest),
        "deck" => crate::linux_proof::cmd_deck(rest),
        "browser" => crate::linux_browser::cmd_browser(rest),
        "vz-agent" => crate::linux_vz::cmd_vz_agent(rest),
        "vz-host" => crate::linux_vz_host::cmd_vz_host(rest),
        "vz-smoke" => crate::linux_vz_driver::cmd_vz_smoke(rest),
        "gpu-host" => crate::linux_gpu::cmd_gpu_host(rest),
        "clean" => cmd_clean(),
        "destroy" => cmd_destroy(),
        "help" | "-h" | "--help" => {
            print!("{}", USAGE.replace("{rust}", guest::RUST));
            Ok(0)
        }
        other => Err(Fail::usage(format!(
            "linux: unknown verb '{other}' ({LAB} linux help)"
        ))),
    }
}

/// A lane that cannot run on this machine: `aterm-lab: NOT RUN: <why>`, exit 3.
#[must_use]
pub fn not_run(why: impl std::fmt::Display) -> Fail {
    Fail::exit(NOT_RUN, format!("NOT RUN: {why}"))
}

/// What one guest command did: exactly one of an exit code, a signal, a command that never
/// started, or a guest lost under it (§10's four outcomes), each a field of its own.
#[derive(Debug)]
pub struct Out {
    /// `None`: killed by a signal, never started, or lost.
    pub code: Option<i32>,
    /// stdout, then stderr (merged in order by [`Driver::shell_streamed`]); a signal's end
    /// is also named at its end, as lima's `ended_on_signal` names `limactl`'s own.
    pub text: String,
    pub secs: f64,
    /// The vz arm's `Signaled(signal)`: the guest command's own signal (lima's comes back as
    /// exit 255, and `limactl`'s own signal is named only in `text`).
    pub signal: Option<i32>,
    /// The vz arm's `NotStarted(reason)`: a missing program or `cwd`, an unexecutable file.
    pub not_started: Option<String>,
    /// The vz arm's `Lost(why)`: the guest stopped, stopped answering, or was stopped.
    pub lost: Option<String>,
}

fn ended_on_signal(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().map_or_else(String::new, |signal| {
        format!("\n(limactl ended on signal {signal})\n")
    })
}

impl Out {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// How the command ended, for a failure's detail, so an empty reply still tells the guest's side from this Mac's: `exit N`; `exit 255, nothing said` (a guest command that a signal ended comes back so through `limactl shell`, as does one that exited 255 silently); or `no exit status` (`limactl` itself was ended by a signal, which its text then names, or never ran).
    #[must_use]
    pub fn status(&self) -> String {
        if let Some(why) = &self.lost {
            return format!("lost: {why}");
        }
        if let Some(why) = &self.not_started {
            return format!("not started: {why}");
        }
        if let Some(signal) = self.signal {
            return format!("ended on signal {signal}");
        }
        match self.code {
            Some(255) if self.text.trim().is_empty() => "exit 255, nothing said: a guest command that a signal ended, or one that exited 255".into(),
            Some(code) => format!("exit {code}"),
            None => "no exit status".into(),
        }
    }

    /// The output's last `n` lines, for a failure's detail, then how it ended where its
    /// text cannot say: a command never started, or a guest lost under it (a signal's end is
    /// already the text's last line).
    #[must_use]
    pub fn tail(&self, n: usize) -> String {
        let lines: Vec<&str> = self.text.lines().collect();
        let mut out = lines[lines.len().saturating_sub(n)..].join("\n");
        let end = match (&self.not_started, &self.lost) {
            (_, Some(why)) => Some(format!("(lost: {why})")),
            (Some(why), None) => Some(format!("(not started: {why})")),
            (None, None) => None,
        };
        if let Some(end) = end {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&end);
        }
        out
    }
}

/// `exec`'s exit for its command's `out`, the end named on stderr whenever it was not an
/// exit, so §10's four outcomes reach the person apart: the command's own code; a signal
/// 128 + its number (a shell's convention for `$?`); a command never started 127, with the
/// agent's reason; a guest lost under it NOT RUN (exit 3), with the session's words; and no
/// status at all (`limactl` ended or never ran, its text naming why) 1.
///
/// # Errors
/// The guest was lost under the command: NOT RUN.
pub fn exec_exit(out: &Out, say: &mut dyn FnMut(&str)) -> Result<i32, Fail> {
    if let Some(why) = &out.lost {
        return Err(not_run(format!(
            "linux exec: the guest was lost under the command: {why}"
        )));
    }
    if let Some(why) = &out.not_started {
        say(&format!("linux exec: the command was not started: {why}"));
        return Ok(127);
    }
    if let Some(signal) = out.signal {
        say(&format!("linux exec: the command ended on signal {signal}"));
        return Ok(128 + signal);
    }
    match out.code {
        Some(code) => Ok(code),
        None => {
            say(&format!(
                "linux exec: no exit status{}",
                out.text
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .map_or_else(String::new, |l| format!(": {}", l.trim()))
            ));
            Ok(1)
        }
    }
}

/// The VM as `limactl list --json` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmState {
    /// `Running`, `Stopped`, `Broken`, …
    pub status: String,
    /// The instance's directory on this Mac (its disk lives there).
    pub dir: PathBuf,
    /// Each mount: host location and whether it is writable.
    pub mounts: Vec<(String, bool)>,
    pub cpus: u64,
    pub memory: u64,
    pub disk: u64,
}

/// Read the instance named `name` out of `limactl list --json` (one JSON object per line).
#[must_use]
pub fn parse_list(json_lines: &str, name: &str) -> Option<VmState> {
    json_lines.lines().find_map(|line| {
        let v = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
        if v.get("name")?.as_str()? != name {
            return None;
        }
        let mounts = v
            .get("config")
            .and_then(|c| c.get("mounts"))
            .and_then(aterm_json::Value::as_array)
            .map(|ms| {
                ms.iter()
                    .filter_map(|m| {
                        Some((
                            m.get("location")?.as_str()?.to_string(),
                            m.get("writable")
                                .and_then(aterm_json::Value::as_bool)
                                .unwrap_or(false),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let num = |k: &str| v.get(k).and_then(aterm_json::Value::as_u64).unwrap_or(0);
        Some(VmState {
            status: v.get("status")?.as_str()?.to_string(),
            dir: PathBuf::from(v.get("dir")?.as_str()?),
            mounts,
            cpus: num("cpus"),
            memory: num("memory"),
            disk: num("disk"),
        })
    })
}

/// What the guest mounts, each read-only at its own path: the checkout `repo`, and the git directory its `.git` file names outside it ([`outside_git_dir`]), so git in the guest reads a linked worktree's history as it does here.
#[must_use]
pub fn lane_mounts(repo: &str) -> Vec<String> {
    let mut mounts = vec![repo.to_string()];
    mounts.extend(outside_git_dir(Path::new(repo)).map(|dir| dir.to_string_lossy().into_owned()));
    mounts
}

/// The repository directory a checkout's `.git` FILE points to outside it: for a linked worktree (`git worktree add`) the common directory (its gitdir's `commondir`), which holds the worktree's own `worktrees/<name>`, the objects and the refs.
#[must_use]
pub fn outside_git_dir(repo: &Path) -> Option<PathBuf> {
    let file = std::fs::read_to_string(repo.join(".git")).ok()?;
    let gitdir = repo.join(file.lines().next()?.strip_prefix("gitdir:")?.trim());
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(relative) => gitdir.join(relative.trim()),
        Err(_) => gitdir,
    };
    let common = crate::paths::physical(&common);
    (!common.starts_with(repo)).then_some(common)
}

/// The `limactl edit --set` expression that makes `mounts` the VM's mounts, each read-only at its own path.
#[must_use]
pub fn mount_expr(mounts: &[String]) -> String {
    let entries: Vec<String> = mounts
        .iter()
        .map(|path| {
            let quoted = path.replace('\\', "\\\\").replace('"', "\\\"");
            format!("{{\"location\": \"{quoted}\", \"writable\": false}}")
        })
        .collect();
    format!(".mounts = [{}]", entries.join(", "))
}

/// Whether `state`'s mounts are exactly `mounts`, each read-only.
#[must_use]
pub fn mounted_exactly(state: &VmState, mounts: &[String]) -> bool {
    state.mounts.len() == mounts.len()
        && state
            .mounts
            .iter()
            .zip(mounts)
            .all(|((have, writable), want)| have == want && !writable)
}

/// `limactl create`'s arguments for the lane's VM.
#[must_use]
pub fn create_args() -> Vec<String> {
    [
        "create".to_string(),
        format!("--name={VM}"),
        "--tty=false".into(),
        "--vm-type=vz".into(),
        "--rosetta".into(),
        "--arch=aarch64".into(),
        format!("--cpus={CPUS}"),
        format!("--memory={MEMORY_GIB}"),
        format!("--disk={DISK_GIB}"),
        "--containerd=none".into(),
        "--mount-none".into(),
        TEMPLATE.into(),
    ]
    .into()
}

/// Select the `limactl` executable from `named`, `pinned`, or the host search path.
/// # Errors
/// A relative `named` (usage), or none that runs (NOT RUN).
pub fn pick_limactl(
    named: Option<&str>,
    pinned: Option<&str>,
    executable: impl Fn(&str) -> bool,
) -> Result<String, Fail> {
    match named {
        Some(p) if !p.starts_with('/') => Err(Fail::usage(format!(
            "LAB_LIMACTL={p} must be an absolute path"
        ))),
        Some(p) if executable(p) => Ok(p.to_string()),
        Some(p) => Err(not_run(format!(
            "LAB_LIMACTL={p} is not an executable limactl"
        ))),
        None => {
            let lima = aterm_uds::host::tool("limactl");
            pinned
                .into_iter()
                .chain(lima.at.iter().copied())
                .find(|p| executable(p))
                .map(str::to_string)
                .ok_or_else(|| {
                    not_run(format!(
                        "no limactl: LAB_LIMACTL=/path/to/limactl names one; {}",
                        lima.remedy()
                    ))
                })
        }
    }
}

/// The `limactl` this Mac has, and the instance this run drives with it.
pub struct Lima {
    pub bin: PathBuf,
    /// The instance: [`VM`], or the GPU instance under `--gpu`.
    pub vm: &'static str,
    /// What every `limactl` of the run gets beyond this process's environment (the GPU
    /// instance's krunkit, [`crate::linux_gpu::lima_env`]).
    pub env: Vec<(String, String)>,
    owned: owned::Namespace,
}

impl Lima {
    /// Locate `limactl` on this Mac.
    /// # Errors
    /// No `limactl`: the lane cannot run here.
    pub fn find() -> Result<Lima, Fail> {
        let gpu = crate::linux_gpu::on();
        let env = if gpu {
            crate::linux_gpu::lima_env()?
        } else {
            Vec::new()
        };
        if !cfg!(target_arch = "aarch64") {
            return Err(not_run(
                "the lane needs Apple silicon (lima's vz VM with Rosetta for x86_64)",
            ));
        }
        if !gpu && !Path::new("/Library/Apple/usr/libexec/oah/RosettaLinux").exists() {
            return Err(not_run(
                "Rosetta for Linux is not installed (softwareupdate --install-rosetta)",
            ));
        }
        let named = std::env::var("LAB_LIMACTL").ok().filter(|p| !p.is_empty());
        let pinned = aterm_uds::host::tooling_dir()
            .and_then(|t| aterm_uds::host::tool("limactl").placed_in(&t))
            .map(|p| p.to_string_lossy().into_owned());
        let owned = lima_namespace()?;
        pick_limactl(named.as_deref(), pinned.as_deref(), crate::sys::executable).map(|bin| Lima {
            bin: PathBuf::from(bin),
            vm: crate::linux_gpu::vm(),
            env,
            owned,
        })
    }

    fn command(&self) -> aterm_uds::spawnfd::Command {
        let mut c = aterm_uds::spawnfd::command(&self.bin);
        c.stdin(Stdio::null());
        c.envs(self.env.iter().map(|(k, v)| (k, v)));
        c.env("LIMA_HOME", &self.owned.path);
        c
    }

    /// Run `limactl ARGS`, capturing what it says.
    #[must_use]
    pub fn run(&self, args: &[String]) -> Out {
        let verb = args.first().map(String::as_str);
        let result = match verb {
            Some("list") => self.owned.existing_vm().map(|()| self.run_raw(args)),
            Some("--version") => self.owned.check().map(|()| self.run_raw(args)),
            Some("create") => self.owned.create_vm(|| {
                let out = self.run_raw(args);
                if out.ok() { Ok(out) } else { Err(out.tail(8)) }
            }),
            _ => self.owned.with_vm(|| self.run_raw(args)),
        };
        match result {
            Ok(out) if out.ok() && verb == Some("delete") => match self.owned.retired_vm() {
                Ok(()) => out,
                Err(why) => Self::refused(why),
            },
            Ok(out) => out,
            Err(why) => Self::refused(why),
        }
    }

    fn refused(why: String) -> Out {
        Out {
            code: Some(NOT_RUN),
            text: format!("Lima ownership refused: {why}"),
            secs: 0.0,
            signal: None,
            not_started: Some(why),
            lost: None,
        }
    }

    fn run_raw(&self, args: &[String]) -> Out {
        let t0 = Instant::now();
        let out = self.command().args(args).output();
        let secs = t0.elapsed().as_secs_f64();
        match out {
            Ok(o) => Out {
                code: o.status.code(),
                text: format!(
                    "{}{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr),
                    ended_on_signal(o.status)
                ),
                secs,
                signal: None,
                not_started: None,
                lost: None,
            },
            Err(e) => Out {
                code: None,
                text: format!("{} could not start: {e}", self.bin.display()),
                secs,
                signal: None,
                not_started: None,
                lost: None,
            },
        }
    }

    /// `limactl stop`, the GPU instance's file systems synced first: krunkit's stop loses what
    /// the guest had not written back (measured 2026-10-24: a Mesa install and the provisioning
    /// stamp, written a minute before, were gone at the next boot). The sync is the guest's
    /// `timeout`'s, so a wedged guest still reaches the stop; the lane's vz VM, whose stop
    /// keeps its writes, is stopped as it always was.
    #[must_use]
    pub fn stop(&self) -> Out {
        if self.vm == crate::linux_gpu::VM {
            let sync = ["timeout", "60", "sync"].map(String::from);
            let _ = self.shell("/", &sync, None);
        }
        self.run(&["stop".into(), self.vm.into()])
    }

    /// `limactl --version`'s answer.
    #[must_use]
    pub fn version(&self) -> String {
        self.run(&["--version".into()]).text.trim().to_string()
    }

    /// The lane's VM, or `None` when there is none.
    /// # Errors
    /// `limactl list` failed.
    pub fn state(&self) -> Result<Option<VmState>, String> {
        let out = self.run(&["list".into(), "--json".into()]);
        if !out.ok() {
            return Err(format!("limactl list failed: {}", out.tail(5)));
        }
        let vm = parse_list(&out.text, self.vm);
        if let Some(vm) = &vm {
            self.owned.admit_vm(&vm.dir)?;
        }
        Ok(vm)
    }

    fn shell_argv(&self, workdir: &str, argv: &[String]) -> Vec<String> {
        let mut a = vec![
            "shell".to_string(),
            format!("--workdir={workdir}"),
            self.vm.to_string(),
            "--".to_string(),
        ];
        a.extend(argv.iter().cloned());
        a
    }

    /// Run `argv` in the guest from `workdir`, capturing its output; `stdin` is written to it when given.
    #[must_use]
    pub fn shell(&self, workdir: &str, argv: &[String], stdin: Option<&str>) -> Out {
        if let Err(why) = self.owned.admit_vm(&self.owned.vm_dir()) {
            return Self::refused(why);
        }
        let t0 = Instant::now();
        let mut cmd = self.command();
        cmd.args(self.shell_argv(workdir, argv))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Out {
                    code: None,
                    text: format!("{} could not start: {e}", self.bin.display()),
                    secs: 0.0,
                    signal: None,
                    not_started: None,
                    lost: None,
                };
            }
        };
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let _ = pipe.write_all(text.as_bytes());
        }
        let out = child.wait_with_output();
        let secs = t0.elapsed().as_secs_f64();
        match out {
            Ok(o) => Out {
                code: o.status.code(),
                text: format!(
                    "{}{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr),
                    ended_on_signal(o.status)
                ),
                secs,
                signal: None,
                not_started: None,
                lost: None,
            },
            Err(e) => Out {
                code: None,
                text: format!("waiting for limactl shell: {e}"),
                secs,
                signal: None,
                not_started: None,
                lost: None,
            },
        }
    }

    /// Run `argv` in the guest with its stderr merged into its stdout there (so cargo's `Running` lines and libtest's lines keep their order), each line shown here as it comes, prefixed, and kept.
    #[must_use]
    pub fn shell_streamed(&self, workdir: &str, argv: &[String], prefix: &str) -> Out {
        if let Err(why) = self.owned.admit_vm(&self.owned.vm_dir()) {
            return Self::refused(why);
        }
        let t0 = Instant::now();
        let mut merged = vec![
            "sh".to_string(),
            "-c".into(),
            "exec \"$@\" 2>&1".into(),
            "merged".into(),
        ];
        merged.extend(argv.iter().cloned());
        let mut child = match self
            .command()
            .args(self.shell_argv(workdir, &merged))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return Out {
                    code: None,
                    text: format!("{} could not start: {e}", self.bin.display()),
                    secs: 0.0,
                    signal: None,
                    not_started: None,
                    lost: None,
                };
            }
        };
        let mut err_text = String::new();
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
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                {
                    let mut lock = stdout.lock();
                    let _ = writeln!(lock, "{prefix}{line}");
                }
                text.push_str(&line);
                text.push('\n');
            }
        }
        if let Some(h) = err_reader {
            err_text = h.join().unwrap_or_default();
        }
        let status = child.wait();
        text.push_str(&err_text);
        if let Ok(status) = &status {
            text.push_str(&ended_on_signal(*status));
        }
        Out {
            code: status.ok().and_then(|s| s.code()),
            text,
            secs: t0.elapsed().as_secs_f64(),
            signal: None,
            not_started: None,
            lost: None,
        }
    }

    /// Copy a guest file to this Mac.
    #[must_use]
    pub fn copy_out(&self, guest_path: &str, host_path: &Path) -> Out {
        self.run(&[
            "copy".into(),
            format!("{}:{guest_path}", self.vm),
            host_path.to_string_lossy().into_owned(),
        ])
    }

    /// Copy the directory `host_dir` of this Mac into the guest directory `guest_parent`, where it lands under its own name.
    #[must_use]
    pub fn copy_dir_in(&self, host_dir: &Path, guest_parent: &str) -> Out {
        self.run(&[
            "copy".into(),
            "-r".into(),
            host_dir.to_string_lossy().into_owned(),
            format!("{}:{guest_parent}/", self.vm),
        ])
    }

    /// Copy the guest directory `guest_dir` into `host_parent` on this Mac, where it lands under its own name.
    #[must_use]
    pub fn copy_dir_out(&self, guest_dir: &str, host_parent: &Path) -> Out {
        self.run(&[
            "copy".into(),
            "-r".into(),
            format!("{}:{guest_dir}", self.vm),
            format!("{}/", host_parent.to_string_lossy()),
        ])
    }

    fn guest_output(&self, home: &str, argv: &[String]) -> Result<String, String> {
        let out = self.shell(home, argv, None);
        if out.ok() {
            Ok(out.text)
        } else {
            Err(out.tail(5))
        }
    }
}

/// The checkout this lane tests: the one this program was built from, by its physical path (the guest sees it at the same path).
#[must_use]
pub fn repo() -> String {
    crate::paths::physical(&paths().repo)
        .to_string_lossy()
        .into_owned()
}

/// The VM's disk file on this Mac, and the bytes it really holds (a sparse file: what the guest has written and not yet trimmed).
#[must_use]
pub fn host_disk_bytes(state: &VmState) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    ["disk", "diffdisk"]
        .iter()
        .find_map(|f| std::fs::metadata(state.dir.join(f)).ok())
        .map(|m| m.blocks() * 512)
}

/// The host agent of the lima instance in `dir` (`ha.pid`): one process per boot.
#[must_use]
pub fn lima_agent(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join("ha.pid"))
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
}

/// A booted lima instance's processes, in its directory `dir` ([`crate::vm_admit::vm_procs`]):
/// its host agent ([`lima_agent`]), whose children krunkit and QEMU are, and whatever holds its
/// disk (Virtualization.framework's VM process, which launchd parents).
#[must_use]
pub fn lima_procs(dir: &Path) -> Vec<u32> {
    let agent = lima_agent(dir);
    let disk = ["disk", "diffdisk"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.exists())
        .unwrap_or_else(|| dir.join("disk"));
    crate::vm_admit::vm_procs(&disk, agent.as_slice())
}

/// `n` bytes in GiB, one decimal.
#[must_use]
pub fn gib(n: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size shown to one decimal")]
    let g = n as f64 / f64::from(1u32 << 30);
    format!("{g:.1} GiB")
}

/// The one lane run of this Mac: a [`named_lock`] on [`lane_lock_path`], whose file names the
/// run that holds it, released by `LOCK_UN` before the file closes (clippy.toml's flock rule).
/// Taken for a boot ([`LaneLock::boot`]), it holds the boot's disk claim too, let go, and what
/// the VM's directory took kept, when the lock is.
pub struct LaneLock {
    held: named_lock::NamedLock,
    /// The lane and its boot's disk claim ([`claim_boot_disk`]).
    disk: Option<(&'static str, atpkg::admit_disk::Admitted)>,
}

impl Drop for LaneLock {
    fn drop(&mut self) {
        if let Some((lane, held)) = self.disk.take() {
            boot_disk_done(lane, held);
        }
    }
}

/// This run as a held lane names it (`lab linux proof`), whether it waits for a held lane, and
/// how it admits its boot's disk and its VM: set once, by the lane's verb dispatch ([`name_run`]).
static RUN: OnceLock<(String, Wait, Admit)> = OnceLock::new();

/// Name this run for the lane lock it takes: `verb` (`lab linux proof`), waiting for a held lane
/// unless `wait` is [`Wait::No`] (`--no-wait`), its boot's disk and its VM admitted by `admit`
/// (`--admit wait|now`).
pub fn name_run(verb: String, wait: Wait, admit: Admit) {
    let _ = RUN.set((verb, wait, admit));
}

/// The lane's `--admit <wait|now>` before the verb, at the head of `args`: the mode and how many
/// words it took, or `None` where `args` does not start with it.
/// # Errors
/// A usage error: no value, or one that is neither `wait` nor `now`.
pub fn admit_flag(lane: &str, args: &[String]) -> Result<Option<(Admit, usize)>, Fail> {
    match args {
        [flag, word, ..] if flag == Admit::FLAG => Admit::parse(word)
            .map(|mode| Some((mode, 2)))
            .ok_or_else(|| {
                Fail::usage(format!(
                    "{lane} --admit takes wait or now, not '{word}' ({LAB} {lane} help)"
                ))
            }),
        [flag] if flag == Admit::FLAG => Err(Fail::usage(format!(
            "{lane} --admit needs wait or now ({LAB} {lane} help)"
        ))),
        _ => Ok(None),
    }
}

/// This run as its claims and records name it: the verb the dispatch named ([`name_run`]), else
/// `lab <lane>`.
#[must_use]
pub fn run_verb(lane: &str) -> String {
    RUN.get()
        .map_or_else(|| format!("lab {lane}"), |(verb, ..)| verb.clone())
}

/// How this run admits its boot's disk and its VM (`--admit wait|now`, [`name_run`]): waiting,
/// unless the dispatch named `now`.
#[must_use]
pub fn run_admit() -> Admit {
    RUN.get().map_or(Admit::Wait, |r| r.2)
}

impl LaneLock {
    /// How long this run waited for the lane: zero when it was free. A verb's own clock leaves
    /// it out.
    #[must_use]
    pub fn waited(&self) -> std::time::Duration {
        self.held.waited()
    }

    /// Take the lane for a run that boots the VM: the boot's disk claimed first
    /// ([`claim_boot_disk`]), so no run holds the lane while it waits for disk and none that
    /// holds disk waits behind one that does, then the lane ([`LaneLock::take`]). The claim is
    /// held, and the VM's directory measured, until the lock is let go at the run's end. A lima
    /// lane with no `limactl` is NOT RUN before any wait.
    /// # Errors
    /// As [`claim_boot_disk`] and [`LaneLock::take`], and lima's absence.
    pub fn boot() -> Result<LaneLock, Fail> {
        let kind = crate::linux_driver::kind()?;
        if kind == Kind::Lima {
            Lima::find()?;
        }
        LaneLock::boot_with(
            "linux",
            || claim_boot_disk("linux", linux_boot_run(kind)?),
            LaneLock::take,
        )
    }

    /// `lane` taken for a boot: `claim` claims the boot's disk, and only then does `take` take
    /// the lane, the lock holding the claim until it is let go. A claim refused takes no lane.
    /// # Errors
    /// `claim`'s or `take`'s.
    pub(crate) fn boot_with(
        lane: &'static str,
        claim: impl FnOnce() -> Result<Option<atpkg::admit_disk::Admitted>, Fail>,
        take: impl FnOnce() -> Result<LaneLock, Fail>,
    ) -> Result<LaneLock, Fail> {
        let disk = claim()?;
        let mut lock = take()?;
        lock.disk = disk.map(|d| (lane, d));
        Ok(lock)
    }

    /// Take the lane, waiting for the run that holds it ([`LaneLock::at`]).
    /// # Errors
    /// `--no-wait` and another run holds it (NOT RUN, naming it), an interrupt while waiting, or
    /// the lock file cannot be made.
    pub fn take() -> Result<LaneLock, Fail> {
        let path = match crate::linux_driver::kind()? {
            Kind::Lima => lima_namespace()?.lock_path(),
            Kind::Vz => crate::linux_vz_driver::lane_lock_path(&crate::linux_vz_driver::root()?),
        };
        LaneLock::at(&path, "linux")
    }

    /// Take the lane lock at `path` for `lane` as this run ([`name_run`]; `lab <lane>`, waiting,
    /// when the dispatch named none), from this checkout.
    /// # Errors
    /// [`LaneLock::at_as`]'s.
    pub fn at(path: &Path, lane: &str) -> Result<LaneLock, Fail> {
        let (verb, wait) = RUN
            .get()
            .map(|r| (r.0.clone(), r.1))
            .unwrap_or_else(|| (format!("lab {lane}"), Wait::Block));
        let me = named_lock::Holder::me(&verb, Path::new(&repo()));
        LaneLock::at_as(
            path,
            lane,
            &me,
            wait,
            &mut |line| println!("{line}"),
            &crate::stop::check,
        )
    }

    /// Take the lane lock at `path` as `me`. Held by another run: under [`Wait::Block`] the wait
    /// lasts while that run does, its holder named on `say` as it starts and once a minute, and
    /// ends at once when `stopped` says an interrupt came; under [`Wait::No`] the lane is NOT RUN
    /// at once, its holder named.
    /// # Errors
    /// NOT RUN under `--no-wait`; NOT RUN for an interrupt that ended a wait, naming the holder
    /// and carrying the signal (the lane ran nothing, so it decided nothing); or a lock file that
    /// cannot be made.
    pub fn at_as(
        path: &Path,
        lane: &str,
        me: &named_lock::Holder,
        wait: Wait,
        say: &mut dyn FnMut(&str),
        stopped: &dyn Fn() -> Result<(), crate::stop::Stopped>,
    ) -> Result<LaneLock, Fail> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Fail::die(format!("{}: {e}", dir.display())))?;
        }
        let taken = named_lock::take(
            path,
            me,
            wait,
            &mut |holder, waited| {
                say(&if waited < named_lock::SAY_EVERY {
                    format!(
                        "{lane}: waiting for {}, which holds the lane ({})",
                        holder_name(holder),
                        path.display()
                    )
                } else {
                    format!(
                        "{lane}: still waiting for {} ({} min)",
                        holder_name(holder),
                        waited.as_secs() / 60
                    )
                });
            },
            &|| stopped().is_err(),
        );
        match taken {
            Ok(held) => Ok(LaneLock { held, disk: None }),
            Err(Refused::Held(holder)) => Err(not_run(format!(
                "{} holds the lane ({}); --no-wait does not wait for it",
                holder_name(holder.as_ref()),
                path.display()
            ))),
            Err(Refused::Stopped { holder, signal }) => Err(Fail {
                stopped: signal
                    .inspect(|sig| crate::stop::note(*sig))
                    .or_else(|| stopped().err().map(|s| s.0)),
                ..not_run(format!(
                    "interrupted while waiting for {}, which holds the lane ({}); this run took \
                     no part of it",
                    holder_name(holder.as_ref()),
                    path.display()
                ))
            }),
            Err(Refused::Io(e)) => Err(Fail::die(format!("{}: {e}", path.display()))),
        }
    }
}

/// Whom a held lane names: its record's run while that pid lives, its clock this Mac's.
fn holder_name(holder: Option<&named_lock::Holder>) -> String {
    named_lock::name(
        holder,
        |since| {
            i64::try_from(since)
                .ok()
                .and_then(aterm_uds::zone::local_offset_at)
                .unwrap_or(0)
        },
        aterm_uds::process::pid_alive,
    )
}

/// The lane lock of Lima's instance `vm`, beside its instances in the owned namespace.
#[must_use]
pub fn lane_lock_path(vm: &str, home: &Path) -> PathBuf {
    home.join(format!(".{vm}.lane.lock"))
}

fn lima_namespace() -> Result<owned::Namespace, Fail> {
    let named = match std::env::var("LIMA_HOME") {
        Ok(s) => s,
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(std::env::VarError::NotUnicode(_)) => return Err(not_run("LIMA_HOME is not Unicode")),
    };
    let default = if named.is_empty() {
        let path = aterm_uds::host::host_user_cache("lima")
            .ok_or_else(|| not_run("no HOME for the owned Lima namespace"))?;
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| not_run("Lima cache has no parent"))?,
        )
        .map_err(|e| not_run(format!("Lima cache parent: {e}")))?;
        path
    } else {
        PathBuf::new()
    };
    let path = owned::home(Some(&named), &default).map_err(not_run)?;
    let namespace = owned::Namespace::open(&path, crate::linux_gpu::vm()).map_err(not_run)?;
    namespace.existing_vm().map_err(not_run)?;
    Ok(namespace)
}

/// `$LAB_STATE/linux`, made when missing.
/// # Errors
/// It cannot be made.
pub fn state_dir() -> Result<PathBuf, Fail> {
    let dir = paths().need_state()?.join("linux");
    std::fs::create_dir_all(&dir).map_err(|e| Fail::die(format!("{}: {e}", dir.display())))?;
    Ok(dir)
}

/// The VM, booted and provisioned, and whether this call booted it (then the caller stops it when it ends).
pub struct Up {
    /// The guest, through this run's driver.
    pub guest: Driver,
    /// The guest user's home and `XDG_RUNTIME_DIR`.
    pub home: String,
    pub runtime: String,
    build_dir: String,
    pub booted_here: bool,
    /// Reserved on the first display start, once per run. Both success and
    /// failure are retained, so a failed reservation cannot fall back to a
    /// shared path.
    config: OnceLock<Result<guest::Config, String>>,
    /// What it took, in order.
    pub timings: Vec<Step>,
    /// The renderer every weston the lane starts here draws with: `gl` unless a run names `pixman` (`--weston-renderer`).
    pub weston_renderer: &'static str,
    /// This run's claim on the VM ([`crate::vm_admit`]), held until the run ends, and
    /// `CARGO_BUILD_JOBS` for the guest's builds ([`Up::cargo_env`]).
    admitted: crate::vm_admit::Admitted,
    /// The booter record of the VM this run booted ([`crate::vm_admit::Booter`]), which its end
    /// settles.
    booter: Option<PathBuf>,
}

/// One step of bringing the lane up.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    pub secs: f64,
    /// Why it failed, for the one step whose failure leaves the lane running (the Rosetta cache reset): the proof shows it NOT RUN, never PASS.
    pub failed: Option<String>,
}

impl Step {
    fn done(name: &str, secs: f64) -> Step {
        Step {
            name: name.to_string(),
            secs,
            failed: None,
        }
    }
}

impl Up {
    /// The environment the guest's `cargo` gets for `arch` ([`guest::cargo_env`]), its job count
    /// the cores this run was admitted (`CARGO_BUILD_JOBS`).
    #[must_use]
    pub fn cargo_env(
        &self,
        arch: Arch,
        target_dir: &str,
        default_target: bool,
    ) -> Vec<(String, String)> {
        let mut env = guest::cargo_env(arch, target_dir, default_target);
        env.push(("CARGO_BUILD_JOBS".into(), self.admitted.jobs.to_string()));
        env
    }

    /// The guest-local build root: Lima's reserved directory, or VZ's [`GUEST_DIR`].
    #[must_use]
    pub fn lane_dir(&self) -> String {
        self.build_dir.clone()
    }

    /// Run `argv` in the guest, captured, from its home.
    #[must_use]
    pub fn sh(&self, argv: &[String]) -> Out {
        self.guest.shell(&self.home, argv, None)
    }

    /// This run's exclusively created guest configuration directory.
    /// # Errors
    /// The reservation failed or did not return its single expected path.
    pub fn config(&self) -> Result<&guest::Config, String> {
        self.config
            .get_or_init(|| {
                let out = self.sh(&guest::Config::reserve());
                if !out.ok() {
                    return Err(format!("guest config reservation: {}", out.tail(3)));
                }
                guest::Config::from_created(&out.text)
                    .ok_or_else(|| "guest config reservation returned no valid path".into())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Write only beneath this run's reserved root.
    /// # Errors
    /// No reservation, an unbound path, or a directory/write failure.
    pub fn write_config(&self, path: &str, text: &str) -> Result<(), String> {
        if !self.config()?.contains(path) {
            return Err(format!("configuration path is outside this run: {path}"));
        }
        let dir = Path::new(path)
            .parent()
            .ok_or_else(|| format!("configuration path has no parent: {path}"))?;
        let made = self.sh(&[
            "install".into(),
            "-d".into(),
            "-m".into(),
            "700".into(),
            dir.to_string_lossy().into_owned(),
        ]);
        if !made.ok() {
            return Err(format!("could not make {dir:?}: {}", made.tail(3)));
        }
        let wrote = self
            .guest
            .shell(&self.home, &["tee".into(), path.into()], Some(text));
        if !wrote.ok() {
            return Err(format!("could not write {path}: {}", wrote.tail(3)));
        }
        Ok(())
    }

    /// Remove the guest's build output and trim the VM disk.
    /// # Errors
    /// The removal failed.
    pub fn clean_build(&self) -> Result<(), String> {
        clean_guest_build(&self.guest, &self.home)
    }

    /// Stop the lane's units in the guest (a display, an aterm).
    pub fn stop_units(&self) {
        for argv in guest::stop_units() {
            let _ = self.sh(&argv);
        }
    }

    /// Stop the VM when this run booted it (lima), or close this run's session under the
    /// release rule (vz).
    pub fn release(self) {
        self.end(false);
    }

    /// A run's end: its units stopped, then the VM let go, kept up with `keep_up`
    /// (`--keep-up`; the vz arm sets `keep` on its session), else as [`Up::release`].
    pub fn end(self, keep_up: bool) {
        self.stop_units();
        let stopped = self.guest.end(self.booted_here, keep_up);
        crate::vm_admit::settle(self.booter.as_deref(), keep_up, stopped);
    }

    /// `up`'s end: the VM kept up (the vz arm sets `keep` on its session) and nothing else,
    /// as lima's `up` always ended; the bring-up already stopped the lane's units.
    pub fn keep(self) {
        let stopped = self.guest.end(self.booted_here, true);
        crate::vm_admit::settle(self.booter.as_deref(), true, stopped);
    }
}

/// What a Linux VM boot claims of its volume where no boot was measured: 10 GiB, less what the
/// VM's directory holds already (`aterm_capacity::Footprints::need`). MEASURED for
/// the vz arm's bring-up floor (`aterm_vz::space::BRING_UP_FLOOR`,
/// `docs/DESIGN-vz-guest-2026-10-23.md` §4): a bake writes the provisioned guest (4.9 GiB) and a
/// proof its build output (4.3 GiB), 9.2 GiB together, read up to 10. The floor doubles that for
/// the other sessions on the volume, which the capacity ledger counts by their own claims.
pub const BOOT_COLD_DISK: u64 = 10 << 30;

/// The disk a boot of the lane VM claims (`docs/DESIGN-adaptive-build-2026-10-24.md`,
/// DISK-ADMIT): `dir`, the VM's directory, measured, on the volume holding `base`, with what
/// the last boots wrote kept in `record`, or where none was measured `cold` less what `dir`
/// holds.
#[must_use]
pub fn boot_run(
    lane: &str,
    base: &Path,
    dir: PathBuf,
    record: PathBuf,
    cold: u64,
) -> atpkg::admit_disk::Run {
    atpkg::admit_disk::Run {
        what: format!("lab {lane} boot"),
        dirs: vec![dir],
        record,
        cold,
        on: base.to_path_buf(),
    }
}

/// [`boot_run`] for the Linux lane on `kind`: this run's lima instance
/// ([`crate::linux_gpu::vm`]) in the owned Lima namespace, or the vz
/// arm's root.
fn linux_boot_run(kind: Kind) -> Result<atpkg::admit_disk::Run, Fail> {
    let vm = crate::linux_gpu::vm();
    let base = match kind {
        Kind::Lima => lima_namespace()?.path,
        Kind::Vz => crate::linux_vz_driver::root()?,
    };
    let dir = match kind {
        Kind::Lima => base.join(vm),
        Kind::Vz => base.clone(),
    };
    let record = base.join(format!(".{vm}.boot-footprint.json"));
    Ok(boot_run("linux", &base, dir, record, BOOT_COLD_DISK))
}

/// Claims a lane VM boot's disk before it boots ([`atpkg::admit_disk::admit`]), as this run
/// admits ([`name_run`]; `--admit now` takes it at once): a short volume is relieved and then
/// waited for, each line printed as `<lane>: <line>`. A ledger that cannot be used is said, and
/// the VM boots without a claim.
///
/// # Errors
/// NOT RUN: the need is more than the volume holds above its floor.
pub fn claim_boot_disk(
    lane: &str,
    run: atpkg::admit_disk::Run,
) -> Result<Option<atpkg::admit_disk::Admitted>, Fail> {
    let mode = run_admit();
    claim_boot_disk_with(lane, run, &mut |run, say| {
        atpkg::admit_disk::admit(run, mode, say)
    })
}

/// The answer an admission gives a boot's claim.
type Admission = Result<atpkg::admit_disk::Admitted, atpkg::admit_disk::Error>;

/// What admits a boot's claim: the run, and where its lines go.
type Admitter<'a> = dyn FnMut(atpkg::admit_disk::Run, &mut dyn FnMut(&str)) -> Admission + 'a;

/// [`claim_boot_disk`] through `admit`. A run already ending on an interrupt
/// ([`crate::stop::check`]) asks for no claim: none of its work waits anew.
fn claim_boot_disk_with(
    lane: &str,
    run: atpkg::admit_disk::Run,
    admit: &mut Admitter<'_>,
) -> Result<Option<atpkg::admit_disk::Admitted>, Fail> {
    if let Err(s) = crate::stop::check() {
        return Err(Fail {
            stopped: Some(s.0),
            ..not_run(format!(
                "{lane}: the boot's disk claim was not asked for: the run was interrupted; this \
                 run booted nothing"
            ))
        });
    }
    match admit(run, &mut |line| println!("{lane}: {line}")) {
        Ok(held) => Ok(Some(held)),
        Err(atpkg::admit_disk::Error::Never(why)) => Err(not_run(format!(
            "{why}; the VM can never boot there. Fix: free room on that volume, or move the VM \
             to a larger one"
        ))),
        Err(e @ atpkg::admit_disk::Error::Stopped { signal, .. }) => Err(Fail {
            stopped: signal.inspect(|sig| crate::stop::note(*sig)),
            ..not_run(format!(
                "{lane}: the boot's disk claim was {e}; this run booted nothing"
            ))
        }),
        Err(e) => {
            eprintln!(
                "aterm-lab: {lane}: the boot's disk is not claimed ({e}); it boots without one"
            );
            Ok(None)
        }
    }
}

/// A boot's claim let go at its run's end, with what the VM's directory took kept for the next
/// boot and said.
fn boot_disk_done(lane: &str, held: atpkg::admit_disk::Admitted) {
    let claimed = held.estimate;
    let took = held.finish();
    println!(
        "{lane}: disk: claimed {claimed}; the VM's directory grew by {} this run (it held {} at \
         most)",
        atpkg::admit_disk::size(took.grew),
        atpkg::admit_disk::size(took.peak)
    );
}

fn clean_guest_build(guest: &Driver, home: &str) -> Result<(), String> {
    if let Driver::Lima(lima) = guest {
        lima.owned
            .clear_guest(|argv| lima.guest_output(home, argv))?;
        let _ = guest.shell(home, &["sudo".into(), "fstrim".into(), "/".into()], None);
        return Ok(());
    }
    let dir = format!("{home}/{GUEST_DIR}");
    let out = guest.shell(home, &["rm".into(), "-rf".into(), dir], None);
    if !out.ok() {
        return Err(out.tail(5));
    }
    let _ = guest.shell(home, &["sudo".into(), "fstrim".into(), "/".into()], None);
    Ok(())
}

fn guest_path_var(guest: &Driver, var: &str) -> Option<String> {
    let out = guest.shell("/", &["printenv".into(), var.into()], None);
    out.ok()
        .then(|| out.text.trim().to_string())
        .filter(|s| s.starts_with('/'))
}

/// Create, mount, boot, and provision the lane VM as needed, through this run's driver
/// ([`crate::linux_driver::kind`]).
/// # Errors
/// NOT RUN: no lima (or the vz arm's gates), or the VM could not be made, booted or
/// provisioned.
pub fn bring_up() -> Result<Up, Fail> {
    let repo = repo();
    let (guest, booted_here, timings, admitted, booter) = match crate::linux_driver::kind()? {
        Kind::Lima => bring_up_lima(&repo)?,
        Kind::Vz => bring_up_vz(&repo)?,
    };
    finish_bring_up(guest, booted_here, timings, &repo, admitted, booter)
}

/// What a bring-up's arm hands on: the guest, whether this run booted it, the steps, this run's
/// claim on it, and its booter record when this run booted it.
type Brought = (
    Driver,
    bool,
    Vec<Step>,
    crate::vm_admit::Admitted,
    Option<PathBuf>,
);

/// Where this run's lima instance ([`VM`], or the GPU instance under `--gpu`) records the run
/// that booted it ([`crate::vm_admit::Booter`]): beside its lane lock.
/// # Errors
/// No home.
pub fn booter_path() -> Result<PathBuf, Fail> {
    let vm = crate::linux_gpu::vm();
    let lock = lima_namespace()?.lock_path();
    Ok(lock.with_file_name(format!(".{vm}.booter")))
}

/// The vz arm's boot: [`crate::linux_vz_driver::open`], and where that boot is a bake's,
/// the bake (§4: provisioned through the agent, finished, powered off and sealed), then the
/// bring-up again on a clone of the sealed disk.
fn bring_up_vz(repo: &str) -> Result<Brought, Fail> {
    let mut timings = Vec::new();
    for _ in 0..2 {
        let opened = crate::linux_vz_driver::open(Path::new(repo))?;
        timings.extend(opened.steps.iter().map(|(n, s)| Step::done(n, *s)));
        let Some(partial) = opened.bake else {
            return Ok((
                Driver::Vz(Box::new(opened.vz)),
                opened.booted_here,
                timings,
                opened.admitted,
                None,
            ));
        };
        let t0 = Instant::now();
        let user = opened.vz.user.clone();
        let up = Up {
            guest: Driver::Vz(Box::new(opened.vz)),
            home: user.home(),
            runtime: user.runtime(),
            build_dir: format!("{}/{GUEST_DIR}", user.home()),
            booted_here: true,
            config: OnceLock::new(),
            timings: Vec::new(),
            weston_renderer: "gl",
            admitted: opened.admitted,
            booter: None,
        };
        if let Err(f) = provision(&up) {
            up.release();
            return Err(f);
        }
        let Up {
            guest: Driver::Vz(vz),
            ..
        } = up
        else {
            unreachable!("the bake's guest is the vz arm's");
        };
        let sealed = crate::linux_vz_driver::finish_bake(*vz, partial)?;
        println!("linux: baked {}", sealed.display());
        timings.push(Step::done("bake", t0.elapsed().as_secs_f64()));
    }
    Err(not_run(
        "the bake sealed its disk, but the bring-up after it found no baked disk",
    ))
}

/// lima's `create`, `edit` (the mounts), and `start`, behind the driver: a running VM a dead
/// one-shot run left is stopped first, as `down` stops it, and a boot is admitted
/// ([`crate::vm_admit`]) and recorded before `start`.
fn bring_up_lima(repo: &str) -> Result<Brought, Fail> {
    let mut lima = Lima::find()?;
    let vm_name = lima.vm;
    let booter = booter_path()?;
    let mut timings = Vec::new();
    let mut state = lima.state().map_err(not_run)?;
    if state.is_none() {
        println!(
            "linux: making {vm_name} ({CPUS} CPUs, {MEMORY_GIB} GiB, disk <= {DISK_GIB} GiB, {TEMPLATE}) …"
        );
        let out = lima.run(&if crate::linux_gpu::on() {
            crate::linux_gpu::create_args()
        } else {
            create_args()
        });
        timings.push(Step::done("create", out.secs));
        if !out.ok() {
            return Err(not_run(format!("limactl create failed: {}", out.tail(8))));
        }
        state = lima.state().map_err(not_run)?;
    }
    let Some(mut vm) = state else {
        return Err(not_run("limactl create left no VM"));
    };
    if vm.status == "Running"
        && let Some(gone) = crate::vm_admit::leftover(&booter)
    {
        println!(
            "linux: stopping {vm_name} left by dead run {}",
            gone.describe()
        );
        lima = down_lima(lima).map_err(|f| {
            not_run(format!(
                "{vm_name}, left by dead run pid {}, did not stop: {}",
                gone.pid,
                f.msg.unwrap_or_default()
            ))
        })?;
        crate::vm_admit::clear(&booter);
        vm = lima
            .state()
            .map_err(not_run)?
            .ok_or_else(|| not_run("the VM went away"))?;
    }
    let mounts = lane_mounts(repo);
    if !mounted_exactly(&vm, &mounts) {
        if vm.status == "Running" {
            let out = lima.stop();
            if !out.ok() {
                return Err(not_run(format!("limactl stop failed: {}", out.tail(5))));
            }
        }
        let out = lima.run(&[
            "edit".into(),
            "--tty=false".into(),
            vm_name.into(),
            "--set".into(),
            mount_expr(&mounts),
        ]);
        if !out.ok() {
            return Err(not_run(format!(
                "limactl edit (the mount) failed: {}",
                out.tail(5)
            )));
        }
        vm = lima
            .state()
            .map_err(not_run)?
            .ok_or_else(|| not_run("the VM went away"))?;
    }
    let booted_here = vm.status != "Running";
    let mut admitted = crate::vm_admit::admit(&crate::vm_admit::Guest::lima(vm_name), booted_here)?;
    let mut recorded = None;
    if booted_here {
        recorded = crate::vm_admit::record_boot("linux", &booter);
        println!("linux: booting {vm_name} (mount: {repo}, read-only) …");
        let out = lima.run(&["start".into(), "--tty=false".into(), vm_name.into()]);
        timings.push(Step::done("boot", out.secs));
        if !out.ok() {
            return Err(not_run(format!("limactl start failed: {}", out.tail(8))));
        }
        admitted.credit("linux", &lima_procs(&vm.dir));
        crate::vm_admit::record_vm(recorded.as_deref(), lima_agent(&vm.dir));
    }
    Ok((Driver::Lima(lima), booted_here, timings, admitted, recorded))
}

/// Both arms' rest of a bring-up: the guest's HOME and `XDG_RUNTIME_DIR`, provisioning in
/// place, the Rosetta cache reset and the build-output claim.
fn finish_bring_up(
    guest: Driver,
    booted_here: bool,
    timings: Vec<Step>,
    repo: &str,
    admitted: crate::vm_admit::Admitted,
    booter: Option<PathBuf>,
) -> Result<Up, Fail> {
    let (home, runtime) = (
        guest_path_var(&guest, "HOME"),
        guest_path_var(&guest, "XDG_RUNTIME_DIR"),
    );
    let mut up = Up {
        guest,
        home: home.clone().unwrap_or_default(),
        runtime: runtime.clone().unwrap_or_default(),
        build_dir: String::new(),
        booted_here,
        config: OnceLock::new(),
        timings,
        weston_renderer: "gl",
        admitted,
        booter,
    };
    if home.is_none() || runtime.is_none() {
        up.release();
        return Err(not_run("the guest named no HOME or XDG_RUNTIME_DIR"));
    }
    up.build_dir = if let Driver::Lima(lima) = &up.guest {
        match lima
            .owned
            .prepare_guest(&up.home, |argv| lima.guest_output(&up.home, argv))
        {
            Ok(dir) => dir.path,
            Err(why) => {
                up.release();
                return Err(not_run(why));
            }
        }
    } else {
        format!("{}/{GUEST_DIR}", up.home)
    };
    match provision(&up) {
        Ok(Some(secs)) => up.timings.push(Step::done("provision", secs)),
        Ok(None) => {}
        Err(f) => {
            up.release();
            return Err(f);
        }
    }
    if crate::linux_gpu::on() {
        // krunkit has no Rosetta (no rosettad, no cache to reset); its guest must boot the
        // kernel Venus needs, and be on this Mac's GPU, before anything runs on it.
        if let Err(f) = boot_gpu_kernel(&mut up) {
            up.release();
            return Err(f);
        }
        match crate::linux_gpu::prove(&|argv| up.sh(argv)) {
            Ok((named, secs)) => {
                println!("linux: on the GPU: {named}");
                up.timings
                    .push(Step::done(&format!("gpu proof ({named})"), secs));
            }
            Err(f) => {
                up.release();
                return Err(f);
            }
        }
    } else if let Some(step) = reset_rosetta_cache(&up) {
        up.timings.push(step);
    }
    match claim_build_output(&up, repo) {
        Ok(Some(step)) => up.timings.push(step),
        Ok(None) => {}
        Err(f) => {
            up.release();
            return Err(f);
        }
    }
    up.stop_units();
    Ok(up)
}

/// What the selected guest build directory holds for `repo`, from its existence and [`BUILT_FROM`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildOutput {
    /// No build directory: this run makes it and names itself in it.
    Absent,
    /// This checkout's own output, which the run builds on.
    Own,
    /// Another checkout's output, or output no stamp names: removed first.
    Foreign,
}

fn build_output(dir_exists: bool, built_from: Option<&str>, repo: &str) -> BuildOutput {
    match (dir_exists, built_from) {
        (false, _) => BuildOutput::Absent,
        (true, Some(stamp)) if stamp == repo => BuildOutput::Own,
        (true, _) => BuildOutput::Foreign,
    }
}

/// Ensure the selected build root belongs to `repo`, clearing another checkout's output first.
/// # Errors
/// NOT RUN: the foreign output could not be removed, or the stamp written.
fn claim_build_output(up: &Up, repo: &str) -> Result<Option<Step>, Fail> {
    let dir = up.lane_dir();
    let stamp = format!("{dir}/{BUILT_FROM}");
    let exists = up.sh(&["test".into(), "-d".into(), dir.clone()]).ok();
    let read = up.sh(&["cat".into(), stamp.clone()]);
    let built_from = read.ok().then(|| read.text.trim().to_string());
    let mut step = None;
    match build_output(exists, built_from.as_deref(), repo) {
        BuildOutput::Own => return Ok(None),
        BuildOutput::Absent => {}
        BuildOutput::Foreign => {
            println!(
                "linux: removing the guest's build output, which {} built …",
                built_from.as_deref().map_or_else(
                    || "a checkout no stamp names".to_string(),
                    |c| format!("{c}, not this checkout,")
                )
            );
            let t0 = Instant::now();
            up.clean_build().map_err(|e| {
                not_run(format!(
                    "another checkout's build output could not be removed: {e}"
                ))
            })?;
            step = Some(Step::done(
                "foreign build output removed",
                t0.elapsed().as_secs_f64(),
            ));
        }
    }
    let made = up.sh(&["mkdir".into(), "-p".into(), dir]);
    let wrote = up
        .guest
        .shell(&up.home, &["tee".into(), stamp], Some(&format!("{repo}\n")));
    if !made.ok() || !wrote.ok() {
        return Err(not_run(format!(
            "could not name this checkout in {BUILT_FROM}: {}",
            wrote.tail(5)
        )));
    }
    Ok(step)
}

fn reset_rosetta_cache(up: &Up) -> Option<Step> {
    let boot = up.sh(&["cat".into(), guest::BOOT_ID.into()]);
    let stamp = up.sh(&["cat".into(), guest::ROSETTA_CACHE_BOOT.into()]);
    let boot_id = boot.text.trim();
    if !boot.ok() || boot_id.is_empty() || (stamp.ok() && stamp.text.trim() == boot_id) {
        return None;
    }
    let t0 = Instant::now();
    let mut failed = None;
    for s in guest::rosetta_cache_reset(boot_id) {
        let out = up.guest.shell(&up.home, &s.argv, s.stdin.as_deref());
        if !out.ok() {
            let why = format!(
                "step '{}' failed (the x86_64 rows ran on the old cache): {}",
                s.label,
                out.tail(3)
            );
            eprintln!("aterm-lab: linux: Rosetta cache reset, {why}");
            failed = Some(why);
            break;
        }
    }
    Some(Step {
        failed,
        ..Step::done("rosetta cache reset", t0.elapsed().as_secs_f64())
    })
}

/// The plan this run's instance is provisioned with: the lane's, or under `--gpu` the lane's for
/// aarch64 alone, then the GPU instance's own steps.
fn lane_plan() -> Vec<guest::Step> {
    if crate::linux_gpu::on() {
        let mut steps = crate::linux_gpu::gpu_first_steps();
        steps.extend(guest::provision_steps_for(false));
        steps.extend(crate::linux_gpu::gpu_steps());
        steps
    } else {
        guest::provision_steps()
    }
}

/// Under `--gpu`, the guest on [`crate::linux_gpu::KERNEL`]: a guest provisioned onto it but
/// still on the image's kernel is stopped and started again (krunkit cannot reboot in place).
fn boot_gpu_kernel(up: &mut Up) -> Result<(), Fail> {
    let want = crate::linux_gpu::KERNEL.release;
    let uname = |up: &Up| {
        let out = up.sh(&["uname".into(), "-r".into()]);
        out.text.trim().to_string()
    };
    if uname(up) == want {
        return Ok(());
    }
    let Driver::Lima(lima) = &up.guest else {
        return Err(not_run("the GPU instance is lima's"));
    };
    println!("linux: booting {} on Linux {want} …", lima.vm);
    let t0 = Instant::now();
    for verb in ["stop", "start"] {
        let out = if verb == "stop" {
            lima.stop()
        } else {
            lima.run(&["start".into(), "--tty=false".into(), lima.vm.into()])
        };
        if !out.ok() {
            return Err(not_run(format!("limactl {verb} failed: {}", out.tail(8))));
        }
    }
    up.booted_here = true;
    up.timings.push(Step::done(
        "reboot onto the GPU kernel",
        t0.elapsed().as_secs_f64(),
    ));
    let have = uname(up);
    if have != want {
        return Err(not_run(format!(
            "the GPU instance booted Linux {have}, not {want} (grub's default)"
        )));
    }
    Ok(())
}

fn provision(up: &Up) -> Result<Option<f64>, Fail> {
    let steps = lane_plan();
    let want = guest::fingerprint(&steps);
    let have = up.sh(&["cat".into(), guest::STAMP.into()]);
    if have.ok() && have.text.trim() == want {
        return Ok(None);
    }
    println!("linux: provisioning the guest (plan {want}) …");
    let t0 = Instant::now();
    for s in &steps {
        println!("linux:   {} …", s.label);
        let out = up.guest.shell(&up.home, &s.argv, s.stdin.as_deref());
        if !out.ok() {
            return Err(not_run(format!(
                "provisioning step '{}' failed ({}):\n{}",
                s.label,
                out.status(),
                out.tail(15)
            )));
        }
    }
    let stamp_dir = Path::new(guest::STAMP)
        .parent()
        .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    let made = up.sh(&["sudo".into(), "mkdir".into(), "-p".into(), stamp_dir]);
    let wrote = up.guest.shell(
        &up.home,
        &["sudo".into(), "tee".into(), guest::STAMP.into()],
        Some(&format!("{want}\n")),
    );
    if !made.ok() || !wrote.ok() {
        return Err(not_run(format!(
            "could not write {}: {}",
            guest::STAMP,
            wrote.tail(5)
        )));
    }
    Ok(Some(t0.elapsed().as_secs_f64()))
}

fn cmd_up() -> Res {
    let _lock = LaneLock::boot()?;
    let up = bring_up()?;
    for step in &up.timings {
        match &step.failed {
            None => println!("linux: {} {:.1} s", step.name, step.secs),
            Some(why) => println!("linux: {} FAILED: {why}", step.name),
        }
    }
    let (vm, gpu) = (
        crate::linux_gpu::vm(),
        if crate::linux_gpu::on() { " --gpu" } else { "" },
    );
    println!(
        "linux: {vm} is up ({}); `{LAB} linux{gpu} exec -- CMD` runs in it, `{LAB} linux{gpu} down` cleans and stops it",
        up.guest.kind().name()
    );
    up.keep();
    Ok(0)
}

fn cmd_down() -> Res {
    if crate::linux_driver::kind()? == Kind::Vz {
        let _lock = LaneLock::take()?;
        return crate::linux_vz_driver::down(clean_guest_build);
    }
    let lima = Lima::find()?;
    let _lock = LaneLock::take()?;
    let vm_name = lima.vm;
    match lima.state().map_err(Fail::die)? {
        None => println!("linux: there is no {vm_name} VM"),
        Some(vm) if vm.status != "Running" => println!("linux: {vm_name} is {}", vm.status),
        Some(_) => {
            down_lima(lima)?;
            crate::vm_admit::clear(&booter_path()?);
        }
    }
    Ok(0)
}

/// `down` on lima's running VM: the guest's build output removed and its disk trimmed, then the
/// VM stopped.
/// # Errors
/// `limactl stop` failed.
fn down_lima(lima: Lima) -> Result<Lima, Fail> {
    let guest = Driver::Lima(lima);
    match guest_path_var(&guest, "HOME").map(|home| clean_guest_build(&guest, &home)) {
        Some(Ok(())) => println!("linux: the guest's build output removed, disk trimmed"),
        Some(Err(e)) => eprintln!("aterm-lab: linux: build output NOT removed: {e}"),
        None => eprintln!("aterm-lab: linux: the guest named no HOME; build output kept"),
    }
    let Driver::Lima(lima) = guest else {
        unreachable!("lima's down")
    };
    let out = lima.stop();
    if !out.ok() {
        return Err(Fail::die(format!("limactl stop failed: {}", out.tail(5))));
    }
    println!("linux: {} stopped", lima.vm);
    Ok(lima)
}

fn cmd_status() -> Res {
    if crate::linux_driver::kind()? == Kind::Vz {
        return crate::linux_vz_driver::status();
    }
    let lima = Lima::find()?;
    println!("limactl   {} ({})", lima.bin.display(), lima.version());
    println!("LIMA_HOME {}", lima.owned.path.display());
    let gpu = if crate::linux_gpu::on() { " --gpu" } else { "" };
    let Some(vm) = lima.state().map_err(Fail::die)? else {
        println!("vm        none (`{LAB} linux{gpu} up` makes it)");
        return Ok(0);
    };
    println!(
        "vm        {} {} — {} CPUs, {}, disk <= {}",
        lima.vm,
        vm.status,
        vm.cpus,
        gib(vm.memory),
        gib(vm.disk)
    );
    for (loc, writable) in &vm.mounts {
        println!(
            "mount     {loc} ({})",
            if *writable { "writable" } else { "read-only" }
        );
    }
    if let Some(bytes) = host_disk_bytes(&vm) {
        println!("host disk {} allocated in {}", gib(bytes), vm.dir.display());
    }
    if vm.status == "Running"
        && let Some(line) = crate::vm_admit::status_line(&booter_path()?)
    {
        println!("run       {line}");
    }
    if vm.status == "Running" {
        let stamp = lima.shell("/", &["cat".into(), guest::STAMP.into()], None);
        let want = guest::fingerprint(&lane_plan());
        let have = stamp.text.trim();
        println!(
            "provision {}",
            if stamp.ok() && have == want {
                format!("current ({want})")
            } else if stamp.ok() {
                format!("stale ({have}; the plan is {want}: the next run provisions)")
            } else {
                "never (the next run provisions)".to_string()
            }
        );
        let df = lima.shell(
            "/",
            &[
                "df".into(),
                "-B1".into(),
                "--output=used,size".into(),
                "/".into(),
            ],
            None,
        );
        if let Some((used, size)) = parse_df(&df.text) {
            println!("guest /   {} used of {}", gib(used), gib(size));
        }
    }
    Ok(0)
}

/// `df -B1 --output=used,size /`'s numbers.
#[must_use]
pub fn parse_df(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().nth(1)?;
    let mut nums = line.split_whitespace().map(str::parse::<u64>);
    Some((nums.next()?.ok()?, nums.next()?.ok()?))
}

/// What `exec` was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct ExecArgs {
    pub arch: Arch,
    pub display: Option<Display>,
    pub argv: Vec<String>,
}

/// `exec [--arch A] [--display D] [--] CMD...`.
/// # Errors
/// A usage error.
pub fn parse_exec(args: &[String]) -> Result<ExecArgs, Fail> {
    let mut arch = Arch::Aarch64;
    let mut display = None;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--arch" => {
                let w = args.get(i + 1).map_or("", String::as_str);
                arch = Arch::parse(w).ok_or_else(|| {
                    Fail::usage(format!("linux exec: --arch {w}: aarch64 or x86_64"))
                })?;
                i += 2;
            }
            "--display" => {
                let w = args.get(i + 1).map_or("", String::as_str);
                display = Some(Display::parse(w).ok_or_else(|| {
                    Fail::usage(format!(
                        "linux exec: --display {w}: xvfb, weston, mutter, xwayland or weston-x11"
                    ))
                })?);
                i += 2;
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
            "linux exec: no command (… exec [--arch A] [--display D] -- CMD)",
        ));
    }
    Ok(ExecArgs {
        arch,
        display,
        argv,
    })
}

fn cmd_exec(args: &[String]) -> Res {
    let ask = parse_exec(args)?;
    crate::linux_gpu::arches(vec![ask.arch], "exec")?;
    let _lock = LaneLock::boot()?;
    let up = bring_up()?;
    let mut env = up.cargo_env(ask.arch, &format!("{}/target", up.lane_dir()), true);
    env.push((
        "DBUS_SESSION_BUS_ADDRESS".into(),
        format!("unix:path={}/bus", up.runtime),
    ));
    if let Some(d) = ask.display {
        if let Err(why) = start_display(&up, d) {
            up.release();
            return Err(not_run(why));
        }
        env.push(d.client_env(&up.runtime));
    }
    let out = up
        .guest
        .shell_streamed(&repo(), &guest::with_env(&env, &ask.argv), "");
    if up.booted_here
        && let Err(e) = up.clean_build()
    {
        eprintln!("aterm-lab: linux: build output NOT removed: {e}");
    }
    up.release();
    exec_exit(&out, &mut |words| eprintln!("aterm-lab: {words}"))
}

/// Start `display`'s servers in the guest and return the accessibility bus address.
/// # Errors
/// A server that did not come up within a minute, or no accessibility bus.
pub fn start_display(up: &Up, display: Display) -> Result<String, String> {
    start_screen(up, guest::Screen::plain(display))
}

/// Start `screen` in the guest: write its files, start its servers, then run its after-steps.
/// # Errors
/// As [`start_display`], or a file or an after-step that failed.
pub fn start_screen(up: &Up, screen: guest::Screen) -> Result<String, String> {
    let config = up.config()?;
    start_servers(
        up,
        screen,
        screen.files(config),
        screen.units(&up.runtime, config),
        None,
    )
}

/// The user's pipewire, through which Mutter makes a screencast stream: the space a RemoteDesktop finger is placed in (`lane-bus seat`'s finger steps), and the frame `lane-bus cast` reads.
pub fn start_pipewire(up: &Up) {
    let _ = up.sh(&[
        "systemctl".into(),
        "--user".into(),
        "start".into(),
        "pipewire.socket".into(),
        "pipewire.service".into(),
        "wireplumber.service".into(),
    ]);
}

/// Start `screen` with baseline files, units, and keymap.
/// # Errors
/// As [`start_screen`], or a keymap that was not set.
pub fn start_baseline_screen(up: &Up, screen: guest::Screen) -> Result<String, String> {
    let config = up.config()?;
    start_servers(
        up,
        screen,
        screen.baseline_files(config),
        screen.baseline_units(&up.runtime, config),
        screen.baseline_keymap(),
    )
}

fn start_servers(
    up: &Up,
    screen: guest::Screen,
    files: Vec<(String, String)>,
    units: Vec<guest::Unit>,
    then: Option<(String, guest::Step)>,
) -> Result<String, String> {
    // Mutter after the user's pipewire, always: its screencasts (a
    // RemoteDesktop finger's stream, `lane-bus cast`'s frame) need it.
    if screen.display == Display::Mutter {
        start_pipewire(up);
    }
    for (path, text) in files {
        up.write_config(&path, &text)
            .map_err(|e| format!("{}: {e}", screen.name()))?;
    }
    let units = units
        .into_iter()
        .map(|mut u| {
            for a in &mut u.argv {
                if a == "--renderer=gl" {
                    *a = format!("--renderer={}", up.weston_renderer);
                }
            }
            u
        })
        .collect();
    start_units(up, &screen.name(), units, then)?;
    for step in screen.after() {
        let out = up.guest.shell(&up.home, &step.argv, step.stdin.as_deref());
        if !out.ok() {
            up.stop_units();
            return Err(format!(
                "{}: {} failed: {}",
                screen.name(),
                step.label,
                out.tail(3)
            ));
        }
    }
    a11y_bus(up)
}

/// Start `units` in order, each listening within a minute (a hang detector); `name` says which screen in an error.
pub(crate) fn start_units(
    up: &Up,
    name: &str,
    units: Vec<guest::Unit>,
    then: Option<(String, guest::Step)>,
) -> Result<(), String> {
    for unit in units {
        let started = up.sh(&guest::run_unit(&unit, None));
        let listening = started.ok() && up.sh(&guest::wait_socket(&unit.socket, 60)).ok();
        if !listening {
            let journal = up.sh(&[
                "journalctl".into(),
                "--user".into(),
                "--no-pager".into(),
                "-n".into(),
                "15".into(),
                "-u".into(),
                unit.name.clone(),
            ]);
            up.stop_units();
            return Err(format!(
                "{name} ({}) did not come up: {}\n{}",
                unit.argv.join(" "),
                started.tail(3),
                journal.tail(15)
            ));
        }
        if let Some((_, step)) = then.as_ref().filter(|(socket, _)| *socket == unit.socket) {
            let out = up.guest.shell(&up.home, &step.argv, step.stdin.as_deref());
            if !out.ok() {
                up.stop_units();
                return Err(format!("{name}: {} failed: {}", step.label, out.tail(3)));
            }
        }
    }
    Ok(())
}

fn a11y_bus(up: &Up) -> Result<String, String> {
    let a11y = up.sh(&guest::a11y_address());
    guest::parse_a11y_address(&a11y.text)
        .map(str::to_string)
        .ok_or_else(|| {
            up.stop_units();
            format!("at-spi2-core: no accessibility bus ({})", a11y.tail(3))
        })
}

/// Start Xvfb with MIT-SHM disabled (`-extension MIT-SHM`).
/// # Errors
/// As [`start_display`].
pub fn start_xvfb_without_shm(up: &Up) -> Result<String, String> {
    let mut units = Display::Xvfb.units(&up.runtime, up.config()?);
    for u in &mut units {
        u.argv
            .extend(["-extension".to_string(), "MIT-SHM".to_string()]);
    }
    start_units(up, "xvfb without MIT-SHM", units, None)?;
    a11y_bus(up)
}

/// Start headless weston with debug capture protocols enabled (`--debug`).
/// # Errors
/// As [`start_display`].
pub fn start_weston_capturing(up: &Up) -> Result<String, String> {
    start_weston_capturing_with(up, up.weston_renderer)
}

/// [`start_weston_capturing`] with weston's `renderer` (`gl`, `pixman`), whatever the run's.
/// # Errors
/// As [`start_display`].
pub fn start_weston_capturing_with(up: &Up, renderer: &str) -> Result<String, String> {
    let screen = guest::Screen::plain(Display::Weston);
    let config = up.config()?;
    let mut units = screen.units(&up.runtime, config);
    for u in &mut units {
        for a in &mut u.argv {
            if a.starts_with("--renderer=") {
                *a = format!("--renderer={renderer}");
            }
        }
        u.argv.push("--debug".to_string());
    }
    start_servers(up, screen, screen.files(config), units, None)
}

fn cmd_clean() -> Res {
    let _lock = LaneLock::boot()?;
    let up = bring_up()?;
    let cleaned = up.clean_build();
    let df = up.sh(&[
        "df".into(),
        "-B1".into(),
        "--output=used,size".into(),
        "/".into(),
    ]);
    up.release();
    if let Err(e) = cleaned {
        return Err(Fail::die(format!("linux clean: {e}")));
    }
    if let Some((used, _)) = parse_df(&df.text) {
        println!(
            "linux: guest build output removed; guest / now {} used",
            gib(used)
        );
    }
    Ok(0)
}

fn cmd_destroy() -> Res {
    let _lock = LaneLock::take()?;
    if crate::linux_driver::kind()? == Kind::Vz {
        return crate::linux_vz_driver::destroy();
    }
    let lima = Lima::find()?;
    if lima.state().map_err(Fail::die)?.is_none() {
        println!("linux: there is no {} VM", lima.vm);
        return Ok(0);
    }
    let out = lima.run(&[
        "delete".into(),
        "--force".into(),
        "--tty=false".into(),
        lima.vm.into(),
    ]);
    if !out.ok() {
        return Err(Fail::die(format!("limactl delete failed: {}", out.tail(5))));
    }
    println!("linux: {} deleted with its disk", lima.vm);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unowned_lima_never_reaches_the_mutating_or_guest_command() {
        use std::os::unix::fs::DirBuilderExt as _;
        let t = aterm_tempfile::tempdir().unwrap();
        let owned = owned::Namespace::open(&t.path().join("lima"), VM).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(owned.vm_dir())
            .unwrap();
        std::fs::write(owned.vm_dir().join("notes"), "keep").unwrap();
        let lima = Lima {
            bin: t.path().join("must-not-start"),
            vm: VM,
            env: Vec::new(),
            owned,
        };
        for verb in ["list", "create", "stop", "start", "edit", "delete", "copy"] {
            let out = lima.run(&[verb.into(), VM.into()]);
            assert_eq!(out.code, Some(NOT_RUN), "{verb}: {}", out.text);
            assert!(out.not_started.is_some(), "{verb}: {}", out.text);
        }
        for out in [
            lima.shell("/", &["true".into()], None),
            lima.shell_streamed("/", &["true".into()], ""),
        ] {
            assert_eq!(out.code, Some(NOT_RUN));
            assert!(out.not_started.is_some());
        }
        assert_eq!(
            std::fs::read_to_string(lima.owned.vm_dir().join("notes")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn a_signal_that_ended_limactl_is_named() {
        let run = |script: &str| {
            aterm_uds::spawnfd::command("/bin/sh")
                .args(["-c", script])
                .status()
                .expect("/bin/sh runs")
        };
        assert_eq!(
            ended_on_signal(run("kill -9 $$")),
            "\n(limactl ended on signal 9)\n"
        );
        assert_eq!(ended_on_signal(run("exit 255")), "");
    }

    #[test]
    fn a_failure_names_how_it_ended() {
        let out = |code: Option<i32>, text: &str| Out {
            code,
            text: text.into(),
            secs: 0.0,
            signal: None,
            not_started: None,
            lost: None,
        };
        assert_eq!(
            out(Some(255), " \n").status(),
            "exit 255, nothing said: a guest command that a signal ended, or one that exited 255"
        );
        assert_eq!(
            out(Some(255), "ssh: connect refused\n").status(),
            "exit 255"
        );
        assert_eq!(
            out(None, "\n(limactl ended on signal 9)\n").status(),
            "no exit status"
        );
        assert_eq!(out(Some(1), "").status(), "exit 1");
    }

    const LIST: &str = r#"{"name":"other","status":"Running","dir":"/x","cpus":2,"memory":1,"disk":2}
{"name":"aterm-linux","status":"Stopped","dir":"/Users//u/.lima/aterm-linux","vmType":"vz","cpus":8,"memory":17179869184,"disk":64424509440,"config":{"mounts":[{"location":"/Users//u/aterm","mountPoint":"/Users//u/aterm","writable":false}]}}
"#;

    #[test]
    fn the_lane_vm_is_read_out_of_limas_list() {
        let vm = parse_list(LIST, VM).expect("the lane's VM");
        assert_eq!(vm.status, "Stopped");
        assert_eq!(vm.dir, PathBuf::from("/Users//u/.lima/aterm-linux"));
        assert_eq!(vm.cpus, 8);
        assert_eq!(vm.memory, 16 << 30);
        assert_eq!(vm.disk, 60 << 30);
        let alone = ["/Users//u/aterm".to_string()];
        assert!(mounted_exactly(&vm, &alone));
        assert!(!mounted_exactly(&vm, &["/Users//u/other-worktree".into()]));
        assert!(!mounted_exactly(
            &vm,
            &["/Users//u/aterm".into(), "/Users//u/main/.git".into()]
        ));
        assert_eq!(parse_list(LIST, "absent"), None);
        assert_eq!(parse_list("", VM), None);
        let writable = LIST.replace("\"writable\":false", "\"writable\":true");
        assert!(!mounted_exactly(
            &parse_list(&writable, VM).expect("vm"),
            &alone
        ));
    }

    #[test]
    fn a_run_builds_only_on_its_own_checkouts_output() {
        let me = "/Users//u/aterm-dv-a";
        assert_eq!(build_output(false, None, me), BuildOutput::Absent);
        assert_eq!(
            build_output(false, Some("/Users//u/aterm-dv-b"), me),
            BuildOutput::Absent
        );
        assert_eq!(build_output(true, Some(me), me), BuildOutput::Own);
        assert_eq!(
            build_output(true, Some("/Users//u/aterm-dv-b"), me),
            BuildOutput::Foreign
        );
        assert_eq!(build_output(true, None, me), BuildOutput::Foreign);
        assert_eq!(
            build_output(true, Some("/Users//u/aterm-dv-a-base"), me),
            BuildOutput::Foreign
        );
    }

    #[test]
    fn a_linked_worktree_mounts_its_repositorys_git_directory() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let root = crate::paths::physical(root.path());
        let main_git = root.join("main/.git");
        let gitdir = main_git.join("worktrees/wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let wt = root.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert_eq!(outside_git_dir(&wt), Some(main_git.clone()));
        let wt_path = wt.to_string_lossy().into_owned();
        assert_eq!(
            lane_mounts(&wt_path),
            [wt_path.clone(), main_git.to_string_lossy().into_owned()]
        );
        let main = root.join("main").to_string_lossy().into_owned();
        assert_eq!(outside_git_dir(Path::new(&main)), None);
        assert_eq!(lane_mounts(&main), std::slice::from_ref(&main));
        let inner = root.join("inner");
        std::fs::create_dir_all(inner.join("gitdir")).unwrap();
        std::fs::write(inner.join(".git"), "gitdir: gitdir\n").unwrap();
        assert_eq!(outside_git_dir(&inner), None);
    }

    /// A wait no interrupt ends.
    fn go_on() -> Result<(), crate::stop::Stopped> {
        Ok(())
    }

    /// An interrupt that ends a wait for a held lane is NOT RUN, naming the holder and carrying
    /// the signal: the lane ran nothing. Negative control: the holder's own take, with no
    /// interrupt, has the lane.
    #[test]
    fn an_interrupted_wait_is_not_run_naming_the_holder() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("lab-lane-stop-")
            .tempdir()
            .expect("a scratch dir");
        let path = dir.path().join(".aterm-linux.lane.lock");
        let holder = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/holder"));
        let held = LaneLock::at_as(&path, "linux", &holder, Wait::No, &mut |_| {}, &go_on)
            .unwrap_or_else(|f| panic!("the lane is free: {:?}", f.msg));
        assert_eq!(held.waited(), std::time::Duration::ZERO);
        let me = named_lock::Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
        let refused = LaneLock::at_as(&path, "linux", &me, Wait::Block, &mut |_| {}, &|| {
            Err(crate::stop::Stopped(libc::SIGTERM))
        })
        .err()
        .expect("an interrupt ends the wait");
        let why = refused.msg.unwrap_or_default();
        assert_eq!(
            (refused.code, refused.stopped),
            (NOT_RUN, Some(libc::SIGTERM)),
            "{why}"
        );
        assert!(
            why.starts_with(&format!(
                "NOT RUN: interrupted while waiting for pid {} (lab linux proof, /checkouts/holder, since ",
                holder.pid
            )),
            "{why}"
        );
        drop(held);
    }

    /// [`a_held_lane_is_waited_for_until_its_holder_dies_by_its_own_hand`]'s holder.
    const STAND_IN: &str = "linux::tests::a_lane_holder_stand_in";
    /// The lock file the stand-in takes.
    const STAND_IN_ENV: &str = "ATERM_LAB_TEST_LANE_HOLDER";
    /// What the stand-in says once it holds the lane (libtest's own words share its line).
    const STAND_IN_HOLDS: &str = "the stand-in holds the lane";

    /// Not a test alone: as [`STAND_IN`] it takes the lane at [`STAND_IN_ENV`]'s path, says
    /// [`STAND_IN_HOLDS`], and at its first line on stdin dies by its own `SIGKILL`.
    #[test]
    fn a_lane_holder_stand_in() {
        let Some(path) = std::env::var_os(STAND_IN_ENV) else {
            return;
        };
        let me = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/holder"));
        let _held = LaneLock::at_as(
            Path::new(&path),
            "linux",
            &me,
            Wait::No,
            &mut |_| {},
            &go_on,
        )
        .unwrap_or_else(|f| panic!("the lane is free: {:?}", f.msg));
        println!("{STAND_IN_HOLDS}");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
        aterm_uds::process::kill_self_now()
    }

    /// A held lane is waited for with its holder named, and is the waiter's once the holder is
    /// gone, though the holder died by its own `SIGKILL` and nobody signalled anyone; the waiter
    /// says how soon it had the lane. Negative control: `--no-wait` is NOT RUN at once, naming
    /// the same holder.
    #[test]
    fn a_held_lane_is_waited_for_until_its_holder_dies_by_its_own_hand() {
        use std::os::unix::process::ExitStatusExt as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("lab-lane-wait-")
            .tempdir()
            .expect("a scratch dir");
        let path = dir.path().join(".aterm-linux.lane.lock");
        let mut holder = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
            .args(["--exact", STAND_IN, "--nocapture", "--test-threads=1"])
            .env(STAND_IN_ENV, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stand-in starts");
        let pid = holder.id();
        let said = BufReader::new(holder.stdout.take().expect("its stdout"));
        let (holds, held_now) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in said.lines().map_while(Result::ok) {
                if line.contains(STAND_IN_HOLDS) {
                    let _ = holds.send(());
                }
            }
        });
        // The one-minute hang detector: the stand-in takes a free lane at once.
        held_now
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the stand-in holds the lane");
        let me = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/waiter"));
        let refused = LaneLock::at_as(&path, "linux", &me, Wait::No, &mut |_| {}, &go_on)
            .err()
            .expect("--no-wait does not wait");
        let why = refused.msg.unwrap_or_default();
        assert_eq!(refused.code, NOT_RUN, "{why}");
        assert!(
            why.contains(&format!(
                "pid {pid} (lab linux proof, /checkouts/holder, since "
            )),
            "{why}"
        );

        let mut stdin = holder.stdin.take().expect("its stdin");
        let (died, death) = std::sync::mpsc::channel();
        let reaper = std::thread::spawn(move || {
            let status = holder.wait().expect("the stand-in is reaped");
            let _ = died.send(Instant::now());
            status
        });
        let (took, taken) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let mut heard = Vec::new();
            let held = LaneLock::at_as(
                &path,
                "linux",
                &me,
                Wait::Block,
                &mut |line| {
                    heard.push(line.to_string());
                    let _ = stdin.write_all(b"die\n");
                },
                &go_on,
            );
            let _ = took.send(Instant::now());
            (held.map(|_| ()).map_err(|f| f.msg), heard)
        });
        // The one-minute hang detector: the lane frees itself as the holder dies.
        let had = taken
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the waiter has the lane once its holder is gone");
        let (held, heard) = waiter.join().expect("the waiter");
        held.expect("the lane");
        let status = reaper.join().expect("the reaper");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
        let gone = death.recv().expect("the death's instant");
        assert!(
            heard.first().is_some_and(|l| l.starts_with(&format!(
                "linux: waiting for pid {pid} (lab linux proof, /checkouts/holder, since "
            ))),
            "{heard:?}"
        );
        eprintln!(
            "lane free {} ms after its holder died",
            had.saturating_duration_since(gone).as_millis()
        );
    }

    #[test]
    fn every_checkout_takes_the_one_lock_beside_the_vm() {
        let home = Path::new("/private/lima");
        let mine = lane_lock_path(VM, home);
        assert_eq!(mine, PathBuf::from("/private/lima/.aterm-linux.lane.lock"));
        assert_eq!(
            lane_lock_path(VM, Path::new("/v/lima")),
            PathBuf::from("/v/lima/.aterm-linux.lane.lock")
        );
        assert_eq!(
            lane_lock_path(crate::linux_gpu::VM, home),
            PathBuf::from("/private/lima/.aterm-linux-gpu.lane.lock"),
            "the GPU instance's runs never wait on the lane VM's"
        );
    }

    #[test]
    fn the_mount_expression_quotes_its_paths() {
        assert_eq!(
            mount_expr(&["/Users//u/aterm".into()]),
            r#".mounts = [{"location": "/Users//u/aterm", "writable": false}]"#
        );
        assert_eq!(
            mount_expr(&["/Users//u/wt".into(), "/Users//u/aterm/.git".into()]),
            r#".mounts = [{"location": "/Users//u/wt", "writable": false}, {"location": "/Users//u/aterm/.git", "writable": false}]"#
        );
        assert!(mount_expr(&["/a\"b".into()]).contains(r#""/a\"b""#));
    }

    #[test]
    fn exec_reads_its_switches_then_the_command() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let e = parse_exec(&s(&[
            "--arch",
            "x86_64",
            "--display",
            "weston",
            "--",
            "cargo",
            "test",
        ]))
        .expect("parses");
        assert_eq!(e.arch, Arch::X86_64);
        assert_eq!(e.display, Some(Display::Weston));
        assert_eq!(e.argv, ["cargo", "test"]);
        let bare = parse_exec(&s(&["uname", "-m"])).expect("parses");
        assert_eq!((bare.arch, bare.display), (Arch::Aarch64, None));
        assert_eq!(bare.argv, ["uname", "-m"]);
        assert_eq!(
            parse_exec(&s(&["--arch", "mips", "x"]))
                .map_err(|f| f.code)
                .err(),
            Some(2)
        );
        assert_eq!(parse_exec(&s(&["--"])).map_err(|f| f.code).err(), Some(2));
    }

    #[test]
    fn df_and_sizes_read_as_shown() {
        assert_eq!(
            parse_df("    Used     1B-blocks\n 6940000000 61285326848\n"),
            Some((6_940_000_000, 61_285_326_848))
        );
        assert_eq!(parse_df("Used 1B-blocks\n"), None);
        assert_eq!(gib(3 << 29), "1.5 GiB");
    }

    #[test]
    fn a_named_limactl_is_the_only_one_tried() {
        let code = |r: Result<String, Fail>| r.map_err(|f| f.code);
        let none = |_: &str| false;
        let brew = |p: &str| p == "/opt/homebrew/bin/limactl";
        let cache = "/c/tooling.noindex/lima-2.2.1/bin/limactl";
        assert_eq!(
            code(pick_limactl(None, Some(cache), brew)),
            Ok("/opt/homebrew/bin/limactl".into())
        );
        assert_eq!(
            code(pick_limactl(None, Some(cache), |_| true)),
            Ok(cache.into())
        );
        assert_eq!(code(pick_limactl(None, Some(cache), none)), Err(NOT_RUN));
        assert_eq!(
            code(pick_limactl(Some("/x/limactl"), Some(cache), brew)),
            Err(NOT_RUN),
            "no fallback"
        );
        assert_eq!(
            code(pick_limactl(Some("/x/limactl"), None, |_| true)),
            Ok("/x/limactl".into())
        );
        assert_eq!(code(pick_limactl(Some("limactl"), None, |_| true)), Err(2));
    }

    #[test]
    fn a_lane_that_cannot_run_exits_three_and_says_so() {
        let f = not_run("no limactl");
        assert_eq!(f.code, NOT_RUN);
        assert_eq!(f.msg.as_deref(), Some("NOT RUN: no limactl"));
    }

    /// A boot claims its disk before it takes the lane, so a run waiting for disk holds no lane
    /// that a run holding disk would wait for. With the lane held by another run, the claim is
    /// made first and only then is the lane found held. Negative control: a claim the volume can
    /// never hold is NOT RUN, and the lane is never asked for.
    #[test]
    fn a_boot_claims_its_disk_before_it_takes_the_lane() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("lab-lane-boot-")
            .tempdir()
            .expect("a scratch dir");
        let path = dir.path().join(".aterm-linux.lane.lock");
        let holder = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/holder"));
        let _held = LaneLock::at_as(&path, "linux", &holder, Wait::No, &mut |_| {}, &go_on)
            .unwrap_or_else(|f| panic!("the lane is free: {:?}", f.msg));
        let me = named_lock::Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
        let order = std::cell::RefCell::new(Vec::new());
        let refused = LaneLock::boot_with(
            "linux",
            || {
                order.borrow_mut().push("claim");
                Ok(None)
            },
            || {
                order.borrow_mut().push("lane");
                LaneLock::at_as(&path, "linux", &me, Wait::No, &mut |_| {}, &go_on)
            },
        )
        .err()
        .expect("the lane is held");
        assert_eq!(*order.borrow(), ["claim", "lane"]);
        assert_eq!(refused.code, NOT_RUN, "{:?}", refused.msg);
        order.borrow_mut().clear();
        let never = LaneLock::boot_with(
            "linux",
            || {
                order.borrow_mut().push("claim");
                Err(not_run(
                    "lab linux boot: needs 10 GiB; the VM can never boot there",
                ))
            },
            || {
                order.borrow_mut().push("lane");
                LaneLock::at_as(&path, "linux", &me, Wait::No, &mut |_| {}, &go_on)
            },
        )
        .err()
        .expect("never fits");
        assert_eq!(*order.borrow(), ["claim"], "no lane asked for");
        assert!(
            never.msg.is_some_and(|m| m.contains("can never boot")),
            "the claim's NOT RUN"
        );
    }

    /// A boot claims the VM's directory on its volume, its record beside it; a need the volume
    /// can never hold is NOT RUN naming the need, and a ledger that cannot be used boots without
    /// a claim.
    #[test]
    fn a_boot_claims_its_vms_directory_or_cannot_run() {
        let base = Path::new("/Users//u/.lima");
        let run = boot_run(
            "linux",
            base,
            base.join(VM),
            base.join(".aterm-linux.boot-footprint.json"),
            BOOT_COLD_DISK,
        );
        assert_eq!(run.what, "lab linux boot");
        assert_eq!(run.dirs, [base.join(VM)]);
        assert_eq!(run.on, base);
        let mut asked = Vec::new();
        let refused = claim_boot_disk_with("linux", run.clone(), &mut |r, _| {
            asked.push(r);
            Err(atpkg::admit_disk::Error::Never(
                "lab linux boot: needs 10 GiB on /Volumes/s, which holds 6 GiB above its floor"
                    .into(),
            ))
        });
        let f = refused.expect_err("never fits");
        assert_eq!(f.code, NOT_RUN);
        let msg = f.msg.unwrap_or_default();
        assert!(
            msg.contains("needs 10 GiB on /Volumes/s") && msg.contains("can never boot"),
            "{msg}"
        );
        assert_eq!(asked, std::slice::from_ref(&run));
        let unclaimed = claim_boot_disk_with("linux", run, &mut |_, _| {
            Err(atpkg::admit_disk::Error::Ledger(std::io::Error::other(
                "read-only",
            )))
        });
        assert!(matches!(unclaimed, Ok(None)));
    }

    /// Set, the test binary is [`standin_boot_disk_claim_interrupted`].
    const BOOT_STANDIN: &str = "ATERM_LAB_BOOT_DISK_INTERRUPT_STANDIN";

    /// Run by [`an_interrupted_boot_disk_claim_ends_the_run_not_run`] in a process of its own,
    /// since the interrupt becomes the run's: a boot's disk claim that an interrupt (SIGTERM)
    /// ended is NOT RUN carrying it, the run's own interrupt path has it, and a later boot's
    /// claim is refused without being asked for; it exits as the lab's main would.
    #[test]
    fn standin_boot_disk_claim_interrupted() {
        if std::env::var_os(BOOT_STANDIN).is_none() {
            return;
        }
        let base = Path::new("/Users//u/.lima");
        let run = boot_run(
            "linux",
            base,
            base.join(VM),
            base.join(".aterm-linux.boot-footprint.json"),
            BOOT_COLD_DISK,
        );
        let f = claim_boot_disk_with("linux", run.clone(), &mut |_, _| {
            Err(atpkg::admit_disk::Error::Stopped {
                signal: Some(libc::SIGTERM),
                short: None,
            })
        })
        .expect_err("the interrupt ended the claim");
        assert_eq!((f.code, f.stopped), (NOT_RUN, Some(libc::SIGTERM)));
        assert!(
            crate::stop::ended_a_wait(),
            "the claim handed its interrupt on"
        );
        assert_eq!(
            crate::stop::check(),
            Err(crate::stop::Stopped(libc::SIGTERM))
        );
        let again = claim_boot_disk_with("linux", run, &mut |_, _| {
            panic!("a boot's disk claim asked for once the run is ending")
        })
        .expect_err("no claim once the run is ending");
        println!("{}", f.msg.unwrap_or_default());
        println!("{}", again.msg.unwrap_or_default());
        std::process::exit(crate::stop::status(f.code, false));
    }

    /// An interrupt that ended a boot's disk claim ends the run NOT RUN (exit 3), naming it,
    /// and no later boot of the run asks for a claim.
    #[test]
    fn an_interrupted_boot_disk_claim_ends_the_run_not_run() {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.args([
            "--exact",
            "linux::tests::standin_boot_disk_claim_interrupted",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(BOOT_STANDIN, "1");
        let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
            .expect("the stand-in ends within the hang detector");
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(NOT_RUN), "{}\n{text}", out.status);
        assert!(
            text.contains("NOT RUN: linux: the boot's disk claim was interrupted (SIGTERM)"),
            "{text}"
        );
        assert!(
            text.contains("NOT RUN: linux: the boot's disk claim was not asked for"),
            "{text}"
        );
    }

    use crate::linux_driver::fake::{Answer, Call, Guest};

    fn up_on(guest: &Guest, booted_here: bool) -> Up {
        Up {
            guest: Driver::Fake(guest.clone()),
            home: "/home/u.guest".into(),
            runtime: "/run/user/501".into(),
            build_dir: "/home/u.guest/aterm-lane".into(),
            booted_here,
            config: OnceLock::new(),
            timings: Vec::new(),
            weston_renderer: "gl",
            admitted: crate::vm_admit::Admitted {
                claim: None,
                jobs: 3,
            },
            booter: None,
        }
    }

    /// The guest's `cargo` runs as many jobs as cores this run was admitted, on every arch, on
    /// top of the arch's own environment.
    #[test]
    fn the_guests_cargo_runs_the_jobs_admitted() {
        let up = up_on(&Guest::new(), false);
        for arch in [Arch::Aarch64, Arch::X86_64] {
            let env = up.cargo_env(arch, "/t", false);
            assert!(
                env.contains(&("CARGO_BUILD_JOBS".into(), "3".into())),
                "{env:?}"
            );
            assert!(
                env.starts_with(&guest::cargo_env(arch, "/t", false)),
                "{env:?}"
            );
        }
    }

    fn ends(guest: &Guest) -> Vec<Call> {
        guest
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::End { .. }))
            .collect()
    }

    #[test]
    fn a_runs_end_stops_its_units_then_lets_the_driver_end_or_keep_the_vm() {
        let guest = Guest::new();
        up_on(&guest, true).release();
        let calls = guest.calls();
        let stops: Vec<_> = guest::stop_units().into_iter().collect();
        assert!(
            matches!(&calls[..], [Call::Shell { argv: a, .. }, Call::Shell { argv: b, .. }, Call::End { booted_here: true, keep: false }] if *a == stops[0] && *b == stops[1]),
            "{calls:?}"
        );
        // --keep-up and `up`: kept, whoever booted it.
        let guest = Guest::new();
        up_on(&guest, false).end(true);
        assert_eq!(
            ends(&guest),
            [Call::End {
                booted_here: false,
                keep: true
            }]
        );
    }

    #[test]
    fn provisioning_runs_in_place_only_when_the_guests_stamp_is_another_plans() {
        let steps = guest::provision_steps();
        let want = guest::fingerprint(&steps);
        let current = Guest::new().answer(
            &["cat", guest::STAMP],
            Answer::Exited(0, format!("{want}\n")),
        );
        assert_eq!(provision(&up_on(&current, true)).ok(), Some(None));
        assert_eq!(current.calls().len(), 1, "only the stamp was read");
        // A stale stamp: every step, in order, through the driver, then the new stamp.
        let stale = Guest::new().answer(&["cat", guest::STAMP], Answer::Exited(0, "0123\n".into()));
        assert!(matches!(provision(&up_on(&stale, true)), Ok(Some(_))));
        let ran: Vec<Vec<String>> = stale
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Shell { argv, .. } => Some(argv),
                _ => None,
            })
            .collect();
        for (i, s) in steps.iter().enumerate() {
            assert_eq!(ran[1 + i], s.argv, "step {i}, {}", s.label);
        }
        assert!(
            stale.calls().iter().any(|c| matches!(c, Call::Shell { argv, stdin: Some(t), .. } if argv.last().map(String::as_str) == Some(guest::STAMP) && *t == format!("{want}\n"))),
            "the stamp is the plan's"
        );
    }

    #[test]
    fn a_step_that_a_signal_ended_never_started_or_was_lost_names_why_and_stops_provisioning() {
        for (answer, words) in [
            (
                Answer::Signaled(9),
                [
                    "(ended on signal 9)",
                    "(the guest command ended on signal 9)",
                ],
            ),
            (
                Answer::NotStarted("rustup: No such file or directory (os error 2)".into()),
                [
                    "(not started: rustup: No such file or directory (os error 2))",
                    "",
                ],
            ),
            (
                Answer::Lost(
                    "the guest stopped answering (no heartbeat for 60 s); VM stopped".into(),
                ),
                [
                    "(lost: the guest stopped answering (no heartbeat for 60 s); VM stopped)",
                    "",
                ],
            ),
        ] {
            // `rustup` alone is also one of apt's packages.
            let guest = Guest::new().answer(&["rustup", "toolchain"], answer);
            let fail = provision(&up_on(&guest, true)).unwrap_err();
            assert_eq!(fail.code, NOT_RUN);
            let msg = fail.msg.unwrap_or_default();
            assert!(
                msg.contains("provisioning step 'rust toolchain' failed")
                    && words.iter().all(|w| msg.contains(w)),
                "{msg}"
            );
            // No later step ran, and no stamp was written.
            assert!(!guest.calls().iter().any(
                |c| matches!(c, Call::Shell { argv, .. } if argv.iter().any(|a| a == guest::STAMP) && argv[0] == "sudo")
            ));
        }
    }

    #[test]
    fn up_keeps_the_vm_and_runs_nothing_more_in_the_guest() {
        let guest = Guest::new();
        up_on(&guest, true).keep();
        assert_eq!(
            guest.calls(),
            [Call::End {
                booted_here: true,
                keep: true
            }],
            "no unit is stopped again: the bring-up did that, as lima's up"
        );
        // The negative control: a run's end with --keep-up stops its units first.
        let guest = Guest::new();
        up_on(&guest, true).end(true);
        assert_eq!(guest.calls().len(), 1 + guest::stop_units().len());
    }

    fn out(code: Option<i32>, signal: Option<i32>) -> Out {
        Out {
            code,
            text: "before\n".into(),
            secs: 0.1,
            signal,
            not_started: None,
            lost: None,
        }
    }

    #[test]
    fn exec_names_each_end_that_was_not_an_exit_and_exits_apart() {
        let run = |o: &Out| {
            let mut said = Vec::new();
            let code = exec_exit(o, &mut |w| said.push(w.to_string()));
            (code.map_err(|f| (f.code, f.msg.unwrap_or_default())), said)
        };
        // The negative control: an exit is its own code, and nothing more is said.
        assert_eq!(run(&out(Some(7), None)), (Ok(7), vec![]));
        assert_eq!(run(&out(Some(0), None)), (Ok(0), vec![]));
        assert_eq!(
            run(&out(None, Some(9))),
            (
                Ok(137),
                vec!["linux exec: the command ended on signal 9".to_string()]
            )
        );
        let mut missing = out(None, None);
        missing.not_started = Some("/nonexistent: No such file or directory (os error 2)".into());
        assert_eq!(
            run(&missing),
            (
                Ok(127),
                vec![
                    "linux exec: the command was not started: /nonexistent: No such file or \
                     directory (os error 2)"
                        .to_string()
                ]
            )
        );
        let mut lost = out(None, None);
        lost.lost = Some("the guest stopped answering (no heartbeat for 60 s); VM stopped".into());
        let (code, said) = run(&lost);
        let (exit, msg) = code.unwrap_err();
        assert_eq!(exit, NOT_RUN);
        assert!(
            msg.contains("lost under the command: the guest stopped answering"),
            "{msg}"
        );
        assert!(said.is_empty());
        let mut limactl = out(None, None);
        limactl.text = "half\n(limactl ended on signal 15)\n".into();
        assert_eq!(
            run(&limactl),
            (
                Ok(1),
                vec!["linux exec: no exit status: (limactl ended on signal 15)".to_string()]
            )
        );
    }

    #[test]
    fn a_failures_tail_names_an_end_its_text_cannot() {
        let mut o = out(None, None);
        o.text = "a\nb\nc\n".into();
        // The negative control: an exit's tail is its text alone.
        assert_eq!(o.tail(2), "b\nc");
        o.not_started = Some("no cwd".into());
        assert_eq!(o.tail(2), "b\nc\n(not started: no cwd)");
        assert_eq!(o.status(), "not started: no cwd");
        o.lost = Some("gone".into());
        assert_eq!(o.tail(1), "c\n(lost: gone)");
        o.text.clear();
        assert_eq!(o.tail(5), "(lost: gone)");
        assert_eq!(out(None, Some(15)).status(), "ended on signal 15");
    }

    #[test]
    fn the_host_disk_and_version_lines_come_from_the_driver() {
        let mut guest = Guest::new();
        guest.host_disk = Some(crate::linux_driver::HostDisk {
            bytes: 5 << 30,
            path: "/c/vz.noindex/live/aterm-linux.img".into(),
        });
        let up = up_on(&guest, false);
        assert_eq!(
            up.guest.host_disk().map(|d| gib(d.bytes)),
            Some("5.0 GiB".into())
        );
        assert_eq!(up.guest.version(), "fake guest");
    }
}
