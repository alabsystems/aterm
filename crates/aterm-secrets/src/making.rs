// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a process making a vault identity knows of itself, read now ([`crate::Making::now`]): the
//! facts [`crate::lab_identity`] marks a lab identity by (A12), on Unix and on Windows alike.

use std::path::Path;

/// The environment variable that points the members browse (`discover::browse_members`) at a
/// lab's own members service in place of the ALab's: `lab mac nux --scenario vault-phrase` sets it
/// on its joiner, whose guest hears the members the lab Mac's LAN advertises and must never ask
/// them. Which Bonjour type a Mac browses is no credential (whoever answers still proves who it
/// is), but this build takes only a lab service there (`discover::lab_service`) and fails the
/// browse on any other value (`discover::member_service`), so a value it cannot take never falls
/// back to the real members. No install sets it. Set, whatever its value, it marks the process a
/// lab lane's ([`crate::Making::lab_lane_of`]).
pub const LAB_MEMBER_SERVICE_ENV: &str = "ATERM_LAB_MEMBER_SERVICE";

/// The environment variable that points the client's vault look at a lab's own vault service in
/// place of the ALab's (`discover` names each browse it changes): the look then asks no
/// compiled-in default vault, and names only what its lab browse heard. This changes a browse's
/// target, without starting a browse or admitting an automatic default-vault join: a lab
/// identity is still refused before that join looks (A12). As with
/// [`LAB_MEMBER_SERVICE_ENV`], this build takes only a lab service here, so a value it cannot take
/// browses for no vault, and the variable set marks the process a lab's
/// ([`crate::Making::lab_lane_of`]). A vault server's own browse and advert keep
/// `_alab-vault._tcp` whatever it says. No install sets it.
pub const LAB_VAULT_SERVICE_ENV: &str = "ATERM_LAB_VAULT_SERVICE";

impl crate::Making {
    /// What this process is, read now ([`crate::lab_identity`]): a test binary by its executable's
    /// name (`aterm_uds::test_binary::is_test_binary`), a build with debug assertions on, a
    /// process a lab lane marked ([`Self::lab_lane_of`]: `ATERM_LAB_MEMBER_SERVICE` or
    /// `ATERM_LAB_VAULT_SERVICE` set, whatever its value),
    /// whether it runs inside a virtual machine ([`aterm_sysprobe::vm_guest`]: macOS's
    /// `kern.hv_vmm_present`; on Windows the firmware's names and a guest hypervisor's CPUID
    /// signature, never the hypervisor bit alone or `Microsoft Hv`, which a PC running
    /// virtualization-based security reads of itself on bare metal), and whether its home
    /// is a folder other than its login's account home ([`Self::foreign_home_now`]), as
    /// [`Self::reading`] reads them.
    #[must_use]
    pub fn now() -> Self {
        Self::reading(
            std::env::current_exe().is_ok_and(|exe| aterm_uds::test_binary::is_test_binary(&exe)),
            aterm_sysprobe::vm_guest,
            Self::foreign_home_now,
        )
    }

    /// Whether this process's home (`HOME`, as [`aterm_types::dirs::home_dir`] reads it; on
    /// Windows `USERPROFILE`) is a folder other than the one the account names for its user (the
    /// account database's on Unix; on Windows the profile folder the shell knows the user by,
    /// `FOLDERID_Profile`, which no environment variable moves): the process was handed a scratch
    /// home. `None` where either home is unresolved.
    pub(crate) fn foreign_home_now() -> Option<bool> {
        #[cfg(any(unix, windows))]
        let account = aterm_uds::account::home();
        #[cfg(not(any(unix, windows)))]
        let account = None;
        Self::foreign_home_of(aterm_types::dirs::home_dir().as_deref(), account.as_deref())
    }

    /// [`Self::foreign_home_now`] for a process whose home is `home` and whose account home is
    /// `account`: whether they are two folders ([`one_folder`], so a symlinked or differently
    /// spelled home is the login's own), or `None` where either is unresolved.
    pub(crate) fn foreign_home_of(home: Option<&Path>, account: Option<&Path>) -> Option<bool> {
        Some(!one_folder(home?, account?))
    }

    /// Whether a lab lane marked a process whose environment has each variable `set` says it
    /// has: [`LAB_MEMBER_SERVICE_ENV`] or [`LAB_VAULT_SERVICE_ENV`], whatever its value (one this
    /// build cannot take browses for nothing, and still marks the process a lab's).
    pub(crate) fn lab_lane_of(set: impl Fn(&str) -> bool) -> bool {
        set(LAB_MEMBER_SERVICE_ENV) || set(LAB_VAULT_SERVICE_ENV)
    }

    /// [`Self::now`] for a process that is a test binary or not (`test_binary`), its
    /// virtual-machine fact read through `virtual_machine` and its home fact through
    /// `foreign_home`. A test binary, in any crate, reads no
    /// probe: a test states the fact through [`crate::Making`], so no test's outcome hangs on the
    /// host it runs on, a macOS VM included, and every identity a test binary makes is marked
    /// [`crate::LabMark::TestBinary`] first anyway. A probe that does not answer (Linux, a
    /// kernel with no such name, a firmware that names nothing; an account home the database does
    /// not name) is no virtual machine and no foreign home, so a machine's own hardware never makes
    /// a lab identity because a probe failed.
    pub(crate) fn reading(
        test_binary: bool,
        virtual_machine: fn() -> Option<bool>,
        foreign_home: fn() -> Option<bool>,
    ) -> Self {
        Self {
            test_binary,
            debug_build: cfg!(debug_assertions),
            lab_lane: Self::lab_lane_of(|knob| std::env::var_os(knob).is_some()),
            virtual_machine: !test_binary && virtual_machine() == Some(true),
            foreign_home: !test_binary && foreign_home() == Some(true),
        }
    }
}

/// Whether `a` and `b` name one folder: one device and inode where both can be read (so a
/// symlinked or differently spelled home is the same one), else one canonical path (on Windows
/// always: its canonical path resolves a junction and a symlink, and a short or differently cased
/// spelling, to the folder's own).
pub(crate) fn one_folder(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return (a.dev(), a.ino()) == (b.dev(), b.ino());
        }
    }
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(a) == canonical(b)
}

#[cfg(test)]
#[path = "making_tests.rs"]
mod tests;
