// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live Wayland runtime tests against a real compositor (`$WAYLAND_DISPLAY`).

#![cfg(target_os = "linux")]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use aterm_wayland::globals::Registry;
use aterm_wayland::protocols::wayland::{
    wl_buffer, wl_compositor, wl_shm, wl_shm_pool, wl_surface,
};
use aterm_wayland::proxy::{EventArg, OwnedDisplay, Queue};

fn connect() -> Option<OwnedDisplay> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("SKIP: no WAYLAND_DISPLAY");
        return None;
    }
    Some(OwnedDisplay::connect(None).expect("WAYLAND_DISPLAY is set: the display must open"))
}

fn memfd(len: usize) -> OwnedFd {
    // SAFETY: a fresh memfd, sized for this test.
    unsafe {
        let fd = libc::memfd_create(c"aterm-wayland-live".as_ptr(), libc::MFD_CLOEXEC);
        assert!(fd >= 0);
        assert_eq!(libc::ftruncate(fd, len as libc::off_t), 0);
        OwnedFd::from_raw_fd(fd)
    }
}

#[test]
fn the_registry_lists_and_shm_announces_its_formats() {
    let Some(display) = connect() else {
        return;
    };
    // SAFETY: this owner outlives the local queue and objects; dispatch and teardown are serial.
    let queue = Queue::new(unsafe { display.display() }).unwrap();
    let registry = Registry::new(&queue).unwrap();
    let names: Vec<_> = registry
        .globals()
        .iter()
        .map(|g| g.interface.to_string_lossy().into_owned())
        .collect();
    for want in ["wl_compositor", "wl_shm"] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    let global = registry.find(c"wl_shm").unwrap();
    let shm = registry.bind(&global, &wl_shm::INTERFACE, 1).unwrap();
    let formats = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&formats);
    // SAFETY: the wl_shm just bound; the listener outlives it (dropped last).
    let listener = unsafe {
        shm.listen(move |_, opcode, args| {
            if let (wl_shm::evt::FORMAT, [EventArg::Uint(f)]) = (opcode, args) {
                sink.lock().unwrap().push(*f);
            }
        })
    }
    .unwrap();
    queue.roundtrip().unwrap();
    let got = formats.lock().unwrap().clone();
    assert!(
        got.contains(&wl_shm::format::XRGB8888) && got.contains(&wl_shm::format::ARGB8888),
        "every compositor offers both: {got:?}"
    );
    // SAFETY: the bound wl_shm, destroyed once; its listener after.
    unsafe { shm.destroy() };
    drop(listener);
    assert_eq!(unsafe { display.display() }.error(), None);
}

#[test]
fn a_buffer_reaches_a_surface_and_a_refusal_is_seen() {
    let Some(display) = connect() else {
        return;
    };
    // SAFETY: this owner outlives the local queue and objects; dispatch and teardown are serial.
    let queue = Queue::new(unsafe { display.display() }).unwrap();
    let registry = Registry::new(&queue).unwrap();
    let compositor = registry
        .bind(
            &registry.find(c"wl_compositor").unwrap(),
            &wl_compositor::INTERFACE,
            4,
        )
        .unwrap();
    let shm = registry
        .bind(&registry.find(c"wl_shm").unwrap(), &wl_shm::INTERFACE, 1)
        .unwrap();
    let fd = memfd(4 * 4 * 4);
    // SAFETY: live objects of this test, each destroyed once below.
    unsafe {
        let surface = wl_compositor::create_surface(compositor).unwrap();
        let pool = wl_shm::create_pool(shm, fd.as_raw_fd(), 64).unwrap();
        let buffer =
            wl_shm_pool::create_buffer(pool, 0, 4, 4, 16, wl_shm::format::XRGB8888).unwrap();
        wl_surface::attach(surface, Some(buffer), 0, 0);
        wl_surface::damage_buffer(surface, 0, 0, 4, 4);
        wl_surface::commit(surface);
        queue.roundtrip().unwrap();
        assert_eq!(display.display().error(), None, "a well-formed buffer");
        wl_surface::destroy(surface);
        wl_buffer::destroy(buffer);
        wl_shm_pool::destroy(pool);
        queue.roundtrip().unwrap();
        assert_eq!(display.display().error(), None);

        let pool = wl_shm::create_pool(shm, fd.as_raw_fd(), 64).unwrap();
        let _ = wl_shm_pool::create_buffer(pool, 0, 0, 4, 16, wl_shm::format::XRGB8888);
        assert!(
            queue.roundtrip().is_err(),
            "the refusal fails the round trip"
        );
        assert!(
            display.display().error().is_some(),
            "and is the display's error"
        );
    }
}

#[test]
fn a_theme_hands_out_its_default_frame() {
    use aterm_wayland::cursor::Theme;
    let Some(display) = connect() else {
        return;
    };
    if aterm_wayland::cursor::cursor_lib().is_err() {
        eprintln!("SKIP: no libwayland-cursor");
        return;
    }
    // SAFETY: this owner outlives the local queue and objects; dispatch and teardown are serial.
    let queue = Queue::new(unsafe { display.display() }).unwrap();
    let registry = Registry::new(&queue).unwrap();
    let shm = registry
        .bind(&registry.find(c"wl_shm").unwrap(), &wl_shm::INTERFACE, 1)
        .unwrap();
    // SAFETY: the wl_shm just bound outlives the theme (dropped first).
    let theme = unsafe { Theme::load(None, 24, shm) }.expect("a theme or the built-in cursors");
    let frame = theme
        .frame_named(&[c"default", c"left_ptr"])
        .expect("a default cursor");
    assert!(frame.width > 0 && frame.height > 0);
    assert!(frame.hotspot_x <= frame.width && frame.hotspot_y <= frame.height);
    queue.roundtrip().unwrap();
    assert_eq!(unsafe { display.display() }.error(), None);
    drop(theme);
    // SAFETY: the bound wl_shm, destroyed once, after the theme.
    unsafe { shm.destroy() };
}

#[test]
fn every_output_reports_a_current_mode() {
    use aterm_wayland::output::Outputs;
    let Some(display) = connect() else {
        return;
    };
    // SAFETY: this owner outlives the local queue and objects; dispatch and teardown are serial.
    let queue = Queue::new(unsafe { display.display() }).unwrap();
    let registry = Registry::new(&queue).unwrap();
    let dones = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let heard = std::sync::Arc::clone(&dones);
    let mut outputs = Outputs::bind_all(
        &registry,
        std::sync::Arc::new(move |info| heard.lock().unwrap().push(info)),
    )
    .unwrap();
    queue.roundtrip().unwrap();
    let infos = outputs.infos();
    assert!(!infos.is_empty(), "a compositor has an output");
    let dones = std::mem::take(&mut *dones.lock().unwrap());
    for info in &infos {
        assert!(dones.contains(info), "{info:?} not in {dones:?}");
    }
    let generation = registry.generation();
    assert!(
        !outputs.sync(&registry).unwrap(),
        "a sync with no change changed something"
    );
    assert_eq!(registry.generation(), generation);
    for info in &infos {
        let mode = info.current_mode().expect("a current mode");
        assert!(mode.width > 0 && mode.height > 0, "{info:?}");
        assert!(info.scale >= 1, "{info:?}");
        let proxy = outputs.proxy(info.global).expect("bound");
        let handle = outputs.by_proxy(proxy).expect("a handle by proxy");
        assert_eq!(handle.global(), info.global);
        assert_eq!(handle.info().as_ref(), Some(info));
        eprintln!(
            "output {:?}: {}x{} @{} mHz, scale {}",
            info.name, mode.width, mode.height, mode.refresh, info.scale
        );
    }
    drop(outputs);
    queue.roundtrip().unwrap();
    assert_eq!(unsafe { display.display() }.error(), None);
}

fn map_toplevel(
    display: &OwnedDisplay,
    second_role: bool,
) -> (Option<aterm_wayland::xdg::Configure>, bool) {
    use aterm_wayland::xdg::{Shell, Toplevel};
    // SAFETY: this owner outlives the local queue and objects; dispatch and teardown are serial.
    let queue = Queue::new(unsafe { display.display() }).unwrap();
    let registry = Registry::new(&queue).unwrap();
    let compositor = registry
        .bind(
            &registry.find(c"wl_compositor").unwrap(),
            &wl_compositor::INTERFACE,
            4,
        )
        .unwrap();
    let shm = registry
        .bind(&registry.find(c"wl_shm").unwrap(), &wl_shm::INTERFACE, 1)
        .unwrap();
    let shell = Shell::bind(&registry).unwrap();
    // SAFETY: objects of this test on one display, each destroyed once below or left to the
    // display's end.
    unsafe {
        let surface = wl_compositor::create_surface(compositor).unwrap();
        let toplevel = Toplevel::new(&shell, surface, c"aterm live", c"aterm.live").unwrap();
        if second_role {
            let _again = Toplevel::new(&shell, surface, c"again", c"again").unwrap();
            let dead = queue.roundtrip().is_err() || display.display().error().is_some();
            return (None, !dead);
        }
        wl_surface::commit(surface);
        queue.roundtrip().unwrap();
        let configure = toplevel.take_configure().expect("the first configure");
        toplevel.ack(configure.serial);
        let first = Some(configure);
        let (w, h) = first
            .as_ref()
            .map(|c| (c.width, c.height))
            .filter(|&(w, h)| w > 0 && h > 0)
            .unwrap_or((120, 80));
        let len = usize::try_from(w * h * 4).unwrap();
        let fd = memfd(len);
        let pool = wl_shm::create_pool(shm, fd.as_raw_fd(), i32::try_from(len).unwrap()).unwrap();
        let buffer =
            wl_shm_pool::create_buffer(pool, 0, w, h, w * 4, wl_shm::format::XRGB8888).unwrap();
        wl_surface::attach(surface, Some(buffer), 0, 0);
        wl_surface::damage_buffer(surface, 0, 0, w, h);
        wl_surface::commit(surface);
        let survived = queue.roundtrip().is_ok() && display.display().error().is_none();
        if survived {
            assert!(!toplevel.close_requested());
            drop(toplevel);
            wl_surface::destroy(surface);
            wl_buffer::destroy(buffer);
            wl_shm_pool::destroy(pool);
            queue.roundtrip().unwrap();
            assert_eq!(display.display().error(), None);
        }
        (first, survived)
    }
}

#[test]
fn a_toplevel_is_configured_acked_and_mapped() {
    let Some(display) = connect() else {
        return;
    };
    let (configure, survived) = map_toplevel(&display, false);
    let configure = configure.unwrap();
    assert!(
        survived,
        "an acked toplevel with a buffer is protocol-clean"
    );
    assert!(
        configure.serial != 0 || configure.width >= 0,
        "{configure:?}"
    );
    eprintln!("first configure: {configure:?}");
    let other = OwnedDisplay::connect(None).unwrap();
    let (_, survived) = map_toplevel(&other, true);
    assert!(!survived, "a second role is a protocol error");
}

#[test]
fn decorations_are_negotiated_or_the_client_draws() {
    use aterm_wayland::xdg::{Decorations, Shell, Toplevel};
    let Some(display) = connect() else {
        return;
    };
    // SAFETY: this owner outlives the local queue and objects; dispatch and teardown are serial.
    let queue = Queue::new(unsafe { display.display() }).unwrap();
    let registry = Registry::new(&queue).unwrap();
    let compositor = registry
        .bind(
            &registry.find(c"wl_compositor").unwrap(),
            &wl_compositor::INTERFACE,
            4,
        )
        .unwrap();
    let shell = Shell::bind(&registry).unwrap();
    // SAFETY: as in map_toplevel.
    unsafe {
        let surface = wl_compositor::create_surface(compositor).unwrap();
        let toplevel = Toplevel::new(&shell, surface, c"aterm", c"aterm").unwrap();
        wl_surface::commit(surface);
        queue.roundtrip().unwrap();
        let mode = toplevel.decorations();
        eprintln!("decorations: {mode:?}");
        if shell.has_decorations() {
            assert!(
                matches!(mode, Decorations::Client | Decorations::Server),
                "{mode:?}"
            );
        } else {
            assert_eq!(mode, Decorations::ClientUnoffered);
        }
        drop(toplevel);
        wl_surface::destroy(surface);
    }
    queue.roundtrip().unwrap();
    assert_eq!(unsafe { display.display() }.error(), None);
}

#[test]
fn a_poll_loop_reads_the_socket_and_dispatches_and_a_full_queue_refuses_the_read() {
    use aterm_wayland::protocols::wayland::{wl_callback, wl_display};

    let Some(display) = connect() else {
        return;
    };
    // SAFETY: this local owner outlives the queue and every derived handle; dispatch and teardown
    // of this private queue are serialized on this thread.
    let queue = Queue::new(unsafe { display.display() }).unwrap();
    let done = Arc::new(Mutex::new(0u32));
    let wrapper = queue.display_wrapper().unwrap();
    // SAFETY: the wrapper is live; the callback is on `queue`, destroyed below after its one event,
    // its listener after it.
    let callback = unsafe { wl_display::sync(wrapper.proxy()) }.unwrap();
    drop(wrapper);
    let sink = Arc::clone(&done);
    // SAFETY: the callback made above, without a listener.
    let listener = unsafe {
        callback.listen(move |_, opcode, _| {
            if opcode == wl_callback::evt::DONE {
                *sink.lock().unwrap() += 1;
            }
        })
    }
    .unwrap();
    // SAFETY: the same local owner remains live; dispatch and teardown stay on this thread.
    unsafe { display.display() }.flush().unwrap();
    // SAFETY: the same live local owner and serialized queue lifetime.
    let fd = unsafe { display.display() }.fd().unwrap();
    let t0 = std::time::Instant::now();
    while *done.lock().unwrap() == 0 {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "the sync never answered"
        );
        let Some(intent) = queue.prepare_read().unwrap() else {
            queue.dispatch_pending().unwrap();
            continue;
        };
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, 60_000) };
        assert!(rc >= 0, "poll: {}", std::io::Error::last_os_error());
        if pfd.revents != 0 {
            intent.read().unwrap();
        } else {
            drop(intent);
        }
        queue.dispatch_pending().unwrap();
    }
    assert_eq!(*done.lock().unwrap(), 1);

    let wrapper = queue.display_wrapper().unwrap();
    // SAFETY: as above.
    let second = unsafe { wl_display::sync(wrapper.proxy()) }.unwrap();
    drop(wrapper);
    let sink = Arc::clone(&done);
    // SAFETY: as above.
    let second_listener = unsafe {
        second.listen(move |_, opcode, _| {
            if opcode == wl_callback::evt::DONE {
                *sink.lock().unwrap() += 1;
            }
        })
    }
    .unwrap();
    // SAFETY: the same local owner remains live; dispatch and teardown stay on this thread.
    unsafe { display.display() }.flush().unwrap();
    let t0 = std::time::Instant::now();
    loop {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "the second sync never came"
        );
        let intent = queue
            .prepare_read()
            .unwrap()
            .expect("nothing is queued yet");
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live pollfd.
        unsafe { libc::poll(&mut pfd, 1, 60_000) };
        intent.read().unwrap();
        if queue.prepare_read().unwrap().is_none() {
            break;
        }
    }
    assert_eq!(*done.lock().unwrap(), 1, "read, not yet dispatched");
    queue.dispatch_pending().unwrap();
    assert_eq!(*done.lock().unwrap(), 2);
    // SAFETY: both callbacks ended with their event; destroyed once, listeners after.
    unsafe {
        callback.destroy();
        second.destroy();
    }
    drop((listener, second_listener));
    // SAFETY: both callbacks and listeners have ended; the owner still outlives the queue.
    assert_eq!(unsafe { display.display() }.error(), None);
}
