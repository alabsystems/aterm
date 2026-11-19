// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use aterm_wayland::protocols::wayland::{wl_compositor, wl_registry};
use aterm_wayland::proxy::OwnedDisplay;

/// The private peer implements the requests needed for a surface and a processing
/// callback. No compositor, installed app, window or external process participates.
struct Connection {
    display: OwnedDisplay,
    registry: Proxy,
    compositor: Proxy,
    surface: Proxy,
}

impl Connection {
    fn new(socket: UnixStream) -> Arc<Self> {
        let display = OwnedDisplay::connect_to_fd(socket.into()).expect("the private display");
        // SAFETY: the owned display is moved beside these proxies. Drop destroys all
        // three before disconnecting it; their event queues are never dispatched here.
        let (registry, compositor, surface) = unsafe {
            let registry = wl_display::get_registry(display.display().proxy()).unwrap();
            let compositor = wl_registry::bind(registry, 1, &wl_compositor::INTERFACE, 4).unwrap();
            let surface = wl_compositor::create_surface(compositor).unwrap();
            (registry, compositor, surface)
        };
        Arc::new(Self {
            display,
            registry,
            compositor,
            surface,
        })
    }

    fn submit(self: &Arc<Self>) -> SubmitSync<Arc<Self>> {
        // SAFETY: this connection owns the live surface and display; the returned fence
        // retains it. The sync is marshalled after this exact surface commit.
        unsafe {
            wl_surface::commit(self.surface);
            SubmitSync::after_submit(self.display.display(), Arc::clone(self)).unwrap()
        }
    }

    fn display(&self) -> Display {
        // SAFETY: every caller borrows this owner or retains it through a fence.
        unsafe { self.display.display() }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: no listeners belong to these proxies, and all fences retaining this
        // owner have destroyed their own callbacks and private queues first.
        unsafe {
            wl_surface::destroy(self.surface);
            self.compositor.destroy();
            self.registry.destroy();
        }
    }
}

fn sockets() -> (UnixStream, UnixStream) {
    let (near, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    far.set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    (near, far)
}

fn request(peer: &mut UnixStream) -> (u32, u16, Vec<u8>) {
    let mut header = [0; 8];
    peer.read_exact(&mut header)
        .expect("a request, not a closed private display");
    let object = u32::from_ne_bytes(header[..4].try_into().unwrap());
    let word = u32::from_ne_bytes(header[4..].try_into().unwrap());
    let len = usize::try_from(word >> 16).unwrap();
    assert!((8..=65_535).contains(&len) && len % 4 == 0);
    let mut body = vec![0; len - 8];
    peer.read_exact(&mut body).unwrap();
    (object, u16::try_from(word & 0xffff).unwrap(), body)
}

fn sync_after_commit(peer: &mut UnixStream, surface: Proxy) -> u32 {
    // SAFETY: the connection retaining this surface lives through the read.
    let surface = unsafe { surface.id() };
    let mut committed = false;
    for _ in 0..16 {
        let (object, opcode, body) = request(peer);
        committed |= object == surface && u32::from(opcode) == wl_surface::req::COMMIT;
        if object == 1 && u32::from(opcode) == wl_display::req::SYNC {
            assert!(committed, "the fence cannot precede the captured commit");
            return u32::from_ne_bytes(body[..4].try_into().unwrap());
        }
    }
    panic!("no sync after the submitted surface");
}

fn done(peer: &mut UnixStream, callback: u32) {
    let mut wire = Vec::new();
    for word in [callback, 12 << 16, 7, 1, (12 << 16) | 1, callback] {
        wire.extend_from_slice(&word.to_ne_bytes());
    }
    peer.write_all(&wire).unwrap();
}

fn confirmed<Owner: Send + 'static>(fence: SubmitSync<Owner>) -> Result<(), String> {
    use super::super::SubmitFence;
    std::thread::spawn(move || {
        let began = Instant::now();
        // The peer is another thread of this test process. This is its one-minute
        // hang detector, never an assertion about how quickly confirmation arrives.
        Box::new(fence).wait(&move || began.elapsed() >= aterm_spec::wait::MUST_HAPPEN)
    })
    .join()
    .expect("the confirmation worker does not panic")
}

#[test]
fn a_wayland_capture_fence_waits_for_its_processed_sync_under_backpressure() {
    let (mut socket, mut peer) = sockets();
    let send_bytes: libc::c_int = 4096;
    // SAFETY: a live socket and one initialized integer whose exact size is supplied.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&send_bytes as *const libc::c_int).cast(),
                libc::socklen_t::try_from(std::mem::size_of_val(&send_bytes)).unwrap(),
            )
        },
        0
    );
    socket.set_nonblocking(true).unwrap();
    let mut tail = Vec::new();
    let mut sent = 0;
    // Valid registry requests fill the socket before libwayland owns it. The private
    // peer withholds their globals; no event for these unused registries is sent.
    'full: for id in 10_000u32..20_000 {
        let bytes: Vec<_> = [1u32, (12 << 16) | 1, id]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        let mut at = 0;
        while at < bytes.len() {
            // A one-byte write cannot leave an unknown partial byte at EAGAIN. The
            // exact remainder of this valid request is completed after the peer drains.
            match socket.write(&bytes[at..at + 1]) {
                Ok(0) => panic!("the private socket closed while filling"),
                Ok(n) => {
                    at += n;
                    sent += n;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tail.extend_from_slice(&bytes[at..]);
                    break 'full;
                }
                Err(error) => panic!("filling the private socket: {error}"),
            }
        }
    }
    assert!(!tail.is_empty(), "the fixture actually filled its socket");
    let mut writer = socket.try_clone().unwrap();
    let connection = Connection::new(socket);
    let mut fence = connection.submit();
    assert_eq!(connection.display().try_flush(), Ok(Flushed::Queued));
    assert!(
        !fence.poll_once(0, &|| false).unwrap(),
        "queued writes are not confirmation"
    );
    let mut filled = vec![0; sent];
    peer.read_exact(&mut filled).unwrap();
    writer.write_all(&tail).unwrap();
    let mut rest = vec![0; tail.len()];
    peer.read_exact(&mut rest).unwrap();
    assert!(!fence.poll_once(0, &|| false).unwrap());
    let callback = sync_after_commit(&mut peer, connection.surface);
    assert!(
        !fence.poll_once(0, &|| false).unwrap(),
        "even a fully sent commit needs the peer's acknowledgment"
    );
    done(&mut peer, callback);
    assert_eq!(confirmed(fence), Ok(()));
    assert_eq!(connection.display().error(), None);
}

#[test]
fn a_cancelled_wayland_fence_retires_late_callbacks_and_preserves_other_queues() {
    use super::super::SubmitFence;
    for sent_before_cancel in [false, true] {
        let (socket, mut peer) = sockets();
        let connection = Connection::new(socket);
        let mut first = connection.submit();
        let late = if sent_before_cancel {
            assert!(!first.poll_once(0, &|| false).unwrap());
            Some(sync_after_commit(&mut peer, connection.surface))
        } else {
            None
        };
        assert!(Box::new(first).wait(&|| true).is_err());

        let ordinary = Queue::new(connection.display()).unwrap();
        let wrapper = ordinary.display_wrapper().unwrap();
        // SAFETY: this new callback belongs to ordinary, with one listener destroyed
        // after its proxy, before that queue and its retained display are dropped.
        let callback = unsafe { wl_display::sync(wrapper.proxy()).unwrap() };
        let called = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&called);
        // SAFETY: this callback has no other listener, and is destroyed before this
        // closure and its private queue after their only dispatch finishes below.
        let listener =
            unsafe { callback.listen(move |_, _, _| observed.store(true, Ordering::SeqCst)) }
                .unwrap();
        drop(wrapper);
        let mut next = connection.submit();
        assert!(!next.poll_once(0, &|| false).unwrap());
        let late = late.unwrap_or_else(|| sync_after_commit(&mut peer, connection.surface));
        let (object, opcode, body) = request(&mut peer);
        assert_eq!((object, u32::from(opcode)), (1, wl_display::req::SYNC));
        let ordinary_id = u32::from_ne_bytes(body[..4].try_into().unwrap());
        let own = sync_after_commit(&mut peer, connection.surface);
        done(&mut peer, late);
        done(&mut peer, ordinary_id);
        done(&mut peer, own);
        assert_eq!(confirmed(next), Ok(()));
        assert!(
            !called.load(Ordering::SeqCst),
            "the worker never dispatches the UI queue"
        );
        ordinary.dispatch_pending().unwrap();
        assert!(called.load(Ordering::SeqCst));
        assert_eq!(
            connection.display().error(),
            None,
            "a late cancelled callback cannot poison the shared connection"
        );
        // SAFETY: ordinary's dispatch is complete and no other thread dispatches it.
        unsafe { callback.destroy() };
        drop(listener);
    }
}

#[test]
fn a_wayland_capture_fence_refuses_peer_hangup_or_protocol_error() {
    for fault in ["hangup", "protocol"] {
        let (socket, mut peer) = sockets();
        let connection = Connection::new(socket);
        let mut fence = connection.submit();
        assert!(!fence.poll_once(0, &|| false).unwrap());
        let _ = sync_after_commit(&mut peer, connection.surface);
        if fault == "protocol" {
            // wl_display.error(object=1, code=1, message="refused").
            let mut wire = Vec::new();
            for word in [1u32, 28 << 16, 1, 1, 8] {
                wire.extend_from_slice(&word.to_ne_bytes());
            }
            wire.extend_from_slice(b"refused\0");
            peer.write_all(&wire).unwrap();
            assert!(confirmed(fence).is_err());
        } else {
            drop(peer);
            assert!(confirmed(fence).is_err());
        }
    }
}
