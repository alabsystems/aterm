// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_x11::proto::xproto::{Depth, Format, Screen, Visualtype};

fn setup() -> Setup {
    let visual = |id| Visualtype {
        visual_id: id,
        class: visual_class::TRUE_COLOR,
        bits_per_rgb_value: 8,
        colormap_entries: 256,
        red_mask: 0x00ff_0000,
        green_mask: 0x0000_ff00,
        blue_mask: 0x0000_00ff,
    };
    Setup {
        status: 1,
        protocol_major_version: 11,
        protocol_minor_version: 0,
        release_number: 0,
        resource_id_base: 0,
        resource_id_mask: 0,
        motion_buffer_size: 0,
        maximum_request_length: 65535,
        image_byte_order: if cfg!(target_endian = "little") {
            image_order::LSB_FIRST
        } else {
            image_order::MSB_FIRST
        },
        bitmap_format_bit_order: 0,
        bitmap_format_scanline_unit: 32,
        bitmap_format_scanline_pad: 32,
        min_keycode: 8,
        max_keycode: 255,
        vendor: Vec::new(),
        pixmap_formats: vec![
            Format {
                depth: 24,
                bits_per_pixel: 32,
                scanline_pad: 32,
            },
            Format {
                depth: 32,
                bits_per_pixel: 32,
                scanline_pad: 32,
            },
        ],
        roots: vec![Screen {
            root: 1,
            default_colormap: 0,
            white_pixel: 0,
            black_pixel: 0,
            current_input_masks: 0,
            width_in_pixels: 80,
            height_in_pixels: 24,
            width_in_millimeters: 0,
            height_in_millimeters: 0,
            min_installed_maps: 0,
            max_installed_maps: 0,
            root_visual: 0x24,
            backing_stores: 0,
            save_unders: false,
            root_depth: 24,
            allowed_depths: vec![
                Depth {
                    depth: 24,
                    visuals: vec![visual(0x24)],
                },
                Depth {
                    depth: 32,
                    visuals: vec![visual(0x32)],
                },
            ],
        }],
    }
}

enum Ack {
    Reply,
    Error,
    Close,
}

struct NativePeer {
    conn: Arc<Connection>,
    seen: std::sync::mpsc::Receiver<u64>,
    answer: std::sync::mpsc::Sender<Ack>,
    requests: Arc<std::sync::Mutex<Vec<u8>>>,
    shutdown: std::os::unix::net::UnixStream,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl NativePeer {
    fn new(held_focus: usize) -> Self {
        use std::io::{Read as _, Write as _};
        use std::os::unix::net::UnixStream;
        let (client, mut server) = aterm_uds::kept_out(UnixStream::pair).unwrap();
        let shutdown = aterm_uds::kept_out(|| server.try_clone()).unwrap();
        server
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .unwrap();
        let (sent, seen) = std::sync::mpsc::channel();
        let (answer, replies) = std::sync::mpsc::channel();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let worker = std::thread::spawn(move || {
            let mut hello = [0; 12];
            if server.read_exact(&mut hello).is_err() {
                return;
            }
            assert_eq!(
                hello[0],
                if cfg!(target_endian = "little") {
                    b'l'
                } else {
                    b'B'
                }
            );
            assert_eq!(
                &hello[6..10],
                &[0; 4],
                "owned transport needs no authority cookie"
            );
            let mut setup = [0u8; 40];
            setup[0] = 1;
            setup[2..4].copy_from_slice(&11u16.to_ne_bytes());
            setup[6..8].copy_from_slice(&8u16.to_ne_bytes());
            setup[12..16].copy_from_slice(&0x1000_0000u32.to_ne_bytes());
            setup[16..20].copy_from_slice(&0x000f_ffffu32.to_ne_bytes());
            setup[26..28].copy_from_slice(&u16::MAX.to_ne_bytes());
            setup[32] = 32;
            setup[33] = 32;
            setup[34] = 8;
            setup[35] = 255;
            server.write_all(&setup).unwrap();
            let mut sequence = 0u16;
            let mut focuses = 0;
            loop {
                let mut head = [0; 4];
                if server.read_exact(&mut head).is_err() {
                    return;
                }
                sequence = sequence.checked_add(1).unwrap();
                let bytes = usize::from(u16::from_ne_bytes([head[2], head[3]])) * 4;
                assert!(bytes >= 4);
                let mut body = vec![0; bytes - 4];
                server.read_exact(&mut body).unwrap();
                recorded.lock().unwrap().push(head[0]);
                let mut reply = [0u8; 32];
                reply[0] = 1;
                reply[2..4].copy_from_slice(&sequence.to_ne_bytes());
                match head[0] {
                    98 => {
                        // BIG-REQUESTS is absent; the ordinary request bound is enough.
                        server.write_all(&reply).unwrap();
                    }
                    14 => {
                        reply[1] = 24;
                        reply[8..12].copy_from_slice(&1u32.to_ne_bytes());
                        reply[16..18].copy_from_slice(&2u16.to_ne_bytes());
                        reply[18..20].copy_from_slice(&2u16.to_ne_bytes());
                        server.write_all(&reply).unwrap();
                    }
                    43 => {
                        focuses += 1;
                        if focuses == held_focus {
                            sent.send(u64::from(sequence)).unwrap();
                            match replies.recv().unwrap_or(Ack::Close) {
                                Ack::Close => {
                                    let _ = server.shutdown(std::net::Shutdown::Both);
                                    return;
                                }
                                Ack::Error => {
                                    reply[0] = 0;
                                    reply[1] = 2;
                                    reply[10] = 43;
                                }
                                Ack::Reply => {}
                            }
                        }
                        reply[8..12].copy_from_slice(&2u32.to_ne_bytes());
                        if server.write_all(&reply).is_err() {
                            return;
                        }
                    }
                    72 | 130 => {} // Wire PutImage or this peer's MIT-SHM opcode.
                    other => panic!("unexpected native request {other}"),
                }
                if server.flush().is_err() {
                    return;
                }
            }
        });
        let conn = Arc::new(Connection::from_fd(client.into()).unwrap());
        Self {
            conn,
            seen,
            answer,
            requests,
            shutdown,
            worker: Some(worker),
        }
    }
}

impl Drop for NativePeer {
    fn drop(&mut self) {
        let _ = self.answer.send(Ack::Close);
        let _ = self.shutdown.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn submitted(peer: &NativePeer, shm: bool) -> Store {
    let mut store = if shm {
        Store::Shm {
            major: 130,
            seg: Some(Segment {
                mem: ShmMem::new(c"aterm-x11-capture-test", 16).unwrap(),
                id: 5,
            }),
            pending: None,
        }
    } else {
        Store::Wire(vec![0x123456; 4])
    };
    let damage = DamageRect {
        x: 0,
        y: 0,
        width: NonZeroU32::new(2).unwrap(),
        height: NonZeroU32::new(2).unwrap(),
    };
    assert!(
        submit_pixels(&peer.conn, 2, 3, 24, &mut store, (2, 2), &[damage])
            .unwrap()
            .is_some()
    );
    // An ordinary server reply orders the preceding native requests before inspection.
    peer.conn
        .send(xproto::get_geometry(2))
        .unwrap()
        .reply()
        .unwrap();
    let requests = peer.requests.lock().unwrap();
    assert_eq!(
        requests.iter().filter(|opcode| **opcode == 43).count(),
        usize::from(shm),
        "ordinary SHM keeps its existing reuse fence; wire adds no round trip"
    );
    assert!(requests.contains(if shm { &130 } else { &72 }));
    store
}

struct WaitingFence {
    stopped: Arc<std::sync::atomic::AtomicBool>,
    shutdown: std::os::unix::net::UnixStream,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl WaitingFence {
    fn join(&mut self) -> Result<(), String> {
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for WaitingFence {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stopped
                .store(true, std::sync::atomic::Ordering::Release);
            let _ = self.shutdown.shutdown(std::net::Shutdown::Both);
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap().ok();
            }
        }
    }
}

#[test]
fn capture_ack_is_on_the_image_connection_and_cancellation_keeps_that_connection_live() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for shm in [false, true] {
        for outcome in ["ack", "cancel", "error", "closed"] {
            let peer = NativePeer::new(if shm { 2 } else { 1 });
            let _store = submitted(&peer, shm);
            let stopped = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stopped);
            let fence = submit_fence(Arc::clone(&peer.conn), ());
            let mut waiting = WaitingFence {
                stopped: Arc::clone(&stopped),
                shutdown: aterm_uds::kept_out(|| peer.shutdown.try_clone()).unwrap(),
                worker: Some(std::thread::spawn(move || {
                    let began = std::time::Instant::now();
                    fence.wait(&|| {
                        worker_stop.load(Ordering::Acquire)
                            || began.elapsed() >= aterm_spec::wait::MUST_HAPPEN
                    })
                })),
            };
            let sequence = peer
                .seen
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
            assert!(
                peer.conn
                    .poll_reply::<GetInputFocusReply>(sequence)
                    .unwrap()
                    .is_none(),
                "no server acknowledgement can be invented while the native peer holds it"
            );
            match outcome {
                "cancel" => stopped.store(true, Ordering::Release),
                "error" => peer.answer.send(Ack::Error).unwrap(),
                "closed" => peer.answer.send(Ack::Close).unwrap(),
                _ => peer.answer.send(Ack::Reply).unwrap(),
            }
            let result = waiting.join();
            assert_eq!(
                result.is_ok(),
                outcome == "ack",
                "shm={shm} {outcome}: {result:?}"
            );
            if outcome == "cancel" {
                peer.answer.send(Ack::Reply).unwrap();
                peer.conn
                    .send(xproto::get_input_focus())
                    .unwrap()
                    .reply()
                    .unwrap();
                assert!(
                    peer.conn.status().is_ok(),
                    "cancel discards only its own sequence"
                );
            }
        }
    }
}

/// A post sends what changed only where the segment still holds the
/// last post; a fresh or resized one is sent whole.
#[test]
fn a_post_sends_its_damage_only_over_the_last_post() {
    let damage = Some([10, 40, 900, 70]);
    assert_eq!(
        post_rect(damage, true, (800, 600)),
        [10, 40, 800, 70],
        "the damage, inside the frame"
    );
    assert_eq!(
        post_rect(damage, false, (800, 600)),
        [0, 0, 800, 600],
        "a segment holding no post at this size is sent whole"
    );
    assert_eq!(post_rect(None, true, (800, 600)), [0, 0, 800, 600]);
    assert_eq!(
        post_rect(Some([0, 0, 0, 0]), true, (800, 600)),
        [0, 0, 0, 0],
        "nothing changed, nothing sent"
    );
}

/// Formats, visuals and the observed window depth must describe one layout.
#[test]
fn the_windows_depth_owns_its_pixel_format_and_visual() {
    let ordinary = setup();
    assert!(visual_supported(&ordinary, 0x24, 24));
    assert!(visual_supported(&ordinary, 0x32, 32));
    assert!(!visual_supported(&ordinary, 0xff, 24));
    let mut wrong_masks = ordinary.clone();
    wrong_masks.roots[0].allowed_depths[0].visuals[0].red_mask ^= 1;
    assert!(!visual_supported(&wrong_masks, 0x24, 24));
    let mut wrong_class = ordinary.clone();
    wrong_class.roots[0].allowed_depths[0].visuals[0].class = u8::MAX;
    assert!(!visual_supported(&wrong_class, 0x24, 24));
    let mut absent = ordinary.clone();
    absent.pixmap_formats.clear();
    assert!(!visual_supported(&absent, 0x24, 24));

    let mut admitted = Vec::new();
    let mut packed24 = ordinary.clone();
    packed24.pixmap_formats[0].bits_per_pixel = 24;
    if visual_supported(&packed24, 0x24, 24) {
        admitted.push("24-bit format borrowed from depth32");
    }
    let mut packed32 = ordinary.clone();
    packed32.pixmap_formats[1].bits_per_pixel = 24;
    if visual_supported(&packed32, 0x32, 32) {
        admitted.push("32-bit format borrowed from depth24");
    }
    if visual_supported(&ordinary, 0x24, 32) {
        admitted.push("visual from another depth");
    }
    if visual_supported(&ordinary, 0x24, 16) {
        admitted.push("unsupported window depth");
    }
    assert!(
        admitted.is_empty(),
        "X11_WINDOW_DEPTH_MUST_OWN_ITS_LAYOUT: {admitted:?}"
    );
}
