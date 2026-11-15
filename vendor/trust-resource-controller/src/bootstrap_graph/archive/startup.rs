//! Bind the child pre-read exchange to the exact archive request/context.
//! The native host retains its real child and original endpoint independently.

use std::io;

use super::Request;
use crate::wire::{Decoder, Encoder, header, invalid};

fn binding(e: &mut Encoder, request: &Request, digest: [u8; 32]) -> io::Result<()> {
    request.validate()?;
    e.frame_bytes(&request.encode()?)?;
    e.0.extend(digest);
    Ok(())
}
fn check(d: &mut Decoder<'_>, request: &Request, digest: [u8; 32]) -> io::Result<()> {
    request.validate()?;
    if d.frame_bytes()? != request.encode()? || d.array::<32>()? != digest {
        return Err(invalid());
    }
    Ok(())
}

/// Emit only after the actual child has captured its immutable Instant anchor
/// and original descriptions, before reading original source contents.
pub fn awaiting(request: &Request, digest: [u8; 32]) -> io::Result<Vec<u8>> {
    let mut e = Encoder::new(119);
    e.number(1);
    binding(&mut e, request, digest)?;
    e.finish()
}
pub fn validate_awaiting(request: &Request, digest: [u8; 32], bytes: &[u8]) -> io::Result<()> {
    let mut d = header(bytes, 119)?;
    if d.number()? != 1 {
        return Err(invalid());
    }
    check(&mut d, request, digest)?;
    d.done()
}

/// The host captures this restrictive remaining duration ONLY AFTER receiving
/// AwaitWork from its actual retained child. The child adds it to its EARLIER
/// entry anchor, never the receipt time. One immutable Run is consumed once;
/// repeated/late/malformed frames cannot renew the original operation endpoint.
pub fn run(request: &Request, digest: [u8; 32], remaining_ns: u64) -> io::Result<Vec<u8>> {
    if remaining_ns == 0 || remaining_ns > request.timeout_ns {
        return Err(invalid());
    }
    let mut e = Encoder::new(120);
    e.number(1);
    binding(&mut e, request, digest)?;
    e.number(remaining_ns);
    e.finish()
}
pub fn decode_run(request: &Request, digest: [u8; 32], bytes: &[u8]) -> io::Result<u64> {
    let mut d = header(bytes, 120)?;
    if d.number()? != 1 {
        return Err(invalid());
    }
    check(&mut d, request, digest)?;
    let remaining = d.number()?;
    d.done()?;
    if remaining == 0 || remaining > request.timeout_ns {
        return Err(invalid());
    }
    Ok(remaining)
}

#[cfg(test)]
mod tests;
