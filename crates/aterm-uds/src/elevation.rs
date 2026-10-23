// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process token elevation check for [`crate::control_socket_dir`].

/// Whether this process's token is elevated, read once and kept.
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

    /// `TOKEN_INFORMATION_CLASS::TokenElevation`.
    const TOKEN_ELEVATION_CLASS: u32 = 20;
    const TOKEN_QUERY: u32 = 0x0008;

    /// This process token's `TokenIsElevated`, or `None` when it cannot be read.
    pub(super) fn token_is_elevated() -> Option<bool> {
        let mut token: ffi::RawHandle = 0;
        // SAFETY: `GetCurrentProcess` is a pseudo-handle, always valid; the out pointer is a live
        // local.
        if unsafe { ffi::OpenProcessToken(ffi::GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
        {
            return None;
        }
        let mut elevated: u32 = 0;
        let mut len: u32 = 0;
        // SAFETY: `token` is the live handle just opened; the buffer is one `DWORD`, exactly
        // `TOKEN_ELEVATION`'s size, and `len` a live local.
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
