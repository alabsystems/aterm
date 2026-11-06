// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

use std::io;

use crate::wire::invalid;
use crate::*;

fn os_bytes(bytes: &[u8], nonempty: bool) -> bool {
    bytes.len() <= MAX_FIELD && (!nonempty || !bytes.is_empty()) && !bytes.contains(&0)
}
fn absolute(bytes: &[u8]) -> bool {
    os_bytes(bytes, true) && bytes.starts_with(b"/")
}
impl Request {
    /// Validate syntax only; every descriptor and producer still needs native authentication.
    pub fn validate(&self) -> io::Result<()> {
        self.binding.validate()?;
        if self.member_attempt == [0; 16]
            || self.member_attempt == self.binding.root_attempt
            || self.timeout_ns == Some(0)
        {
            return Err(invalid());
        }
        let c = &self.command;
        if !absolute(&c.program)
            || !absolute(&c.cwd)
            || c.args.len() > MAX_ITEMS
            || c.environment.len() > MAX_ITEMS
            || c.arg0.as_ref().is_some_and(|v| !os_bytes(v, true))
            || c.args.iter().any(|v| !os_bytes(v, false))
        {
            return Err(invalid());
        }
        let mut names = std::collections::BTreeSet::new();
        for (key, value) in &c.environment {
            if !os_bytes(key, true)
                || key.contains(&b'=')
                || !os_bytes(value, false)
                || !names.insert(key)
            {
                return Err(invalid());
            }
        }
        let ds = &self.delegations;
        if ds.fences.len() > MAX_FENCES {
            return Err(invalid());
        }
        let mut descriptors = ds.fences.clone();
        if let Some(cpu) = ds.cpu {
            descriptors.push(cpu);
        }
        match &ds.gnu {
            GnuTransport::None => {}
            GnuTransport::NumericPipe { read, write } => descriptors.extend([*read, *write]),
            GnuTransport::NamedFifo { locator, inode, read, write, .. } => {
                if !absolute(locator)
                    || *inode == 0
                    || std::str::from_utf8(locator)
                        .map_or(true, |v| v.chars().any(char::is_whitespace))
                {
                    return Err(invalid());
                }
                descriptors.extend([*read, *write]);
            }
        }
        let mut sources = std::collections::BTreeSet::new();
        let mut targets = std::collections::BTreeSet::new();
        for d in descriptors {
            if d.source < 5
                || d.target < 3
                || d.source > i32::MAX as u32
                || d.target > i32::MAX as u32
                || !sources.insert(d.source)
                || !targets.insert(d.target)
            {
                return Err(invalid());
            }
        }
        match self.role {
            Role::Compile if ds.cpu.is_none() || matches!(ds.gnu, GnuTransport::None) => {
                return Err(invalid());
            }
            Role::MetadataVersion
                if c.args.len() != 1 || !matches!(c.args[0].as_slice(), b"-vV" | b"--version") =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        Ok(())
    }
}

impl JobBinding {
    /// Validate private routing syntax, never native custody or permission.
    pub fn validate(&self) -> io::Result<()> {
        validate_session(&self.session)?;
        if self.root_attempt == [0; 16] || self.capability == [0; 32] {
            return Err(invalid());
        }
        Ok(())
    }
}

pub(crate) fn validate_session(value: &str) -> io::Result<()> {
    let session = value.strip_prefix("s-").ok_or_else(invalid)?;
    if session.is_empty()
        || session.len() > 62
        || !session.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(invalid());
    }
    Ok(())
}
