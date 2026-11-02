// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A scratch machine: a home with Claude Code's versions directory and an atpkg prefix. Unix
/// only, as the tests that build one are: its programs are copies of `/bin/sleep`.
#[cfg(unix)]
struct World {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    versions: PathBuf,
    roots: ImageRoots,
}

#[cfg(unix)]
impl World {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = std::fs::canonicalize(dir.path()).expect("canonical scratch");
        let versions = claude_versions(&root.join("home"));
        std::fs::create_dir_all(&versions).expect("versions");
        let prefix = root.join("pkg");
        std::fs::create_dir_all(prefix.join("store")).expect("store");
        let roots = ImageRoots::new(&atpkg::store::Layout { prefix });
        Self {
            _dir: dir,
            root,
            versions,
            roots,
        }
    }

    fn places(&self) -> Places<'_> {
        Places {
            claude_versions: Some(&self.versions),
            roots: Some(&self.roots),
        }
    }

    /// A copy of `/bin/sleep` at `path`: a program that is no agent.
    fn sleep_at(&self, path: &Path) -> PathBuf {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("dir");
        }
        std::fs::copy("/bin/sleep", path).expect("a copy of sleep");
        path.to_path_buf()
    }

    /// A store build of `program` at `<prefix>/store/<program>/<build>/bin/<program>`.
    fn stored(&self, program: &str, build: u64) -> PathBuf {
        let path = self
            .roots
            .layout()
            .prefix
            .join(format!("store/{program}/{build}/bin/{program}"));
        self.sleep_at(&path)
    }
}

#[cfg(unix)]
const CLAUDE_304: u64 = 1_000_002_000_001_000_304;

#[cfg(unix)]
fn proven(agent: Program, image: &Image<'_>, places: &Places<'_>) -> Option<Proof> {
    proof_among(agent, image, places, &[])
}

/// The vendor layouts confirm by the file the kernel mapped: Claude Code's own versions entry,
/// an atpkg store build registered for that very agent. NEGATIVE CONTROL (by hand, run):
/// `FileId::of_entry` following links (`metadata`) confirms the link row below.
#[cfg(unix)]
#[test]
fn an_install_by_its_vendors_layout_is_confirmed_and_a_link_to_another_program_is_not() {
    let w = World::new();
    let places = w.places();
    let native = w.sleep_at(&w.versions.join("2.1.399"));
    let image = Image::at(&native);
    assert_eq!(
        proven(Program::Claude, &image, &places),
        Some(Proof::Layout)
    );
    assert!(in_vendor_layout(Program::Claude, &image, &places));
    assert!(
        !in_vendor_layout(Program::Agy, &image, &places),
        "another agent's"
    );
    let unversioned = w.sleep_at(&w.versions.join("claude"));
    assert_eq!(
        proven(Program::Claude, &Image::at(&unversioned), &places),
        None,
        "an entry that names no version"
    );
    let store = w.stored("claude", CLAUDE_304);
    assert_eq!(
        proven(Program::Claude, &Image::at(&store), &places),
        Some(Proof::Layout),
        "a registered store build"
    );
    assert_eq!(
        proven(Program::Muse, &Image::at(&store), &places),
        None,
        "Claude Code's store build is no Muse Code"
    );
    for (program, agent) in [("agy", Program::Agy), ("muse", Program::Muse)] {
        let built = w.stored(program, 1_000_001_000_004_000_012);
        assert_eq!(
            proven(agent, &Image::at(&built), &places),
            Some(Proof::Layout),
            "{program}"
        );
    }
    let unregistered = w.stored("claude", 7);
    assert_eq!(
        proven(Program::Claude, &Image::at(&unregistered), &places),
        None,
        "a store build no vendor stamp records"
    );
    // A copy of sleep reached as `claude` through a link in the versions directory: the kernel
    // names the copy, whose name is no entry there, and the link's own entry is a link.
    let sleep = w.sleep_at(&w.root.join("elsewhere/sleep"));
    let link = w.versions.join("9.9.9");
    std::os::unix::fs::symlink(&sleep, &link).expect("link");
    for exe in [&sleep, &link] {
        assert_eq!(
            proven(Program::Claude, &Image::at(exe), &places),
            None,
            "{}",
            exe.display()
        );
    }
    // The file the kernel mapped is not the entry: the entry was replaced since the exec.
    let replaced = Image {
        file: FileId::of(&sleep),
        ..Image::at(&native)
    };
    assert_eq!(proven(Program::Claude, &replaced, &places), None);
    assert_eq!(
        proven(Program::Claude, &image, &Places::default()),
        None,
        "no layout known"
    );
}

/// `exec -a claude` of another program: the kernel names that program's image, which no layout,
/// team or stand-in confirms, whatever its argv[0] says.
#[cfg(unix)]
#[test]
fn an_exec_a_claude_of_another_program_is_not_confirmed() {
    let w = World::new();
    let places = w.places();
    let image = Image::at(Path::new("/bin/sleep"));
    for agent in [Program::Claude, Program::Agy, Program::Muse, Program::Codex] {
        assert_eq!(proven(agent, &image, &places), None, "{agent:?}");
    }
    assert_eq!(
        Act::of(proven(Program::Claude, &image, &places)),
        Act::Observe
    );
}

/// The vendor's signing team confirms its agent's image, and no other agent's; an ad-hoc-signed
/// copy of the real binary carries no team (the kernel's `csops` names none), so the team never
/// confirms it, and only its place in the vendor's layout can. NEGATIVE CONTROL (by hand, run):
/// any team confirming confirms the Agy row.
#[cfg(unix)]
#[test]
fn the_vendors_signing_team_confirms_its_own_agent_only() {
    let w = World::new();
    let places = w.places();
    let elsewhere = w.sleep_at(&w.root.join("opt/claude"));
    let signed = |team: &'static str| Image {
        team: Some(team),
        ..Image::at(&elsewhere)
    };
    assert_eq!(
        proven(Program::Claude, &signed(CLAUDE_TEAM), &places),
        Some(Proof::Team)
    );
    assert_eq!(
        proven(Program::Agy, &signed(AGY_TEAM), &places),
        Some(Proof::Team)
    );
    assert_eq!(
        proven(Program::Muse, &signed(MUSE_TEAM), &places),
        Some(Proof::Team)
    );
    assert_eq!(proven(Program::Agy, &signed(CLAUDE_TEAM), &places), None);
    assert_eq!(
        proven(Program::Claude, &signed("QZPTEAM001"), &places),
        None
    );
    assert_eq!(
        proven(Program::Codex, &signed(CLAUDE_TEAM), &places),
        None,
        "codex-alab has no team"
    );
    let ad_hoc_copy = Image::at(&elsewhere);
    assert_eq!(proven(Program::Claude, &ad_hoc_copy, &places), None);
    let laid = w.sleep_at(&w.versions.join("2.1.400"));
    assert_eq!(
        proven(Program::Claude, &Image::at(&laid), &places),
        Some(Proof::Layout),
        "the same copy, laid in the vendor's layout"
    );
    assert_eq!(
        [CLAUDE_TEAM, AGY_TEAM, MUSE_TEAM],
        ["Q6L2SF6YDW", "EQHXZ8M8AV", "V9WTTPBFK9"]
    );
}

/// The lab's stand-ins confirm by identity, an image or the script a shell runs, and only the
/// files named: a debug build's seam. NEGATIVE CONTROL (by hand, run): with no stand-ins
/// passed, the stand-in rows are unconfirmed.
#[cfg(unix)]
#[test]
fn a_lab_stand_in_is_confirmed_by_identity_alone() {
    let w = World::new();
    let places = w.places();
    let fake = w.root.join("lab/agents/claude");
    std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    let alias = w.root.join("lab/bin/claude");
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&fake, &alias).unwrap();
    let other = w.sleep_at(&w.root.join("lab/other"));
    let standins = [FileId::of(&alias).unwrap()];
    let shell = |script: &Path| Image {
        script: FileId::of(script),
        ..Image::at(Path::new("/bin/sh"))
    };
    assert_eq!(
        proof_among(Program::Claude, &shell(&fake), &places, &standins),
        Some(Proof::Seam)
    );
    assert_eq!(
        proof_among(Program::Claude, &Image::at(&fake), &places, &standins),
        Some(Proof::Seam)
    );
    assert_eq!(
        proof_among(Program::Claude, &shell(&other), &places, &standins),
        None
    );
    assert_eq!(
        proof_among(Program::Claude, &Image::at(&other), &places, &standins),
        None
    );
}

/// The seam reads absolute paths only, by identity, from the development seam's list.
#[cfg(unix)]
#[test]
fn the_seam_lists_its_stand_ins_by_identity() {
    let w = World::new();
    let fake = w.sleep_at(&w.root.join("lab/claude"));
    let gone = w.root.join("lab/gone");
    let list = std::env::join_paths([fake.as_path(), Path::new("relative/claude"), gone.as_path()])
        .unwrap();
    assert_eq!(standins_in(Some(&list)), vec![FileId::of(&fake).unwrap()]);
    assert!(standins_in(None).is_empty());
}

#[test]
fn the_act_words_are_its_enumeration() {
    let words: Vec<&str> = Act::ALL.into_iter().map(Act::word).collect();
    assert_eq!(words, ["on", "observe"]);
    for act in Act::ALL {
        assert_eq!(Act::of_word(act.word()), Some(act));
    }
    assert_eq!(Act::of_word("-"), None);
    assert_eq!(Act::of(Some(Proof::Team)), Act::On);
    assert_eq!(Act::of(None), Act::Observe);
    let proofs: Vec<&str> = Proof::ALL.into_iter().map(Proof::word).collect();
    assert_eq!(proofs, ["layout", "team", "seam"]);
}
