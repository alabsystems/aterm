// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus xgrab <window> <out>` X11 `GetImage` PPM reader via [`crate::xdnd`].

use std::io::Write as _;
use std::path::Path;

use crate::xdnd::{Error, X, fault, get_geometry, request, u16_at, u32_at, words};

/// The largest image this reads: a 4096 x 4096 window of 32-bit pixels.
const MAX_IMAGE: usize = 4096 * 4096 * 4;

/// `GetImage` (73) of the whole of `drawable`, ZPixmap, every plane.
fn get_image(drawable: u32, width: u16, height: u16) -> Vec<u8> {
    let mut body = words(&[drawable]);
    for v in [0u16, 0, width, height] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    body.extend_from_slice(&u32::MAX.to_le_bytes());
    request(73, 2, &body)
}

pub(crate) fn run(window: u32, out: &Path) -> Result<(), Error> {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let mut x = X::connect(&display)?;
    let g = x.call(&get_geometry(window))?;
    let short = || fault("a short GetGeometry reply");
    let depth = *g.get(1).ok_or_else(short)?;
    let width = u16_at(&g, 16).ok_or_else(short)?;
    let height = u16_at(&g, 18).ok_or_else(short)?;
    if !matches!(depth, 24 | 32) {
        return Err(fault(format!(
            "a depth-{depth} window, which this probe does not read"
        )));
    }
    x.max_reply = MAX_IMAGE;
    let r = x.call(&get_image(window, width, height))?;
    let pixels = usize::from(width) * usize::from(height);
    let data = r.get(32..32 + pixels * 4).ok_or_else(|| {
        fault(format!(
            "a GetImage reply short of {width}x{height} 32-bit pixels"
        ))
    })?;
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    ppm.reserve(pixels * 3);
    for i in 0..pixels {
        let p = u32_at(data, i * 4).ok_or_else(|| fault("a short pixel"))?;
        ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
    }
    let mut f = std::fs::File::create(out).map_err(|e| fault(format!("{}: {e}", out.display())))?;
    f.write_all(&ppm)
        .map_err(|e| fault(format!("{}: {e}", out.display())))?;
    println!("{width}x{height} depth {depth}");
    Ok(())
}
