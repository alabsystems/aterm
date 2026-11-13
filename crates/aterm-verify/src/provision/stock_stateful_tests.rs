// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;

/// The host this rustup installs for: the one `observe` reads off a scratch home whose
/// settings name none.
const HOST: &str = aterm_toolchain::rustup::NATIVE_TRIPLE;

#[derive(Default)]
struct State {
    auto_install_off: bool,
    /// Installed toolchain (rustup's full name) -> its std targets.
    toolchains: BTreeMap<String, Vec<String>>,
    /// Installed toolchain -> its components past the minimal profile's.
    components: BTreeMap<String, Vec<String>>,
    calls: Vec<String>,
    /// The binary each call ran.
    ran: Vec<PathBuf>,
    auto_installed: Vec<String>,
}

#[derive(Default)]
pub(super) struct FakeRustup(RefCell<State>);

impl FakeRustup {
    fn calls(&self) -> Vec<String> {
        self.0.borrow().calls.clone()
    }

    fn find(s: &State, name: &str) -> Option<String> {
        s.toolchains
            .keys()
            .find(|t| *t == name || t.starts_with(&format!("{name}-")))
            .cloned()
    }

    /// `name`, installed, auto-installed, or refused.
    fn need(s: &mut State, name: &str) -> Result<String, String> {
        if let Some(t) = Self::find(s, name) {
            return Ok(t);
        }
        if s.auto_install_off {
            return Err(format!("error: toolchain '{name}-{HOST}' is not installed"));
        }
        let full = format!("{name}-{HOST}");
        s.toolchains.insert(full.clone(), vec![HOST.into()]);
        s.auto_installed.push(name.to_string());
        Ok(full)
    }
}

impl Rustup for FakeRustup {
    fn run(
        &self,
        homes: &Homes,
        tool: &Path,
        args: &[String],
        toolchain: Option<&str>,
    ) -> Result<String, String> {
        let mut s = self.0.borrow_mut();
        let name = tool.file_name().map(|n| n.to_string_lossy().into_owned());
        assert_eq!(
            name.as_deref(),
            Some("rustup"),
            "only rustup runs, never an installer: {}",
            tool.display()
        );
        s.calls.push(format!(
            "{}rustup {}",
            toolchain.map(|t| format!("+{t} ")).unwrap_or_default(),
            args.join(" ")
        ));
        s.ran.push(tool.to_path_buf());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["toolchain", "list"] => Ok(s
                .toolchains
                .keys()
                .map(|t| {
                    let d = if t.starts_with("stable") {
                        " (default)"
                    } else {
                        ""
                    };
                    format!("{t}{d}\n")
                })
                .collect()),
            ["target", "list", "--installed"] => {
                let t = Self::need(&mut s, toolchain.ok_or("no toolchain named")?)?;
                Ok(s.toolchains[&t].iter().map(|x| format!("{x}\n")).collect())
            }
            ["component", "list", "--installed"] => {
                let t = Self::need(&mut s, toolchain.ok_or("no toolchain named")?)?;
                let extra = s.components.get(&t).into_iter().flatten();
                let host = ["cargo", "rustc"]
                    .into_iter()
                    .chain(extra.map(String::as_str));
                Ok(host
                    .map(|c| format!("{c}-{HOST}\n"))
                    .chain(s.toolchains[&t].iter().map(|x| format!("rust-std-{x}\n")))
                    .collect())
            }
            ["set", "auto-install", "disable"] => {
                std::fs::create_dir_all(&homes.rustup_home).unwrap();
                std::fs::write(homes.rustup_home.join(SETTINGS), "auto_install = false\n").unwrap();
                s.auto_install_off = true;
                Ok(String::new())
            }
            ["toolchain", "install", name, rest @ ..] => {
                let (mut targets, mut components) = (vec![HOST.to_string()], Vec::new());
                let mut it = rest.iter();
                while let Some(x) = it.next() {
                    match *x {
                        "--target" => targets.extend(it.next().map(|t| (*t).to_string())),
                        "--component" => {
                            components.extend(it.next().map(|c| (*c).to_string()));
                        }
                        _ => {}
                    }
                }
                s.toolchains.insert(format!("{name}-{HOST}"), targets);
                s.components.insert(format!("{name}-{HOST}"), components);
                self_update(&homes.cargo_home, args)?;
                Ok(String::new())
            }
            ["component", "add", "--toolchain", name, components @ ..] => {
                let t = Self::need(&mut s, name)?;
                let have = s.components.entry(t).or_default();
                have.extend(components.iter().map(|x| (*x).to_string()));
                Ok(String::new())
            }
            ["target", "add", "--toolchain", name, targets @ ..] => {
                let t = Self::need(&mut s, name)?;
                let have = s.toolchains.get_mut(&t).unwrap();
                have.extend(targets.iter().map(|x| (*x).to_string()));
                Ok(String::new())
            }
            other => Err(format!("unexpected rustup call {other:?}")),
        }
    }
}

fn repo() -> PathBuf {
    aterm_tempfile::manifest_dir().join("../..")
}

fn pin() -> String {
    stock_pin(&repo()).unwrap()
}

/// A scratch account's homes: nothing on `$PATH`, no `$ATERM_TEST_RUSTUP`, and the row's
/// copy looked for in the scratch's own tooling cache.
pub(super) fn homes(dir: &Path) -> Homes {
    Homes {
        home: dir.to_path_buf(),
        rustup_home: dir.join(".rustup"),
        cargo_home: dir.join(".cargo"),
        lookup: Lookup {
            named: None,
            tooling: Some(dir.join("tooling.noindex")),
            path: None,
        },
    }
}

/// The row's copy, as `verify provision` places it: `bin/rustup` beside its stamp.
fn placed(homes: &Homes) -> PathBuf {
    let row = aterm_uds::host::tool("rustup");
    let tooling = homes.lookup.tooling.as_deref().unwrap();
    let exe = row.pinned_in(tooling).unwrap();
    stub(&exe);
    std::fs::write(
        row.pinned_root(tooling)
            .unwrap()
            .join(aterm_uds::host::PIN_STAMP),
        "{}",
    )
    .unwrap();
    exe
}

/// The release chain's install line on this host.
fn release_install() -> String {
    let targets = RELEASE_TARGETS.iter().map(|t| (*t).to_string()).collect();
    argv(&Step::ReleaseToolchain(targets)).join(" ")
}

fn position(calls: &[String], prefix: &str) -> usize {
    calls
        .iter()
        .position(|c| c.starts_with(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}` in {calls:#?}"))
}

#[test]
fn a_bare_home_gets_auto_install_off_then_the_pin_by_name() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let homes = homes(tmp.path());
    let rustup = FakeRustup::default();
    let refused = run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap_err();
    assert!(
        refused.contains("no rustup to drive") && refused.contains("verify provision"),
        "{refused}"
    );
    assert!(rustup.calls().is_empty(), "{:#?}", rustup.calls());
    let copy = placed(&homes);
    let acquired = run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap();
    assert!(!acquired.is_empty());
    let calls = rustup.calls();
    assert!(
        position(&calls, "rustup set auto-install disable")
            < position(&calls, "rustup toolchain install"),
        "auto-install is off before anything is installed: {calls:#?}"
    );
    let pin = pin();
    position(
        &calls,
        &format!("rustup toolchain install {pin} --profile minimal --component clippy"),
    );
    position(
        &calls,
        &format!(
            "rustup target add --toolchain {pin} {}",
            CELL_TARGETS.join(" ")
        ),
    );
    let release = release_install();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.contains("stable"))
            .collect::<Vec<_>>(),
        [&release],
        "rustup's floating stable gets the release chain's toolchain alone: {calls:#?}"
    );
    assert!(
        rustup.0.borrow().auto_installed.is_empty(),
        "every toolchain is installed by name: {:?}",
        rustup.0.borrow().auto_installed
    );
    assert!(
        rustup.0.borrow().ran.iter().all(|r| *r == copy),
        "every call ran the row's copy: {:?}",
        rustup.0.borrow().ran
    );
    let add = ["component", "add", "--toolchain", &pin, "clippy"].map(String::from);
    rustup
        .run(&homes, &copy, &add, None)
        .expect("the pin was installed by name");
    assert_eq!(
        missing(&homes, &rustup, Some(&pin), false),
        Ok(Vec::<String>::new())
    );
    assert_eq!(
        run(&repo(), &homes, &rustup, false, &mut |_| {}),
        Ok(Vec::new())
    );
}

/// The release chain builds its x86_64 compat slice on rustup's `stable`: a bare home gets
/// it by name, with that slice's std on a Mac, and a second run finds it in place.
#[test]
fn a_bare_home_gets_the_release_chains_stable_with_its_slice_std() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let homes = homes(tmp.path());
    let rustup = FakeRustup::default();
    placed(&homes);
    run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap();
    let calls = rustup.calls();
    let release = release_install();
    position(&calls, &release);
    if cfg!(target_os = "macos") {
        assert!(
            release.contains(" --target x86_64-apple-darwin "),
            "{release}"
        );
    }
    {
        let state = rustup.0.borrow();
        let stable = &state.toolchains[&format!("stable-{HOST}")];
        for t in RELEASE_TARGETS {
            assert!(stable.iter().any(|s| s == t), "{stable:?}");
        }
        assert!(state.auto_installed.is_empty());
    }
    assert_eq!(
        run(&repo(), &homes, &rustup, false, &mut |_| {}),
        Ok(Vec::new())
    );
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// A TOOLCHAIN INSTALL WRITES NOTHING TO `CARGO_HOME`: on a bare account (no `CARGO_HOME`
/// at all) every step succeeds and none is made, and where a person's own rustup sits in
/// `CARGO_HOME/bin` it stays alone there, because the install carries
/// `--no-self-update`. NEGATIVE CONTROL: the same install line without the flag, run
/// through the fake that models the pinned copy's self-update, fail the bare account's
/// step (`rustup is not installed at …`) and write 13 proxies beside the person's rustup.
#[test]
fn a_toolchain_install_never_self_updates_into_cargo_home() {
    let pin = pin();
    let bare = aterm_tempfile::tempdir().unwrap();
    let bare = homes(bare.path());
    placed(&bare);
    let rustup = FakeRustup::default();
    let acquired = run(&repo(), &bare, &rustup, false, &mut |_| {}).unwrap();
    assert!(
        acquired
            .iter()
            .any(|a| a.contains(&format!("toolchain install {pin}"))),
        "{acquired:#?}"
    );
    assert!(!bare.cargo_home.exists(), "no CARGO_HOME was made");

    let own = aterm_tempfile::tempdir().unwrap();
    let own = homes(own.path());
    let copy = placed(&own);
    stub(&own.cargo_home.join("bin/rustup"));
    run(&repo(), &own, &FakeRustup::default(), false, &mut |_| {}).unwrap();
    assert_eq!(entries(&own.cargo_home.join("bin")), ["rustup"]);

    {
        let with: Vec<String> = argv(&Step::PinToolchain(pin.clone()))[1..].to_vec();
        assert_eq!(with.last().map(String::as_str), Some(NO_SELF_UPDATE));
        let without: Vec<String> = with
            .iter()
            .filter(|a| *a != NO_SELF_UPDATE)
            .cloned()
            .collect();
        let refused = FakeRustup::default()
            .run(&bare, &copy, &without, None)
            .unwrap_err();
        assert!(refused.contains("rustup is not installed at"), "{refused}");
        FakeRustup::default()
            .run(&own, &copy, &without, None)
            .unwrap();
        assert_eq!(
            entries(&own.cargo_home.join("bin")).len(),
            1 + SELF_UPDATE_PROXIES.len(),
            "control: without the flag the install writes CARGO_HOME"
        );
        std::fs::remove_dir_all(own.cargo_home.join("bin")).unwrap();
        stub(&own.cargo_home.join("bin/rustup"));
    }
}

/// An executable that is not run: the fake answers for it.
fn stub(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "#!/bin/false\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A PIN MISSING A PIECE GETS IT ON THE PIN ALONE, WHATEVER STABLE HOLDS: where `stable`
/// carries every cell's std and clippy and the pin only its host's std, the probe and the
/// run name the pin's missing stds and components, and nothing of `stable`.
#[test]
fn a_pin_missing_a_piece_gets_it_on_the_pin_alone() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let homes = homes(tmp.path());
    let rustup = FakeRustup::default();
    let pin = pin();
    {
        let targets: Vec<String> = CELL_TARGETS.iter().map(|t| (*t).to_string()).collect();
        let mut s = rustup.0.borrow_mut();
        s.auto_install_off = true;
        s.toolchains.insert(format!("stable-{HOST}"), targets);
        s.components.insert(
            format!("stable-{HOST}"),
            PIN_COMPONENTS.iter().map(|c| (*c).to_string()).collect(),
        );
        s.toolchains
            .insert(format!("{pin}-{HOST}"), vec![HOST.into()]);
    }
    std::fs::create_dir_all(&homes.rustup_home).unwrap();
    std::fs::write(homes.rustup_home.join(SETTINGS), "").unwrap();
    placed(&homes);
    let others: Vec<&str> = CELL_TARGETS
        .iter()
        .copied()
        .filter(|t| *t != HOST)
        .collect();
    let add = format!("rustup target add --toolchain {pin} {}", others.join(" "));
    let clippy = format!(
        "rustup component add --toolchain {pin} {}",
        PIN_COMPONENTS.join(" ")
    );
    assert_eq!(
        missing(&homes, &rustup, Some(&pin), false),
        Ok(vec![add.clone(), clippy.clone()]),
        "the read-only probe names what a run adds"
    );
    run(&repo(), &homes, &rustup, false, &mut |_| {}).unwrap();
    let calls = rustup.calls();
    position(&calls, &add);
    position(&calls, &clippy);
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("stable") && !c.contains(" list")),
        "nothing of the pin is added to stable: {calls:#?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("toolchain install") || c.contains(" set ")),
        "{calls:#?}"
    );
    assert!(rustup.0.borrow().auto_installed.is_empty());
    assert_eq!(
        missing(&homes, &rustup, Some(&pin), false),
        Ok(Vec::<String>::new())
    );

    // `--linux` holds the same machine to the guest agent's musl std as well, and
    // adds it to the pin alone.
    let musl = format!("rustup target add --toolchain {pin} aarch64-unknown-linux-musl");
    assert_eq!(
        missing(&homes, &rustup, Some(&pin), true),
        Ok(vec![musl.clone()])
    );
    let acquired = run(&repo(), &homes, &rustup, true, &mut |_| {}).unwrap();
    assert_eq!(acquired, [musl]);
    assert_eq!(
        missing(&homes, &rustup, Some(&pin), true),
        Ok(Vec::<String>::new())
    );
}

/// THE ROW DRIVES ITS STAMPED COPY, BEFORE ANY RUSTUP ON `$PATH`, AND NEVER HOMEBREW'S
/// KEG: the homes' rustup is the `rustup` row's answer over their lookup. NEGATIVE
/// CONTROLS: with no copy, a rustup on `$PATH` (a person's own) answers, and the keg
/// planted beside it answers only when its own directory is put on `$PATH`.
#[test]
fn the_row_drives_its_stamped_copy_before_path_and_never_the_keg() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let keg = tmp.path().join("opt/homebrew/opt/rustup/bin/rustup");
    stub(&keg);
    let on_path = tmp.path().join("bin");
    stub(&on_path.join("rustup"));
    let mut h = homes(tmp.path());
    assert!(h.rustup().is_err(), "the keg never answers");
    h.lookup.path = Some(on_path.clone().into_os_string());
    assert_eq!(h.rustup(), Ok(on_path.join("rustup")));
    let copy = placed(&h);
    assert_eq!(h.rustup(), Ok(copy), "the stamped copy before $PATH");
    let kegged = Homes {
        lookup: Lookup {
            tooling: None,
            path: Some(keg.parent().unwrap().as_os_str().to_owned()),
            named: None,
        },
        ..h
    };
    assert_eq!(kegged.rustup(), Ok(keg));
}
