// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live Wayland seat, selection, drag, and touch tests against headless Mutter.

#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_wayland::globals::Registry;
use aterm_wayland::pointer_frame::PointerKind;
use aterm_wayland::protocols::wayland::{
    wl_compositor, wl_data_device_manager, wl_shm, wl_shm_pool, wl_surface,
};
use aterm_wayland::protocols::xdg_shell::xdg_toplevel;
use aterm_wayland::proxy::{OwnedDisplay, Proxy, Queue};
use aterm_wayland::seat::{Input, KeyboardEvent, Seats, TouchEvent};
use aterm_wayland::selection::{
    Device, DragEvent, Kind, Manager, Source, SourceEvent, read_limited, write_detached,
};
use aterm_wayland::sink::Sink;
use aterm_wayland::xdg::{Shell, Toplevel};

/// A wait for what must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

const TEXT: &CStr = c"text/plain;charset=utf-8";
const URI_LIST: &CStr = c"text/uri-list";
const BTN_LEFT: u32 = 0x110;
const KEY_A: u32 = 30;
const KEY_B: u32 = 48;
const KEY_C: u32 = 46;
/// `wl_seat.capability`'s touch bit.
const CAP_TOUCH: u32 = 4;

struct Lane {
    display: CString,
    mutter: String,
    bus: PathBuf,
}

fn lane() -> Option<Lane> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match (
        var("LANE_SEAT_WAYLAND_DISPLAY"),
        var("LANE_SEAT_MUTTER_PID"),
        var("LANE_SEAT_BUS"),
    ) {
        (Some(display), Some(mutter), Some(bus)) => Some(Lane {
            display: CString::new(display).expect("a display name"),
            mutter,
            bus: PathBuf::from(bus),
        }),
        _ => {
            eprintln!("SKIP: no Mutter with a RemoteDesktop seat named (LANE_SEAT_*)");
            None
        }
    }
}

/// Dispatch `queue` until `done` holds, within the hang detector.
fn pump(queue: &Queue, what: &str, mut done: impl FnMut() -> bool) {
    let t0 = Instant::now();
    loop {
        if let Err(e) = queue.roundtrip() {
            panic!("{what}: the display failed: {e}");
        }
        if done() {
            eprintln!("{what}: {:.2} s", t0.elapsed().as_secs_f64());
            return;
        }
        assert!(
            t0.elapsed() < HANG,
            "{what}: not within {} s",
            HANG.as_secs()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn touch(dir: &Path, name: &str) {
    std::fs::File::create(dir.join(name)).expect("a pacing file");
}

fn memfd(len: usize) -> OwnedFd {
    // SAFETY: A fresh memfd, sized for this test.
    unsafe {
        let fd = libc::memfd_create(c"aterm-wayland-seat".as_ptr(), libc::MFD_CLOEXEC);
        assert!(fd >= 0);
        assert_eq!(libc::ftruncate(fd, len as libc::off_t), 0);
        OwnedFd::from_raw_fd(fd)
    }
}

/// What the helpers heard and the test has not read yet, by kind.
#[derive(Default)]
struct Heard {
    input: Vec<Input>,
    drags: Vec<DragEvent>,
    /// The sources' requests, each with the name the test gave its source.
    requests: Vec<(&'static str, SourceEvent)>,
}

/// Shared event log that every helper sink writes into.
#[derive(Clone, Default)]
struct Ears(Arc<Mutex<Heard>>);

impl Ears {
    fn heard(&self) -> std::sync::MutexGuard<'_, Heard> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn seats(&self) -> Sink<Input> {
        let ears = self.clone();
        Arc::new(move |input| ears.heard().input.push(input))
    }

    fn drags(&self) -> Sink<DragEvent> {
        let ears = self.clone();
        Arc::new(move |event| ears.heard().drags.push(event))
    }

    fn source(&self, name: &'static str) -> Sink<SourceEvent> {
        let ears = self.clone();
        Arc::new(move |event| ears.heard().requests.push((name, event)))
    }

    /// The seats' input since the last take.
    fn input(&self) -> Vec<Input> {
        std::mem::take(&mut self.heard().input)
    }

    /// The drag events since the last take.
    fn drag_events(&self) -> Vec<DragEvent> {
        std::mem::take(&mut self.heard().drags)
    }

    /// The requests source `name` heard since the last take.
    fn requests(&self, name: &str) -> Vec<SourceEvent> {
        let mut heard = self.heard();
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut heard.requests)
            .into_iter()
            .partition(|(n, _)| *n == name);
        heard.requests = rest;
        mine.into_iter().map(|(_, event)| event).collect()
    }
}

/// Answer every `send` source `name` was asked for with `bytes`; the other requests, by name.
fn serve(ears: &Ears, name: &str, bytes: &[u8]) -> Vec<String> {
    let mut other = Vec::new();
    for event in ears.requests(name) {
        match event {
            SourceEvent::Send { mime_type, fd } => {
                eprintln!("source asked for {mime_type}");
                write_detached(fd, bytes.to_vec()).expect("a writer thread");
            }
            e => other.push(format!("{e:?}")),
        }
    }
    other
}

/// Dispatches `queue`, answers the source with `bytes`, and returns what `reader` receives.
fn read_through(
    queue: &Queue,
    reader: std::io::Result<std::io::PipeReader>,
    (ears, source): (&Ears, &str),
    bytes: &[u8],
    heard: &mut Vec<String>,
) -> Vec<u8> {
    let reader = reader.expect("a pipe to read");
    queue.roundtrip().expect("the request reaches the source");
    heard.extend(serve(ears, source, bytes));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_limited(reader, 1 << 16));
    });
    rx.recv_timeout(HANG)
        .expect("the bytes and their end within the hang detector")
        .expect("a read")
}

/// The client's state: a maximized toplevel and the seat helpers.
struct Client {
    registry: Registry,
    queue: Queue,
    display: OwnedDisplay,
}

impl Client {
    fn connect(lane: &Lane) -> Client {
        let display = OwnedDisplay::connect(Some(&lane.display)).expect("Mutter's display");
        // SAFETY: Client drops registry, queue, then display; all dispatch
        // and object teardown is serialized on this test thread.
        let queue = Queue::new(unsafe { display.display() }).unwrap();
        let registry = Registry::new(&queue).unwrap();
        Client {
            display,
            queue,
            registry,
        }
    }

    fn bind(&self, name: &CStr, iface: &'static aterm_wayland::iface::Interface, v: u32) -> Proxy {
        let global = self
            .registry
            .find(name)
            .unwrap_or_else(|| panic!("{name:?}"));
        self.registry.bind(&global, iface, v).unwrap()
    }
}

#[test]
fn a_real_seat_reaches_the_helpers() {
    let Some(lane) = lane() else {
        return;
    };
    let a = Client::connect(&lane);
    let compositor = a.bind(c"wl_compositor", &wl_compositor::INTERFACE, 4);
    let shm = a.bind(c"wl_shm", &wl_shm::INTERFACE, 1);
    let shell = Shell::bind(&a.registry).unwrap();
    let ears = Ears::default();
    let seats = Seats::bind_all(&a.registry, ears.seats()).unwrap();
    let clipboard = Manager::bind(&a.registry, Kind::Clipboard).expect("wl_data_device_manager");
    let primary =
        Manager::bind(&a.registry, Kind::Primary).expect("Mutter offers the primary selection");
    a.queue.roundtrip().unwrap();
    let infos = seats.infos();
    eprintln!("seats: {infos:?}");
    let seat = infos.first().expect("Mutter has a seat").id;
    let seat_proxy = seats.proxy(seat).unwrap();
    // SAFETY: The live seat, outliving the devices (dropped first below).
    let (device, primary_device) = unsafe {
        (
            Device::new(&clipboard, seat_proxy, ears.drags()).unwrap(),
            Device::new(&primary, seat_proxy, ears.drags()).unwrap(),
        )
    };

    // A maximized toplevel, so the seat's pointer is over it wherever it is.
    // SAFETY: The compositor bound above.
    let surface = unsafe { wl_compositor::create_surface(compositor) }.unwrap();
    // SAFETY: A fresh surface with no role, outliving the toplevel.
    let toplevel = unsafe { Toplevel::new(&shell, surface, c"aterm seat", c"aterm.seat") }.unwrap();
    // SAFETY: The live toplevel and surface.
    unsafe {
        xdg_toplevel::set_maximized(toplevel.toplevel());
        wl_surface::commit(surface);
    }
    let mut configure = None;
    pump(&a.queue, "a maximized configure", || {
        if let Some(c) = toplevel.take_configure() {
            let maximized = c.states.contains(&xdg_toplevel::state::MAXIMIZED);
            let sized = c.width > 0 && c.height > 0;
            toplevel.ack(c.serial);
            if maximized && sized {
                configure = Some(c);
            }
        }
        configure.is_some()
    });
    let configure = configure.unwrap();
    eprintln!("configure: {configure:?}");
    let (w, h) = (configure.width, configure.height);
    let len = usize::try_from(w * h * 4).unwrap();
    let fd = memfd(len);
    // SAFETY: Live Wayland objects destroyed once when the test ends.
    let (pool, buffer) = unsafe {
        let pool = wl_shm::create_pool(shm, fd.as_raw_fd(), i32::try_from(len).unwrap()).unwrap();
        let buffer =
            wl_shm_pool::create_buffer(pool, 0, w, h, w * 4, wl_shm::format::XRGB8888).unwrap();
        wl_surface::attach(surface, Some(buffer), 0, 0);
        wl_surface::damage_buffer(surface, 0, 0, w, h);
        wl_surface::commit(surface);
        (pool, buffer)
    };
    a.queue.roundtrip().unwrap();
    assert_eq!(
        unsafe { a.display.display() }.error(),
        None,
        "mapped cleanly"
    );

    let pace = aterm_tempfile::tempdir().unwrap();
    let file = |n: &str| pace.path().join(n).display().to_string();
    let steps = [
        format!("wait:{}", file("focused")),
        "tap:a".into(),
        "tap:b".into(),
        "move:3,3".into(),
        format!("wait:{}", file("bound")),
        "move:3,3".into(),
        format!("wait:{}", file("pointed")),
        "press:left".into(),
        format!("wait:{}", file("fresh")),
        "tap:c".into(),
        format!("wait:{}", file("dragging")),
        "move:6,6".into(),
        "move:6,6".into(),
        "release:left".into(),
        format!("touch:{}", file("released")),
        format!("wait:{}", file("touchable")),
        "finger-down:100,100".into(),
        "finger-move:120,130".into(),
        "finger-up:".into(),
        format!("wait:{}", file("done")),
    ];
    let mut child = aterm_uds::spawnfd::command(&lane.bus)
        .args(["seat", &lane.mutter, "-", "0"])
        .args(&steps)
        .spawn()
        .expect("lane-bus seat starts");
    struct Reap<'c>(&'c mut aterm_uds::spawnfd::Child);
    impl Drop for Reap<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    let reap = Reap(&mut child);

    let mut log: Vec<Input> = Vec::new();
    pump(&a.queue, "the keyboard's focus", || {
        log.extend(ears.input());
        seats.keyboard_focus(seat) == Some(surface)
    });
    let keymap = log.iter().find_map(|i| match i {
        Input::Keyboard {
            event: KeyboardEvent::Keymap(k),
            ..
        } => Some(k),
        _ => None,
    });
    let keymap = keymap
        .expect("a keymap")
        .as_ref()
        .expect("a readable keymap");
    assert!(
        keymap.text.starts_with(b"xkb_keymap"),
        "{:?}",
        String::from_utf8_lossy(&keymap.text[..keymap.text.len().min(40)])
    );
    let repeat = log.iter().find_map(|i| match i {
        Input::Keyboard {
            event: KeyboardEvent::RepeatInfo { rate, delay },
            ..
        } => Some((*rate, *delay)),
        _ => None,
    });
    eprintln!(
        "keymap {} bytes, repeat {repeat:?}, caps {:?}",
        keymap.text.len(),
        seats.infos()
    );
    assert!(
        repeat.is_some(),
        "a version-4 keyboard hears its repeat settings"
    );

    touch(pace.path(), "focused");
    let keys = |log: &[Input]| -> Vec<(u32, u32)> {
        log.iter()
            .filter_map(|i| match i {
                Input::Keyboard {
                    event: KeyboardEvent::Key { key, state, .. },
                    ..
                } if [KEY_A, KEY_B, KEY_C].contains(key) => Some((*key, *state)),
                _ => None,
            })
            .collect()
    };
    let press = |log: &[Input]| -> Option<u32> {
        log.iter().find_map(|i| match i {
            Input::Pointer { frame, .. } => frame.iter().find_map(|e| match e.kind {
                PointerKind::Press { button, serial, .. }
                    if button == BTN_LEFT && e.surface == surface =>
                {
                    Some(serial)
                }
                _ => None,
            }),
            _ => None,
        })
    };
    let entered = |log: &[Input]| {
        log.iter().any(|i| {
            matches!(i, Input::Pointer { frame, .. }
                if frame.iter().any(|e| matches!(e.kind, PointerKind::Enter { .. }) && e.surface == surface))
        })
    };
    pump(&a.queue, "the keys and the pointer", || {
        log.extend(ears.input());
        keys(&log).len() == 4 && seats.pointer(seat).is_some()
    });
    touch(pace.path(), "bound");
    pump(&a.queue, "the pointer's enter", || {
        log.extend(ears.input());
        entered(&log)
    });
    touch(pace.path(), "pointed");
    pump(&a.queue, "the press", || {
        log.extend(ears.input());
        press(&log).is_some()
    });
    assert_eq!(
        keys(&log)[..4],
        [(KEY_A, 1), (KEY_A, 0), (KEY_B, 1), (KEY_B, 0)],
        "a then b, each pressed then released"
    );
    let press_serial = press(&log).unwrap();
    assert!(seats.pointer_button_serial(seat) == Some(press_serial));
    let lent = seats.with_seat(seat, |r| (r.seat, r.pointer, r.pointer_button_serial));
    assert_eq!(
        lent,
        Some((seat_proxy, seats.pointer(seat), Some(press_serial)))
    );

    let serial = seats.latest_serial(seat).expect("the seat gave a serial");
    let first = Source::new(&clipboard, &[TEXT], 0, ears.source("first")).unwrap();
    device.set_selection(Some(&first), serial);
    pump(&a.queue, "the first selection", || {
        device
            .selection()
            .is_some_and(|o| o.mime_types.iter().any(|m| m.as_bytes() == TEXT.to_bytes()))
    });
    let mut heard = Vec::new();
    let got = read_through(
        &a.queue,
        device.receive_selection(TEXT),
        (&ears, "first"),
        b"LANE-SEL-1",
        &mut heard,
    );
    assert_eq!(got, b"LANE-SEL-1");
    let stale = Source::new(&clipboard, &[TEXT], 0, ears.source("stale")).unwrap();
    device.set_selection(Some(&stale), serial);
    a.queue.roundtrip().unwrap();
    a.queue.roundtrip().unwrap();
    assert!(!first.cancelled(), "a stale serial replaces nothing");
    let got = read_through(
        &a.queue,
        device.receive_selection(TEXT),
        (&ears, "first"),
        b"LANE-SEL-1",
        &mut heard,
    );
    assert_eq!(got, b"LANE-SEL-1", "the first selection stands");
    drop(stale);
    drop(ears.requests("stale"));
    touch(pace.path(), "fresh");
    pump(&a.queue, "the key c", || {
        log.extend(ears.input());
        keys(&log).len() == 6
    });
    assert_eq!(keys(&log)[4..], [(KEY_C, 1), (KEY_C, 0)]);
    let newer = seats.latest_serial(seat).unwrap();
    assert!(
        newer.wrapping_sub(serial) < u32::MAX / 2 && newer != serial,
        "{serial} then {newer}"
    );
    let second = Source::new(&clipboard, &[TEXT], 0, ears.source("second")).unwrap();
    device.set_selection(Some(&second), newer);
    pump(&a.queue, "the first source cancelled", || first.cancelled());
    let got = read_through(
        &a.queue,
        device.receive_selection(TEXT),
        (&ears, "second"),
        b"LANE-SEL-2",
        &mut heard,
    );
    assert_eq!(
        got, b"LANE-SEL-2",
        "the second source's bytes, not the first's"
    );

    {
        let b = Client::connect(&lane);
        let b_ears = Ears::default();
        let b_seats = Seats::bind_all(&b.registry, b_ears.seats()).unwrap();
        let b_clipboard = Manager::bind(&b.registry, Kind::Clipboard).unwrap();
        b.queue.roundtrip().unwrap();
        let b_seat = b_seats.infos().first().unwrap().id;
        // SAFETY: B's live seat, outliving the device.
        let b_device =
            unsafe { Device::new(&b_clipboard, b_seats.proxy(b_seat).unwrap(), b_ears.drags()) }
                .unwrap();
        let intruder = Source::new(&b_clipboard, &[TEXT], 0, b_ears.source("intruder")).unwrap();
        b_device.set_selection(Some(&intruder), newer);
        pump(&b.queue, "the unfocused client's source cancelled", || {
            intruder.cancelled()
        });
        drop((intruder, b_device, b_clipboard, b_seats));
        b.queue.roundtrip().unwrap();
        assert_eq!(unsafe { b.display.display() }.error(), None);
    }
    a.queue.roundtrip().unwrap();
    assert!(!second.cancelled(), "the focused client's selection stands");
    let got = read_through(
        &a.queue,
        device.receive_selection(TEXT),
        (&ears, "second"),
        b"LANE-SEL-2",
        &mut heard,
    );
    assert_eq!(got, b"LANE-SEL-2");

    let prim = Source::new(&primary, &[TEXT], 0, ears.source("prim")).unwrap();
    primary_device.set_selection(Some(&prim), newer);
    pump(&a.queue, "the primary selection", || {
        primary_device
            .selection()
            .is_some_and(|o| o.mime_types.iter().any(|m| m.as_bytes() == TEXT.to_bytes()))
    });
    let got = read_through(
        &a.queue,
        primary_device.receive_selection(TEXT),
        (&ears, "prim"),
        b"LANE-PRIMARY",
        &mut heard,
    );
    assert_eq!(got, b"LANE-PRIMARY");

    let copy = wl_data_device_manager::dnd_action::COPY;
    let dragged = Source::new(&clipboard, &[URI_LIST], copy, ears.source("dragged")).unwrap();
    // SAFETY: The target surface is a live Wayland surface.
    assert!(unsafe { device.start_drag(Some(&dragged), surface, None, press_serial) });
    let mut drag_log = Vec::new();
    pump(&a.queue, "the drag's enter", || {
        drag_log.extend(ears.drag_events());
        drag_log
            .iter()
            .any(|e| matches!(e, DragEvent::Enter { .. }))
    });
    let Some(DragEvent::Enter {
        surface: on,
        offer: Some(offer),
        ..
    }) = drag_log
        .iter()
        .find(|e| matches!(e, DragEvent::Enter { .. }))
        .cloned()
    else {
        panic!("an enter carrying an offer: {drag_log:?}");
    };
    assert_eq!(on, Some(surface));
    assert!(
        offer
            .mime_types
            .iter()
            .any(|m| m.as_bytes() == URI_LIST.to_bytes()),
        "{offer:?}"
    );
    assert_eq!(offer.source_actions & copy, copy, "{offer:?}");
    assert!(device.accept_drag(Some(URI_LIST), copy, copy));
    touch(pace.path(), "dragging");
    pump(&a.queue, "the drop", || {
        drag_log.extend(ears.drag_events());
        drag_log.iter().any(|e| matches!(e, DragEvent::Drop))
    });
    eprintln!("drag: {drag_log:?}");
    assert!(
        drag_log
            .iter()
            .any(|e| matches!(e, DragEvent::Motion { .. })),
        "the drag moved over the surface"
    );
    let dropped = device.take_dropped().expect("the drop carried an offer");
    assert!(device.take_dropped().is_none(), "a drop is taken over once");
    let got = read_through(
        &a.queue,
        dropped.receive(URI_LIST),
        (&ears, "dragged"),
        b"file:///tmp/aterm-lane-dnd/dropped%20file.txt\r\n",
        &mut heard,
    );
    assert_eq!(got, b"file:///tmp/aterm-lane-dnd/dropped%20file.txt\r\n");
    dropped.finish();
    pump(&a.queue, "the source hears the drop finished", || {
        heard.extend(serve(&ears, "dragged", b""));
        heard.iter().any(|e| e == "Finished")
    });
    eprintln!("drag source heard: {heard:?}");
    assert!(heard.iter().any(|e| e == "DropPerformed"), "{heard:?}");

    pump(&a.queue, "the seat's touch", || {
        log.extend(ears.input());
        seats
            .infos()
            .iter()
            .any(|s| s.id == seat && s.capabilities & CAP_TOUCH != 0)
    });
    touch(pace.path(), "touchable");
    let touches = |log: &[Input]| -> Vec<TouchEvent> {
        log.iter()
            .filter_map(|i| match i {
                Input::Touch { event, .. } => Some(*event),
                _ => None,
            })
            .collect()
    };
    pump(&a.queue, "the finger's up", || {
        log.extend(ears.input());
        touches(&log)
            .iter()
            .any(|e| matches!(e, TouchEvent::Up { .. }))
    });
    a.queue.roundtrip().unwrap();
    log.extend(ears.input());
    let finger = touches(&log);
    eprintln!("touch: {finger:?}");
    let [
        TouchEvent::Down {
            surface: on,
            id,
            x: x0,
            y: y0,
            ..
        },
        TouchEvent::Frame,
        rest @ ..,
    ] = finger.as_slice()
    else {
        panic!("a down, in a frame of its own, first: {finger:?}");
    };
    assert_eq!(*on, Some(surface), "the finger went down on the surface");
    let moved: Vec<(f64, f64)> = rest
        .iter()
        .filter_map(|e| match e {
            TouchEvent::Motion { id: m, x, y, .. } if m == id => Some((*x, *y)),
            _ => None,
        })
        .collect();
    let (x1, y1) = *moved.last().expect("a motion of the same finger");
    assert!(x1 > *x0 && y1 > *y0, "({x0}, {y0}) then ({x1}, {y1})");
    assert!(
        rest.iter()
            .any(|e| matches!(e, TouchEvent::Up { id: u, .. } if u == id)),
        "the same finger lifted: {finger:?}"
    );
    assert!(
        !rest.iter().any(|e| matches!(e, TouchEvent::Cancel)),
        "{finger:?}"
    );

    touch(pace.path(), "done");
    let t0 = Instant::now();
    let status = loop {
        if let Some(status) = reap.0.try_wait().unwrap() {
            break status;
        }
        assert!(t0.elapsed() < HANG, "lane-bus seat did not end");
        a.queue.roundtrip().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "lane-bus seat: {status:?}");
    pump(&a.queue, "the seat without devices", || {
        log.extend(ears.input());
        seats.infos().iter().all(|s| s.capabilities == 0)
    });
    assert!(seats.keyboard_focus(seat).is_none());
    assert!(seats.pointer(seat).is_none());
    drop(reap);

    drop((dragged, prim, second, first));
    drop((device, primary_device, clipboard, primary));
    drop(toplevel);
    // SAFETY: This test's objects, each destroyed once.
    unsafe {
        wl_surface::destroy(surface);
        aterm_wayland::protocols::wayland::wl_buffer::destroy(buffer);
        wl_shm_pool::destroy(pool);
        shm.destroy();
        compositor.destroy();
    }
    drop(seats);
    a.queue.roundtrip().unwrap();
    assert_eq!(
        unsafe { a.display.display() }.error(),
        None,
        "every release protocol-clean"
    );
}
