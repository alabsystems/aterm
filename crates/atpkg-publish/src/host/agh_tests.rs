// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::AghHost;

fn home() -> agh::Slug {
    agh::Slug::new("fixture-dev", "packs").expect("the home slug")
}

/// `dirs` under a fresh scratch root, with `text` as its agh.toml when one is given.
fn production(text: Option<&str>) -> Result<AghHost, String> {
    let root = aterm_tempfile::Builder::new()
        .prefix("atpkg-publish-agh-production-")
        .tempdir()
        .expect("scratch dir");
    let dirs = agh::config::Dirs::under(root.path());
    if let Some(text) = text {
        std::fs::create_dir_all(dirs.config.parent().expect("a parent"))
            .expect("the config parent");
        aterm_uds::private_file::create_private_dir(&dirs.config).expect("the config directory");
        aterm_uds::private_file::write_private(&dirs.config, "agh.toml", text.as_bytes())
            .expect("agh.toml");
    }
    AghHost::production(&dirs, home())
}

fn refusal(text: Option<&str>) -> String {
    match production(text) {
        Ok(_) => panic!("{text:?} built a host"),
        Err(why) => why,
    }
}

#[test]
fn production_allows_missing_or_vault_config_but_refuses_invalid_config() {
    assert!(production(None).is_ok());
    assert!(production(Some("vault = true\n")).is_ok());
    let invalid = refusal(Some("[profiles.x]\ntier = \"dev\"\nbogus = true\n"));
    assert!(invalid.contains("L1:"), "{invalid}");
    assert!(invalid.contains("bogus"), "{invalid}");
}

#[test]
fn production_builds_over_a_file_that_names_its_writers() {
    let text = "[profiles.dev]\ntoken_file = \"/nonexistent/dev.token\"\ntier = \"dev\"\n\
                login = \"fixture-dev-bot\"\n\n[[owners]]\nname = \"fixture-dev\"\n\
                tier = \"dev\"\nread = [\"dev\"]\nwrite = \"dev\"\n";
    if let Err(why) = production(Some(text)) {
        panic!("{why}");
    }
}
