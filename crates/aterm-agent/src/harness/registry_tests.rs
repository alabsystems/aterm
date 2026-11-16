// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registry's four rules (`docs/HARNESS-CONTRACT.md` §2), each over a scratch store whose
//! records are signed by a synthetic master and machine, through the same verification path an
//! installed record takes.

use super::*;
use atpkg::harness_record::{Holdings, Trust, compose, compose_from};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// The synthetic paper master.
const MASTER: [u8; 32] = [0xA5; 32];
/// The one machine on the synthetic roster.
const MACHINE: [u8; 32] = [0xB5; 32];
/// A key on no roster.
const OUTSIDER: [u8; 32] = [0xC5; 32];

fn key(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).expect("a 32-byte seed")
}

fn pubkey(seed: &[u8; 32]) -> String {
    aterm_codec::base64::encode(key(seed).public_key().as_ref()).expect("a 32-byte key")
}

fn sign(seed: &[u8; 32], bytes: &[u8]) -> Vec<u8> {
    key(seed).sign(bytes).as_ref().to_vec()
}

/// What a record is verified against: the synthetic master, and `revoked`.
fn trust(revoked: &[&str]) -> Trust {
    Trust::of(
        atpkg::Anchor::of(vec![pubkey(&MASTER)], 0),
        revoked.iter().map(|r| (*r).to_string()).collect(),
    )
}

/// The master-signed roster naming the machine. Its window closed long ago: a kept record is
/// judged by the roster that authorized it at install, not by that roster's freshness now.
fn roster() -> (Vec<u8>, Vec<u8>) {
    let bytes = aterm_update_core::roster::Roster {
        schema: 1,
        roster_seq: 7,
        valid_until: "2001-01-01T00:00:00Z".into(),
        machines: vec![aterm_update_core::roster::Machine {
            id: "m7".into(),
            pubkey: pubkey(&MACHINE),
            added_at: "2000-01-01T00:00:00Z".into(),
            not_after: Some("2001-01-01T00:00:00Z".into()),
        }],
        revoked: vec![],
    }
    .to_toml()
    .expect("a valid roster")
    .into_bytes();
    let sig = sign(&MASTER, &bytes);
    (bytes, sig)
}

fn scratch() -> (aterm_tempfile::TempDir, atpkg::store::Layout) {
    let dir = aterm_tempfile::tempdir().expect("a scratch prefix");
    let layout = atpkg::store::Layout {
        prefix: dir.path().to_path_buf(),
    };
    (dir, layout)
}

/// How a fixture record goes wrong.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// The manifest is signed by a key on no roster.
    Forged,
    /// The installed tool changes after the manifest signed its tree.
    Tampered,
    /// Only the readable `.harness` copy is there: no kept manifest.
    CopyOnly,
}

/// Install `id`'s `build` exposing `tool` with the `[harness]` table `table` in its signed
/// manifest, as an install leaves it: the tree, its ready marker, the kept records, and the
/// active `bin/` link.
fn install(
    layout: &atpkg::store::Layout,
    id: &str,
    tool: &str,
    build: u64,
    table: &str,
    fault: Fault,
) {
    let dir = layout.build_dir(id, build);
    std::fs::create_dir_all(dir.join("bin")).expect("the build's bin");
    let exe = dir.join("bin").join(tool);
    std::fs::write(&exe, format!("#!/bin/sh\necho {id} {build}\n")).expect("the tool");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }
    atpkg::store::mark_build_ready(&dir).expect("ready");
    let root = atpkg::tree::tree_root(&dir).expect("the tree folds");
    let manifest = format!(
        "schema = 1\nprogram = \"{id}\"\nversion = \"1.0.0\"\nbuild_number = {build}\n\
         exposes = [\"{tool}\"]\n\n{table}\n\n[[artifact]]\ntarget = \"{}\"\nkind = \"binary\"\n\
         asset = \"{id}-{build}.tar.zst\"\nsha256 = \"{}\"\ntree_root = \"{root}\"\nsize = 1\n",
        atpkg::cli::current_triple(),
        "0".repeat(64)
    );
    let signer = if fault == Fault::Forged {
        &OUTSIDER
    } else {
        &MACHINE
    };
    let sig = sign(signer, manifest.as_bytes());
    let (roster, roster_sig) = roster();
    let record = |suffix: &str, bytes: &[u8]| {
        std::fs::write(dir.with_file_name(format!("{build}{suffix}")), bytes).expect("a record");
    };
    record(atpkg::harness_record::RECORD_SUFFIX, table.as_bytes());
    if fault != Fault::CopyOnly {
        record(atpkg::harness_record::PKG_SUFFIX, manifest.as_bytes());
        record(atpkg::harness_record::PKG_SIG_SUFFIX, &sig);
        record(atpkg::harness_record::ROSTER_SUFFIX, &roster);
        record(atpkg::harness_record::ROSTER_SIG_SUFFIX, &roster_sig);
    }
    if fault == Fault::Tampered {
        std::fs::write(&exe, "#!/bin/sh\necho forged\n").expect("the tampered tool");
    }
    std::fs::create_dir_all(layout.prefix.join("bin")).expect("bin");
    let shim = layout.prefix.join("bin").join(tool);
    let _ = std::fs::remove_file(&shim);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&exe, &shim).expect("the shim");
    let current = layout.program_current(id);
    let _ = std::fs::remove_file(&current);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dir, &current).expect("current");
}

/// A `[harness]` table for `id` exposing `tool` and leading `leads`.
fn table(id: &str, tool: &str, leads: &[&str], extra: &str) -> String {
    let leads = leads
        .iter()
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[harness]\nid = \"{id}\"\ntool = \"{tool}\"\nleads = [{leads}]\nname = \"Test\"\n{extra}"
    )
}

/// agy-alab's `alab-harness.toml` as its repository declares it (`~/agy-alab`, 2026-11-14): it
/// leads no word, its id and tool are both `agy-alab`, and it claims under contract 4 alone.
const AGY_ALAB: &str = r#"[harness]
id        = "agy-alab"
tool      = "agy-alab"
leads     = []
contracts = [4]
name      = "Antigravity CLI"
env_strip = ["ANTIGRAVITY_", "GEMINI_"]
"#;

/// kimi-alab's `alab-harness.toml` as its repository declares it (2026-11-14).
const KIMI_ALAB: &str = r#"[harness]
id        = "kimi-alab"
tool      = "kimi-alab"
leads     = ["kimi"]
contracts = [3, 4]
name      = "Kimi Code"
env_strip = ["KIMI_", "MOONSHOT_"]
"#;

/// grok-alab's `alab-harness.toml` as its repository declares it (2026-11-14).
const GROK_ALAB: &str = r#"[harness]
id        = "grok-alab"
tool      = "grok-alab"
leads     = ["grok"]
contracts = [3, 4]
name      = "Grok Build"
env_strip = ["GROK_", "XAI_"]
"#;

/// muse-alab's `alab-harness.toml` as its repository declares it (2026-11-14): like agy-alab's,
/// it leads no word.
const MUSE_ALAB: &str = r#"[harness]
id        = "muse-alab"
tool      = "muse-alab"
leads     = []
contracts = [3, 4]
name      = "Muse Code"
env_strip = []
"#;

/// aterm.toml's `[harness]` listing `dev` and giving each `(word, id)` of `lead`.
fn config(dev: &[&str], lead: &[(&str, &str)]) -> atpkg::config::HarnessConfig {
    atpkg::config::HarnessConfig {
        dev: dev.iter().map(|d| (*d).to_string()).collect(),
        lead: lead
            .iter()
            .map(|(w, id)| ((*w).to_string(), (*id).to_string()))
            .collect(),
        ..atpkg::config::HarnessConfig::default()
    }
}

fn registry(layout: &atpkg::store::Layout, dev: &[&str], trust: &Trust) -> Registry {
    Registry::from_composition(
        compose(layout, &config(dev, &[]), trust),
        trust.clone(),
        None,
    )
}

/// Overwrite `path` with `bytes` of the same length, then put its modification time back: the
/// change a fingerprint of length and mtime cannot see.
fn swap_keeping_length_and_mtime(path: &std::path::Path, bytes: &[u8]) {
    let before = std::fs::metadata(path).expect("the file");
    assert_eq!(before.len(), bytes.len() as u64, "the same length");
    let mtime = before.modified().expect("an mtime");
    std::fs::write(path, bytes).expect("the swap");
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(mtime))
        .expect("the old mtime");
}

/// A ROW COMES ONLY FROM A VERIFIED RECORD: a record whose manifest a rostered machine signed
/// and whose installed tree hashes to the signed `tree_root` makes a row, with its agent and its
/// words; a forged signature, a tree changed after signing, a revoked signer and a readable copy
/// with no kept manifest each make none, and say why.
#[test]
fn a_row_comes_only_from_a_record_whose_signed_manifest_and_tree_root_verify() {
    let claude = table("claude-code-alab", "claude-alab", &["claude"], "");
    let (_dir, layout) = scratch();
    install(
        &layout,
        "claude-code-alab",
        "claude-alab",
        3,
        &claude,
        Fault::None,
    );
    let r = registry(&layout, &[], &trust(&[]));
    let entry = r
        .entry("claude-code-alab")
        .expect("the verified record makes a row");
    assert_eq!(entry.origin, Origin::Signed { build: 3 });
    assert_eq!(
        (entry.row.program, entry.row.vendor, entry.row.allows),
        (Program::Claude, "claude", &Scope::ALL[..])
    );
    assert_eq!(
        r.leader_of("claude").map(|e| e.row.id),
        Some("claude-code-alab")
    );
    assert!(r.rows().iter().any(|row| row.id == "claude-code-alab"));
    let exe = layout
        .build_dir("claude-code-alab", 3)
        .join("bin/claude-alab");
    assert!(
        r.admits_image(&layout, entry.row, &exe),
        "its own build runs as it"
    );

    let revoked = registry(&layout, &[], &trust(&["m7"]));
    assert!(
        revoked.entry("claude-code-alab").is_none(),
        "a revoked signer makes no row, and the id's seed goes with its refused record"
    );
    assert!(!revoked.admits_image(&layout, entry.row, &exe));
    let yanked = registry(&layout, &[], &trust(&[]).yanking(&["claude-code-alab@3"]));
    assert!(
        yanked
            .refused()
            .iter()
            .any(|f| f.id == "claude-code-alab" && f.reason.contains("yanks")),
        "a build the index yanks makes no row: {:?}",
        yanked.refused()
    );

    // THE CHECK IS MADE AGAIN AT EACH PROOF, whatever the registry read: a signer revoked since
    // refuses the claimant, and so does its tool swapped with its length and mtime kept.
    assert!(
        !r.admits_image_under(&layout, entry.row, &exe, &trust(&["m7"])),
        "a signer revoked after the read is refused at the next proof"
    );
    assert!(
        !r.admits_image_under(
            &layout,
            entry.row,
            &exe,
            &trust(&[]).yanking(&["claude-code-alab@3"])
        ),
        "a build yanked after the read is refused at the next proof"
    );
    // An update makes build 4 current while build 3's process still runs: 3 keeps its proof.
    install(
        &layout,
        "claude-code-alab",
        "claude-alab",
        4,
        &claude,
        Fault::None,
    );
    let updated = registry(&layout, &[], &trust(&[]));
    let row = updated
        .entry("claude-code-alab")
        .expect("build 4's row")
        .row;
    assert!(
        updated.admits_image(&layout, row, &exe),
        "the retained signed build 3 still proves its claim"
    );
    let swapped = std::fs::read(&exe).expect("the tool");
    let forged: Vec<u8> = swapped
        .iter()
        .map(|b| if *b == b'3' { b'9' } else { *b })
        .collect();
    assert_ne!(forged, swapped);
    swap_keeping_length_and_mtime(&exe, &forged);
    assert!(
        !updated.admits_image(&layout, row, &exe),
        "a tool swapped with its length and mtime kept is refused at the next proof"
    );

    for (fault, why) in [
        (Fault::Forged, "does not verify"),
        (Fault::Tampered, "does not hash"),
        (Fault::CopyOnly, ".pkg"),
    ] {
        let (_dir, layout) = scratch();
        install(
            &layout,
            "claude-code-alab",
            "claude-alab",
            3,
            &claude,
            fault,
        );
        let r = registry(&layout, &[], &trust(&[]));
        assert!(r.entry("claude-code-alab").is_none(), "{why}: no row");
        assert!(
            r.leader_of("claude").is_none(),
            "{why}: nothing leads claude"
        );
        let refusal = r
            .refused()
            .iter()
            .find(|f| f.id == "claude-code-alab")
            .expect("the record is refused by name");
        assert!(refusal.reason.contains(why), "{why}: {}", refusal.reason);
    }

    // A SEED ID'S BUILD THAT CARRIES A RECORD IS HELD TO IT: kimi-alab's build 9 records itself,
    // its tool is swapped, and the seed row goes with the refused record, so the swapped build
    // neither claims through the seed's image rule nor keeps leading `kimi`.
    let (_dir, layout) = scratch();
    install(
        &layout,
        "kimi-alab",
        "kimi-alab",
        9,
        KIMI_ALAB,
        Fault::Tampered,
    );
    let r = registry(&layout, &[], &trust(&[]));
    assert!(
        r.entry("kimi-alab").is_none(),
        "no row for a refused record"
    );
    assert!(r.leader_of("kimi").is_none());
    let seed = Registry::seeds();
    let kimi_row = seed.entry("kimi-alab").expect("the seed").row;
    let swapped = layout.build_dir("kimi-alab", 9).join("bin/kimi-alab");
    assert!(
        !seed.admits_image(&layout, kimi_row, &swapped),
        "a seed's image rule refuses a build whose record fails"
    );
}

/// ONE WORD, ONE LEADER: a second record leading a word another row leads is refused whole, by
/// the registry and at install; a record leading a seed's word is refused the same way.
#[test]
fn a_second_harness_for_a_led_word_is_refused() {
    let (_dir, layout) = scratch();
    install(
        &layout,
        "claude-code-alab",
        "claude-alab",
        3,
        &table("claude-code-alab", "claude-alab", &["claude"], ""),
        Fault::None,
    );
    let second = table("claude-two-alab", "claude", &["claude"], "");
    install(
        &layout,
        "claude-two-alab",
        "claude",
        1,
        &second,
        Fault::None,
    );
    let kimi = table("kimi-two-alab", "kimi", &["kimi"], "");
    install(&layout, "kimi-two-alab", "kimi", 1, &kimi, Fault::None);
    let r = registry(&layout, &[], &trust(&[]));
    assert_eq!(
        r.leader_of("claude").map(|e| e.row.id),
        Some("claude-code-alab")
    );
    assert!(r.entry("claude-two-alab").is_none());
    assert!(
        r.refused()
            .iter()
            .any(|f| f.id == "claude-two-alab"
                && f.reason.contains("already led by claude-code-alab")),
        "{:?}",
        r.refused()
    );
    assert_eq!(
        r.leader_of("kimi").map(|e| e.row.id),
        Some("kimi-alab"),
        "the seed leads kimi"
    );
    assert!(r.entry("kimi-two-alab").is_none(), "a seed's word is led");

    let at_install = atpkg::harness_record::harness_table_of(&second).expect("the table");
    let refused =
        atpkg::harness_record::admit_install(&layout, "claude-two-alab", &at_install, &trust(&[]))
            .expect_err("the install is refused");
    assert!(
        refused.contains("claude-code-alab already leads"),
        "{refused}"
    );
    let own = atpkg::harness_record::harness_table_of(&table(
        "claude-code-alab",
        "claude-alab",
        &["claude"],
        "",
    ))
    .expect("the table");
    atpkg::harness_record::admit_install(&layout, "claude-code-alab", &own, &trust(&[]))
        .expect("a harness's own next build leads its own word");
}

/// Write `declaration` as `checkout`'s `alab-harness.toml`, and `bin/<tool>` beside it.
fn checkout_at(
    layout: &atpkg::store::Layout,
    name: &str,
    tool: &str,
    declaration: Option<&str>,
) -> std::path::PathBuf {
    let checkout = layout.prefix.join(name);
    std::fs::create_dir_all(checkout.join("bin")).expect("a checkout");
    std::fs::write(checkout.join("bin").join(tool), "#!/bin/sh\n").expect("its tool");
    if let Some(declaration) = declaration {
        std::fs::write(
            checkout.join(atpkg::harness_record::DECLARATION_FILE),
            declaration,
        )
        .expect("its declaration");
    }
    checkout
}

/// THE DEVELOPMENT-LINK RULE: a link of an id with no verified row makes no row unless the
/// person lists the id in `[harness] dev`, where its checkout's declaration is the row (agy-alab's
/// own, which leads nothing, read as agy by its id); a link over a seed or verified row keeps that
/// row's `leads` and `env_strip` and takes `name` and `mark` from the checkout; a listed link whose
/// checkout declares nothing keeps the row it is linked over. A link over an installed build is
/// read through the store as `aterm pkg link` leaves it. A dev-listed link loses a word an
/// installed row leads, unless aterm.toml's `[harness] lead.<word>` names it.
#[test]
fn a_linked_build_needs_a_verified_row_or_the_persons_dev_list() {
    let (_dir, layout) = scratch();
    let checkout = checkout_at(&layout, "agy-checkout", "agy-alab", Some(AGY_ALAB));
    let kimi_checkout = checkout_at(
        &layout,
        "kimi-checkout",
        "kimi-alab",
        Some(&table(
            "kimi-alab",
            "kimi-alab",
            &["kimi", "claude"],
            "env_strip = [\"ZED_\"]",
        )),
    );
    let grok_checkout = checkout_at(&layout, "grok-checkout", "grok-alab", None);
    let holdings = Holdings {
        recorded: vec![],
        linked: vec![
            ("agy-alab".to_string(), checkout.clone()),
            ("kimi-alab".to_string(), kimi_checkout.clone()),
            ("grok-alab".to_string(), grok_checkout.clone()),
        ],
    };
    let read = |dev: &[&str]| {
        Registry::from_composition(
            compose_from(&layout, &holdings, &config(dev, &[]), &trust(&[])),
            trust(&[]),
            None,
        )
    };

    let unlisted = read(&[]);
    assert!(
        unlisted.entry("agy-alab").is_none(),
        "no verified row, not listed: no row"
    );
    assert!(
        unlisted
            .refused()
            .iter()
            .any(|f| f.id == "agy-alab" && f.reason.contains("[harness] dev")),
        "{:?}",
        unlisted.refused()
    );
    let kimi = unlisted.entry("kimi-alab").expect("the seed row stands");
    assert_eq!(
        kimi.origin,
        Origin::Linked {
            checkout: kimi_checkout.clone(),
            listed: false
        }
    );
    assert_eq!(kimi.leads, ["kimi"], "the seed's words, not the checkout's");
    assert_eq!(kimi.name, "Test", "the checkout's name");
    assert!(unlisted.leader_of("claude").is_none());
    assert!(
        !unlisted.env_strip().strips("ZED_X"),
        "nor its strip entries"
    );

    let listed = read(&["agy-alab", "grok-alab"]);
    let agy = listed
        .entry("agy-alab")
        .expect("listed: the checkout's row");
    assert_eq!(
        agy.origin,
        Origin::Linked {
            checkout: checkout.clone(),
            listed: true
        }
    );
    assert_eq!(
        (agy.row.program, agy.row.vendor, agy.name.as_str()),
        (Program::Agy, "agy", "Antigravity CLI"),
        "its own declaration: the name table reads its id as agy"
    );
    assert!(agy.leads.is_empty(), "it leads nothing");
    assert_eq!(agy.images, [Image::Checkout(checkout.clone())]);
    assert!(
        listed.admits_image(&layout, agy.row, &checkout.join("bin/agy-alab")),
        "its checkout claims"
    );
    let grok = listed
        .entry("grok-alab")
        .expect("a listed link with no declaration keeps the seed row it is linked over");
    assert_eq!(grok.leads, ["grok"]);
    assert!(
        grok.images
            .contains(&Image::Checkout(grok_checkout.clone()))
    );
    let claimant = grok_checkout.join("bin/grok-alab");
    assert!(
        listed.admits_image(&layout, grok.row, &claimant),
        "its checkout claims"
    );
    assert!(
        listed.notes().iter().any(|n| n.id == "grok-alab"),
        "the unread declaration is said as a note: {:?}",
        listed.notes()
    );
    assert!(listed.refused().iter().all(|f| f.id != "grok-alab"));

    // Over an installed, verified build, read through the store after `aterm pkg link`.
    let claude = table("claude-code-alab", "claude-alab", &["claude"], "");
    install(
        &layout,
        "claude-code-alab",
        "claude-alab",
        3,
        &claude,
        Fault::None,
    );
    let claude_checkout = checkout_at(
        &layout,
        "claude-checkout",
        "claude-alab",
        Some(
            &table("claude-code-alab", "claude-alab", &[], "")
                .replace("\"Test\"", "\"Claude Dev\""),
        ),
    );
    atpkg::linkmode::link(
        &layout,
        "claude-code-alab",
        &claude_checkout,
        &[std::path::PathBuf::from("bin/claude-alab")],
    )
    .expect("the link");
    let linked = registry(&layout, &[], &trust(&[]));
    let entry = linked
        .entry("claude-code-alab")
        .expect("a link over a verified row keeps it");
    assert_eq!(
        entry.origin,
        Origin::Linked {
            checkout: claude_checkout.clone(),
            listed: false
        }
    );
    assert_eq!(entry.leads, ["claude"], "the signed record's words");
    assert_eq!(entry.name, "Claude Dev", "the checkout's name");
    assert!(linked.admits_image(&layout, entry.row, &claude_checkout.join("bin/claude-alab")));

    // A dev-listed link leading an installed row's word loses it, unless aterm.toml names it.
    let aaa = checkout_at(
        &layout,
        "aaa-checkout",
        "claude",
        Some(&table("aaa-alab", "claude", &["claude"], "")),
    );
    let holdings = Holdings {
        recorded: vec![("claude-code-alab".to_string(), 3)],
        linked: vec![("aaa-alab".to_string(), aaa)],
    };
    let ranked = |lead: &[(&str, &str)]| {
        Registry::from_composition(
            compose_from(
                &layout,
                &holdings,
                &config(&["aaa-alab"], lead),
                &trust(&[]),
            ),
            trust(&[]),
            None,
        )
    };
    let installed_first = ranked(&[]);
    assert_eq!(
        installed_first.leader_of("claude").map(|e| e.row.id),
        Some("claude-code-alab"),
        "the installed row keeps its word"
    );
    assert!(installed_first.entry("aaa-alab").is_none());
    assert!(
        installed_first
            .refused()
            .iter()
            .any(|f| f.id == "aaa-alab" && f.reason.contains("already led by claude-code-alab")),
        "{:?}",
        installed_first.refused()
    );
    let named = ranked(&[("claude", "aaa-alab")]);
    assert_eq!(
        named.leader_of("claude").map(|e| e.row.id),
        Some("aaa-alab"),
        "[harness] lead.claude moves the word to the link"
    );
    let kept = named
        .entry("claude-code-alab")
        .expect("the installed row keeps its claim row");
    assert!(kept.leads.is_empty(), "it only cedes the word");
    assert!(named.notes().iter().any(|n| n.id == "claude-code-alab"));
}

/// `env_strip` ONLY ADDS AND SPARES PROTECTED NAMES: a record's entries join the default list,
/// which keeps every entry; a record whose entry would strip a protected name makes no row and
/// strips nothing; an entry is refused when it or a protected name is a prefix of the other; the
/// person's own list replaces the default, and records still add to it.
#[test]
fn env_strip_only_adds_and_spares_protected_names() {
    let (_dir, layout) = scratch();
    install(
        &layout,
        "claude-code-alab",
        "claude-alab",
        3,
        &table(
            "claude-code-alab",
            "claude-alab",
            &["claude"],
            "env_strip = [\"ZED_\", \"CLAUDE\"]",
        ),
        Fault::None,
    );
    install(
        &layout,
        "agy-alab",
        "agy-alab",
        2,
        &AGY_ALAB.replace(r#"["ANTIGRAVITY_", "GEMINI_"]"#, r#"["QUUX_", "PAT"]"#),
        Fault::None,
    );
    let r = registry(&layout, &[], &trust(&[]));
    let prefixes = r.env_strip().prefixes();
    for default in aterm_types::domain::DEFAULT_ENV_STRIP {
        assert!(prefixes.iter().any(|p| p == default), "{default} stays");
    }
    assert!(
        r.env_strip().strips("ZED_TOKEN"),
        "a record's entry is added"
    );
    assert_eq!(
        prefixes.len(),
        aterm_types::domain::DEFAULT_ENV_STRIP.len() + 1,
        "a repeated entry is kept once"
    );
    assert!(
        r.entry("agy-alab").is_none(),
        "an entry that strips PATH refuses its record"
    );
    assert!(!r.env_strip().strips("QUUX_A"), "and adds nothing");
    assert!(!r.env_strip().strips("PATH"));

    let person = vec!["MINE_".to_string()];
    let own = Registry::from_composition(
        compose(&layout, &config(&[], &[]), &trust(&[])),
        trust(&[]),
        Some(&person),
    );
    assert_eq!(own.env_strip().prefixes(), ["MINE_", "ZED_", "CLAUDE"]);
    let (_, refused) = EnvStrip::compose(Some(&["SSH_AGENT".to_string()]), [("x-alab", "TER")]);
    assert_eq!(
        refused.len(),
        2,
        "a protected family and a protected name are refused"
    );
    for protected in [
        "GIT_",
        "GIT_EDITOR",
        "USER",
        "USERNAME",
        "XDG_",
        "XDG_RUNTIME_DIR",
        "TERM_",
        "TERM_PROGRAM",
        "DYLD_",
        "LOGNAME",
        "TMP",
        "HOMEBREW_",
        "LANGUAGE",
        "PATHX",
        "ATERM_SESSION",
        "SSH_AGENT",
        "LC_ALL",
        "SHELLX",
    ] {
        assert!(
            aterm_types::domain::admit_env_strip_entry(protected).is_err(),
            "{protected} would strip a protected name"
        );
    }
    for allowed in ["ZED_", "CLAUDE", "MOONSHOT_", "AI_", "_DEVTOOL_"] {
        assert!(
            aterm_types::domain::admit_env_strip_entry(allowed).is_ok(),
            "{allowed}"
        );
    }
}

/// A SPAWN'S GLANCE SEES EVERY CHANGE THAT CAN MOVE THE STRIP LIST: a harness build and its
/// record landing, its `current` link moving, a link made, and a link's declaration edited each
/// move the glance a spawn takes before it starts a shell, so the window reads the registry
/// again and the next shell loses what the store now says; with nothing changed the glance
/// holds, and the spawn reads nothing. NEGATIVE CONTROL (2026-11-14): before the glance, a tab
/// spawn read nothing at all, so a record installed while the window ran changed no shell's
/// list until a config reload or a claim.
#[test]
fn a_spawns_glance_moves_with_what_the_strip_list_is_read_from() {
    let (_dir, layout) = scratch();
    std::fs::create_dir_all(layout.prefix.join("store")).expect("the store");
    // Each glanced file's modification time is put back first, so a change made within the
    // clock tick of the last one still moves the glance on a filesystem with coarse times.
    let glance = || {
        for (path, _) in &Glance::of(Some(&layout)).0 {
            let _ = std::fs::File::open(path).and_then(|f| {
                f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            });
        }
        let before = Glance::of(Some(&layout));
        before.with_declarations(&Fingerprint::of(Some(&layout), config(&["agy-alab"], &[])))
    };
    let seen = glance();
    assert!(seen.holds(), "nothing moved");

    install(&layout, "agy-alab", "agy-alab", 2, AGY_ALAB, Fault::None);
    assert!(!seen.holds(), "a build and its record landed");
    let next = layout.build_dir("agy-alab", 3);
    std::fs::create_dir_all(&next).expect("another build");
    let seen = glance();
    assert!(seen.holds());

    let current = layout.program_current("agy-alab");
    std::fs::remove_file(&current).expect("the old link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&next, &current).expect("current moves");
    assert!(!seen.holds(), "current moved");

    let checkout = checkout_at(&layout, "agy-checkout", "agy-alab", Some(AGY_ALAB));
    let seen = glance();
    atpkg::linkmode::link(
        &layout,
        "agy-alab",
        &checkout,
        &[std::path::PathBuf::from("bin/agy-alab")],
    )
    .expect("the link");
    assert!(!seen.holds(), "a link was made");
    let seen = glance();
    assert!(seen.holds());
    std::fs::write(
        checkout.join(atpkg::harness_record::DECLARATION_FILE),
        AGY_ALAB.replace(r#""GEMINI_"]"#, r#""GEMINI_", "ZED_"]"#),
    )
    .expect("the declaration is edited");
    assert!(!seen.holds(), "a link's declaration moved");
}

/// THE REGISTRY NAMES EVERY `shell_env_strip` ITEM IT CANNOT READ: an item that is not a string
/// is refused from aterm.toml beside the strings that stand, and a value that is not a list is
/// refused whole while the default list applies, so `aterm harness ls` says why a prefix is
/// missing. NEGATIVE CONTROL (2026-11-14): before, both registries refused nothing.
#[test]
fn the_registry_names_each_shell_env_strip_item_it_cannot_read() {
    let mixed = Registry::load_from(
        None,
        &atpkg::config::parse_harness("shell_env_strip = [\"MINE_\", 1]\n"),
    );
    assert_eq!(mixed.env_strip().prefixes(), ["MINE_"]);
    assert!(
        mixed
            .env_refused()
            .iter()
            .any(|r| r.origin == "aterm.toml" && r.entry == "1 (an integer)"),
        "{:?}",
        mixed.env_refused()
    );
    let string = Registry::load_from(
        None,
        &atpkg::config::parse_harness("shell_env_strip = \"CLAUDE\"\n"),
    );
    assert_eq!(*string.env_strip(), EnvStrip::default());
    assert!(
        string
            .env_refused()
            .iter()
            .any(|r| r.origin == "aterm.toml" && r.reason.contains("default list applies")),
        "{:?}",
        string.env_refused()
    );
}

/// AN INSTALL CEDES A WORD THE PERSON GAVE A LINK, AS THE REGISTRY DOES: with aaa-alab
/// dev-linked leading `claude` and aterm.toml's `[harness] lead.claude` naming it, a
/// claude-code-alab build whose record leads `claude` installs, and its row keeps everything but
/// that word; without the person's `lead` the link holds the word and the install is refused,
/// told to unlink it, not to uninstall it. NEGATIVE CONTROL (2026-11-14): before, the install
/// was refused under the person's `lead` too, so every update pass refused to stage the build.
#[test]
fn an_install_cedes_a_word_the_person_gave_a_link() {
    let (_dir, layout) = scratch();
    let aaa = checkout_at(
        &layout,
        "aaa-checkout",
        "claude",
        Some(&table("aaa-alab", "claude", &["claude"], "")),
    );
    atpkg::linkmode::link(
        &layout,
        "aaa-alab",
        &aaa,
        &[std::path::PathBuf::from("bin/claude")],
    )
    .expect("the link");
    let build = atpkg::harness_record::harness_table_of(&table(
        "claude-code-alab",
        "claude-alab",
        &["claude"],
        "",
    ))
    .expect("the table");
    let admit = |config: &atpkg::config::HarnessConfig| {
        atpkg::harness_record::admit_install_under(
            &layout,
            "claude-code-alab",
            &build,
            &trust(&[]),
            config,
        )
    };

    let named = config(&["aaa-alab"], &[("claude", "aaa-alab")]);
    admit(&named).expect("the person gave the word to the link; the build cedes it");
    install(
        &layout,
        "claude-code-alab",
        "claude-alab",
        3,
        &table("claude-code-alab", "claude-alab", &["claude"], ""),
        Fault::None,
    );
    let narrowed =
        Registry::from_composition(compose(&layout, &named, &trust(&[])), trust(&[]), None);
    assert_eq!(
        narrowed.leader_of("claude").map(|e| e.row.id),
        Some("aaa-alab")
    );
    let kept = narrowed
        .entry("claude-code-alab")
        .expect("the installed row stands, narrowed");
    assert!(kept.leads.is_empty());

    let unnamed = config(&["aaa-alab"], &[]);
    let (_other, fresh) = scratch();
    let aaa = checkout_at(
        &fresh,
        "aaa-checkout",
        "claude",
        Some(&table("aaa-alab", "claude", &["claude"], "")),
    );
    atpkg::linkmode::link(
        &fresh,
        "aaa-alab",
        &aaa,
        &[std::path::PathBuf::from("bin/claude")],
    )
    .expect("the link");
    let refused = atpkg::harness_record::admit_install_under(
        &fresh,
        "claude-code-alab",
        &build,
        &trust(&[]),
        &unnamed,
    )
    .expect_err("the link leads the word");
    assert!(
        refused.contains("aaa-alab already leads") && refused.contains("unlink it first"),
        "{refused}"
    );
}

/// A SEED ID'S CONTRACT 3 CLAIM OVER A REAL COMPOSITION (`docs/HARNESS-CONTRACT.md` §2, until
/// HX-14b deletes contract 3). claude-code-alab, which speaks only contract 3, dev-linked over its
/// seed (`aterm pkg link`, its checkout declaring nothing): its row is a link's, its contract 3
/// claim is read, and its checkout's build passes contract 3's image rule. kimi-alab with a
/// verified record on its live build 9 and an older build 8 that carries none: its signed row
/// serves contract 3 too, and build 8, which the signed row's contract 4 rule refuses, still
/// proves by contract 3's rule as it always did. A dev-listed agy-alab, no seed id, is refused
/// contract 3 and claims contract 4, and contract 3's rule admits nothing for it. kimi-alab whose
/// live build's record fails has no row: its contract 3 and contract-less claims are refused
/// unregistered, never read as foreign. NEGATIVE CONTROLS: before HX-05's review fix the linked
/// claude-code-alab and the signed kimi-alab were refused contract 3 (`ERR usage`), and the
/// rowless kimi-alab's claim was stored as a foreign claim taking every duty; a file outside the
/// checkout and a build that is not ready prove nothing.
#[test]
fn a_seed_ids_contract_3_claim_stands_over_a_link_or_a_record_and_falls_with_its_row() {
    use super::super::contract::{SCOPED, UNIVERSAL, Words, read_in};
    fn read<'a>(r: &Registry, holder: &str, words: &'a [&'a str]) -> Words<'a> {
        read_in(r.rows(), holder, words)
    }
    let v3 = ["contract=3", "scope=relaunch,upgrade", "version=0.2.0"];
    let v4 = ["contract=4", "scope=relaunch,upgrade", "version=0.3.0"];
    let contract_of = |words: Words<'_>| match words {
        Words::Scoped(claim) => Some(claim.contract),
        _ => None,
    };

    let (_dir, layout) = scratch();
    let claude_checkout = checkout_at(&layout, "claude-checkout", "claude-alab", None);
    atpkg::linkmode::link(
        &layout,
        "claude-code-alab",
        &claude_checkout,
        &[std::path::PathBuf::from("bin/claude-alab")],
    )
    .expect("the link");
    let agy_checkout = checkout_at(&layout, "agy-checkout", "agy-alab", Some(AGY_ALAB));
    atpkg::linkmode::link(
        &layout,
        "agy-alab",
        &agy_checkout,
        &[std::path::PathBuf::from("bin/agy-alab")],
    )
    .expect("the link");
    let old = layout.build_dir("kimi-alab", 8);
    std::fs::create_dir_all(old.join("bin")).expect("build 8");
    let old_exe = old.join("bin/kimi-alab");
    std::fs::write(&old_exe, "#!/bin/sh\necho kimi-alab 8\n").expect("build 8's tool");
    atpkg::store::mark_build_ready(&old).expect("build 8 ready");
    install(&layout, "kimi-alab", "kimi-alab", 9, KIMI_ALAB, Fault::None);
    let r = registry(&layout, &["agy-alab"], &trust(&[]));

    let claude = r.entry("claude-code-alab").expect("the seed row stands");
    assert_eq!(claude.row.source, Source::Linked, "a link's row");
    assert_eq!(
        contract_of(read(&r, "claude-code-alab@4242", &v3)),
        Some(SCOPED)
    );
    let built = claude_checkout.join("bin/claude-alab");
    assert!(
        atpkg::harness_record::admits_seed_image(&layout, "claude-code-alab", &built),
        "its checkout's build proves its contract 3 claim"
    );
    assert!(
        r.admits_image(&layout, claude.row, &built),
        "and contract 4's"
    );
    let stray = layout.prefix.join("stray-claude-alab");
    std::fs::write(&stray, "x").expect("a stray file");
    assert!(!atpkg::harness_record::admits_seed_image(
        &layout,
        "claude-code-alab",
        &stray
    ));

    let signed = r.entry("kimi-alab").expect("the verified record's row");
    assert_eq!(signed.row.source, Source::Signed);
    assert_eq!(contract_of(read(&r, "kimi-alab@4242", &v3)), Some(SCOPED));
    assert!(
        atpkg::harness_record::admits_seed_image(&layout, "kimi-alab", &old_exe),
        "build 8, older than the first record, proves contract 3 as it always did"
    );
    assert!(
        !r.admits_image(&layout, signed.row, &old_exe),
        "the signed row's contract 4 rule refuses a build with no record"
    );
    let unready = layout.build_dir("kimi-alab", 7);
    std::fs::create_dir_all(unready.join("bin")).expect("build 7");
    std::fs::write(unready.join("bin/kimi-alab"), "x").expect("build 7's tool");
    assert!(
        !atpkg::harness_record::admits_seed_image(
            &layout,
            "kimi-alab",
            &unready.join("bin/kimi-alab")
        ),
        "a build that is not ready"
    );

    let agy = r.entry("agy-alab").expect("listed: the checkout's row");
    assert_eq!(agy.row.source, Source::Linked);
    assert!(matches!(read(&r, "agy-alab@4242", &v3), Words::Refused(_)));
    assert_eq!(contract_of(read(&r, "agy-alab@4242", &v4)), Some(UNIVERSAL));
    assert!(
        !atpkg::harness_record::admits_seed_image(
            &layout,
            "agy-alab",
            &agy_checkout.join("bin/agy-alab")
        ),
        "contract 3's rule admits nothing for an id that is no seed id"
    );

    let (_other, failed) = scratch();
    install(
        &failed,
        "kimi-alab",
        "kimi-alab",
        9,
        KIMI_ALAB,
        Fault::Tampered,
    );
    let r = registry(&failed, &[], &trust(&[]));
    assert!(
        r.entry("kimi-alab").is_none(),
        "no row for a refused record"
    );
    for words in [&v3[..], &[], &["ttl=5000"], &v4] {
        assert_eq!(
            read(&r, "kimi-alab@4242", words),
            Words::Unregistered,
            "{words:?}"
        );
    }
    assert_eq!(
        contract_of(read(&r, "grok-alab@4242", &v3)),
        Some(SCOPED),
        "another seed's row still stands"
    );
}

/// EACH HARNESS'S OWN DECLARATION MAKES ITS ROW, WITH THE AGENT THE NAME TABLE READS
/// (`docs/HARNESS-CONTRACT.md` §2). agy-alab's, kimi-alab's, grok-alab's and muse-alab's
/// `alab-harness.toml`, as their repositories declare them, each signed into a verified record,
/// make signed rows: kimi-alab and grok-alab read as their agents by the word each leads, agy-alab
/// and muse-alab, which lead nothing, by their ids. agy-alab, which has no seed row, claims through
/// its own: its build proves the claim, its contract 4 claim is granted what it claims, contract 3
/// is refused it, and it is the row that runs agy. The name table judges only the words a row
/// leads: a row leading a word the table reads as no agent is refused, naming the word; a row
/// leading `agy` whose id and tool the table does not read stands, as agy's; a row that leads
/// nothing and whose id and tool the table does not read stands with no agent (`Program::Generic`),
/// proven and granted as any row is, and no program's row or mark is it. NEGATIVE CONTROLS: without
/// `agy-alab` in the name table, agy-alab's row has no agent and does not run agy; under the rule
/// before HX-05's review fix (the id or tool had to read as the row's agent) the agy-alab, `agy`-led
/// and agent-less records were refused (`it leads no agent aterm's name table knows`, `reads
/// neither`), and agy-alab's contract 4 claim was `ERR unproven`.
#[test]
fn each_harness_declaration_makes_its_row_with_the_agent_the_name_table_reads() {
    use super::super::contract::{UNIVERSAL, Words, read_in, row_for_in};
    let v3 = ["contract=3", "scope=relaunch,upgrade", "version=0.2.0"];
    let v4 = ["contract=4", "scope=relaunch,upgrade", "version=0.3.0"];
    let (_dir, layout) = scratch();
    let real = [
        ("agy-alab", 5, AGY_ALAB, Program::Agy, None),
        ("kimi-alab", 9, KIMI_ALAB, Program::Kimi, Some("kimi")),
        ("grok-alab", 4, GROK_ALAB, Program::Grok, Some("grok")),
        ("muse-alab", 7, MUSE_ALAB, Program::Muse, None),
    ];
    for (id, build, declaration, _, _) in real {
        install(&layout, id, id, build, declaration, Fault::None);
    }
    let r = registry(&layout, &[], &trust(&[]));
    for (id, build, _, program, lead) in real {
        let entry = r
            .entry(id)
            .unwrap_or_else(|| panic!("{id}'s record makes a row: {:?}", r.refused()));
        assert_eq!(entry.origin, Origin::Signed { build }, "{id}");
        assert_eq!(
            (entry.row.program, entry.row.vendor, entry.row.source),
            (program, program.name(), Source::Signed),
            "{id}"
        );
        assert_eq!(
            entry.leads,
            lead.map(str::to_string).into_iter().collect::<Vec<_>>(),
            "{id}"
        );
        assert_eq!(
            entry.row.allows,
            &[Scope::Relaunch, Scope::Upgrade][..],
            "{id}"
        );
        let exe = layout.build_dir(id, build).join("bin").join(id);
        assert!(
            r.admits_image(&layout, entry.row, &exe),
            "{id} runs from its own build"
        );
    }
    assert!(r.refused().is_empty(), "{:?}", r.refused());

    assert!(!is_seed("agy-alab"), "agy-alab has no seed row");
    match read_in(r.rows(), "agy-alab@4242", &v4) {
        Words::Scoped(claim) => {
            assert_eq!(claim.contract, UNIVERSAL);
            assert_eq!(
                claim.scopes.words(),
                "relaunch,upgrade",
                "a signed row is granted what it claims"
            );
        }
        other => panic!("agy-alab's contract 4 claim is read: {other:?}"),
    }
    assert!(
        matches!(read_in(r.rows(), "agy-alab@4242", &v3), Words::Refused(_)),
        "contract 3 is the seed ids' alone"
    );
    assert_eq!(
        row_for_in(r.rows(), Program::Agy).map(|row| row.id),
        Some("agy-alab"),
        "agy-alab's row is the one that runs agy"
    );

    let (_other, more) = scratch();
    let odd = [
        ("orca-alab", table("orca-alab", "orca-alab", &["orca"], "")),
        ("zed-alab", table("zed-alab", "zed-alab", &["agy"], "")),
        ("quiet-alab", table("quiet-alab", "quiet-alab", &[], "")),
    ];
    for (id, declaration) in &odd {
        install(&more, id, id, 1, declaration, Fault::None);
    }
    let r = registry(&more, &[], &trust(&[]));
    assert!(r.entry("orca-alab").is_none(), "it leads no agent");
    assert!(
        r.refused()
            .iter()
            .any(|f| f.id == "orca-alab" && f.reason.contains("`orca`")),
        "{:?}",
        r.refused()
    );
    let zed = r
        .entry("zed-alab")
        .expect("a row leading a word the table reads stands, whatever its id reads as");
    assert_eq!(zed.row.program, Program::Agy);
    assert_eq!(r.leader_of("agy").map(|e| e.row.id), Some("zed-alab"));
    let quiet = r
        .entry("quiet-alab")
        .expect("leading nothing is allowed, whatever its id reads as");
    assert_eq!(quiet.row.program, Program::Generic);
    let exe = more.build_dir("quiet-alab", 1).join("bin/quiet-alab");
    assert!(r.admits_image(&more, quiet.row, &exe));
    assert!(
        matches!(read_in(r.rows(), "quiet-alab@4242", &v4), Words::Scoped(_)),
        "a generic row's claim is read as any row's"
    );
    assert_eq!(
        row_for_in(r.rows(), Program::Generic),
        None,
        "no generic tab's harness"
    );
    assert!(r.mark_for(Program::Generic).is_none());
}

/// A READ AND THE GENERATION IT WAS INSTALLED AS ARE ONE VALUE (HX-05's review, findings 5 and
/// 11): a read of an unchanged store hands back the registry it holds with its generation and
/// installs nothing; a change to what it is read from (a dev-listed link made) makes a new read,
/// installed once with the next generation, whose rows are the changed store's; and a pass keeps
/// the pair it was handed, the old registry with the old generation, as the slot holds the new
/// registry with the new one. NEGATIVE CONTROL: before the fix the generation was a counter of its
/// own, bumped after the read was installed and read by a second call, so a pass that took the
/// registry and then the generation after another thread's read recorded the newer generation for
/// the older rows, and every later pass skipped the newer ones.
#[test]
fn a_read_and_the_generation_it_was_installed_as_are_one_value() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (_dir, layout) = scratch();
    std::fs::create_dir_all(layout.prefix.join("store")).expect("the store");
    let slot = RwLock::new(None);
    let installed = AtomicUsize::new(0);
    let read = || {
        read_into(
            &slot,
            Some(&layout),
            config(&["quiet-alab"], &[]),
            &|_: &Registry| {
                installed.fetch_add(1, Ordering::SeqCst);
            },
        )
    };

    let (first, at) = read();
    assert_eq!(at, 1, "the first read");
    assert!(first.entry("quiet-alab").is_none(), "nothing linked yet");
    let (again, same) = read();
    assert!(Arc::ptr_eq(&first, &again), "nothing moved: the same read");
    assert_eq!(same, at, "and its generation");
    assert_eq!(installed.load(Ordering::SeqCst), 1, "installed once");

    let declaration = table("quiet-alab", "quiet-alab", &[], "");
    let checkout = checkout_at(&layout, "quiet-checkout", "quiet-alab", Some(&declaration));
    atpkg::linkmode::link(
        &layout,
        "quiet-alab",
        &checkout,
        &[std::path::PathBuf::from("bin/quiet-alab")],
    )
    .expect("the link");
    let (moved, next) = read();
    assert_eq!(next, at + 1, "a new read, the next generation");
    assert!(
        moved.entry("quiet-alab").is_some(),
        "its rows are the changed store's: {:?}",
        moved.refused()
    );
    assert_eq!(installed.load(Ordering::SeqCst), 2, "installed once more");
    assert!(
        first.entry("quiet-alab").is_none(),
        "the pass that took the first read keeps its rows with its generation"
    );
    let held = slot.read().expect("the slot");
    let held = held.as_ref().expect("a read");
    assert!(Arc::ptr_eq(&held.registry, &moved));
    assert_eq!(held.generation, next);
}
