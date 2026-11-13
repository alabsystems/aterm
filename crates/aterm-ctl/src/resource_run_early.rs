// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Original CLI expectation captured before startup can open/reuse descriptors.
use super::*;

/// Non-Clone ownership of the actual startup description and its exact argv.
/// This is a backend expectation, never resource or backend-selection authority.
pub struct OriginalRootStore {
    #[cfg(unix)]
    source: i32,
    #[cfg(unix)]
    arguments: Vec<OsString>,
    #[cfg(unix)]
    directory: std::fs::File,
    #[cfg(not(unix))]
    _never: std::convert::Infallible,
}

impl OriginalRootStore {
    #[cfg(unix)]
    pub(super) fn check_arguments(&self, arguments: &[OsString]) -> io::Result<()> {
        if self.arguments != arguments {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original Store expectation startup arguments changed",
            ));
        }
        Ok(())
    }
    #[cfg(unix)]
    pub(super) fn into_file(
        self,
        arguments: &[OsString],
        source: i32,
    ) -> io::Result<std::fs::File> {
        self.check_arguments(arguments)?;
        if self.source != source {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original Store expectation startup arguments changed",
            ));
        }
        Ok(self.directory)
    }
}

/// Snapshot/capture only an explicit typed Root option at native entry.
/// `argv` excludes the executable and begins with the actual verb. Missing or
/// closed originals refuse now; no later parser may adopt a replacement FD.
///
/// # Safety
/// Call before descriptor-opening startup callbacks or threads. An advertised
/// source must not be closed/replaced concurrently during this one capture.
pub unsafe fn capture_original_root_store(
    argv: &[OsString],
) -> io::Result<Option<OriginalRootStore>> {
    if argv.first().is_none_or(|value| value != "resource-run") {
        return Ok(None);
    }
    let mut words = argv[1..].iter();
    let mut source = None;
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if matches!(
            word.to_str(),
            Some("--accounted" | "--capabilities" | "--help" | "-h")
        ) {
            continue;
        }
        let value = words.next();
        if word != "--expect-original-store-fd" && word != "--expect-native-job-original-store-fd" {
            continue;
        }
        if source.is_some() {
            return Err(super::super::usage(USAGE));
        }
        source = Some(
            value
                .and_then(|value| value.to_str())
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|value| *value >= 6)
                .ok_or_else(|| super::super::usage(USAGE))?,
        );
    }
    let Some(source) = source else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        Ok(Some(OriginalRootStore {
            source,
            arguments: argv[1..].to_vec(),
            directory: admission::store_file(source)?,
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = source;
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(all(test, unix))]
#[path = "resource_run_early_tests.rs"]
mod tests;
