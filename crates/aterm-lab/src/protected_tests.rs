// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn inside_means_the_path_or_below_it_and_prefix_means_the_name_too() {
    let h = "/Users//o";
    assert!(inside_protected("/Users//o/.ssh", h, CCSOCKS));
    assert!(inside_protected("/Users//o/.ssh/id_rsa", h, CCSOCKS));
    assert!(!inside_protected("/Users//o/.sshx", h, CCSOCKS));
    assert!(inside_protected("/Users//o/.claude.json.bak", h, CCSOCKS));
    assert!(inside_protected("/private/tmp/cc-socks/1.sock", h, CCSOCKS));
    assert!(!inside_protected(
        "/Users//o/Developer/aterm/target/real-claude-lab",
        h,
        CCSOCKS
    ));
}

/// Muse Code and Antigravity data paths are protected; sibling prefixes are not.
#[test]
fn muse_code_s_and_antigravity_s_own_data_are_on_the_list() {
    let h = "/Users//o";
    for p in [
        "/Users//o/Library/Application Support/Muse/session-name-authority/session-names.db",
        "/Users//o/Library/Application Support/Muse",
        "/Users//o/.local/share/muse/sessions/2026/10/17/x/session.jsonl",
        "/Users//o/Library/Application Support/Antigravity/User/globalStorage/state.vscdb",
        "/Users//o/Library/Application Support/Antigravity Nightly",
        "/Users//o/.antigravity/extensions",
        "/Users//o/.antigravity-server",
    ] {
        assert!(inside_protected(p, h, CCSOCKS), "{p}");
    }
    for p in [
        "/Users//o/Library/Application Support/Museum",
        "/Users//o/.local/share/muse-other",
        "/Users//o/.local/share/fish",
    ] {
        assert!(!inside_protected(p, h, CCSOCKS), "{p}");
    }
    let lines = render_lines(h, CCSOCKS);
    for want in [
        "subpath\t/Users//o/Library/Application Support/Muse\n",
        "subpath\t/Users//o/.local/share/muse\n",
        "prefix\t/Users//o/Library/Application Support/Antigravity\n",
        "prefix\t/Users//o/.antigravity\n",
    ] {
        assert!(lines.contains(want), "{want}");
    }
}

/// Grok Build's home and Kimi Code's current and legacy homes are protected;
/// siblings of the Grok home and unrelated Kimi folders are not.
#[test]
fn grok_build_s_and_kimi_code_s_own_data_are_on_the_list() {
    let h = "/Users//o";
    for p in [
        "/Users//o/.grok",
        "/Users//o/.grok/auth.json",
        "/Users//o/.kimi",
        "/Users//o/.kimi/config.toml",
        "/Users//o/.kimi-code/credentials",
        "/Users//o/.kimi-code/config.toml",
        "/Users//o/.kimi-code/credentials/kimi-code.json",
        "/Users//o/.kimi/.migrated-to-kimi-code",
    ] {
        assert!(inside_protected(p, h, CCSOCKS), "{p}");
    }
    for p in [
        "/Users//o/.grokx",
        "/Users//o/Developer/grok",
        "/Users//o/.grokker",
        "/Users//o/.local/share/kimi",
        "/Users//o/kimi",
    ] {
        assert!(!inside_protected(p, h, CCSOCKS), "{p}");
    }
    let lines = render_lines(h, CCSOCKS);
    for want in ["subpath\t/Users//o/.grok\n", "prefix\t/Users//o/.kimi\n"] {
        assert!(lines.contains(want), "{want}");
    }
}

/// `~/.aterm` and `~/.secrets` are protected; sibling prefixes are not.
#[test]
fn aterm_s_own_secrets_and_a_secrets_dir_are_on_the_list() {
    let h = "/Users//o";
    for p in [
        "/Users//o/.aterm",
        "/Users//o/.aterm/machine.key",
        "/Users//o/.aterm/release-credentials.toml",
        "/Users//o/.aterm/release-ring/machine.key",
        "/Users//o/.aterm/release-ring/release-credentials.toml",
        "/Users//o/.aterm/shell.d/zsh.zsh",
        "/Users//o/.secrets",
        "/Users//o/.secrets/token",
    ] {
        assert!(inside_protected(p, h, CCSOCKS), "{p}");
    }
    for p in [
        "/Users//o/.aterm-update",
        "/Users//o/.aterm-copy-1",
        "/Users//o/.secretsx",
        "/Users//o/Developer/aterm/.aterm-verify",
    ] {
        assert!(!inside_protected(p, h, CCSOCKS), "{p}");
    }
    let lines = render_lines(h, CCSOCKS);
    for want in ["subpath\t/Users//o/.aterm\n", "subpath\t/Users//o/.secrets\n"] {
        assert!(lines.contains(want), "{want}");
    }
}
