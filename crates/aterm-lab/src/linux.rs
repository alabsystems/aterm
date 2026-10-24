// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux runtime lane on macOS via Lima VM (`limactl`).

use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use aterm_uds::spawnfd::Stdio;

use crate::linux_guest::{self as guest, Arch, Display};
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

/// The guest-local directory every build of the lane writes, under the guest user's home (never a host mount); the end of a run (a proof, a one-shot `exec`, `down`) and `clean` empty it, and a run that finds another checkout's output in it empties it first ([`BUILT_FROM`]).
pub const GUEST_DIR: &str = "aterm-lane";

/// The file in [`GUEST_DIR`] that names the checkout whose build output it
/// holds. Cargo hashes a path package by its path inside the workspace and
/// keeps its dep-info relative to it, so the output another checkout left
/// (`--keep-build`, or an `up` never taken `down`) reads as fresh to this
/// one's build, which then links that checkout's units.
pub const BUILT_FROM: &str = "built-from";

/// Exit status of a lane that could not run (no lima, no VM, no display): never a pass.
pub const NOT_RUN: i32 = 3;

/// The verbs, as `linux help` prints them.
pub const USAGE: &str = "\
targo --unverified lab linux <verb> — the Linux runtime lane: a lima VM (vz, Ubuntu 24.04 aarch64, Rosetta for x86_64)
  up                       make the VM on first use, mount this checkout read-only, boot, provision; leaves it running
  down                     remove the guest's build output and stop the VM (it is kept; the next run boots it in ~10 s)
  status                   lima, the VM, its mount, provisioning and disk use (guest and host)
  exec [--arch aarch64|x86_64] [--display xvfb|weston|mutter|xwayland|weston-x11] [--] CMD...
                           run CMD in the guest at this checkout's path with the arch's cargo environment (stock
                           Rust {rust}; x86_64: CARGO_BUILD_TARGET and the cross linker, run under Rosetta), under
                           the display when one is named, with the session bus and at-spi2-core up; the build
                           output is kept for the next `exec` under `up`, else removed when CMD ends
  proof [--arch A]... [--display D]... [--weston-renderer gl|pixman] [--keep-build] [--keep-up]
                           the lane's proof: aterm-uds, aterm-pty and aterm-libc's tests, the libc
                           oracle's conformance cells, and the windowing crates' and the winit fork's tests
                           with the displays up, on each arch; then aterm-gui under each
                           display (and --headless):
                           a session over its control socket, typed, resized, `status` read, a frame kept;
                           then, under xvfb and weston, the display killed under a running aterm-gui, and a
                           person's quit as their control (`gone`); then the accessibility runtime (`a11y`);
                           then the stats Deck's layout rows (`deck`); then LW.4's baseline replayed (below)
                           under every screen of those displays (weston's include weston-x11, the same weston on
                           an Xvfb window). Every session but the Deck's holds the Deck off ([sys] stats_bars =
                           \"off\"): its figures redraw every second, which races a frame held to the server's.
                           Exit 0 pass, 1 a row failed, 3 NOT RUN. Report and frames: $LAB_STATE/linux/
  gone [--arch A]... [--display D]... [--wgpu-oracle] [--weston-renderer gl|pixman] [--keep-build] [--keep-up]
                           the proof's display-gone rows alone (xvfb and weston unless named): the display's
                           servers killed with SIGKILL under a running aterm-gui, at rest, mid-resize (the
                           server frozen until aterm's requests wait in it) and, on X11, mid-capture (mid-resize
                           on a turn that owes the crash journal a capture, which asks the window's position),
                           after which aterm-gui must exit by
                           itself, nonzero and without a panic, say that its display connection ended, and
                           leave session.toml holding its tab's folder and no crash journal; and a person's
                           quit (`invoke Quit`, the display up), which must exit 0, say nothing about the
                           display and save the same layout; --wgpu-oracle builds aterm-gui with aterm-gpu's
                           wgpu-oracle, so its windows draw on wgpu, and a Wayland mid-resize row must then
                           show the resize's surface configure rejected inside its error scope
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
  baseline [--arch A]... [--screen S]... [--record | --wgpu-oracle] [--keep-build] [--keep-up]
                           record a scripted aterm-gui session under each screen (xvfb, xvfb-dpi144, weston,
                           weston-scale2, mutter, mutter-scale1.5, xwayland, weston-x11): dims and scale, resizes
                           and cast drift, clipboard and primary, key repeat, compose, XIM (a dead key, and a
                           pinyin composition through ibus-libpinyin: its preedit, spot and commit) and an XDND
                           drop (X11), a Wayland drop and the frame's gestures (a seat: Mutter's RemoteDesktop, or
                           XTEST into weston's x11 backend), aterm-gui --cpu typed and resized on weston-x11's seat
                           with each frame held to weston's output, the AT-SPI tree and event stream, present
                           percentiles and the item the slowest present ended in, and the stack under it; replay
                           it against the recordings committed in crates/aterm-lab/linux-baseline/ (exit 0 same
                           behaviour, 1 it moved, 3 NOT RUN), or with --record write them there; --wgpu-oracle
                           replays with aterm-gui built with aterm-gpu's wgpu-oracle, drawing on wgpu (GW.2's
                           Vulkan arm) instead of the GL arm
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
  clean                    remove the guest's build output and trim the VM disk (its blocks go back to this Mac)
  destroy                  delete the VM and its disk (`limactl delete`); the next `up` makes it again
";

/// `linux <verb> …`.
/// # Errors
/// A usage error, or a step that failed.
pub fn cmd_linux(args: &[String]) -> Res {
    let verb = args.first().map_or("help", String::as_str);
    let rest = args.get(1..).unwrap_or_default();
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

/// What one guest command did.
#[derive(Debug)]
pub struct Out {
    /// `None`: killed by a signal, or never started.
    pub code: Option<i32>,
    /// stdout, then stderr (merged in order by [`Lima::shell_streamed`]).
    pub text: String,
    pub secs: f64,
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
        match self.code {
            Some(255) if self.text.trim().is_empty() => "exit 255, nothing said: a guest command that a signal ended, or one that exited 255".into(),
            Some(code) => format!("exit {code}"),
            None => "no exit status".into(),
        }
    }

    /// The output's last `n` lines, for a failure's detail.
    #[must_use]
    pub fn tail(&self, n: usize) -> String {
        let lines: Vec<&str> = self.text.lines().collect();
        lines[lines.len().saturating_sub(n)..].join("\n")
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

/// The `limactl` this Mac has.
pub struct Lima {
    pub bin: PathBuf,
}

impl Lima {
    /// Locate `limactl` on this Mac.
    /// # Errors
    /// No `limactl`: the lane cannot run here.
    pub fn find() -> Result<Lima, Fail> {
        if !cfg!(target_arch = "aarch64") {
            return Err(not_run(
                "the lane needs Apple silicon (lima's vz VM with Rosetta for x86_64)",
            ));
        }
        if !Path::new("/Library/Apple/usr/libexec/oah/RosettaLinux").exists() {
            return Err(not_run(
                "Rosetta for Linux is not installed (softwareupdate --install-rosetta)",
            ));
        }
        let named = std::env::var("LAB_LIMACTL").ok().filter(|p| !p.is_empty());
        let pinned = aterm_uds::host::tooling_dir()
            .and_then(|t| aterm_uds::host::tool("limactl").placed_in(&t))
            .map(|p| p.to_string_lossy().into_owned());
        pick_limactl(named.as_deref(), pinned.as_deref(), crate::sys::executable).map(|bin| Lima {
            bin: PathBuf::from(bin),
        })
    }

    fn command(&self) -> aterm_uds::spawnfd::Command {
        let mut c = aterm_uds::spawnfd::command(&self.bin);
        c.stdin(Stdio::null());
        c
    }

    /// Run `limactl ARGS`, capturing what it says.
    #[must_use]
    pub fn run(&self, args: &[String]) -> Out {
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
            },
            Err(e) => Out {
                code: None,
                text: format!("{} could not start: {e}", self.bin.display()),
                secs,
            },
        }
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
        Ok(parse_list(&out.text, VM))
    }

    fn shell_argv(workdir: &str, argv: &[String]) -> Vec<String> {
        let mut a = vec![
            "shell".to_string(),
            format!("--workdir={workdir}"),
            VM.to_string(),
            "--".to_string(),
        ];
        a.extend(argv.iter().cloned());
        a
    }

    /// Run `argv` in the guest from `workdir`, capturing its output; `stdin` is written to it when given.
    #[must_use]
    pub fn shell(&self, workdir: &str, argv: &[String], stdin: Option<&str>) -> Out {
        let t0 = Instant::now();
        let mut cmd = self.command();
        cmd.args(Self::shell_argv(workdir, argv))
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
            },
            Err(e) => Out {
                code: None,
                text: format!("waiting for limactl shell: {e}"),
                secs,
            },
        }
    }

    /// Run `argv` in the guest with its stderr merged into its stdout there (so cargo's `Running` lines and libtest's lines keep their order), each line shown here as it comes, prefixed, and kept.
    #[must_use]
    pub fn shell_streamed(&self, workdir: &str, argv: &[String], prefix: &str) -> Out {
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
            .args(Self::shell_argv(workdir, &merged))
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
        }
    }

    /// Copy a guest file to this Mac.
    #[must_use]
    pub fn copy_out(&self, guest_path: &str, host_path: &Path) -> Out {
        self.run(&[
            "copy".into(),
            format!("{VM}:{guest_path}"),
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
            format!("{VM}:{guest_parent}/"),
        ])
    }

    /// Copy the guest directory `guest_dir` into `host_parent` on this Mac, where it lands under its own name.
    #[must_use]
    pub fn copy_dir_out(&self, guest_dir: &str, host_parent: &Path) -> Out {
        self.run(&[
            "copy".into(),
            "-r".into(),
            format!("{VM}:{guest_dir}"),
            format!("{}/", host_parent.to_string_lossy()),
        ])
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

/// `n` bytes in GiB, one decimal.
#[must_use]
pub fn gib(n: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size shown to one decimal")]
    let g = n as f64 / f64::from(1u32 << 30);
    format!("{g:.1} GiB")
}

/// The one lane run of this Mac: `flock` on [`lane_lock_path`], released by `LOCK_UN` before the file closes (clippy.toml's flock rule).
pub struct LaneLock {
    file: std::fs::File,
}

impl LaneLock {
    /// Take the lane, or say who holds it.
    /// # Errors
    /// Another run holds it (NOT RUN), or the lock file cannot be made.
    pub fn take() -> Result<LaneLock, Fail> {
        let path = lane_lock_path(
            std::env::var("LIMA_HOME").ok().as_deref(),
            paths().need_home().map_err(Fail::die)?,
        );
        LaneLock::at(&path, "linux")
    }

    /// Acquire the lane lock at `path` for `lane`.
    /// # Errors
    /// Another run holds it (NOT RUN), or the lock file cannot be made.
    pub fn at(path: &Path, lane: &str) -> Result<LaneLock, Fail> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Fail::die(format!("{}: {e}", dir.display())))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| Fail::die(format!("{}: {e}", path.display())))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Drop releases by File::unlock before the close"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(LaneLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(not_run(format!(
                "another lane run holds the VM ({}); wait for it, or `{LAB} {lane} status`",
                path.display()
            ))),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(Fail::die(format!("{}: {e}", path.display())))
            }
        }
    }
}

impl Drop for LaneLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The lane lock: one per VM, so beside lima's instances (`$LIMA_HOME`, else `~/.lima`), never in a checkout.
#[must_use]
pub fn lane_lock_path(lima_home: Option<&str>, home: &str) -> PathBuf {
    let base = match lima_home.filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => Path::new(home).join(".lima"),
    };
    base.join(format!(".{VM}.lane.lock"))
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
    pub lima: Lima,
    /// The guest user's home and `XDG_RUNTIME_DIR`.
    pub home: String,
    pub runtime: String,
    pub booted_here: bool,
    /// Reserved on the first display start, once per run. Both success and
    /// failure are retained, so a failed reservation cannot fall back to a
    /// shared path.
    config: OnceLock<Result<guest::Config, String>>,
    /// What it took, in order.
    pub timings: Vec<Step>,
    /// The renderer every weston the lane starts here draws with: `gl` unless a run names `pixman` (`--weston-renderer`).
    pub weston_renderer: &'static str,
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
    /// The guest-local build directory ([`GUEST_DIR`]).
    #[must_use]
    pub fn lane_dir(&self) -> String {
        format!("{}/{GUEST_DIR}", self.home)
    }

    /// Run `argv` in the guest, captured, from its home.
    #[must_use]
    pub fn sh(&self, argv: &[String]) -> Out {
        self.lima.shell(&self.home, argv, None)
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
            .lima
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
        clean_guest_build(&self.lima, &self.home)
    }

    /// Stop the lane's units in the guest (a display, an aterm).
    pub fn stop_units(&self) {
        for argv in guest::stop_units() {
            let _ = self.sh(&argv);
        }
    }

    /// Stop the VM when this run booted it.
    pub fn release(self) {
        self.stop_units();
        if self.booted_here {
            let out = self.lima.run(&["stop".into(), VM.into()]);
            if out.ok() {
                println!("linux: VM stopped (this run booted it)");
            } else {
                eprintln!("aterm-lab: linux: limactl stop failed: {}", out.tail(5));
            }
        }
    }
}

fn clean_guest_build(lima: &Lima, home: &str) -> Result<(), String> {
    let dir = format!("{home}/{GUEST_DIR}");
    let out = lima.shell(home, &["rm".into(), "-rf".into(), dir], None);
    if !out.ok() {
        return Err(out.tail(5));
    }
    let _ = lima.shell(home, &["sudo".into(), "fstrim".into(), "/".into()], None);
    Ok(())
}

fn guest_path_var(lima: &Lima, var: &str) -> Option<String> {
    let out = lima.shell("/", &["printenv".into(), var.into()], None);
    out.ok()
        .then(|| out.text.trim().to_string())
        .filter(|s| s.starts_with('/'))
}

/// Create, mount, boot, and provision the lane VM as needed.
/// # Errors
/// NOT RUN: no lima, or the VM could not be made, booted or provisioned.
pub fn bring_up() -> Result<Up, Fail> {
    let lima = Lima::find()?;
    let repo = repo();
    let mut timings = Vec::new();
    let mut state = lima.state().map_err(not_run)?;
    if state.is_none() {
        println!(
            "linux: making the VM ({CPUS} CPUs, {MEMORY_GIB} GiB, disk <= {DISK_GIB} GiB, {TEMPLATE}) …"
        );
        let out = lima.run(&create_args());
        timings.push(Step::done("create", out.secs));
        if !out.ok() {
            return Err(not_run(format!("limactl create failed: {}", out.tail(8))));
        }
        state = lima.state().map_err(not_run)?;
    }
    let Some(mut vm) = state else {
        return Err(not_run("limactl create left no VM"));
    };
    let mounts = lane_mounts(&repo);
    if !mounted_exactly(&vm, &mounts) {
        if vm.status == "Running" {
            let out = lima.run(&["stop".into(), VM.into()]);
            if !out.ok() {
                return Err(not_run(format!("limactl stop failed: {}", out.tail(5))));
            }
        }
        let out = lima.run(&[
            "edit".into(),
            "--tty=false".into(),
            VM.into(),
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
    if booted_here {
        println!("linux: booting {VM} (mount: {repo}, read-only) …");
        let out = lima.run(&["start".into(), "--tty=false".into(), VM.into()]);
        timings.push(Step::done("boot", out.secs));
        if !out.ok() {
            return Err(not_run(format!("limactl start failed: {}", out.tail(8))));
        }
    }
    let (home, runtime) = (
        guest_path_var(&lima, "HOME"),
        guest_path_var(&lima, "XDG_RUNTIME_DIR"),
    );
    let mut up = Up {
        lima,
        home: home.clone().unwrap_or_default(),
        runtime: runtime.clone().unwrap_or_default(),
        booted_here,
        config: OnceLock::new(),
        timings,
        weston_renderer: "gl",
    };
    if home.is_none() || runtime.is_none() {
        up.release();
        return Err(not_run("the guest named no HOME or XDG_RUNTIME_DIR"));
    }
    match provision(&up) {
        Ok(Some(secs)) => up.timings.push(Step::done("provision", secs)),
        Ok(None) => {}
        Err(f) => {
            up.release();
            return Err(f);
        }
    }
    if let Some(step) = reset_rosetta_cache(&up) {
        up.timings.push(step);
    }
    match claim_build_output(&up, &repo) {
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

/// What the guest's build directory holds for the checkout `repo`, from
/// whether [`GUEST_DIR`] exists and what its [`BUILT_FROM`] names.
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

/// Ensure [`GUEST_DIR`] belongs to `repo`, removing any foreign build output first.
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
        .lima
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
        let out = up.lima.shell(&up.home, &s.argv, s.stdin.as_deref());
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

fn provision(up: &Up) -> Result<Option<f64>, Fail> {
    let steps = guest::provision_steps();
    let want = guest::fingerprint(&steps);
    let have = up.sh(&["cat".into(), guest::STAMP.into()]);
    if have.ok() && have.text.trim() == want {
        return Ok(None);
    }
    println!("linux: provisioning the guest (plan {want}) …");
    let t0 = Instant::now();
    for s in &steps {
        println!("linux:   {} …", s.label);
        let out = up.lima.shell(&up.home, &s.argv, s.stdin.as_deref());
        if !out.ok() {
            return Err(not_run(format!(
                "provisioning step '{}' failed:\n{}",
                s.label,
                out.tail(15)
            )));
        }
    }
    let stamp_dir = Path::new(guest::STAMP)
        .parent()
        .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    let made = up.sh(&["sudo".into(), "mkdir".into(), "-p".into(), stamp_dir]);
    let wrote = up.lima.shell(
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
    let _lock = LaneLock::take()?;
    let up = bring_up()?;
    for step in &up.timings {
        match &step.failed {
            None => println!("linux: {} {:.1} s", step.name, step.secs),
            Some(why) => println!("linux: {} FAILED: {why}", step.name),
        }
    }
    println!(
        "linux: {VM} is up; `{LAB} linux exec -- CMD` runs in it, `{LAB} linux down` cleans and stops it"
    );
    Ok(0)
}

fn cmd_down() -> Res {
    let lima = Lima::find()?;
    let _lock = LaneLock::take()?;
    match lima.state().map_err(Fail::die)? {
        None => println!("linux: there is no {VM} VM"),
        Some(vm) if vm.status != "Running" => println!("linux: {VM} is {}", vm.status),
        Some(_) => {
            match guest_path_var(&lima, "HOME").map(|home| clean_guest_build(&lima, &home)) {
                Some(Ok(())) => println!("linux: the guest's build output removed, disk trimmed"),
                Some(Err(e)) => eprintln!("aterm-lab: linux: build output NOT removed: {e}"),
                None => eprintln!("aterm-lab: linux: the guest named no HOME; build output kept"),
            }
            let out = lima.run(&["stop".into(), VM.into()]);
            if !out.ok() {
                return Err(Fail::die(format!("limactl stop failed: {}", out.tail(5))));
            }
            println!("linux: {VM} stopped");
        }
    }
    Ok(0)
}

fn cmd_status() -> Res {
    let lima = Lima::find()?;
    println!("limactl   {} ({})", lima.bin.display(), lima.version());
    let Some(vm) = lima.state().map_err(Fail::die)? else {
        println!("vm        none (`{LAB} linux up` makes it)");
        return Ok(0);
    };
    println!(
        "vm        {VM} {} — {} CPUs, {}, disk <= {}",
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
    if vm.status == "Running" {
        let stamp = lima.shell("/", &["cat".into(), guest::STAMP.into()], None);
        let want = guest::fingerprint(&guest::provision_steps());
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
    let _lock = LaneLock::take()?;
    let up = bring_up()?;
    let mut env = guest::cargo_env(ask.arch, &format!("{}/target", up.lane_dir()), true);
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
        .lima
        .shell_streamed(&repo(), &guest::with_env(&env, &ask.argv), "");
    if up.booted_here
        && let Err(e) = up.clean_build()
    {
        eprintln!("aterm-lab: linux: build output NOT removed: {e}");
    }
    up.release();
    Ok(out.code.unwrap_or(1))
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
        let out = up.lima.shell(&up.home, &step.argv, step.stdin.as_deref());
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
            let out = up.lima.shell(&up.home, &step.argv, step.stdin.as_deref());
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
    let _lock = LaneLock::take()?;
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
    let lima = Lima::find()?;
    if lima.state().map_err(Fail::die)?.is_none() {
        println!("linux: there is no {VM} VM");
        return Ok(0);
    }
    let out = lima.run(&[
        "delete".into(),
        "--force".into(),
        "--tty=false".into(),
        VM.into(),
    ]);
    if !out.ok() {
        return Err(Fail::die(format!("limactl delete failed: {}", out.tail(5))));
    }
    println!("linux: {VM} deleted with its disk");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn every_checkout_takes_the_one_lock_beside_the_vm() {
        let mine = lane_lock_path(None, "/Users//u");
        assert_eq!(mine, PathBuf::from("/Users//u/.lima/.aterm-linux.lane.lock"));
        assert_eq!(
            lane_lock_path(Some(""), "/Users//u"),
            mine,
            "an empty LIMA_HOME is unset"
        );
        assert_eq!(
            lane_lock_path(Some("/v/lima"), "/Users//u"),
            PathBuf::from("/v/lima/.aterm-linux.lane.lock")
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
}
