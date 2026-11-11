// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A program word matches without case, from a path and past a login shell's `-`, and is
/// borrowed, never rebuilt: the GUI asks it of every pane on every frame.
#[test]
fn program_words_match_without_case_from_a_path_and_a_login_dash() {
    for (program, want) in [
        ("codex", Some(Agent::Codex)),
        ("CODEX", Some(Agent::Codex)),
        ("/usr/local/bin/Claude --resume", Some(Agent::Claude)),
        ("-Claude-Code", Some(Agent::Claude)),
        ("claude-alab", Some(Agent::Claude)),
        ("-CLAUDE-CODE-ALAB --resume", Some(Agent::Claude)),
        (
            "/opt/pkg/store/claude-code-alab/14275/bin/claude-alab --resume x",
            Some(Agent::Claude),
        ),
        ("Agy", Some(Agent::Agy)),
        ("MUSE x", Some(Agent::Muse)),
        (
            "/Users//x/.local/bin/muse-bin-1.4.10-R6427.1",
            Some(Agent::Muse),
        ),
        ("muse-alab", Some(Agent::Muse)),
        (
            "/opt/pkg/store/muse-alab/7/bin/muse-alab resume x",
            Some(Agent::Muse),
        ),
        ("Muse-Alab", Some(Agent::Muse)),
        ("muse-alabs", None),
        ("xmuse-alab", None),
        ("muse-bin-", None),
        ("muse-bin-latest", None),
        ("muse-binary", None),
        ("codexx", None),
        ("codex-old", Some(Agent::Codex)),
        (
            "/opt/pkg/store/codex-old/1000000000163000000/bin/codex-old resume",
            Some(Agent::Codex),
        ),
        ("CODEX-OLD", Some(Agent::Codex)),
        ("codex-older", None),
        ("xcodex-old", None),
        ("codex-", None),
        ("grok", Some(Agent::Grok)),
        ("grok-alab", Some(Agent::Grok)),
        (
            "/opt/pkg/bin/grok-alab run -- --resume x",
            Some(Agent::Grok),
        ),
        ("Grok-Alab", Some(Agent::Grok)),
        ("grok-alabs", None),
        ("grok-", None),
        ("kimi", Some(Agent::Kimi)),
        ("kimi-code", Some(Agent::Kimi)),
        ("kimi-alab", Some(Agent::Kimi)),
        (
            "/opt/pkg/bin/kimi-alab run -- --session x",
            Some(Agent::Kimi),
        ),
        ("kimi-alabs", None),
        ("claude-codes", None),
        ("claude-alabs", None),
        ("claude-code-alabs", None),
        ("xclaude-alab", None),
        ("node", None),
        ("", None),
    ] {
        assert_eq!(agent_of(program), want, "{program:?}");
    }
    assert!(may_host_agent("Node"));
    assert!(may_host_agent("/opt/homebrew/bin/BUN run x"));
    assert!(!may_host_agent("nodes"));
    assert!(!may_host_agent("codex"));
}
