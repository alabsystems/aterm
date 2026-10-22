// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn each_clis_own_flags_name_its_level() {
    for (id, words, default, want) in [
        (
            "claude",
            &["--dangerously-skip-permissions", "--resume", "x"][..],
            None,
            Level::Yolo,
        ),
        (
            "claude",
            &["--permission-mode", "bypassPermissions"][..],
            None,
            Level::Yolo,
        ),
        (
            "claude",
            &["--permission-mode=acceptEdits"][..],
            None,
            Level::Edits,
        ),
        (
            "claude",
            &["--permission-mode", "plan"][..],
            None,
            Level::Plan,
        ),
        ("claude", &[][..], None, Level::Ask),
        // Its settings' default mode, where the flags name none; the flags win.
        ("claude", &[][..], Some("bypassPermissions"), Level::Yolo),
        (
            "claude",
            &["--permission-mode", "plan"][..],
            Some("bypassPermissions"),
            Level::Plan,
        ),
        (
            "codex",
            &["--dangerously-bypass-approvals-and-sandbox"][..],
            None,
            Level::Yolo,
        ),
        ("codex", &["-s", "read-only"][..], None, Level::Plan),
        ("codex", &[][..], None, Level::Edits),
        (
            "agy",
            &["--dangerously-skip-permissions"][..],
            None,
            Level::Yolo,
        ),
        ("agy", &["--mode", "plan"][..], None, Level::Plan),
        ("agy", &[][..], None, Level::Ask),
        ("muse", &["--yolo"][..], None, Level::Yolo),
        ("muse", &[][..], None, Level::Ask),
    ] {
        assert_eq!(
            level_of(id, &argv(words), default),
            if words.is_empty() { None } else { Some(want) },
            "{id} {words:?} {default:?}"
        );
    }
}

#[test]
fn a_word_names_a_choice() {
    assert_eq!(Choice::parse("inherit"), Some(Choice::Inherit));
    for level in [Level::Yolo, Level::Edits, Level::Ask, Level::Plan] {
        assert_eq!(Choice::parse(level.word()), Some(Choice::Fixed(level)));
    }
    assert_eq!(Choice::parse("root"), None);
}

#[test]
fn a_managed_or_vendor_path_names_its_cli() {
    for (path, want) in [
        (
            "/x/pkg/store/claude/1000002000001000302/bin/claude",
            Some("claude"),
        ),
        (
            "/Users//me/.local/share/claude/versions/2.1.301",
            Some("claude"),
        ),
        ("/opt/homebrew/bin/codex", Some("codex")),
        ("/bin/zsh", None),
        ("/x/versions/2.0", None),
    ] {
        assert_eq!(cli_of_path(path).map(|c| c.id), want, "{path}");
    }
}

/// A process tree: pid → (parent, exec path, argv).
struct Tree(Vec<(u32, u32, &'static str, Vec<String>)>);

impl Procs for Tree {
    fn first(&self) -> Option<u32> {
        self.0.first().map(|p| p.0)
    }
    fn parent(&self, pid: u32) -> Option<u32> {
        self.0.iter().find(|p| p.0 == pid).map(|p| p.1)
    }
    fn args(&self, pid: u32) -> Option<(String, Vec<String>)> {
        self.0
            .iter()
            .find(|p| p.0 == pid)
            .map(|p| (p.2.to_string(), p.3.clone()))
    }
}

#[test]
fn the_nearest_ai_cli_above_is_the_asker() {
    let tree = Tree(vec![
        (40, 30, "/bin/zsh", argv(&["zsh", "-c", "aterm ai review"])),
        (
            30,
            20,
            "/x/pkg/store/claude/1/bin/claude",
            argv(&["claude", "--dangerously-skip-permissions"]),
        ),
        (
            20,
            10,
            "/opt/homebrew/bin/codex",
            argv(&["codex", "-s", "read-only"]),
        ),
        (10, 1, "/bin/zsh", argv(&["-zsh"])),
    ]);
    let none = || None;
    let (id, level) = asker(&tree, &none).agent().expect("an asker");
    assert_eq!((id, level), ("claude", Level::Yolo));
    // A node runtime's argv[0] names the CLI when its executable does not.
    let node = Tree(vec![
        (40, 30, "/bin/zsh", argv(&["zsh"])),
        (
            30,
            1,
            "/usr/local/bin/node",
            argv(&["/usr/local/lib/node_modules/muse/bin/muse", "--yolo"]),
        ),
    ]);
    assert_eq!(asker(&node, &none).agent(), Some(("muse", Level::Yolo)));
    // The control: no AI CLI above is a person.
    let person = Tree(vec![
        (40, 10, "/bin/zsh", argv(&["zsh"])),
        (10, 1, "/sbin/launchd", argv(&["launchd"])),
    ]);
    assert_eq!(asker(&person, &none), Asker::Person);
}

/// Permission-looking words in a value or after `--` are data, not a launch mode.
#[test]
fn permission_looking_argument_data_never_raises_the_inherited_level() {
    let plan = argv(&["--permission-mode", "plan"]);
    assert_eq!(level_of("claude", &plan, None), Some(Level::Plan));
    let raised: Vec<_> = [
        vec![
            "--permission-mode",
            "plan",
            "--",
            "--dangerously-skip-permissions",
        ],
        vec![
            "--permission-mode",
            "plan",
            "--append-system-prompt",
            "--dangerously-skip-permissions",
        ],
    ]
    .into_iter()
    .filter(|words| level_of("claude", &argv(words), None) != Some(Level::Plan))
    .collect();
    assert!(
        raised.is_empty(),
        "argument data raised permission: {raised:?}"
    );
}

/// Meta's launcher execs its versioned build; the canonical reader already knows it.
#[test]
fn a_versioned_muse_executable_keeps_its_permission_custody() {
    let known = Tree(vec![(40, 1, "/x/muse", argv(&["muse", "--yolo"]))]);
    assert_eq!(asker(&known, &|| None).agent(), Some(("muse", Level::Yolo)));
    let build = Tree(vec![(
        40,
        1,
        "/x/muse-bin-1.4.10-R6427.1",
        argv(&["muse-bin-1.4.10-R6427.1", "--yolo"]),
    )]);
    assert_eq!(asker(&build, &|| None).agent(), Some(("muse", Level::Yolo)));
}

/// atpkg's `codex-old` (OpenAI's own Codex beside codex-alab) runs as that name, and a
/// review it asks for inherits its own launch's sandbox: a read-only one plans, never
/// the person's Yolo the walk past an unknown name would prove.
///
/// NEGATIVE CONTROL: a name that only begins so is no CLI, so the walk reaches the root.
#[test]
fn a_codex_old_keeps_codexs_permission_custody() {
    let store = "/x/pkg/store/codex-old/1000000000163000000/bin/codex-old";
    assert_eq!(cli_of_path(store).map(|c| c.id), Some("codex"));
    let read_only = Tree(vec![
        (40, 30, "/bin/zsh", argv(&["zsh", "-c", "aterm ai review"])),
        (30, 1, store, argv(&["codex-old", "-s", "read-only"])),
    ]);
    assert_eq!(
        asker(&read_only, &|| None).agent(),
        Some(("codex", Level::Plan))
    );
    let lookalike = Tree(vec![(
        40,
        1,
        "/x/codex-older",
        argv(&["codex-older", "-s", "read-only"]),
    )]);
    assert_eq!(asker(&lookalike, &|| None), Asker::Person);
}

/// The kernel's conventional interpreter argv has the script at argv[1].
#[test]
fn a_script_runtime_keeps_its_agents_script_and_permission_arguments() {
    let direct = Tree(vec![(40, 1, "/x/agy", argv(&["agy", "--mode", "plan"]))]);
    assert_eq!(asker(&direct, &|| None).agent(), Some(("agy", Level::Plan)));
    let node = Tree(vec![(
        40,
        1,
        "/usr/bin/node",
        argv(&["node", "/x/agy", "--mode", "plan"]),
    )]);
    assert_eq!(asker(&node, &|| None).agent(), Some(("agy", Level::Plan)));
}

#[test]
fn unknown_launch_grammar_and_implicit_defaults_prove_no_permission() {
    for (id, words) in [
        ("codex", vec![]),
        ("claude", vec![]),
        (
            "claude",
            vec![
                "--permission-mode",
                "plan",
                "--unfamiliar",
                "--dangerously-skip-permissions",
            ],
        ),
        (
            "claude",
            vec![
                "--permission-mode",
                "plan",
                "--dangerously-skip-permissions",
            ],
        ),
        ("muse", vec!["--disable-write"]),
    ] {
        assert_eq!(
            level_of(id, &argv(&words), Some("bypassPermissions")),
            None,
            "{id} {words:?}"
        );
    }
    assert_eq!(
        level_of("codex", &argv(&["--sandbox=read-only"]), None),
        Some(Level::Plan)
    );
    assert_eq!(
        level_of("muse", &argv(&["--disable-write", "--disable-shell"]), None),
        Some(Level::Plan)
    );
}

#[test]
fn an_incomplete_walk_and_an_unresolved_runtime_are_unknown() {
    struct MissingFirst;
    impl Procs for MissingFirst {
        fn first(&self) -> Option<u32> {
            None
        }
        fn parent(&self, _: u32) -> Option<u32> {
            panic!("no walk was started")
        }
        fn args(&self, _: u32) -> Option<(String, Vec<String>)> {
            panic!("no walk was started")
        }
    }
    assert_eq!(asker(&MissingFirst, &|| None), Asker::Unknown);
    for (exec, words, parent) in [
        ("/usr/bin/node", argv(&["node", "-e", "anything"]), 1),
        ("/usr/bin/node", argv(&["node", "/x/unknown-script"]), 1),
        ("/bin/zsh", argv(&["zsh"]), 40),
        ("/x/codex", argv(&["codex"]), 1),
    ] {
        let tree = Tree(vec![(40, parent, exec, words)]);
        assert_eq!(asker(&tree, &|| None), Asker::Unknown, "{exec}");
    }
}

/// Removing a sandbox does not establish that approval prompts are bypassed.
#[test]
fn a_sandbox_without_approval_bypass_does_not_prove_yolo() {
    for words in [
        vec!["--sandbox=danger-full-access"],
        vec!["--sandbox", "danger-full-access", "--model", "a-model"],
    ] {
        assert_eq!(level_of("codex", &argv(&words), None), None, "{words:?}");
    }
    assert_eq!(
        level_of(
            "codex",
            &argv(&["--dangerously-bypass-approvals-and-sandbox"]),
            None
        ),
        Some(Level::Yolo)
    );
}
