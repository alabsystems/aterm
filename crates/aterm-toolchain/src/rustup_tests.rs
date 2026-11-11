// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_set_but_empty_rustup_home_is_the_default() {
    let home = Path::new("/h");
    assert_eq!(rustup_home(None, home), PathBuf::from("/h/.rustup"));
    assert_eq!(
        rustup_home(Some(OsStr::new("")), home),
        PathBuf::from("/h/.rustup")
    );
    assert_eq!(
        rustup_home(Some(OsStr::new("/r")), home),
        PathBuf::from("/r")
    );
}

#[test]
fn a_target_is_listed_only_by_its_exact_line() {
    let listing = "aarch64-apple-darwin\nwasm32-wasip1\nwasm32-unknown-unknown-nightly\n";
    assert!(lists_target(listing, "aarch64-apple-darwin"));
    assert!(!lists_target(listing, "wasm32-unknown-unknown"));
    assert!(lists_target(
        "  wasm32-unknown-unknown  \n",
        "wasm32-unknown-unknown"
    ));
}

#[test]
fn toolchain_names_drop_rustups_markers() {
    let listing = "stable-aarch64-apple-darwin (active, default)\nnightly-2026-01-01\ntrust\n";
    assert_eq!(
        toolchain_names(listing),
        ["stable-aarch64-apple-darwin", "nightly-2026-01-01", "trust"]
    );
}

const HOST: &str = "aarch64-apple-darwin";

#[test]
fn a_toolchain_is_held_by_its_name_on_rustups_host_or_its_bare_linked_name() {
    let listing = "stable-aarch64-apple-darwin (active, default)\n1.97.1-aarch64-apple-darwin\n\
                   1.97.10-aarch64-apple-darwin\ntrust\n";
    assert!(holds_toolchain(listing, "1.97.1", HOST));
    assert!(holds_toolchain(listing, "trust", HOST));
    assert!(holds_toolchain(listing, "stable", HOST));
    assert!(
        !holds_toolchain(listing, "1.97", HOST),
        "a prefix of a release is not that release"
    );
    assert!(!holds_toolchain(listing, "1.99.1", HOST));
    assert!(!holds_toolchain("", "1.97.1", HOST));
}

/// Only rustup's host's own install of a release is that release: a cross-host install and
/// a linked toolchain that begins with the release's name are other toolchains, which
/// `rustup which --toolchain 1.97.1` never resolves to.
#[test]
fn a_cross_host_or_custom_toolchain_is_not_the_hosts_pin() {
    let listing = "1.97.1-x86_64-apple-darwin\n1.97.1-mine\n";
    assert!(!holds_toolchain(listing, "1.97.1", HOST), "{listing}");
    assert!(holds_toolchain(listing, "1.97.1", "x86_64-apple-darwin"));
}

/// rustup's host is its settings' `default_host_triple`, else this build's own triple.
#[test]
fn rustups_host_is_its_settings_default_host_else_this_builds_own() {
    let home = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        default_host(home.path()),
        NATIVE_TRIPLE,
        "no settings at all"
    );
    let settings = home.path().join("settings.toml");
    std::fs::write(&settings, "version = \"12\"\nprofile = \"minimal\"\n").unwrap();
    assert_eq!(
        default_host(home.path()),
        NATIVE_TRIPLE,
        "settings that name none"
    );
    std::fs::write(
        &settings,
        "version = \"12\"\ndefault_host_triple = \"x86_64-apple-darwin\"\n\
         [overrides]\n",
    )
    .unwrap();
    assert_eq!(default_host(home.path()), "x86_64-apple-darwin");
    std::fs::write(
        &settings,
        "version = \"12\"\n[overrides]\ndefault_host_triple = \"x86_64-apple-darwin\"\n",
    )
    .unwrap();
    assert_eq!(
        default_host(home.path()),
        NATIVE_TRIPLE,
        "a key under a table is not the setting"
    );
    assert!(
        !NATIVE_TRIPLE.is_empty(),
        "every host a lane runs on has a triple"
    );
}

#[test]
fn the_wasm32_clause_is_read_out_of_the_engines_own_line() {
    let good = "CHECK_CMD_DEFAULT=\"rustup run trust targo trust check && rustup target add \
                --toolchain 1.95.0 wasm32-unknown-unknown && sh -c \
                'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=$(rustup which --toolchain 1.95.0 \
                rustc) rustup run 1.95.0 cargo check --locked --target wasm32-unknown-unknown \
                -p aterm-wasm'\"\n";
    let clause = wasm_clause(good).unwrap();
    assert_eq!(clause.pin, "1.95.0");
    assert!(
        clause
            .clause
            .starts_with("sh -c 'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=")
    );
    for config in [
        "CHECK_CMD_DEFAULT=\"targo check\"\n",
        &good.replace(
            "rustup target add --toolchain 1.95.0 wasm32-unknown-unknown && ",
            "",
        ),
        &good.replace("RUSTC=$(rustup which --toolchain 1.95.0 rustc) ", ""),
        &good.replace("rustup run 1.95.0 cargo", "cargo"),
    ] {
        assert!(wasm_clause(config).is_err(), "{config}");
    }
}

/// An engine gate whose wasm32 clause builds on `pin`, as publish/config.sh spells it.
fn gate(pin: &str) -> String {
    format!(
        "CHECK_CMD_DEFAULT=\"rustup run trust targo trust check && rustup target add \
         --toolchain {pin} wasm32-unknown-unknown && sh -c \
         'RUSTFLAGS=-Ctarget-feature=+simd128 RUSTC=$(rustup which --toolchain {pin} rustc) \
         rustup run {pin} cargo check --locked --target wasm32-unknown-unknown -p \
         aterm-wasm'\"\n"
    )
}

/// A public snapshot carries no `publish/`, and still names the stock pin.
#[test]
fn a_public_snapshot_without_publish_names_the_stock_pin() {
    let public = aterm_tempfile::tempdir().unwrap();
    assert_eq!(stock_pin(public.path()), Ok(STOCK_PIN.to_string()));
}

/// Where the checkout holds the engine gate, the pin is held to its wasm32 clause: a clause
/// on the pin agrees, one on another release or one the gate refuses is an error naming why,
/// never a pin read off either alone. This checkout, public or not, agrees.
#[test]
fn the_stock_pin_is_held_to_the_engine_gates_wasm32_clause() {
    let repo = aterm_tempfile::manifest_dir().join("../..");
    assert_eq!(stock_pin(&repo), Ok(STOCK_PIN.to_string()));

    let tree = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tree.path().join("publish")).unwrap();
    let config = tree.path().join(STOCK_PIN_FILE);
    std::fs::write(&config, gate(STOCK_PIN)).unwrap();
    assert_eq!(stock_pin(tree.path()), Ok(STOCK_PIN.to_string()));

    std::fs::write(&config, gate("1.95.0")).unwrap();
    let other = stock_pin(tree.path()).unwrap_err();
    assert!(
        other.why.contains("builds the engine on 1.95.0")
            && other.why.contains(&format!("stock pin is {STOCK_PIN}")),
        "{other}"
    );

    std::fs::write(&config, "CHECK_CMD_DEFAULT=\"true\"\n").unwrap();
    let refused = stock_pin(tree.path()).unwrap_err();
    assert!(refused.why.contains("no wasm32 clause"), "{refused}");
}
