// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Paired file-event instrument: run under the sysprobe file_events example with the
//! same private scratch root, first `legacy`, then `quiet`. `file-legacy` / `file-quiet`
//! exercise the discovery file-cache handoff. Neither pair uses live state.

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let root = std::path::PathBuf::from(
        args.next()
            .ok_or_else(|| std::io::Error::other("expected scratch root"))?,
    );
    let (quiet, file) = match args.next().and_then(|s| s.into_string().ok()).as_deref() {
        Some("legacy") => (false, false),
        Some("quiet") => (true, false),
        Some("file-legacy") => (false, true),
        Some("file-quiet") => (true, true),
        _ => {
            return Err(std::io::Error::other(
                "expected legacy, quiet, file-legacy or file-quiet",
            ));
        }
    };
    let legacy = root.join("history");
    aterm_uds::private_file::create_private_dir(&root)?;
    if file {
        let legacy = root.join("commons-peers.state");
        let path = if quiet {
            aterm_uds::quiet_state::prepare_file_cache(&legacy, 1024)?
        } else {
            legacy
        };
        for revision in 0..100_u64 {
            aterm_uds::private_file::write_private(
                path.parent().unwrap(),
                path.file_name().unwrap().to_str().unwrap(),
                &revision.to_le_bytes(),
            )?;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        println!("writes=100 path={}", path.display());
        return Ok(());
    }
    if quiet {
        aterm_uds::quiet_state::prepare(&legacy, || {
            aterm_uds::private_file::PrivateLock::take(
                &legacy,
                "writer.lock",
                std::time::Duration::ZERO,
                "writer held",
            )
        })?;
    } else {
        aterm_uds::private_file::create_private_dir(&legacy)?;
    }
    let dir = aterm_uds::quiet_state::selected(&legacy);
    for revision in 0..100_u64 {
        aterm_uds::private_file::write_private(&dir, "checkpoint", &revision.to_le_bytes())?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    println!("writes=100 path={}", dir.display());
    Ok(())
}

/// The instrument reads unix file events and private files; elsewhere it has nothing to run.
#[cfg(not(unix))]
fn main() {
    eprintln!("quiet_state: the file-event instrument runs on unix only");
}
