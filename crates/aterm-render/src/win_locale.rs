// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which Han face a Windows user reads first.
//!
//! Japanese and Chinese share their ideographs' code points and differ in the drawn forms, so the
//! fallback chain has to put one script's face ahead of the other's. Microsoft YaHei maps the
//! kana and the ideographs everyday text in either script uses, so it draws a line of either
//! script in one face and leads unless the user's languages name Japanese ahead of Chinese; then
//! Yu Gothic leads and gives kana and kanji their Japanese forms.

/// Whether `languages` (BCP 47 names, most preferred first) name Japanese ahead of Chinese. A list
/// that names neither does not.
pub(crate) fn names_japanese_first<'a>(languages: impl IntoIterator<Item = &'a str>) -> bool {
    languages
        .into_iter()
        .filter_map(|name| name.split(['-', '_']).next())
        .find_map(|primary| {
            if primary.eq_ignore_ascii_case("ja") {
                Some(true)
            } else if primary.eq_ignore_ascii_case("zh") {
                Some(false)
            } else {
                None
            }
        })
        .unwrap_or(false)
}

/// [`names_japanese_first`] over this user's display languages, then the languages their profile
/// lists (where a language added for reading or typing alone stands), then their regional format;
/// read once per process.
pub(crate) fn user_reads_japanese_first() -> bool {
    static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READ.get_or_init(|| {
        let mut names = sys::preferred_ui_languages();
        names.extend(sys::profile_languages());
        names.extend(sys::default_locale_name());
        names_japanese_first(names.iter().map(String::as_str))
    })
}

/// The names of a NUL-separated UTF-16 list, in its order.
fn names_of(list: &[u16]) -> Vec<String> {
    list.split(|&unit| unit == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// The kernel32 and advapi32 calls, declared directly as `font_file`'s are.
mod sys {
    /// `MUI_LANGUAGE_NAME`: the list is spelled as language names (`ja-JP`), not hexadecimal ids.
    const MUI_LANGUAGE_NAME: u32 = 0x8;
    /// `LOCALE_NAME_MAX_LENGTH`, in UTF-16 units with the terminator.
    const LOCALE_NAME_MAX_LENGTH: usize = 85;
    /// The most UTF-16 units a language list is read into; a longer list reads as empty.
    const LANGUAGES_MAX_UNITS: u32 = 4096;
    /// `HKEY_CURRENT_USER`, the predefined handle (sign-extended to pointer width).
    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    /// `RRF_RT_REG_MULTI_SZ`: the value is read only as a list of strings.
    const RRF_RT_REG_MULTI_SZ: u32 = 0x20;

    // SAFETY: standard kernel32 declarations, signatures per the Win32 docs.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserPreferredUILanguages(
            flags: u32,
            languages: *mut u32,
            buffer: *mut u16,
            buffer_units: *mut u32,
        ) -> i32;
        fn GetUserDefaultLocaleName(name: *mut u16, name_units: i32) -> i32;
    }

    // SAFETY: the standard advapi32 declaration, signature per the Win32 docs.
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            data_bytes: *mut u32,
        ) -> i32;
    }

    /// The user's display languages, most preferred first; empty where the call fails.
    pub(super) fn preferred_ui_languages() -> Vec<String> {
        let mut count = 0u32;
        let mut units = 0u32;
        // SAFETY: a null buffer with a zero length asks only for the length, which the call writes to `units`; `count` and `units` are writable.
        let sized = unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                std::ptr::null_mut(),
                &mut units,
            )
        };
        if sized == 0 || units == 0 || units > LANGUAGES_MAX_UNITS {
            return Vec::new();
        }
        let mut buffer = vec![0u16; units as usize];
        // SAFETY: `buffer` holds `units` writable UTF-16 units, the length the call is told; `count` and `units` are writable.
        let read = unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                buffer.as_mut_ptr(),
                &mut units,
            )
        };
        if read == 0 {
            return Vec::new();
        }
        super::names_of(&buffer)
    }

    /// The languages the user's profile lists (Settings, Language, Preferred languages), most
    /// preferred first: `Languages` under `HKCU\Control Panel\International\User Profile`. Empty
    /// where the value is absent, is not a string list or is longer than a language list gets.
    pub(super) fn profile_languages() -> Vec<String> {
        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
        let key = wide("Control Panel\\International\\User Profile");
        let value = wide("Languages");
        let mut bytes = 0u32;
        // SAFETY: `key` and `value` are NUL-terminated and outlive the call; a null data pointer asks only for the size, which the call writes to `bytes`.
        let sized = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_MULTI_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if sized != 0 || bytes == 0 || bytes > LANGUAGES_MAX_UNITS * 2 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; (bytes as usize).div_ceil(2)];
        let mut room = bytes;
        // SAFETY: `buffer` holds at least `room` writable bytes, the size the call is told; `key` and `value` are NUL-terminated and outlive the call.
        let read = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_MULTI_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut room,
            )
        };
        if read != 0 {
            return Vec::new();
        }
        buffer.truncate((room as usize / 2).min(buffer.len()));
        super::names_of(&buffer)
    }

    /// The user's regional format (`en-US`), or `None` where the call fails.
    pub(super) fn default_locale_name() -> Option<String> {
        let mut buffer = [0u16; LOCALE_NAME_MAX_LENGTH];
        // SAFETY: `buffer` holds `LOCALE_NAME_MAX_LENGTH` writable UTF-16 units, the length the call is told.
        let units =
            unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), LOCALE_NAME_MAX_LENGTH as i32) };
        let len = usize::try_from(units).ok()?.checked_sub(1)?;
        Some(String::from_utf16_lossy(buffer.get(..len)?))
    }
}

#[cfg(test)]
#[path = "win_locale_tests.rs"]
mod tests;
