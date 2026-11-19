// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared app ownership follows loaded executable identities, never process names or arguments.

use aterm_sysprobe::identity::{CS_VALID, Identity, Voucher};
use aterm_sysprobe::kproc::ImageFile;
use aterm_vitals::model::{ProcSample, SharedRoot, TabRoot, attribute};

#[derive(Clone)]
pub(super) struct Images {
    image: fn(u32) -> Option<ImageFile>,
    start: fn(u32) -> Option<u64>,
    signature: fn(u32) -> Option<Identity>,
    version: fn(u32) -> Option<i32>,
    shell: fn(aterm_pty::ShellIdentity) -> Option<bool>,
    /// PID versions change on fork/reuse and exec. No process-name allow-list is involved.
    cache: std::collections::BTreeMap<(u32, i32), bool>,
    /// The owner's pid, pid version, start and image the cache was proved against. Where the
    /// owner is this process, they stand for its reads at every later call ([`Self::roots`]).
    own: Option<(u32, Option<i32>, Option<u64>, ImageFile)>,
}

impl Images {
    fn read_shell(&self, identity: aterm_pty::ShellIdentity) -> Option<bool> {
        aterm_sysprobe::lane::mark("shell_generation", u32::try_from(identity.pid).ok());
        (self.shell)(identity)
    }

    fn read_start(&self, pid: u32) -> Option<u64> {
        aterm_sysprobe::lane::mark("process_start", Some(pid));
        (self.start)(pid)
    }

    fn read_version(&self, pid: u32) -> Option<i32> {
        aterm_sysprobe::lane::mark("pid_version", Some(pid));
        (self.version)(pid)
    }

    fn read_image(&self, pid: u32) -> Option<ImageFile> {
        aterm_sysprobe::lane::mark("loaded_executable", Some(pid));
        (self.image)(pid)
    }

    fn read_signature(&self, pid: u32) -> Option<Identity> {
        aterm_sysprobe::lane::mark("code_identity", Some(pid));
        (self.signature)(pid)
    }

    pub(super) const INERT: Self = Self {
        image: |_| None,
        start: |_| None,
        signature: |_| None,
        version: |_| None,
        shell: |_| None,
        cache: std::collections::BTreeMap::new(),
        own: None,
    };
    pub(super) const NATIVE: Self = Self {
        image: loaded_image,
        start: |pid| {
            aterm_uds::process::read_process_start(i32::try_from(pid).ok()?).map(|s| s.boot.0)
        },
        signature: aterm_sysprobe::identity::identity,
        version: aterm_sysprobe::pid_version,
        shell: |identity| {
            #[cfg(windows)]
            {
                Some(identity.verify())
            }
            #[cfg(not(windows))]
            {
                Some(aterm_pty::process_birth(identity.pid)? == identity.birth?)
            }
        },
        cache: std::collections::BTreeMap::new(),
        own: None,
    };

    /// Check the recorded shell generation after the process sweep. A stale PID cannot claim
    /// its replacement's subtree, while descendants already proved in earlier sweeps keep their
    /// own origins. Unknown proof is a lower bound, never a new zero-work tab.
    pub(super) fn tab_roots(
        &self,
        tabs: &mut [TabRoot],
        identities: &[(u64, aterm_pty::ShellIdentity)],
        present: &std::collections::BTreeSet<u32>,
        complete: bool,
    ) -> Vec<u64> {
        let mut unproved = Vec::new();
        for root in tabs {
            let Some((_, identity)) = identities.iter().find(|(key, _)| *key == root.key) else {
                continue;
            };
            if root.shell_pid == 0 {
                continue;
            }
            let proof = (u32::try_from(identity.pid).ok() == Some(root.shell_pid))
                .then(|| self.read_shell(*identity))
                .flatten();
            if proof != Some(true) {
                if proof.is_none() && (!complete || present.contains(&root.shell_pid)) {
                    unproved.push(root.key);
                }
                root.shell_pid = 0;
            }
        }
        unproved
    }

    /// A same-image or validly signed copy outside this instance's known trees is shared app work.
    /// Recheck start and loaded image around the read: PID reuse or an intervening exec supplies
    /// no proof. An installed update replacing the image pathname does not change either inode.
    /// A generation the cache holds unowned is read no further: no read after it could prove it
    /// shared, and its entry would be put back unchanged. The cache keeps the generations of the
    /// call's rows outside this instance's trees, each by its sweep version or, where the sweep
    /// read none, the version read here, and drops every other. A kept verdict stands for its
    /// generation while it lives: the image and signature it was judged by are taken to stay
    /// while the pid version stays, and macOS can clear a running process's `CS_VALID` with no
    /// exec, which a kept verdict does not see. Where `self_pid` is this process, its own start,
    /// pid version and image are kept from the first call whose image read and start or version
    /// read answered, and not read again around the read: none of them moves but by an exec,
    /// which runs this code from its start again. With none read again, no failed read of them
    /// empties a later call's roots or clears the cache. The owner's signature is read at most
    /// once a call, and only once a row is read that the owner's image under a pid version does
    /// not prove; another process's is read only where it can decide its row: never where the
    /// owner's is not one a copy could share ([`trusted`]), nor for a row the owner's image under
    /// a pid version proves.
    pub(super) fn roots(
        &mut self,
        self_pid: u32,
        procs: &mut [ProcSample],
        tabs: &[TabRoot],
    ) -> Vec<SharedRoot> {
        let kept = self
            .own
            .filter(|own| own.0 == self_pid && self_pid == std::process::id());
        let (own_version, own_start, own) = match kept {
            Some((_, version, start, image)) => (version, start, image),
            None => {
                let own_start = self.read_start(self_pid);
                let own_version = self.read_version(self_pid);
                let Some(own) = self.read_image(self_pid) else {
                    return Vec::new();
                };
                if own_start.is_none() && own_version.is_none() {
                    return Vec::new();
                }
                (own_version, own_start, own)
            }
        };
        let epoch = (self_pid, own_version, own_start, own);
        if self.own != Some(epoch) {
            self.cache.clear();
            self.own = Some(epoch);
        }
        let mut own_signature: Option<Option<Identity>> = None;
        let membership = attribute(procs, self_pid, tabs);
        // The generations of this call's rows outside this instance's trees, each by its sweep
        // version or the one read here.
        let mut present = std::collections::BTreeSet::new();
        let roots = procs
            .iter_mut()
            .zip(membership)
            .filter_map(|(p, member)| {
                if member.aterm {
                    return None;
                }
                let version = p.version.or_else(|| self.read_version(p.pid));
                if let Some(version) = version {
                    present.insert((p.pid, version));
                }
                let start = if version.is_some() {
                    p.start
                } else {
                    self.read_start(p.pid)
                };
                if start.is_none() && version.is_none() {
                    return None;
                }
                if p.start.is_some() && p.start != start {
                    return None;
                }
                let cached = version.and_then(|v| self.cache.get(&(p.pid, v)).copied());
                if cached == Some(false) {
                    return None;
                }
                let owned = cached.unwrap_or_else(|| {
                    let image = self.read_image(p.pid);
                    let same_image = image == Some(own);
                    let signer = if same_image && version.is_some() {
                        None
                    } else {
                        own_signature
                            .get_or_insert_with(|| self.read_signature(self_pid))
                            .as_ref()
                            .filter(|own| trusted(own))
                    };
                    let signed = || {
                        signer.is_some_and(|own| {
                            self.read_signature(p.pid)
                                .is_some_and(|other| same_signature(own, &other))
                        })
                    };
                    let signed_first = signed();
                    (same_image || signed_first)
                        && (version.is_some()
                            || (self.read_image(p.pid) == image && (!signed_first || signed())))
                });
                let stable = if let Some(version) = version {
                    self.read_version(p.pid) == Some(version)
                } else {
                    self.read_start(p.pid) == start
                };
                if stable && let Some(version) = version {
                    self.cache.insert((p.pid, version), owned);
                }
                let proven = owned && stable;
                if proven {
                    // The bulk task reader can refuse another user's start while the public
                    // kernel start record answers. Keep that generation proof in this sweep too.
                    p.start = start;
                    p.version = version;
                    Some(SharedRoot {
                        pid: p.pid,
                        start,
                        version,
                    })
                } else {
                    None
                }
            })
            .collect();
        self.cache.retain(|key, _| present.contains(key));
        if self_pid == std::process::id()
            || (self.read_start(self_pid) == own_start
                && self.read_version(self_pid) == own_version
                && self.read_image(self_pid) == Some(own))
        {
            roots
        } else {
            Vec::new()
        }
    }
}

fn loaded_image(pid: u32) -> Option<ImageFile> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        aterm_uds::process::LoadedImage::read(i32::try_from(pid).ok()?)?.file_identity()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        aterm_sysprobe::kproc::image_file(pid)
    }
}

/// A signature another process's can match ([`same_signature`]): valid, vouched for by a
/// Developer ID, development, enterprise or App Store certificate, with a signing identifier and
/// a team.
fn trusted(id: &Identity) -> bool {
    id.cs_status.is_some_and(|flags| flags & CS_VALID != 0)
        && matches!(
            id.voucher(),
            Voucher::DeveloperId | Voucher::Development | Voucher::Enterprise | Voucher::AppStore
        )
        && id.signing_id.as_ref().is_some_and(|s| !s.is_empty())
        && id.team.as_ref().is_some_and(|s| !s.is_empty())
}

fn same_signature(own: &Identity, other: &Identity) -> bool {
    trusted(own) && trusted(other) && own.signing_id == other.signing_id && own.team == other.team
}

#[cfg(test)]
#[path = "vitals_ownership_tests.rs"]
mod tests;
