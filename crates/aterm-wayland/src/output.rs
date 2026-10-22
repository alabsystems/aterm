// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Outputs: every `wl_output` the registry lists, bound on the registry's
//! queue (version 4 or what the compositor offers), and what each says about
//! itself (geometry, modes, scale, name, description), kept as of its last
//! `done`, and handed to the caller's [`Sink`] at each `done`, in the order
//! the queue dispatched it ([`crate::sink`]). The window system reads
//! monitors, their modes and scale factors from here, and binds outputs as
//! they come and releases them as they go ([`Outputs::sync`]).

use std::ffi::CStr;
use std::sync::{Arc, Mutex, PoisonError};

use crate::globals::{Global, Registry};
use crate::protocols::wayland::wl_output;
use crate::proxy::{Error, EventArg, Listener, Proxy};
use crate::sink::Sink;

/// One mode an output offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    /// `wl_output.mode` flags: current (1), preferred (2).
    pub flags: u32,
    pub width: i32,
    pub height: i32,
    /// Refresh rate in mHz.
    pub refresh: i32,
}

/// What one output says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputInfo {
    /// The registry's name for the global.
    pub global: u32,
    /// `wl_output.name` (version 4), such as `DP-1`.
    pub name: Option<String>,
    pub description: Option<String>,
    pub make: String,
    pub model: String,
    pub x: i32,
    pub y: i32,
    pub physical_width: i32,
    pub physical_height: i32,
    pub subpixel: i32,
    pub transform: i32,
    /// The integer scale factor (`wl_output.scale`, version 2); 1 before.
    pub scale: i32,
    pub modes: Vec<Mode>,
}

impl OutputInfo {
    /// The mode flagged current, if any.
    #[inline]
    pub fn current_mode(&self) -> Option<Mode> {
        self.modes
            .iter()
            .rev()
            .find(|m| m.flags & wl_output::mode::CURRENT != 0)
            .copied()
    }
}

/// An output this module bound. Dropping it releases it (a destructor
/// request from version 3, the proxy alone before), then frees its listener.
struct Bound {
    proxy: Proxy,
    version: u32,
    /// The registry's name for the global.
    global: u32,
    output_book: Arc<Mutex<OutputBook>>,
    listener: Option<Listener>,
}

/// One output's reports: what is being built since the last `done`, and the
/// state as of the last `done` (`None` before the first). A leaf of the lock
/// graph: only its own method runs while it is held.
struct OutputBook {
    pending: OutputInfo,
    done: Option<OutputInfo>,
}

/// One `wl_output` event, decoded.
enum Reported {
    Geometry {
        x: i32,
        y: i32,
        physical_width: i32,
        physical_height: i32,
        subpixel: i32,
        make: String,
        model: String,
        transform: i32,
    },
    Mode(Mode),
    Scale(i32),
    Name(String),
    Description(String),
    Done,
}

fn reported(opcode: u32, args: &[EventArg<'_>]) -> Option<Reported> {
    Some(match (opcode, args) {
        (
            wl_output::evt::GEOMETRY,
            [
                EventArg::Int(x),
                EventArg::Int(y),
                EventArg::Int(pw),
                EventArg::Int(ph),
                EventArg::Int(subpixel),
                EventArg::Str(make),
                EventArg::Str(model),
                EventArg::Int(transform),
            ],
        ) => Reported::Geometry {
            x: *x,
            y: *y,
            physical_width: *pw,
            physical_height: *ph,
            subpixel: *subpixel,
            make: text(*make),
            model: text(*model),
            transform: *transform,
        },
        (
            wl_output::evt::MODE,
            [
                EventArg::Uint(flags),
                EventArg::Int(width),
                EventArg::Int(height),
                EventArg::Int(refresh),
            ],
        ) => Reported::Mode(Mode {
            flags: *flags,
            width: *width,
            height: *height,
            refresh: *refresh,
        }),
        (wl_output::evt::SCALE, [EventArg::Int(scale)]) => Reported::Scale(*scale),
        (wl_output::evt::NAME, [EventArg::Str(name)]) => Reported::Name(text(*name)),
        (wl_output::evt::DESCRIPTION, [EventArg::Str(desc)]) => Reported::Description(text(*desc)),
        (wl_output::evt::DONE, []) => Reported::Done,
        _ => return None,
    })
}

impl OutputBook {
    /// Take one report; at `done`, the output's state as of it, for the sink.
    fn hear(&mut self, report: Reported) -> Option<OutputInfo> {
        let info = &mut self.pending;
        match report {
            Reported::Geometry {
                x,
                y,
                physical_width,
                physical_height,
                subpixel,
                make,
                model,
                transform,
            } => {
                info.x = x;
                info.y = y;
                info.physical_width = physical_width;
                info.physical_height = physical_height;
                info.subpixel = subpixel;
                info.make = make;
                info.model = model;
                info.transform = transform;
            }
            Reported::Mode(mode) => {
                // A mode sent again replaces its earlier report.
                info.modes.retain(|m| {
                    (m.width, m.height, m.refresh) != (mode.width, mode.height, mode.refresh)
                });
                if mode.flags & wl_output::mode::CURRENT != 0 {
                    for m in &mut info.modes {
                        m.flags &= !wl_output::mode::CURRENT;
                    }
                }
                info.modes.push(mode);
            }
            Reported::Scale(scale) => info.scale = scale,
            Reported::Name(name) => info.name = Some(name),
            Reported::Description(desc) => info.description = Some(desc),
            Reported::Done => {
                self.done = Some(self.pending.clone());
                return self.done.clone();
            }
        }
        None
    }
}

/// Listen to an output: each event, decoded with the book unlocked, goes to
/// its book, and a `done`'s state to `sink`, with the book unlocked again.
///
/// # Safety
/// `proxy` is a live output this crate bound, without a listener, released
/// before the listener is freed.
unsafe fn output_listener(
    proxy: Proxy,
    output_book: &Arc<Mutex<OutputBook>>,
    sink: &Sink<OutputInfo>,
) -> Result<Listener, Error> {
    let output_book = Arc::clone(output_book);
    let sink = Arc::clone(sink);
    // SAFETY: the caller's contract.
    unsafe {
        proxy.listen(move |_, opcode, args| {
            let Some(report) = reported(opcode, args) else {
                return;
            };
            let done = output_book
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .hear(report);
            if let Some(done) = done {
                sink(done);
            }
        })
    }
}

/// One output: its registry name, its proxy and what it said about itself.
/// A cheap handle, kept by whoever names a monitor; [`Output::info`] reads the
/// output's book, which stays readable after the output is released.
#[derive(Clone)]
pub struct Output {
    global: u32,
    proxy: Proxy,
    output_book: Arc<Mutex<OutputBook>>,
}

impl Output {
    /// The registry's name for the global: how outputs are told apart.
    #[inline]
    pub const fn global(&self) -> u32 {
        self.global
    }

    /// The `wl_output`, live while [`Outputs`] holds it bound (a surface's
    /// `enter` names it; `xdg_toplevel.set_fullscreen` takes it).
    #[inline]
    pub const fn proxy(&self) -> Proxy {
        self.proxy
    }

    /// What the output said as of its last `done` (`None` before its first).
    pub fn info(&self) -> Option<OutputInfo> {
        self.output_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .done
            .clone()
    }
}

impl PartialEq for Output {
    fn eq(&self, other: &Output) -> bool {
        self.global == other.global
    }
}

impl Eq for Output {}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("global", &self.global)
            .finish_non_exhaustive()
    }
}

/// Every output of a registry, bound and listened to.
pub struct Outputs {
    bound: Vec<Bound>,
    /// Where every output's state goes at its `done`.
    sink: Sink<OutputInfo>,
}

fn text(s: Option<&CStr>) -> String {
    s.map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Bind `global` (a `wl_output`) at version 4 at most, and listen to it.
fn bind_one(registry: &Registry, global: &Global, sink: &Sink<OutputInfo>) -> Result<Bound, Error> {
    let version = global.version.min(4);
    let proxy = registry
        .bind(global, &wl_output::INTERFACE, version)
        .ok_or(Error::NoObject)?;
    let output_book = Arc::new(Mutex::new(OutputBook {
        pending: OutputInfo {
            global: global.name,
            scale: 1,
            ..OutputInfo::default()
        },
        done: None,
    }));
    let mut output = Bound {
        proxy,
        version,
        global: global.name,
        output_book,
        listener: None,
    };
    // SAFETY: an output this crate bound, without a listener; its owner releases
    // or destroys it before the listener on success or error.
    output.listener = Some(unsafe { output_listener(proxy, &output.output_book, sink) }?);
    Ok(output)
}

impl Outputs {
    /// Bind every `wl_output` `registry` lists. Their first `done` arrives
    /// on the registry's queue: dispatch it (a round trip) before reading.
    /// Each output's state goes to `sink` at each of its `done`s.
    pub fn bind_all(registry: &Registry, sink: Sink<OutputInfo>) -> Result<Outputs, Error> {
        let mut outputs = Outputs {
            bound: Vec::new(),
            sink,
        };
        outputs.sync(registry)?;
        Ok(outputs)
    }

    /// Bind the outputs `registry` lists that are not bound yet (their first
    /// `done` follows on the registry's queue), and release the ones it no
    /// longer lists. Whether anything was bound or released.
    pub fn sync(&mut self, registry: &Registry) -> Result<bool, Error> {
        let globals: Vec<Global> = registry
            .globals()
            .into_iter()
            .filter(|g| g.interface.as_c_str() == c"wl_output")
            .collect();
        let before = self.bound.len();
        // Released as they drop.
        self.bound
            .retain(|b| globals.iter().any(|g| g.name == b.global));
        let mut changed = self.bound.len() != before;
        for global in &globals {
            if self.bound.iter().any(|b| b.global == global.name) {
                continue;
            }
            self.bound.push(bind_one(registry, global, &self.sink)?);
            changed = true;
        }
        Ok(changed)
    }

    /// Every bound output, in the order they were bound.
    pub fn outputs(&self) -> Vec<Output> {
        self.bound.iter().map(Bound::handle).collect()
    }

    /// The output whose `wl_output` is `proxy` (a surface's `enter` names
    /// one).
    pub fn by_proxy(&self, proxy: Proxy) -> Option<Output> {
        self.bound
            .iter()
            .find(|b| b.proxy == proxy)
            .map(Bound::handle)
    }

    /// Every output's state as of its last `done` (outputs that have not
    /// sent one yet are left out).
    pub fn infos(&self) -> Vec<OutputInfo> {
        self.bound
            .iter()
            .filter_map(|b| b.handle().info())
            .collect()
    }

    /// The proxy of the output the registry names `global`, for matching a
    /// surface's `enter` events.
    pub fn proxy(&self, global: u32) -> Option<Proxy> {
        self.bound
            .iter()
            .find(|b| b.global == global)
            .map(|b| b.proxy)
    }
}

impl Bound {
    fn handle(&self) -> Output {
        Output {
            global: self.global,
            proxy: self.proxy,
            output_book: Arc::clone(&self.output_book),
        }
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        // SAFETY: an output this value bound, released or destroyed once; its
        // listener is freed after.
        unsafe {
            if self.version >= 3 {
                wl_output::release(self.proxy);
            } else {
                self.proxy.destroy();
            }
        }
        drop(self.listener.take());
    }
}
