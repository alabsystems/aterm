// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE.
//
// LOCAL PATCH (aterm), 2026-10-17 (docs/AUDIT-third-party-2026-10-15.md, LW.7 with winit-core
// WC.8): the globals are bound through first-party `aterm-wayland`'s registry, at the versions
// smithay-client-toolkit 0.19.2 bound them, and the state applies what the loop's dispatch
// heard in the order it heard it (`WinitState::drain`): one `Inbox` holds the backend's own
// objects' events and, through sinks, the seats', the drags', the selection sources' and the
// outputs' `done`s. sctk's `CompositorHandler`, `WindowHandler`, `SeatHandler`, device handlers
// and output scale watchers are the `Wire` arms here; the per-surface scale is the backend's own
// `SurfaceScale`, as sctk's `SurfaceData` kept it, and an output's scale is the one its last
// applied `done` gave, as sctk's `OutputData` kept it.

use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use aterm_hash::FxHashMap;
use aterm_wayland::globals::Registry;
use aterm_wayland::output::Outputs;
use aterm_wayland::protocols::cursor_shape_v1::wp_cursor_shape_manager_v1;
use aterm_wayland::protocols::fractional_scale_v1::wp_fractional_scale_manager_v1;
use aterm_wayland::protocols::text_input_unstable_v3::zwp_text_input_manager_v3;
use aterm_wayland::protocols::viewporter::wp_viewporter;
use aterm_wayland::protocols::wayland::{wl_compositor, wl_shm};
use aterm_wayland::protocols::xdg_activation_v1::xdg_activation_v1;
use aterm_wayland::proxy::{Display, Proxy, Queue};
use aterm_wayland::seat::Seats;
use aterm_wayland::selection::{Kind, Manager};
use aterm_wayland::xdg::Shell;
use tracing::warn;

use crate::platform_impl::wayland::clipboard::ClipboardState;
use crate::platform_impl::wayland::event_loop::sink::EventSink;
use crate::platform_impl::wayland::output::MonitorHandle;
use crate::platform_impl::wayland::seat::data_device::DndPipe;
use crate::platform_impl::wayland::seat::{Retired, WinitPointer, WinitSeatState};
use crate::platform_impl::wayland::window::state::{
    DecorationMode, WindowConfigure, WindowManagerCapabilities, XdgWindowState,
};
use crate::platform_impl::wayland::window::{WindowRequests, WindowState};
use crate::platform_impl::wayland::wire::{Inbox, Owned, SurfaceEvent, ToplevelEvent, Wire};
use crate::platform_impl::wayland::{WaylandError, WindowId};
use crate::platform_impl::OsError;

/// The display connection and the loop's queue, shared by the loop's state and every window,
/// so neither outlives them: a window kept past the loop keeps its connection.
pub(crate) struct Connection {
    /// The queue every object of the backend's is on; destroyed before the display goes.
    pub(crate) queue: Queue,
    display: aterm_wayland::proxy::OwnedDisplay,
}

// SAFETY: the queue is read and dispatched only by the event loop's thread (its state's
// methods, and `Window::new`, which runs there); every other thread holding the connection
// holds it to keep it alive, and reads only the display pointer, which libwayland lets any
// thread use.
unsafe impl Sync for Connection {}

impl Connection {
    /// Connect to the compositor `name` names, else to `$WAYLAND_DISPLAY` (or
    /// `$WAYLAND_SOCKET`, as libwayland reads them), and make the loop's queue.
    pub(crate) fn connect(
        name: Option<&std::ffi::CStr>,
    ) -> Result<Connection, aterm_wayland::proxy::Error> {
        let display = aterm_wayland::proxy::OwnedDisplay::connect(name)?;
        // SAFETY: the owner moves into Connection after its queue, so the queue is
        // destroyed first. Dispatch is confined to the event-loop thread; window
        // object destruction is serialized there, or follows its final dispatch.
        let queue = Queue::new(unsafe { display.display() })?;
        Ok(Connection { queue, display })
    }

    /// The display.
    pub(crate) fn display(&self) -> Display {
        // SAFETY: callers retain this Connection while using the handle. Its queue
        // drops before its display, and only the event-loop thread dispatches it.
        unsafe { self.display.display() }
    }
}

/// The scale of one of the backend's surfaces (a window's, or a cursor's), as sctk's
/// `SurfaceData` kept it: the outputs the surface is on, and its integer scale (1 until the
/// compositor or an output says otherwise).
#[derive(Debug)]
pub struct SurfaceScale {
    outputs: Vec<Proxy>,
    factor: i32,
}

impl Default for SurfaceScale {
    fn default() -> Self {
        Self { outputs: Vec::new(), factor: 1 }
    }
}

/// Winit's Wayland state.
///
/// The fields drop in order: the windows and the seats' objects first, then the globals, the
/// registry, and the connection last.
pub struct WinitState {
    /// The currently present windows.
    pub windows: RefCell<FxHashMap<WindowId, Arc<Mutex<WindowState>>>>,

    /// The requests from the `Window` to EventLoop, such as close operations and redraw requests.
    pub window_requests: RefCell<FxHashMap<WindowId, Arc<WindowRequests>>>,

    /// The events that were generated directly from the window.
    pub window_events_sink: Arc<Mutex<EventSink>>,

    /// The update for the `windows` coming from the compositor.
    pub window_compositor_updates: Vec<WindowCompositorUpdate>,

    /// Currently handled seats, by registry name.
    pub seats: FxHashMap<u32, WinitSeatState>,

    /// Currently present cursor surfaces.
    pub pointer_surfaces: FxHashMap<WindowId, Arc<WinitPointer>>,

    /// Pointers and text inputs whose capability went, until no window holds them.
    pub(crate) retired: Vec<Retired>,

    /// The scale of each of the backend's surfaces.
    pub surface_scales: FxHashMap<WindowId, SurfaceScale>,

    /// Each output's integer scale as of its last `done` the inbox applied, by registry name.
    output_scales: FxHashMap<u32, i32>,

    /// Observed monitors.
    pub monitors: Arc<Mutex<Vec<MonitorHandle>>>,

    /// Sink to accumulate window events from the compositor, which is latter dispatched in
    /// event loop run.
    pub events_sink: EventSink,

    /// The clipboard sources this client offers and the own-selection slots
    /// the `WaylandClipboard` handle shares (see `clipboard.rs`).
    pub(crate) clipboard: ClipboardState,

    /// In-flight drag-and-drop `text/uri-list` reads, streamed through the event
    /// loop until EOF and then parsed into `WindowEvent::DroppedFile`s.
    pub dnd_pipes: Vec<DndPipe>,

    /// The window a file drag is currently hovering over (a `HoveredFile` was
    /// emitted for it), so `leave` knows which window to send `HoveredFileCancelled`
    /// to — the Wayland `leave` event carries no surface. `None` when no file drag
    /// is over any of our windows. See `data_device.rs`.
    pub dnd_hover_window: Option<WindowId>,

    /// Whether we have dispatched events to the user thus we want to
    /// send `AboutToWait` and normally wakeup the user.
    pub dispatched_events: bool,

    /// Where the backend's own objects record their events.
    pub(crate) inbox: Inbox,

    /// The `wl_data_device_manager`, used to create a `wl_data_device` per seat
    /// for drag-and-drop and the clipboard. `None` if the compositor doesn't
    /// advertise it (file drops then simply never arrive). See `data_device.rs`.
    pub(crate) data_device_manager: Option<Manager>,

    /// The `zwp_primary_selection_device_manager_v1`, one device per seat, for
    /// the PRIMARY selection (see `clipboard.rs`). `None` without the global.
    pub(crate) primary_selection_manager: Option<Manager>,

    /// The text-input-v3 manager.
    pub(crate) text_input_manager: Option<Owned>,

    /// cursor-shape-v1's manager.
    pub(crate) cursor_shape_manager: Option<Owned>,

    /// Xdg activation.
    pub(crate) xdg_activation: Option<Arc<Owned>>,

    /// Viewporter (bound only beside fractional scaling, as upstream bound it).
    pub(crate) viewporter: Option<Owned>,

    /// Fractional scaling manager.
    pub(crate) fractional_scaling_manager: Option<Owned>,

    /// The XDG shell that is used for windows.
    pub(crate) xdg_shell: Arc<Shell>,

    /// The `wl_shm`, for the cursor theme's images.
    shm: Arc<Owned>,

    /// The compositor, which is used to create new surfaces and regions.
    pub(crate) compositor: Arc<Owned>,

    /// The seats and their devices (aterm-wayland's), shared with the windows for the serials
    /// and seat a move, a resize or a menu names.
    pub(crate) seats_book: Arc<Seats>,

    /// The outputs.
    pub(crate) outputs: Outputs,

    /// The registry, and its generation when the outputs and seats were last synced.
    registry_generation: u64,
    registry: Registry,

    /// The connection; dropped last.
    pub(crate) connection: Arc<Connection>,
}

impl WinitState {
    pub fn new(connection: Arc<Connection>) -> Result<Self, OsError> {
        let queue = &connection.queue;
        let registry = Registry::new(queue)?;
        let bind = |name: &std::ffi::CStr,
                    iface: &'static aterm_wayland::iface::Interface,
                    version: u32|
         -> Option<Proxy> {
            registry.find(name).and_then(|global| registry.bind(&global, iface, version))
        };
        let owned = |proxy: Option<Proxy>, destroy: unsafe fn(Proxy)| {
            // SAFETY: a global just bound, owned here alone; its destructor (or the proxy's
            // client-side destruction for one with none).
            proxy.map(|proxy| unsafe { Owned::new(proxy, destroy) })
        };
        // LOCAL PATCH (aterm), LW.5: no `wl_subcompositor` bind; it served only the
        // client-side frame's subsurfaces, and the window has no frame.
        let compositor = owned(
            bind(c"wl_compositor", &wl_compositor::INTERFACE, 6),
            aterm_wayland::proxy::Proxy::destroy,
        )
        .ok_or(WaylandError::Missing("wl_compositor"))?;
        let shm =
            owned(bind(c"wl_shm", &wl_shm::INTERFACE, 1), aterm_wayland::proxy::Proxy::destroy)
                .ok_or(WaylandError::Missing("wl_shm"))?;
        let xdg_shell = Shell::bind(&registry).map_err(|_| WaylandError::Missing("xdg_wm_base"))?;

        // Every event the backend hears goes to one inbox, in the order it was dispatched.
        let inbox = Inbox::default();
        let outputs = Outputs::bind_all(&registry, inbox.output_sink())?;
        let seats_book = Arc::new(Seats::bind_all(&registry, inbox.seat_sink())?);

        // The data-device manager drives drag-and-drop (and clipboard) offers. It
        // is optional: a compositor without it just never delivers file drops.
        let data_device_manager = Manager::bind(&registry, Kind::Clipboard);
        let primary_selection_manager = Manager::bind(&registry, Kind::Primary);

        let fractional_scaling_manager = owned(
            bind(c"wp_fractional_scale_manager_v1", &wp_fractional_scale_manager_v1::INTERFACE, 1),
            wp_fractional_scale_manager_v1::destroy,
        );
        let viewporter = fractional_scaling_manager.as_ref().and_then(|_| {
            owned(bind(c"wp_viewporter", &wp_viewporter::INTERFACE, 1), wp_viewporter::destroy)
        });
        let xdg_activation = owned(
            bind(c"xdg_activation_v1", &xdg_activation_v1::INTERFACE, 1),
            xdg_activation_v1::destroy,
        );
        let text_input_manager = owned(
            bind(c"zwp_text_input_manager_v3", &zwp_text_input_manager_v3::INTERFACE, 1),
            zwp_text_input_manager_v3::destroy,
        );
        let cursor_shape_manager = owned(
            bind(c"wp_cursor_shape_manager_v1", &wp_cursor_shape_manager_v1::INTERFACE, 1),
            wp_cursor_shape_manager_v1::destroy,
        );

        let registry_generation = registry.generation();
        let mut state = Self {
            windows: Default::default(),
            window_requests: Default::default(),
            window_events_sink: Default::default(),
            window_compositor_updates: Vec::new(),
            seats: Default::default(),
            pointer_surfaces: Default::default(),
            retired: Vec::new(),
            surface_scales: Default::default(),
            output_scales: Default::default(),
            monitors: Arc::new(Mutex::new(Vec::new())),
            events_sink: EventSink::new(),
            clipboard: ClipboardState::default(),
            dnd_pipes: Vec::new(),
            dnd_hover_window: None,
            // Make it true by default.
            dispatched_events: true,
            inbox,
            data_device_manager,
            primary_selection_manager,
            text_input_manager,
            cursor_shape_manager,
            xdg_activation: xdg_activation.map(Arc::new),
            viewporter,
            fractional_scaling_manager,
            xdg_shell: Arc::new(xdg_shell),
            shm: Arc::new(shm),
            compositor: Arc::new(compositor),
            seats_book,
            outputs,
            registry_generation,
            registry,
            connection: Arc::clone(&connection),
        };
        // Bind a data device for each startup seat (later seats get theirs as they come),
        // so drags onto our surfaces are received from the start.
        state.add_new_seats();
        state.refresh_monitors();
        Ok(state)
    }

    /// The registry.
    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The `wl_compositor`.
    pub(crate) fn compositor(&self) -> Proxy {
        self.compositor.proxy()
    }

    /// The `wl_shm`.
    pub(crate) fn shm(&self) -> Arc<Owned> {
        Arc::clone(&self.shm)
    }

    /// Destroy the retired seat objects no window holds; apply everything the last dispatch
    /// heard, in the order it heard it (the backend's own objects', the outputs', the seats',
    /// the drags' and the selection sources' events share one inbox); then bind or release the
    /// globals that came or went.
    pub(crate) fn drain(&mut self) {
        self.reap_retired();
        self.add_new_seats();
        for wire in self.inbox.take() {
            self.apply(wire);
        }
        let generation = self.registry.generation();
        if generation != self.registry_generation {
            self.registry_generation = generation;
            self.sync_globals();
        }
        if !self.events_sink.is_empty() || !self.window_compositor_updates.is_empty() {
            self.dispatched_events = true;
        }
    }

    /// A global came or went: outputs and seats follow the registry (sctk's
    /// `registry_handlers![OutputState, SeatState]`).
    fn sync_globals(&mut self) {
        // The monitors' lock is held while an output is released and the list renewed, so a
        // window's request naming a monitor's output (`set_fullscreen`) finds it bound or not
        // at all.
        let monitors = Arc::clone(&self.monitors);
        let mut monitors = monitors.lock().unwrap();
        match self.outputs.sync(&self.registry) {
            Ok(true) => {
                *monitors = self.outputs.outputs().into_iter().map(MonitorHandle::new).collect()
            },
            Ok(false) => {},
            Err(_error) => {
                warn!("wayland: the outputs could not be bound: {_error}");
                // sync may have released old outputs before a later bind failed.
                *monitors = self.outputs.outputs().into_iter().map(MonitorHandle::new).collect();
            },
        }
        drop(monitors);
        let outputs = &self.outputs;
        self.output_scales.retain(|global, _| outputs.proxy(*global).is_some());
        self.sync_seats();
    }

    /// The monitors list, as the outputs now are.
    fn refresh_monitors(&mut self) {
        let monitors = self.outputs.outputs().into_iter().map(MonitorHandle::new).collect();
        *self.monitors.lock().unwrap() = monitors;
    }

    fn apply(&mut self, wire: Wire) {
        match wire {
            Wire::Toplevel { window, event } => self.toplevel_event(window, event),
            Wire::Surface { surface, event } => self.surface_event(surface, event),
            Wire::PreferredScale { window, scale } => {
                self.scale_factor_changed(window, scale as f64 / SCALE_DENOMINATOR, false)
            },
            Wire::Frame { window } => self.frame(window),
            Wire::TextInput { seat, event } => self.text_input_event(seat, event),
            Wire::ActivationDone { window, token } => self.activation_done(window, &token),
            Wire::OutputDone(info) => self.output_done(info),
            Wire::Seat(input) => self.seat_input(input),
            Wire::Drag { seat, event } => self.drag_event(seat, event),
            Wire::Source { selection, source, event } => {
                self.source_event(selection, source, event)
            },
        }
    }

    /// One event of a window's xdg objects: sctk 0.19.2's pending configure, updated by each
    /// toplevel and decoration event and handed to the window on `xdg_surface.configure`.
    fn toplevel_event(&mut self, window_id: WindowId, event: ToplevelEvent) {
        let configure = {
            let windows = self.windows.get_mut();
            let Some(window) = windows.get(&window_id) else { return };
            let mut window = window.lock().unwrap();
            let pending = &mut window.pending_configure;
            match event {
                ToplevelEvent::Configure { width, height, states } => {
                    let width = u32::try_from(width).ok().and_then(std::num::NonZeroU32::new);
                    let height = u32::try_from(height).ok().and_then(std::num::NonZeroU32::new);
                    pending.new_size = (width, height);
                    pending.state = XdgWindowState::from_wire(&states);
                    return;
                },
                ToplevelEvent::Bounds { width, height } => {
                    pending.suggested_bounds = if width == 0 && height == 0 {
                        None
                    } else {
                        Some((width as u32, height as u32))
                    };
                    return;
                },
                ToplevelEvent::Capabilities(capabilities) => {
                    pending.capabilities = WindowManagerCapabilities::from_wire(&capabilities);
                    return;
                },
                ToplevelEvent::DecorationMode(mode) => {
                    use aterm_wayland::protocols::xdg_decoration_unstable_v1::zxdg_toplevel_decoration_v1::mode as m;
                    match mode {
                        m::CLIENT_SIDE => pending.decoration_mode = DecorationMode::Client,
                        m::SERVER_SIDE => pending.decoration_mode = DecorationMode::Server,
                        _unknown => tracing::error!("unknown decoration mode 0x{:x}", _unknown),
                    }
                    return;
                },
                ToplevelEvent::Close => None,
                ToplevelEvent::Configured => Some(pending.clone()),
            }
        };
        match configure {
            Some(configure) => self.configure(window_id, configure),
            // sctk's `WindowHandler::request_close`.
            None => Self::queue_close(&mut self.window_compositor_updates, window_id),
        }
    }

    /// sctk's `WindowHandler::configure` (the configure is already acked).
    fn configure(&mut self, window_id: WindowId, configure: WindowConfigure) {
        let pos = if let Some(pos) =
            self.window_compositor_updates.iter().position(|update| update.window_id == window_id)
        {
            pos
        } else {
            self.window_compositor_updates.push(WindowCompositorUpdate::new(window_id));
            self.window_compositor_updates.len() - 1
        };

        // Populate the configure to the window.
        self.window_compositor_updates[pos].resized |= self
            .windows
            .get_mut()
            .get_mut(&window_id)
            .expect("got configure for dead window.")
            .lock()
            .unwrap()
            .configure(configure);

        // NOTE: configure demands wl_surface::commit, however winit doesn't commit on behalf of the
        // users, since it can break a lot of things, thus it'll ask users to redraw instead.
        if let Some(requests) = self.window_requests.get_mut().get(&window_id) {
            requests.redraw_requested.store(true, Ordering::Relaxed);
        }

        // Manually mark that we've got an event, since configure may not generate a resize.
        self.dispatched_events = true;
    }

    /// One event of a surface of the backend's (sctk's `CompositorHandler` and its
    /// `SurfaceData`): the outputs it is on, and with them its integer scale.
    fn surface_event(&mut self, surface: WindowId, event: SurfaceEvent) {
        let entry = self.surface_scales.entry(surface).or_default();
        match event {
            SurfaceEvent::Enter(Some(output)) => {
                entry.outputs.push(output);
                if let Some(output) = self.outputs.by_proxy(output) {
                    if let Some(window) = self.windows.get_mut().get(&surface) {
                        window.lock().unwrap().outputs.push(output);
                    }
                }
            },
            SurfaceEvent::Leave(Some(output)) => {
                entry.outputs.retain(|o| *o != output);
                if let Some(window) = self.windows.get_mut().get(&surface) {
                    window.lock().unwrap().outputs.retain(|o| o.proxy() != output);
                }
            },
            SurfaceEvent::Enter(None) | SurfaceEvent::Leave(None) => return,
            SurfaceEvent::PreferredBufferScale(factor) => {
                // NOTE: with v6 the scale is the compositor's to say, and only this says it.
                let current = std::mem::replace(&mut entry.factor, factor);
                if current != factor {
                    self.scale_factor_changed(surface, factor as f64, true);
                }
                return;
            },
        }
        if self.surface_version(surface) < 6 {
            self.rescale_surface(surface);
        }
    }

    /// The surface's protocol version (a window's or a cursor's).
    fn surface_version(&mut self, surface: WindowId) -> u32 {
        if let Some(window) = self.windows.get_mut().get(&surface) {
            // SAFETY: the window's live surface.
            return unsafe { window.lock().unwrap().surface().version() };
        }
        if self.pointer_surfaces.contains_key(&surface) {
            // SAFETY: the live compositor; a surface's version is its compositor's.
            return unsafe { self.compositor().version() };
        }
        0
    }

    /// An output's `done`: its scale as of it, and, when the scale changed, every surface that
    /// takes its scale from the outputs rescaled (sctk's output scale watchers, which ran at the
    /// `done` that changed a scale).
    fn output_done(&mut self, info: aterm_wayland::output::OutputInfo) {
        if self.output_scales.insert(info.global, info.scale) != Some(info.scale) {
            self.rescale_legacy_surfaces();
        }
    }

    /// A surface's integer scale before `wl_compositor` version 6: the largest scale of the
    /// outputs it is on, each as of its last applied `done` (sctk's
    /// `dispatch_surface_state_updates`); unchanged while it is on none.
    fn rescale_surface(&mut self, surface: WindowId) {
        let Some(entry) = self.surface_scales.get_mut(&surface) else { return };
        let factor = entry
            .outputs
            .iter()
            .filter_map(|proxy| self.outputs.by_proxy(*proxy))
            .filter_map(|output| self.output_scales.get(&output.global()).copied())
            .max();
        let Some(factor) = factor else { return };
        let current = std::mem::replace(&mut entry.factor, factor);
        if factor != current {
            self.scale_factor_changed(surface, factor as f64, true);
        }
    }

    /// An output's scale moved: rescale every surface that takes its scale from the outputs
    /// (sctk's output scale watcher).
    fn rescale_legacy_surfaces(&mut self) {
        let surfaces: Vec<WindowId> = self.surface_scales.keys().copied().collect();
        for surface in surfaces {
            if self.surface_version(surface) < 6 {
                self.rescale_surface(surface);
            }
        }
    }

    pub fn scale_factor_changed(&mut self, surface: WindowId, scale_factor: f64, is_legacy: bool) {
        // Check if the cursor surface.
        let window_id = surface;

        if let Some(window) = self.windows.get_mut().get(&window_id) {
            // Don't update the scaling factor, when legacy method is used.
            if is_legacy && self.fractional_scaling_manager.is_some() {
                return;
            }

            // The scale factor change is for the window.
            let pos = if let Some(pos) = self
                .window_compositor_updates
                .iter()
                .position(|update| update.window_id == window_id)
            {
                pos
            } else {
                self.window_compositor_updates.push(WindowCompositorUpdate::new(window_id));
                self.window_compositor_updates.len() - 1
            };

            // Update the scale factor right away.
            window.lock().unwrap().set_scale_factor(scale_factor);
            self.window_compositor_updates[pos].scale_changed = true;
        } else if let Some(pointer) = self.pointer_surfaces.get(&surface) {
            pointer.set_cursor_scale(scale_factor as i32);

            // Get the window, where the pointer resides right now.
            let focused_window = match pointer.focused_window() {
                Some(focused_window) => focused_window,
                None => return,
            };

            if let Some(window_state) = self.windows.get_mut().get(&focused_window) {
                window_state.lock().unwrap().reload_cursor_style()
            }
        }
    }

    /// sctk's `CompositorHandler::frame`.
    fn frame(&mut self, window_id: WindowId) {
        let window = match self.windows.get_mut().get(&window_id) {
            Some(window) => window,
            None => return,
        };

        // In case we have a redraw requested we must indicate the wake up.
        if self
            .window_requests
            .get_mut()
            .get(&window_id)
            .is_some_and(|requests| requests.redraw_requested.load(Ordering::Relaxed))
        {
            self.dispatched_events = true;
        }

        window.lock().unwrap().frame_callback_received();
    }

    /// The attention request's activation token arrived: activate the window with it.
    fn activation_done(&mut self, window_id: WindowId, token: &str) {
        let Some(window) = self.windows.get_mut().get(&window_id) else { return };
        let mut window = window.lock().unwrap();
        if let Some(activation) = self.xdg_activation.as_ref() {
            let token = crate::platform_impl::wayland::window::state::protocol_string(token);
            // SAFETY: the live activation global and the window's live surface.
            unsafe { xdg_activation_v1::activate(activation.proxy(), &token, window.surface()) };
        }
        // Mark that no request attention is in process, and destroy the spent token.
        window.attention_requested = false;
        window.objects.attention_token = None;
    }

    pub fn queue_close(updates: &mut Vec<WindowCompositorUpdate>, window_id: WindowId) {
        let pos = if let Some(pos) = updates.iter().position(|update| update.window_id == window_id)
        {
            pos
        } else {
            updates.push(WindowCompositorUpdate::new(window_id));
            updates.len() - 1
        };

        updates[pos].close_window = true;
    }
}

impl Drop for WinitState {
    fn drop(&mut self) {
        // A Window may retain this list after the loop. Empty it before Outputs
        // drops; the same lock lets any in-flight fullscreen request finish first.
        self.monitors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }
}

/// The scaling factor denominator.
const SCALE_DENOMINATOR: f64 = 120.;

// The window update coming from the compositor.
#[derive(Debug, Clone, Copy)]
pub struct WindowCompositorUpdate {
    /// The id of the window this updates belongs to.
    pub window_id: WindowId,

    /// New window size.
    pub resized: bool,

    /// New scale factor.
    pub scale_changed: bool,

    /// Close the window.
    pub close_window: bool,
}

impl WindowCompositorUpdate {
    fn new(window_id: WindowId) -> Self {
        Self { window_id, resized: false, scale_changed: false, close_window: false }
    }
}
