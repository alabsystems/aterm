// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host `IOPlatformUUID` lookup via `ioreg`.

use crate::{
    hardware::{ioreg_value, uuid_shaped},
    port::{Cmd, System},
};

/// `ioreg -rd1 -c IOPlatformExpertDevice`: the platform's identity record.
#[must_use]
pub fn ioreg_cmd() -> Cmd {
    Cmd::new("/usr/sbin/ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"])
}

/// This Mac's IOPlatformUUID.
///
/// # Errors
/// One line when `ioreg` does not answer it.
pub fn platform_uuid(sys: &dyn System) -> Result<String, String> {
    let ran = sys.run(&ioreg_cmd()).map_err(|e| format!("ioreg: {e}"))?;
    if !ran.success() {
        return Err(format!("ioreg: {}", ran.describe()));
    }
    let uuid = ioreg_value(&ran.out(), "IOPlatformUUID")
        .ok_or_else(|| "ioreg printed no IOPlatformUUID".to_owned())?;
    if !uuid_shaped(&uuid) {
        return Err(format!("ioreg's IOPlatformUUID {uuid:?} is not a UUID"));
    }
    Ok(uuid)
}

#[cfg(test)]
#[path = "platform_tests.rs"]
pub(crate) mod tests;
