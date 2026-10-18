// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Every byte the caller asks for is written, at every length from 0 to 4096
//! (sixteen of getentropy(2)'s 256-byte chunks, and every remainder), through
//! [`getrandom`] and [`getrandom_uninit`].
//!
//! The property is [`every_byte_is_written`]: a buffer preset to a sentinel
//! is filled, and each byte must leave the sentinel at least once in up to
//! [`ROUNDS`] fills (a random byte equals it with probability 1/256, so a
//! position misses all of them with probability 2^-64), and two fills of 16
//! bytes or more must differ (2^-128). Length 0 is in the sweep, so an empty
//! request must succeed too.

use core::mem::MaybeUninit;

use getrandom::{Error, getrandom, getrandom_uninit};

/// The longest buffer checked.
const MAX_LEN: usize = 4096;
/// Fills per length before a byte that never left the sentinel is a failure.
const ROUNDS: usize = 8;
/// What every byte holds before a fill: zero, the state a fresh or zeroed
/// buffer that nothing wrote would show.
const SENTINEL: u8 = 0;

/// `Ok` when `fill` writes every byte of every length up to [`MAX_LEN`] and
/// does not repeat itself; otherwise the first length and byte it missed.
fn every_byte_is_written(fill: impl Fn(&mut [u8]) -> Result<(), Error>) -> Result<(), String> {
    for len in 0..=MAX_LEN {
        let mut written = vec![false; len];
        let mut first: Option<Vec<u8>> = None;
        for round in 0..ROUNDS {
            let mut buf = vec![SENTINEL; len];
            fill(&mut buf).map_err(|e| format!("len {len}: the fill failed: {e}"))?;
            for (w, &b) in written.iter_mut().zip(&buf) {
                *w |= b != SENTINEL;
            }
            match &first {
                None => first = Some(buf),
                Some(prev) if len >= 16 && *prev == buf => {
                    return Err(format!("len {len}: two fills wrote the same bytes"));
                }
                Some(_) => {}
            }
            if round >= 1 && written.iter().all(|&w| w) {
                break;
            }
        }
        if let Some(at) = written.iter().position(|&w| !w) {
            return Err(format!(
                "len {len}: byte {at} kept the sentinel through {ROUNDS} fills"
            ));
        }
    }
    Ok(())
}

#[test]
fn getrandom_writes_every_byte_of_every_length() {
    assert_eq!(every_byte_is_written(getrandom), Ok(()));
}

#[test]
fn getrandom_uninit_writes_every_byte_and_returns_the_same_memory() {
    let through_uninit = |buf: &mut [u8]| -> Result<(), Error> {
        let mut dest: Vec<MaybeUninit<u8>> = buf.iter().map(|&b| MaybeUninit::new(b)).collect();
        let at = dest.as_ptr().cast::<u8>();
        let out = getrandom_uninit(&mut dest)?;
        assert_eq!((out.as_ptr(), out.len()), (at, buf.len()));
        buf.copy_from_slice(out);
        Ok(())
    };
    assert_eq!(every_byte_is_written(through_uninit), Ok(()));
}
