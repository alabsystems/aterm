// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Complete startup DATA on an independently peer-checked physical channel.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt as _;

const LIMIT: usize = 16 * 1024 * 1024;
#[derive(Clone)]
pub(crate) struct Settings {
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}
impl Settings {
    pub fn read_socket(
        stream: &aterm_uds::CtlStream,
        deadline: std::time::Instant,
    ) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Self::read(&mut SocketInput { stream, deadline })
    }
    pub fn read(input: &mut impl Read) -> io::Result<Self> {
        let mut header = [0; 12];
        input.read_exact(&mut header)?;
        if &header[..8] != b"ALABSET1" {
            return Err(invalid());
        }
        let len = u32::from_be_bytes(header[8..].try_into().map_err(|_| invalid())?) as usize;
        if len > LIMIT {
            return Err(invalid());
        }
        let mut body = vec![0; len];
        input.read_exact(&mut body)?;
        let mut tail = [0];
        if input.read(&mut tail)? != 0 {
            return Err(invalid());
        }
        Self::decode(&body)
    }
    fn decode(mut body: &[u8]) -> io::Result<Self> {
        fn number(body: &mut &[u8]) -> io::Result<usize> {
            let value = u32::from_be_bytes(
                body.get(..4)
                    .ok_or_else(invalid)?
                    .try_into()
                    .map_err(|_| invalid())?,
            ) as usize;
            *body = &body[4..];
            Ok(value)
        }
        fn word(body: &mut &[u8]) -> io::Result<Vec<u8>> {
            let len = number(body)?;
            let value = body.get(..len).ok_or_else(invalid)?.to_vec();
            if value.contains(&0) {
                return Err(invalid());
            }
            *body = &body[len..];
            Ok(value)
        }
        let count = number(&mut body)?;
        if count > body.len() / 4 {
            return Err(invalid());
        }
        let mut arguments = Vec::new();
        for _ in 0..count {
            arguments.push(OsString::from_vec(word(&mut body)?));
        }
        let count = number(&mut body)?;
        if count > body.len() / 8 {
            return Err(invalid());
        }
        let mut environment = BTreeMap::new();
        for _ in 0..count {
            let key = word(&mut body)?;
            let value = word(&mut body)?;
            if key.is_empty() || key.contains(&b'=') || environment.insert(key, value).is_some() {
                return Err(invalid());
            }
        }
        if !body.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            arguments,
            environment: environment
                .into_iter()
                .map(|(key, value)| (OsString::from_vec(key), OsString::from_vec(value)))
                .collect(),
        })
    }
}
// Read buffered bytes through hangup without changing socket timeout options.
// One absolute deadline covers the complete frame and its required physical EOF.
struct SocketInput<'a> {
    stream: &'a aterm_uds::CtlStream,
    deadline: std::time::Instant,
}
impl Read for SocketInput<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        use std::os::fd::AsFd as _;
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            aterm_uds::ready::until(
                self.stream.as_fd(),
                aterm_uds::ready::Interest::Read,
                self.deadline,
            )?;
            match self.stream.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid native startup settings",
    )
}

#[cfg(test)]
#[path = "codex_retained_settings_tests.rs"]
mod tests;
