// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! External archive RPC DATA, bound to the complete original request. Neither
//! an event, a terminal nor an acknowledgement creates native closure, input
//! custody, a GNU return, seed availability or a completed operation owner.
use std::io;

use super::Request;
use crate::wire::{Decoder, Encoder, header, invalid};
use crate::{MetadataCompletion, MetadataPrepared, Started, Status, StatusFrame};

pub const MAX_OUTPUT: u64 = 64 * 1024;
pub const MAX_CHUNK: usize = 4096;
const MAX_REASON: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}
impl Stream {
    fn write(self, encoder: &mut Encoder) {
        encoder.number(match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        });
    }
    fn read(decoder: &mut Decoder<'_>) -> io::Result<Self> {
        match decoder.number()? {
            0 => Ok(Self::Stdout),
            1 => Ok(Self::Stderr),
            _ => Err(invalid()),
        }
    }
}

// Keep the std-only codec independent of a hash provider. The owner supplies
// its actual expected digest; exact request bytes additionally bind every
// selector, owner observation and endpoint even if a caller reuses a digest.
fn write_binding(e: &mut Encoder, request: &Request, digest: [u8; 32]) -> io::Result<()> {
    e.frame_bytes(&request.encode()?)?;
    e.0.extend(digest);
    Ok(())
}
fn read_binding(d: &mut Decoder<'_>, request: &Request, digest: [u8; 32]) -> io::Result<()> {
    if d.frame_bytes()? != request.encode()? || d.array::<32>()? != digest {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Prepared(Box<MetadataPrepared>),
    Started(Started),
    Output {
        stream: Stream,
        offset: u64,
        bytes: Vec<u8>,
    },
    Eof {
        stream: Stream,
        bytes: u64,
    },
}
impl Event {
    pub fn encode(&self, request: &Request, digest: [u8; 32]) -> io::Result<Vec<u8>> {
        let mut e = Encoder::new(121);
        e.number(1);
        write_binding(&mut e, request, digest)?;
        match self {
            Self::Prepared(prepared) => {
                request.validate_prepared(prepared)?;
                if prepared.host != request.host || prepared.boot != request.boot {
                    return Err(invalid());
                }
                e.number(0);
                e.frame_bytes(&prepared.encode()?)?;
            }
            Self::Started(started) => {
                e.number(1);
                e.frame_bytes(
                    &StatusFrame {
                        root_attempt: request.key.graph.query.binding.root_attempt,
                        member_attempt: request.key.operation,
                        status: Status::Started(*started),
                    }
                    .encode()?,
                )?;
            }
            Self::Output {
                stream,
                offset,
                bytes,
            } => {
                if bytes.is_empty()
                    || bytes.len() > MAX_CHUNK
                    || offset
                        .checked_add(bytes.len() as u64)
                        .is_none_or(|end| end > MAX_OUTPUT)
                {
                    return Err(invalid());
                }
                e.number(2);
                stream.write(&mut e);
                e.number(*offset);
                e.bytes(bytes)?;
            }
            Self::Eof { stream, bytes } => {
                if *bytes > MAX_OUTPUT {
                    return Err(invalid());
                }
                e.number(3);
                stream.write(&mut e);
                e.number(*bytes);
            }
        }
        e.finish()
    }
    pub fn decode(bytes: &[u8], request: &Request, digest: [u8; 32]) -> io::Result<Self> {
        let mut d = header(bytes, 121)?;
        if d.number()? != 1 {
            return Err(invalid());
        }
        read_binding(&mut d, request, digest)?;
        let value = match d.number()? {
            0 => Self::Prepared(Box::new(MetadataPrepared::decode(
                &d.frame_bytes()?,
                request.key.graph.query.binding.root_attempt,
                request.key.operation,
            )?)),
            1 => match StatusFrame::decode(
                &d.frame_bytes()?,
                request.key.graph.query.binding.root_attempt,
                request.key.operation,
            )?
            .status
            {
                Status::Started(started) => Self::Started(started),
                _ => return Err(invalid()),
            },
            2 => Self::Output {
                stream: Stream::read(&mut d)?,
                offset: d.number()?,
                bytes: d.bytes()?,
            },
            3 => Self::Eof {
                stream: Stream::read(&mut d)?,
                bytes: d.number()?,
            },
            _ => return Err(invalid()),
        };
        d.done()?;
        value.encode(request, digest)?;
        Ok(value)
    }
}

/// A diagnostic completed outcome may still have a nonzero exit, a signal or
/// failed input postchecks. Only the actual native issuer can close its work;
/// decoding this DATA cannot return resources or authorize a later seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminal {
    Completed {
        completion: Box<MetadataCompletion>,
        observation: crate::accounting::Observation,
    },
    Refused {
        reason: String,
    },
    Unknown {
        reason: String,
    },
}
impl Terminal {
    pub fn encode(&self, request: &Request, digest: [u8; 32]) -> io::Result<Vec<u8>> {
        let mut e = Encoder::new(122);
        e.number(1);
        write_binding(&mut e, request, digest)?;
        match self {
            Self::Completed {
                completion,
                observation,
            } => {
                let bytes = completion.encode()?;
                MetadataCompletion::decode(
                    &bytes,
                    request.key.graph.query.binding.root_attempt,
                    request.key.operation,
                )?;
                e.number(0);
                e.frame_bytes(&bytes)?;
                e.frame_bytes(&observation.encode()?)?;
            }
            Self::Refused { reason } | Self::Unknown { reason } => {
                if reason.is_empty()
                    || reason.len() > MAX_REASON
                    || reason.chars().any(char::is_control)
                {
                    return Err(invalid());
                }
                e.number(if matches!(self, Self::Refused { .. }) {
                    1
                } else {
                    2
                });
                e.bytes(reason.as_bytes())?;
            }
        }
        e.finish()
    }
    pub fn decode(bytes: &[u8], request: &Request, digest: [u8; 32]) -> io::Result<Self> {
        let mut d = header(bytes, 122)?;
        if d.number()? != 1 {
            return Err(invalid());
        }
        read_binding(&mut d, request, digest)?;
        let value = match d.number()? {
            0 => Self::Completed {
                completion: Box::new(MetadataCompletion::decode(
                    &d.frame_bytes()?,
                    request.key.graph.query.binding.root_attempt,
                    request.key.operation,
                )?),
                observation: crate::accounting::Observation::decode(&d.frame_bytes()?)?,
            },
            1 => Self::Refused { reason: d.text()? },
            2 => Self::Unknown { reason: d.text()? },
            _ => return Err(invalid()),
        };
        d.done()?;
        value.encode(request, digest)?;
        Ok(value)
    }
}

/// Delivery DATA binding the complete exact terminal and original request.
/// Constructing/decoding an Ack supplies no native closure or resource return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack;
impl Ack {
    pub fn encode(request: &Request, digest: [u8; 32], terminal: &Terminal) -> io::Result<Vec<u8>> {
        let mut e = Encoder::new(123);
        e.number(1);
        write_binding(&mut e, request, digest)?;
        e.frame_bytes(&terminal.encode(request, digest)?)?;
        e.finish()
    }
    pub fn decode(
        bytes: &[u8],
        request: &Request,
        digest: [u8; 32],
        terminal: &Terminal,
    ) -> io::Result<Self> {
        let mut d = header(bytes, 123)?;
        if d.number()? != 1 {
            return Err(invalid());
        }
        read_binding(&mut d, request, digest)?;
        if d.frame_bytes()? != terminal.encode(request, digest)? {
            return Err(invalid());
        }
        d.done()?;
        Ok(Self)
    }
}

#[cfg(test)]
mod tests;
