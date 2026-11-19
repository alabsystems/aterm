// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exit test of DC-32: a fixture of m39's cases (docs/DESIGN-disk-map-2026-11-18.md
//! §1), built as data, classes each as designed and keeps the four laws; each rule then
//! meets a mutant that fails it.

use super::classify::{self, Facts, Marker, Place, Root, Rule};
use super::fold::{self, FOLDER_MIN_BYTES};
use super::laws;
use super::*;

const V: Volume = 0x1000_0004;
const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

fn dir(inode: u64) -> NodeId {
    NodeId::Dir(DirKey { volume: V, inode })
}
fn part(inode: u64, p: Part) -> NodeId {
    NodeId::Part(DirKey { volume: V, inode }, p)
}
fn region(r: Region) -> NodeId {
    NodeId::Region(r)
}

/// One fixture case: the facts the walker would read, and what the design says of them.
struct Case {
    id: NodeId,
    kind: NodeKind,
    parent: NodeId,
    name: &'static str,
    facts: Facts,
    class: Class,
    proofs: Vec<Proof>,
    fate: Fate,
}

fn read(tally: Tally) -> Facts {
    Facts {
        read: true,
        tally,
        ..Facts::default()
    }
}
fn ignored() -> Place {
    Place {
        git_ignored: true,
        ..Place::default()
    }
}
fn cargo_root(name: &str) -> Root {
    Root {
        names: vec![".rustc_info.json".into(), "debug".into()],
        name: name.into(),
        ..Root::default()
    }
}
const CARGO_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n";

fn container(id: NodeId, kind: NodeKind, parent: Option<NodeId>, name: &str) -> Node {
    let mut n = Node::leaf(
        id,
        kind,
        region(Region::Sys),
        name,
        Class::Unread,
        Tally::default(),
    );
    n.parent = parent;
    n.classes.clear();
    n
}

/// m39's cases, 2026-11-18 (§1, §6.1).
fn cases() -> Vec<Case> {
    let mut out = vec![
        // $HOME/trust, the main checkout: tracked, untracked and its store stay.
        Case {
            id: part(101, Part::Pushed),
            kind: NodeKind::Part,
            parent: dir(101),
            name: "pushed",
            facts: Facts {
                part: Some(Part::Pushed),
                ..read(Tally::of(FileKind::Source, false, 2 * GIB))
            },
            class: Class::Pushed,
            proofs: vec![],
            fate: Fate::Stays,
        },
        Case {
            id: part(101, Part::Untracked),
            kind: NodeKind::Part,
            parent: dir(101),
            name: "untracked",
            facts: Facts {
                part: Some(Part::Untracked),
                ..read(Tally::of(FileKind::Source, true, 40 * MIB))
            },
            class: Class::Unique,
            proofs: vec![],
            fate: Fate::Stays,
        },
        Case {
            id: part(101, Part::Store),
            kind: NodeKind::Part,
            parent: dir(101),
            name: "store",
            facts: Facts {
                part: Some(Part::Store),
                ..read(Tally::of(FileKind::GitObjects, false, 3 * GIB))
            },
            class: Class::Store,
            proofs: vec![],
            fate: Fate::Stays,
        },
        // Trust's bootstrap layout inside its git-ignored build/: a stage is proven, an
        // unknown child is not.
        Case {
            id: dir(102),
            kind: NodeKind::Build,
            parent: part(101, Part::Ignored("build".into())),
            name: "stage1",
            facts: Facts {
                root: Root {
                    trust_build_path: Some("aarch64-apple-darwin/stage1".into()),
                    name: "stage1".into(),
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Libraries, false, 60 * GIB))
            },
            class: Class::Build,
            proofs: vec![Proof::Marker],
            fate: Fate::Remove,
        },
        Case {
            id: dir(103),
            kind: NodeKind::Folder,
            parent: part(101, Part::Ignored("build".into())),
            name: "hand-notes",
            facts: Facts {
                root: Root {
                    trust_build_path: Some("hand-notes".into()),
                    name: "hand-notes".into(),
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Data, false, GIB))
            },
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // $HOME/trust-land/vcgen-flake: a linked worktree of an ended session, 1 commit no
        // remote has, 24.9 GiB of Cargo output: pin, then remove.
        Case {
            id: part(110, Part::Pushed),
            kind: NodeKind::Part,
            parent: dir(110),
            name: "pushed",
            facts: Facts {
                part: Some(Part::Pushed),
                ..read(Tally::of(FileKind::Source, false, GIB))
            },
            class: Class::Pushed,
            proofs: vec![],
            fate: Fate::None,
        },
        Case {
            id: part(110, Part::Unpushed),
            kind: NodeKind::Part,
            parent: dir(110),
            name: "unpushed",
            facts: Facts {
                part: Some(Part::Unpushed),
                ..read(Tally::of(FileKind::Source, false, MIB))
            },
            class: Class::Unpushed,
            proofs: vec![],
            fate: Fate::None,
        },
        Case {
            id: part(110, Part::Ignored("target".into())),
            kind: NodeKind::Part,
            parent: dir(110),
            name: "target",
            facts: Facts {
                part: Some(Part::Ignored("target".into())),
                root: cargo_root("target"),
                place: ignored(),
                ..read(Tally::of(FileKind::Incremental, false, 24 * GIB))
            },
            class: Class::Build,
            proofs: vec![Proof::Marker],
            fate: Fate::None,
        },
        // $HOME/trust-land/mem-proportional: its session is live and 54 commits no remote
        // has; only its build output goes.
        Case {
            id: part(120, Part::Unpushed),
            kind: NodeKind::Part,
            parent: dir(120),
            name: "unpushed",
            facts: Facts {
                part: Some(Part::Unpushed),
                ..read(Tally::of(FileKind::Source, false, 30 * MIB))
            },
            class: Class::Unpushed,
            proofs: vec![],
            fate: Fate::None,
        },
        Case {
            id: part(120, Part::Dirty),
            kind: NodeKind::Part,
            parent: dir(120),
            name: "dirty",
            facts: Facts {
                part: Some(Part::Dirty),
                ..read(Tally::of(FileKind::Source, true, MIB))
            },
            class: Class::Unique,
            proofs: vec![],
            fate: Fate::None,
        },
        Case {
            id: part(120, Part::Ignored("target".into())),
            kind: NodeKind::Part,
            parent: dir(120),
            name: "target",
            facts: Facts {
                part: Some(Part::Ignored("target".into())),
                root: Root {
                    cachedir_tag: Some(CARGO_TAG.into()),
                    name: "target".into(),
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Libraries, false, 15 * GIB))
            },
            class: Class::Build,
            proofs: vec![Proof::Marker],
            fate: Fate::None,
        },
        // A git-ignored folder with no marker: drawn by its own class, unproven.
        Case {
            id: part(120, Part::Ignored("scratch".into())),
            kind: NodeKind::Part,
            parent: dir(120),
            name: "scratch",
            facts: Facts {
                part: Some(Part::Ignored("scratch".into())),
                place: ignored(),
                ..read(Tally::of(FileKind::Data, false, 200 * MIB))
            },
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // ~/ay-bench: a corpus beside the script that downloads it (a saved rule), and
        // result files no rule explains.
        Case {
            id: dir(301),
            kind: NodeKind::Folder,
            parent: dir(300),
            name: "main2025-full",
            facts: Facts {
                rule: Some(Rule::Data {
                    source: "main2025 listing".into(),
                }),
                ..read(Tally::of(FileKind::Data, false, 52 * GIB))
            },
            class: Class::Data,
            proofs: vec![],
            fate: Fate::Stays,
        },
        Case {
            id: dir(302),
            kind: NodeKind::Folder,
            parent: dir(300),
            name: "results",
            facts: read(Tally::of(FileKind::Data, false, 2 * GIB)),
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // ~/.cargo/registry: a tool cache.
        Case {
            id: dir(311),
            kind: NodeKind::Cache,
            parent: dir(310),
            name: "registry",
            facts: Facts {
                tool_cache: true,
                ..read(Tally::of(FileKind::Packages, false, 80 * GIB))
            },
            class: Class::Cache,
            proofs: vec![Proof::ToolCache],
            fate: Fate::Remove,
        },
        // A person's notes, and a backup folder a person tagged by hand.
        Case {
            id: dir(320),
            kind: NodeKind::Folder,
            parent: region(Region::Home),
            name: "notes",
            facts: read(Tally::of(FileKind::Documents, false, GIB)),
            class: Class::Personal,
            proofs: vec![],
            fate: Fate::Stays,
        },
        Case {
            id: dir(330),
            kind: NodeKind::Folder,
            parent: region(Region::Home),
            name: "backup",
            facts: Facts {
                root: Root {
                    cachedir_tag: Some("Signature: 8a477f597d28d172789f06886806bc55\n".into()),
                    name: "backup".into(),
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Archives, false, GIB))
            },
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // data/vendor/x/target/CACHEDIR.TAG proves target, never data.
        Case {
            id: dir(341),
            kind: NodeKind::Build,
            parent: dir(340),
            name: "target",
            facts: Facts {
                root: Root {
                    cachedir_tag: Some(CARGO_TAG.into()),
                    name: "target".into(),
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Libraries, false, 3 * GIB))
            },
            class: Class::Build,
            proofs: vec![Proof::Marker],
            fate: Fate::Remove,
        },
        Case {
            id: dir(342),
            kind: NodeKind::Folder,
            parent: dir(340),
            name: "data",
            facts: read(Tally::of(FileKind::Data, false, GIB)),
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // A dependency store beside its tracked lockfile; and one written after it.
        Case {
            id: dir(350),
            kind: NodeKind::Build,
            parent: region(Region::Home),
            name: "node_modules",
            facts: Facts {
                root: Root {
                    names: vec![".package-lock.json".into()],
                    name: "node_modules".into(),
                    lockfile_tracked_unmodified: true,
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Packages, false, GIB))
            },
            class: Class::Build,
            proofs: vec![Proof::Marker],
            fate: Fate::Remove,
        },
        Case {
            id: dir(351),
            kind: NodeKind::Folder,
            parent: region(Region::Home),
            name: "node_modules",
            facts: Facts {
                root: Root {
                    names: vec![".package-lock.json".into()],
                    name: "node_modules".into(),
                    lockfile_tracked_unmodified: true,
                    written_after_package_lock: true,
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Packages, false, GIB))
            },
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // Out-of-source CMake proves itself; an in-source tree has no marker.
        Case {
            id: dir(360),
            kind: NodeKind::Build,
            parent: region(Region::Home),
            name: "proj-build",
            facts: Facts {
                root: Root {
                    names: vec!["CMakeCache.txt".into(), ".ninja_log".into()],
                    name: "proj-build".into(),
                    path: "/Users//u/proj-build".into(),
                    cmake_home: Some("/Users//u/proj".into()),
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Libraries, false, GIB))
            },
            class: Class::Build,
            proofs: vec![Proof::Marker],
            fate: Fate::Remove,
        },
        Case {
            id: dir(361),
            kind: NodeKind::Folder,
            parent: region(Region::Home),
            name: "proj2",
            facts: Facts {
                root: Root {
                    names: vec!["CMakeCache.txt".into()],
                    name: "proj2".into(),
                    path: "/Users//u/proj2".into(),
                    cmake_home: Some("/Users//u/proj2/".into()),
                    ..Root::default()
                },
                place: ignored(),
                ..read(Tally::of(FileKind::Libraries, false, GIB))
            },
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // A Cargo marker in a folder git tracks files under proves nothing.
        Case {
            id: dir(390),
            kind: NodeKind::Folder,
            parent: region(Region::Home),
            name: "vendored-target",
            facts: Facts {
                root: cargo_root("vendored-target"),
                place: Place {
                    git_ignored: true,
                    tracks_under: true,
                    ..Place::default()
                },
                ..read(Tally::of(FileKind::Libraries, false, GIB))
            },
            class: Class::Unexplained,
            proofs: vec![],
            fate: Fate::None,
        },
        // A hand-made VM image, by a saved rule; a folder not read yet.
        Case {
            id: dir(370),
            kind: NodeKind::Folder,
            parent: region(Region::Home),
            name: "vm",
            facts: Facts {
                rule: Some(Rule::Costly),
                ..read(Tally::of(FileKind::Other, false, 20 * GIB))
            },
            class: Class::Costly,
            proofs: vec![],
            fate: Fate::Stays,
        },
        Case {
            id: dir(380),
            kind: NodeKind::Folder,
            parent: region(Region::Home),
            name: "toolchains",
            facts: Facts {
                read: false,
                tally: Tally::of(FileKind::Other, false, 7 * GIB),
                ..Facts::default()
            },
            class: Class::Unread,
            proofs: vec![],
            fate: Fate::None,
        },
        // $TMPDIR/.ubd: temp, a person's PDF there included (rule 3's one exception).
        Case {
            id: dir(401),
            kind: NodeKind::Temp,
            parent: dir(400),
            name: "scratch",
            facts: Facts {
                place: Place {
                    under_temp_root: true,
                    ..Place::default()
                },
                ..read(Tally::of(FileKind::Logs, false, 18 * GIB))
            },
            class: Class::Temp,
            proofs: vec![Proof::TempRoot],
            fate: Fate::Remove,
        },
        Case {
            id: dir(402),
            kind: NodeKind::Temp,
            parent: dir(400),
            name: "report.pdf-dir",
            facts: Facts {
                place: Place {
                    under_temp_root: true,
                    ..Place::default()
                },
                ..read(Tally::of(FileKind::Documents, true, 5 * MIB))
            },
            class: Class::Temp,
            proofs: vec![Proof::TempRoot],
            fate: Fate::Remove,
        },
        Case {
            id: dir(500),
            kind: NodeKind::Folder,
            parent: region(Region::Sys),
            name: "System",
            facts: Facts {
                rule: Some(Rule::System),
                ..read(Tally::of(FileKind::Executables, false, 30 * GIB))
            },
            class: Class::System,
            proofs: vec![],
            fate: Fate::Stays,
        },
    ];
    out.iter_mut()
        .for_each(|c| c.facts.place.under_temp_root |= c.parent == dir(400));
    out
}

/// The fixture index: the cases' leaves under their containers, rolled up, with used
/// set from the regions and 8 GiB unreached (Documents, never entered).
fn fixture() -> Index {
    let mut index = Index {
        volume: V,
        ..Index::default()
    };
    for r in Region::ALL {
        index.insert(container(region(r), NodeKind::Region, None, r.word()));
    }
    let g = |i| {
        NodeId::Repository(DirKey {
            volume: V,
            inode: i,
        })
    };
    let linked = |pinned| NodeKind::Checkout {
        linked: true,
        pinned,
    };
    for (id, kind, parent, name) in [
        (g(100), NodeKind::Repository, region(Region::Git), "trust"),
        (
            dir(101),
            NodeKind::Checkout {
                linked: false,
                pinned: false,
            },
            g(100),
            "$HOME/trust",
        ),
        (
            part(101, Part::Ignored("build".into())),
            NodeKind::Part,
            dir(101),
            "ignored/build",
        ),
        (dir(110), linked(false), g(100), "vcgen-flake"),
        (dir(120), linked(false), g(100), "mem-proportional"),
        (dir(300), NodeKind::Folder, region(Region::Home), "ay-bench"),
        (dir(310), NodeKind::Folder, region(Region::Home), ".cargo"),
        (dir(340), NodeKind::Folder, region(Region::Home), "vendor-x"),
        (dir(400), NodeKind::Temp, region(Region::Tmp), ".ubd"),
    ] {
        index.insert(container(id, kind, Some(parent), name));
    }
    for c in cases() {
        let (class, evidence) = classify::classify(&c.facts);
        let mut n = Node::leaf(c.id, c.kind, c.parent, c.name, class, c.facts.tally);
        n.evidence = evidence;
        n.fate = c.fate;
        n.under_temp_root = c.facts.place.under_temp_root;
        n.as_of = 1_795_000_000;
        index.insert(n);
    }
    if let Some(n) = index.nodes.get_mut(&dir(110)) {
        n.fate = Fate::Pin;
    }
    if let Some(n) = index.nodes.get_mut(&dir(120)) {
        n.fate = Fate::Strip;
    }
    index.roll_up();
    index.unreached = 8 * GIB;
    index.used = index
        .nodes
        .values()
        .filter(|n| matches!(n.id, NodeId::Region(r) if r != Region::Free))
        .map(|n| n.bytes)
        .sum::<u64>()
        + index.unreached;
    let free = 995_000_000_000 - index.used;
    if let Some(n) = index.nodes.get_mut(&region(Region::Free)) {
        n.bytes = free;
    }
    index
}

fn law_of(r: Result<(), laws::Break>) -> Option<u8> {
    r.err().map(|b| b.law)
}

fn mutate(f: impl FnOnce(&mut Index)) -> Index {
    let mut i = fixture();
    f(&mut i);
    i
}

fn node<'a>(i: &'a mut Index, id: &NodeId) -> &'a mut Node {
    i.nodes
        .get_mut(id)
        .unwrap_or_else(|| panic!("{id} is a fixture row"))
}

#[test]
fn the_fixture_of_this_macs_cases_classes_each_as_designed_and_each_rules_mutant_fails() {
    // Every case classes as designed, with the designed proof words.
    for c in cases() {
        let (class, ev) = classify::classify(&c.facts);
        assert_eq!(
            (class, &ev.proofs),
            (c.class, &c.proofs),
            "{} ({}): {:?}",
            c.name,
            c.id,
            ev.facts
        );
    }
    let index = fixture();
    laws::check(&index).unwrap_or_else(|b| panic!("the fixture keeps the laws: {b}"));
    assert_eq!(fold::check_fold(&index), Ok(()));
    let un = index.unclassed();
    assert_eq!(
        (un.unread, un.unexplained, un.unreached),
        (7 * GIB, 8 * GIB + 200 * MIB, 8 * GIB)
    );
    // What goes: the stage, the ended worktree under its pin, only the live one's
    // build output, the cache, the proven builds, temp. Nothing protected.
    let going: Vec<String> = laws::going(&index)
        .iter()
        .map(|(n, _)| n.id.to_string())
        .collect();
    for id in [
        dir(102).to_string(),
        part(110, Part::Unpushed).to_string(),
        part(120, Part::Ignored("target".into())).to_string(),
        dir(402).to_string(),
    ] {
        assert!(going.contains(&id), "{id} goes: {going:?}");
    }
    for id in [
        part(120, Part::Unpushed),
        part(120, Part::Dirty),
        part(120, Part::Ignored("scratch".into())),
        dir(103),
    ] {
        assert!(!going.contains(&id.to_string()), "{id} stays: {going:?}");
    }

    // ---- Mutants of the classifier's rules: each flips the designed class.
    let class_of = |f: Facts| classify::classify(&f).0;
    let tgt = |root: Root| Facts {
        part: Some(Part::Ignored("target".into())),
        root,
        place: ignored(),
        ..read(Tally::of(FileKind::Libraries, false, GIB))
    };
    assert_eq!(
        class_of(tgt(Root::default())),
        Class::Unexplained,
        "no marker, no proof"
    );
    assert_eq!(
        class_of(tgt(Root {
            cachedir_tag: Some(
                "Signature: 8a477f597d28d172789f06886806bc55\n# made by hand\n".into()
            ),
            ..Root::default()
        })),
        Class::Unexplained,
        "a tag with no known producer"
    );
    assert_eq!(
        class_of(tgt(Root {
            cachedir_tag: Some("Signature: 0\n# created by cargo\n".into()),
            ..Root::default()
        })),
        Class::Unexplained,
        "an unsigned tag"
    );
    assert_eq!(
        class_of(Facts {
            place: Place::default(),
            ..tgt(cargo_root("target"))
        }),
        Class::Unexplained,
        "a marker where git does not ignore"
    );
    assert_eq!(
        class_of(Facts {
            place: Place {
                in_tool_cache: true,
                ..Place::default()
            },
            ..tgt(cargo_root("target"))
        }),
        Class::Build,
        "in a tool cache it counts"
    );
    for (path, proves) in [
        ("aarch64-apple-darwin/stage0-std", true),
        ("aarch64-apple-darwin/llvm", true),
        ("bootstrap", true),
        ("cache", true),
        ("aarch64-apple-darwin/doc", false),
        ("stage1", false),
        ("a/b/c", false),
        ("x/stage1", false),
    ] {
        assert_eq!(classify::trust_bootstrap_name(path), proves, "{path}");
    }
    let nm = |f: fn(&mut Root)| {
        let mut r = Root {
            names: vec![".package-lock.json".into()],
            name: "node_modules".into(),
            lockfile_tracked_unmodified: true,
            ..Root::default()
        };
        f(&mut r);
        classify::marker(&r)
    };
    assert_eq!(nm(|_| {}), Some(Marker::DependencyStore));
    assert_eq!(
        nm(|r| r.lockfile_tracked_unmodified = false),
        None,
        "a modified lockfile"
    );
    assert_eq!(
        nm(|r| r.written_after_package_lock = true),
        None,
        "written after its lock"
    );
    assert_eq!(nm(|r| r.names.clear()), None, "no .package-lock.json");
    assert_eq!(nm(|r| r.name = "vendor".into()), None, "not a node_modules");
    let cm = |home: &str| {
        classify::marker(&Root {
            names: vec!["CMakeCache.txt".into()],
            path: "/p/b".into(),
            cmake_home: Some(home.into()),
            ..Root::default()
        })
    };
    assert_eq!(
        (cm("/p/src"), cm("/p/b/")),
        (Some(Marker::CMake), None),
        "only out of source"
    );
    let tool = Facts {
        tool_cache: true,
        ..read(Tally::of(FileKind::Packages, false, GIB))
    };
    assert_eq!(
        (
            class_of(tool.clone()),
            class_of(Facts {
                tool_cache: false,
                ..tool
            })
        ),
        (Class::Cache, Class::Unexplained)
    );
    let pdf = Facts {
        place: Place {
            under_temp_root: true,
            ..Place::default()
        },
        ..read(Tally::of(FileKind::Documents, true, MIB))
    };
    assert_eq!(
        (
            class_of(pdf.clone()),
            class_of(Facts {
                place: Place::default(),
                ..pdf
            })
        ),
        (Class::Temp, Class::Personal),
        "rule 3's temp exception"
    );
    let corpus = Facts {
        rule: Some(Rule::Data { source: "s".into() }),
        ..read(Tally::of(FileKind::Data, false, GIB))
    };
    assert_eq!(
        (
            class_of(corpus.clone()),
            class_of(Facts {
                rule: None,
                ..corpus.clone()
            })
        ),
        (Class::Data, Class::Unexplained)
    );
    assert_eq!(
        class_of(Facts {
            read: false,
            ..corpus
        }),
        Class::Unread
    );

    // ---- Mutants of the four laws: each breaks the law it names, and only it.
    let l1 = mutate(|i| node(i, &dir(300)).bytes += 1);
    assert_eq!(
        law_of(laws::parent_equals_children_plus_rest(&l1)),
        Some(1),
        "a parent off its children"
    );
    let orphan = mutate(|i| node(i, &dir(320)).parent = Some(dir(9999)));
    assert_eq!(
        law_of(laws::parent_equals_children_plus_rest(&orphan)),
        Some(1),
        "a parent that is not a row"
    );
    let rootless = mutate(|i| node(i, &dir(320)).parent = None);
    assert_eq!(
        law_of(laws::parent_equals_children_plus_rest(&rootless)),
        Some(1),
        "a root that is not a region"
    );
    let l2 = mutate(|i| i.used += 1);
    assert_eq!(
        (
            law_of(laws::regions_plus_unreached_equal_used(&l2)),
            law_of(laws::parent_equals_children_plus_rest(&l2))
        ),
        (Some(2), None)
    );
    let l2b = mutate(|i| i.unreached -= 1);
    assert_eq!(
        law_of(laws::regions_plus_unreached_equal_used(&l2b)),
        Some(2),
        "unreached is never called kept"
    );
    let l3 = mutate(|i| *node(i, &dir(302)).classes.entry(Class::Build).or_default() += 1);
    assert_eq!(
        (
            law_of(laws::class_figures_equal_bytes(&l3)),
            law_of(laws::parent_equals_children_plus_rest(&l3))
        ),
        (Some(3), None)
    );
    let l3b = mutate(|i| node(i, &dir(302)).tally.0[FileKind::Logs as usize][0] += 1);
    assert_eq!(
        law_of(laws::class_figures_equal_bytes(&l3b)),
        Some(3),
        "bytes by kind off its bytes"
    );
    let rank = |i: &Index| {
        laws::protection_wins(i)
            .err()
            .map(|b| (b.law, b.detail.split(' ').next().map(str::to_owned)))
    };
    let p = |w: &str| Some((4, Some(w.to_owned())));
    assert_eq!(
        rank(&mutate(|i| node(i, &dir(320)).fate = Fate::Remove)),
        p("Personal"),
        "a person's notes"
    );
    assert_eq!(
        rank(&mutate(|i| {
            let n = node(i, &dir(401));
            n.under_temp_root = false;
            n.tally = Tally::of(FileKind::Media, false, n.bytes);
        })),
        p("Personal"),
        "media outside a temp root, with a temp-root proof"
    );
    assert_eq!(
        rank(&mutate(
            |i| node(i, &part(101, Part::Untracked)).fate = Fate::Remove
        )),
        p("CheckoutFiles"),
        "a main checkout's untracked files"
    );
    assert_eq!(
        rank(&mutate(|i| node(i, &dir(101)).fate = Fate::Strip)),
        None,
        "a main checkout's build output may go"
    );
    assert_eq!(
        rank(&mutate(|i| node(i, &dir(101)).fate = Fate::Pin)),
        p("CheckoutFiles"),
        "a main checkout never goes"
    );
    assert_eq!(
        rank(&mutate(|i| node(i, &dir(110)).fate = Fate::Remove)),
        p("CheckoutFiles"),
        "a linked worktree unpinned, removed without its pin"
    );
    assert_eq!(
        rank(&mutate(|i| {
            let n = node(i, &dir(110));
            n.fate = Fate::Remove;
            n.kind = NodeKind::Checkout {
                linked: true,
                pinned: true,
            };
        })),
        None,
        "once pinned and read back it may go"
    );
    assert_eq!(
        rank(&mutate(
            |i| node(i, &part(101, Part::Store)).fate = Fate::Remove
        )),
        p("GitStore"),
        "a git store, whatever proof"
    );
    assert_eq!(
        rank(&mutate(|i| {
            let n = node(i, &part(101, Part::Untracked));
            n.fate = Fate::Remove;
            n.tally = Tally::of(FileKind::Documents, false, n.bytes);
        })),
        p("Personal"),
        "a person's files outrank a checkout's"
    );
    assert_eq!(
        rank(&mutate(|i| node(i, &dir(302)).fate = Fate::Remove)),
        p("Unproven"),
        "unexplained is never enough"
    );
    assert_eq!(
        rank(&mutate(|i| node(i, &dir(380)).fate = Fate::Trim)),
        p("Unproven"),
        "unread is never enough"
    );
    assert_eq!(
        rank(&mutate(|i| {
            let n = node(i, &dir(102));
            n.evidence.proofs.push(Proof::Ended);
            n.tally = Tally::of(FileKind::Documents, false, n.bytes);
        })),
        p("Personal"),
        "protection outranks any proof"
    );

    // ---- The fold rule and the bound.
    assert!(fold::qualifies(FOLDER_MIN_BYTES, 200 * FOLDER_MIN_BYTES));
    assert!(
        !fold::qualifies(FOLDER_MIN_BYTES - 1, FOLDER_MIN_BYTES),
        "under 64 MiB"
    );
    assert!(
        !fold::qualifies(FOLDER_MIN_BYTES, 200 * FOLDER_MIN_BYTES + 1),
        "under 0.5% of its parent"
    );
    let small = mutate(|i| {
        let mut n = Node::leaf(
            dir(303),
            NodeKind::Folder,
            dir(300),
            "tiny",
            Class::Unexplained,
            Tally::of(FileKind::Data, false, MIB),
        );
        n.measured = Measured::Carried;
        i.insert(n);
        i.roll_up();
        i.used += MIB;
    });
    assert_eq!(
        fold::check_fold(&small),
        Err(dir(303)),
        "the mutant keeps a small folder as a node"
    );
    let mut folded = small.clone();
    fold::fold_small(&mut folded);
    assert_eq!(fold::check_fold(&folded), Ok(()));
    laws::check(&folded).unwrap_or_else(|b| panic!("folding keeps the laws: {b}"));
    let rest = &folded.nodes[&NodeId::Rest(Box::new(dir(300)))];
    assert_eq!(
        (rest.bytes, rest.measured, rest.class(), rest.id.to_string()),
        (
            MIB,
            Measured::Carried,
            Class::Unexplained,
            "d9c:rest".to_owned()
        )
    );
    let mut wide = fixture();
    for k in 0..(NODE_BOUND as u64 + 500) {
        let mut n = Node::leaf(
            dir(10_000 + k),
            NodeKind::Build,
            dir(400),
            "b",
            Class::Build,
            Tally::of(FileKind::Libraries, false, MIB + k),
        );
        n.under_temp_root = true;
        wide.insert(n);
    }
    wide.roll_up();
    wide.used = wide
        .nodes
        .values()
        .filter(|n| matches!(n.id, NodeId::Region(r) if r != Region::Free))
        .map(|n| n.bytes)
        .sum::<u64>()
        + wide.unreached;
    assert!(
        fold::check_bound(&wide, NODE_BOUND).is_err(),
        "the unbounded mutant"
    );
    let before = wide.nodes[&region(Region::Tmp)].bytes;
    fold::bound(&mut wide, NODE_BOUND);
    assert_eq!(fold::check_bound(&wide, NODE_BOUND), Ok(()));
    assert!(wide.nodes.len() <= NODE_BOUND);
    assert_eq!(
        wide.nodes[&region(Region::Tmp)].bytes,
        before,
        "folding moves bytes, never drops them"
    );
    assert!(
        wide.nodes
            .contains_key(&dir(10_000 + NODE_BOUND as u64 + 499)),
        "the largest stays"
    );
    assert!(!wide.nodes.contains_key(&dir(10_000)), "the smallest folds");
    laws::check(&wide).unwrap_or_else(|b| panic!("the bound keeps the laws: {b}"));

    // ---- Ids: from (volume, inode), printed in base 32, read back on their volume.
    for id in [
        region(Region::Free),
        NodeId::Repository(DirKey {
            volume: V,
            inode: 100,
        }),
        dir(u64::MAX),
        part(7, Part::Ignored("build/x".into())),
        NodeId::Rest(Box::new(part(7, Part::Store))),
    ] {
        assert_eq!(NodeId::parse(V, &id.to_string()), Some(id.clone()), "{id}");
    }
    assert_eq!(
        (
            dir(100).to_string(),
            NodeId::Repository(DirKey {
                volume: V,
                inode: 100
            })
            .to_string()
        ),
        ("d34".to_owned(), "g34".to_owned())
    );
    assert_ne!(
        NodeId::parse(V, "d34"),
        NodeId::parse(V + 1, "d34"),
        "scoped by volume"
    );
    for bad in [
        "",
        "d",
        "dI",
        "x12",
        "d1:nope",
        "d1:ignored/",
        "d1:ignored/../x",
        "g",
        "rest",
    ] {
        assert_eq!(NodeId::parse(V, bad), None, "{bad:?}");
    }
    // ---- The closed word lists read back what they print and nothing else.
    assert!(
        Class::ALL
            .iter()
            .all(|c| Class::from_word(c.word()) == Some(*c))
            && Class::ALL.len() == 13
    );
    assert!(
        Proof::ALL
            .iter()
            .all(|c| Proof::from_word(c.word()) == Some(*c))
            && Proof::ALL.len() == 9
    );
    assert!(FileKind::ALL.len() == 12 && Region::ALL.len() == 5);
    for f in [
        Fate::Stays,
        Fate::Trim,
        Fate::Remove,
        Fate::Pin,
        Fate::Strip,
        Fate::Held("live build".into()),
        Fate::None,
    ] {
        assert_eq!(Fate::from_word(&f.word()), Some(f));
    }
    for bad in ["kept", "held()", "held(a)b)", "Stays"] {
        assert_eq!(
            (Fate::from_word(bad), Class::from_word(bad)),
            (None, None),
            "{bad}"
        );
    }
    assert_eq!(
        (
            Measured::from_word("carried"),
            Measured::from_word("estimated")
        ),
        (Some(Measured::Carried), None)
    );
}

/// Sets parents from their leaves again and used from the regions, so a row a review
/// test adds breaks only the law it names.
fn settle(index: &mut Index) {
    index.roll_up();
    index.used = index
        .nodes
        .values()
        .filter(|n| matches!(n.id, NodeId::Region(r) if r != Region::Free))
        .fold(index.unreached, |s, n| s.saturating_add(n.bytes));
}

/// A row of 1 GiB of `kind` and one class, with a fate.
fn row(
    id: NodeId,
    kind: NodeKind,
    parent: NodeId,
    class: Class,
    file: FileKind,
    fate: Fate,
) -> Node {
    let mut n = Node::leaf(
        id,
        kind,
        parent,
        "review",
        class,
        Tally::of(file, false, GIB),
    );
    n.fate = fate;
    n
}

fn with(rows: Vec<Node>) -> Index {
    let mut i = fixture();
    for r in rows {
        i.insert(r);
    }
    settle(&mut i);
    i
}

#[test]
fn law_4_protects_what_sits_under_a_protected_part_or_store_and_the_personal_class() {
    let home = region(Region::Home);
    let repo = NodeId::Repository(DirKey {
        volume: V,
        inode: 100,
    });
    for (what, rows) in [
        (
            "a build folder under a main checkout's untracked part",
            vec![row(
                dir(900),
                NodeKind::Build,
                part(101, Part::Untracked),
                Class::Build,
                FileKind::Other,
                Fate::Remove,
            )],
        ),
        (
            "a folder in a repository's store, outside its checkouts",
            vec![row(
                dir(901),
                NodeKind::Folder,
                repo,
                Class::Build,
                FileKind::Other,
                Fate::Remove,
            )],
        ),
        (
            "a folder whose class is personal though its kinds are other",
            vec![row(
                dir(902),
                NodeKind::Folder,
                home.clone(),
                Class::Personal,
                FileKind::Other,
                Fate::Remove,
            )],
        ),
        (
            "documents flagged under a temp root that sit in home",
            vec![{
                let mut n = row(
                    dir(903),
                    NodeKind::Folder,
                    home,
                    Class::Temp,
                    FileKind::Documents,
                    Fate::Remove,
                );
                n.under_temp_root = true;
                n
            }],
        ),
    ] {
        let index = with(rows);
        assert_eq!(law_of(laws::check(&index)), Some(4), "{what}");
    }
}

#[test]
fn law_1_holds_ids_kinds_and_parents_to_one_tree_of_regions() {
    let home = region(Region::Home);
    let build = |id, parent| {
        row(
            id,
            NodeKind::Folder,
            parent,
            Class::Build,
            FileKind::Other,
            Fate::Remove,
        )
    };
    let mut cycle = build(dir(960), dir(960));
    cycle.fate = Fate::Remove;
    let mut tmp_in_home = fixture();
    node(&mut tmp_in_home, &region(Region::Tmp)).parent = Some(home.clone());
    settle(&mut tmp_in_home);
    for (what, index) in [
        (
            "a part with no checkout above it",
            with(vec![row(
                part(950, Part::Untracked),
                NodeKind::Part,
                home.clone(),
                Class::Build,
                FileKind::Other,
                Fate::Remove,
            )]),
        ),
        (
            "a rest row under another parent",
            with(vec![row(
                NodeId::Rest(Box::new(dir(300))),
                NodeKind::Rest,
                home.clone(),
                Class::Build,
                FileKind::Other,
                Fate::None,
            )]),
        ),
        (
            "a repository id whose kind says folder",
            with(vec![row(
                NodeId::Repository(DirKey {
                    volume: V,
                    inode: 7,
                }),
                NodeKind::Folder,
                home.clone(),
                Class::Build,
                FileKind::Other,
                Fate::Remove,
            )]),
        ),
        ("a row that is its own parent", with(vec![cycle])),
        (
            "two rows each the other's parent",
            with(vec![build(dir(961), dir(962)), build(dir(962), dir(961))]),
        ),
        ("a region under another region", tmp_in_home),
    ] {
        assert_eq!(law_of(laws::check(&index)), Some(1), "{what}");
    }
    // A loop no law refused still ends every walk.
    let index = with(vec![build(dir(963), dir(963))]);
    assert!(laws::going(&index).len() < index.nodes.len() * 2);
}

#[test]
fn a_sum_that_overflows_breaks_a_law_and_never_wraps() {
    let mut index = fixture();
    let home = region(Region::Home);
    let mut parent = row(
        dir(970),
        NodeKind::Folder,
        home,
        Class::Build,
        FileKind::Other,
        Fate::None,
    );
    for (id, bytes) in [(dir(971), u64::MAX), (dir(972), 2)] {
        let mut n = Node::leaf(
            id,
            NodeKind::Folder,
            dir(970),
            "big",
            Class::Build,
            Tally::of(FileKind::Other, false, bytes),
        );
        n.fate = Fate::None;
        index.insert(n);
    }
    // What a wrapping add makes of the two: 1.
    parent.bytes = 1;
    parent.tally = Tally::of(FileKind::Other, false, 1);
    parent.classes = BTreeMap::from([(Class::Build, 1)]);
    index.insert(parent);
    // Home and used take the wrapped 1, so only the wrapped sum is wrong.
    let home = node(&mut index, &region(Region::Home));
    home.bytes += 1;
    home.tally.add(&Tally::of(FileKind::Other, false, 1));
    *home.classes.entry(Class::Build).or_default() += 1;
    index.used += 1;
    assert_eq!(law_of(laws::check(&index)), Some(1));
    let mut tally = Tally::of(FileKind::Other, false, u64::MAX);
    tally.add(&Tally::of(FileKind::Other, true, 2));
    assert_eq!(tally.checked_total(), None);
    assert_eq!(tally.total(), u64::MAX);
}

#[test]
fn a_fold_keeps_every_fate_and_carries_growth_and_newest_write() {
    let home = region(Region::Home);
    let small = |id, fate| {
        let mut n = Node::leaf(
            id,
            NodeKind::Folder,
            home.clone(),
            "small",
            Class::Build,
            Tally::of(FileKind::Other, false, MIB),
        );
        n.fate = fate;
        n.growth = [5, 7];
        n.newest_write = Some(1_795_000_009);
        n
    };
    let mut index = with(vec![
        small(dir(980), Fate::None),
        small(dir(981), Fate::Remove),
    ]);
    fold::fold_small(&mut index);
    assert!(!index.nodes.contains_key(&dir(980)));
    assert!(
        index.nodes.contains_key(&dir(981)),
        "a fold never drops a fate"
    );
    let rest = &index.nodes[&NodeId::Rest(Box::new(home))];
    assert!(
        rest.growth[0] >= 5 && rest.growth[1] >= 7,
        "{:?}",
        rest.growth
    );
    assert!(rest.newest_write >= Some(1_795_000_009));
    laws::check(&index).unwrap_or_else(|b| panic!("{b}"));
    assert_eq!(fold::check_fold(&index), Ok(()));
}

#[test]
fn unproven_ignored_bytes_are_their_own_number_and_a_pushed_marker_proves_nothing() {
    let index = fixture();
    // The live worktree's unproven `ignored/` bytes, apart from every other
    // unexplained byte (8 GiB 200 MiB in all).
    assert_eq!(index.unclassed().unproven_ignored, GIB + 200 * MIB);

    let mut facts = read(Tally::of(FileKind::Source, false, MIB));
    facts.root = cargo_root("target");
    facts.place = ignored();
    facts.part = Some(Part::Pushed);
    let (class, evidence) = classify::classify(&facts);
    assert_ne!(class, Class::Build);
    assert!(!evidence.proofs.contains(&Proof::Marker), "{evidence:?}");
}
