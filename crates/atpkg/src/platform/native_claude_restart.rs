// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Immutable direct Claude target selection for an already supervised native restart.

use std::path::PathBuf;

/// Frozen launch data, not execution authority or an executable probe.
#[derive(Debug)]
pub struct NativeClaudeRestart {
    /// The actual physical vendor image, never its mutable wrapper.
    pub target: PathBuf,
    /// Exports from the exact recognized producer, before caller overrides.
    pub env: crate::shim_env::ShimEnv,
}

/// Resolve the configured Claude twin for a typed resume whose posture is already explicit.
/// The caller still owns authentication, permissions, account admission and execution custody.
///
/// # Errors
/// Unsupported platform, non-resume/defaulted posture, unreadable or unknown producer,
/// unrepresented compatibility/managed routes, or an executable outside Claude's store/checkout.
pub fn native_claude_restart(
    #[cfg(unix)] layout: &crate::store::Layout,
    #[cfg(not(unix))] _: &crate::store::Layout,
    #[cfg(unix)] final_argv: &[String],
    #[cfg(not(unix))] _: &[String],
) -> Result<NativeClaudeRestart, &'static str> {
    #[cfg(not(unix))]
    {
        Err("native Claude restart projection is unavailable on this platform")
    }
    #[cfg(unix)]
    {
        use std::io::Read as _;
        use std::os::unix::fs::PermissionsExt as _;
        if !final_argv
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| arg == "--resume" || arg.starts_with("--resume="))
            || !matches!(
                crate::yolo::decide("claude", final_argv),
                crate::yolo::Launch::AsTyped
            )
        {
            return Err("native Claude restart requires an explicit resume posture");
        }
        let wrapper =
            layout.agent_shim(&crate::store::ToolName::new("claude").ok_or("Claude tool name")?);
        let (file, metadata) = crate::metadata_io::open_regular_with_meta(&wrapper)
            .map_err(|_| "Claude producer cannot be read")?;
        if metadata.len() > super::MAX_SHIM_BYTES as u64 {
            return Err("Claude producer exceeds its bound");
        }
        let mut content = String::new();
        file.take((super::MAX_SHIM_BYTES + 1) as u64)
            .read_to_string(&mut content)
            .map_err(|_| "Claude producer is not bounded UTF-8")?;
        let producer = super::shim_producer::recognize(&wrapper, &content)
            .ok_or("Claude producer is unknown")?;
        if producer.kind == super::shim_producer::Kind::Factory
            && (producer.program != "claude" || producer.prefix != layout.prefix)
        {
            return Err("Claude producer names another program or prefix");
        }
        if producer.route.is_some() || producer.launch.is_some() || producer.token_route {
            return Err("Claude producer requires an unrepresented launch route");
        }
        let target = std::fs::canonicalize(&producer.target)
            .map_err(|_| "Claude target cannot be resolved")?;
        let metadata = std::fs::metadata(&target).map_err(|_| "Claude target cannot be read")?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || target.file_name() != Some(std::ffi::OsStr::new("claude"))
        {
            return Err("Claude target is not its executable");
        }
        let (head, _) = crate::metadata_io::read_head_with_meta(&target, 8)
            .map_err(|_| "Claude target header cannot be read")?;
        if !crate::macho::is_native_executable(&head) {
            return Err("Claude target is not a native image");
        }
        let prefix = std::fs::canonicalize(&layout.prefix)
            .map_err(|_| "Claude prefix cannot be resolved")?;
        let store = prefix.join("store/claude");
        let in_store = crate::shim_env::store_build_of(&target)
            .is_some_and(|build| build.parent() == Some(store.as_path()));
        let linked = crate::linkmode::live_linked_checkout(layout, "claude")
            .map_err(|_| "Claude checkout binding cannot be read")?;
        let in_checkout = match linked {
            Some(checkout) => target.starts_with(checkout),
            None => false,
        };
        if !in_store && !in_checkout {
            return Err("Claude target is outside its store or checkout");
        }
        Ok(NativeClaudeRestart {
            target,
            env: producer.env,
        })
    }
}

#[cfg(all(test, unix))]
#[path = "native_claude_restart_tests.rs"]
mod tests;
