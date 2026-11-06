// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

use std::ffi::{OsStr, OsString};
use std::io;

use crate::wire::invalid;
use crate::{CONTROL_FD, Delegations, GnuTransport, PRODUCER_FD};

/// Actual inherited socket-number locators, never resource authority. Production
/// uses held source numbers and child-only CLOEXEC clearing, avoiding dup2 onto
/// a runtime's private exec-status descriptors or a parent's live FD table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HelperFds {
    pub control: u32,
    pub producer: u32,
}
impl Default for HelperFds {
    fn default() -> Self {
        Self { control: CONTROL_FD, producer: PRODUCER_FD }
    }
}
impl HelperFds {
    pub fn validate(self) -> io::Result<()> {
        if self.control < 3
            || self.producer < 3
            || self.control == self.producer
            || self.control > i32::MAX as u32
            || self.producer > i32::MAX as u32
        {
            return Err(invalid());
        }
        Ok(())
    }
    /// Recognize only this private entry. Invalid recognized shapes refuse,
    /// while unrelated CLI entries return None without changing their routing.
    pub fn parse(args: &[OsString]) -> io::Result<Option<Self>> {
        if args.first().map(OsString::as_os_str) != Some(OsStr::new("__resource-member-v3")) {
            return Ok(None);
        }
        if args.len() != 4 || args[1] != "--protocol=3" {
            return Err(invalid());
        }
        fn number(arg: &OsStr, prefix: &str) -> io::Result<u32> {
            let text = arg.to_str().and_then(|v| v.strip_prefix(prefix)).ok_or_else(invalid)?;
            let value: u32 = text.parse().map_err(|_| invalid())?;
            if value.to_string() != text {
                return Err(invalid());
            }
            Ok(value)
        }
        let fds = Self {
            control: number(&args[2], "--control-fd=")?,
            producer: number(&args[3], "--producer-fd=")?,
        };
        fds.validate()?;
        Ok(Some(fds))
    }
    pub fn arguments(self) -> io::Result<Vec<OsString>> {
        self.validate()?;
        Ok(vec![
            "__resource-member-v3".into(),
            "--protocol=3".into(),
            format!("--control-fd={}", self.control).into(),
            format!("--producer-fd={}", self.producer).into(),
        ])
    }
    /// Check BEFORE descriptor duplication, which could reuse an advertised
    /// closed slot. Target numbers belong to a separate namespace.
    pub fn validate_delegations(self, delegations: &Delegations) -> io::Result<()> {
        self.validate()?;
        let mut sources = delegations.fences.iter().map(|d| d.source).collect::<Vec<_>>();
        if let Some(cpu) = delegations.cpu {
            sources.push(cpu.source);
        }
        match delegations.gnu {
            GnuTransport::None => {}
            GnuTransport::NumericPipe { read, write }
            | GnuTransport::NamedFifo { read, write, .. } => {
                sources.extend([read.source, write.source])
            }
        }
        if sources.into_iter().any(|fd| fd == self.control || fd == self.producer) {
            return Err(invalid());
        }
        Ok(())
    }
}
