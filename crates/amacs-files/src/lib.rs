// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs's document I/O, shared by both faces: the native Editor view aterm-gui hosts, and the
//! terminal face.

#![deny(clippy::debug_assert_with_mut_call)]

#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

pub mod host;
pub mod io;
pub mod save_words;

/// A document store whose write capability a test mints: a test process that handles no untrusted
/// input is its own trusted launcher.
#[cfg(test)]
pub(crate) fn test_document_store() -> amacs::store::DocumentStore {
    // SAFETY: the `root_authority` contract is "trusted launcher, before untrusted input", which a
    // unit test satisfies.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    amacs::store::DocumentStore::new(authority.grant(aterm_cap::Tier::Trusted))
}

/// Creates a `.noindex` scratch directory for tests with a short UTF-8-safe prefix.
#[cfg(test)]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let end = tag.floor_char_boundary(24);
    let prefix = format!("amacs-files-{}-", &tag[..end]);
    aterm_tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".noindex")
        .tempdir()
        .expect("create .noindex scratch dir")
}

#[cfg(test)]
mod tests {
    /// NEGATIVE CONTROL: multi-byte UTF-8 tags straddling byte 24 must truncate at a valid char
    /// boundary without panicking and preserve the `.noindex` Spotlight-exclusion suffix.
    #[test]
    fn test_scratch_dir_truncates_utf8_tags_and_preserves_noindex_suffix() {
        let dir = crate::test_scratch_dir("aaaaaaaaaaaaaaaaaaaaaaaé-extra-tail");
        let name = dir
            .path()
            .file_name()
            .and_then(|os| os.to_str())
            .expect("scratch dir name is UTF-8");
        assert!(name.starts_with("amacs-files-aaaaaaaaaaaaaaaaaaaaaaa-"));
        assert!(name.ends_with(".noindex"));
    }
}
