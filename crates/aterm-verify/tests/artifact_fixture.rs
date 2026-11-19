// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development-only facts probe for the verifier's private-image execution tests.

#[path = "../../atpkg/tests/facts_fixture.rs"]
mod facts;

fn main() -> std::process::ExitCode {
    if std::env::args_os().len() == 1 {
        #[cfg(target_os = "macos")]
        if let Some(reply) = appkit_reply() {
            return reply;
        }
        return match (std::env::current_exe(), std::env::current_dir()) {
            (Ok(executable), Ok(cwd)) => {
                println!("__snapshot_executable={}", executable.display());
                println!("__snapshot_cwd={}", cwd.display());
                std::process::ExitCode::SUCCESS
            }
            _ => std::process::ExitCode::from(1),
        };
    }
    facts::entry().unwrap_or(std::process::ExitCode::from(2))
}

/// Contract fixtures place typed replies beside their empty runtime cwd, outside the sealed
/// executable and its persistent namespace. No arbitrary program or shell body is evaluated.
#[cfg(target_os = "macos")]
fn appkit_reply() -> Option<std::process::ExitCode> {
    use std::io::{Read as _, Write as _};
    let executable = std::env::current_exe().ok()?;
    let name = executable.file_name()?.to_str()?;
    let cwd = std::env::current_dir().ok()?;
    let file = cwd.parent()?.join(format!(".{name}.fixture"));
    let file = match std::fs::File::open(file) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(std::process::ExitCode::from(2)),
    };
    let mut reply = String::new();
    if file.take(16_385).read_to_string(&mut reply).is_err() || reply.len() > 16_384 {
        return Some(std::process::ExitCode::from(2));
    }
    println!("__snapshot_executable={}", executable.display());
    println!("__snapshot_cwd={}", cwd.display());
    if reply == "kill\n" {
        let _ = std::io::stdout().flush();
        aterm_uds::process::kill_self_now();
    }
    Some(
        match reply.split_once('\n').and_then(|(mode, output)| {
            let code = mode.strip_prefix("exit:")?.parse::<u8>().ok()?;
            print!("{output}");
            Some(code)
        }) {
            Some(code) => std::process::ExitCode::from(code),
            None => std::process::ExitCode::from(2),
        },
    )
}
