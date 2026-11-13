// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux runtime lane on macOS: a VM reached through a [`Driver`] (lima's `limactl` by
//! default; the lane's own VM through `aterm-vz-host` under `LAB_LINUX_DRIVER=vz`).

use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

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

/// What the VM is made with (`limactl create`), the same size on every host; `--memory`
/// boots it with less memory ([`memory_gib`]).
pub const CPUS: u32 = 8;
pub const MEMORY_GIB: u32 = 16;
pub const DISK_GIB: u32 = 60;

/// The least memory `--memory` boots the guest with, GiB: Ubuntu, the guest's toolchain and one
/// crate's build, which pages into the guest's swap ([`crate::linux_guest`]) past it.
pub const MEMORY_MIN_GIB: u32 = 4;

/// `--memory GIB`, when this process named it.
static MEMORY: OnceLock<u32> = OnceLock::new();

/// The memory this run boots lima's VM with, GiB: `--memory`'s, else [`MEMORY_GIB`].
#[must_use]
pub fn memory_gib() -> u32 {
    MEMORY.get().copied().unwrap_or(MEMORY_GIB)
}

/// `--memory`'s word: whole GiB from [`MEMORY_MIN_GIB`] to [`MEMORY_GIB`].
/// # Errors
/// Anything else (usage).
pub fn parse_memory(word: &str) -> Result<u32, Fail> {
    match word.parse::<u32>() {
        Ok(gib) if (MEMORY_MIN_GIB..=MEMORY_GIB).contains(&gib) => Ok(gib),
        _ => Err(Fail::usage(format!(
            "linux --memory needs whole GiB from {MEMORY_MIN_GIB} to {MEMORY_GIB}, not '{word}' ({LAB} linux help)"
        ))),
    }
}

/// `--memory`'s word into `cell`, this run's guest size: the first holds, and a later one that
/// names another size is refused, as a second `--driver` is.
/// # Errors
/// A word [`parse_memory`] refuses, or another size than `cell` holds (usage).
pub fn memory_in(cell: &OnceLock<u32>, word: &str) -> Result<u32, Fail> {
    let gib = parse_memory(word)?;
    let held = *cell.get_or_init(|| gib);
    if held == gib {
        Ok(gib)
    } else {
        Err(Fail::usage(format!(
            "linux --memory {gib}: this run's guest is {held} GiB (an earlier --memory) ({LAB} linux help)"
        )))
    }
}

/// `limactl edit`'s arguments that give the stopped VM `name` (as `state` reports it)
/// `gib` GiB of memory, or `None` when it has that already or is running: a running VM is
/// adopted at the size it booted with, and a run that adopts one claims no memory for it.
#[must_use]
pub fn memory_edit_args(name: &str, state: &VmState, gib: u32) -> Option<Vec<String>> {
    (state.status != "Running" && state.memory != u64::from(gib) << 30).then(|| {
        vec![
            "edit".into(),
            "--tty=false".into(),
            name.into(),
            format!("--memory={gib}"),
        ]
    })
}

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
targo --unverified lab linux [--driver lima|vz] [--gpu] [--memory GIB] [--no-wait] [--admit wait|now] <verb> — the Linux runtime lane: an
Ubuntu 24.04 aarch64 VM (Rosetta for x86_64), lima's by default; --driver vz or LAB_LINUX_DRIVER=vz runs every verb on the
lane's own VM instead (aterm-vz-host, the guest agent, the state in LAB_VZ_STATE or ~/Library/Caches/aterm/vz.noindex; it
needs a console session, since the framework's VM process opens a WindowServer connection at each boot); --gpu runs every
verb on the lane's GPU instance instead, lima's aterm-linux-gpu on krunkit (krunkit from Homebrew's slp/krun tap,
KosmicKrisp placed by `gpu-host`): the lane's provisioning for aarch64 (krunkit has no Rosetta, so --arch x86_64 is
refused), then the mainline 7.2.6 kernel and Mesa 26.3's Venus, and before any verb runs the guest's Vulkan must be Venus on
this Mac's GPU and its GL zink over it (else NOT RUN); cargo and aterm-gui run with Venus as the only Vulkan driver and zink
as GL (the display servers stay on llvmpipe). One run holds a VM at a time: a verb that finds it held waits for that run to
end, naming it (pid, verb, checkout, since when) as the wait starts and once a minute; --no-wait answers NOT RUN at once
instead, naming it. A verb that boots the VM first takes its turn to boot, waiting for a run that boots it to end (one run
of a VM claims its disk at a time), then claims the disk its boots write (what the last ones wrote, else 10 GiB
less what the VM's directory holds) and waits for room before it waits for the lane, a line a minute naming the need and the
holders; once it holds the lane, the boot claims the guest's 16 GiB and 8 vCPUs in this Mac's capacity ledger (`aterm pkg
capacity`) and waits while they do not fit with 8 GiB to spare beside them, naming the claims that hold the room once a
minute (NOT RUN at once only on a Mac too small for them); --admit now boots at once, saying what it lacks or would have
waited for. --memory GIB (4 to 16, lima's VM without --gpu) boots the guest with that much memory instead, and the boot
claims that: a run whose crates build in less fits a Mac in use; each later boot without it gives the guest 16 GiB again,
and a VM already up is adopted at the size it booted with. The guest's builds run as many jobs as cores the claim was granted (CARGO_BUILD_JOBS), and it keeps 16 GiB of swap, which a compile's peak pages into. A lima VM still running
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
                           the lane's proof: aterm-uds, aterm-pty, aterm-libc and amacs-files's tests,
                           the system readers' (aterm-sysprobe, aterm-vitals, aterm-diskwalk),
                           aterm-render's library tests, the libc oracle's conformance cells, and the windowing crates' and
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
                           chrome-headless-shell linux64 on SwiftShader, aarch64 Firefox under Xvfb and WebKit's
                           WPE, headless.
                           Exit 0 pass, 1 a row failed, 3 NOT RUN. Report and frames: $LAB_STATE/linux/
  vz-agent                 cross-build the vz guest agent (crates/aterm-vz-agent; STOCK EXCEPTION: the
                           checkout's stock pin, static aarch64-unknown-linux-musl, linked by its rust-lld)
                           and place it at ~/Library/Caches/aterm/vz.noindex/agent/<sha256>/; prints the
                           sha256. NOT RUN without the pin's musl std (targo --unverified verify provision
                           --linux)
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
            [flag, word, rest @ ..] if flag == "--memory" => {
                memory_in(&MEMORY, word)?;
                args = rest;
            }
            [flag] if flag == "--memory" => {
                return Err(Fail::usage(format!(
                    "linux --memory needs GiB ({LAB} linux help)"
                )));
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
    if MEMORY.get().is_some()
        && (crate::linux_gpu::on()
            || verb == "vz-smoke"
            || crate::linux_driver::kind()? == Kind::Vz)
    {
        return Err(Fail::usage(
            "linux --memory sizes lima's VM; the GPU instance and the vz arm boot at their own size",
        ));
    }
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
    let file = crate::util::read_regular_text(&repo.join(".git")).ok()?;
    let gitdir = repo.join(file.lines().next()?.strip_prefix("gitdir:")?.trim());
    let common = match crate::util::read_regular_text(&gitdir.join("commondir")) {
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
        format!("--memory={}", memory_gib()),
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

/// How long a command cut off on the GPU instance waits for lima to read the VM down
/// ([`left_running`]): lima reads a VM whose krunkit just died as running until its host agent
/// sees the death, which can come after the cut ssh's 255.
pub const LOSS_WAIT: Duration = Duration::from_secs(15);

/// Whether the VM left running within `wait`, `running` (lima's reading: running or not, `None`
/// where it reads none) asked every `pace` until then. A VM lima cannot read is not claimed down.
pub fn left_running(
    mut running: impl FnMut() -> Option<bool>,
    wait: Duration,
    pace: Duration,
) -> bool {
    let t0 = Instant::now();
    loop {
        match running() {
            Some(false) => return true,
            None => return false,
            Some(true) if t0.elapsed() >= wait => return false,
            Some(true) => std::thread::sleep(pace),
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
    /// Why the GPU instance went down under a command of this run ([`Lima::loss`]): every
    /// later command is lost with it, unrun.
    lost: OnceLock<String>,
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
            lost: OnceLock::new(),
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
        if self.lost.get().is_some() {
            return self.run(&["stop".into(), "--force".into(), self.vm.into()]);
        }
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
        if let Some(out) = self.after_loss() {
            return out;
        }
        let since = SystemTime::now();
        let out = self.shell_run(workdir, argv, stdin);
        self.loss(since, out)
    }

    fn shell_run(&self, workdir: &str, argv: &[String], stdin: Option<&str>) -> Out {
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
        if let Some(out) = self.after_loss() {
            return out;
        }
        let since = SystemTime::now();
        let out = self.shell_streamed_run(workdir, argv, prefix);
        self.loss(since, out)
    }

    fn shell_streamed_run(&self, workdir: &str, argv: &[String], prefix: &str) -> Out {
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
        let err_reader = child
            .stderr
            .take()
            .map(|e| std::thread::spawn(move || read_lines(e, MAX_GUEST_STREAM, |_| {})));
        let mut text = String::new();
        let mut cut = false;
        if let Some(out) = child.stdout.take() {
            let stdout = std::io::stdout();
            let read = read_lines(out, MAX_GUEST_STREAM, |line| {
                let mut lock = stdout.lock();
                let _ = writeln!(lock, "{prefix}{line}");
            });
            text = read.text;
            cut = read.cut;
            if cut {
                let _ = child.kill();
            }
        }
        if let Some(h) = err_reader {
            let read = h.join().unwrap_or_default();
            err_text = read.text;
            cut |= read.cut;
        }
        let status = child.wait();
        text.push_str(&err_text);
        if let Ok(status) = &status {
            text.push_str(&ended_on_signal(*status));
        }
        Out {
            // A verdict is judged on the text, so a run whose text was cut has none to give.
            code: status.ok().and_then(|s| s.code()).filter(|_| !cut),
            text,
            secs: t0.elapsed().as_secs_f64(),
            signal: None,
            not_started: None,
            lost: None,
        }
    }

    /// Why the GPU instance went down under a command of this run, when it did.
    #[must_use]
    pub fn lost(&self) -> Option<&str> {
        self.lost.get().map(String::as_str)
    }

    /// A command after the GPU instance went down: lost with it, unrun.
    fn after_loss(&self) -> Option<Out> {
        let why = self.lost.get()?;
        Some(Out {
            code: None,
            text: String::new(),
            secs: 0.0,
            signal: None,
            not_started: None,
            lost: Some(format!("{why} (earlier in this run; not run)")),
        })
    }

    /// `out` of a command begun at `since`, lost when it was cut off because the GPU instance
    /// went down (L-REALGPU's krunkit aborts, LR-3): its shell ended with no status or ssh's
    /// 255, and lima no longer reads the VM running. Why is macOS's krunkit crash report, by
    /// [`crate::linux_gpu::krunkit_lost`]; `limactl stop --force` then clears what the dead VM
    /// left, so the next `lab linux --gpu up` boots it again.
    fn loss(&self, since: SystemTime, mut out: Out) -> Out {
        let cut = matches!(out.code, None | Some(255)) && out.signal.is_none();
        if self.vm != crate::linux_gpu::VM || out.lost.is_some() || !cut {
            return out;
        }
        let running = || match self.state() {
            Ok(Some(vm)) => Some(vm.status == "Running"),
            _ => None,
        };
        if !left_running(running, LOSS_WAIT, Duration::from_millis(250)) {
            return out;
        }
        let reports = aterm_uds::account::home()
            .unwrap_or_default()
            .join(crate::linux_gpu::CRASH_REPORTS);
        let why = crate::linux_gpu::krunkit_lost(&reports, since, crate::linux_gpu::REPORT_WAIT);
        let cleared = self.run(&["stop".into(), "--force".into(), self.vm.into()]);
        let after = if cleared.ok() {
            format!(
                "; the VM is down (`limactl stop --force` cleared it; `{LAB} linux --gpu up` boots it again)"
            )
        } else {
            format!("; `limactl stop --force` failed: {}", cleared.tail(2))
        };
        out.lost = Some(self.lost.get_or_init(|| format!("{why}{after}")).clone());
        out
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
        .find_map(|f| {
            std::fs::symlink_metadata(state.dir.join(f))
                .ok()
                .filter(std::fs::Metadata::is_file)
        })
        .map(|m| m.blocks() * 512)
}

/// The host agent of the lima instance in `dir` (`ha.pid`): one process per boot.
#[must_use]
pub fn lima_agent(dir: &Path) -> Option<u32> {
    crate::util::read_regular_text(&dir.join("ha.pid"))
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
/// Taken for a boot ([`LaneLock::boot`]), it holds the VM's turn to boot and the boot's disk
/// claim too, let go, and what the VM's directory took kept, when the lock is.
pub struct LaneLock {
    held: named_lock::NamedLock,
    /// The lane and its boot's disk claim ([`claim_boot_disk`]).
    disk: Option<(&'static str, atpkg::admit_disk::Admitted)>,
    /// The VM's turn to boot ([`boot_turn_path`]), let go after the lane and the claim.
    turn: Option<named_lock::NamedLock>,
}

impl Drop for LaneLock {
    fn drop(&mut self) {
        if let Some((lane, held)) = self.disk.take() {
            boot_disk_done(lane, held, crate::vm_admit::granted());
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

/// The remedy beside what to free that a lane's claim refused as UNMET names: the lane's own
/// `--admit now`, which takes the claim at once.
pub const AT_ONCE: &str = "or boot at once by adding `--admit now` before the verb";

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

    /// Name the lane's VM's processes `pids` (its host agent or helper, QEMU, the process holding
    /// its disk: [`crate::vm_admit::vm_procs`]) in the lane's record, so a run waiting for the
    /// lane reads the guest's work with this run's own (`named_lock::NamedLock::credit`): launchd
    /// parents them, and this run's own processes while its guest works are an ssh client that
    /// moves only when the guest prints, or a relay in this process. A record that cannot be
    /// written is said, and the run goes on.
    pub fn credit(&mut self, pids: &[u32]) {
        if let Err(e) = self.held.credit(pids) {
            eprintln!(
                "aterm-lab: the lane's record does not name the VM's processes ({e}): a run \
                 waiting for the lane reads this run's own processes alone"
            );
        }
    }

    /// Take the lane for a run that boots the VM: the VM's turn to boot first
    /// ([`LaneLock::turn`]), then the boot's disk claimed ([`claim_boot_disk`]), so no run holds
    /// the lane while it waits for disk and none that holds disk waits behind one that does, then
    /// the lane ([`LaneLock::take`]). The claim is held, and the VM's directory measured, until
    /// the lock is let go at the run's end. A lima lane with no `limactl` is NOT RUN before any
    /// wait.
    /// # Errors
    /// As [`LaneLock::turn`], [`claim_boot_disk`] and [`LaneLock::take`], and lima's absence.
    pub fn boot() -> Result<LaneLock, Fail> {
        let kind = crate::linux_driver::kind()?;
        if kind == Kind::Lima {
            Lima::find()?;
        }
        let path = LaneLock::path()?;
        LaneLock::boot_with(
            "linux",
            || LaneLock::turn(&path, "linux"),
            || claim_boot_disk("linux", linux_boot_run(kind)?),
            || LaneLock::at(&path, "linux"),
        )
    }

    /// `lane` taken for a boot: `turn` takes the VM's turn to boot, `claim` then claims the
    /// boot's disk, and only then does `take` take the lane, the lock holding the turn and the
    /// claim until it is let go. Runs of one VM that queue for it so claim its disk one at a
    /// time, each once the run before it is over: one VM's boots write one directory in turn, and
    /// a run queued behind another claimed the same room again (2026-10-30: two runs of the
    /// Windows lane held 35 and 32.5 GiB for its one 33.8 GiB disk). A turn refused claims
    /// nothing; a claim refused takes no lane.
    /// # Errors
    /// `turn`'s, `claim`'s or `take`'s.
    pub(crate) fn boot_with(
        lane: &'static str,
        turn: impl FnOnce() -> Result<named_lock::NamedLock, Fail>,
        claim: impl FnOnce() -> Result<Option<atpkg::admit_disk::Admitted>, Fail>,
        take: impl FnOnce() -> Result<LaneLock, Fail>,
    ) -> Result<LaneLock, Fail> {
        let turn = turn()?;
        let disk = claim()?;
        let mut lock = take()?;
        lock.disk = disk.map(|d| (lane, d));
        lock.turn = Some(turn);
        Ok(lock)
    }

    /// The lane lock's path for this run's driver.
    /// # Errors
    /// The driver's, or where it keeps its VM cannot be read.
    fn path() -> Result<PathBuf, Fail> {
        Ok(match crate::linux_driver::kind()? {
            Kind::Lima => lima_namespace()?.lock_path(),
            Kind::Vz => crate::linux_vz_driver::lane_lock_path(&crate::linux_vz_driver::root()?),
        })
    }

    /// Take the lane, waiting for the run that holds it ([`LaneLock::at`]).
    /// # Errors
    /// `--no-wait` and another run holds it (NOT RUN, naming it), an interrupt while waiting, or
    /// the lock file cannot be made.
    pub fn take() -> Result<LaneLock, Fail> {
        LaneLock::at(&LaneLock::path()?, "linux")
    }

    /// Take the turn to boot the VM whose lane lock is at `lane_lock` ([`boot_turn_path`]) as
    /// this run, as [`LaneLock::at`] takes the lane: a boot of the VM that another run holds is
    /// waited for, or NOT RUN under `--no-wait`.
    /// # Errors
    /// [`LaneLock::at`]'s, for the turn.
    pub fn turn(lane_lock: &Path, lane: &str) -> Result<named_lock::NamedLock, Fail> {
        let (verb, wait) = RUN
            .get()
            .map(|r| (r.0.clone(), r.1))
            .unwrap_or_else(|| (format!("lab {lane}"), Wait::Block));
        let me = named_lock::Holder::me(&verb, Path::new(&repo()));
        LaneLock::named(
            &boot_turn_path(lane_lock),
            lane,
            TURN,
            &me,
            wait,
            &mut |line| println!("{line}"),
            &crate::stop::check,
        )
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
        LaneLock::named(path, lane, "the lane", me, wait, say, stopped).map(|held| LaneLock {
            held,
            disk: None,
            turn: None,
        })
    }

    /// [`LaneLock::at_as`] for the lock at `path`, which its holder's lines name as `holds`
    /// (`the lane`, or [`TURN`]).
    /// # Errors
    /// As [`LaneLock::at_as`].
    fn named(
        path: &Path,
        lane: &str,
        holds: &str,
        me: &named_lock::Holder,
        wait: Wait,
        say: &mut dyn FnMut(&str),
        stopped: &dyn Fn() -> Result<(), crate::stop::Stopped>,
    ) -> Result<named_lock::NamedLock, Fail> {
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
                        "{lane}: waiting for {}, which holds {holds} ({})",
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
            Ok(held) => Ok(held),
            Err(Refused::Held(holder)) => Err(not_run(format!(
                "{} holds {holds} ({}); --no-wait does not wait for it",
                holder_name(holder.as_ref()),
                path.display()
            ))),
            Err(Refused::Stopped { holder, signal }) => Err(Fail {
                stopped: signal
                    .inspect(|sig| crate::stop::note(*sig))
                    .or_else(|| stopped().err().map(|s| s.0)),
                ..not_run(format!(
                    "interrupted while waiting for {}, which holds {holds} ({}); this run took \
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

/// What a run waiting for [`boot_turn_path`]'s lock says its holder holds.
const TURN: &str = "the VM's turn to boot, whose disk one run claims at a time";

/// The lock a boot of the VM whose lane lock is at `lane_lock` holds from before it claims its
/// disk until its run ends ([`LaneLock::boot_with`]): beside it, `boot` in place of its `lane`
/// (`.aterm-linux.boot.lock`, the Windows lane's `.boot.lock`).
#[must_use]
pub fn boot_turn_path(lane_lock: &Path) -> PathBuf {
    let name = lane_lock
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    lane_lock.with_file_name(
        name.strip_suffix("lane.lock")
            .map_or_else(|| format!("{name}.boot"), |stem| format!("{stem}boot.lock")),
    )
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
        scope: String::new(),
        dirs: vec![dir],
        record,
        cold,
        on: base.to_path_buf(),
        stuck_ends: None,
        scratch: false,
        machine: None,
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
/// NOT RUN: the need is more than the volume holds above its floor, or than it has free above its
/// floor once the relief ran, which no wait admits.
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
        Err(e @ atpkg::admit_disk::Error::Unmet { .. }) => Err(not_run(format!(
            "{lane}: the boot's disk claim was refused: {e}, {AT_ONCE}; this run booted nothing"
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
/// boot and said where the run's VM was admitted (`ran`). A run whose VM was never admitted (an
/// interrupt or a NOT RUN ended its wait) booted nothing, so what it took is no boot's and is not
/// kept: kept, a first run's few KiB would be the next boot's whole claim.
fn boot_disk_done(lane: &str, held: atpkg::admit_disk::Admitted, ran: bool) {
    let claimed = held.estimate;
    if !ran {
        drop(held);
        println!("{lane}: disk: claimed {claimed}; this run booted nothing, so none of it is kept");
        return;
    }
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
/// ([`crate::linux_driver::kind`]), under `lane`, which this run holds: once the VM is up, its
/// processes are named in the lane's record ([`LaneLock::credit`]), before any guest step.
/// # Errors
/// NOT RUN: no lima (or the vz arm's gates), or the VM could not be made, booted or
/// provisioned.
pub fn bring_up(lane: &mut LaneLock) -> Result<Up, Fail> {
    let repo = repo();
    let (guest, booted_here, timings, admitted, booter) = match crate::linux_driver::kind()? {
        Kind::Lima => bring_up_lima(&repo, lane)?,
        Kind::Vz => bring_up_vz(&repo, lane)?,
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
/// bring-up again on a clone of the sealed disk, under the claim the bake was admitted with;
/// each VM opened is named in `lane`'s record.
fn bring_up_vz(repo: &str, lane: &mut LaneLock) -> Result<Brought, Fail> {
    let mut timings = Vec::new();
    let mut held = None;
    for _ in 0..2 {
        let opened = crate::linux_vz_driver::open(Path::new(repo), held.take())?;
        lane.credit(&opened.procs);
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
            admitted,
            ..
        } = up
        else {
            unreachable!("the bake's guest is the vz arm's");
        };
        let sealed = crate::linux_vz_driver::finish_bake(*vz, partial)?;
        println!("linux: baked {}", sealed.display());
        timings.push(Step::done("bake", t0.elapsed().as_secs_f64()));
        held = Some(admitted);
    }
    Err(not_run(
        "the bake sealed its disk, but the bring-up after it found no baked disk",
    ))
}

/// lima's `create`, `edit` (the mounts, then a stopped VM's memory to this run's size:
/// [`memory_edit_args`]), and `start`, behind the driver: a running VM a dead
/// one-shot run left is stopped first, as `down` stops it, and a boot is admitted
/// ([`crate::vm_admit`]) and recorded before `start`; the VM up, its processes are named in
/// `lane`'s record.
fn bring_up_lima(repo: &str, lane: &mut LaneLock) -> Result<Brought, Fail> {
    let mut lima = Lima::find()?;
    let vm_name = lima.vm;
    let booter = booter_path()?;
    let mut timings = Vec::new();
    let mut state = lima.state().map_err(not_run)?;
    if state.is_none() {
        println!(
            "linux: making {vm_name} ({CPUS} CPUs, {} GiB, disk <= {DISK_GIB} GiB, {TEMPLATE}) …",
            memory_gib()
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
    let gpu = crate::linux_gpu::on();
    let gib = if gpu { MEMORY_GIB } else { memory_gib() };
    if let Some(edit) = (!gpu)
        .then(|| memory_edit_args(vm_name, &vm, gib))
        .flatten()
    {
        println!("linux: giving {vm_name} {gib} GiB of memory for this boot …");
        let out = lima.run(&edit);
        if !out.ok() {
            return Err(not_run(format!(
                "limactl edit (the memory) failed: {}",
                out.tail(5)
            )));
        }
        vm = lima
            .state()
            .map_err(not_run)?
            .ok_or_else(|| not_run("the VM went away"))?;
    }
    let booted_here = vm.status != "Running";
    let mut admitted =
        crate::vm_admit::admit(&crate::vm_admit::Guest::lima(vm_name, gib), booted_here)?;
    let mut recorded = None;
    if booted_here {
        recorded = crate::vm_admit::record_boot("linux", &booter);
        println!("linux: booting {vm_name} (mount: {repo}, read-only) …");
        let out = lima.run(&["start".into(), "--tty=false".into(), vm_name.into()]);
        timings.push(Step::done("boot", out.secs));
        if !out.ok() {
            let running = matches!(lima.state(), Ok(Some(vm)) if vm.status == "Running");
            let revived = after_failed_start(
                running,
                &mut |script| lima.shell("/", &["sh".into(), "-c".into(), script.into()], None),
                &mut || lima.run(&["stop".into(), "--force".into(), vm_name.into()]),
            );
            match revived {
                Ok(line) => println!("linux: {line}"),
                Err(after) => {
                    if running {
                        crate::vm_admit::clear(&booter);
                    }
                    return Err(not_run(format!(
                        "limactl start failed: {}{after}",
                        out.tail(8)
                    )));
                }
            }
        }
    }
    let procs = lima_procs(&vm.dir);
    if booted_here {
        admitted.credit("linux", &procs);
        crate::vm_admit::record_vm(recorded.as_deref(), lima_agent(&vm.dir));
    }
    lane.credit(&procs);
    Ok((Driver::Lima(lima), booted_here, timings, admitted, recorded))
}

/// lima's guest agent, which a boot installs from its cidata and systemd runs.
const GUEST_AGENT: &str = "lima-guestagent";

/// What a run does with a VM `limactl start` failed on (`running`: lima reads it running), so a
/// failed start never leaves a VM up that nothing uses. LR-3: a VM that went down with its
/// writes unflushed (a krunkit abort) keeps a guest agent binary that is garbage, so the next
/// boot's systemd fails it (`Exec format error`) until it stops retrying, before lima's boot
/// scripts install the agent afresh; `limactl start` then ends degraded on a guest that is
/// otherwise up. That unit is reset and restarted (`shell` runs a script in the guest) and the
/// boot goes on, saying so; any other failure stops the VM (`stop`). `Err` is what the NOT RUN
/// adds after limactl's own words.
fn after_failed_start(
    running: bool,
    shell: &mut dyn FnMut(&str) -> Out,
    stop: &mut dyn FnMut() -> Out,
) -> Result<String, String> {
    if !running {
        return Err(String::new());
    }
    if shell(&format!("systemctl is-failed --quiet {GUEST_AGENT}")).ok() {
        let restarted = shell(&format!(
            "sudo systemctl reset-failed {GUEST_AGENT} && sudo systemctl restart {GUEST_AGENT} \
             && systemctl is-active --quiet {GUEST_AGENT}"
        ));
        if restarted.ok() {
            return Ok(format!(
                "lima's guest agent had failed at boot (as after a loss cut the last boot's \
                 writes off); {GUEST_AGENT} reset and restarted, the boot goes on"
            ));
        }
    }
    let stopped = stop();
    Err(if stopped.ok() {
        "; the VM it left running is stopped (`limactl stop --force`)".to_string()
    } else {
        format!(
            "; `limactl stop --force` of the VM it left running failed: {}",
            stopped.tail(2)
        )
    })
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
                // Package managers can print the actionable error before a
                // long unpack/configure tail. Preserve the driver's output.
                out.tail(usize::MAX)
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
    let mut lane = LaneLock::boot()?;
    let up = bring_up(&mut lane)?;
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
    let mut lane = LaneLock::boot()?;
    let up = bring_up(&mut lane)?;
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
    let mut lane = LaneLock::boot()?;
    let up = bring_up(&mut lane)?;
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

/// A run on the scripted guest `guest`, for the lane's tests.
#[cfg(test)]
pub(crate) fn fake_up(guest: &crate::linux_driver::fake::Guest, booted_here: bool) -> Up {
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

/// The most of one stream of a guest run [`read_lines`] reads. A verbose test run reaches tens of
/// MiB (docs/measured); this stops a runaway printer, which no other bound on a run's rows does.
const MAX_GUEST_STREAM: usize = 256 * 1024 * 1024;

/// The longest piece of a line [`read_lines`] holds at once.
const MAX_LINE_PIECE: u64 = 1024 * 1024;

/// What [`read_lines`] read.
#[derive(Default)]
struct ReadLines {
    /// Every line read, whole up to where the read stopped.
    text: String,
    /// The stream passed its limit, so `text` is not all of it and says so on its last line.
    cut: bool,
}

/// Read `from` to its end or until the next line would pass `limit` bytes, handing `each` every
/// line (a longer one in pieces of [`MAX_LINE_PIECE`], bytes that are not UTF-8 replaced). What
/// is returned is therefore always a prefix of the stream. Stopping drops the reader, closing the
/// writer's pipe, and a caller must not read a clean verdict from text that was cut.
fn read_lines(from: impl Read, limit: usize, mut each: impl FnMut(&str)) -> ReadLines {
    let mut reader = BufReader::new(from);
    let mut read = ReadLines::default();
    let mut piece = Vec::new();
    loop {
        piece.clear();
        match reader
            .by_ref()
            .take(MAX_LINE_PIECE)
            .read_until(b'\n', &mut piece)
        {
            Ok(0) | Err(_) => return read,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&piece);
        let line = line.strip_suffix('\n').unwrap_or(&line);
        each(line);
        if read.text.len().saturating_add(line.len()).saturating_add(1) > limit {
            read.cut = true;
            read.text.push_str(&format!(
                "[output passed {limit} bytes: the read stopped here]\n"
            ));
            return read;
        }
        read.text.push_str(line);
        read.text.push('\n');
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
