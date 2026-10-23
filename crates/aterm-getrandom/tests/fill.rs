// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests that [`getrandom`] and [`getrandom_uninit`] fill every requested byte across lengths 0..=4096.

use core::mem::MaybeUninit;

use getrandom::{Error, getrandom, getrandom_uninit};

const MAX_LEN: usize = 4096;
const ROUNDS: usize = 8;
const SENTINEL: u8 = 0;

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
