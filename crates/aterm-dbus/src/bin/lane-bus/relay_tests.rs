// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Cuts a stream into 4-byte messages and drops every one starting 0xff.
struct Fours;

impl Filter for Fours {
    fn len(&mut self, buf: &[u8]) -> Result<Option<usize>, String> {
        Ok((!buf.is_empty()).then_some(4))
    }

    fn pass(&mut self, msg: &mut [u8]) -> bool {
        msg[1] = msg[1].wrapping_add(1);
        msg[0] != 0xff
    }
}

#[test]
fn a_stream_is_cut_into_messages_filtered_and_its_descriptors_carried() {
    let (mut a, b) = UnixStream::pair().unwrap();
    let (c, mut d) = UnixStream::pair().unwrap();
    let t = std::thread::spawn(move || pump(&b, &c, &mut Fours));
    let (e, mut f) = UnixStream::pair().unwrap();
    let mut fds = vec![OwnedFd::from(e)];
    send(&a, &[1, 1, 1], &mut fds).unwrap();
    use std::io::{Read as _, Write as _};
    a.write_all(&[1, 0xff, 1, 1, 1, 2, 3, 4]).unwrap();
    a.write_all(&[2, 2]).unwrap();
    drop(a);
    let mut got = Vec::new();
    let mut buf = [0u8; 64];
    let mut carried = Vec::new();
    loop {
        let n = recv(&d, &mut buf, &mut carried).unwrap();
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, [1, 2, 1, 1, 2, 4, 4, 2]);
    assert!(
        t.join().unwrap().is_err(),
        "a stream ended inside a message"
    );
    assert_eq!(carried.len(), 1, "the descriptor came through");
    let mut through = UnixStream::from(carried.pop().unwrap());
    through.write_all(b"x").unwrap();
    let mut one = [0u8; 1];
    f.read_exact(&mut one).unwrap();
    assert_eq!(&one, b"x");
    let _ = d.flush();
}
