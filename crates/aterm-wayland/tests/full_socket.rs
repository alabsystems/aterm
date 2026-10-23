// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A compositor that stops reading: the client's socket fills, and `Display::try_flush`, the flush
//! aterm-window's event loop makes, says `Flushed::Queued` where libwayland's flush says EAGAIN, so
//! the loop keeps the rest queued and waits for the socket to take it.

#![cfg(target_os = "linux")]

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use aterm_wayland::protocols::wayland::wl_display;
use aterm_wayland::proxy::{Error, Flushed, OwnedDisplay};

/// `wl_display.sync`'s size on the wire: object, size and opcode, new id.
const SYNC_BYTES: usize = 12;

fn socket_pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    // SAFETY: a two-int array for socketpair(2) to fill.
    let r = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    assert_eq!(r, 0, "socketpair: {}", std::io::Error::last_os_error());
    // SAFETY: two fresh descriptors this test owns.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// A small send buffer, so the socket fills after a few kilobytes rather
/// than the default couple of hundred.
fn shrink(fd: &OwnedFd, option: libc::c_int) {
    let size: libc::c_int = 4096;
    // SAFETY: a live socket and an int option value.
    let r = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            (&raw const size).cast(),
            libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>()).unwrap(),
        )
    };
    assert_eq!(r, 0, "setsockopt: {}", std::io::Error::last_os_error());
}

/// Whether `fd` polls writable now.
fn writable(fd: libc::c_int) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one pollfd, no wait.
    let n = unsafe { libc::poll(&raw mut p, 1, 0) };
    n == 1 && p.revents & libc::POLLOUT != 0
}

/// Everything the far end holds now, appended to `into`; nonblocking.
fn drain(fd: &OwnedFd, into: &mut Vec<u8>) {
    let mut buf = [0u8; 65536];
    loop {
        // SAFETY: a live socket and a buffer of that length.
        let n = unsafe {
            libc::recv(
                fd.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if n <= 0 {
            return;
        }
        into.extend_from_slice(&buf[..usize::try_from(n).unwrap()]);
    }
}

/// The requests read off the wire: each `(object, opcode, size, new id)`.
fn requests(wire: &[u8]) -> Vec<(u32, u16, u16, u32)> {
    let word = |b: &[u8]| u32::from_ne_bytes(b.try_into().unwrap());
    wire.chunks(SYNC_BYTES)
        .map(|m| {
            let head = word(&m[4..8]);
            let half = |v: u32| u16::try_from(v & 0xffff).unwrap();
            (
                word(&m[0..4]),
                half(head),
                half(head >> 16),
                word(&m[8..12]),
            )
        })
        .collect()
}

#[test]
fn a_full_socket_keeps_the_rest_queued_until_the_compositor_reads() {
    let (client, server) = socket_pair();
    shrink(&client, libc::SO_SNDBUF);
    shrink(&server, libc::SO_RCVBUF);
    let fd = client.as_raw_fd();
    let display = OwnedDisplay::connect_to_fd(client).expect("libwayland takes the socket");
    // SAFETY: `display` outlives every use below; one thread.
    let d = unsafe { display.display() };
    let mut sent = 0usize;
    let full = loop {
        // SAFETY: the live display's own object.
        assert!(
            unsafe { wl_display::sync(d.proxy()) }.is_some(),
            "a request is marshalled"
        );
        sent += 1;
        match d.try_flush() {
            Ok(Flushed::All) => assert!(sent < 100_000, "the socket never filled"),
            full => break full,
        }
    };
    assert_eq!(full, Ok(Flushed::Queued), "a full socket queues the rest");
    assert_eq!(
        d.flush(),
        Err(Error::Display(libc::EAGAIN)),
        "libwayland's own answer for a full socket"
    );
    assert!(d.error().is_none(), "EAGAIN is not the display's error");
    assert!(!writable(fd), "a full socket does not poll writable");
    for _ in 0..16 {
        // SAFETY: as above.
        assert!(unsafe { wl_display::sync(d.proxy()) }.is_some());
        sent += 1;
    }
    assert!(d.error().is_none(), "queued behind a full socket");

    let mut wire = Vec::new();
    let mut flushes = 0;
    loop {
        drain(&server, &mut wire);
        assert!(writable(fd), "a socket the far end read polls writable");
        flushes += 1;
        match d.try_flush() {
            Ok(Flushed::All) => break,
            Ok(Flushed::Queued) => assert!(flushes < 10_000, "never drained"),
            Err(e) => panic!("a flush after the far end read: {e:?}"),
        }
    }
    drain(&server, &mut wire);
    assert_eq!(
        wire.len(),
        sent * SYNC_BYTES,
        "every request on the wire, once"
    );
    let got = requests(&wire);
    for (i, r) in got.iter().enumerate() {
        let id = u32::try_from(i).unwrap() + 2;
        assert_eq!(*r, (1, 0, 12, id), "request {i} of {sent}");
    }
    println!(
        "full_socket: the socket filled after {sent} syncs ({} bytes)",
        sent * SYNC_BYTES
    );
}

#[test]
fn queued_past_libwaylands_own_buffer_the_connection_ends() {
    let (client, server) = socket_pair();
    shrink(&client, libc::SO_SNDBUF);
    shrink(&server, libc::SO_RCVBUF);
    let display = OwnedDisplay::connect_to_fd(client).expect("libwayland takes the socket");
    // SAFETY: `display` outlives every use below; one thread.
    let d = unsafe { display.display() };
    loop {
        // SAFETY: the live display's own object.
        let _ = unsafe { wl_display::sync(d.proxy()) };
        if d.flush().is_err() {
            break;
        }
    }
    let mut more = 0usize;
    let fatal = loop {
        // SAFETY: as above; once the display has failed a marshal is dropped.
        let _ = unsafe { wl_display::sync(d.proxy()) };
        more += 1;
        if let Some(e) = d.error() {
            break Some(e);
        }
        if more * SYNC_BYTES > 1 << 16 {
            break None;
        }
    };
    println!(
        "full_socket: past a full socket, libwayland queued {more} more syncs ({} bytes); its \
         end: {fatal:?}",
        more * SYNC_BYTES
    );
    assert!(
        matches!(fatal, None | Some(Error::Display(libc::EAGAIN))),
        "{fatal:?}"
    );
    if let Some(fatal) = fatal {
        assert_eq!(d.try_flush(), Err(fatal), "an ended display is not queued");
    }
    drop(server);
}
