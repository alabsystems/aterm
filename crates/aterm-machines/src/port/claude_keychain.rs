// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, noninteractive reads of Claude's exact private staging Keychain item.

use aterm_secrets::SecretBytes;
use std::path::Path;
use std::time::Instant;

#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
mod pipe;

const CHILD: &str = "--aterm-claude-stage-keychain-v1";
const PREPARE_CHILD: &str = "--aterm-claude-stage-keychain-prepare-v1";
#[cfg(any(test, debug_assertions))]
const TEST_CHILD: &str = "--aterm-test-claude-stage-keychain-v1";
#[cfg(any(test, debug_assertions))]
const TEST_DIAGNOSTIC_CHILD: &str = "--aterm-test-claude-stage-keychain-diagnostic-v1";
#[cfg(any(test, debug_assertions))]
const TEST_SEED_CHILD: &str = "--aterm-test-claude-stage-keychain-seed-v1";
#[cfg(any(test, debug_assertions))]
const TEST_PREPARE_CHILD: &str = "--aterm-test-claude-stage-keychain-prepare-v1";
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
const TEST_KEYCHAIN: &str = "synthetic.keychain-db";
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
const TEST_KEYCHAIN_DIR: &str = ".synthetic-keychain";
#[cfg(target_os = "macos")]
const MAX_BYTES: usize = 64 * 1024;

/// A stage snapshot was unavailable; none of these outcomes permits a file fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Not an owned private Claude staging directory, or it changed during the read.
    Refused,
    /// Keychain would require an interaction; this reader never requests one.
    Interaction,
    /// The exact service matches more than one item.
    Ambiguous,
    /// The native API or helper failed without a complete answer.
    Unavailable,
    /// The caller's overall deadline elapsed.
    TimedOut,
    /// The item exceeded the bounded complete capture.
    TooLarge,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Refused => "Claude staging scope is unavailable or changed",
            Self::Interaction => "Claude staging Keychain item is unavailable without interaction",
            Self::Ambiguous => "Claude staging Keychain service is ambiguous",
            Self::Unavailable => "Claude staging Keychain snapshot is unavailable",
            Self::TimedOut => "Claude staging Keychain snapshot exceeded its deadline",
            Self::TooLarge => "Claude staging Keychain item exceeds the capture bound",
        })
    }
}

impl std::error::Error for ReadError {}

/// Read the sole item belonging to this exact Claude staging path, without UI.
/// `None` means a native not-found result, never a denied read.
/// Filesystem validation and the native call both run in a supervised child under
/// `deadline`; returned bytes have no printable representation and are wiped on drop.
///
/// # Errors
/// [`ReadError`] for an invalid stage, multiple items, interaction, timeout, oversized
/// data or an incomplete helper response. The caller must retain its pending proof.
pub fn read_claude_stage(
    stage: &Path,
    deadline: Instant,
) -> Result<Option<SecretBytes>, ReadError> {
    stage_text(stage)?;
    if Instant::now() >= deadline {
        return Err(ReadError::TimedOut);
    }
    #[cfg(target_os = "macos")]
    {
        let program = std::env::current_exe().map_err(|_| ReadError::Unavailable)?;
        pipe::read(&program, stage, deadline)
    }
    #[cfg(not(target_os = "macos"))]
    Err(ReadError::Unavailable)
}

/// Provision only this owned stage's missing native item. A private durable
/// marker permits retry without changing any existing item's data or access.
///
/// # Errors
/// Refuses an invalid stage, unowned existing item, changed marker, unavailable
/// native access or the shared deadline. No UI or arbitrary service is accepted.
pub fn prepare_claude_stage(stage: &Path, deadline: Instant) -> Result<(), ReadError> {
    stage_text(stage)?;
    #[cfg(target_os = "macos")]
    {
        let program = std::env::current_exe().map_err(|_| ReadError::Unavailable)?;
        prepared(pipe::read_mode(&program, stage, deadline, PREPARE_CHILD)?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = deadline;
        Err(ReadError::Unavailable)
    }
}

#[cfg(target_os = "macos")]
fn prepared(result: Option<SecretBytes>) -> Result<(), ReadError> {
    if result.is_some_and(|bytes| bytes.as_slice() == b"ready") {
        Ok(())
    } else {
        Err(ReadError::Unavailable)
    }
}

/// The executable's private child route, before GUI or account initialization.
/// It accepts only a staged directory, and writes bytes only to an anonymous pipe.
#[must_use]
pub fn entry() -> Option<std::process::ExitCode> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next();
    let preparing = mode.as_deref() == Some(std::ffi::OsStr::new(PREPARE_CHILD));
    #[cfg(any(test, debug_assertions))]
    let test = mode.as_deref() == Some(std::ffi::OsStr::new(TEST_CHILD));
    #[cfg(any(test, debug_assertions))]
    let diagnostic = mode.as_deref() == Some(std::ffi::OsStr::new(TEST_DIAGNOSTIC_CHILD));
    #[cfg(any(test, debug_assertions))]
    let seed = mode.as_deref() == Some(std::ffi::OsStr::new(TEST_SEED_CHILD));
    #[cfg(any(test, debug_assertions))]
    let test_preparing = mode.as_deref() == Some(std::ffi::OsStr::new(TEST_PREPARE_CHILD));
    #[cfg(not(any(test, debug_assertions)))]
    let (test, diagnostic, seed, test_preparing) = (false, false, false, false);
    if mode.as_deref() != Some(std::ffi::OsStr::new(CHILD))
        && !preparing
        && !test
        && !diagnostic
        && !seed
        && !test_preparing
    {
        return None;
    }
    let stage = args.next();
    if args.next().is_some() {
        return Some(std::process::ExitCode::from(code(ReadError::Refused)));
    }
    #[cfg(target_os = "macos")]
    let answer = stage
        .as_deref()
        .ok_or(ReadError::Refused)
        .and_then(|stage| {
            #[cfg(any(test, debug_assertions))]
            if test_preparing {
                return native::test_prepare_answer(Path::new(stage));
            }
            if preparing {
                return native::prepare_answer(Path::new(stage));
            }
            #[cfg(any(test, debug_assertions))]
            if seed {
                return native::test_seed_answer(Path::new(stage));
            }
            #[cfg(any(test, debug_assertions))]
            if diagnostic {
                return native::test_diagnostic_answer(Path::new(stage));
            }
            #[cfg(any(test, debug_assertions))]
            if test {
                return native::test_answer(Path::new(stage));
            }
            native::answer(Path::new(stage))
        });
    #[cfg(not(target_os = "macos"))]
    let answer: Result<Option<SecretBytes>, ReadError> = {
        let _ = stage;
        Err(ReadError::Unavailable)
    };
    Some(std::process::ExitCode::from(match answer {
        Ok(Some(bytes)) => {
            use std::io::Write as _;
            match std::io::stdout().lock().write_all(bytes.as_slice()) {
                Ok(()) => 0,
                Err(_) => code(ReadError::Unavailable),
            }
        }
        Ok(None) => 10,
        Err(error) => code(error),
    }))
}

/// Exercise the real child against only `synthetic.keychain-db` in a private stage.
/// This seam never changes the normal Keychain search list.
///
/// # Errors
/// The same bounded, private-scope and native read errors as [`read_claude_stage`].
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub fn test_read_private_keychain(
    program: &Path,
    stage: &Path,
    deadline: Instant,
) -> Result<Option<SecretBytes>, ReadError> {
    stage_text(stage)?;
    pipe::read_mode(program, stage, deadline, TEST_CHILD)
}

/// The exact vendor service and account for a synthetic stage fixture.
///
/// # Errors
/// The staging path is invalid or cannot be normalized.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub fn test_vendor_scope(stage: &Path) -> Result<(String, String), ReadError> {
    native::test_vendor_scope(stage)
}

/// Return only native status and result-shape metadata for the synthetic query.
///
/// # Errors
/// The stage, private synthetic keychain or bounded helper is unavailable.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub fn test_private_keychain_diagnostic(
    program: &Path,
    stage: &Path,
    deadline: Instant,
) -> Result<String, ReadError> {
    stage_text(stage)?;
    let bytes = pipe::read_mode(program, stage, deadline, TEST_DIAGNOSTIC_CHILD)?
        .ok_or(ReadError::Unavailable)?;
    std::str::from_utf8(bytes.as_slice())
        .map(str::to_owned)
        .map_err(|_| ReadError::Unavailable)
}

/// Create only the fixture's non-authentication item, trusting this helper and
/// the native security writer. Existing items are never updated by this seam.
///
/// # Errors
/// The private fixture, native access object or bounded helper is unavailable.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub fn test_seed_private_keychain(
    program: &Path,
    stage: &Path,
    deadline: Instant,
) -> Result<(), ReadError> {
    stage_text(stage)?;
    let result = pipe::read_mode(program, stage, deadline, TEST_SEED_CHILD)?;
    if result.is_some_and(|result| result.as_slice() == b"created") {
        Ok(())
    } else {
        Err(ReadError::Unavailable)
    }
}

/// Exercise the real create/resume protocol against only the synthetic keychain.
///
/// # Errors
/// The same private ownership, native and deadline refusals as the provisioner.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
pub fn test_prepare_private_keychain(
    program: &Path,
    stage: &Path,
    deadline: Instant,
) -> Result<(), ReadError> {
    stage_text(stage)?;
    prepared(pipe::read_mode(
        program,
        stage,
        deadline,
        TEST_PREPARE_CHILD,
    )?)
}

fn code(error: ReadError) -> u8 {
    match error {
        ReadError::Refused => 11,
        ReadError::Interaction => 12,
        ReadError::Ambiguous => 13,
        ReadError::Unavailable => 14,
        ReadError::TimedOut => 15,
        ReadError::TooLarge => 16,
    }
}

fn stage_text(stage: &Path) -> Result<&str, ReadError> {
    let text = stage.to_str().ok_or(ReadError::Refused)?;
    let parent = stage.parent().ok_or(ReadError::Refused)?;
    let leaf = stage
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or(ReadError::Refused)?;
    let (label, number) = leaf.rsplit_once('+').ok_or(ReadError::Refused)?;
    let number_value = crate::parse_uint::<u16>(number).ok_or(ReadError::Refused)?;
    if !stage.is_absolute()
        || parent.file_name() != Some(std::ffi::OsStr::new("logins"))
        || label.is_empty()
        || label.len() > 64
        || !label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || !(1..=9999).contains(&number_value)
        || number_value.to_string() != number
    {
        return Err(ReadError::Refused);
    }
    Ok(text)
}

#[cfg(any(target_os = "macos", test))]
fn service(text: &str) -> String {
    let digest = aterm_secrets::materialize::digest_hex(text.as_bytes());
    format!("Claude Code-credentials-{}", &digest[..8])
}

#[cfg(test)]
mod tests;
