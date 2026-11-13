// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact descriptor roles carried by a controlled native window launch.

use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd, RawFd};

pub(crate) struct MemoryGuard {
    pub descriptor: OwnedFd,
    pub claim: String,
}

/// An observation of the original admitted record, never a new memory grant.
pub(crate) struct MemoryObservation {
    pub claim: String,
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub pid: u32,
    pub start_ms: u64,
    pub sequence: u64,
}

pub(crate) struct WindowGuards {
    pub lane: OwnedFd,
    pub memory: Option<MemoryGuard>,
}

impl WindowGuards {
    pub fn from_original(
        lane: OwnedFd,
        memory: Option<&aterm_capacity::RetainedClaim>,
    ) -> Result<Self, String> {
        let memory = memory
            .map(|memory| {
                let claim = memory
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("original memory claim has no lossless leaf")?
                    .to_owned();
                Self::arguments(&[3, 4], Some(&claim))?;
                Ok::<_, String>(MemoryGuard {
                    descriptor: memory.try_clone_fd().map_err(|why| why.to_string())?,
                    claim,
                })
            })
            .transpose()?;
        Ok(Self { lane, memory })
    }

    pub fn claim(&self) -> Option<&str> {
        self.memory.as_ref().map(|memory| memory.claim.as_str())
    }

    pub fn borrowed(&self) -> Vec<BorrowedFd<'_>> {
        let mut descriptors = vec![self.lane.as_fd()];
        if let Some(memory) = &self.memory {
            descriptors.push(memory.descriptor.as_fd());
        }
        descriptors
    }

    pub fn into_descriptors(self) -> Vec<OwnedFd> {
        let mut descriptors = vec![self.lane];
        if let Some(memory) = self.memory {
            descriptors.push(memory.descriptor);
        }
        descriptors
    }

    pub fn from_received(
        mut descriptors: Vec<OwnedFd>,
        claim: Option<String>,
    ) -> Result<Self, String> {
        if descriptors.len() != if claim.is_some() { 2 } else { 1 } {
            return Err("window handoff has the wrong exact descriptor roles".into());
        }
        let memory = claim.map(|claim| MemoryGuard {
            descriptor: descriptors.pop().expect("memory role count was checked"),
            claim,
        });
        Ok(Self {
            lane: descriptors.pop().expect("lane role count was checked"),
            memory,
        })
    }

    pub fn arguments(slots: &[RawFd], claim: Option<&str>) -> Result<Vec<String>, String> {
        use aterm_types::lab_window_guard::{FLAG, MEMORY_FLAG, take};
        if slots.len() != if claim.is_some() { 2 } else { 1 } {
            return Err("window guard argument roles do not match native descriptor slots".into());
        }
        let mut words = vec![FLAG.to_owned(), slots[0].to_string()];
        if let Some(claim) = claim {
            words.extend([
                MEMORY_FLAG.to_owned(),
                slots[1].to_string(),
                claim.to_owned(),
            ]);
        }
        let mut checked = words.iter().map(std::ffi::OsString::from).collect();
        if take(&mut checked)?.is_none() || !checked.is_empty() {
            return Err("invalid exact window guard offer".into());
        }
        Ok(words)
    }
}

#[cfg(test)]
#[path = "window_guards_tests.rs"]
mod tests;
