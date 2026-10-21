// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The edges LW.5-LW.7 left untested (`docs/AUDIT-third-party-2026-10-15.md`,
//! step L-HEADER-LW7): rows of `linux proof` on each arch, and `linux
//! edges`, which runs them alone. Each row drives aterm-gui where the edge
//! is and holds what it does there to a control the same row runs, so a
//! pass cannot come from the lane alone.
//!
//! On X11 (Xvfb):
//!
//! * **Resource files that `#include`** ([`Edge::XresInclude`]): no
//!   `RESOURCE_MANAGER` (`xrdb -remove`), and a `~/.Xresources` whose
//!   `#include "xres/main"` includes `dpi` beside itself, which sets
//!   `Xft.dpi: 144`, while a decoy `~/dpi` (what a loader resolving from HOME
//!   would read) sets 192. aterm-gui must draw at scale 1.5; with the
//!   `#include` line taken out, at 1.
//! * **A server without RandR, XInput 2 or XKB** ([`Edge::NoRandr`],
//!   [`Edge::NoXinput2`], [`Edge::NoXkb`]): Xvfb's `-extension RANDR`, and
//!   `lane-bus xhide`, a proxy that answers `QueryExtension` for
//!   XInputExtension or XKEYBOARD as absent (Xvfb cannot turn those off).
//!   aterm-gui must refuse by name and exit 1, never panic. Its control
//!   ([`Edge::XProxy`]): the same proxy hiding nothing, through which
//!   aterm-gui runs and draws.
//! * **A server with XInput 1 alone** ([`Edge::Xinput1`]): the proxy keeps
//!   XInputExtension present and answers its `XIQueryVersion` with the
//!   `Request` error an XInput 1 server answers; aterm-gui must refuse it
//!   as a server without XInput 2, by name, exit 1.
//!
//! On Wayland, under sway (headless wlroots, pixman, no input devices):
//!
//! * **Server-side decorations** ([`Edge::SwayDecorations`]): sway grants
//!   them through xdg-decoration, so the window's frame is the system's (no
//!   header buttons) and a drag on the band asks the compositor for nothing;
//!   with the decoration manager hidden (`lane-bus wlhide`) the same sway
//!   leaves the client to decorate itself, the header has its buttons and
//!   the same drag asks for `xdg_toplevel.move`.
//! * **cursor-shape-v1** ([`Edge::CursorShape`]): a virtual pointer
//!   (`lane-wl pointer`, wlroots' virtual-pointer protocol) over the text and
//!   over a floating client-decorated window's right edge: the window must
//!   name each shape through `wp_cursor_shape_device_v1.set_shape`; with the
//!   shape manager hidden it must set a cursor surface instead.
//! * **An output that comes and goes** ([`Edge::OutputHotplug`]): sway makes
//!   a second output at scale 2 and the window moves there (aterm-gui at
//!   scale 2), then the output is unplugged under it: the window must live
//!   on the first output, still drawing and still reading its pointer, at
//!   the scale the compositor last asked of its surface (`preferred_scale`
//!   in the wire log).
//! * **A seat's devices that come and go** ([`Edge::SeatHotplug`]): the seat
//!   gains a pointer, loses it and gains another (the window releases and
//!   makes its `wl_pointer`, reading each), and a second seat appears, then
//!   goes (sway's reload drops a seat its config does not name): the window
//!   releases the `wl_seat` it bound for it and keeps drawing.
//! * **A compositor without `wl_compositor` 6** ([`Edge::LegacyCompositor`]):
//!   `wlhide` announces `wl_compositor` at 5 and hides fractional scaling,
//!   so the scale comes only from the outputs the surface is on: an output
//!   at scale 2 must rescale the window at that output's `done`, and back;
//!   on a second output at 2, then that output unplugged, the window must
//!   forget it. Control: `wl_output` capped at 1 too (no `scale` event),
//!   where the same output scale leaves the window at 1.
//! * **The shadow margin** ([`Edge::ShadowMargin`]): sctk-adwaita's border
//!   parts around a floating client-decorated window (decoration manager
//!   hidden), restored after LW.5 took them away. A screenshot (`grim`, over
//!   a grey background) must show the visible border against the window's
//!   right edge and the shadow darkening the background beyond it. sway
//!   draws a subsurface outside the window's geometry but gives it no input
//!   there (a floating container takes the pointer only inside its own
//!   rectangle), so the handles' presses are [`Edge::ShadowHandles`]'s, under
//!   Mutter.
//! * **`window_padding = 0`** ([`Edge::Padding0`]): a floating
//!   client-decorated window; the virtual pointer pressed 1 px inside its
//!   left edge on a grid row, and on the last row, and dragged: a selection,
//!   and no `xdg_toplevel.resize`. Control: the default padding, where the
//!   same press resizes.
//!
//! Under Mutter:
//!
//! * **fullscreen** ([`Edge::Fullscreen`]) at `tab_strip_rows = 0`: the
//!   header's buttons and its row go while the window is fullscreen, and
//!   come back when it leaves.
//! * **The shadow margin's resize handles** ([`Edge::ShadowHandles`]): the
//!   seat's pointer (`lane-bus seat`, a RemoteDesktop session) pressed 6 px
//!   outside the window's right edge, on the right part's 12 px handle, must
//!   ask for `xdg_toplevel.resize` by the right edge and set the east-west
//!   resize cursor there; pressed 24 px outside it, past the handle, it must
//!   ask for nothing.
//! * **GNOME's `button-layout`** ([`Edge::ButtonLayout`]), set with
//!   `gsettings` and served by the lane's Settings portal (xdg-desktop-portal
//!   and its GTK backend, `linux_a11y::portal_up`): a layout that puts close
//!   and minimize at the left end and maximize at the right must lay the
//!   header out so; GNOME's own default (`appmenu:close`) must leave close
//!   alone at the right; and with the portal stopped (no answer) the header
//!   must fall back to minimize, maximize and close at the right, which is
//!   what LW.5's fixed buttons showed for all three.
//!
//! Under weston:
//!
//! * **Its pixman renderer** ([`Edge::WestonPixman`]), the VM and kiosk case,
//!   which LW.2 found crashing when a client grew a `wl_shm` pool it was
//!   still reading (sctk's frame pool, before aterm's own presenter and
//!   frame): aterm-gui on the CPU resized seven times, growing and shrinking,
//!   each frame held to weston's own output pixels (`weston-screenshooter`)
//!   and weston alive after each. Its control is the proof's CPU row under
//!   weston's GL renderer, which holds the same frames.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::linux::{self, Up};
use crate::linux_a11y::Cell;
use crate::linux_baseline::{Caption, dims_summary, frame_requests, parse_caption};
use crate::linux_guest::{self as guest, Display, Unit};
use crate::linux_proof::{self as proof, Gui, Row, Verdict};
use crate::stop::Stopped;

/// One wait for something that must happen: a hang detector, never a budget.
const HANG_SECS: u32 = 60;
const HANG: Duration = Duration::from_secs(60);

/// The X display `lane-bus xhide` serves, and the RandR-less Xvfb's.
const XHIDE_DISPLAY: &str = ":93";
const NO_RANDR_DISPLAY: &str = ":94";

/// The lane's Xvfb (`Display::Xvfb`'s client display).
const XVFB: &str = ":91";

/// Where sway keeps its sockets and config.
const SWAY_DIR: &str = "/tmp/aterm-lane-sway";

/// sway's one output at start, and the layout the virtual pointer moves in.
const SWAY_OUTPUT: &str = "output HEADLESS-1 resolution 1280x800\n";
const LAYOUT: (i32, i32) = (1280, 800);

/// The protocol a client-decorated window's frame comes from, which
/// `lane-bus wlhide` hides to make sway leave the client to decorate itself.
const DECORATION: &str = "zxdg_decoration_manager_v1";

/// One row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    XresInclude,
    NoRandr,
    NoXinput2,
    Xinput1,
    NoXkb,
    XProxy,
    SwayDecorations,
    CursorShape,
    OutputHotplug,
    SeatHotplug,
    LegacyCompositor,
    Padding0,
    ShadowMargin,
    ShadowHandles,
    Fullscreen,
    ButtonLayout,
    WestonPixman,
}

impl Edge {
    pub const ALL: [Edge; 17] = [
        Edge::XresInclude,
        Edge::NoRandr,
        Edge::NoXinput2,
        Edge::Xinput1,
        Edge::NoXkb,
        Edge::XProxy,
        Edge::SwayDecorations,
        Edge::CursorShape,
        Edge::OutputHotplug,
        Edge::SeatHotplug,
        Edge::LegacyCompositor,
        Edge::Padding0,
        Edge::ShadowMargin,
        Edge::ShadowHandles,
        Edge::Fullscreen,
        Edge::ButtonLayout,
        Edge::WestonPixman,
    ];

    /// The row's name on the command line and in its files.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Edge::XresInclude => "xresources-include",
            Edge::NoRandr => "no-randr",
            Edge::NoXinput2 => "no-xinput2",
            Edge::Xinput1 => "xinput1",
            Edge::NoXkb => "no-xkb",
            Edge::XProxy => "x-proxy",
            Edge::SwayDecorations => "sway-decorations",
            Edge::CursorShape => "cursor-shape",
            Edge::OutputHotplug => "output-hotplug",
            Edge::SeatHotplug => "seat-hotplug",
            Edge::LegacyCompositor => "legacy-compositor",
            Edge::Padding0 => "padding-0",
            Edge::ShadowMargin => "shadow-margin",
            Edge::ShadowHandles => "shadow-handles",
            Edge::Fullscreen => "fullscreen",
            Edge::ButtonLayout => "button-layout",
            Edge::WestonPixman => "weston-pixman",
        }
    }

    /// What the row's name says after its arch.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Edge::XresInclude => "a resource file that #includes Xft.dpi: xvfb (X11)",
            Edge::NoRandr => "refused without RANDR: xvfb -extension RANDR (X11)",
            Edge::NoXinput2 => "refused without XInput 2: xvfb through xhide (X11)",
            Edge::Xinput1 => "refused with XInput 1 alone: xvfb through xhide (X11)",
            Edge::NoXkb => "refused without XKB: xvfb through xhide (X11)",
            Edge::XProxy => "runs through xhide hiding nothing: xvfb (X11)",
            Edge::SwayDecorations => "server-side decorations: sway (Wayland)",
            Edge::CursorShape => "cursor-shape-v1: sway (Wayland)",
            Edge::OutputHotplug => "an output plugged and unplugged: sway (Wayland)",
            Edge::SeatHotplug => "a seat's pointer and a second seat: sway (Wayland)",
            Edge::LegacyCompositor => "wl_compositor 5, no fractional scale: sway (Wayland)",
            Edge::Padding0 => "window_padding = 0 under a pointer: sway (Wayland)",
            Edge::ShadowMargin => "the shadow margin outside the window: sway (Wayland)",
            Edge::ShadowHandles => "the shadow margin's resize handles: mutter (Wayland)",
            Edge::Fullscreen => "fullscreen at tab_strip_rows = 0: mutter (Wayland)",
            Edge::ButtonLayout => "GNOME's button-layout through the portal: mutter (Wayland)",
            Edge::WestonPixman => "the CPU presenter resized seven times: weston pixman (Wayland)",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Edge> {
        Edge::ALL.into_iter().find(|e| e.slug() == word)
    }

    /// Whether the row needs `lane-wl` (a sway row with a pointer).
    const fn points(self) -> bool {
        matches!(
            self,
            Edge::SwayDecorations
                | Edge::CursorShape
                | Edge::OutputHotplug
                | Edge::SeatHotplug
                | Edge::Padding0
                | Edge::ShadowMargin
        )
    }
}

/// How a row ended: its verdict and detail. A row whose display or helper
/// did not come up did not run.
type Ended = (Verdict, String);

fn not_run(why: impl Into<String>) -> Ended {
    (Verdict::NotRun, why.into())
}

/// The rows of `only` (every row when empty) on `cell`'s arch.
pub(crate) fn rows(
    cell: &Cell<'_>,
    built: bool,
    only: &[Edge],
    rows: &mut Vec<Row>,
) -> Result<(), Stopped> {
    let wanted: Vec<Edge> = Edge::ALL
        .into_iter()
        .filter(|e| only.is_empty() || only.contains(e))
        .collect();
    // `lane-wl pointer`, built once for the rows that point.
    let lane_wl = if built && wanted.iter().any(|e| e.points()) {
        println!("\n=== {}: build lane-wl ===", cell.tag);
        let out = cell.up.lima.shell_streamed(
            &cell.up.home,
            &cell.cargo(
                &cell.env,
                "build",
                &["-p", "aterm-wayland", "--example", "lane-wl"],
            ),
            "  | ",
        );
        if out.ok() {
            Ok(format!("{}/examples/lane-wl", cell.bin))
        } else {
            Err(format!("lane-wl did not build: {}", out.tail(5)))
        }
    } else {
        Err("lane-wl was not built".to_string())
    };
    for edge in wanted {
        let name = format!("{} {}", cell.tag, edge.title());
        if !built {
            rows.push(Row {
                name,
                verdict: Verdict::NotRun,
                secs: 0.0,
                detail: "no aterm-gui: its build failed".into(),
            });
            continue;
        }
        println!("\n=== {name} ===");
        let started = Instant::now();
        let gui = Gui {
            up: cell.up,
            bin: cell.bin.clone(),
            dir: format!(
                "/tmp/{}{}-edge-{}",
                guest::UNIT_PREFIX,
                cell.arch.name(),
                edge.slug()
            ),
            name: format!("{}-edge-{}", cell.arch.name(), edge.slug()),
            cpu: true,
            deck: false,
        };
        let row = Run {
            cell,
            gui,
            lane_wl: lane_wl.as_deref(),
        };
        let (verdict, detail) = row.run(edge);
        row.gui.stop();
        cell.up.stop_units();
        println!("  {} {detail}", verdict.label());
        rows.push(Row {
            name,
            verdict,
            secs: started.elapsed().as_secs_f64(),
            detail,
        });
        crate::stop::check()?;
    }
    Ok(())
}

/// The cursor-shape-v1 shapes (`wp_cursor_shape_device_v1.shape`), by value.
pub const SHAPES: [&str; 34] = [
    "default",
    "context_menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical_text",
    "alias",
    "copy",
    "move",
    "no_drop",
    "not_allowed",
    "grab",
    "grabbing",
    "e_resize",
    "n_resize",
    "ne_resize",
    "nw_resize",
    "s_resize",
    "se_resize",
    "sw_resize",
    "w_resize",
    "ew_resize",
    "ns_resize",
    "nesw_resize",
    "nwse_resize",
    "col_resize",
    "row_resize",
    "all_scroll",
    "zoom_in",
    "zoom_out",
];

/// The shapes a client's wire log (`WAYLAND_DEBUG=client`) set, by name, in
/// order, each once where it repeats in a row.
#[must_use]
pub fn shapes_set(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in log.lines() {
        let Some(at) = line.find(".set_shape(") else {
            continue;
        };
        if !line[..at].contains("-> wp_cursor_shape_device_v1@") {
            continue;
        }
        let args = &line[at + ".set_shape(".len()..];
        let shape = args
            .split([',', ')'])
            .nth(1)
            .and_then(|v| v.trim().parse::<usize>().ok());
        let name = shape.map_or_else(
            || "?".to_string(),
            |v| {
                v.checked_sub(1)
                    .and_then(|i| SHAPES.get(i))
                    .map_or_else(|| format!("shape{v}"), |n| (*n).to_string())
            },
        );
        if out.last() != Some(&name) {
            out.push(name);
        }
    }
    out
}

/// The cursors a client's wire log set through `wl_pointer.set_cursor`, each
/// with the surface the pointer was on when it set it (from the pointer's
/// `enter` and `leave`): `(wl_surface@N, (hotspot x, hotspot y))`, in order.
#[must_use]
pub fn cursors_set(log: &str) -> Vec<(String, (i32, i32))> {
    let mut on = String::new();
    let mut out = Vec::new();
    for line in log.lines() {
        let args = |at: usize, name: &str| -> Vec<String> {
            line[at + name.len()..]
                .trim_end()
                .trim_end_matches(')')
                .split(',')
                .map(|a| a.trim().to_string())
                .collect()
        };
        if line.contains("-> wl_pointer@") {
            if let Some(at) = line.find(".set_cursor(") {
                let a = args(at, ".set_cursor(");
                let hot = |i: usize| a.get(i).and_then(|v| v.parse().ok());
                if let (Some(x), Some(y)) = (hot(2), hot(3)) {
                    out.push((on.clone(), (x, y)));
                }
            }
        } else if line.contains(" wl_pointer@") {
            if let Some(at) = line.find(".enter(") {
                on = args(at, ".enter(").get(1).cloned().unwrap_or_default();
            } else if line.contains(".leave(") {
                on.clear();
            }
        }
    }
    out
}

/// The window's own surface: the one its `xdg_surface` was made for.
#[must_use]
pub fn window_surface(log: &str) -> Option<String> {
    log.lines()
        .find_map(|l| l.split_once(".get_xdg_surface(new id xdg_surface@"))
        .and_then(|(_, rest)| rest.split_once(", "))
        .map(|(_, s)| s.trim_end().trim_end_matches(')').to_string())
}

/// Each `wl_seat` a wire log shows the client binding, in order: the
/// global's name and the object it was bound as (`wl_seat@N`).
#[must_use]
pub fn seat_binds(log: &str) -> Vec<(u32, String)> {
    log.lines()
        .filter(|l| l.contains("-> wl_registry@"))
        .filter_map(|l| l.split_once(".bind(")?.1.split_once(", \"wl_seat\", "))
        .filter_map(|(name, rest)| {
            let object = rest.split_once("new id [unknown]@")?.1.trim_end();
            let object = object.strip_suffix(')')?;
            Some((name.trim().parse().ok()?, format!("wl_seat@{object}")))
        })
        .collect()
}

/// The last scale the compositor asked of `surface` (`wl_surface@N`) through
/// fractional-scale-v1, in 120ths: the last `preferred_scale` of the
/// `wp_fractional_scale_v1` the client made for that surface.
#[must_use]
pub fn preferred_scale(log: &str, surface: &str) -> Option<u32> {
    let made_for = format!(", {surface})");
    let object = log.lines().find_map(|l| {
        let (_, rest) = l.split_once(".get_fractional_scale(new id ")?;
        let (object, tail) = rest.split_once(',')?;
        (format!(",{tail}").trim_end() == made_for).then(|| object.trim().to_string())
    })?;
    let event = format!("{object}.preferred_scale(");
    log.lines()
        .filter(|l| !l.contains("-> "))
        .filter_map(|l| l.split_once(&event))
        .filter_map(|(_, rest)| rest.split_once(')'))
        .filter_map(|(scale, _)| scale.trim().parse().ok())
        .next_back()
}

/// How many requests of `method` on interface `iface` a wire log shows
/// (`-> iface@N.method(`).
#[must_use]
pub fn requests_of(log: &str, iface: &str, method: &str) -> usize {
    log.lines()
        .filter(|l| {
            l.split_once(&format!("-> {iface}@"))
                .is_some_and(|(_, rest)| {
                    rest.trim_start_matches(|c: char| c.is_ascii_digit())
                        .starts_with(&format!(".{method}("))
                })
        })
        .count()
}

/// How many events `event` of interface `iface` a wire log shows
/// (` iface@N.event(` with no arrow).
#[must_use]
pub fn events_of(log: &str, iface: &str, event: &str) -> usize {
    log.lines()
        .filter(|l| !l.contains("-> "))
        .filter(|l| {
            l.split_once(&format!("{iface}@")).is_some_and(|(_, rest)| {
                rest.trim_start_matches(|c: char| c.is_ascii_digit())
                    .starts_with(&format!(".{event}("))
            })
        })
        .count()
}

/// The version a wire log shows the client binding `interface` at.
#[must_use]
pub fn bound_version(log: &str, interface: &str) -> Option<u32> {
    let quoted = format!("\"{interface}\", ");
    log.lines()
        .filter(|l| l.contains("-> wl_registry@") && l.contains(".bind("))
        .find_map(|l| {
            let rest = &l[l.find(&quoted)? + quoted.len()..];
            rest.split(',').next()?.trim().parse().ok()
        })
}

/// A `dims` reply's scale.
fn scale_of(dims: &str) -> Option<f64> {
    proof::field(dims, "scale").and_then(|v| v.parse().ok())
}

fn num(dims: &str, key: &str) -> Option<i32> {
    proof::field(dims, key).and_then(|v| v.parse().ok())
}

/// The X display's socket, `/tmp/.X11-unix/X<n>`.
/// Whether `swaymsg -t get_tree`'s answer holds a window whose `app_id` is
/// `app_id` (however swaymsg spaces its JSON).
#[must_use]
pub fn sway_shows(tree: &str, app_id: &str) -> bool {
    let tree: String = tree.split_whitespace().collect();
    tree.contains(&format!("\"app_id\":\"{app_id}\""))
}

fn x_socket(display: &str) -> String {
    format!("/tmp/.X11-unix/X{}", display.trim_start_matches(':'))
}

/// sway, started as a lane unit.
struct Sway {
    /// Its Wayland socket.
    socket: String,
    /// Its IPC socket, for `swaymsg -s`.
    ipc: String,
}

/// One row's aterm-gui and what it runs with.
struct Run<'a> {
    cell: &'a Cell<'a>,
    gui: Gui<'a>,
    lane_wl: Result<&'a str, &'a String>,
}

impl Run<'_> {
    fn up(&self) -> &Up {
        self.cell.up
    }

    fn sh(&self, argv: &[&str]) -> linux::Out {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        self.up().sh(&argv)
    }

    fn stderr(&self) -> String {
        format!("{}/gui.stderr", self.gui.dir)
    }

    fn marks(&self) -> String {
        format!("{}/marks", self.gui.dir)
    }

    fn mark(&self, name: &str) -> String {
        format!("{}/{name}", self.marks())
    }

    /// Write `text` to the guest file `path`, its directory made first.
    fn put(&self, path: &str, text: &str) -> Result<(), String> {
        let dir = Path::new(path)
            .parent()
            .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
        let made = self.sh(&["install", "-d", &dir]);
        let wrote = self
            .up()
            .lima
            .shell(&self.up().home, &["tee".into(), path.into()], Some(text));
        if made.ok() && wrote.ok() {
            Ok(())
        } else {
            Err(format!("{path}: {}", wrote.tail(2)))
        }
    }

    /// The scratch world, with `top` (top-level config keys) put before the
    /// lane's config, and an empty marks directory.
    fn prepare(&self, top: &str) -> Result<(), String> {
        self.gui.prepare()?;
        if !top.is_empty() {
            let path = format!("{}/cfg/aterm/aterm.toml", self.gui.dir);
            let out = self.sh(&[
                "sh",
                "-c",
                "printf '%s' \"$1\" | cat - \"$2\" > \"$2.new\" && mv \"$2.new\" \"$2\"",
                "config",
                top,
                &path,
            ]);
            if !out.ok() {
                return Err(format!("{path}: {}", out.tail(2)));
            }
        }
        let out = self.sh(&["install", "-d", &self.marks()]);
        if out.ok() {
            Ok(())
        } else {
            Err(format!("{}: {}", self.marks(), out.tail(2)))
        }
    }

    /// One control verb: its reply, or why not.
    fn ctl(&self, verb: &[&str]) -> Result<String, String> {
        self.gui.expect(verb, &|r| !r.starts_with("ERR"))
    }

    /// Start aterm-gui on `client` with `extra` in its environment, its
    /// stderr (a `WAYLAND_DEBUG` wire log too) to [`Run::stderr`], and wait
    /// until it is ready.
    fn launch(
        &self,
        client: &[(String, String)],
        extra: &[(String, String)],
    ) -> Result<(), String> {
        self.gui.start_logged_with(client, extra, &self.stderr())?;
        if !self
            .up()
            .sh(&guest::wait_socket(&self.gui.sock(), HANG_SECS))
            .ok()
        {
            return Err(format!(
                "aterm-gui never listened ({HANG_SECS} s): {}",
                self.said(6)
            ));
        }
        let r = self.ctl(&["ready", "60000"])?;
        if r.starts_with("OK ready") {
            Ok(())
        } else {
            Err(format!("`ready` answered {r}"))
        }
    }

    /// The last `n` lines of aterm-gui's stderr, on one line.
    fn said(&self, n: usize) -> String {
        let out = self.sh(&["tail", "-n", &n.to_string(), &self.stderr()]);
        crate::util::one_line(&out.text, 400)
    }

    /// aterm-gui's whole stderr: its wire log under `WAYLAND_DEBUG`.
    fn wire(&self) -> String {
        self.sh(&["cat", &self.stderr()]).text
    }

    /// Keep aterm-gui's stderr beside the report, as `<name>-<what>.txt`.
    fn keep(&self, what: &str) -> String {
        let text = self.wire();
        let _ = std::fs::write(
            self.cell
                .results
                .join(format!("{}-{what}.txt", self.gui.name)),
            &text,
        );
        text
    }

    /// Photograph the frame (`image`) and keep it beside the report.
    fn photograph(&self, what: &str) -> Result<(), String> {
        // `image` takes a bare name and answers `OK image <w>x<h> <path>`.
        let name = format!("{}-{what}.png", self.gui.name);
        let reply = self.ctl(&["image", &name])?;
        let guest_png = reply
            .split_whitespace()
            .nth(3)
            .ok_or_else(|| format!("`image` answered {reply}"))?;
        let host = self.cell.results.join(&name);
        let out = self.up().lima.copy_out(guest_png, &host);
        if out.ok() {
            Ok(())
        } else {
            Err(format!("{guest_png}: {}", out.tail(2)))
        }
    }

    fn dims(&self) -> Result<String, String> {
        self.ctl(&["dims"])
    }

    /// Poll `dims` until `done` holds, within the hang detector.
    fn dims_until(&self, what: &str, done: &dyn Fn(&str) -> bool) -> Result<String, String> {
        let t0 = Instant::now();
        loop {
            let d = self.dims()?;
            if done(&d) {
                return Ok(d);
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "{what} never came ({HANG_SECS} s): {}",
                    dims_summary(&d)
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn scale_until(&self, step: &str, want: f64) -> Result<String, String> {
        self.dims_until(&format!("scale {want:.2} {step}"), &|d| {
            scale_of(d).is_some_and(|s| (s - want).abs() < 0.01)
        })
    }

    fn caption(&self) -> Result<Caption, String> {
        let chrome = self.ctl(&["chrome"])?;
        parse_caption(&chrome).ok_or_else(|| {
            format!(
                "`chrome` has no strip-caption line: {}",
                crate::util::one_line(&chrome, 200)
            )
        })
    }

    fn caption_until(
        &self,
        what: &str,
        done: &dyn Fn(&Caption) -> bool,
    ) -> Result<Caption, String> {
        let t0 = Instant::now();
        loop {
            let c = self.caption()?;
            if done(&c) {
                return Ok(c);
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "{what} never came ({HANG_SECS} s): buttons={}",
                    c.names()
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Start the units in order (each listening on its socket).
    fn units(&self, what: &str, units: Vec<Unit>) -> Result<(), String> {
        linux::start_units(self.up(), what, units, None)
    }

    /// A unit's journal, on one line.
    fn journal(&self, unit: &str) -> String {
        let out = self.sh(&[
            "journalctl",
            "--user",
            "--no-pager",
            "-o",
            "cat",
            "-n",
            "20",
            "-u",
            unit,
        ]);
        out.text
    }

    /// Stop the `wlhide` proxy, so the next one takes its socket.
    fn stop_wlhide(&self) {
        let _ = self.sh(&[
            "systemctl",
            "--user",
            "stop",
            &format!("{}wlhide", guest::UNIT_PREFIX),
        ]);
    }

    fn lane_bus(&self) -> String {
        format!("{}/lane-bus", self.cell.bin)
    }

    fn run(&self, edge: Edge) -> Ended {
        if edge.points()
            && let Err(why) = &self.lane_wl
        {
            return not_run((*why).clone());
        }
        match edge {
            Edge::XresInclude => self.xres_include(),
            Edge::NoRandr => self.no_randr(),
            Edge::NoXinput2 => self.hidden_x("XInputExtension"),
            Edge::Xinput1 => self.xinput1(),
            Edge::NoXkb => self.hidden_x("XKEYBOARD"),
            Edge::XProxy => self.x_proxy(),
            Edge::SwayDecorations => self.decorations(),
            Edge::CursorShape => self.cursor_shape(),
            Edge::OutputHotplug => self.output_hotplug(),
            Edge::SeatHotplug => self.seat_hotplug(),
            Edge::LegacyCompositor => self.legacy(),
            Edge::Padding0 => self.padding0(),
            Edge::ShadowMargin => self.shadow_margin(),
            Edge::ShadowHandles => self.shadow_handles(),
            Edge::Fullscreen => self.fullscreen(),
            Edge::ButtonLayout => self.button_layout(),
            Edge::WestonPixman => self.weston_pixman(),
        }
    }

    fn verdict(r: Result<String, String>) -> Ended {
        match r {
            Ok(d) => (Verdict::Pass, d),
            Err(e) => (Verdict::Fail, e),
        }
    }

    // ---------------------------------------------------------------- X11

    fn xres_include(&self) -> Ended {
        if let Err(why) = linux::start_display(self.up(), Display::Xvfb) {
            return not_run(why);
        }
        let removed = self.sh(&["env", &format!("DISPLAY={XVFB}"), "xrdb", "-remove"]);
        let left = self.sh(&[
            "env",
            &format!("DISPLAY={XVFB}"),
            "xprop",
            "-root",
            "RESOURCE_MANAGER",
        ]);
        if !removed.ok() || !left.text.contains("not found") {
            return not_run(format!(
                "the root window keeps a RESOURCE_MANAGER after `xrdb -remove`: {}",
                crate::util::one_line(&left.text, 200)
            ));
        }
        Self::verdict((|| {
            self.prepare("")?;
            let home = format!("{}/home", self.gui.dir);
            self.put(
                &format!("{home}/.Xresources"),
                "! the lane's include row\n#include \"xres/main\"\n",
            )?;
            self.put(&format!("{home}/xres/main"), "#include \"dpi\"\n")?;
            self.put(&format!("{home}/xres/dpi"), "Xft.dpi: 144\n")?;
            // What a loader resolving the nested include from HOME reads.
            self.put(&format!("{home}/dpi"), "Xft.dpi: 192\n")?;
            let client = [Display::Xvfb.client_env(&self.up().runtime)];
            self.launch(&client, &[])?;
            let with = self.dims()?;
            self.gui.stop();
            // The control: the same files, the include line taken out.
            self.put(&format!("{home}/.Xresources"), "! the lane's include row\n")?;
            self.launch(&client, &[])?;
            let without = self.dims()?;
            let (a, b) = (scale_of(&with), scale_of(&without));
            let line = format!(
                "scale {} through ~/.Xresources -> xres/main -> xres/dpi (Xft.dpi 144; the decoy \
                 ~/dpi says 192), {} without the #include line",
                a.map_or("?".into(), |s| format!("{s:.2}")),
                b.map_or("?".into(), |s| format!("{s:.2}"))
            );
            let close = |s: Option<f64>, want: f64| s.is_some_and(|s| (s - want).abs() < 0.01);
            if close(a, 1.5) && close(b, 1.0) {
                Ok(line)
            } else {
                Err(line)
            }
        })())
    }

    /// aterm-gui on `client`, which must refuse the display naming
    /// `extension` and exit 1, without a panic: the exit and its line.
    fn refused(&self, client: &[(String, String)], extension: &str) -> Result<String, String> {
        self.prepare("")?;
        let status = format!("{}/status", self.gui.dir);
        self.gui.start_watched_on(client, &self.stderr(), &status)?;
        if !self.up().sh(&guest::wait_file(&status, HANG_SECS)).ok() {
            return Err(format!(
                "aterm-gui did not end within {HANG_SECS} s: it neither refused the display nor \
                 failed ({})",
                self.said(4)
            ));
        }
        let code = self.sh(&["cat", &status]).text.trim().to_string();
        let said = self.wire();
        if let Some(panic) = said.lines().find(|l| l.contains("panicked")) {
            return Err(format!("exit {code}, a panic: {}", panic.trim()));
        }
        let line = said
            .lines()
            .find(|l| l.contains("cannot open a display"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let named = line.contains(&format!("has no {extension} extension"));
        if code == "1" && named {
            Ok(format!("exit 1: {line}"))
        } else {
            Err(format!(
                "exit {code}; wanted 1 and a line naming {extension}: {}",
                crate::util::one_line(&said, 400)
            ))
        }
    }

    fn no_randr(&self) -> Ended {
        let unit = Unit {
            name: format!("{}xvfb-norandr", guest::UNIT_PREFIX),
            env: Vec::new(),
            argv: [
                "Xvfb",
                NO_RANDR_DISPLAY,
                "-screen",
                "0",
                "1280x800x24",
                "-nolisten",
                "tcp",
                "-noreset",
                "-extension",
                "RANDR",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            socket: x_socket(NO_RANDR_DISPLAY),
        };
        if let Err(why) = self.units("xvfb -extension RANDR", vec![unit]) {
            return not_run(why);
        }
        let client = [("DISPLAY".to_string(), NO_RANDR_DISPLAY.to_string())];
        Self::verdict(self.refused(&client, "RANDR"))
    }

    /// `lane-bus xhide` on [`XHIDE_DISPLAY`] in front of the lane's Xvfb,
    /// hiding `hide` (`-`: nothing).
    fn xhide(&self, hide: &str) -> Result<(), String> {
        linux::start_display(self.up(), Display::Xvfb)?;
        self.units(
            "lane-bus xhide",
            vec![Unit {
                name: format!("{}xhide", guest::UNIT_PREFIX),
                env: Vec::new(),
                argv: vec![
                    self.lane_bus(),
                    "xhide".into(),
                    XHIDE_DISPLAY.into(),
                    XVFB.into(),
                    hide.into(),
                ],
                socket: x_socket(XHIDE_DISPLAY),
            }],
        )
    }

    fn hidden_x(&self, extension: &str) -> Ended {
        if let Err(why) = self.xhide(extension) {
            return not_run(why);
        }
        let client = [("DISPLAY".to_string(), XHIDE_DISPLAY.to_string())];
        let r = self.refused(&client, extension).and_then(|line| {
            let proxy = self.journal(&format!("{}xhide", guest::UNIT_PREFIX));
            let hid = format!("answered QueryExtension {extension}: absent");
            if proxy.contains(&hid) {
                Ok(format!("{line}; the proxy {hid}"))
            } else {
                Err(format!(
                    "{line}, but the proxy never said it {hid}: {}",
                    crate::util::one_line(&proxy, 300)
                ))
            }
        });
        Self::verdict(r)
    }

    /// XInputExtension present, its `XIQueryVersion` refused as an XInput 1
    /// server refuses it: aterm-gui must say it lacks XInput 2.
    fn xinput1(&self) -> Ended {
        if let Err(why) = self.xhide("XInput2") {
            return not_run(why);
        }
        let client = [("DISPLAY".to_string(), XHIDE_DISPLAY.to_string())];
        let r = self.refused(&client, "XInputExtension").and_then(|line| {
            let proxy = self.journal(&format!("{}xhide", guest::UNIT_PREFIX));
            let refused = "answered XIQueryVersion: Request error";
            if proxy.contains(refused) {
                Ok(format!("{line}; the proxy {refused}"))
            } else {
                Err(format!(
                    "{line}, but the proxy never {refused}: {}",
                    crate::util::one_line(&proxy, 300)
                ))
            }
        });
        Self::verdict(r)
    }

    fn x_proxy(&self) -> Ended {
        if let Err(why) = self.xhide("-") {
            return not_run(why);
        }
        Self::verdict((|| {
            self.prepare("")?;
            let client = [("DISPLAY".to_string(), XHIDE_DISPLAY.to_string())];
            self.launch(&client, &[])?;
            let status = self.ctl(&["status"])?;
            let render = proof::field(&status, "render").unwrap_or("?").to_string();
            let d = self.dims()?;
            if render == "ok" {
                Ok(format!(
                    "ready through the proxy, render=ok, {}",
                    dims_summary(&d)
                ))
            } else {
                Err(format!("render={render} through the proxy"))
            }
        })())
    }

    // ---------------------------------------------------------------- sway

    fn sway(&self) -> Result<Sway, String> {
        let rt = format!("{SWAY_DIR}/rt");
        let fresh = self.sh(&[
            "sh",
            "-c",
            "rm -rf \"$1\" && install -d -m 700 \"$1\" \"$1/rt\"",
            "sway",
            SWAY_DIR,
        ]);
        if !fresh.ok() {
            return Err(format!("{SWAY_DIR}: {}", fresh.tail(2)));
        }
        let cfg = format!("{SWAY_DIR}/sway.cfg");
        self.put(&cfg, SWAY_OUTPUT)?;
        let socket = format!("{rt}/wayland-1");
        self.units(
            "sway",
            vec![Unit {
                name: format!("{}sway", guest::UNIT_PREFIX),
                env: vec![
                    ("XDG_RUNTIME_DIR".into(), rt.clone()),
                    ("WLR_BACKENDS".into(), "headless".into()),
                    ("WLR_LIBINPUT_NO_DEVICES".into(), "1".into()),
                    ("WLR_RENDERER".into(), "pixman".into()),
                ],
                argv: vec!["sway".into(), "-c".into(), cfg],
                socket: socket.clone(),
            }],
        )?;
        let ipc = self.sh(&[
            "timeout",
            &HANG_SECS.to_string(),
            "sh",
            "-c",
            "until ls \"$1\"/sway-ipc.*.sock >/dev/null 2>&1; do sleep 0.1; done; ls \"$1\"/sway-ipc.*.sock",
            "ipc",
            &rt,
        ]);
        let ipc = ipc
            .text
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if ipc.is_empty() {
            return Err(format!("sway made no IPC socket ({HANG_SECS} s)"));
        }
        Ok(Sway { socket, ipc })
    }

    /// `swaymsg` to `sway`: its answer, or why not.
    fn swaymsg(&self, sway: &Sway, args: &[&str]) -> Result<String, String> {
        let mut argv = vec!["swaymsg", "-s", &sway.ipc];
        argv.extend_from_slice(args);
        let out = self.sh(&argv);
        if out.ok() && !out.text.contains("\"success\": false") {
            Ok(out.text)
        } else {
            Err(format!(
                "swaymsg {}: {}",
                args.join(" "),
                crate::util::one_line(&out.text, 300)
            ))
        }
    }

    /// `lane-bus wlhide` in front of `sway` with `rules`: the socket a
    /// client reaches the edited compositor by.
    fn wlhide(&self, sway: &Sway, rules: &[&str]) -> Result<String, String> {
        let socket = format!("{SWAY_DIR}/rt/hidden");
        let mut argv = vec![
            self.lane_bus(),
            "wlhide".into(),
            socket.clone(),
            sway.socket.clone(),
        ];
        argv.extend(rules.iter().map(|r| (*r).to_string()));
        self.units(
            "lane-bus wlhide",
            vec![Unit {
                name: format!("{}wlhide", guest::UNIT_PREFIX),
                env: Vec::new(),
                argv,
                socket: socket.clone(),
            }],
        )?;
        Ok(socket)
    }

    /// aterm-gui on the Wayland socket `socket`, its wire log on.
    fn launch_wayland(&self, sway: &Sway, socket: &str) -> Result<(), String> {
        self.launch(
            &[("WAYLAND_DISPLAY".into(), socket.into())],
            &[("WAYLAND_DEBUG".into(), "client".into())],
        )?;
        self.mapped(sway)
    }

    /// Wait (a hang detector) until `sway` shows aterm-gui's window in its
    /// tree. `ready` can answer before the window's first buffer has reached
    /// sway: under Rosetta a `floating enable` right after it found "an empty
    /// workspace" (`proof-20261020-212543`, x86_64, three rows).
    fn mapped(&self, sway: &Sway) -> Result<(), String> {
        let t0 = Instant::now();
        loop {
            let tree = self.swaymsg(sway, &["-t", "get_tree"])?;
            if sway_shows(&tree, "aterm") {
                return Ok(());
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "sway never showed aterm-gui's window ({HANG_SECS} s): {}",
                    self.said(3)
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// `lane-wl pointer` on `socket` with `steps`, as a lane unit that ends
    /// with its last step.
    fn pointer(&self, socket: &str, steps: &[String]) -> Result<(), String> {
        let lane_wl = self.lane_wl.map_err(Clone::clone)?;
        let mut argv = vec![
            lane_wl.to_string(),
            "pointer".into(),
            format!("{}x{}", LAYOUT.0, LAYOUT.1),
        ];
        argv.extend(steps.iter().cloned());
        let unit = Unit {
            name: format!("{}pointer", guest::UNIT_PREFIX),
            env: vec![("WAYLAND_DISPLAY".into(), socket.into())],
            argv,
            socket: String::new(),
        };
        let out = self.up().sh(&guest::run_unit(&unit, None));
        if out.ok() {
            Ok(())
        } else {
            Err(format!("lane-wl pointer: {}", out.tail(3)))
        }
    }

    /// Wait for the mark `name` the pointer touches.
    fn reached(&self, name: &str) -> Result<(), String> {
        if self
            .up()
            .sh(&guest::wait_file(&self.mark(name), HANG_SECS))
            .ok()
        {
            Ok(())
        } else {
            Err(format!(
                "the pointer never reached {name} ({HANG_SECS} s): {}",
                crate::util::one_line(
                    &self.journal(&format!("{}pointer", guest::UNIT_PREFIX)),
                    300
                )
            ))
        }
    }

    fn go(&self, name: &str) {
        let _ = self.sh(&["touch", &self.mark(name)]);
    }

    /// The window pixel the window reads its pointer at, once it reads one.
    fn pointer_px(&self) -> Result<(i32, i32), String> {
        let t0 = Instant::now();
        loop {
            let at = self.ctl(&["pointer", "status"]);
            let px = at.as_ref().ok().and_then(|a| {
                proof::field(a, "px")
                    .and_then(|p| p.split_once(','))
                    .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
            });
            if let Some(px) = px {
                return Ok(px);
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "the window never read its pointer ({HANG_SECS} s): {}",
                    at.unwrap_or_else(|e| e)
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The window pixel of grid cell `row`, `col` (the control socket's own
    /// pointer, which reads back where that cell is).
    fn cell_px(&self, row: i32, col: i32) -> Result<(i32, i32), String> {
        let at = self.ctl(&["pointer", "move", &row.to_string(), &col.to_string()])?;
        proof::field(&at, "px")
            .and_then(|p| p.split_once(','))
            .and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))
            .ok_or_else(|| format!("`pointer move {row} {col}` read no px: {at}"))
    }

    /// Put the layout point for window pixel `px` (the window's top-left at
    /// `origin` in the layout, scale 1) in the mark `name`, for `at:@`.
    fn aim(&self, name: &str, origin: (i32, i32), px: (i32, i32)) -> Result<(), String> {
        let (x, y) = (origin.0 + px.0, origin.1 + px.1);
        if x < 0 || y < 0 || x >= LAYOUT.0 || y >= LAYOUT.1 {
            return Err(format!("{px:?} of the window is off the layout at {x},{y}"));
        }
        self.put(&self.mark(name), &format!("{x} {y}\n"))
    }

    /// The window's top-left in the layout: the pointer was put at the
    /// layout's centre and the window read it at `px`.
    fn origin(px: (i32, i32)) -> (i32, i32) {
        (LAYOUT.0 / 2 - px.0, LAYOUT.1 / 2 - px.1)
    }

    fn decorations(&self) -> Ended {
        let sway = match self.sway() {
            Ok(s) => s,
            Err(why) => return not_run(why),
        };
        Self::verdict((|| {
            // Server-side: sway's own socket.
            let ssd = self.band_drag(&sway, &sway.socket, "ssd")?;
            if ssd.0.client || !ssd.0.buttons.is_empty() {
                return Err(format!(
                    "sway granted server-side decorations, yet the frame is the client's: \
                     buttons={}",
                    ssd.0.names()
                ));
            }
            if ssd.1 != "none" {
                return Err(format!(
                    "under server-side decorations a drag on the band asked for {}",
                    ssd.1
                ));
            }
            self.gui.stop();
            // Client-side: the same sway with its decoration manager hidden.
            let hidden = self.wlhide(&sway, &[&format!("hide:{DECORATION}")])?;
            let csd = self.band_drag(&sway, &hidden, "csd")?;
            if !csd.0.client || csd.0.buttons.is_empty() {
                return Err(format!(
                    "with the decoration manager hidden the frame is not the client's: buttons={}",
                    csd.0.names()
                ));
            }
            if !csd.1.split(' ').any(|r| r == "move") {
                return Err(format!(
                    "with the window decorating itself a drag on the band asked for {}, not move",
                    csd.1
                ));
            }
            Ok(format!(
                "server-side: frame=system buttons=none, a drag on the band asked for {}; \
                 decoration manager hidden: frame=client buttons={}, the same drag asked for {}",
                ssd.1,
                csd.0.names(),
                csd.1
            ))
        })())
    }

    /// aterm-gui on `socket`, photographed, then dragged by the band's
    /// middle with the virtual pointer: its caption and the frame requests
    /// its wire log shows.
    fn band_drag(
        &self,
        sway: &Sway,
        socket: &str,
        what: &str,
    ) -> Result<(Caption, String), String> {
        self.prepare("")?;
        self.launch_wayland(sway, socket)?;
        let caption = self.caption()?;
        self.photograph(what)?;
        let d = self.dims()?;
        let band_y = num(&d, "pad_top").unwrap_or(0)
            + num(&d, "head").unwrap_or(0)
            + num(&d, "cell_h").unwrap_or(0) / 2;
        // A bare stretch of the band, right of a lone tab and its `+`: four
        // cells left of the first button, or of the strip's end where there
        // is none.
        let (cw, pad) = (num(&d, "cell_w").unwrap_or(0), num(&d, "pad").unwrap_or(0));
        let cols = u16::try_from(num(&d, "window_cols").unwrap_or(0)).unwrap_or(0);
        let end = i32::from(caption.right_end(cols));
        let band_x = pad + (end - 4).max(0) * cw;
        let steps = [
            "make".to_string(),
            format!("at:{},{}", LAYOUT.0 / 2, LAYOUT.1 / 2),
            format!("touch:{}", self.mark("centred")),
            format!("wait:{}", self.mark("aimed")),
            format!("at:@{}", self.mark("band")),
            "sleep:200".into(),
            "press:left".into(),
            format!("at:@{}", self.mark("band-6")),
            "sleep:300".into(),
            format!("at:@{}", self.mark("band-40")),
            "sleep:300".into(),
            "release:left".into(),
            "sleep:300".into(),
            format!("touch:{}", self.mark("dragged")),
            format!("wait:{}", self.mark("done")),
        ];
        self.pointer(socket, &steps)?;
        self.reached("centred")?;
        let origin = Self::origin(self.pointer_px()?);
        self.aim("band", origin, (band_x, band_y))?;
        self.aim("band-6", origin, (band_x + 6, band_y))?;
        self.aim("band-40", origin, (band_x + 40, band_y + 30))?;
        self.go("aimed");
        self.reached("dragged")?;
        self.go("done");
        let wire = self.keep(&format!("{what}-wire"));
        Ok((caption, frame_requests(&wire)))
    }

    fn cursor_shape(&self) -> Ended {
        let sway = match self.sway() {
            Ok(s) => s,
            Err(why) => return not_run(why),
        };
        Self::verdict((|| {
            // Over the text, under server-side decorations.
            let text = self.hover(&sway, &sway.socket, "shapes", false)?;
            if !text.0.iter().any(|s| s == "text") {
                return Err(format!(
                    "over the text the window set {}, never text",
                    text.0.join(",")
                ));
            }
            self.gui.stop();
            // Over a floating client-decorated window's right edge.
            let hidden = self.wlhide(&sway, &[&format!("hide:{DECORATION}")])?;
            let edge = self.hover(&sway, &hidden, "shapes-edge", true)?;
            if !edge.0.iter().any(|s| s.ends_with("_resize")) {
                return Err(format!(
                    "over the right edge the window set {}, no resize shape",
                    edge.0.join(",")
                ));
            }
            self.gui.stop();
            self.stop_wlhide();
            // The control: the shape manager hidden.
            let hidden = self.wlhide(&sway, &["hide:wp_cursor_shape_manager_v1"])?;
            let control = self.hover(&sway, &hidden, "shapes-hidden", false)?;
            if !control.0.is_empty() || control.1 == 0 {
                return Err(format!(
                    "with the shape manager hidden the window set shapes {} and {} cursor \
                     surfaces",
                    control.0.join(","),
                    control.1
                ));
            }
            Ok(format!(
                "over the text: {}; over a floating client-decorated window's right edge: {}; \
                 with the shape manager hidden, no shape and {} wl_pointer.set_cursor",
                text.0.join(","),
                edge.0.join(","),
                control.1
            ))
        })())
    }

    /// aterm-gui on `socket` (floating when `edge`), the virtual pointer
    /// put on its text, or on its right edge: the shapes it set, and how
    /// many cursor surfaces it set.
    fn hover(
        &self,
        sway: &Sway,
        socket: &str,
        what: &str,
        edge: bool,
    ) -> Result<(Vec<String>, usize), String> {
        self.prepare("")?;
        self.launch_wayland(sway, socket)?;
        if edge {
            self.swaymsg(sway, &["floating", "enable"])?;
            self.caption_until("resize edges", &|c| c.edges)?;
        }
        let steps = [
            "make".to_string(),
            format!("at:{},{}", LAYOUT.0 / 2, LAYOUT.1 / 2),
            format!("touch:{}", self.mark("centred")),
            format!("wait:{}", self.mark("aimed")),
            format!("at:@{}", self.mark("target")),
            "sleep:300".into(),
            format!("touch:{}", self.mark("there")),
            format!("wait:{}", self.mark("done")),
        ];
        self.pointer(socket, &steps)?;
        self.reached("centred")?;
        let px = self.pointer_px()?;
        let origin = Self::origin(px);
        let target = if edge {
            let d = self.dims()?;
            (
                num(&d, "frame_w").unwrap_or(0) - 2,
                num(&d, "frame_h").unwrap_or(0) / 2,
            )
        } else {
            self.cell_px(2, 2)?
        };
        self.aim("target", origin, target)?;
        self.go("aimed");
        self.reached("there")?;
        self.go("done");
        let wire = self.keep(&format!("{what}-wire"));
        Ok((
            shapes_set(&wire),
            requests_of(&wire, "wl_pointer", "set_cursor"),
        ))
    }

    fn output_hotplug(&self) -> Ended {
        let sway = match self.sway() {
            Ok(s) => s,
            Err(why) => return not_run(why),
        };
        Self::verdict((|| {
            self.prepare("")?;
            self.launch_wayland(&sway, &sway.socket)?;
            let before = self.dims()?;
            self.swaymsg(&sway, &["create_output"])?;
            self.swaymsg(&sway, &["output", "HEADLESS-2", "scale", "2"])?;
            self.swaymsg(&sway, &["move", "container", "to", "output", "HEADLESS-2"])?;
            let there = self.scale_until("on HEADLESS-2", 2.0)?;
            self.swaymsg(&sway, &["output", "HEADLESS-2", "unplug"])?;
            // sway carries the gone output's workspace to the first output but
            // keeps showing that output's own; focusing the window shows its.
            self.swaymsg(&sway, &["[app_id=aterm]", "focus"])?;
            // The window lives on the first output: it reads a pointer there
            // (the layout's centre, HEADLESS-1's), and keeps drawing.
            let steps = [
                "make".to_string(),
                format!("at:{},{}", LAYOUT.0 / 2, LAYOUT.1 / 2),
                format!("touch:{}", self.mark("centred")),
                format!("wait:{}", self.mark("done")),
            ];
            self.pointer(&sway.socket, &steps)?;
            self.reached("centred")?;
            let px = self.pointer_px()?;
            self.go("done");
            let status = self.ctl(&["status"])?;
            let render = proof::field(&status, "render").unwrap_or("?");
            if render != "ok" {
                return Err(format!("render={render} after the unplug"));
            }
            // The scale after is the compositor's: the last preferred_scale it
            // sent the window's surface (sway sends none at the unplug, so it
            // stays HEADLESS-2's). The wire is read again until the two agree,
            // in case one more came between the reads.
            let t0 = Instant::now();
            let (after, wire, preferred) = loop {
                let after = self.dims()?;
                let wire = self.wire();
                let preferred =
                    window_surface(&wire).and_then(|surface| preferred_scale(&wire, &surface));
                let agree = preferred
                    .zip(scale_of(&after))
                    .is_some_and(|(p, s)| (s - f64::from(p) / 120.0).abs() < 0.01);
                if agree || t0.elapsed() > HANG {
                    break (after, wire, preferred);
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let _ = self.keep("wire");
            let removed = events_of(&wire, "wl_registry", "global_remove");
            if removed == 0 {
                return Err("the window was never told the output went".into());
            }
            let Some(preferred) = preferred else {
                return Err("the compositor sent the window's surface no preferred_scale".into());
            };
            let wanted = f64::from(preferred) / 120.0;
            if scale_of(&after).is_none_or(|s| (s - wanted).abs() >= 0.01) {
                return Err(format!(
                    "scale {} after the unplug, where the compositor's last preferred_scale \
                     was {preferred} ({wanted:.2}; {HANG_SECS} s): {}",
                    scale_of(&after).map_or("?".into(), |s| format!("{s:.2}")),
                    dims_summary(&after)
                ));
            }
            Ok(format!(
                "scale {} on HEADLESS-1, {} on HEADLESS-2; unplugged ({removed} global_remove): \
                 the pointer read at px={},{}, render=ok, scale {} after (the compositor's \
                 last preferred_scale, {preferred}), {}",
                scale_of(&before).map_or("?".into(), |s| format!("{s:.2}")),
                scale_of(&there).map_or("?".into(), |s| format!("{s:.2}")),
                px.0,
                px.1,
                scale_of(&after).map_or("?".into(), |s| format!("{s:.2}")),
                dims_summary(&after)
            ))
        })())
    }

    fn seat_hotplug(&self) -> Ended {
        let sway = match self.sway() {
            Ok(s) => s,
            Err(why) => return not_run(why),
        };
        Self::verdict((|| {
            self.prepare("")?;
            self.launch_wayland(&sway, &sway.socket)?;
            let steps = [
                "make".to_string(),
                format!("at:{},{}", LAYOUT.0 / 2, LAYOUT.1 / 2),
                format!("touch:{}", self.mark("first")),
                format!("wait:{}", self.mark("first-read")),
                "drop".into(),
                format!("touch:{}", self.mark("dropped")),
                format!("wait:{}", self.mark("again")),
                "make".into(),
                format!("at:{},{}", LAYOUT.0 / 2 - 40, LAYOUT.1 / 2 - 20),
                format!("touch:{}", self.mark("second")),
                format!("wait:{}", self.mark("done")),
            ];
            self.pointer(&sway.socket, &steps)?;
            self.reached("first")?;
            let first = self.pointer_px()?;
            self.go("first-read");
            self.reached("dropped")?;
            // The window let its pointer go before the next one comes.
            let t0 = Instant::now();
            while requests_of(&self.wire(), "wl_pointer", "release") == 0 {
                if t0.elapsed() > HANG {
                    return Err(format!(
                        "the seat lost its pointer and the window never released its own \
                         ({HANG_SECS} s)"
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            self.go("again");
            self.reached("second")?;
            // The new pointer, 40,20 left of where the first one was read.
            let t0 = Instant::now();
            let second = loop {
                let px = self.pointer_px()?;
                if px != first || t0.elapsed() > HANG {
                    break px;
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            self.go("done");
            // A second seat.
            self.swaymsg(&sway, &["seat", "seat1", "fallback", "false"])?;
            let t0 = Instant::now();
            let wire = loop {
                let wire = self.wire();
                let bound = wire
                    .lines()
                    .filter(|l| l.contains("-> wl_registry@") && l.contains("\"wl_seat\""))
                    .count();
                if bound >= 2 || t0.elapsed() > HANG {
                    break wire;
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let _ = self.keep("wire");
            let status = self.ctl(&["status"])?;
            let (got, released) = (
                requests_of(&wire, "wl_seat", "get_pointer"),
                requests_of(&wire, "wl_pointer", "release"),
            );
            let seats = wire
                .lines()
                .filter(|l| l.contains("-> wl_registry@") && l.contains("\"wl_seat\""))
                .count();
            let line = format!(
                "pointer read at px={},{}, released ({released} release), a new one read at \
                 px={},{} ({got} get_pointer); {seats} seats bound; render={}",
                first.0,
                first.1,
                second.0,
                second.1,
                proof::field(&status, "render").unwrap_or("?")
            );
            if !(got >= 2 && released >= 1 && second != first && seats >= 2) {
                return Err(line);
            }
            // The second seat goes: sway's reload drops a seat its config does
            // not name. The window must release the seat it bound for it
            // (`wl_seat.release`, version 5 and later) and keep drawing.
            let Some((global, object)) = seat_binds(&wire).pop() else {
                return Err(format!("{line}; no wl_seat bind in the wire log"));
            };
            self.swaymsg(&sway, &["reload"])?;
            let removed = format!(".global_remove({global})");
            let release = format!("-> {object}.release()");
            let t0 = Instant::now();
            let wire = loop {
                let wire = self.wire();
                if (wire.contains(&removed) && wire.contains(&release)) || t0.elapsed() > HANG {
                    break wire;
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let _ = self.keep("wire");
            if !wire.contains(&removed) {
                return Err(format!(
                    "{line}; sway's reload never removed seat global {global} ({HANG_SECS} s)"
                ));
            }
            if !wire.contains(&release) {
                return Err(format!(
                    "{line}; seat global {global} went and the window never released {object} \
                     ({HANG_SECS} s)"
                ));
            }
            let status = self.ctl(&["status"])?;
            let render = proof::field(&status, "render").unwrap_or("?");
            if render != "ok" {
                return Err(format!("{line}; render={render} after the seat went"));
            }
            Ok(format!(
                "{line}; the second seat removed (global_remove {global}) and {object} \
                 released, render=ok"
            ))
        })())
    }

    fn legacy(&self) -> Ended {
        let sway = match self.sway() {
            Ok(s) => s,
            Err(why) => return not_run(why),
        };
        Self::verdict((|| {
            let rules = ["cap:wl_compositor=5", "hide:wp_fractional_scale_manager_v1"];
            let hidden = self.wlhide(&sway, &rules)?;
            self.prepare("")?;
            self.launch_wayland(&sway, &hidden)?;
            let v = bound_version(&self.wire(), "wl_compositor");
            if v != Some(5) {
                return Err(format!("the window bound wl_compositor at {v:?}, not 5"));
            }
            self.swaymsg(&sway, &["output", "HEADLESS-1", "scale", "2"])?;
            let two = self.scale_until("at output scale 2", 2.0)?;
            self.swaymsg(&sway, &["output", "HEADLESS-1", "scale", "1"])?;
            let one = self.scale_until("at output scale 1", 1.0)?;
            // A second output at 2, the window moved there, the output gone.
            self.swaymsg(&sway, &["create_output"])?;
            self.swaymsg(&sway, &["output", "HEADLESS-2", "scale", "2"])?;
            self.swaymsg(&sway, &["move", "container", "to", "output", "HEADLESS-2"])?;
            self.scale_until("on HEADLESS-2", 2.0)?;
            self.swaymsg(&sway, &["output", "HEADLESS-2", "unplug"])?;
            let back = self.scale_until("once HEADLESS-2 was unplugged", 1.0)?;
            let wire = self.keep("wire");
            let preferred = events_of(&wire, "wl_surface", "preferred_buffer_scale");
            let set = requests_of(&wire, "wl_surface", "set_buffer_scale");
            if preferred != 0 {
                return Err(format!(
                    "{preferred} preferred_buffer_scale events reached a wl_compositor 5 surface"
                ));
            }
            self.gui.stop();
            self.stop_wlhide();
            // The control: no `wl_output.scale` either. The output's scale
            // changes the window's logical size, so once the window has
            // resized it has read everything sent before.
            let control = self.wlhide(&sway, &[rules[0], rules[1], "cap:wl_output=1"])?;
            self.prepare("")?;
            self.launch_wayland(&sway, &control)?;
            let start = self.dims()?;
            self.swaymsg(&sway, &["output", "HEADLESS-1", "scale", "2"])?;
            let w0 = num(&start, "surface_w");
            let resized = self.dims_until("the resize the output's scale makes", &|d| {
                num(d, "surface_w") != w0
            })?;
            let _ = self.keep("control-wire");
            let stayed = scale_of(&resized).is_some_and(|s| (s - 1.0).abs() < 0.01);
            let line = format!(
                "wl_compositor 5, no fractional scale: scale {} at output scale 2, {} at 1, back \
                 to {} after a scale-2 output it moved to was unplugged ({set} set_buffer_scale, \
                 no preferred_buffer_scale); control with wl_output 1: scale {} after the \
                 output's resize",
                scale_of(&two).map_or("?".into(), |s| format!("{s:.2}")),
                scale_of(&one).map_or("?".into(), |s| format!("{s:.2}")),
                scale_of(&back).map_or("?".into(), |s| format!("{s:.2}")),
                scale_of(&resized).map_or("?".into(), |s| format!("{s:.2}"))
            );
            if stayed { Ok(line) } else { Err(line) }
        })())
    }

    fn padding0(&self) -> Ended {
        let sway = match self.sway() {
            Ok(s) => s,
            Err(why) => return not_run(why),
        };
        Self::verdict((|| {
            let hidden = self.wlhide(&sway, &[&format!("hide:{DECORATION}")])?;
            let zero = self.edge_press(&sway, &hidden, "window_padding = 0\n", "padding0")?;
            self.gui.stop();
            let control = self.edge_press(&sway, &hidden, "", "padding-default")?;
            let resized = |r: &str| r.split(' ').any(|q| q.starts_with("resize("));
            let line = format!(
                "padding 0: left edge selected {:?}, last row selected {:?}, requests {}; \
                 default padding: the same left press asked for {} (the last row's press \
                 selected {:?})",
                zero.0, zero.1, zero.2, control.2, control.1
            );
            // Column 0 holds `0`, and the last row `LAST-ROW-…` from column 0, so
            // its press at column 2 starts at `S`.
            if zero.0.starts_with("0123")
                && zero.1.starts_with("ST-")
                && !resized(&zero.2)
                && resized(&control.2)
            {
                Ok(line)
            } else {
                Err(line)
            }
        })())
    }

    /// aterm-gui floating and client-decorated on `socket` with `top` in
    /// its config; the pointer pressed 1 px inside the left edge on grid
    /// row 0 and dragged four cells right, then pressed on the last row's
    /// bottom pixel at column 2 and dragged right: what each selected (as
    /// `selection` reads it) and the frame requests its wire log shows.
    fn edge_press(
        &self,
        sway: &Sway,
        socket: &str,
        top: &str,
        what: &str,
    ) -> Result<(String, String, String), String> {
        self.prepare(top)?;
        self.launch_wayland(sway, socket)?;
        self.swaymsg(sway, &["floating", "enable"])?;
        self.caption_until("resize edges", &|c| c.edges)?;
        // Text from the first column of the first row to the last row, so a
        // drag there selects some: a screen of lines, then the last one
        // without its newline while the shell sleeps (`LAST-%s` so the typed
        // line is not what appears).
        self.ctl(&[
            "send",
            "clear; yes 0123456789abcdefghijklmnopqrstuvwxyz | head -n 99; \
             printf 'LAST-%s' ROW-0123456789; sleep 600",
        ])?;
        self.ctl(&["key", "enter"])?;
        let shown = self.ctl(&["await", "match", "LAST-ROW-0123456789", "timeout=60000"])?;
        if !shown.starts_with("OK match") {
            return Err(format!("the screen of text never came: {shown}"));
        }
        let steps = [
            "make".to_string(),
            format!("at:{},{}", LAYOUT.0 / 2, LAYOUT.1 / 2),
            format!("touch:{}", self.mark("centred")),
            format!("wait:{}", self.mark("aimed")),
            format!("at:@{}", self.mark("left")),
            "sleep:200".into(),
            "press:left".into(),
            format!("at:@{}", self.mark("left-to")),
            "sleep:300".into(),
            "release:left".into(),
            "sleep:300".into(),
            format!("touch:{}", self.mark("left-done")),
            format!("wait:{}", self.mark("aimed-bottom")),
            format!("at:@{}", self.mark("bottom")),
            "sleep:200".into(),
            "press:left".into(),
            format!("at:@{}", self.mark("bottom-to")),
            "sleep:300".into(),
            "release:left".into(),
            "sleep:300".into(),
            format!("touch:{}", self.mark("bottom-done")),
            format!("wait:{}", self.mark("done")),
        ];
        self.pointer(socket, &steps)?;
        self.reached("centred")?;
        let origin = Self::origin(self.pointer_px()?);
        let d = self.dims()?;
        let (cw, ch, h) = (
            num(&d, "cell_w").unwrap_or(0),
            num(&d, "cell_h").unwrap_or(0),
            num(&d, "frame_h").unwrap_or(0),
        );
        let rows = d
            .split_whitespace()
            .nth(1)
            .and_then(|r| r.parse::<i32>().ok())
            .unwrap_or(1);
        let row0 = self.cell_px(0, 0)?;
        self.aim("left", origin, (1, row0.1))?;
        self.aim("left-to", origin, (1 + 4 * cw, row0.1))?;
        self.go("aimed");
        self.reached("left-done")?;
        let left = self.ctl(&["selection"])?;
        // The last row's bottom pixel (the control socket's pointer reads
        // back a cell's middle).
        let last = self.cell_px(rows - 1, 2)?;
        let bottom = (last.1 + ch / 2 - 1).min(h - 1);
        self.aim("bottom", origin, (last.0, bottom))?;
        self.aim("bottom-to", origin, (last.0 + 3 * cw, bottom))?;
        self.go("aimed-bottom");
        self.reached("bottom-done")?;
        let bottom = self.ctl(&["selection"])?;
        self.go("done");
        let wire = self.keep(&format!("{what}-wire"));
        // aterm-ctl prints a selection's lines alone, and says `OK 0 (selection:
        // no results)` for none.
        let text = |r: &str| {
            r.lines()
                .filter(|l| !l.contains("OK 0 (selection"))
                .collect::<Vec<_>>()
                .join("\\n")
        };
        Ok((text(&left), text(&bottom), frame_requests(&wire)))
    }

    fn shadow_margin(&self) -> Ended {
        let sway = match self.sway() {
            Ok(s) => s,
            Err(why) => return not_run(why),
        };
        Self::verdict((|| {
            // A grey ground the shadow darkens (sway's own is black).
            self.swaymsg(&sway, &["output", "*", "bg", GROUND_HEX, "solid_color"])?;
            let hidden = self.wlhide(&sway, &[&format!("hide:{DECORATION}")])?;
            self.prepare("")?;
            self.launch_wayland(&sway, &hidden)?;
            self.swaymsg(&sway, &["floating", "enable"])?;
            // sway's own border would sit where the window's is.
            self.swaymsg(&sway, &["[app_id=aterm]", "border", "none"])?;
            self.caption_until("resize edges", &|c| c.edges)?;
            // The pointer at the layout's centre, where the window reads it:
            // the window's place on the screen.
            let steps = [
                "make".to_string(),
                format!("at:{},{}", LAYOUT.0 / 2, LAYOUT.1 / 2),
                format!("touch:{}", self.mark("centred")),
                format!("wait:{}", self.mark("shot")),
            ];
            self.pointer(&hidden, &steps)?;
            self.reached("centred")?;
            let origin = Self::origin(self.pointer_px()?);
            let d = self.dims()?;
            let (w, h) = (
                num(&d, "frame_w").unwrap_or(0),
                num(&d, "frame_h").unwrap_or(0),
            );
            // grim leaves the cursor out.
            let shot = self.screenshot(&sway, "shadow-screen");
            self.go("shot");
            let shot = shot?;
            let (x, y) = (origin.0 + w, origin.1 + h / 2);
            let at = |dx: i32| shot.at(x + dx, y);
            let (border, near, far) = (at(0), at(1), at(80));
            let pixels =
                format!("right edge {border:?}, 1 px past it {near:?}, 80 px past it {far:?}");
            if border != BORDER_DARK && border != BORDER_LIGHT {
                return Err(format!("no visible border against the window: {pixels}"));
            }
            if far != GROUND || near.iter().zip(far).any(|(n, f)| *n >= f) {
                return Err(format!(
                    "no shadow darkening the ground beyond the border: {pixels}"
                ));
            }
            Ok(format!("{w}x{h} at {},{}: {pixels}", origin.0, origin.1))
        })())
    }

    /// The whole of sway's output, as `grim` takes it, kept beside the
    /// report as `<name>-<what>.png` and decoded.
    fn screenshot(&self, sway: &Sway, what: &str) -> Result<Screen, String> {
        let guest_png = format!("{}/{what}.png", self.gui.dir);
        let rt = format!("{SWAY_DIR}/rt");
        let wayland = format!(
            "WAYLAND_DISPLAY={}",
            Path::new(&sway.socket)
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        );
        let out = self.sh(&[
            "env",
            &format!("XDG_RUNTIME_DIR={rt}"),
            &wayland,
            "grim",
            &guest_png,
        ]);
        if !out.ok() {
            return Err(format!("grim: {}", out.tail(2)));
        }
        let host = self
            .cell
            .results
            .join(format!("{}-{what}.png", self.gui.name));
        let copied = self.up().lima.copy_out(&guest_png, &host);
        if !copied.ok() {
            return Err(format!("{guest_png}: {}", copied.tail(2)));
        }
        let bytes = std::fs::read(&host).map_err(|e| format!("{}: {e}", host.display()))?;
        aterm_png::decode_rgba8(&bytes, 8192)
            .map(|(rgba, w, h)| Screen { rgba, w, h })
            .ok_or_else(|| format!("{}: not a PNG this lab decodes", host.display()))
    }

    // --------------------------------------------------------------- Mutter

    fn fullscreen(&self) -> Ended {
        if let Err(why) = linux::start_display(self.up(), Display::Mutter) {
            return not_run(why);
        }
        Self::verdict((|| {
            self.prepare("tab_strip_rows = 0\n")?;
            self.launch(&[Display::Mutter.client_env(&self.up().runtime)], &[])?;
            let framed = self.caption()?;
            let before = self.dims()?;
            if !framed.client || framed.buttons.is_empty() {
                return Err(format!(
                    "under Mutter the window has no header to lose: buttons={}",
                    framed.names()
                ));
            }
            self.ctl(&["invoke", "ToggleFullScreen"])?;
            let full = self.caption_until("the fullscreen header", &|c| c.buttons.is_empty())?;
            let fd = self.dims_until("the header row's removal", &|d| {
                num(d, "tab_rows") == Some(0)
            })?;
            self.photograph("fullscreen")?;
            self.ctl(&["invoke", "ToggleFullScreen"])?;
            let back = self.caption_until("the header's return", &|c| !c.buttons.is_empty())?;
            let bd = self.dims_until("the header row's return", &|d| {
                num(d, "tab_rows") == Some(1)
            })?;
            Ok(format!(
                "windowed: buttons={} tab_rows={} {}x{}; fullscreen: buttons={} tab_rows=0 {}x{}; \
                 left: buttons={} tab_rows=1 {}x{}",
                framed.names(),
                num(&before, "tab_rows").unwrap_or(-1),
                num(&before, "frame_w").unwrap_or(0),
                num(&before, "frame_h").unwrap_or(0),
                full.names(),
                num(&fd, "frame_w").unwrap_or(0),
                num(&fd, "frame_h").unwrap_or(0),
                back.names(),
                num(&bd, "frame_w").unwrap_or(0),
                num(&bd, "frame_h").unwrap_or(0)
            ))
        })())
    }

    /// `lane-bus seat` on the lane's Mutter, as a lane unit that runs `steps`
    /// and ends. It awaits no frame (`-`: the row has no accessibility bus);
    /// the steps pace themselves with marks, and the unit is waited for until
    /// its RemoteDesktop session has started (a hang detector).
    fn seat(&self, steps: &[String]) -> Result<(), String> {
        let unit_pid = |unit: &str| {
            let out = self.sh(&[
                "systemctl",
                "--user",
                "show",
                "--property=MainPID",
                "--value",
                unit,
            ]);
            let pid = out.text.trim().to_string();
            (out.ok() && !pid.is_empty() && pid != "0").then_some(pid)
        };
        let ready = self.mark("seat-ready");
        let mut argv = vec![
            self.lane_bus(),
            "seat".into(),
            unit_pid(&format!("{}mutter", guest::UNIT_PREFIX)).unwrap_or_else(|| "0".into()),
            "-".into(),
            unit_pid(&Gui::unit_name()).unwrap_or_else(|| "0".into()),
            format!("touch:{ready}"),
        ];
        argv.extend(steps.iter().cloned());
        let unit = Unit {
            name: format!("{}seat", guest::UNIT_PREFIX),
            env: vec![(
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={}/bus", self.up().runtime),
            )],
            argv,
            socket: String::new(),
        };
        let out = self.up().sh(&guest::run_unit(&unit, None));
        if !out.ok() {
            return Err(format!("lane-bus seat: {}", out.tail(3)));
        }
        if self.up().sh(&guest::wait_file(&ready, HANG_SECS)).ok() {
            Ok(())
        } else {
            Err(format!(
                "the seat never started ({HANG_SECS} s): {}",
                crate::util::one_line(&self.journal(&unit.name), 300)
            ))
        }
    }

    /// Wait for the mark `name` the seat touches.
    fn seat_reached(&self, name: &str) -> Result<(), String> {
        if self
            .up()
            .sh(&guest::wait_file(&self.mark(name), HANG_SECS))
            .ok()
        {
            Ok(())
        } else {
            Err(format!(
                "the seat never reached {name} ({HANG_SECS} s): {}",
                crate::util::one_line(&self.journal(&format!("{}seat", guest::UNIT_PREFIX)), 300)
            ))
        }
    }

    /// The window pixel the pointer rests on: the same reading twice,
    /// 300 ms apart (a motion reaches the window a moment after
    /// RemoteDesktop takes it), within the hang detector.
    fn settled_px(&self) -> Result<(i32, i32), String> {
        let t0 = Instant::now();
        let mut last = self.pointer_px()?;
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let now = self.pointer_px()?;
            if now == last {
                return Ok(now);
            }
            if t0.elapsed() > HANG {
                return Err(format!(
                    "the pointer never settled ({HANG_SECS} s): {now:?}"
                ));
            }
            last = now;
        }
    }

    /// The shadow margin's input under Mutter, which (unlike sway) gives a
    /// subsurface outside the window's geometry the pointer.
    fn shadow_handles(&self) -> Ended {
        if let Err(why) = linux::start_display(self.up(), Display::Mutter) {
            return not_run(why);
        }
        Self::verdict((|| {
            self.prepare("")?;
            self.launch(
                &[Display::Mutter.client_env(&self.up().runtime)],
                &[("WAYLAND_DEBUG".into(), "client".into())],
            )?;
            self.caption_until("resize edges", &|c| c.client && c.edges)?;
            let d = self.dims()?;
            let (w, h) = (
                num(&d, "frame_w").unwrap_or(0),
                num(&d, "frame_h").unwrap_or(0),
            );
            let scale = scale_of(&d).filter(|s| *s > 0.0).unwrap_or(1.0);
            // The seat moves in logical pixels, the window reads device ones.
            let logical = |px: i32| (f64::from(px) / scale).round() as i32;
            let mark = |name: &str| self.mark(name);
            let steps = [
                // Mutter makes the seat's pointer with its first motion: parked
                // in the top-left corner, then to the screen's centre, on the
                // window, which Mutter centres.
                "move:-4000,-4000".into(),
                "sleep:300".into(),
                format!("move:{},{}", guest::SCREEN.0 / 2, guest::SCREEN.1 / 2),
                format!("touch:{}", mark("centred")),
                format!("wait:{}", mark("aim")),
                format!("move:@{}", mark("to-handle")),
                "sleep:300".into(),
                format!("touch:{}", mark("on-handle")),
                format!("wait:{}", mark("press-handle")),
                "press:left".into(),
                "sleep:300".into(),
                "release:left".into(),
                "sleep:300".into(),
                format!("touch:{}", mark("pressed-handle")),
                format!("wait:{}", mark("counted")),
                format!("move:@{}", mark("to-past")),
                "sleep:300".into(),
                "press:left".into(),
                "sleep:300".into(),
                "release:left".into(),
                "sleep:300".into(),
                format!("touch:{}", mark("pressed-past")),
            ];
            self.seat(&steps)?;
            self.seat_reached("centred")?;
            let (x, y) = self.settled_px()?;
            // 6 px outside the right edge at mid-height (on the right part's
            // 12 px handle), then 18 px further (past it).
            let to_handle = (logical(w - x) + 6, logical(h / 2 - y));
            self.put(
                &mark("to-handle"),
                &format!("{} {}", to_handle.0, to_handle.1),
            )?;
            self.put(&mark("to-past"), "18 0")?;
            self.go("aim");
            self.seat_reached("on-handle")?;
            self.go("press-handle");
            self.seat_reached("pressed-handle")?;
            let on_handle = frame_requests(&self.wire());
            self.go("counted");
            self.seat_reached("pressed-past")?;
            let wire = self.keep("handles-wire");
            let after = frame_requests(&wire);
            let resizes = requests_of(&wire, "xdg_toplevel", "resize");
            let shaped = bound_version(&wire, "wp_cursor_shape_manager_v1").is_some();
            let shapes = shapes_set(&wire);
            // Without cursor-shape-v1 (Mutter 46) the window sets a cursor
            // surface: on the part, one whose hotspot differs from every one
            // it set on the window itself (the text's, the default's).
            let window = window_surface(&wire).unwrap_or_default();
            let set = cursors_set(&wire);
            let own: Vec<(i32, i32)> = set
                .iter()
                .filter(|(s, _)| *s == window)
                .map(|(_, h)| *h)
                .collect();
            let on_part: Vec<(i32, i32)> = set
                .iter()
                .filter(|(s, _)| !s.is_empty() && *s != window)
                .map(|(_, h)| *h)
                .collect();
            let resize_cursor = on_part.iter().any(|h| !own.contains(h));
            let cursor = if shaped {
                format!("shapes {}", shapes.join(","))
            } else {
                format!("on the part a cursor with hotspot {on_part:?}, on the window {own:?}")
            };
            if on_handle != "resize(right)" {
                return Err(format!(
                    "a press 6 px outside the right edge asked for {:?}, not resize(right) \
                     (pointer at {x},{y} of {w}x{h}, then {},{} seat px)",
                    on_handle, to_handle.0, to_handle.1
                ));
            }
            if resizes != 1 || after != on_handle {
                return Err(format!(
                    "a press 24 px outside the right edge, past the handle, asked for more: \
                     {resizes} resizes, {after}"
                ));
            }
            if (shaped && !shapes.iter().any(|s| s == "e_resize")) || (!shaped && !resize_cursor) {
                return Err(format!(
                    "over the handle the window set no resize cursor: {cursor}"
                ));
            }
            Ok(format!(
                "{w}x{h}, scale {scale}; 6 px out: {on_handle}, {cursor}; 24 px out: nothing more"
            ))
        })())
    }

    /// `gsettings` with the lane's session bus, its trimmed stdout.
    fn gsettings(&self, args: &[&str]) -> Result<String, String> {
        let bus = format!(
            "DBUS_SESSION_BUS_ADDRESS=unix:path={}/bus",
            self.up().runtime
        );
        let mut argv = vec!["env", bus.as_str(), "gsettings"];
        argv.extend_from_slice(args);
        let out = self.sh(&argv);
        if out.ok() {
            Ok(out.text.trim().to_string())
        } else {
            Err(format!("gsettings {}: {}", args.join(" "), out.tail(2)))
        }
    }

    /// Set GNOME's `button-layout` and wait until the Settings portal
    /// answers it (its GTK backend hears dconf when it will: a hang
    /// detector).
    fn set_button_layout(&self, value: &str) -> Result<(), String> {
        self.gsettings(&["set", WM_PREFERENCES, BUTTON_LAYOUT, &format!("'{value}'")])?;
        let bus = format!(
            "DBUS_SESSION_BUS_ADDRESS=unix:path={}/bus",
            self.up().runtime
        );
        let secs = HANG_SECS.to_string();
        let out = self.sh(&[
            "env",
            &bus,
            "timeout",
            &secs,
            "sh",
            "-c",
            "until gdbus call --session --dest org.freedesktop.portal.Desktop \
             --object-path /org/freedesktop/portal/desktop \
             --method org.freedesktop.portal.Settings.Read \"$1\" \"$2\" 2>/dev/null \
             | grep -qF \"'$3'\"; do sleep 0.1; done",
            "portal-read",
            WM_PREFERENCES,
            BUTTON_LAYOUT,
            value,
        ]);
        if out.ok() {
            Ok(())
        } else {
            Err(format!(
                "the portal never answered button-layout {value:?} ({HANG_SECS} s): {}",
                out.tail(2)
            ))
        }
    }

    /// aterm-gui under Mutter, fresh (the layout is read once a process):
    /// its header's buttons, the strip's width, and a photograph.
    fn header_under_mutter(&self, what: &str) -> Result<(Caption, u16), String> {
        self.gui.stop();
        self.prepare("")?;
        self.launch(&[Display::Mutter.client_env(&self.up().runtime)], &[])?;
        let caption = self.caption_until(&format!("the {what} header"), &|c| {
            c.client && !c.buttons.is_empty()
        })?;
        let cols = num(&self.dims()?, "window_cols")
            .and_then(|c| u16::try_from(c).ok())
            .ok_or("`dims` has no window_cols")?;
        self.photograph(what)?;
        Ok((caption, cols))
    }

    fn button_layout(&self) -> Ended {
        if let Err(why) = crate::linux_a11y::portal_up(self.up()) {
            return not_run(format!("the Settings portal did not come up: {why}"));
        }
        if let Err(why) = linux::start_display(self.up(), Display::Mutter) {
            return not_run(why);
        }
        let was = match self.gsettings(&["get", WM_PREFERENCES, BUTTON_LAYOUT]) {
            Ok(v) => v,
            Err(why) => return not_run(why),
        };
        let r = (|| {
            // Both ends: close and minimize flush left in that order, the
            // strip after them, maximize flush right.
            self.set_button_layout("close,minimize:maximize")?;
            let (both, cols) = self.header_under_mutter("layout-both-ends")?;
            let w = both.buttons.first().map_or(0, |&(_, s, e)| e - s);
            let want = vec![
                ("close".to_string(), 0, w),
                ("minimize".to_string(), w, 2 * w),
                ("maximize".to_string(), cols - w, cols),
            ];
            if w == 0 || both.buttons != want || both.right_end(cols) != cols - w {
                return Err(format!(
                    "`close,minimize:maximize` laid out {} on {cols} columns, not \
                     close:0..{w},minimize:{w}..{},maximize:{}..{cols}",
                    cols_of(&both),
                    2 * w,
                    cols - w
                ));
            }
            // GNOME's default: close alone, at the right end.
            self.set_button_layout("appmenu:close")?;
            let (gnome, _) = self.header_under_mutter("layout-gnome-default")?;
            if gnome.buttons != vec![("close".to_string(), cols - w, cols)] {
                return Err(format!(
                    "`appmenu:close` laid out {}, not close:{}..{cols}",
                    cols_of(&gnome),
                    cols - w
                ));
            }
            // No answer: the portal stopped, the same value still set.
            let portal = format!("{}portal", guest::UNIT_PREFIX);
            if !self.sh(&["systemctl", "--user", "stop", &portal]).ok() {
                return Err(format!("{portal} did not stop"));
            }
            let (fallback, _) = self.header_under_mutter("layout-no-portal")?;
            let right: Vec<&str> = fallback
                .buttons
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect();
            if right != ["minimize", "maximize", "close"]
                || fallback.right_end(cols) != cols - 3 * w
            {
                return Err(format!(
                    "with no portal the header laid out {}, not minimize,maximize,close at \
                     the right end",
                    cols_of(&fallback)
                ));
            }
            Ok(format!(
                "close,minimize:maximize -> {}; appmenu:close -> {}; no portal -> {} \
                 ({cols} columns, {w} a button)",
                cols_of(&both),
                cols_of(&gnome),
                cols_of(&fallback)
            ))
        })();
        let put_back = self.gsettings(&["set", WM_PREFERENCES, BUTTON_LAYOUT, &was]);
        let _ = self.sh(&["rm", "-rf", crate::linux_a11y::PORTAL_CONFIG]);
        match (r, put_back) {
            (Ok(d), Ok(_)) => (Verdict::Pass, d),
            (Ok(d), Err(e)) => (Verdict::Fail, format!("{d}; but {e}")),
            (Err(e), _) => (Verdict::Fail, e),
        }
    }
}

impl Run<'_> {
    // ---------------------------------------------------------------- weston

    fn weston_pixman(&self) -> Ended {
        if let Err(why) = linux::start_weston_capturing_with(self.up(), "pixman") {
            return not_run(why);
        }
        let weston = format!("{}weston", guest::UNIT_PREFIX);
        // What this weston said (the unit's journal since its last start).
        let log = self.sh(&[
            "journalctl",
            "--user",
            "--no-pager",
            "-o",
            "cat",
            "-n",
            "400",
            "-u",
            &weston,
        ]);
        let said = log.text.rsplit("Started ").next().unwrap_or_default();
        if !said.contains("Using Pixman renderer") {
            return not_run(format!(
                "weston never said it draws with pixman: {}",
                crate::util::one_line(said, 300)
            ));
        }
        let r = (|| {
            self.prepare("")?;
            self.launch(&[Display::Weston.client_env(&self.up().runtime)], &[])?;
            // Growing past each pool's size (a `wl_shm_pool.resize`) and
            // shrinking back, twice over.
            let sizes = [
                (30, 100),
                (24, 80),
                (34, 120),
                (20, 60),
                (36, 110),
                (26, 90),
                (30, 100),
            ];
            let mut held = Vec::new();
            for (n, (rows, cols)) in sizes.into_iter().enumerate() {
                self.ctl(&["resize", &rows.to_string(), &cols.to_string()])?;
                self.dims_until(&format!("{rows}x{cols}"), &|d| {
                    proof::dims_are(d, rows, cols)
                })?;
                let status = self.ctl(&["status"])?;
                if proof::field(&status, "render") != Some("ok") {
                    return Err(format!("at {rows}x{cols}: {status}"));
                }
                let alive = self.sh(&["systemctl", "--user", "is-active", "--quiet", &weston]);
                if !alive.ok() {
                    return Err(format!(
                        "weston died at the {} resize, to {rows}x{cols}: {}",
                        n + 1,
                        crate::util::one_line(&self.journal(&weston), 400)
                    ));
                }
                let d = self.gui.held_to_server(
                    Display::Weston,
                    self.cell.results,
                    &format!("-pixman-{}", n + 1),
                )?;
                held.push(format!("{rows}x{cols}: {d}"));
            }
            let alive = self.sh(&["systemctl", "--user", "is-active", "--quiet", &weston]);
            if !alive.ok() {
                return Err(format!(
                    "weston died after the last resize: {}",
                    crate::util::one_line(&self.journal(&weston), 400)
                ));
            }
            Ok(format!(
                "weston (pixman) alive after {} resizes, each frame held: {}",
                sizes.len(),
                held.join("; ")
            ))
        })();
        // A step aterm-gui could not answer, because weston died under it
        // (its connection gone), says so first.
        let r = r.map_err(|e| {
            let alive = self.sh(&["systemctl", "--user", "is-active", "--quiet", &weston]);
            if alive.ok() {
                e
            } else {
                let log = self.sh(&[
                    "journalctl",
                    "--user",
                    "--no-pager",
                    "-o",
                    "cat",
                    "-n",
                    "3",
                    "-u",
                    &weston,
                ]);
                format!(
                    "weston died ({}): {e}",
                    crate::util::one_line(&log.text, 200)
                )
            }
        });
        Self::verdict(r)
    }
}

/// The ground the shadow-margin row shows the window over, and the visible
/// border's two colours (sctk-adwaita's light and dark themes').
const GROUND_HEX: &str = "#a0a0a0";
const GROUND: [u8; 3] = [0xa0, 0xa0, 0xa0];
const BORDER_LIGHT: [u8; 3] = [220, 220, 220];
const BORDER_DARK: [u8; 3] = [58, 58, 58];

/// A screenshot, decoded.
struct Screen {
    rgba: Vec<u8>,
    w: usize,
    h: usize,
}

impl Screen {
    /// The colour at (`x`, `y`), black off the screen.
    fn at(&self, x: i32, y: i32) -> [u8; 3] {
        match (usize::try_from(x), usize::try_from(y)) {
            (Ok(x), Ok(y)) if x < self.w && y < self.h => {
                let i = (y * self.w + x) * 4;
                [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
            }
            _ => [0; 3],
        }
    }
}

/// GNOME's window-manager preferences, and the key that places the
/// header's buttons.
const WM_PREFERENCES: &str = "org.gnome.desktop.wm.preferences";
const BUTTON_LAYOUT: &str = "button-layout";

/// A caption's buttons with their columns, as `chrome` printed them.
fn cols_of(c: &Caption) -> String {
    c.buttons
        .iter()
        .map(|(n, s, e)| format!("{n}:{s}..{e}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_has_a_name_it_is_found_by() {
        for e in Edge::ALL {
            assert_eq!(Edge::parse(e.slug()), Some(e));
        }
        assert_eq!(Edge::parse("nope"), None);
    }

    /// sway's tree as swaymsg prints it: an output's empty workspace, then
    /// the same with a view on it.
    #[test]
    fn a_window_is_found_in_swaymsg_tree_however_it_is_spaced() {
        let empty = r#"{ "id": 1, "name": "root", "type": "root", "nodes": [ { "id": 3, "name": "HEADLESS-1", "type": "output", "nodes": [ { "id": 4, "name": "1", "type": "workspace", "nodes": [ ], "floating_nodes": [ ] } ] } ] }"#;
        assert!(!sway_shows(empty, "aterm"), "an empty workspace");
        let pretty = "{\n  \"nodes\": [\n    {\n      \"type\": \"con\",\n      \"app_id\": \"aterm\",\n      \"pid\": 4242\n    }\n  ]\n}";
        assert!(sway_shows(pretty, "aterm"));
        assert!(sway_shows(
            r#"{"nodes":[{"app_id":"aterm","pid":1}]}"#,
            "aterm"
        ));
        assert!(!sway_shows(
            r#"{"nodes":[{"app_id":"aterm-other"}]}"#,
            "aterm"
        ));
    }

    #[test]
    fn the_wire_log_is_read_for_shapes_requests_and_bindings() {
        let log = "\
[1.0]  -> wl_registry@2.bind(2, \"wl_compositor\", 5, new id [unknown]@3)
[1.1]  -> wp_cursor_shape_device_v1@33.set_shape(9, 1)
[1.2]  -> wp_cursor_shape_device_v1@33.set_shape(9, 9)
[1.3]  -> wp_cursor_shape_device_v1@33.set_shape(10, 9)
[1.4]  -> wp_cursor_shape_device_v1@33.set_shape(11, 18)
[1.5] wl_seat@8.capabilities(0)
[1.6]  -> wl_pointer@31.release()
[1.7]  -> wl_pointer@40.set_cursor(12, wl_surface@41, 3, 3)
[1.8] wl_registry@2.global_remove(46)
[1.9] wl_surface@18.preferred_buffer_scale(2)
";
        assert_eq!(shapes_set(log), ["default", "text", "e_resize"]);
        assert_eq!(requests_of(log, "wl_pointer", "release"), 1);
        assert_eq!(requests_of(log, "wl_pointer", "set_cursor"), 1);
        assert_eq!(requests_of(log, "wl_seat", "capabilities"), 0);
        assert_eq!(events_of(log, "wl_registry", "global_remove"), 1);
        assert_eq!(events_of(log, "wl_surface", "preferred_buffer_scale"), 1);
        assert_eq!(events_of(log, "wl_pointer", "release"), 0);
        assert_eq!(bound_version(log, "wl_compositor"), Some(5));
        assert_eq!(bound_version(log, "wl_shm"), None);
    }

    /// Recorded under sway 1.9 (aarch64, `proof-20261020-230136`, the
    /// seat-hotplug row): the first seat, and the second as sway made it.
    #[test]
    fn the_seats_bound_are_read_with_their_globals_and_objects() {
        let log = "\
[2355583.982] wl_registry@2.global(42, \"wl_seat\", 8)
[2355584.025]  -> wl_registry@2.bind(42, \"wl_seat\", 7, new id [unknown]@9)
[2355584.030]  -> wl_registry@2.bind(43, \"wl_shm\", 1, new id [unknown]@10)
[2356443.875] wl_registry@2.global(46, \"wl_seat\", 8)
[2356443.961]  -> wl_registry@2.bind(46, \"wl_seat\", 7, new id [unknown]@33)
";
        assert_eq!(
            seat_binds(log),
            [
                (42, "wl_seat@9".to_string()),
                (46, "wl_seat@33".to_string())
            ]
        );
        assert!(seat_binds("[1.0] wl_registry@2.global(42, \"wl_seat\", 8)").is_empty());
    }

    /// Recorded under sway 1.9 (aarch64, `edges-20261020-222818`, the
    /// output-hotplug row): the window's surface on HEADLESS-1 at 120, moved
    /// to HEADLESS-2 at 240, which then went with no preferred_scale after.
    #[test]
    fn the_preferred_scale_is_the_last_one_sent_to_the_window_s_surface() {
        let log = "\
[156408.709]  -> wl_registry@2.bind(30, \"wp_fractional_scale_manager_v1\", 1, new id [unknown]@12)
[156415.590]  -> xdg_wm_base@6.get_xdg_surface(new id xdg_surface@20, wl_surface@19)
[156415.599]  -> wp_fractional_scale_manager_v1@12.get_fractional_scale(new id wp_fractional_scale_v1@24, wl_surface@19)
[156415.600]  -> wp_fractional_scale_manager_v1@12.get_fractional_scale(new id wp_fractional_scale_v1@25, wl_surface@191)
[156636.041] wl_surface@19.enter(wl_output@8)
[156636.047] wp_fractional_scale_v1@24.preferred_scale(120)
[156780.941] wp_fractional_scale_v1@24.preferred_scale(240)
[156780.942] wp_fractional_scale_v1@25.preferred_scale(360)
[156843.811] wl_surface@19.enter(wl_output@8)
[156843.832] wl_registry@2.global_remove(46)
";
        assert_eq!(window_surface(log).as_deref(), Some("wl_surface@19"));
        assert_eq!(preferred_scale(log, "wl_surface@19"), Some(240));
        assert_eq!(preferred_scale(log, "wl_surface@191"), Some(360));
        assert_eq!(preferred_scale(log, "wl_surface@1"), None);
        let before = log.lines().take(6).collect::<Vec<_>>().join("\n");
        assert_eq!(preferred_scale(&before, "wl_surface@19"), Some(120));
    }

    /// Recorded under Mutter 46 (aarch64, 2026-10-20): the text's cursor set
    /// on the window, the pointer onto the right part, the resize cursor set
    /// there, and a leave with nothing set.
    #[test]
    fn the_wire_log_says_which_surface_each_cursor_was_set_on() {
        let log = "\
[2284390.180]  -> xdg_wm_base@6.get_xdg_surface(new id xdg_surface@18, wl_surface@17)
[2285062.528] wl_pointer@49.enter(1, wl_surface@17, 373.00000000, 310.00000000)
[2285065.678]  -> wl_pointer@49.set_cursor(1, wl_surface@52, 3, 1)
[2285065.866]  -> wl_pointer@49.set_cursor(1, wl_surface@52, 11, 12)
[2285675.737] wl_pointer@49.leave(2, wl_surface@17)
[2285675.771] wl_pointer@49.enter(3, wl_surface@27, 6.00000000, 264.00000000)
[2285675.827]  -> wl_pointer@49.set_cursor(3, wl_surface@52, 19, 13)
[2286081.354]  -> xdg_toplevel@19.resize(wl_seat@8, 4, 8)
[2286785.496] wl_pointer@49.leave(9, wl_surface@27)
[2286785.600]  -> wl_pointer@49.set_cursor(9, nil, 0, 0)
";
        assert_eq!(window_surface(log).as_deref(), Some("wl_surface@17"));
        assert_eq!(
            cursors_set(log),
            [
                ("wl_surface@17".to_string(), (3, 1)),
                ("wl_surface@17".to_string(), (11, 12)),
                ("wl_surface@27".to_string(), (19, 13)),
                (String::new(), (0, 0)),
            ]
        );
        assert_eq!(frame_requests(log), "resize(right)");
    }
}
