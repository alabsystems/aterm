// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-wl`: the second Wayland client the Linux runtime lane's baseline
//! needs (`targo --unverified lab linux baseline`,
//! `crates/aterm-lab/src/linux_baseline.rs`, builds it in the guest with
//! `cargo build -p aterm-wayland --example lane-wl`). A dev tool over this
//! crate, never shipped; an example, so it builds from the crate's
//! dev-dependencies and adds nothing to what depends on it.
//!
//! * `drag <uri> <marks>`: a drag source on the compositor `WAYLAND_DISPLAY`
//!   names, which drags `<uri>` (as `text/uri-list`, the copy action) from a
//!   window of its own. It maps a maximized toplevel, so the seat's pointer
//!   is over it wherever the pointer is, and creates `<marks>/mapped`; then
//!   `<marks>/pointed` once the pointer has entered it. On the left button's
//!   press it starts the drag with that press's serial and unmaps the window
//!   (the drag lives on: a compositor ends a drag when its origin surface is
//!   destroyed, not when it unmaps), so what was under it is under the
//!   pointer, and creates `<marks>/dragging`. From then on each thing the
//!   compositor tells the source is a line of `<marks>/report`, flushed as
//!   it comes (`target <type|none>`, `action <none|copy|move|ask>`,
//!   `send <type>`, `drop-performed`, `finished`, `cancelled`), and a `send`
//!   is answered with `<uri>` and CRLF. It ends once the drag has finished or
//!   was cancelled.
//!
//! The seat is driven from outside (`lane-bus seat`, Mutter's RemoteDesktop,
//! or `lane-bus xseat`, XTEST into weston's x11 backend), which paces itself
//! on the markers. Exit 0 done, 1 failed, 2 usage, 3 not
//! available here (no compositor, no seat, no data device): the lane reads 3
//! as NOT RUN.

use std::process::ExitCode;

#[cfg(target_os = "linux")]
mod lane {
    use std::ffi::CStr;
    use std::io::Write as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::path::Path;
    use std::process::ExitCode;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use aterm_wayland::globals::Registry;
    use aterm_wayland::iface::Interface;
    use aterm_wayland::pointer_frame::PointerKind;
    use aterm_wayland::protocols::wayland::{
        wl_buffer, wl_compositor, wl_data_device_manager, wl_shm, wl_shm_pool, wl_surface,
    };
    use aterm_wayland::protocols::xdg_shell::xdg_toplevel;
    use aterm_wayland::proxy::{OwnedDisplay, Proxy, Queue};
    use aterm_wayland::seat::{Input, Seats};
    use aterm_wayland::selection::{Device, Kind, Manager, Source, SourceEvent, write_detached};
    use aterm_wayland::xdg::{Shell, Toplevel};

    /// A wait for what the compositor must do on its own (a configure, an
    /// event answering a request): a hang detector, never a budget.
    const HANG: Duration = Duration::from_secs(60);

    /// A wait for what the lab's seat does, which paces itself on the lab's
    /// photographs (each a one-minute hang detector of its own): longer.
    const LAB: Duration = Duration::from_secs(600);

    const URI_LIST: &CStr = c"text/uri-list";
    const BTN_LEFT: u32 = 0x110;

    /// What went wrong: nothing here to drag on (NOT RUN), or a fault.
    enum Fail {
        NotHere(String),
        Fault(String),
    }

    fn fault(why: impl std::fmt::Display) -> Fail {
        Fail::Fault(why.to_string())
    }

    pub(crate) fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let r = match args.as_slice() {
            [verb, uri, marks] if verb == "drag" => drag(uri, Path::new(marks)),
            _ => {
                eprintln!("usage: lane-wl drag <uri> <marks>");
                return ExitCode::from(2);
            }
        };
        match r {
            Ok(()) => ExitCode::SUCCESS,
            Err(Fail::NotHere(why)) => {
                eprintln!("lane-wl: NOT AVAILABLE: {why}");
                ExitCode::from(3)
            }
            Err(Fail::Fault(why)) => {
                eprintln!("lane-wl: {why}");
                ExitCode::from(1)
            }
        }
    }

    /// What the seat and the source said and the drag has not read yet.
    #[derive(Default)]
    struct Heard {
        input: Vec<Input>,
        source: Vec<SourceEvent>,
    }

    #[derive(Clone, Default)]
    struct Book(Arc<Mutex<Heard>>);

    impl Book {
        fn heard(&self) -> std::sync::MutexGuard<'_, Heard> {
            self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    fn mark(marks: &Path, name: &str) -> Result<(), Fail> {
        std::fs::File::create(marks.join(name))
            .map(drop)
            .map_err(|e| fault(format!("{}: {e}", marks.join(name).display())))
    }

    /// Dispatch `queue` until `done` holds, within `limit`.
    fn pump(
        queue: &Queue,
        what: &str,
        limit: Duration,
        mut done: impl FnMut() -> Result<bool, Fail>,
    ) -> Result<(), Fail> {
        let t0 = Instant::now();
        loop {
            queue
                .roundtrip()
                .map_err(|e| fault(format!("{what}: the display failed: {e}")))?;
            if done()? {
                return Ok(());
            }
            if t0.elapsed() > limit {
                return Err(fault(format!("{what}: not within {} s", limit.as_secs())));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn bind(
        registry: &Registry,
        name: &CStr,
        iface: &'static Interface,
        v: u32,
    ) -> Result<Proxy, Fail> {
        let global = registry
            .find(name)
            .ok_or_else(|| Fail::NotHere(format!("the compositor offers no {name:?}")))?;
        registry
            .bind(&global, iface, v.min(global.version))
            .ok_or_else(|| fault(format!("{name:?} could not be bound")))
    }

    /// A shm file of `len` bytes, every pixel a mid grey.
    fn grey(len: usize) -> Result<OwnedFd, Fail> {
        // SAFETY: a fresh memfd this function owns from here on.
        let fd = unsafe {
            let fd = libc::memfd_create(c"lane-wl".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return Err(fault(format!("memfd: {}", std::io::Error::last_os_error())));
            }
            OwnedFd::from_raw_fd(fd)
        };
        let mut file = std::fs::File::from(fd);
        let row = [0x80_u8; 4096];
        let mut left = len;
        while left > 0 {
            let n = left.min(row.len());
            file.write_all(&row[..n])
                .map_err(|e| fault(format!("the buffer: {e}")))?;
            left -= n;
        }
        Ok(OwnedFd::from(file))
    }

    fn action(bits: u32) -> &'static str {
        use wl_data_device_manager::dnd_action::{ASK, COPY, MOVE};
        match bits {
            0 => "none",
            COPY => "copy",
            MOVE => "move",
            ASK => "ask",
            _ => "several",
        }
    }

    fn drag(uri: &str, marks: &Path) -> Result<(), Fail> {
        let display = OwnedDisplay::connect(None)
            .map_err(|e| Fail::NotHere(format!("no Wayland display: {e}")))?;
        // SAFETY: the display outlives the queue, the registry and every
        // object below, all dropped at the end of this function in reverse
        // order of their making; one thread dispatches.
        let queue = Queue::new(unsafe { display.display() }).map_err(fault)?;
        let registry = Registry::new(&queue).map_err(fault)?;
        let compositor = bind(&registry, c"wl_compositor", &wl_compositor::INTERFACE, 4)?;
        let shm = bind(&registry, c"wl_shm", &wl_shm::INTERFACE, 1)?;
        let shell =
            Shell::bind(&registry).map_err(|e| Fail::NotHere(format!("xdg_wm_base: {e}")))?;
        let book = Book::default();
        let seats = {
            let book = book.clone();
            Seats::bind_all(&registry, Arc::new(move |i| book.heard().input.push(i)))
                .map_err(fault)?
        };
        let clipboard = Manager::bind(&registry, Kind::Clipboard).ok_or_else(|| {
            Fail::NotHere("the compositor offers no wl_data_device_manager".into())
        })?;
        queue.roundtrip().map_err(fault)?;
        let seat = seats
            .infos()
            .first()
            .map(|s| s.id)
            .ok_or_else(|| Fail::NotHere("the compositor offers no wl_seat".into()))?;
        let seat_proxy = seats.proxy(seat).ok_or_else(|| fault("the seat went"))?;
        // SAFETY: the live seat, outliving the device (dropped first below).
        let device =
            unsafe { Device::new(&clipboard, seat_proxy, Arc::new(drop)) }.map_err(fault)?;

        // SAFETY: the bound compositor.
        let surface = unsafe { wl_compositor::create_surface(compositor) }
            .ok_or_else(|| fault("no surface"))?;
        // SAFETY: a fresh surface with no role, outliving the toplevel.
        let toplevel =
            unsafe { Toplevel::new(&shell, surface, c"aterm lane drag", c"aterm.lane.drag") }
                .map_err(fault)?;
        // SAFETY: the live toplevel and surface.
        unsafe {
            xdg_toplevel::set_maximized(toplevel.toplevel());
            wl_surface::commit(surface);
        }
        let mut size = None;
        pump(&queue, "a maximized configure", HANG, || {
            if let Some(c) = toplevel.take_configure() {
                toplevel.ack(c.serial);
                if c.states.contains(&xdg_toplevel::state::MAXIMIZED) && c.width > 0 && c.height > 0
                {
                    size = Some((c.width, c.height));
                }
            }
            Ok(size.is_some())
        })?;
        let (w, h) = size.unwrap_or((1, 1));
        let stride = w.checked_mul(4).ok_or_else(|| fault("a width past i32"))?;
        let len = stride
            .checked_mul(h)
            .ok_or_else(|| fault("a size past i32"))?;
        let fd = grey(usize::try_from(len).map_err(|_| fault("a negative size"))?)?;
        // SAFETY: live objects of this function, each destroyed once at its end.
        let (pool, buffer) = unsafe {
            let pool =
                wl_shm::create_pool(shm, fd.as_raw_fd(), len).ok_or_else(|| fault("no pool"))?;
            let buffer =
                wl_shm_pool::create_buffer(pool, 0, w, h, stride, wl_shm::format::XRGB8888)
                    .ok_or_else(|| fault("no buffer"))?;
            wl_surface::attach(surface, Some(buffer), 0, 0);
            wl_surface::damage_buffer(surface, 0, 0, w, h);
            wl_surface::commit(surface);
            (pool, buffer)
        };
        queue.roundtrip().map_err(fault)?;
        mark(marks, "mapped")?;

        // The seat's pointer: onto the window, then the left button's press.
        let mut entered = false;
        let mut press = None;
        pump(&queue, "the pointer's press on the window", LAB, || {
            for input in std::mem::take(&mut book.heard().input) {
                let Input::Pointer { frame, .. } = input else {
                    continue;
                };
                for e in frame.iter().filter(|e| e.surface == surface) {
                    match e.kind {
                        PointerKind::Enter { .. } if !entered => {
                            entered = true;
                            mark(marks, "pointed")?;
                        }
                        PointerKind::Press { button, serial, .. } if button == BTN_LEFT => {
                            press = Some(serial);
                        }
                        _ => {}
                    }
                }
            }
            Ok(press.is_some())
        })?;
        let serial = press.unwrap_or(0);

        let source = {
            let book = book.clone();
            Source::new(
                &clipboard,
                &[URI_LIST],
                wl_data_device_manager::dnd_action::COPY,
                Arc::new(move |e| book.heard().source.push(e)),
            )
            .map_err(fault)?
        };
        // SAFETY: the live surface the pointer pressed on; no icon.
        if !unsafe { device.start_drag(Some(&source), surface, None, serial) } {
            return Err(fault("the data device refused the drag"));
        }
        // Out of the way: what was under the window is under the pointer.
        // SAFETY: the live surface; a null buffer unmaps it.
        unsafe {
            wl_surface::attach(surface, None, 0, 0);
            wl_surface::commit(surface);
        }
        queue.roundtrip().map_err(fault)?;
        mark(marks, "dragging")?;

        let path = marks.join("report");
        let mut report =
            std::fs::File::create(&path).map_err(|e| fault(format!("{}: {e}", path.display())))?;
        let mut ended = false;
        pump(&queue, "the drag's end", LAB, || {
            for event in std::mem::take(&mut book.heard().source) {
                let line = match event {
                    SourceEvent::Target(t) => format!("target {}", t.as_deref().unwrap_or("none")),
                    SourceEvent::Action(a) => format!("action {}", action(a)),
                    SourceEvent::Send { mime_type, fd } => {
                        let mut bytes = uri.as_bytes().to_vec();
                        bytes.extend_from_slice(b"\r\n");
                        write_detached(fd, bytes).map_err(|e| fault(format!("send: {e}")))?;
                        format!("send {mime_type}")
                    }
                    SourceEvent::DropPerformed => "drop-performed".into(),
                    SourceEvent::Finished => {
                        ended = true;
                        "finished".into()
                    }
                    SourceEvent::Cancelled => {
                        ended = true;
                        "cancelled".into()
                    }
                };
                println!("{line}");
                writeln!(report, "{line}")
                    .and_then(|()| report.flush())
                    .map_err(|e| fault(format!("{}: {e}", path.display())))?;
            }
            Ok(ended)
        })?;

        drop(source);
        drop(device);
        drop(toplevel);
        // SAFETY: this function's objects, each destroyed once.
        unsafe {
            wl_surface::destroy(surface);
            wl_buffer::destroy(buffer);
            wl_shm_pool::destroy(pool);
            shm.destroy();
            compositor.destroy();
        }
        drop((clipboard, seats));
        queue.roundtrip().map_err(fault)?;
        // SAFETY: the display is live.
        match unsafe { display.display() }.error() {
            None => Ok(()),
            Some(e) => Err(fault(format!("a protocol error: {e}"))),
        }
    }
}

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    {
        lane::main()
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("lane-wl: NOT AVAILABLE: a Wayland client for the Linux lane's guest");
        ExitCode::from(3)
    }
}
