// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this process runs ELEVATED: on Windows, with an administrator's full
//! token (`TokenElevation`), the token a "Run as administrator" launch gets from
//! UAC. Never off Windows.
//!
//! It decides where the control socket lives ([`crate::control_socket_dir`]): an
//! elevated instance gets a directory of its own, so it never shares the `latest`
//! alias, a token or a socket with the unelevated instances of the same user
//! (`docs/WINDOWS_PARAGON_DESIGN.md` §5.7, the elevation law). The same user's
//! unelevated processes run with the Administrators group as deny-only, which is
//! what lets that directory's Administrators-only DACL exclude them.

/// Whether this process's token is elevated, read once and kept: a process's
/// token does not change elevation while it runs.
///
/// A token that cannot be read counts as elevated. That is the side that fails
/// closed: an elevated directory an unelevated process cannot provision leaves
/// it without a control socket, where the other mistake would put an elevated
/// instance's socket where unelevated processes reach it.
#[must_use]
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        static ELEVATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ELEVATED.get_or_init(|| imp::token_is_elevated().unwrap_or(true))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
mod imp {
    use crate::win::ffi;

    /// `TOKEN_INFORMATION_CLASS::TokenElevation`: a `TOKEN_ELEVATION`, one
    /// `DWORD` that is nonzero for an elevated token.
    const TOKEN_ELEVATION_CLASS: u32 = 20;
    const TOKEN_QUERY: u32 = 0x0008;

    /// This process token's `TokenIsElevated`, or `None` when it cannot be read.
    pub(super) fn token_is_elevated() -> Option<bool> {
        let mut token: ffi::RawHandle = 0;
        // SAFETY: `GetCurrentProcess` is a pseudo-handle, always valid; the out
        // pointer is a live local.
        if unsafe { ffi::OpenProcessToken(ffi::GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
        {
            return None;
        }
        let mut elevated: u32 = 0;
        let mut len: u32 = 0;
        // SAFETY: `token` is the live handle just opened; the buffer is one
        // `DWORD`, exactly `TOKEN_ELEVATION`'s size, and `len` a live local.
        let ok = unsafe {
            ffi::GetTokenInformation(
                token,
                TOKEN_ELEVATION_CLASS,
                (&raw mut elevated).cast(),
                std::mem::size_of::<u32>() as u32,
                &mut len,
            )
        };
        // SAFETY: `token` is still the live handle OpenProcessToken returned.
        let _ = unsafe { ffi::CloseHandle(token) };
        (ok != 0).then_some(elevated != 0)
    }
}

#[cfg(all(test, windows))]
mod tests {
    /// The token query answers on every Windows host, and the cached answer is
    /// what it says. Which answer depends on how the test runs: an unelevated
    /// session reads `false`, an elevated one `true`.
    #[test]
    fn elevation_is_read_from_the_process_token() {
        let read = super::imp::token_is_elevated().expect("the process token is readable");
        assert_eq!(super::is_elevated(), read);
    }
}
