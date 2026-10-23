// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! LW.7's capability-removal race, live (d5f7317b01): a window's pointer and text input are
//! destroyed on the loop's thread even when another thread holds them as their capability goes.

use std::ffi::CString;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Weak};
use std::time::{Duration, Instant};

use aterm_wayland::protocols::wayland::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};

use super::{TextInput, WinitPointer, RETIRE};
use crate::platform_impl::wayland::event_loop::EventLoop;
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::window::Window;
use crate::platform_impl::ActiveEventLoop as PlatformActiveEventLoop;
use crate::window::WindowAttributes;

/// A wait for what must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

struct Lane {
    display: CString,
    mutter: String,
    bus: PathBuf,
}

fn lane() -> Option<Lane> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match (var("LANE_SEAT_WAYLAND_DISPLAY"), var("LANE_SEAT_MUTTER_PID"), var("LANE_SEAT_BUS")) {
        (Some(display), Some(mutter), Some(bus)) => Some(Lane {
            display: CString::new(display).expect("a display name"),
            mutter,
            bus: PathBuf::from(bus),
        }),
        _ => {
            eprintln!("SKIP: no Mutter with a RemoteDesktop seat named (LANE_SEAT_*)");
            None
        },
    }
}

/// Read the loop's state.
fn with_state<R>(event_loop: &EventLoop<()>, read: impl FnOnce(&WinitState) -> R) -> R {
    #[allow(irrefutable_let_patterns)]
    let PlatformActiveEventLoop::Wayland(target) = &event_loop.window_target().p
    else {
        panic!("a Wayland loop");
    };
    read(&target.state.borrow())
}

/// Turn the loop until `done` holds, within the hang detector.
fn pump(event_loop: &mut EventLoop<()>, what: &str, mut done: impl FnMut(&EventLoop<()>) -> bool) {
    let t0 = Instant::now();
    loop {
        let _ = event_loop.pump_events(Some(Duration::from_millis(10)), |_, _| {});
        if done(event_loop) {
            eprintln!("{what}: {:.2} s", t0.elapsed().as_secs_f64());
            return;
        }
        assert!(t0.elapsed() < HANG, "{what}: not within {} s", HANG.as_secs());
    }
}

fn touch(dir: &Path, name: &str) {
    std::fs::File::create(dir.join(name)).expect("a pacing file");
}

fn memfd(len: usize) -> OwnedFd {
    // SAFETY: a fresh memfd, sized for this test.
    unsafe {
        let fd = libc::memfd_create(c"winit-retire".as_ptr(), libc::MFD_CLOEXEC);
        assert!(fd >= 0);
        assert_eq!(libc::ftruncate(fd, len as libc::off_t), 0);
        OwnedFd::from_raw_fd(fd)
    }
}

/// What the holder's references came to: how many strong references each object had left once
/// the holder let go (the loop's, or none), and whether the loop's next turns then destroyed
/// them.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    pointer_after_holder: usize,
    text_input_after_holder: usize,
    reaped_by_the_loop: bool,
}

/// One arm: a maximized window with the seat's keyboard and pointer on it, a second thread
/// holding its pointer and text input while the seat's session ends.
fn arm(lane: &Lane, retire: bool) -> Outcome {
    RETIRE.store(retire, Ordering::SeqCst);
    let mut event_loop =
        EventLoop::<()>::on_display(Some(&lane.display)).expect("Mutter's display");
    let window = {
        #[allow(irrefutable_let_patterns)]
        let PlatformActiveEventLoop::Wayland(target) = &event_loop.window_target().p
        else {
            panic!("a Wayland loop");
        };
        let window = Window::new(target, WindowAttributes::default().with_title("winit retire"))
            .expect("a window");
        window.set_maximized(true);
        window
    };
    pump(&mut event_loop, "the maximized configure", |_| window.is_maximized());

    let surface = window.wl_surface();
    let size = window.inner_size();
    let (w, h) = (i32::try_from(size.width).unwrap(), i32::try_from(size.height).unwrap());
    let len = usize::try_from(w * h * 4).unwrap();
    let fd = memfd(len);
    let shm = with_state(&event_loop, WinitState::shm);
    // SAFETY: the loop's live wl_shm and the window's live surface; the pool and buffer are
    // destroyed once each below, after the window.
    let (pool, buffer) = unsafe {
        let pool = wl_shm::create_pool(shm.proxy(), fd.as_raw_fd(), i32::try_from(len).unwrap())
            .expect("a pool");
        let buffer = wl_shm_pool::create_buffer(pool, 0, w, h, w * 4, wl_shm::format::XRGB8888)
            .expect("a buffer");
        wl_surface::attach(surface, Some(buffer), 0, 0);
        wl_surface::damage_buffer(surface, 0, 0, w, h);
        wl_surface::commit(surface);
        (pool, buffer)
    };
    drop(shm);
    pump(&mut event_loop, "the mapped window", |_| true);

    let pace = aterm_tempfile::tempdir().unwrap();
    let file = |n: &str| pace.path().join(n).display().to_string();
    let steps = [
        "move:3,3".to_string(),
        format!("wait:{}", file("bound")),
        "move:3,3".to_string(),
        format!("wait:{}", file("held")),
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

    let shared = window.shared_state();
    pump(&mut event_loop, "the text input on the window", |_| {
        shared.lock().unwrap().text_inputs().iter().any(|t| t.strong_count() > 0)
    });
    pump(&mut event_loop, "the seat's pointer", |el| {
        with_state(el, |s| s.seats.values().any(|seat| seat.pointer.is_some()))
    });
    touch(pace.path(), "bound");
    pump(&mut event_loop, "the pointer on the window", |_| {
        shared.lock().unwrap().pointers.iter().any(|p| p.strong_count() > 0)
    });

    let (to_holder, told) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel::<(Weak<WinitPointer>, Weak<TextInput>)>();
    let (dropped, let_go) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let objects = {
            let state = shared.lock().unwrap();
            let pointer = state.pointers.iter().find_map(Weak::upgrade).expect("a live pointer");
            let text_input =
                state.text_inputs().iter().find_map(Weak::upgrade).expect("a live text input");
            (pointer, text_input)
        };
        drop(shared);
        holding.send((Arc::downgrade(&objects.0), Arc::downgrade(&objects.1))).unwrap();
        told.recv_timeout(HANG).expect("the loop's word within the hang detector");
        drop(objects);
        dropped.send(()).unwrap();
    });
    let (pointer, text_input) = held.recv_timeout(HANG).expect("the holder's upgrades");

    touch(pace.path(), "held");
    pump(&mut event_loop, "the seat without its devices", |el| {
        with_state(el, |s| {
            s.seats.values().all(|seat| {
                seat.capabilities == 0 && seat.pointer.is_none() && seat.text_input.is_none()
            })
        })
    });
    let t0 = Instant::now();
    let status = loop {
        if let Some(status) = reap.0.try_wait().unwrap() {
            break status;
        }
        assert!(t0.elapsed() < HANG, "lane-bus seat did not end");
        let _ = event_loop.pump_events(Some(Duration::from_millis(10)), |_, _| {});
    };
    assert!(status.success(), "lane-bus seat: {status:?}");
    drop(reap);

    to_holder.send(()).unwrap();
    let_go.recv_timeout(HANG).expect("the holder let go");
    holder.join().unwrap();
    let pointer_after_holder = pointer.strong_count();
    let text_input_after_holder = text_input.strong_count();
    let reaped_by_the_loop = pointer_after_holder > 0 && text_input_after_holder > 0 && {
        pump(&mut event_loop, "the loop's reap", |_| {
            pointer.strong_count() == 0 && text_input.strong_count() == 0
        });
        true
    };

    drop(window);
    pump(&mut event_loop, "the window gone", |el| {
        with_state(el, |s| s.windows.borrow().is_empty())
    });
    // SAFETY: this arm's pool and buffer, each destroyed once; the surface went with the window.
    unsafe {
        wl_buffer::destroy(buffer);
        wl_shm_pool::destroy(pool);
    }
    pump(&mut event_loop, "the last requests", |_| true);
    drop(event_loop);
    Outcome { pointer_after_holder, text_input_after_holder, reaped_by_the_loop }
}

#[test]
fn a_held_pointer_and_text_input_die_on_the_loop_s_thread() {
    let Some(lane) = lane() else {
        return;
    };
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            RETIRE.store(true, Ordering::SeqCst);
        }
    }
    let _restore = Restore;
    let fixed = arm(&lane, true);
    eprintln!("retired: {fixed:?}");
    assert_eq!(
        fixed,
        Outcome { pointer_after_holder: 1, text_input_after_holder: 1, reaped_by_the_loop: true },
        "the loop's reference outlived the holder's, and the loop destroyed both"
    );
    let unfixed = arm(&lane, false);
    eprintln!("not retired: {unfixed:?}");
    assert_eq!(
        unfixed,
        Outcome { pointer_after_holder: 0, text_input_after_holder: 0, reaped_by_the_loop: false },
        "without the retire, the holder's thread destroyed both"
    );
}
