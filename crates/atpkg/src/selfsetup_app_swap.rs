// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The bootstrap's atomic placement door. The caller verifies the staged release's signature;
//! this door checks bundle identity, serializes cooperating installers, and keeps both bundles.

use super::*;

pub fn verb(rest: &[String]) -> ExitCode {
    let [stage, installed] = rest else {
        return crate::cli::usage_error(
            "app-swap",
            Some("expected staged and installed app paths"),
        );
    };
    let Some(home) = aterm_types::dirs::home_dir() else {
        return crate::cli::usage_error("app-swap", Some("HOME is unset"));
    };
    match place(Path::new(stage), Path::new(installed), &home) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("atpkg app-swap: {error}");
            ExitCode::FAILURE
        }
    }
}

fn real_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a real directory", path.display()));
    }
    Ok(())
}

fn place(stage: &Path, installed: &Path, home: &Path) -> Result<(), String> {
    real_directory(stage)?;
    if !bundle_is_ours(stage) {
        return Err("the staged directory is not an identified aterm bundle".into());
    }
    let parent = installed.parent().ok_or("installed app has no parent")?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    real_directory(parent)?;
    if !aterm_update_core::same_volume(stage, parent) {
        return Err("the staged app and destination are on different volumes".into());
    }
    let lock_path = parent.join(".aterm.app.install.lock");
    let _lock = aterm_update_core::FileLock::acquire_within(&lock_path, CHILD_HANG)
        .map_err(|error| format!("app installation lock: {error}"))?;
    match std::fs::symlink_metadata(installed) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            aterm_update_core::app_swap::rename_exclusive(stage, installed)
        }
        Err(error) => return Err(format!("cannot inspect installed app: {error}")),
        Ok(_) => {
            real_directory(installed)?;
            if std::fs::canonicalize(stage).map_err(|error| error.to_string())?
                == std::fs::canonicalize(installed).map_err(|error| error.to_string())?
            {
                return Err("the staged and installed app are the same directory".into());
            }
            let owned = bundle_is_ours(installed)
                || (bundle_identifier(installed).is_none()
                    && answers_as_aterm(
                        &installed.join("Contents/MacOS/aterm"),
                        runs_as(
                            aterm_uds::peer::our_uid(),
                            home,
                            &installed.join("Contents/MacOS/aterm"),
                        ),
                    ));
            if !owned {
                return Err("the installed directory is not an aterm bundle".into());
            }
            // The old bundle will move, then be deleted by the caller. Every program must
            // have finished its first exec before either action can race macOS assessment.
            for program in bundle_programs(installed)? {
                if let Some(why) = first_exec_unproven(
                    &program,
                    runs_as(aterm_uds::peer::our_uid(), home, &program),
                    CHILD_HANG,
                ) {
                    return Err(format!("{}: {why}", program.display()));
                }
            }
            aterm_update_core::app_swap::rename_swap(stage, installed)
        }
    }
    .map_err(|error| format!("atomic app placement: {error}"))
}

#[cfg(test)]
#[path = "selfsetup_app_swap_tests.rs"]
mod tests;
