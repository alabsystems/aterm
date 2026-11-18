// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! HARNESS RECORDS: what a harness says it is, and the registry made from what verifies
//! (`docs/HARNESS-CONTRACT.md` §2).
//!
//! A harness declares itself in [`DECLARATION_FILE`] at its repository's root. `atpkg-publish
//! pack` copies the `[harness]` table into the package's signed manifest
//! ([`crate::manifest::PkgManifest::harness`]), and an install keeps beside the build the
//! manifest and its signature, the roster that authorized them, and a copy of the table
//! ([`kept_sidecars`]).
//!
//! The registry ([`compose`]) holds one row per harness id:
//!
//! * a **signed** row comes only from a live build whose kept manifest verifies against the keys
//!   this binary trusts, which no index this machine has seen yanks, and whose tree hashes to the
//!   manifest's `tree_root` ([`verify_kept`]); a `.harness` file alone makes nothing, and a
//!   claimant's build is verified again at each proof ([`admits_image`]);
//! * a **seed** row is compiled ([`SEEDS`]): the harnesses whose index-pinned builds carry no
//!   record yet keep the rows they had, until their builds ship one. A build that carries a
//!   record is held to it: one that fails takes its seed row with it;
//! * a **development link** over a signed or seed row keeps that row's `leads` and `env_strip`
//!   and takes `name` and `mark` from its checkout's declaration; a link of an id the person
//!   lists in aterm.toml's `[harness] dev` takes the checkout's declaration whole; any other
//!   link makes no row;
//! * one word has one leading harness: aterm.toml's `[harness] lead.<word>` gives a word to a
//!   link that leads it, then seeds, installed rows and dev-listed links lead in that order; a
//!   row leading a word another already leads is refused whole, at install ([`admit_install`])
//!   and in the registry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aterm_toml::Value;
use aterm_types::mark::{MarkSpec, Texel};

/// The declaration file at a harness repository's root.
pub const DECLARATION_FILE: &str = "alab-harness.toml";

/// `<build>.harness`: the declaration a build's signed manifest carries, as a readable copy. It
/// is a presence marker only: a row is read from the verified manifest, never from this file.
pub const RECORD_SUFFIX: &str = ".harness";
/// `<build>.pkg`: the build's signed manifest, its exact bytes.
pub const PKG_SUFFIX: &str = ".pkg";
/// `<build>.pkg.sig`: the manifest's detached signature.
pub const PKG_SIG_SUFFIX: &str = ".pkg.sig";
/// `<build>.roster`: the machine roster that authorized the manifest at install.
pub const ROSTER_SUFFIX: &str = ".roster";
/// `<build>.roster.sig`: the master's signature over the roster.
pub const ROSTER_SIG_SUFFIX: &str = ".roster.sig";

/// The largest kept manifest or roster read back.
const MAX_KEPT_BYTES: usize = 5_000_000;
/// The largest detached signature read back.
const MAX_SIG_BYTES: usize = 4096;
/// The largest declaration file read from a checkout.
pub const MAX_DECLARATION_BYTES: usize = 256 * 1024;

/// The longest agent word a harness may lead.
const MAX_WORD: usize = 32;
/// The longest product name.
const MAX_NAME: usize = 64;
/// The most words one harness may lead.
const MAX_LEADS: usize = 8;
/// The most `env_strip` entries one harness may add.
const MAX_ENV_STRIP: usize = 32;

/// A harness's declaration: the `[harness]` table of [`DECLARATION_FILE`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    /// The harness's name, equal to its atpkg package name (`<word>-alab`).
    pub id: String,
    /// The one binary the package installs.
    pub tool: String,
    /// The agent words it runs: typed in a tab, and `spawn agent=<word>`.
    pub leads: Vec<String>,
    /// The contract numbers this build claims under (read by the claim from HX-06 on).
    pub contracts: Vec<u32>,
    /// The product name aterm shows.
    pub name: String,
    /// The vendor's mark, if the record carries one.
    pub mark: Option<MarkSpec>,
    /// Prefixes of environment variables aterm removes from every shell it starts, added to its
    /// default list ([`aterm_types::domain::EnvStrip::compose`]).
    pub env_strip: Vec<String>,
}

/// The keys a declaration may hold; `atpkg-publish pack` refuses any other.
const KEYS: &[&str] = &[
    "id",
    "tool",
    "leads",
    "contracts",
    "name",
    "mark",
    "env_strip",
];
/// The keys a mark table may hold.
const MARK_KEYS: &[&str] = &[
    "word",
    "alab_word",
    "name",
    "name_paper",
    "icon",
    "icon_paper",
    "texels",
    "sd",
    "hd",
];

impl Declaration {
    /// Read a `[harness]` table. `strict` (the publisher) refuses a key it does not know;
    /// otherwise (a client reading a newer writer's record) such a key is ignored.
    ///
    /// # Errors
    /// The first rule the table breaks, as a refusal line prints it.
    pub fn from_table(table: &Value, strict: bool) -> Result<Self, String> {
        let t = table
            .as_table()
            .ok_or("the [harness] declaration is a table")?;
        if strict && let Some(key) = t.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("[harness] has no key `{key}`"));
        }
        let text = |key: &str| -> Result<String, String> {
            t.get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("[harness] `{key}` is a string"))
        };
        let words = |key: &str| -> Result<Vec<String>, String> {
            match t.get(key) {
                None => Ok(Vec::new()),
                Some(v) => v
                    .as_array()
                    .ok_or_else(|| format!("[harness] `{key}` is an array of strings"))?
                    .iter()
                    .map(|w| {
                        w.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| format!("[harness] `{key}` is an array of strings"))
                    })
                    .collect(),
            }
        };
        let id = text("id")?;
        if !crate::harness::is_harness(&id) || crate::store::ToolName::new(&id).is_none() {
            return Err(format!("`{id}` is no harness id (a word ending in -alab)"));
        }
        let tool = text("tool")?;
        if crate::store::ToolName::new(&tool).is_none() {
            return Err(format!("`{tool}` is no tool name"));
        }
        let leads = words("leads")?;
        if leads.len() > MAX_LEADS {
            return Err(format!("a harness leads at most {MAX_LEADS} words"));
        }
        for (i, word) in leads.iter().enumerate() {
            if !agent_word(word) {
                return Err(format!("`{word}` is no agent word ([a-z][a-z0-9-]*)"));
            }
            if leads[..i].contains(word) {
                return Err(format!("`{word}` is led twice"));
            }
        }
        let contracts = match t.get("contracts") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .ok_or("[harness] `contracts` is an array of contract numbers")?
                .iter()
                .map(|n| {
                    n.as_integer()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .ok_or("[harness] `contracts` is an array of contract numbers")
                })
                .collect::<Result<Vec<u32>, _>>()?,
        };
        let name = text("name")?;
        if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
            return Err("the product name is 1 to 64 printable bytes".into());
        }
        let env_strip = words("env_strip")?;
        if env_strip.len() > MAX_ENV_STRIP {
            return Err(format!(
                "a harness adds at most {MAX_ENV_STRIP} env_strip entries"
            ));
        }
        for entry in &env_strip {
            aterm_types::domain::admit_env_strip_entry(entry)
                .map_err(|why| format!("env_strip `{entry}`: {why}"))?;
        }
        let mark = t.get("mark").map(|m| mark_of(m, strict)).transpose()?;
        Ok(Self {
            id,
            tool,
            leads,
            contracts,
            name,
            mark,
            env_strip,
        })
    }

    /// Read [`DECLARATION_FILE`]'s text: its `[harness]` table, strictly or not
    /// ([`Self::from_table`]).
    ///
    /// # Errors
    /// Text that is not TOML, a file with no `[harness]` table, or the table's own refusal.
    pub fn from_file_text(text: &str, strict: bool) -> Result<Self, String> {
        let table = harness_table_of(text)?;
        Self::from_table(&table, strict)
    }
}

/// The `[harness]` table of [`DECLARATION_FILE`]'s text, as written.
///
/// # Errors
/// Text that is not TOML, or that holds no `[harness]` table.
pub fn harness_table_of(text: &str) -> Result<Value, String> {
    let root: Value =
        aterm_toml::from_str(text).map_err(|e| format!("{DECLARATION_FILE} is not TOML: {e}"))?;
    root.get("harness")
        .cloned()
        .ok_or_else(|| format!("{DECLARATION_FILE} has no [harness] table"))
}

/// Whether `word` is an agent word: `[a-z][a-z0-9-]*`, at most [`MAX_WORD`] bytes.
fn agent_word(word: &str) -> bool {
    let b = word.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_WORD
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `#RRGGBB` as `0x00RRGGBB`.
fn rgb_of(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// A `[harness.mark]` table as a [`MarkSpec`], checked ([`MarkSpec::check`]).
///
/// The table spells the parts `cli_marks.rs` compiles: `word` (and `alab_word`), the name and
/// icon inks as `#RRGGBB` strings (`name` and `icon`, with `name_paper` and `icon_paper` for a
/// paper ground when they differ), `texels` as one `"<char> ink <i>"`, `"<char> half <i>"` or
/// `"<char> hole #RRGGBB"` per master character, and the drawings `sd` (and `hd`) as rows.
fn mark_of(value: &Value, strict: bool) -> Result<MarkSpec, String> {
    let t = value.as_table().ok_or("[harness.mark] is a table")?;
    if strict && let Some(key) = t.keys().find(|k| !MARK_KEYS.contains(&k.as_str())) {
        return Err(format!("[harness.mark] has no key `{key}`"));
    }
    let strings = |key: &str| -> Result<Option<Vec<String>>, String> {
        t.get(key)
            .map(|v| {
                v.as_array()
                    .ok_or_else(|| format!("[harness.mark] `{key}` is an array of strings"))?
                    .iter()
                    .map(|s| {
                        s.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| format!("[harness.mark] `{key}` is an array of strings"))
                    })
                    .collect()
            })
            .transpose()
    };
    let inks = |key: &str| -> Result<Option<Vec<u32>>, String> {
        strings(key)?
            .map(|list| {
                list.iter()
                    .map(|c| {
                        rgb_of(c)
                            .ok_or_else(|| format!("[harness.mark] `{key}`: `{c}` is no #RRGGBB"))
                    })
                    .collect()
            })
            .transpose()
    };
    let word = t
        .get("word")
        .and_then(Value::as_str)
        .ok_or("[harness.mark] `word` is a string")?
        .to_string();
    let alab_word = match t.get("alab_word") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .ok_or("[harness.mark] `alab_word` is a string")?
                .to_string(),
        ),
    };
    let name = inks("name")?.ok_or("[harness.mark] names its `name` inks")?;
    let name_paper = inks("name_paper")?.unwrap_or_else(|| name.clone());
    let icon = inks("icon")?.ok_or("[harness.mark] names its `icon` inks")?;
    let icon_paper = inks("icon_paper")?.unwrap_or_else(|| icon.clone());
    let mut chars = Vec::new();
    let mut texels = Vec::new();
    for spec in strings("texels")?.ok_or("[harness.mark] names its `texels`")? {
        let parts: Vec<&str> = spec.split(' ').collect();
        let [ch, kind, arg] = parts.as_slice() else {
            return Err(format!("texel `{spec}` is `<char> ink|half|hole <value>`"));
        };
        let [c] = ch.as_bytes() else {
            return Err(format!("texel `{spec}` names one character"));
        };
        let ink = || {
            arg.parse::<u8>()
                .map_err(|_| format!("texel `{spec}`: an ink index"))
        };
        texels.push(match *kind {
            "ink" => Texel::Ink(ink()?),
            "half" => Texel::Half(ink()?),
            "hole" => Texel::Hole(rgb_of(arg).ok_or_else(|| format!("texel `{spec}`: #RRGGBB"))?),
            _ => return Err(format!("texel `{spec}` is ink, half or hole")),
        });
        chars.push(*c);
    }
    let spec = MarkSpec {
        word,
        alab_word,
        name: [name, name_paper],
        icon: [icon, icon_paper],
        chars,
        texels,
        sd: strings("sd")?.ok_or("[harness.mark] draws `sd`")?,
        hd: strings("hd")?,
    };
    spec.check()
        .map_err(|why| format!("[harness.mark]: {why}"))?;
    Ok(spec)
}

/// The records an install keeps beside a harness program's build: the signed manifest pair and
/// the roster pair that authorized it, and, when the manifest carries `[harness]`, the table's
/// readable copy (`.harness`, the presence marker [`Holdings`] reads). Empty for any other
/// program whose manifest carries no `[harness]`. A harness build's manifest is kept even with no
/// table, so the update pass knows a build it has examined ([`keep_beside`]).
#[must_use]
pub(crate) fn kept_sidecars(
    pkg: &crate::manifest::PkgManifest,
    (raw, sig): (&[u8], &[u8]),
    (roster, roster_sig): (&[u8], &[u8]),
) -> Vec<(&'static str, Vec<u8>)> {
    if pkg.harness.is_none() && !crate::harness::is_harness(&pkg.program) {
        return Vec::new();
    }
    let mut kept = vec![
        (PKG_SUFFIX, raw.to_vec()),
        (PKG_SIG_SUFFIX, sig.to_vec()),
        (ROSTER_SUFFIX, roster.to_vec()),
        (ROSTER_SIG_SUFFIX, roster_sig.to_vec()),
    ];
    if let Some(table) = &pkg.harness {
        let mut root = aterm_toml::Table::new();
        root.insert("harness".to_string(), table.clone());
        let copy = aterm_toml::to_string(&Value::Table(root)).unwrap_or_default();
        kept.push((RECORD_SUFFIX, copy.into_bytes()));
    }
    kept
}

/// What a kept record is verified against: the masters this binary trusts, every machine id a
/// roster this machine has seen since withdrew, and every `<program>@<build>` an index this
/// machine has seen yanks.
#[derive(Clone, Debug)]
pub struct Trust {
    anchor: crate::Anchor,
    revoked: Vec<String>,
    yanked: Vec<(String, u64)>,
}

impl Trust {
    /// The pinned masters ([`crate::PKG_TRUST_ANCHORS`]), with what `layout`'s index cache says
    /// ([`Self::refreshed`]).
    #[must_use]
    pub fn of_layout(layout: &crate::store::Layout) -> Self {
        Self::of(crate::Anchor::pinned(0), Vec::new()).refreshed(layout)
    }

    /// An explicit master keyset and deny-list, yanking nothing.
    #[must_use]
    pub const fn of(anchor: crate::Anchor, revoked: Vec<String>) -> Self {
        Self {
            anchor,
            revoked,
            yanked: Vec::new(),
        }
    }

    /// This trust that also refuses each `<program>@<build>` of `yanked`.
    #[must_use]
    pub fn yanking(mut self, yanked: &[&str]) -> Self {
        self.yanked
            .extend(yanked.iter().filter_map(|entry| yank_of(entry)));
        self.yanked.sort();
        self.yanked.dedup();
        self
    }

    /// This trust with what `layout`'s index cache says now added: the deny-list of every cached
    /// roster that verifies under these masters, fresh or not, and the `yanked` entries of every
    /// channel of every cached index such a roster's machine signed. What a proof checks a
    /// record against, so a roster or an index fetched since the registry was read refuses the
    /// record at its next proof.
    #[must_use]
    pub fn refreshed(&self, layout: &crate::store::Layout) -> Self {
        let mut trust = self.clone();
        let slug = crate::resolve_account(crate::config::cached().account()).slug();
        for c in crate::IndexCache::for_layout(layout)
            .load(&format!("github:{slug}"))
            .unwrap_or_default()
        {
            let Ok(roster) =
                crate::sig::signed_roster(&self.anchor, c.roster_bytes.clone(), &c.roster_sig)
            else {
                continue;
            };
            trust.revoked.extend(roster.revoked);
            let Ok(index) = crate::sig::verify_kept_pkg(
                &self.anchor,
                (c.roster_bytes, &c.roster_sig),
                (c.index_bytes, &c.sig),
                &[],
            )
            .and_then(|bytes| crate::manifest::parse_index(&bytes)) else {
                continue;
            };
            trust.yanked.extend(
                index
                    .channels
                    .iter()
                    .flat_map(|ch| ch.yanked.iter())
                    .filter_map(|entry| yank_of(entry)),
            );
        }
        trust.revoked.sort();
        trust.revoked.dedup();
        trust.yanked.sort();
        trust.yanked.dedup();
        trust
    }

    /// Whether an index this trust has seen yanks `program`'s `build`.
    #[must_use]
    pub fn yanks(&self, program: &str, build: u64) -> bool {
        self.yanked.iter().any(|(p, b)| p == program && *b == build)
    }
}

/// A `yanked` entry `<program>@<build>` as its parts ([`crate::gate::is_yanked`]'s grammar).
fn yank_of(entry: &str) -> Option<(String, u64)> {
    let (program, build) = entry.split_once('@')?;
    Some((program.to_string(), crate::store::parse_build_name(build)?))
}

/// A record that verified ([`verify_kept`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// Its declaration, read from the verified manifest.
    pub declaration: Declaration,
    /// The build it came from.
    pub build: u64,
    /// That build's directory.
    pub build_dir: PathBuf,
    /// The version the verified manifest names for that build.
    pub version: String,
}

/// Verify the record kept beside `program`'s `build`: no index `trust` has seen yanks it; the
/// build is ready and complete; its kept manifest verifies under `trust`
/// ([`crate::sig::verify_kept_pkg`]), parses, and names this program and build; the tree at the
/// build hashes to the manifest's `tree_root` for this target; and the manifest's `[harness]`
/// names this program, exposing exactly its `tool`.
///
/// # Errors
/// The first check that fails, as `aterm harness ls` prints it.
pub fn verify_kept(
    layout: &crate::store::Layout,
    program: &str,
    build: u64,
    trust: &Trust,
) -> Result<Verified, String> {
    if trust.yanks(program, build) {
        return Err(format!("the index yanks {program}@{build}"));
    }
    let build_dir = layout.build_dir(program, build);
    let ready = crate::store::ready_marker_path(&build_dir)
        .is_some_and(|m| std::fs::symlink_metadata(m).is_ok_and(|m| m.file_type().is_file()));
    if !ready || !crate::store::build_is_complete(&build_dir) {
        return Err("the build is not complete".into());
    }
    let read = |suffix: &str, cap: usize| -> Result<Vec<u8>, String> {
        let path = crate::store::sidecar_path(&build_dir, suffix)
            .ok_or_else(|| "the build has no file name".to_string())?;
        crate::metadata_io::read_bounded_regular(&path, cap)
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    let raw = read(PKG_SUFFIX, MAX_KEPT_BYTES)?;
    let sig = read(PKG_SIG_SUFFIX, MAX_SIG_BYTES)?;
    let roster = read(ROSTER_SUFFIX, MAX_KEPT_BYTES)?;
    let roster_sig = read(ROSTER_SIG_SUFFIX, MAX_SIG_BYTES)?;
    let verified = crate::sig::verify_kept_pkg(
        &trust.anchor,
        (roster, &roster_sig),
        (raw, &sig),
        &trust.revoked,
    )
    .map_err(|e| format!("the kept manifest does not verify ({e:?})"))?;
    let pkg = crate::manifest::parse_pkg(&verified)
        .map_err(|e| format!("the kept manifest does not parse ({e:?})"))?;
    if !pkg.is_for(program) || pkg.build_number != build {
        return Err(format!(
            "the kept manifest is not {program}'s build {build}"
        ));
    }
    let triple = crate::cli::current_triple();
    let signed_root = pkg
        .artifact_for(triple)
        .map(|a| a.tree_root.clone())
        .filter(|r| !r.is_empty())
        .ok_or_else(|| format!("the kept manifest signs no tree_root for {triple}"))?;
    match crate::trust_lock::fold_and_record(&build_dir) {
        Ok(Some(root)) if root == signed_root => {}
        Ok(Some(_)) => {
            return Err("the installed tree does not hash to the signed tree_root".into());
        }
        Ok(None) => return Err("the installed tree changed while it was hashed".into()),
        Err(e) => return Err(format!("the installed tree does not hash: {e}")),
    }
    let table = pkg
        .harness
        .as_ref()
        .ok_or("the kept manifest carries no [harness]")?;
    let declaration = Declaration::from_table(table, false)?;
    if declaration.id != program {
        return Err(format!(
            "the record names `{}`, not {program}",
            declaration.id
        ));
    }
    if pkg.exposes != [declaration.tool.clone()] {
        return Err(format!(
            "the build exposes {:?}, not its tool `{}`",
            pkg.exposes, declaration.tool
        ));
    }
    Ok(Verified {
        declaration,
        build,
        build_dir,
        version: pkg.version,
    })
}

/// Keep `pkg`'s records ([`crate::manifest::PkgManifest::kept`]) beside the complete build at
/// `build_dir`, which holds `pkg`'s signed tree, when it keeps none yet: a build staged by an
/// atpkg that kept no records, or one the store held before a named install asked for it. The
/// `.harness` copy is written last, so a build is read as recorded only once the rest is there.
/// Nothing is written for a manifest that keeps nothing, or a build that already keeps its
/// manifest.
///
/// # Errors
/// A record that could not be written.
pub(crate) fn keep_beside(
    build_dir: &Path,
    pkg: &crate::manifest::PkgManifest,
) -> std::io::Result<()> {
    if pkg.kept.is_empty() || keeps_manifest(build_dir) {
        return Ok(());
    }
    let (record, rest): (Vec<_>, Vec<_>) = pkg
        .kept
        .iter()
        .partition(|(suffix, _)| *suffix == RECORD_SUFFIX);
    for (suffix, bytes) in rest.into_iter().chain(record) {
        crate::store::write_sidecar_durably(build_dir, suffix, bytes)?;
    }
    Ok(())
}

/// Whether the build at `build_dir` keeps its signed manifest (`<build>.pkg`).
pub(crate) fn keeps_manifest(build_dir: &Path) -> bool {
    crate::store::sidecar_path(build_dir, PKG_SUFFIX)
        .is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
}

/// A compiled seed row ([`SEEDS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seed {
    /// The harness id, which is also its package.
    pub id: &'static str,
    /// The binary its package installs.
    pub tool: &'static str,
    /// The words its launcher hands over.
    pub leads: &'static [&'static str],
    /// The product it runs.
    pub name: &'static str,
}

/// The rows the index-pinned harness builds had before records existed. Each goes when its
/// harness's index-pinned build carries a record (HX-14); a signed row of the same id replaces
/// its seed. claude-code-alab leads nothing: its launcher does not hand `claude` over yet, and
/// its row exists so its `relaunch,upgrade` claim is proven and aterm's built-in lanes stand
/// down from it (`docs/DESIGN-harness-repos-2026-10-24.md` §11 D53). Its tool is `claude-alab`
/// (D19), but a seed's image rule reads the package's store builds, never a tool's name.
pub const SEEDS: &[Seed] = &[
    Seed {
        id: "grok-alab",
        tool: "grok-alab",
        leads: &["grok"],
        name: "Grok Build",
    },
    Seed {
        id: "muse-alab",
        tool: "muse-alab",
        leads: &[],
        name: "Muse Code",
    },
    Seed {
        id: "kimi-alab",
        tool: "kimi-alab",
        leads: &["kimi"],
        name: "Kimi Code",
    },
    Seed {
        id: "claude-code-alab",
        tool: "claude-alab",
        leads: &[],
        name: "Claude Code",
    },
];

/// Whether `id` is a seed id ([`SEEDS`]): one of the harnesses whose contract 3 claim aterm
/// serves, whatever row the registry composes for it, until HX-14b deletes contract 3
/// (`docs/HARNESS-CONTRACT.md` §2).
#[must_use]
pub fn is_seed(id: &str) -> bool {
    SEEDS.iter().any(|seed| seed.id == id)
}

impl Seed {
    fn declaration(self) -> Declaration {
        Declaration {
            id: self.id.to_string(),
            tool: self.tool.to_string(),
            leads: self.leads.iter().map(|w| (*w).to_string()).collect(),
            contracts: Vec::new(),
            name: self.name.to_string(),
            mark: None,
            env_strip: Vec::new(),
        }
    }
}

/// Where a registry row came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Compiled ([`SEEDS`]).
    Seed,
    /// A verified record of an installed build.
    Signed {
        /// The build.
        build: u64,
    },
    /// A development link.
    Linked {
        /// The linked checkout.
        checkout: PathBuf,
        /// Whether its data is the checkout's whole table: the id is on the person's
        /// `[harness] dev` list and its checkout's declaration reads. Otherwise it is linked
        /// over a signed or seed row, whose `leads` and `env_strip` it keeps.
        listed: bool,
    },
}

/// Where a row's claimant may run from. A store build is judged at the proof, not when the
/// registry was read ([`admits_image`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Image {
    /// A ready, complete store build of the package that carries no record, or whose record
    /// verifies at the proof: a seed row's rule.
    AnyBuild,
    /// A ready, complete store build of the package whose kept record verifies at the proof,
    /// the active build or a retained one: a signed row's rule.
    Recorded,
    /// A development link's checkout.
    Checkout(PathBuf),
}

/// One registry row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The declaration it carries.
    pub declaration: Declaration,
    /// Where it came from.
    pub origin: Origin,
    /// Where its claimant may run from.
    pub images: Vec<Image>,
}

/// A record the registry refused, or a row it narrowed, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The harness id (the store or link program).
    pub id: String,
    /// What was refused: a build (`build 12`), a link (`link <checkout>`) or a seed (`seed`).
    pub what: String,
    /// Why.
    pub reason: String,
}

/// The registry: its rows, ordered by id, what it refused, and what it narrowed in a row it kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Composition {
    /// The rows.
    pub rows: Vec<Row>,
    /// The refused records.
    pub refused: Vec<Refusal>,
    /// The rows kept with less than they asked for: a word another row leads by the person's
    /// `[harness] lead`, or a dev-listed checkout whose declaration does not read.
    pub notes: Vec<Refusal>,
}

impl Composition {
    /// The row whose id is `id`.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.declaration.id == id)
    }

    /// The row that leads `word`: the harness its launcher hands the session to.
    #[must_use]
    pub fn leader_of(&self, word: &str) -> Option<&Row> {
        self.rows
            .iter()
            .find(|r| r.declaration.leads.iter().any(|w| w == word))
    }
}

/// What a store holds that the registry reads: each harness program's live build with a kept
/// record, and each linked harness program's checkout.
#[derive(Clone, Debug, Default)]
pub struct Holdings {
    /// `(program, build)` for each harness program whose live build has a `.harness` record:
    /// the build its `bin/` shims run, or, for a linked program (whose shims run the checkout),
    /// the build `store/<program>/current` names.
    pub recorded: Vec<(String, u64)>,
    /// `(program, checkout)` for each development link of a harness program.
    pub linked: Vec<(String, PathBuf)>,
}

/// Whether `program`'s `build` carries a `.harness` record in `layout`.
fn has_record(layout: &crate::store::Layout, program: &str, build: u64) -> bool {
    crate::store::sidecar_path(&layout.build_dir(program, build), RECORD_SUFFIX)
        .is_some_and(|p| std::fs::symlink_metadata(p).is_ok())
}

impl Holdings {
    /// What `layout` holds now.
    #[must_use]
    pub fn of_layout(layout: &crate::store::Layout) -> Self {
        let linked: Vec<(String, PathBuf)> = crate::linkmode::linked_programs(layout)
            .into_iter()
            .filter(|p| crate::harness::is_harness(p))
            .filter_map(|p| crate::linkmode::linked_checkout(layout, &p).map(|c| (p, c)))
            .collect();
        let mut live: BTreeMap<String, u64> = crate::ops::active_builds(layout)
            .into_iter()
            .filter(|(program, _)| crate::harness::is_harness(program))
            .collect();
        for (program, _) in &linked {
            if let Some(build) = crate::alab::current_build(layout, program) {
                live.entry(program.clone()).or_insert(build);
            }
        }
        let recorded = live
            .into_iter()
            .filter(|(program, build)| has_record(layout, program, *build))
            .collect();
        Self { recorded, linked }
    }
}

/// The registry of `layout`, under `trust`, with aterm.toml's `[harness]` `config` (module docs).
#[must_use]
pub fn compose(
    layout: &crate::store::Layout,
    config: &crate::config::HarnessConfig,
    trust: &Trust,
) -> Composition {
    compose_from(layout, &Holdings::of_layout(layout), config, trust)
}

/// A checkout's declaration, read leniently, naming `program`.
fn checkout_declaration(checkout: &Path, program: &str) -> Result<Declaration, String> {
    let declaration = crate::metadata_io::read_bounded_regular_utf8(
        &checkout.join(DECLARATION_FILE),
        MAX_DECLARATION_BYTES,
    )
    .map_err(|e| format!("{DECLARATION_FILE}: {e}"))
    .and_then(|text| Declaration::from_file_text(&text, false))?;
    if declaration.id == program {
        Ok(declaration)
    } else {
        Err(format!("the checkout declares `{}`", declaration.id))
    }
}

/// [`compose`] over explicit holdings.
#[must_use]
pub fn compose_from(
    layout: &crate::store::Layout,
    holdings: &Holdings,
    config: &crate::config::HarnessConfig,
    trust: &Trust,
) -> Composition {
    let mut refused = Vec::new();
    let mut notes = Vec::new();
    let mut rows: BTreeMap<String, Row> = SEEDS
        .iter()
        .map(|s| {
            (
                s.id.to_string(),
                Row {
                    declaration: s.declaration(),
                    origin: Origin::Seed,
                    images: vec![Image::AnyBuild],
                },
            )
        })
        .collect();
    for (program, build) in &holdings.recorded {
        match verify_kept(layout, program, *build, trust) {
            Ok(v) => {
                rows.insert(
                    program.clone(),
                    Row {
                        declaration: v.declaration,
                        origin: Origin::Signed { build: v.build },
                        images: vec![Image::Recorded],
                    },
                );
            }
            Err(reason) => {
                // A build that carries a record is held to it: its seed goes with it, so the
                // refused build cannot claim through the seed's image rule.
                rows.remove(program);
                refused.push(Refusal {
                    id: program.clone(),
                    what: format!("build {build}"),
                    reason,
                });
            }
        }
    }
    for (program, checkout) in &holdings.linked {
        let what = format!("link {}", checkout.display());
        let listed = config.dev.iter().any(|d| d == program);
        let declared = checkout_declaration(checkout, program);
        let linked = |listed: bool| Origin::Linked {
            checkout: checkout.clone(),
            listed,
        };
        match (rows.get_mut(program), declared) {
            (Some(row), Ok(declaration)) if listed => {
                // Listed: the checkout's table whole, over whatever row the id had.
                row.declaration = declaration;
                row.origin = linked(true);
                row.images.push(Image::Checkout(checkout.clone()));
            }
            (Some(row), declared) => {
                // Over a signed or seed row: its leads and env_strip stay; the checkout's name
                // and mark, when its declaration reads, are the row's.
                match declared {
                    Ok(declaration) => {
                        row.declaration.name = declaration.name;
                        row.declaration.mark = declaration.mark;
                    }
                    Err(reason) if listed => notes.push(Refusal {
                        id: program.clone(),
                        what: what.clone(),
                        reason: format!("{reason}; the link keeps the row's data"),
                    }),
                    Err(_) => {}
                }
                row.origin = linked(false);
                row.images.push(Image::Checkout(checkout.clone()));
            }
            (None, Ok(declaration)) if listed => {
                rows.insert(
                    program.clone(),
                    Row {
                        declaration,
                        origin: linked(true),
                        images: vec![Image::Checkout(checkout.clone())],
                    },
                );
            }
            (None, Err(reason)) if listed => refused.push(Refusal {
                id: program.clone(),
                what,
                reason,
            }),
            (None, _) => refused.push(Refusal {
                id: program.clone(),
                what,
                reason: "a development link of an id with no verified row; aterm.toml's \
                         [harness] dev may list it"
                    .into(),
            }),
        }
    }
    let (named, unnamed): (Vec<Row>, Vec<Row>) = rows
        .into_values()
        .partition(|row| name_table_refusal(&row.declaration).is_none());
    for row in unnamed {
        refused.push(Refusal {
            id: row.declaration.id.clone(),
            what: row.origin.words(),
            reason: name_table_refusal(&row.declaration).unwrap_or_default(),
        });
    }
    let (rows, lead_refused, lead_notes) = one_leader_each(named, config);
    refused.extend(lead_refused);
    notes.extend(lead_notes);
    Composition {
        rows,
        refused,
        notes,
    }
}

/// Why the name table cannot carry `declaration`'s row, if it cannot: a word it leads that the
/// name table reads as no agent ([`aterm_agent_names::agent_of`]). A led word is typed in a tab
/// and named by `spawn agent=<word>`, and until the built-in engine goes (HX-28) aterm reads and
/// supervises that agent by the table. Leading nothing is allowed, and the row's id and tool need
/// not read as an agent: the row's agent is the table's for its first led word, else for its id
/// or tool, else none (`aterm_agent::harness::registry`). A harness that leads nothing and runs
/// its vendor as its child on the tab's terminal keeps the vendor's screen reader where the table
/// reads its id as that vendor (`docs/DESIGN-harness-repos-2026-10-24.md` §8.1 item 5).
#[must_use]
pub fn name_table_refusal(declaration: &Declaration) -> Option<String> {
    declaration
        .leads
        .iter()
        .find(|word| aterm_agent_names::agent_of(word).is_none())
        .map(|word| format!("it leads `{word}`, no agent aterm knows"))
}

/// The strip list `composition`'s rows make with the person's `shell_env_strip` (`None`:
/// unset): [`aterm_types::domain::EnvStrip::compose`].
#[must_use]
pub fn shell_strip_list(
    composition: &Composition,
    person: Option<&[String]>,
) -> (
    aterm_types::domain::EnvStrip,
    Vec<aterm_types::domain::EnvStripRefusal>,
) {
    aterm_types::domain::EnvStrip::compose(
        person,
        composition.rows.iter().flat_map(|r| {
            r.declaration
                .env_strip
                .iter()
                .map(|e| (r.declaration.id.as_str(), e.as_str()))
        }),
    )
}

/// Install, for this process's spawns, the strip list of the configured store's registry and
/// aterm.toml ([`aterm_types::domain::install_env_strip`]): what a process that starts a shell
/// and reads no registry otherwise calls first, so every shell aterm starts loses the same list.
pub fn install_shell_strip_list() {
    let config = crate::config::load_harness();
    let composition = crate::store::resolve_configured().map_or_else(
        || {
            compose_from(
                &crate::store::Layout {
                    prefix: PathBuf::new(),
                },
                &Holdings::default(),
                &config,
                &Trust::of(crate::Anchor::of(Vec::new(), 0), Vec::new()),
            )
        },
        |layout| compose(&layout, &config, &Trust::of_layout(&layout)),
    );
    let (strip, _) = shell_strip_list(&composition, config.env_strip.as_deref());
    aterm_types::domain::install_env_strip(strip);
}

/// Who leads a word among `rows` (§2.1). A word aterm.toml's `[harness] lead.<word>` gives a
/// development link that leads it goes to that link first. Then seeds, then installed rows
/// (signed, or a link over one, which keeps its leads), then dev-listed links, each in id order:
/// a row leading a word another already leads is refused whole, except that an installed row
/// losing a word to the person's `lead` keeps its row and only that word is dropped.
fn one_leader_each(
    mut rows: Vec<Row>,
    config: &crate::config::HarnessConfig,
) -> (Vec<Row>, Vec<Refusal>, Vec<Refusal>) {
    let rank = |row: &Row| match row.origin {
        Origin::Seed => 0,
        Origin::Signed { .. } | Origin::Linked { listed: false, .. } => 1,
        Origin::Linked { listed: true, .. } => 2,
    };
    rows.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.declaration.id.cmp(&b.declaration.id))
    });
    let mut led: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for (word, id) in &config.lead {
        let named = rows.iter().any(|r| {
            r.declaration.id == *id
                && matches!(r.origin, Origin::Linked { .. })
                && r.declaration.leads.contains(word)
        });
        if named {
            led.insert(word.clone(), (id.clone(), true));
        }
    }
    let (mut kept, mut refused, mut notes) = (Vec::new(), Vec::new(), Vec::new());
    for mut row in rows {
        let id = row.declaration.id.clone();
        let taken = |w: &String| led.get(w).filter(|(leader, _)| *leader != id).cloned();
        let blocked = row.declaration.leads.iter().find_map(|w| {
            taken(w)
                .filter(|(_, by_person)| !by_person)
                .map(|(l, _)| (w, l))
        });
        if let Some((word, leader)) = blocked {
            refused.push(Refusal {
                id,
                what: row.origin.words(),
                reason: format!("`{word}` is already led by {leader}"),
            });
            continue;
        }
        let ceded: Vec<String> = row
            .declaration
            .leads
            .iter()
            .filter(|w| taken(w).is_some())
            .cloned()
            .collect();
        for word in &ceded {
            notes.push(Refusal {
                id: id.clone(),
                what: row.origin.words(),
                reason: format!(
                    "`{word}` is led by {}, which aterm.toml's [harness] lead names",
                    led[word].0
                ),
            });
        }
        row.declaration.leads.retain(|w| !ceded.contains(w));
        for word in &row.declaration.leads {
            led.entry(word.clone()).or_insert((id.clone(), false));
        }
        kept.push(row);
    }
    kept.sort_by(|a, b| a.declaration.id.cmp(&b.declaration.id));
    (kept, refused, notes)
}

impl Origin {
    /// How a listing names it: `seed`, `build <n>` or `link <checkout>`.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Origin::Seed => "seed".into(),
            Origin::Signed { build } => format!("build {build}"),
            Origin::Linked { checkout, .. } => format!("link {}", checkout.display()),
        }
    }
}

/// Whether `layout` may install a build of `program` whose signed manifest carries `table`,
/// under aterm.toml's `[harness]` as it stands ([`admit_install_under`]).
///
/// # Errors
/// As [`admit_install_under`].
pub fn admit_install(
    layout: &crate::store::Layout,
    program: &str,
    table: &Value,
    trust: &Trust,
) -> Result<(), String> {
    admit_install_under(
        layout,
        program,
        table,
        trust,
        &crate::config::load_harness(),
    )
}

/// Whether `layout` may install a build of `program` whose signed manifest carries `table`,
/// under `config`: no other row of the registry leads a word its declaration leads, save a word
/// the person gave a development link (`[harness] lead.<word>`), which the installed row cedes
/// and keeps its row without, as [`compose`] narrows it.
///
/// # Errors
/// The word, the harness that already leads it, and how to free it.
pub fn admit_install_under(
    layout: &crate::store::Layout,
    program: &str,
    table: &Value,
    trust: &Trust,
    config: &crate::config::HarnessConfig,
) -> Result<(), String> {
    let Ok(declaration) = Declaration::from_table(table, false) else {
        // A record the registry will refuse leads nothing; the install itself is not refused.
        return Ok(());
    };
    let registry = compose(layout, config, trust);
    for word in &declaration.leads {
        let Some(leader) = registry.leader_of(word) else {
            continue;
        };
        if leader.declaration.id == program || config.lead.get(word) == Some(&leader.declaration.id)
        {
            continue;
        }
        let free = match leader.origin {
            Origin::Linked { .. } => "unlink it first",
            Origin::Seed | Origin::Signed { .. } => "uninstall it first",
        };
        return Err(format!(
            "{program} leads `{word}`, which {} already leads; {free}",
            leader.declaration.id
        ));
    }
    Ok(())
}

/// The words a pinned harness build leads: its record's, or, for a build that carries none, its
/// seed's ([`SEEDS`]); `None` for a record that does not read.
#[must_use]
pub fn pinned_leads(program: &str, harness: Option<&Value>) -> Option<Vec<String>> {
    match harness {
        Some(table) => Declaration::from_table(table, false).ok().map(|d| d.leads),
        None => Some(
            SEEDS
                .iter()
                .find(|s| s.id == program)
                .map(|s| s.leads.iter().map(|w| (*w).to_string()).collect())
                .unwrap_or_default(),
        ),
    }
}

/// One harness leads a word among `pins` (each harness and the words it leads): the first word
/// two of them lead, and the two, ordered by id.
///
/// # Errors
/// The word and both harnesses.
pub fn one_leader_per_word(pins: &[(String, Vec<String>)]) -> Result<(), String> {
    let mut leader: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    let mut sorted: Vec<&(String, Vec<String>)> = pins.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, leads) in sorted {
        for word in leads {
            if let Some(first) = leader.insert(word, id)
                && first != id
            {
                return Err(format!("{first} and {id} both lead `{word}`"));
            }
        }
    }
    Ok(())
}

/// WHAT AN IMAGE PROOF ADMITTED ([`admits_image`], [`admits_seed_image`]): the store build the
/// claimant runs from, or a development link's checkout. It is where `status` and `sessions`
/// read a harness's version from, never the text a claim states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// A ready, complete store build of the row's package.
    Build {
        /// The build's number.
        build: u64,
        /// The version that build's manifest names: the verified manifest's where the build's
        /// record was verified at this proof, else the one recorded when the build was staged
        /// ([`crate::store::recorded_version`]); `None` where the build records none.
        version: Option<String>,
    },
    /// The checkout of a development link (`aterm pkg link`), which no manifest versions.
    Checkout,
}

impl Admitted {
    /// The word a development link's checkout is shown under where a build shows its version.
    pub const LINK: &'static str = "link";

    /// The version to show for what was admitted: the build's own, [`Self::LINK`] for a
    /// checkout, `None` for a build that records no version.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Build { version, .. } => version.as_deref(),
            Self::Checkout => Some(Self::LINK),
        }
    }
}

/// What the claimant image `exe` is admitted as when it may run as `row`'s harness in `layout`,
/// judged now under `trust`; `None` when it may not. A store build the row's images admit has
/// its record verified again here ([`verify_kept`], the signature and a fresh hash of its tree),
/// so a tree changed, a signer revoked or a build yanked since the registry was read is refused
/// at this proof, and the version returned is the one that verified manifest names.
#[must_use]
pub fn admits_image(
    layout: &crate::store::Layout,
    row: &Row,
    exe: &Path,
    trust: &Trust,
) -> Option<Admitted> {
    let exe = std::fs::canonicalize(exe).ok()?;
    let id = &row.declaration.id;
    row.images.iter().find_map(|image| match image {
        Image::AnyBuild | Image::Recorded => {
            let build = store_build_holding(layout, id, &exe)?;
            let version = if has_record(layout, id, build) {
                Some(verify_kept(layout, id, build, trust).ok()?.version)
            } else if *image == Image::AnyBuild {
                crate::store::recorded_version(&layout.build_dir(id, build))
            } else {
                return None;
            };
            Some(Admitted::Build { build, version })
        }
        Image::Checkout(dir) => std::fs::canonicalize(dir)
            .is_ok_and(|d| d.parent().is_some() && exe.starts_with(d))
            .then_some(Admitted::Checkout),
    })
}

/// What the claimant image `exe` is admitted as when it may run as seed id `id`'s CONTRACT 3
/// claimant in `layout`, judged now (`docs/HARNESS-CONTRACT.md` §2): a ready, complete store
/// build of the package (with the version recorded when it was staged), or the checkout of its
/// `aterm pkg link` development link; `None` when it may not. This is the rule
/// contract 3 always had, kept whatever row the registry composed for the id (a seed, a link over
/// it, or a verified record over it), so a harness that speaks only contract 3 keeps its proven
/// claim from its checkout and from a build older than its first record; a contract 4 claim is
/// held to its row's images instead ([`admits_image`]). Any id that is not a seed id admits
/// nothing.
#[must_use]
pub fn admits_seed_image(layout: &crate::store::Layout, id: &str, exe: &Path) -> Option<Admitted> {
    if !is_seed(id) {
        return None;
    }
    let exe = std::fs::canonicalize(exe).ok()?;
    if let Some(build) = store_build_holding(layout, id, &exe) {
        return Some(Admitted::Build {
            build,
            version: crate::store::recorded_version(&layout.build_dir(id, build)),
        });
    }
    crate::linkmode::linked_checkout(layout, id)
        .and_then(|checkout| std::fs::canonicalize(checkout).ok())
        .is_some_and(|checkout| checkout.parent().is_some() && exe.starts_with(&checkout))
        .then_some(Admitted::Checkout)
}

/// The build of `store/<package>/<build>/` that holds `exe` (resolved), when it is ready and
/// complete.
fn store_build_holding(layout: &crate::store::Layout, package: &str, exe: &Path) -> Option<u64> {
    let store = std::fs::canonicalize(layout.prefix.join("store").join(package)).ok()?;
    let rest = exe.strip_prefix(&store).ok()?;
    let Some(std::path::Component::Normal(build)) = rest.components().next() else {
        return None;
    };
    let build = build.to_str()?;
    if build.is_empty() || !build.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number = build.parse::<u64>().ok()?;
    let build_dir = store.join(build);
    let ready = crate::store::ready_marker_path(&build_dir)
        .is_some_and(|m| std::fs::symlink_metadata(m).is_ok_and(|m| m.file_type().is_file()));
    (ready && crate::store::build_is_complete(&build_dir)).then_some(number)
}

#[cfg(test)]
#[path = "harness_record_tests.rs"]
mod tests;
