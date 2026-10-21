// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus`: the D-Bus client the Linux runtime lane runs inside its guest
//! (`targo --unverified lab linux baseline`, `crates/aterm-lab/src/linux_baseline.rs`,
//! builds it there with `-p aterm-dbus --bin lane-bus`). A dev tool over this
//! crate's [`Connection`], never shipped. It reads what an assistive
//! technology reads, drives the one input device a headless compositor can
//! be given, and (the one verb off the bus) is the drag source an X11 drop
//! needs:
//!
//! * `atspi-walk <app> <pid>`: the AT-SPI tree that process `<pid>`
//!   publishes as `<app>` on the accessibility bus (at-spi2-core's, its
//!   address asked of the session bus), one line per node, depth first: role,
//!   name, description, states, interfaces, attributes, and the Text,
//!   Component and Action readings;
//! * `atspi-listen <app> <out> <stop>`: every AT-SPI event an application
//!   named `<app>` emits, one line each with the emitting process's pid,
//!   appended to `<out>` (flushed per line) until the file `<stop>` exists; a
//!   hang detector ends it after ten minutes;
//! * `seat <mutter-pid> <app> <app-pid> <step>...`: a RemoteDesktop session
//!   of the Mutter whose process is `<mutter-pid>`, on the session bus, whose
//!   virtual keyboard and pointer give headless Mutter's seat the
//!   capabilities it otherwise lacks. A harmless `Shift_L` tap goes first
//!   (Mutter adds the keyboard with the first key, which no client hears),
//!   and the steps start once the frame that `<app>` (process `<app-pid>`)
//!   publishes on the accessibility bus is `active`: the window has the new
//!   keyboard's focus, so no key is lost while the client binds the keyboard.
//!   When the steps drive the pointer, a motion parks it in the screen's
//!   top-left corner before that tap (Mutter makes the pointer with its first
//!   motion and announces it to the window a frame later, before the keyboard
//!   made after it), so every step's motion reaches a seat that has it.
//!   When they drive a finger, a tap in that corner makes the touchscreen
//!   (no client hears the first touch either); the client must have bound
//!   it before a finger step reaches it, which the lab awaits with a file.
//!   The session ends once the frame is inactive again. An `<app>` of `-`
//!   awaits no frame: the client paces the steps with files of its own;
//! * `cast <mutter-pid> <out.png>`: one frame of that Mutter's primary
//!   monitor, composited, as a PNG: a ScreenCast session of its own (the
//!   cursor hidden) whose PipeWire stream `gst-launch-1.0` reads one buffer
//!   of, the compositor's side of a present under Mutter;
//! * `xseat <app> <app-pid> <step>...`: `seat`'s steps faked through XTEST on
//!   the X display `DISPLAY` names ([`xtest`]): the input of weston's x11
//!   backend nested in that display, whose seat reads its X window. Its
//!   keyboard and pointer are weston's for as long as weston runs, so no key
//!   primes them and the frame keeps the focus after the steps; they start
//!   once the frame is `active`, and a pointer session parks the pointer in
//!   the top-left corner first, as `seat`'s does. A key is pressed only where
//!   the X keymap's first level holds its keysym; XTEST fakes no finger;
//! * `xdnd <window> <uri> <marks-dir>`: an XDND drag source, a second X
//!   client on the display `DISPLAY` names, that drags `<uri>` over the X
//!   window `<window>` and away, then over it and drops it ([`xdnd`]);
//! * `xgrab <window> <out>`: the X window's pixels as the server holds them,
//!   as a PPM ([`xgrab`]), the server's side of a CPU present;
//! * `xhide <display> <server> <extension>,...|-`: X display
//!   `<display>`, a proxy to `<server>` that answers the named extensions
//!   absent (`xhide`), for the lane's servers without XInput 2 or XKB;
//! * `wlhide <listen> <server> <rule>...`: a Wayland socket that
//!   proxies the compositor `<server>` with some of its globals hidden or
//!   announced at an older version (`wlhide`).
//!
//! Each verb picks its peer by process id, not by name: one guest's buses
//! carry every lane's aterm and every lane's Mutter, and a second Mutter
//! holds no well-known name but still serves its objects at its unique
//! name. Steps of `seat`: `tap:K`, `down:K`, `up:K`,
//!   `chord:K+K+…` (pressed in order, released in reverse), `button:B` (an
//!   evdev button: `left`, `middle`, `right` or a number), `press:B` and
//!   `release:B` (its halves, for a drag), `move:DX,DY` (the pointer, by that
//!   many logical pixels: RemoteDesktop moves it absolutely only inside a
//!   screencast stream, relatively always) and `move:@PATH` (by the `DX DY`
//!   the file holds when the step runs, so the lab can aim from what the
//!   window reports), `finger-down:X,Y`, `finger-move:X,Y` and `finger-up:`
//!   (a touchscreen's finger, at a point of the screen in its pixels, or
//!   `@PATH` for the `X Y` a file holds: RemoteDesktop places a finger only
//!   in a screencast stream's space, so a session with a finger records the
//!   primary monitor, which needs pipewire), `sleep:MS` (a key held that
//!   long: a stimulus, never a wait), `touch:PATH` (create the file: the lab
//!   waits for it) and `wait:PATH` (until the lab creates it; the devices
//!   live until the session's last step). A key `K` is an X keysym, by name
//!   or `0x…`.
//!
//! Exit 0 done, 1 failed, 2 usage, 3 not available here (no accessibility
//! bus, no such application, no Mutter, no X server, no XTEST): the lane reads
//! 3 as NOT RUN.
//!
//! Untrusted input: every reply comes from another process on the bus; a
//! reading this tool cannot make sense of is printed as `ERR(<why>)` in its
//! field, never a panic.

use std::process::ExitCode;

#[cfg(unix)]
mod relay;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
mod wlhide;
#[cfg(unix)]
mod xdnd;
#[cfg(unix)]
mod xgrab;
#[cfg(unix)]
mod xhide;
#[cfg(unix)]
mod xtest;

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        unix::main()
    }
    #[cfg(not(unix))]
    {
        eprintln!("lane-bus: NOT AVAILABLE: a unix D-Bus client for the Linux lane's guest");
        ExitCode::from(3)
    }
}
