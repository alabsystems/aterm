// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER-1 CONFORMANCE of the index channel's writer/reader pair to the derived model
//! `AtpkgIndexPublishWalk` (crates/aterm-spec/src/derive/models_atpkg_index_publish.rs):
//! the model proves that the published index tags are one run and that the walk from
//! every verified floor lands the newest; this binds both halves to the code that ships.
//!
//! * READER — every channel the model reaches (atpkg-index-1 published, 2..=4 each
//!   absent, a draft or published) is served to the REAL tag walk
//!   ([`GithubFetcher::walk_candidates`]) as download-host HEAD answers, from every floor the
//!   model reaches, and the walk must leave the store on the newest published index.
//! * WRITER — every decision a publisher holding a baseline faces in the model (its
//!   baseline number, the number it aims at, the channel) is laid out as a fixture channel
//!   and the REAL `tools/atpkg-index.sh` runs over it with UPLOAD=1 (stub gh, curl,
//!   signers and pack mirror; no network, no key). Its outcome must be the model's one
//!   enabled decision — `Create`, `Converge` or `Refuse` — and leave the tag as the model
//!   does.
//! * NEGATIVE CONTROLS — the `Buggy = 1` decisions (a baseline counted off a draft, a
//!   number past the baseline's successor, a converge onto another publisher's bytes) are
//!   driven too: the real indexer refuses each one the mutant takes. And the hole the
//!   draft baseline makes — two numbers missing directly above the floor — is one the real
//!   walk does not cross either, so a pass here is never vacuous.

use super::GithubFetcher;
use aterm_spec::derive::{Model, atpkg_index_publish_walk_model};
use aterm_spec::interp::{self, State};
use aterm_update_core::{HeadAnswer, HttpError};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Every state `model` reaches, breadth-first over its actions.
fn reachable(model: &Model) -> Vec<State> {
    let key = |s: &State| s.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>();
    let init = model.init_state();
    let mut seen = BTreeSet::from([key(&init)]);
    let mut queue = VecDeque::from([init]);
    let mut out = Vec::new();
    while let Some(state) = queue.pop_front() {
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                if seen.insert(key(&next)) {
                    queue.push_back(next);
                }
            }
        }
        out.push(state);
    }
    out
}

/// The channel of a state: the codes of atpkg-index-2..=4 (0 absent, 1/3 a draft, 2/4
/// published; 1/2 publisher A's bytes, 3/4 another publisher's).
fn channel(state: &State) -> [i64; 3] {
    [state["t2"], state["t3"], state["t4"]]
}

/// Whether `build` answers on the download host: 1 always, 2..=4 when published.
fn published(tags: [i64; 3], build: u64) -> bool {
    match build {
        1 => true,
        2..=4 => matches!(tags[build as usize - 2], 2 | 4),
        _ => false,
    }
}

fn newest(tags: [i64; 3]) -> u64 {
    (1..=4).rev().find(|b| published(tags, *b)).unwrap_or(1)
}

/// Where the REAL walk leaves a store verified at `floor`: the newest candidate the tag
/// walk returns (it returns the floor's own quad when nothing past it is published, or a
/// run it cannot see across).
fn real_walk(floor: u64, tags: [i64; 3]) -> u64 {
    const PREFIX: &str = "https://github.com/alabsystems/aterm/releases/download/atpkg-index-";
    let fetcher = GithubFetcher::new("alabsystems".into()).with_index_floor(floor);
    let mut head = |url: &str| -> Result<HeadAnswer, HttpError> {
        let build = url
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.strip_suffix("/index.toml"))
            .and_then(|b| b.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("the walk HEADs only index tags: {url}"));
        Ok(if published(tags, build) {
            HeadAnswer {
                code: 302,
                location: Some("https://release-assets.githubusercontent.com/object".into()),
            }
        } else {
            HeadAnswer {
                code: 404,
                location: None,
            }
        })
    };
    // Every quad a candidate names downloads whole: the bind is about which number the
    // walk lands, not about an incomplete release (net.rs's own walk tests cover that).
    let mut fetch =
        |url: &str, _cap: u64| -> Result<Vec<u8>, String> { Ok(url.as_bytes().to_vec()) };
    let candidates = fetcher
        .walk_candidates(&fetcher.index_slug(), &mut head, &mut fetch)
        .expect("a walk over answered HEADs is never an error");
    candidates.first().map_or(floor, |newest| {
        newest
            .label
            .strip_prefix("atpkg-index-")
            .and_then(|b| b.parse().ok())
            .expect("a walk candidate is an index tag")
    })
}

#[test]
fn the_real_walk_lands_the_newest_index_on_every_channel_the_model_reaches() {
    let model = atpkg_index_publish_walk_model();
    let seen: BTreeSet<(u64, [i64; 3])> = reachable(&model)
        .iter()
        .map(|s| (s["floor"] as u64, channel(s)))
        .collect();
    let mut moved = 0;
    for &(floor, tags) in &seen {
        let want = newest(tags);
        assert_eq!(
            real_walk(floor, tags),
            want,
            "from floor {floor} over channel {tags:?} the walk must land atpkg-index-{want}"
        );
        moved += usize::from(want > floor);
    }
    assert!(
        moved >= 6 && seen.len() >= 20,
        "the bind must cover walks that move ({moved} of {} channels)",
        seen.len()
    );

    // NEGATIVE CONTROL: the draft baseline's hole. A's killed publish leaves a draft at 3,
    // B's read counts it and publishes 4 on it, and 2 and 3 are missing directly above a
    // store at 1. The model's invariant is violated there, and the real walk does not cross
    // it either — the gap the writer's guards exist to prevent.
    let buggy = interp::with_buggy(&model, 1);
    let hole = reachable(&buggy)
        .into_iter()
        .find(|s| s["floor"] == 1 && channel(s) == [0, 1, 4])
        .expect("the Buggy = 1 model reaches the draft-baseline hole");
    assert!(!buggy.check_invariant("WalkLandsNewest", &hole));
    assert_ne!(
        real_walk(1, [0, 1, 4]),
        4,
        "two missing numbers directly above the floor are a gap no walk crosses"
    );
}

/// A sandbox holding stub gh / curl / signers / mirror and a fixture public channel, in
/// which the REAL `tools/atpkg-index.sh` publishes — the shape `tools/test-atpkg-index-
/// publish.sh` drives it in.
struct Rig {
    dir: PathBuf,
    /// Publisher A's quad for each index number 1..=4, each built on A's previous one.
    own: BTreeMap<u64, PathBuf>,
    /// Another publisher's quad for 2..=4, built on the same baselines (other bytes).
    foreign: BTreeMap<u64, PathBuf>,
}

const OWN_VALID_UNTIL: &str = "2098-01-01T00:00:00Z";
const FOREIGN_VALID_UNTIL: &str = "2098-01-02T00:00:00Z";
const QUAD: [&str; 4] = [
    "index.toml",
    "index.toml.sig",
    "aterm-machines.toml",
    "aterm-machines.toml.sig",
];

const STUB_ATPKG: &str = r#"#!/usr/bin/env bash
case "${1:-}" in verify-index|verify-pkg) echo "stub-verified" ;; *) echo "atpkg stub: ${1:-}" >&2; exit 1 ;; esac
"#;
const STUB_KEYS: &str = r#"#!/usr/bin/env bash
case "${1:-}" in pubkey) echo "STUBPUBKEY" ;; sign) echo "stub-sig" > "$4" ;; *) echo "atpkg-keys stub: ${1:-}" >&2; exit 1 ;; esac
"#;
const STUB_MIRROR: &str = "#!/usr/bin/env bash\nexit 0\n";
/// gh over the fixture tree `@FX@/<owner>/<repo>/<tag>/<asset>`; a `.draft` file marks an
/// orphaned draft, which gh shows a publisher exactly like a published release.
const STUB_GH: &str = r#"#!/usr/bin/env bash
[[ "${1:-}" == release ]] || { echo "gh stub: $*" >&2; exit 1; }
sub="$2"; tag="${3:-}"; shift 3; repo=""; dest="."; pats=(); files=(); json=""; undraft=""
while (( $# )); do
	case "$1" in
		-R) repo="$2"; shift ;; -D) dest="$2"; shift ;; -p) pats+=("$2"); shift ;;
		--title|--notes|--jq|-q) shift ;; --json) json="$2"; shift ;; --draft=false) undraft=1 ;;
		--clobber|--prerelease|--latest=false) ;; *) files+=("$1") ;;
	esac
	shift
done
d="@FX@/$repo/$tag"
case "$sub" in
	view)
		[[ -d "$d" ]] || { echo "release not found" >&2; exit 1; }
		if [[ "$json" == isDraft ]]; then [[ -f "$d/.draft" ]] && echo true || echo false
		elif [[ -n "$json" ]]; then ls "$d"; fi ;;
	edit) [[ -d "$d" ]] || exit 1; [[ -z "$undraft" ]] || rm -f "$d/.draft" ;;
	download)
		[[ -d "$d" ]] || exit 1; mkdir -p "$dest"
		for p in "${pats[@]}"; do [[ -f "$d/$p" ]] && cp "$d/$p" "$dest/"; done ;;
	create)
		[[ -d "$d" ]] && { echo "a release with the same tag name already exists" >&2; exit 1; }
		mkdir -p "$d" && for f in "${files[@]}"; do cp "$f" "$d/"; done ;;
	upload)
		for f in "${files[@]}"; do [[ -f "$d/$(basename "$f")" ]] && exit 1; cp "$f" "$d/"; done ;;
	*) echo "gh stub: release $sub" >&2; exit 1 ;;
esac
"#;
/// curl as the download host over the same tree: a draft answers 404, like no release.
const STUB_CURL: &str = r#"#!/usr/bin/env bash
url=""; while (( $# )); do [[ "$1" == "--" ]] && { url="$2"; break; }; shift; done
rel="${url#https://github.com/}"; rel="${rel%%/releases/download/*}/${rel#*/releases/download/}"
f="@FX@/$rel"
if [[ -f "$f" && ! -f "$(dirname "$f")/.draft" ]]; then printf '302'; else printf '404'; fi
"#;

impl Rig {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "atpkg-index-publish-conformance-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let fx = dir.join("fx");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let fx_text = fx.to_str().expect("a UTF-8 temp dir");
        for (name, body) in [
            ("atpkg", STUB_ATPKG.to_string()),
            ("atpkg-keys", STUB_KEYS.to_string()),
            ("mirror", STUB_MIRROR.to_string()),
            ("gh", STUB_GH.replace("@FX@", fx_text)),
            ("curl", STUB_CURL.replace("@FX@", fx_text)),
        ] {
            let path = dir.join("bin").join(name);
            std::fs::write(&path, body).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        for (name, body) in [
            ("machine.key", "stub-key\n"),
            ("machine.toml", "id = \"m-test\"\npubkey = \"STUBPUBKEY\"\n"),
            (
                "aterm-machines.toml",
                "roster_seq = 3\nvalid_until = \"2099-01-01T00:00:00Z\"\nrevoked = []\n\
                 [[machine]]\nid = \"m-test\"\npubkey = \"STUBPUBKEY\"\n",
            ),
            ("aterm-machines.toml.sig", "sig\n"),
            (
                "pins.rs",
                "pub const PAPER_MASTER_PUBKEYS: &[&str] = &[\n    \"STUBMASTER\",\n];\n",
            ),
            ("channel-token", "channel-token\n"),
            ("programs.spec", "ay ay prebuilt-only 5\n"),
        ] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        let mut rig = Rig {
            dir,
            own: BTreeMap::new(),
            foreign: BTreeMap::new(),
        };
        let genesis = rig.dry_run("own-1", None, OWN_VALID_UNTIL);
        rig.own.insert(1, genesis);
        for n in 2..=4 {
            let base = rig.own[&(n - 1)].join("index.toml");
            let own = rig.dry_run(&format!("own-{n}"), Some(&base), OWN_VALID_UNTIL);
            let foreign = rig.dry_run(&format!("foreign-{n}"), Some(&base), FOREIGN_VALID_UNTIL);
            rig.own.insert(n, own);
            rig.foreign.insert(n, foreign);
        }
        rig
    }

    /// Run the real indexer; `(exit 0, transcript)`.
    fn index(&self, out: &Path, env: &[(&str, String)]) -> (bool, String) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let bin = self.dir.join("bin");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // HERMETIC: the indexer sees exactly the environment below and nothing of the
        // caller's (a stray token, an operator's BASELINE), so no name has to be listed
        // for removal.
        let tmp = self.dir.join("tmp");
        let _ = std::fs::create_dir_all(&tmp);
        let mut cmd = Command::new("bash");
        cmd.env_clear()
            .arg(root.join("tools/atpkg-index.sh"))
            .current_dir(&self.dir)
            .env("TMPDIR", &tmp)
            .env("HOME", self.dir.join("home"))
            .env("XDG_CONFIG_HOME", self.dir.join("home/.config"))
            .env("PATH", path)
            .env("ATPKG", bin.join("atpkg"))
            .env("ATPKG_KEYS", bin.join("atpkg-keys"))
            .env("MACHINE_KEY", self.dir.join("machine.key"))
            .env("MACHINE_PUB", self.dir.join("machine.toml"))
            .env("ROSTER", self.dir.join("aterm-machines.toml"))
            .env("PINS_FILE", self.dir.join("pins.rs"))
            .env("PROGRAMS", self.dir.join("programs.spec"))
            .env("CHANNEL", "stable")
            .env("CHANNEL_TOKEN_FILE", self.dir.join("channel-token"))
            .env("MIRROR", bin.join("mirror"))
            .env("OUT", out);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let output = cmd.output().expect("bash runs");
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), log)
    }

    /// A signed, self-verified quad for the next index over `baseline` (the genesis, index
    /// 1, when there is none), with no network.
    fn dry_run(&self, name: &str, baseline: Option<&Path>, valid_until: &str) -> PathBuf {
        let out = self.dir.join(name);
        let mut env = vec![("VALID_UNTIL", valid_until.to_string())];
        match baseline {
            Some(base) => env.push(("BASELINE", base.display().to_string())),
            None => {
                env.push(("ALLOW_NO_BASELINE", "1".into()));
                env.push(("INDEX_BUILD", "1".into()));
            }
        }
        let (ok, log) = self.index(&out, &env);
        assert!(ok, "fixture dry run {name} failed:\n{log}");
        out
    }

    fn tag_dir(&self, build: u64) -> PathBuf {
        self.dir
            .join("fx/alabsystems/aterm")
            .join(format!("atpkg-index-{build}"))
    }

    /// Lay out the public channel: 1 published, 2..=4 as `tags` codes.
    fn lay(&self, tags: [i64; 3]) {
        let _ = std::fs::remove_dir_all(self.dir.join("fx"));
        for build in 1..=4u64 {
            let code = if build == 1 {
                2
            } else {
                tags[build as usize - 2]
            };
            let source = match code {
                0 => continue,
                1 | 2 => &self.own[&build],
                _ => &self.foreign[&build],
            };
            let tag = self.tag_dir(build);
            std::fs::create_dir_all(&tag).unwrap();
            for asset in QUAD {
                std::fs::copy(source.join(asset), tag.join(asset)).unwrap();
            }
            if matches!(code, 1 | 3) {
                std::fs::write(tag.join(".draft"), "").unwrap();
            }
        }
    }

    /// The code the fixture channel now holds at `build`.
    fn code_at(&self, build: u64) -> i64 {
        let tag = self.tag_dir(build);
        let Ok(bytes) = std::fs::read(tag.join("index.toml")) else {
            return 0;
        };
        let mine = bytes == std::fs::read(self.own[&build].join("index.toml")).unwrap();
        match (mine, tag.join(".draft").exists()) {
            (true, true) => 1,
            (true, false) => 2,
            (false, true) => 3,
            (false, false) => 4,
        }
    }

    /// Publisher A, holding baseline `ba`, publishes `na` over the channel `tags`: the real
    /// indexer's exit, and the channel it leaves.
    fn publish(&self, ba: u64, na: u64, tags: [i64; 3]) -> (bool, [i64; 3], String) {
        self.lay(tags);
        let out = self
            .dir
            .join(format!("run-{ba}-{na}-{tags:?}").replace([' ', ',', '[', ']'], ""));
        let _ = std::fs::remove_dir_all(&out);
        let mut env = vec![
            ("VALID_UNTIL", OWN_VALID_UNTIL.to_string()),
            (
                "BASELINE",
                self.own[&ba].join("index.toml").display().to_string(),
            ),
            ("UPLOAD", "1".to_string()),
        ];
        if na != ba + 1 {
            env.push(("INDEX_BUILD", na.to_string()));
        }
        let (ok, log) = self.index(&out, &env);
        (ok, [self.code_at(2), self.code_at(3), self.code_at(4)], log)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn the_real_indexer_takes_the_models_decision_on_every_channel_it_reaches() {
    let model = atpkg_index_publish_walk_model();
    let buggy = interp::with_buggy(&model, 1);
    // Every decision publisher A faces while it holds a baseline: (baseline, number,
    // channel), and whether the committed model reaches it (else only the mutant does).
    let mut cases: BTreeMap<(i64, i64, [i64; 3]), bool> = BTreeMap::new();
    for (m, committed) in [(&model, true), (&buggy, false)] {
        for s in reachable(m) {
            if s["pa"] == 1 {
                cases
                    .entry((s["ba"], s["na"], channel(&s)))
                    .or_insert(committed);
            }
        }
    }
    let rig = Rig::new();
    let mut controls: BTreeSet<&str> = BTreeSet::new();
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    for &(ba, na, tags) in cases.keys() {
        let mut state = model.init_state();
        for (var, value) in [
            ("pa", 1),
            ("ba", ba),
            ("na", na),
            ("t2", tags[0]),
            ("t3", tags[1]),
            ("t4", tags[2]),
        ] {
            state.insert(var, value);
        }
        let enabled: Vec<&str> = ["CreateA", "ConvergeA", "RefuseA"]
            .into_iter()
            .filter(|a| model.action_enabled(a, &state))
            .collect();
        assert_eq!(enabled.len(), 1, "one decision at {state:?}: {enabled:?}");
        taken.insert(enabled[0]);
        let (ok, after, log) = rig.publish(ba as u64, na as u64, tags);
        let label = format!("baseline {ba}, number {na}, channel {tags:?}");
        match enabled[0] {
            "RefuseA" => {
                assert!(
                    !ok,
                    "{label}: the model refuses, the indexer published:\n{log}"
                );
                assert_eq!(after, tags, "{label}: a refusal writes nothing:\n{log}");
                // The mutant writes here: the real indexer's refusal is the guard it drops.
                if buggy.action_enabled("CreateA", &state)
                    || buggy.action_enabled("ConvergeA", &state)
                {
                    controls.insert(if na != ba + 1 {
                        "a number past the baseline's successor"
                    } else if !published(tags, ba as u64) {
                        "a baseline counted off a draft"
                    } else {
                        "a converge onto another publisher's bytes"
                    });
                }
            }
            decision => {
                assert!(
                    ok,
                    "{label}: the model takes {decision}, the indexer refused:\n{log}"
                );
                assert!(model.fire(decision, &mut state));
                if decision == "CreateA" {
                    // The stub's create publishes in one step: the model's Create, Finish.
                    assert!(model.fire("FinishA", &mut state));
                }
                assert_eq!(
                    after,
                    channel(&state),
                    "{label}: {decision} leaves the channel the model says:\n{log}"
                );
            }
        }
    }
    assert_eq!(
        controls,
        BTreeSet::from([
            "a baseline counted off a draft",
            "a converge onto another publisher's bytes",
            "a number past the baseline's successor",
        ]),
        "every mutant decision is driven, and refused, at least once"
    );
    assert_eq!(
        taken,
        BTreeSet::from(["ConvergeA", "CreateA", "RefuseA"]),
        "every decision is driven through the real indexer"
    );
    let committed = cases.values().filter(|c| **c).count();
    eprintln!(
        "index publish conformance: {} decisions ({committed} the committed model reaches)",
        cases.len()
    );
    assert!(committed >= 10 && cases.len() > committed);
}
