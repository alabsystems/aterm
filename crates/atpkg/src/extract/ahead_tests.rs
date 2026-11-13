// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A reader that yields `data` a few bytes at a time and then fails, once, with `fail`.
struct Dribble {
    data: Vec<u8>,
    pos: usize,
    fail: Option<io::ErrorKind>,
}

impl Read for Dribble {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let rest = &self.data[self.pos..];
        if rest.is_empty() {
            return match self.fail.take() {
                Some(kind) => Err(io::Error::new(kind, "dribble failed")),
                None => Ok(0),
            };
        }
        let n = rest.len().min(out.len()).min(7);
        out[..n].copy_from_slice(&rest[..n]);
        self.pos += n;
        Ok(n)
    }
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[test]
fn the_walk_reads_every_byte_in_order_and_gets_the_decoder_back() {
    let data = pattern(3 * CHUNK + 12_345);
    let src = Dribble {
        data: data.clone(),
        pos: 0,
        fail: None,
    };
    let (walked, back) = decoded_ahead(src, |r| {
        let mut got = Vec::new();
        r.read_to_end(&mut got).map(|_| got)
    });
    assert_eq!(walked.unwrap(), data);
    assert_eq!(
        back.pos,
        data.len(),
        "the decoder comes back read to its end"
    );
}

#[test]
fn the_walk_gets_the_bytes_before_a_failure_then_the_failure_itself() {
    let data = pattern(CHUNK + 99);
    let src = Dribble {
        data: data.clone(),
        pos: 0,
        fail: Some(io::ErrorKind::InvalidData),
    };
    let (walked, _) = decoded_ahead(src, |r| {
        let mut got = Vec::new();
        let err = r.read_to_end(&mut got).unwrap_err();
        (got, err.kind(), err.to_string())
    });
    assert_eq!(
        walked.0, data,
        "every byte before the failure arrives first"
    );
    assert_eq!(walked.1, io::ErrorKind::InvalidData);
    assert_eq!(
        walked.2, "dribble failed",
        "the decoder's own error, unchanged"
    );
}

#[test]
fn what_one_decoder_read_yields_reaches_the_walk_before_the_next_read_returns() {
    // A decoder whose second read waits until the walk has seen the first one's bytes:
    // a decoder thread that held them for a fuller chunk would wait forever.
    let (seen_tx, seen_rx) = mpsc::channel::<()>();
    struct Gated {
        reads: usize,
        seen: Receiver<()>,
    }
    impl Read for Gated {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            match self.reads {
                1 => {
                    out[..3].copy_from_slice(b"abc");
                    Ok(3)
                }
                2 => {
                    self.seen
                        .recv_timeout(std::time::Duration::from_secs(60))
                        .map_err(|_| io::Error::other("the walk never got the first read"))?;
                    Ok(0)
                }
                _ => Ok(0),
            }
        }
    }
    let (walked, _) = decoded_ahead(
        Gated {
            reads: 0,
            seen: seen_rx,
        },
        |r| {
            let mut first = [0u8; 3];
            r.read_exact(&mut first)?;
            let _ = seen_tx.send(());
            let mut rest = Vec::new();
            r.read_to_end(&mut rest).map(|_| (first, rest))
        },
    );
    let (first, rest) = walked.unwrap();
    assert_eq!(&first, b"abc");
    assert!(rest.is_empty());
}

#[test]
fn a_walk_that_stops_early_ends_the_decoder_thread() {
    // Far more than DEPTH chunks: the decoder thread must stop once the walk is gone,
    // or the scope never joins it.
    let src = Dribble {
        data: pattern((DEPTH + 8) * CHUNK),
        pos: 0,
        fail: None,
    };
    let (first, back) = decoded_ahead(src, |r| {
        let mut one = [0u8; 10];
        r.read_exact(&mut one).map(|()| one)
    });
    assert_eq!(
        first.unwrap(),
        <[u8; 10]>::try_from(&pattern(10)[..]).unwrap()
    );
    assert!(
        back.pos < back.data.len(),
        "the decoder thread stopped short of the end once the walk stopped reading"
    );
}
