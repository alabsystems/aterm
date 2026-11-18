//! One prospective fixed restriction, not an executable or lifetime issuer.

use std::ffi::CStr;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path};

use crate::{MAX_CLOSED_EXEC_IMAGES, MAX_CLOSED_EXEC_NAME_BYTES};

// The source and parameter names are fixed. Unused slots repeat the first
// accepted name; no caller byte is interpreted as policy source. Denying the
// conjunction of all nonmatches permits only this finite set of exec names.
#[cfg(target_os = "macos")]
const PROFILE: &CStr = c"(version 1)(allow default)(deny process-fork)(deny process-exec (require-all (require-not (literal (param \"IMAGE0\"))) (require-not (literal (param \"IMAGE1\"))) (require-not (literal (param \"IMAGE2\"))) (require-not (literal (param \"IMAGE3\"))) (require-not (literal (param \"IMAGE4\"))) (require-not (literal (param \"IMAGE5\"))) (require-not (literal (param \"IMAGE6\"))) (require-not (literal (param \"IMAGE7\"))) (require-not (literal (param \"IMAGE8\"))) (require-not (literal (param \"IMAGE9\"))) (require-not (literal (param \"IMAGE10\"))) (require-not (literal (param \"IMAGE11\"))) (require-not (literal (param \"IMAGE12\"))) (require-not (literal (param \"IMAGE13\"))) (require-not (literal (param \"IMAGE14\"))) (require-not (literal (param \"IMAGE15\")))))";
#[cfg(target_os = "macos")]
const KEYS: [&CStr; MAX_CLOSED_EXEC_IMAGES] = [
    c"IMAGE0", c"IMAGE1", c"IMAGE2", c"IMAGE3", c"IMAGE4", c"IMAGE5", c"IMAGE6", c"IMAGE7",
    c"IMAGE8", c"IMAGE9", c"IMAGE10", c"IMAGE11", c"IMAGE12", c"IMAGE13", c"IMAGE14", c"IMAGE15",
];

/// Restrict this process, before its initial payload exec, to no fork and exec
/// names in one bounded set. This returns neither an owner nor an installed
/// no-exec observation. The native policy remains irreversible on every error;
/// never retry, tighten an existing seal, or fall back to ordinary execution.
///
/// This is a NAME filter, not descriptor-based execution or chain completeness.
/// It does not restrict argv, environment, dynamic loading or non-FD services.
///
/// # Safety
/// The caller is the trusted single-threaded pre-exec role. All supplied names
/// must come from the SAME genuinely held original executable/runtime inputs,
/// already validated under their original deadline. A separate lifetime owner
/// must retain those actual inputs, namespace assumptions and original endpoint
/// across all execs through actual child settlement: self-exec destroys Rust
/// guards and CLOEXEC descriptors. The caller must prevent concurrent same-UID
/// name writers, preserve original environment/selection inputs, and own the
/// child/GO/reap/EOF protocol. Paths alone satisfy none of those obligations.
#[cfg(target_os = "macos")]
pub unsafe fn install_no_fork_closed_exec(images: &[&CStr]) -> io::Result<()> {
    crate::native_entry::single_main_thread()?;
    validate_closed_exec_names(images)?;
    // Existing NoFork must not be mistaken for permission to install a second
    // policy. Other existing/unsupported policies still fail at native install.
    if crate::fork_denied(std::process::id())? {
        return Err(io::Error::other(
            "closed exec policy requires its first native seal",
        ));
    }
    let mut parameters = Vec::with_capacity(2 * MAX_CLOSED_EXEC_IMAGES + 1);
    for (index, key) in KEYS.iter().enumerate() {
        parameters.push(key.as_ptr());
        parameters.push(images.get(index).unwrap_or(&images[0]).as_ptr());
    }
    parameters.push(std::ptr::null());
    crate::seal::install_parameterized_profile(PROFILE, &parameters)?;
    if !crate::fork_denied(std::process::id())? {
        return Err(io::Error::other(
            "closed exec fork restriction is unavailable",
        ));
    }
    crate::native_entry::single_main_thread()?;
    Ok(())
}

/// Validate bounded restrictive DATA only. Names do not create executable
/// ownership, permission to install policy, a child or namespace custody.
pub fn validate_closed_exec_names(images: &[&CStr]) -> io::Result<()> {
    if images.is_empty() || images.len() > MAX_CLOSED_EXEC_IMAGES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "closed exec image population exceeds bound",
        ));
    }
    for (index, image) in images.iter().enumerate() {
        let bytes = image.to_bytes();
        let path = Path::new(std::ffi::OsStr::from_bytes(bytes));
        if bytes.len() > MAX_CLOSED_EXEC_NAME_BYTES
            || !path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
            || path
                .components()
                .collect::<std::path::PathBuf>()
                .as_os_str()
                .as_bytes()
                != bytes
            || images[..index].contains(image)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "closed exec names are not distinct canonical bounded inputs",
            ));
        }
    }
    Ok(())
}
