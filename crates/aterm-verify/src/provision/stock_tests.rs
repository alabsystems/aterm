// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const TARGETS: &[&str] = &["x86_64-apple-darwin", "wasm32-unknown-unknown"];
const RELEASE: &[&str] = &["x86_64-apple-darwin"];
const PIN: &str = "1.97.1";
const HOST: &str = "aarch64-apple-darwin";

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn provisioned() -> Seen {
    Seen {
        fresh_home: false,
        host: HOST.into(),
        toolchains: names(&["stable-aarch64-apple-darwin", "1.97.1-aarch64-apple-darwin"]),
        pin_targets: names(TARGETS),
        pin_components: names(&[
            "cargo-aarch64-apple-darwin",
            "clippy-aarch64-apple-darwin",
            "llvm-tools-aarch64-apple-darwin",
        ]),
        release_targets: names(&[HOST, "x86_64-apple-darwin"]),
    }
}

#[test]
fn a_missing_target_or_component_is_added_to_the_pin() {
    let mut s = provisioned();
    s.pin_targets = names(&["wasm32-unknown-unknown"]);
    s.pin_components = names(&["cargo-aarch64-apple-darwin"]);
    assert_eq!(
        plan(&s, Some(PIN), TARGETS, RELEASE),
        [
            Step::PinTargets(PIN.into(), names(&["x86_64-apple-darwin"])),
            Step::PinComponents(PIN.into(), names(PIN_COMPONENTS)),
        ]
    );
    assert_eq!(
        argv(&Step::PinComponents(PIN.into(), names(PIN_COMPONENTS))).join(" "),
        format!(
            "rustup component add --toolchain 1.97.1 {}",
            PIN_COMPONENTS.join(" ")
        )
    );
    s.pin_components = names(&["cargo-aarch64-apple-darwin", "clippy-aarch64-apple-darwin"]);
    s.pin_targets = names(TARGETS);
    let lacking: Vec<String> = names(PIN_COMPONENTS)
        .into_iter()
        .filter(|c| c != "clippy")
        .collect();
    assert_eq!(
        plan(&s, Some(PIN), TARGETS, RELEASE),
        if lacking.is_empty() {
            Vec::new()
        } else {
            vec![Step::PinComponents(PIN.into(), lacking)]
        },
        "a pin with clippy lacks only what else this host's cells read"
    );
    assert_eq!(plan(&provisioned(), Some(PIN), TARGETS, RELEASE), []);
}

/// THE PIN IS PROVISIONED WHATEVER STABLE HOLDS: a machine whose floating `stable` carries
/// every cell's std and clippy, and which lacks the pin, gets the pin installed with its
/// components ([`PIN_COMPONENTS`]) and every target; nothing of the pin is asked of `stable`.
/// NEGATIVE CONTROL: a plan that counts `stable`'s stds and clippy as the pin's leaves this
/// machine as it is, and the first assertion fails.
#[test]
fn a_missing_pin_is_installed_with_clippy_and_every_target_whatever_stable_holds() {
    let mut s = provisioned();
    s.toolchains = names(&[
        "stable-aarch64-apple-darwin",
        "nightly-aarch64-apple-darwin",
    ]);
    s.pin_targets = Vec::new();
    s.pin_components = Vec::new();
    let steps = plan(&s, Some(PIN), TARGETS, RELEASE);
    assert_eq!(
        steps,
        [
            Step::PinToolchain(PIN.into()),
            Step::PinTargets(PIN.into(), names(TARGETS)),
        ]
    );
    assert_eq!(
        argv(&steps[0]).join(" "),
        format!(
            "rustup toolchain install 1.97.1 --profile minimal {}--no-self-update",
            PIN_COMPONENTS
                .iter()
                .map(|c| format!("--component {c} "))
                .collect::<String>()
        )
    );
    assert!(
        steps
            .iter()
            .flat_map(argv)
            .all(|word| !word.starts_with("stable")),
        "{steps:?}"
    );
    let fresh = Seen {
        fresh_home: true,
        host: HOST.into(),
        ..Seen::default()
    };
    assert_eq!(
        plan(&fresh, Some(PIN), TARGETS, RELEASE),
        [
            Step::AutoInstallOff,
            Step::PinToolchain(PIN.into()),
            Step::PinTargets(PIN.into(), names(TARGETS)),
            Step::ReleaseToolchain(names(RELEASE)),
        ],
        "a home rustup never ran in turns auto-install off first"
    );
    assert_eq!(
        plan(&fresh, None, TARGETS, RELEASE),
        [Step::AutoInstallOff, Step::ReleaseToolchain(names(RELEASE))],
        "a pin that could not be read is asked for nothing, the release chain's stable still"
    );
}

/// THE RELEASE CHAIN'S `stable` IS IN PLACE WITH ITS SLICE'S STD, AND NOTHING MORE: absent, it
/// is installed with the x86_64 slice's std and no clippy; held without that std, the std is
/// added; held with it, nothing. NEGATIVE CONTROL: a plan without the release part leaves a
/// bare machine without `stable`, and `ship provision`'s `x86 slice` stays red.
#[test]
fn the_release_chains_stable_is_put_in_place_with_its_slice_std() {
    let mut s = provisioned();
    s.toolchains = names(&["1.97.1-aarch64-apple-darwin"]);
    s.release_targets = Vec::new();
    let steps = plan(&s, Some(PIN), TARGETS, RELEASE);
    assert_eq!(steps, [Step::ReleaseToolchain(names(RELEASE))]);
    assert_eq!(
        argv(&steps[0]).join(" "),
        "rustup toolchain install stable --profile minimal --target x86_64-apple-darwin \
         --no-self-update"
    );
    let mut s = provisioned();
    s.release_targets = names(&[HOST]);
    let steps = plan(&s, Some(PIN), TARGETS, RELEASE);
    assert_eq!(steps, [Step::ReleaseTargets(names(RELEASE))]);
    assert_eq!(
        argv(&steps[0]).join(" "),
        "rustup target add --toolchain stable x86_64-apple-darwin"
    );
    assert_eq!(plan(&provisioned(), Some(PIN), TARGETS, &[]), []);
    assert_eq!(
        argv(&Step::ReleaseToolchain(Vec::new())).join(" "),
        "rustup toolchain install stable --profile minimal --no-self-update",
        "a Linux worker's stable carries its host's std alone"
    );
}

/// ANOTHER HOST'S INSTALL IS NOT THE HOST'S: a machine whose rustup lists the pin and
/// `stable` only for x86_64 on an arm64 host gets both installed for its host, since rustup
/// resolves `1.97.1` and `stable` to `-aarch64-apple-darwin` and finds neither. NEGATIVE
/// CONTROL: a plan that reads `1.97.1-x86_64-apple-darwin` as the pin adds stds to a
/// toolchain rustup cannot resolve, and the run never converges.
#[test]
fn another_hosts_install_of_the_pin_or_stable_is_not_held() {
    let s = Seen {
        host: HOST.into(),
        toolchains: names(&[
            "1.97.1-x86_64-apple-darwin",
            "stable-x86_64-apple-darwin",
            "1.97.1-mine",
        ]),
        ..Seen::default()
    };
    assert_eq!(
        plan(&s, Some(PIN), TARGETS, RELEASE),
        [
            Step::PinToolchain(PIN.into()),
            Step::PinTargets(PIN.into(), names(TARGETS)),
            Step::ReleaseToolchain(names(RELEASE)),
        ]
    );
}

#[test]
fn only_linux_asks_the_pin_for_the_guest_agents_musl_std() {
    assert!(
        !pin_targets(false).contains(&"aarch64-unknown-linux-musl"),
        "{:?}",
        pin_targets(false)
    );
    let linux = pin_targets(true);
    assert_eq!(linux.last(), Some(&"aarch64-unknown-linux-musl"));
    let mut s = provisioned();
    s.pin_targets = pin_targets(false)
        .iter()
        .map(|t| (*t).to_string())
        .collect();
    assert_eq!(plan(&s, Some(PIN), &pin_targets(false), RELEASE), []);
    assert_eq!(
        plan(&s, Some(PIN), &linux, RELEASE),
        [Step::PinTargets(
            PIN.into(),
            names(&["aarch64-unknown-linux-musl"])
        )]
    );
}
