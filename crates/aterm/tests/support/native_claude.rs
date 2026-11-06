// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Place and positively execute the native Claude stand-in used by private integration worlds.

use std::path::Path;

pub fn place(root: &Path, dest: &Path, idle: &str) {
    std::fs::create_dir_all(root.join("sc")).expect("native fixture screens");
    std::fs::create_dir_all(root.join("work")).expect("native fixture working directory");
    std::fs::write(root.join("sc/idle.txt"), idle).expect("idle screen");
    std::fs::create_dir_all(dest.parent().expect("native fixture parent"))
        .expect("native fixture directory");
    aterm_uds::deadline::ad_hoc_copy(&aterm_tempfile::bin_exe("aterm-claude-fixture"), dest);
    let mut proof = aterm_uds::spawnfd::command(dest)
        .env("ATERM_CLAUDE_FIXTURE_ROOT", root)
        .arg("--version")
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("run the native fixture in place");
    aterm_uds::deadline::prints(
        &mut proof,
        "2.1.281 (Claude Code)",
        aterm_uds::deadline::ECHO_HANG,
    );
    assert!(proof.wait().expect("native fixture version exit").success());
}

/// The server acknowledged a live watch for this exact fixture process before it is ended.
pub fn observed(root: &Path, sid: &str, pid: u32) -> bool {
    fn find(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let kind = entry.file_type().ok()?;
            if kind.is_dir() {
                if let Some(path) = find(&entry.path(), name) {
                    return Some(path);
                }
            } else if kind.is_file() && entry.file_name() == name {
                return Some(entry.path());
            }
        }
        None
    }
    use aterm_agent::harness::native_observe::Request;
    let name = aterm_codec::hex::encode_lower(sid.as_bytes());
    let Some(path) = find(root, &name) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut lines = text.lines();
    let Some(Request::Register(registration)) = lines.next().and_then(Request::parse) else {
        return false;
    };
    let Some(Request::Refresh(binding)) = lines.next().and_then(Request::parse) else {
        return false;
    };
    lines.next().is_none()
        && registration.vendor.pid == pid
        && binding.id == registration.id
        && binding.host_epoch == registration.host_epoch
}
