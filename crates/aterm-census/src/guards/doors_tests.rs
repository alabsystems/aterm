// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests::{run, spots};
use super::*;

#[test]
fn only_the_gesture_worker_runs_a_devtools_change() {
    let flow = "fn spawn_devtools_worker(job: Job) {\n    \
                consent::run_devtools_change(job.change, gate, f);\n}\n\
                fn elsewhere() {\n    consent::run_devtools_change(c, g, f);\n}\n\
                #[cfg(test)]\nmod tests {\n    fn t() { run_devtools_change(c, g, f); }\n}\n";
    let home = "pub fn run_devtools_change<F>(c: C) {}\n\
                const DEVTOOLSSECURITY_PATH: &str = \"/usr/sbin/DevToolsSecurity\";\n\
                pub const REVERT: &str = \"/usr/sbin/DevToolsSecurity -disable\";\n\
                const fn devtools_status_command() -> [&'static str; 2] {\n    \
                [DEVTOOLSSECURITY_PATH, \"-status\"]\n}\n\
                const fn devtools_enable_command() -> [&'static str; 2] {\n    \
                [DEVTOOLSSECURITY_PATH, \"-enable\"]\n}\n\
                impl DebuggerChange {\n    fn command(self) -> [&'static str; 2] {\n        \
                match self {\n            Self::On => devtools_enable_command(),\n        \
                }\n    }\n}\n\
                fn spawn_it(change: DebuggerChange) { run_to_exit_keeping_heads(&change.command(), |_| {}) }\n\
                fn sneaky() {\n    let argv = [DEVTOOLSSECURITY_PATH, \"-enable\"];\n}\n\
                fn probe() { let out = run_bounded_exit(program, &args, ceiling); }\n";
    let found = run(
        &DEVTOOLS_DOOR,
        &[
            (DEVTOOLS_FLOW, flow),
            (DEVTOOLS_HOME, home),
            (
                "crates/aterm-cli/src/lib.rs",
                "fn a() { run_devtools_change(c, g, f); }\n\
                 fn b() { let p = \"/usr/sbin/DevToolsSecurity\"; }\n\
                 fn c() { let s = \"/usr/sbin/DevToolsSecurity -status\"; }\n\
                 fn d() { AuthorizationCopyRights(a, r, e, f, o); }\n\
                 fn e() { let s = \"do shell script x with administrator privileges\"; }\n\
                 fn f() { aterm_uds::spawnfd::command(\"DevToolsSecurity\").arg(\"-enable\"); }\n\
                 fn g() { spawnfd::command(\"sh\").args([\"-c\", \"DevToolsSecurity -enable\"]); }\n\
                 fn h() { let argv = [DEVTOOLSSECURITY_PATH, \"-enable\"]; }\n\
                 fn i() { println!(\"run DevToolsSecurity -status to see\"); }\n",
            ),
            (
                "crates/aterm-gui/tests/t.rs",
                "fn t() { run_devtools_change(c, g, f); }\n",
            ),
            // The elevation's one definition is admitted; a second spelling there, or the
            // same definition in another file, is not.
            (
                ELEVATION,
                "pub const DEVTOOLS_SECURITY: &str = \"/usr/sbin/DevToolsSecurity\";\n\
                 fn on() { run(&Cmd::new(DEVTOOLS_SECURITY).arg(\"-enable\")); }\n\
                 const AGAIN: &str = \"/usr/sbin/DevToolsSecurity\";\n",
            ),
            (
                "crates/aterm-machines/src/items/developer.rs",
                "pub const DEVTOOLS_SECURITY: &str = \"/usr/sbin/DevToolsSecurity\";\n",
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-cli/src/lib.rs:1",
            "crates/aterm-cli/src/lib.rs:2",
            "crates/aterm-cli/src/lib.rs:4",
            "crates/aterm-cli/src/lib.rs:5",
            "crates/aterm-cli/src/lib.rs:6",
            "crates/aterm-cli/src/lib.rs:7",
            "crates/aterm-cli/src/lib.rs:8",
            "crates/aterm-containment/src/consent.rs:17",
            "crates/aterm-containment/src/consent.rs:19",
            "crates/aterm-gui/src/debugger_access.rs:5",
            "crates/aterm-machines/src/items/developer.rs:1",
            "crates/aterm-machines/src/root_steps.rs:3",
        ]
    );
}

// Planted twins of the yes tokens' leaves (the census reads the real files on every
// `verify census`). Each test plants into them, their mounts or a caller, and names the
// lines it expects.

const YES_LEAF: &str = "crates/aterm-gui/src/native_settings_owner_yes.rs";
const YES_PARENT: &str = "crates/aterm-gui/src/native_settings.rs";
const GATE_LEAF: &str = "crates/aterm-gui/src/workstation_setup_gate.rs";
const GATE_PARENT: &str = "crates/aterm-gui/src/workstation_setup.rs";
/// A caller of the tokens, and a `#[path]` child of the yes's parent: the
/// leaf's sibling, which the yes's private field never reaches.
const CALLER_FILE: &str = "crates/aterm-gui/src/debugger_access.rs";
const SIBLING: &str = "crates/aterm-gui/src/native_settings_tabs.rs";

const YES: &str = r#"use super::{ConsentGestures, OwnerAnswer, SharedOwnerQuiet};
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OwnerYes {
    _seal: YesSeal,
}
#[derive(Debug, PartialEq, Eq)]
struct YesSeal;
impl OwnerYes {
    #[cfg(test)]
    pub(crate) const fn for_test() -> Self {
        Self { _seal: YesSeal }
    }
}
pub(crate) fn owner_confirms_with(
    quiet: &SharedOwnerQuiet,
    gestures: &ConsentGestures,
) -> OwnerAnswer {
    match ConfirmArm::ask(gestures.confirm, "Title?", "Body.", "Go") {
        Some(true) => {
            ::aterm_log::info!("the owner said yes");
            OwnerAnswer::Yes(OwnerYes { _seal: YesSeal })
        }
        Some(false) => OwnerAnswer::No,
        None => OwnerAnswer::NotAsked,
    }
}
#[derive(Clone, Copy)]
pub(crate) enum ConfirmArm {
    Alert,
    Nobody,
    #[cfg(test)]
    StandIn(fn(&str, &str, &str) -> Option<bool>),
}
impl ConfirmArm {
    pub(super) fn ask(self, title: &str, body: &str, proceed: &str) -> Option<bool> {
        match self {
            Self::Alert => None,
            Self::Nobody => None,
            #[cfg(test)]
            Self::StandIn(answer) => answer(title, body, proceed),
        }
    }
}
"#;

const GATE: &str = r#"use super::{AdmittedBy, PagePress};
#[derive(Default)]
enum PermissionHold {
    #[default]
    None,
    FreshStatus,
}
#[derive(Default)]
pub(super) struct PermissionGate {
    hold: PermissionHold,
    own: Option<(AdmittedBy, u64)>,
}
impl PermissionGate {
    pub(super) fn allowed(&self) -> bool {
        ::std::matches!(self.hold, PermissionHold::None)
    }
}
"#;

const YES_MOUNT: &str = "#[path = \"native_settings_owner_yes.rs\"]\nmod owner_yes;\n\
                         pub(crate) use owner_yes::{OwnerYes, owner_confirms_with};\n";
const GATE_MOUNT: &str = "#[path = \"workstation_setup_gate.rs\"]\nmod permission_gate;\n\
                          use permission_gate::PermissionGate;\n";

/// A caller that names, holds, aliases inside another type and spends the token, implements
/// traits for other types, and reads a raw pointer where it names no token: none of it is a
/// finding.
const CALLER: &str = r#"use crate::native_settings::{OwnerAnswer, OwnerYes};
type Answer = Option<OwnerYes>;
pub(crate) struct ChangeTicket {
    _yes: crate::native_settings::OwnerYes,
}
fn spend(answer: OwnerAnswer) -> Option<OwnerYes> {
    match answer {
        OwnerAnswer::Yes(yes) => Some(yes),
        _ => None,
    }
}
fn elsewhere(p: *const u8) -> u8 {
    unsafe { std::ptr::read(p) }
}
impl std::fmt::Debug for ChangeTicket {}
#[derive(Clone, Copy)]
pub(crate) enum PagePress { Person }
impl Default for PagePress {}
"#;

/// The file `rel`'s twin before any plant.
fn base_of(rel: &str) -> &'static str {
    match rel {
        YES_LEAF => YES,
        GATE_LEAF => GATE,
        YES_PARENT => YES_MOUNT,
        GATE_PARENT => GATE_MOUNT,
        CALLER_FILE => CALLER,
        _ => "",
    }
}

/// The rule's spots over the leaves `yes` and `gate`, their mounts and the
/// caller, with `plant` appended to `rel` (or as `rel` whole).
fn with_plant(yes: &str, gate: &str, rel: &str, plant: &str) -> Vec<String> {
    let mut files: Vec<(&str, String)> = vec![
        (YES_LEAF, yes.to_string()),
        (GATE_LEAF, gate.to_string()),
        (YES_PARENT, YES_MOUNT.to_string()),
        (GATE_PARENT, GATE_MOUNT.to_string()),
        (CALLER_FILE, CALLER.to_string()),
    ];
    match files.iter_mut().find(|(file, _)| *file == rel) {
        Some((_, src)) => src.push_str(plant),
        None => files.push((rel, plant.to_string())),
    }
    let files: Vec<(&str, &str)> = files.iter().map(|(f, s)| (*f, s.as_str())).collect();
    spots(&run(&DEVTOOLS_DOOR, &files))
}

/// The spots of lines `lines` (1-based) of a plant appended to `rel`.
fn planted(rel: &str, lines: &[usize]) -> Vec<String> {
    let base = base_of(rel).lines().count();
    lines
        .iter()
        .map(|n| format!("{rel}:{}", base + n))
        .collect()
}

/// `rel`'s spots at lines `lines` of its twin as it stands.
fn at(rel: &str, lines: &[usize]) -> Vec<String> {
    lines.iter().map(|n| format!("{rel}:{n}")).collect()
}

/// The real leaves, their parents' one mount each, and a caller that names, holds, spends and
/// aliases the tokens are silent.
#[test]
fn the_real_leaves_and_their_callers_are_silent() {
    assert_eq!(with_plant(YES, GATE, CALLER_FILE, ""), Vec::<String>::new());
}

/// In a leaf, a value of the mark, a `Ty { .. }` literal or a `Self { .. }` outside a door's
/// body is a finding, including one in a fn of a door's name that is no door.
#[test]
fn a_leaf_builds_its_token_only_in_its_doors() {
    let cases: &[(&str, &str, &[usize])] = &[
        (
            YES_LEAF,
            "fn press() -> OwnerAnswer {\n    OwnerAnswer::Yes(OwnerYes { _seal: YesSeal })\n}\n",
            &[2],
        ),
        (
            YES_LEAF,
            "impl OwnerYes {\n    fn sneaky() -> Self {\n        Self { _seal: YesSeal }\n    }\n}\n",
            &[3],
        ),
        (
            YES_LEAF,
            "impl Other {\n    fn for_test() -> OwnerYes {\n        \
             OwnerYes { _seal: YesSeal }\n    }\n}\n",
            &[3],
        ),
        (
            YES_LEAF,
            "impl Marker for OwnerYes {\n    fn for_test() -> Self {\n        \
             Self { _seal: YesSeal }\n    }\n}\n",
            &[1, 3],
        ),
        (
            YES_LEAF,
            "pub(crate) trait Sneak {\n    fn owner_confirms_with(&self, gestures: &ConsentGestures) \
             -> OwnerYes {\n        ConfirmArm::ask(gestures.confirm, \"\", \"\", \"\");\n        \
             OwnerYes { _seal: YesSeal }\n    }\n}\n",
            &[4],
        ),
        (
            YES_LEAF,
            "mod inner {\n    pub(crate) fn owner_confirms_with() -> super::OwnerYes {\n        \
             super::OwnerYes { _seal: super::YesSeal }\n    }\n}\n",
            &[1, 3],
        ),
        (
            YES_LEAF,
            "#[cfg(not(test))]\npub(crate) fn owner_confirms_with(gestures: &ConsentGestures) \
             -> OwnerAnswer {\n    ConfirmArm::ask(gestures.confirm, \"\", \"\", \"\");\n    \
             OwnerAnswer::Yes(OwnerYes { _seal: YesSeal })\n}\n",
            &[4],
        ),
    ];
    for (rel, plant, lines) in cases {
        assert_eq!(
            with_plant(YES, GATE, rel, plant),
            planted(rel, lines),
            "{plant}"
        );
    }
}

/// A door out of its `DoorShape` is no door, so its mints are findings; a fn nested in a
/// door's body is a finding of its own.
#[test]
fn a_door_holds_its_shape() {
    for (old, new) in [
        (
            "match ConfirmArm::ask(gestures.confirm, \"Title?\", \"Body.\", \"Go\") {",
            "match Some(true) {",
        ),
        (
            "match ConfirmArm::ask(gestures.confirm, \"Title?\", \"Body.\", \"Go\") {",
            "match gestures.confirm.ask(\"Title?\", \"Body.\", \"Go\") {",
        ),
        ("gestures: &ConsentGestures,", "gestures: &OtherGestures,"),
    ] {
        let yes = YES.replace(old, new);
        assert_eq!(
            with_plant(&yes, GATE, CALLER_FILE, ""),
            at(YES_LEAF, &[14, 21]),
            "{new}"
        );
    }
    let yes = YES.replace(
        "#[cfg(test)]\n    pub(crate) const fn for_test",
        "#[cfg(debug_assertions)]\n    pub(crate) const fn for_test",
    );
    assert_eq!(
        with_plant(&yes, GATE, CALLER_FILE, ""),
        at(YES_LEAF, &[10, 11])
    );
    let yes = YES.replace(
        "::aterm_log::info!(\"the owner said yes\");",
        "fn leak() -> OwnerYes { OwnerYes { _seal: YesSeal } }",
    );
    assert_eq!(with_plant(&yes, GATE, CALLER_FILE, ""), at(YES_LEAF, &[20]));
}

/// In either leaf, a plant that could build beside the doors, expand into code no line spells
/// or take an arm a caller supplies (a function type on a shipped line) is a finding, and so
/// is a token or mark out of shape (not braced, a public field, no mark field, a `Clone`,
/// `Copy` or `Default` derive, the mark public or gone). The stand-in arm under
/// `#[cfg(test)]` is silent.
#[test]
fn a_leaf_holds_nothing_that_builds_beside_its_doors() {
    let plants: &[(&str, &[usize])] = &[
        ("mod child;\n", &[1]),
        ("mod inner {\n    fn t() {}\n}\n", &[1]),
        (
            "macro mint() {\n    OwnerYes { _seal: {mark} }\n}\n",
            &[1, 2],
        ),
        ("fn f() -> bool {\n    matches!(1, 1)\n}\n", &[2]),
        ("fn f() -> OwnerYes {\n    crate::mint!()\n}\n", &[2]),
        ("::std::include!(\"sneak.rs\");\n", &[1]),
        ("#[cfg_attr(all(), derive(Clone))]\nstruct Other;\n", &[1]),
        ("#[crate::sneak]\nfn f() {}\n", &[1]),
        ("#[derive(serde::Deserialize)]\nstruct Other;\n", &[1]),
        ("use super::*;\n", &[1]),
        ("use crate::m::Debug;\n", &[1]),
        ("use crate::m::thing as other;\n", &[1]),
        ("extern crate alloc as std;\n", &[1]),
        ("fn f() {\n    unsafe { g() }\n}\n", &[2]),
        (
            "impl {mark} {\n    const fn new() -> Self {\n        Self\n    }\n}\n",
            &[1],
        ),
        (
            "impl Default for {mark} {\n    fn default() -> Self {\n        Self\n    }\n}\n",
            &[1],
        ),
        ("type Other = u8;\n", &[1]),
        (
            "pub(crate) enum Arm {\n    Alert,\n    StandIn(fn(&str) -> Option<bool>),\n}\n",
            &[3],
        ),
        (
            "fn ask(answer: impl Fn(u8) -> bool) -> bool {\n    answer(1)\n}\n",
            &[1],
        ),
        ("fn ask(answer: &dyn FnMut(u8)) {}\n", &[1]),
        (
            "pub(crate) enum Arm {\n    Alert,\n    #[cfg(test)]\n    \
             StandIn(fn(&str) -> Option<bool>),\n}\n",
            &[],
        ),
    ];
    for (plant, lines) in plants {
        let plant = plant.replace("{mark}", "YesSeal");
        assert_eq!(
            with_plant(YES, GATE, YES_LEAF, &plant),
            planted(YES_LEAF, lines),
            "{plant}"
        );
    }
    for (old, new, line) in [
        (
            "    _seal: YesSeal,\n}",
            "    pub(crate) _seal: YesSeal,\n}",
            3,
        ),
        ("    _seal: YesSeal,\n}", "    _seal: (),\n}", 3),
        (
            "pub(crate) struct OwnerYes {\n    _seal: YesSeal,\n}",
            "pub(crate) struct OwnerYes(YesSeal);\n// a tuple\n//",
            3,
        ),
        (
            "#[derive(Debug, PartialEq, Eq)]\npub(crate) struct OwnerYes",
            "#[derive(Debug, Clone)]\npub(crate) struct OwnerYes",
            3,
        ),
        (
            "#[derive(Debug, PartialEq, Eq)]\npub(crate) struct OwnerYes",
            "#[derive(Default)]\npub(crate) struct OwnerYes",
            3,
        ),
        ("\nstruct YesSeal;", "\npub(crate) struct YesSeal;", 7),
        (
            "#[derive(Debug, PartialEq, Eq)]\nstruct YesSeal;",
            "#[derive(Clone, Copy)]\nstruct YesSeal;",
            7,
        ),
        ("\nstruct YesSeal;", "\nstruct Other;", 1),
    ] {
        assert!(YES.contains(old), "{old}");
        let yes = YES.replacen(old, new, 1);
        assert_eq!(
            with_plant(&yes, GATE, CALLER_FILE, ""),
            at(YES_LEAF, &[line]),
            "{new}"
        );
    }
}

/// The arm holds aterm's alert and nobody, each a unit, and a test's stand-in under a test
/// `cfg`: a variant carrying a value, the stand-in on a shipped line, another variant even
/// under a test `cfg`, a generic arm and a struct arm are findings.
#[test]
fn the_alerts_arm_holds_its_variants() {
    let fixed = "pub(crate) enum ConfirmArm {\n    Alert,\n    Nobody,\n    \
                 Fixed(Option<bool>),\n}\nimpl ConfirmArm {\n    \
                 pub(super) fn ask(self, t: &str, b: &str, p: &str) -> Option<bool> {\n        \
                 match self {\n            Self::Alert => None,\n            \
                 Self::Nobody => None,\n            Self::Fixed(answer) => answer,\n        \
                 }\n    }\n}\n";
    assert_eq!(
        with_plant(YES, GATE, YES_LEAF, fixed),
        planted(YES_LEAF, &[4])
    );
    let line = |src: &str, needle: &str| src.lines().position(|l| l.contains(needle));
    let arm = "    Nobody,\n    #[cfg(test)]\n    StandIn(";
    for (new, flagged) in [
        (
            "    Nobody,\n    Fixed(Option<bool>),\n    #[cfg(test)]\n    StandIn(",
            vec!["    Fixed("],
        ),
        (
            "    Nobody(Option<bool>),\n    #[cfg(test)]\n    StandIn(",
            vec!["    Nobody("],
        ),
        ("    Nobody,\n    StandIn(", vec!["    StandIn("]),
        (
            "    Nobody,\n    #[cfg(test)]\n    Fixed(Option<bool>),\n    #[cfg(test)]\n    StandIn(",
            vec!["    Fixed("],
        ),
        (
            "    Nobody,\n    #[allow(a > b > c, d)]\n    Fixed(Option<bool>),\n    #[cfg(test)]\n    StandIn(",
            vec!["    Fixed("],
        ),
        (
            "    Nobody,\n    #[allow\n    Alert,\n    #[cfg(test)]\n    StandIn(",
            // An incomplete outer attribute cannot prove that the following
            // cfg excludes the variant.
            vec!["    #[allow", "    StandIn("],
        ),
    ] {
        let yes = YES.replace(arm, new);
        let want: Vec<usize> = flagged
            .iter()
            .filter_map(|needle| line(&yes, needle).map(|i| i + 1))
            .collect();
        assert_eq!(
            with_plant(&yes, GATE, CALLER_FILE, ""),
            at(YES_LEAF, &want),
            "{new}"
        );
    }
    let generic = YES.replace("enum ConfirmArm {", "enum ConfirmArm<T> {");
    let want = line(&generic, "enum ConfirmArm").map_or(0, |i| i + 1);
    assert_eq!(
        with_plant(&generic, GATE, CALLER_FILE, ""),
        at(YES_LEAF, &[want])
    );
    let start = YES.find("#[derive(Clone, Copy)]\npub(crate) enum ConfirmArm");
    let end = YES.find("impl ConfirmArm {");
    if let (Some(start), Some(end)) = (start, end) {
        let yes = format!(
            "{}pub(crate) struct ConfirmArm(Option<bool>);\n{}",
            &YES[..start],
            &YES[end..]
        );
        let want = line(&yes, "struct ConfirmArm").map_or(0, |i| i + 1);
        assert_eq!(
            with_plant(&yes, GATE, CALLER_FILE, ""),
            at(YES_LEAF, &[1, want])
        );
    } else {
        panic!("the yes twin defines its arm");
    }
}

/// Outside its leaf, in a test-named file too: a type or import of a token's name, a copy
/// impl or `type` alias of a token, an unsafe read in an item naming one, a second mount, the
/// mark's name, and an attribute on the mount that could swap its file are findings. Silent:
/// an alias of a type holding a token, another type's copy impl, a fn naming a token with no
/// read, a raw read naming none, a mark in a string or comment, and an `allow` on the mount.
#[test]
fn outside_its_leaf_nothing_renames_copies_forges_or_remounts_a_token() {
    let caller = "pub(crate) struct OwnerYes;\n\
                  pub(crate) use crate::other::Yes as OwnerYes;\n\
                  type Granted = crate::native_settings::OwnerYes;\n\
                  impl std::clone::Clone for crate::native_settings::OwnerYes {}\n\
                  unsafe impl Copy for OwnerYes {}\n\
                  impl Default for OwnerYes {\n    fn default() -> Self { todo!() }\n}\n\
                  const FORGED: OwnerYes = unsafe {\n    std::mem::zeroed()\n};\n\
                  fn pointer(a: &OwnerYes) -> OwnerYes {\n    \
                  unsafe { (a as *const OwnerYes).read() }\n}\n\
                  #[path = \"native_settings_owner_yes.rs\"]\nmod again;\n\
                  include!(\"native_settings_owner_yes.rs\");\n\
                  type Answer = Option<OwnerYes>;\n\
                  impl Clone for Other {}\n\
                  fn count(y: &OwnerYes) -> usize {\n    1\n}\n\
                  fn raw(p: *const u8) -> u8 {\n    unsafe { std::ptr::read(p) }\n}\n";
    assert_eq!(
        with_plant(YES, GATE, CALLER_FILE, caller),
        planted(CALLER_FILE, &[1, 2, 3, 4, 5, 6, 10, 13, 15, 17])
    );
    let twice = "#[path = \"native_settings_owner_yes.rs\"]\nmod twice;\n";
    assert_eq!(
        with_plant(YES, GATE, YES_PARENT, twice),
        planted(YES_PARENT, &[1])
    );
    let marks = "macro_rules! mint {\n    () => {\n        OwnerYes { _seal: YesSeal }\n    };\n}\n\
                 use super::owner_yes::YesSeal;\n\
                 const NOTE: &str = \"YesSeal\";\n\
                 // YesSeal\n";
    assert_eq!(
        with_plant(YES, GATE, CALLER_FILE, marks),
        planted(CALLER_FILE, &[3, 6])
    );
    assert_eq!(
        with_plant(YES, GATE, SIBLING, "use super::owner_yes::YesSeal;\n"),
        at(SIBLING, &[1])
    );
    let testbed = "crates/aterm-gui/src/native_settings_testbed.rs";
    assert_eq!(
        with_plant(YES, GATE, testbed, "pub(crate) struct OwnerYes;\n"),
        at(testbed, &[1])
    );
    for (mount, lines) in [
        ("#[cfg_attr(all(), path = \"testbed.rs\")]\n", &[1][..]),
        ("#[some::macro_attr]\n", &[1]),
        ("#[allow(dead_code)]\n", &[]),
    ] {
        let mount = format!("{mount}{YES_MOUNT}");
        let files = [
            (YES_LEAF, YES),
            (GATE_LEAF, GATE),
            (YES_PARENT, mount.as_str()),
            (GATE_PARENT, GATE_MOUNT),
            (CALLER_FILE, CALLER),
        ];
        assert_eq!(
            spots(&run(&DEVTOOLS_DOOR, &files)),
            at(YES_PARENT, lines),
            "{mount}"
        );
    }
}

// Twins of the gate's inputs: the host, the Turn On… chain, the App's way into the gate and
// the gate itself, each fenced function named only in its callers.

const HOST_TWIN: &str = "impl App {
    pub(crate) fn receive_system_state_status(&mut self, s: SetupStatus) {
        self.resolve_system_state_apply();
        self.observe_initial_setup_permissions(s.permissions);
    }
    pub(crate) fn interact_system_state(&mut self) {
        self.interact_system_state_view();
    }
    pub(crate) fn interact_system_state_view(&mut self) {
        self.note_system_state_touch();
    }
    pub(crate) fn tick_system_state(&mut self) {
        self.resolve_system_state_apply();
        self.drain_system_state_actions();
    }
    pub(crate) fn drain_system_state_actions(&mut self) {
        self.run_system_state_action(action);
    }
    fn run_system_state_action(&mut self, action: SystemStateAction) {
        self.run_system_state_action_with_handlers(action);
    }
    fn run_system_state_action_with_handlers(&mut self, action: SystemStateAction) {
        let press = self.take_system_state_press();
        self.note_system_state_apply(admission);
        self.open_access_from_system_state(access, by_person);
    }
    pub(crate) fn resolve_system_state_apply(&mut self) {
        self.accepted_initial_setup_apply();
    }
}
";
const ACCESS_TWIN: &str = "impl App {
    pub(crate) fn open_access_from_system_state(&mut self, access: Access, by_person: bool) -> bool {
        by_person
    }
}
";
const LIB_TWIN: &str = "impl App {
    fn on_user_event(&mut self, ev: Wake) {
        match ev {
            Wake::Status(s) => self.receive_system_state_status(s),
            Wake::Resolved => self.resolve_system_state_apply(),
        }
    }
}
";
const SETUP_TWIN: &str = "impl App {
    pub(crate) fn note_system_state_touch(&mut self) {
        self.workstation_setup.permissions.note_touch(false);
    }
    pub(crate) fn take_system_state_press(&mut self) -> PagePress {
        self.workstation_setup
            .permissions
            .take_press(0)
    }
    pub(crate) fn note_system_state_apply(&mut self, admission: Admission) {
        let press = self.take_system_state_press();
        self.workstation_setup.permissions.apply_started(true, press);
    }
    pub(crate) fn accepted_initial_setup_apply(&mut self) {
        self.workstation_setup.permissions.accepted();
    }
    pub(crate) fn observe_initial_setup_permissions(&mut self, policy: Option<Policy>) {
        self.workstation_setup.permissions.observe(policy);
    }
}
";
const NATIVE_TWIN: &str = "impl App {
    pub(crate) fn dispatch_native_event(&mut self) {
        self.dispatch_native_target_event();
    }
    fn dispatch_native_target_event(&mut self) {
        self.interact_system_state_view();
        self.drain_system_state_actions();
    }
}
";
/// A file no fence names: it takes no input of the gate's.
const STRANGER: &str = "crates/aterm-gui/src/control.rs";

/// The rule's spots over the inputs' twins, with `setup` as the gate's
/// parent and each plant appended to its file (or as the file whole).
fn fenced(setup: &str, plants: &[(&str, &str)]) -> Vec<String> {
    let gate = GATE.replacen(
        "impl PermissionGate {\n",
        "impl PermissionGate {\n    \
         pub(super) fn note_touch(&mut self, controller: bool) {}\n    \
         pub(super) fn take_press(&mut self, gestures: u64) -> PagePress {\n        \
         PagePress::Nobody\n    }\n    \
         pub(super) fn apply_started(&mut self, offer: bool, press: PagePress) {}\n    \
         pub(super) fn accepted(&mut self) {}\n    \
         pub(super) fn observe(&mut self, policy: Option<Policy>) -> bool {\n        \
         false\n    }\n",
        1,
    );
    let mut files = vec![
        (HOST, HOST_TWIN.to_string()),
        (ACCESS, ACCESS_TWIN.to_string()),
        (GUI_LIB, LIB_TWIN.to_string()),
        (SETUP, setup.to_string()),
        (NATIVE_EVENTS, NATIVE_TWIN.to_string()),
        (ADMISSION_LEAF, gate),
    ];
    for &(rel, plant) in plants {
        match files.iter_mut().find(|(file, _)| *file == rel) {
            Some((_, src)) => src.push_str(plant),
            None => files.push((rel, plant.to_string())),
        }
    }
    let files: Vec<(&str, &str)> = files.iter().map(|(f, s)| (*f, s.as_str())).collect();
    spots(&run(&DEVTOOLS_DOOR, &files))
}

/// The lines of `rel`'s input twin before a plant.
fn fence_base(rel: &str) -> usize {
    match rel {
        HOST => HOST_TWIN,
        SETUP => SETUP_TWIN,
        GUI_LIB => LIB_TWIN,
        NATIVE_EVENTS => NATIVE_TWIN,
        _ => "",
    }
    .lines()
    .count()
}

/// The inputs' twins as they stand are silent. A fenced function named outside its callers (a
/// call or a value), a transition outside its App function or a module in the gate's parent,
/// a test-named file no parent declares test-only, and a fence whose home lost its function
/// are findings; a `#[cfg(test)]` item or a module its parent declares test-only is silent.
#[test]
fn the_yes_inputs_are_taken_by_their_callers_alone() {
    assert_eq!(fenced(SETUP_TWIN, &[]), Vec::<String>::new());
    let cases: &[(&str, &str, &[usize])] = &[
        (
            KEY_EVENTS,
            "impl App {\n    fn fence_menu_action(&mut self) {\n        \
             self.interact_system_state(wid);\n    }\n}\n",
            &[],
        ),
        (
            KEY_EVENTS,
            "impl App {\n    fn dispatch_menu_action(&mut self) {\n        \
             self.interact_system_state(wid);\n    }\n}\n",
            &[3],
        ),
        (
            MOUSE_EVENTS,
            "impl App {\n    fn mouse_input_then_fold(&mut self) {\n        \
             self.interact_system_state_view(wid, view);\n    }\n}\n",
            &[],
        ),
        (
            MOUSE_EVENTS,
            "impl App {\n    fn mouse_input_then_fold(&mut self) {\n        \
             self.interact_system_state(wid);\n    }\n}\n",
            &[3],
        ),
        (
            MOUSE_EVENTS,
            "impl App {\n    fn route_mouse_input(&mut self) {\n        \
             self.interact_system_state_view(wid, view);\n    }\n}\n",
            &[3],
        ),
        (
            STRANGER,
            "fn sneaky(app: &mut App) -> bool {\n    app.open_access_from_system_state(access, true)\n}\n",
            &[2],
        ),
        (
            STRANGER,
            "fn forge(app: &mut App) {\n    \
             app.note_system_state_apply(Admission::Automatic);\n    \
             app.accepted_initial_setup_apply();\n    \
             app.observe_initial_setup_permissions(Some(policy));\n}\n",
            &[2, 3, 4],
        ),
        (
            STRANGER,
            "fn forge(app: &mut App) {\n    app.receive_system_state_status(made_up);\n}\n",
            &[2],
        ),
        (
            STRANGER,
            "fn later(app: &mut App) {\n    app.with_gesture_source(false, App::note_system_state_touch);\n}\n",
            &[2],
        ),
        (
            HOST,
            "impl App {\n    pub(crate) fn act_on_page(&mut self, action: SystemStateAction) {\n        \
             self.run_system_state_action(action);\n    }\n}\n",
            &[3],
        ),
        (
            STRANGER,
            "fn drain(app: &mut App) {\n    app.drain_system_state_actions();\n}\n",
            &[2],
        ),
        (
            HOST,
            "impl App {\n    fn after_close(&mut self) {\n        \
             self.drain_system_state_actions();\n    }\n}\n",
            &[3],
        ),
        (
            NATIVE_EVENTS,
            "impl App {\n    fn replay_actions(&mut self) {\n        \
             self.drain_system_state_actions();\n    }\n}\n",
            &[3],
        ),
        (
            SETUP,
            "impl App {\n    fn shortcut(&mut self) {\n        \
             self.workstation_setup.permissions.accepted();\n        \
             let mut gate = PermissionGate::default();\n        \
             gate.observe(Some(policy));\n        \
             PermissionGate::take_press(&mut gate, 0);\n    }\n}\n",
            &[3, 5, 6],
        ),
        (
            SETUP,
            "#[path = \"workstation_setup_extra.rs\"]\nmod extra;\nmod inline {\n}\n",
            &[2, 3],
        ),
        (
            "crates/aterm-gui/src/control_testbed.rs",
            "fn sneaky(app: &mut App) -> bool {\n    app.open_access_from_system_state(access, true)\n}\n",
            &[2],
        ),
        (
            STRANGER,
            "#[cfg(test)]\nmod tests {\n    fn t(app: &mut App) {\n        \
             app.open_access_from_system_state(access, true);\n    }\n}\n",
            &[],
        ),
    ];
    for &(rel, plant, lines) in cases {
        let want: Vec<String> = lines
            .iter()
            .map(|n| format!("{rel}:{}", fence_base(rel) + n))
            .collect();
        assert_eq!(fenced(SETUP_TWIN, &[(rel, plant)]), want, "{plant}");
    }
    let declared = [
        (
            "crates/aterm-gui/src/control.rs",
            "#[cfg(test)]\nmod tests;\n",
        ),
        (
            "crates/aterm-gui/src/control/tests.rs",
            "fn t(app: &mut App) {\n    app.open_access_from_system_state(access, true);\n}\n",
        ),
    ];
    assert_eq!(fenced(SETUP_TWIN, &declared), Vec::<String>::new());
    // Renamed: the fence's function is gone from its home, and the gate's
    // `observe` is now called from a function no fence names.
    let renamed = SETUP_TWIN.replace(
        "fn observe_initial_setup_permissions",
        "fn observe_initial_setup_perms",
    );
    assert_eq!(
        fenced(&renamed, &[]),
        [format!("{SETUP}:1"), format!("{SETUP}:18")]
    );
}

/// A sidestep: its name, the files it plants (path, text), and where the
/// fence flags it (path, line past the fence's base).
type Sidestep = (
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [(&'static str, usize)],
);

/// Each sidestep of the fences is flagged at the lines named: a second `fn` named like an
/// allowed caller, which any file could call (G1 and G2, spelled across lines or in a trait's
/// impl too); a test-only file a shipped `#[path]` mounts again (G4); a gate transition taken
/// as a value, a macro's argument or a one-line wrapper (G5); an `include!` in the gate's
/// parent, or a macro whose body takes a transition (G6). An `include!` elsewhere and a macro
/// naming a transition as neither a method nor a path are silent.
#[test]
fn the_round_four_sidesteps_of_the_fences_are_flagged() {
    let cases: &[Sidestep] = &[
        (
            "G1",
            &[
                (
                    GUI_LIB,
                    "fn on_user_event(app: &mut App, s: SetupStatus) {\n    \
                     app.receive_system_state_status(s);\n}\n",
                ),
                (
                    STRANGER,
                    "fn forge(app: &mut App) {\n    crate::on_user_event(app, made_up);\n}\n",
                ),
            ],
            &[(GUI_LIB, 1)],
        ),
        (
            "G2",
            &[
                (
                    HOST,
                    "fn tick_system_state(app: &mut App) {\n    \
                     app.drain_system_state_actions();\n}\n",
                ),
                (
                    STRANGER,
                    "fn forge(app: &mut App) {\n    \
                     crate::system_state_host::tick_system_state(app, any);\n}\n",
                ),
            ],
            &[(HOST, 1)],
        ),
        (
            "G2 across lines: its call reads as no fn's",
            &[(
                HOST,
                "pub(crate) fn\ntick_system_state(app: &mut App) {\n    \
                 app.drain_system_state_actions();\n}\n",
            )],
            &[(HOST, 1), (HOST, 3)],
        ),
        (
            "G2 in a trait's impl",
            &[(
                HOST,
                "impl Tick for App {\n    fn tick_system_state(&mut self) {\n        \
                 self.drain_system_state_actions();\n    }\n}\n",
            )],
            &[(HOST, 2)],
        ),
        (
            "the dispatcher cannot skip the drain to supply its own action",
            &[(
                NATIVE_EVENTS,
                "impl App {\n    fn dispatch_native_target_event(&mut self) {\n        \
                 self.run_system_state_action(made_up);\n    }\n}\n",
            )],
            &[(NATIVE_EVENTS, 2), (NATIVE_EVENTS, 3)],
        ),
        (
            "a namesake drain cannot run an action from a free function",
            &[(
                HOST,
                "fn drain_system_state_actions(app: &mut App, made_up: SystemStateAction) {\n    \
                 app.run_system_state_action(made_up);\n}\n",
            )],
            &[(HOST, 1)],
        ),
        (
            "a second drain cannot run an action under the permitted caller's name",
            &[(
                HOST,
                "impl App {\n    fn drain_system_state_actions(&mut self) {\n        \
                 self.run_system_state_action(made_up);\n    }\n}\n",
            )],
            &[(HOST, 2)],
        ),
        (
            "G4",
            &[
                (
                    STRANGER,
                    "#[cfg(test)]\nmod tests;\n#[path = \"control/tests.rs\"]\nmod shipped;\n",
                ),
                (
                    "crates/aterm-gui/src/control/tests.rs",
                    "pub(crate) fn forge(app: &mut App) {\n    app.open_access_from_system_state(access, true);\n}\n",
                ),
            ],
            &[("crates/aterm-gui/src/control/tests.rs", 2)],
        ),
        (
            "G5",
            &[(
                SETUP,
                "impl App {\n    fn shortcut(&mut self) {\n        \
                 let accept = PermissionGate::accepted;\n        \
                 accept(&mut self.workstation_setup.permissions);\n    }\n}\n",
            )],
            &[(SETUP, 3)],
        ),
        (
            "G5 a macro's argument",
            &[(
                SETUP,
                "impl App {\n    fn shortcut(&mut self) {\n        \
                 crate::on_gate!(self.workstation_setup.permissions, accepted);\n    }\n}\n",
            )],
            &[(SETUP, 3)],
        ),
        (
            "G5 a one-line wrapper",
            &[(
                SETUP,
                "pub(crate) fn accept(gate: &mut PermissionGate) { gate.accepted() }\n",
            )],
            &[(SETUP, 1)],
        ),
        (
            "G6",
            &[
                (SETUP, "include!(\"workstation_setup_more.rs\");\n"),
                (
                    "crates/aterm-gui/src/workstation_setup_more.rs",
                    "impl App {\n    fn shortcut(&mut self) {\n        \
                     self.workstation_setup.permissions.accepted();\n        \
                     self.workstation_setup.permissions.observe(Some(policy));\n    }\n}\n",
                ),
            ],
            &[(SETUP, 1)],
        ),
        (
            "G6 a macro whose body takes a transition, invoked in the gate's parent",
            &[
                (
                    "crates/aterm-gui/src/macros.rs",
                    "macro_rules! forge {\n    ($g:expr) => {\n        $g.accepted();\n        \
                     $g.observe(Some(policy))\n    };\n}\n\
                     macro_rules! log {\n    ($x:expr) => {\n        observe($x)\n    };\n}\n",
                ),
                (
                    SETUP,
                    "impl App {\n    fn shortcut(&mut self) {\n        \
                     forge!(self.workstation_setup.permissions);\n    }\n}\n",
                ),
            ],
            &[
                ("crates/aterm-gui/src/macros.rs", 3),
                ("crates/aterm-gui/src/macros.rs", 4),
            ],
        ),
        (
            "CONTROL: an `include!` outside the gate's parent",
            &[(STRANGER, "include!(\"control_table.rs\");\n")],
            &[],
        ),
    ];
    for &(name, plants, lines) in cases {
        let want: Vec<String> = lines
            .iter()
            .map(|&(rel, n)| format!("{rel}:{}", fence_base(rel) + n))
            .collect();
        assert_eq!(fenced(SETUP_TWIN, plants), want, "{name}");
    }
}

/// What becomes of a plant.
enum Fate {
    /// The census flags it, at these lines of the plant.
    Flagged(&'static [usize]),
    /// rustc refuses it when planted into aterm-gui, with this error; the census need not
    /// see it.
    Refused(&'static str),
}

/// Each plant runs alone. A token is a braced struct with a private mark field in a leaf of
/// its own, so a mint outside the leaf does not compile (`Refused`) and the same mint inside
/// it is flagged; the unsafe reads, aliases and copy impls no privacy stops are flagged where
/// they stand.
#[test]
fn the_checkers_plants_are_flagged_or_no_longer_compile() {
    use Fate::{Flagged, Refused};
    let plants: &[(&str, &str, &str, Fate)] = &[
        (
            "P1 a macro in the yes's old home expands to `OwnerYes(())`",
            YES_PARENT,
            "macro_rules! mint {\n    ($t:ident) => {\n        $t(())\n    };\n}\n\
             pub(crate) fn sneaky() -> OwnerYes {\n    mint!(OwnerYes)\n}\n",
            Refused("E0423: expected function, found struct `OwnerYes`"),
        ),
        (
            "P1 the macro spelling the braced field",
            YES_PARENT,
            "macro_rules! mint {\n    ($t:ident) => {\n        $t { _seal: todo!() }\n    };\n}\n\
             pub(crate) fn sneaky() -> OwnerYes {\n    mint!(OwnerYes)\n}\n",
            Refused("E0451: field `_seal` of struct `OwnerYes` is private"),
        ),
        (
            "P1 the macro in the leaf",
            YES_LEAF,
            "macro_rules! mint {\n    ($t:ident) => {\n        $t { _seal: YesSeal }\n    };\n}\n\
             pub(crate) fn sneaky() -> OwnerYes {\n    mint!(OwnerYes)\n}\n",
            Flagged(&[1, 3, 7]),
        ),
        (
            "P2 the constructor as a fn value in a const",
            YES_PARENT,
            "pub(crate) const MINT: fn(()) -> OwnerYes = OwnerYes;\n",
            Refused(
                "E0423: expected value, found struct `OwnerYes`; a braced struct has no constructor fn, in the leaf either",
            ),
        ),
        (
            "P2 a const that mints, in the leaf",
            YES_LEAF,
            "pub(crate) const MINT: fn() -> OwnerYes = || OwnerYes { _seal: YesSeal };\n",
            Flagged(&[1]),
        ),
        (
            "P3 the constructor passed to map",
            YES_PARENT,
            "pub(crate) fn sneaky() -> Option<OwnerYes> {\n    Some(()).map(OwnerYes)\n}\n",
            Refused("E0423: expected value, found struct `OwnerYes`"),
        ),
        (
            "P3 a closure that mints, in the leaf",
            YES_LEAF,
            "pub(crate) fn sneaky() -> Option<OwnerYes> {\n    \
             Some(YesSeal).map(|s| OwnerYes { _seal: s })\n}\n",
            Flagged(&[2]),
        ),
        (
            "P4 `Self` passed to map in `impl OwnerYes`",
            YES_PARENT,
            "impl OwnerYes {\n    pub(crate) fn sneaky() -> Option<Self> {\n        \
             Some(()).map(Self)\n    }\n}\n",
            Refused("the `Self` constructor can only be used with tuple or unit structs"),
        ),
        (
            "P4 `Self { .. }` in a closure, in the leaf",
            YES_LEAF,
            "impl OwnerYes {\n    pub(crate) fn sneaky() -> Option<Self> {\n        \
             Some(YesSeal).map(|_seal| Self { _seal })\n    }\n}\n",
            Flagged(&[3]),
        ),
        (
            "P5 `Self(())` in a #[path] child of the old home",
            SIBLING,
            "impl super::OwnerYes {\n    pub(crate) fn sneaky() -> Self {\n        Self(())\n    }\n}\n",
            Refused(
                "the `Self` constructor can only be used with tuple or unit structs; braced, E0451: the child is the leaf's sibling",
            ),
        ),
        (
            "P5 `Self { .. }` in a second inherent impl, in the leaf",
            YES_LEAF,
            "impl OwnerYes {\n    pub(crate) fn sneaky() -> Self {\n        \
             Self { _seal: YesSeal }\n    }\n}\n",
            Flagged(&[3]),
        ),
        (
            "P6 `Self { 0: () }` in the child",
            SIBLING,
            "impl super::OwnerYes {\n    pub(crate) fn sneaky() -> Self {\n        Self { 0: () }\n    }\n}\n",
            Refused("E0560: struct `OwnerYes` has no field named `0`"),
        ),
        (
            "P6 a functional update, in the leaf",
            YES_LEAF,
            "impl OwnerYes {\n    pub(crate) fn sneaky(y: Self) -> Self {\n        Self { ..y }\n    }\n}\n",
            Flagged(&[3]),
        ),
        (
            "P7 transmute anywhere",
            CALLER_FILE,
            "pub(crate) fn sneaky() -> OwnerYes {\n    unsafe { std::mem::transmute(()) }\n}\n",
            Flagged(&[2]),
        ),
        (
            "P8 mem::zeroed anywhere",
            CALLER_FILE,
            "pub(crate) fn sneaky() {\n    let y: OwnerYes = unsafe { std::mem::zeroed() };\n}\n",
            Flagged(&[2]),
        ),
        (
            "P9 ptr::read duplicates a spent OwnerYes",
            CALLER_FILE,
            "pub(crate) fn twice(a: &OwnerYes) -> OwnerYes {\n    \
             unsafe { std::ptr::read(a) }\n}\n",
            Flagged(&[2]),
        ),
        (
            "P10 the name and its paren on two lines",
            YES_PARENT,
            "pub(crate) fn sneaky() -> OwnerYes {\n    OwnerYes\n        (())\n}\n",
            Refused("E0423: expected function, found struct `OwnerYes`"),
        ),
        (
            "P10 the name and its brace on two lines, in the leaf",
            YES_LEAF,
            "pub(crate) fn sneaky() -> OwnerYes {\n    OwnerYes\n        { _seal: YesSeal }\n}\n",
            Flagged(&[2, 3]),
        ),
        (
            "P11 a shipped fn on a cfg(test) item's line",
            YES_PARENT,
            "#[cfg(test)]\nfn t() {} pub(crate) fn sneaky() -> OwnerYes { OwnerYes(()) }\n",
            Refused("E0423: expected function, found struct `OwnerYes`"),
        ),
        (
            "P11 the same in the leaf, whose every line is read",
            YES_LEAF,
            "#[cfg(test)]\nfn t() {} pub(crate) fn sneaky() -> OwnerYes { OwnerYes { _seal: YesSeal } }\n",
            Flagged(&[2]),
        ),
        (
            "P12 a parenthesised alias (and E0560: no field `0`)",
            YES_PARENT,
            "type Granted = (OwnerYes);\npub(crate) fn sneaky() -> Granted {\n    Granted { 0: () }\n}\n",
            Flagged(&[1]),
        ),
        (
            "P12 the alias in the leaf",
            YES_LEAF,
            "type Granted = (OwnerYes);\npub(crate) fn sneaky() -> Granted {\n    \
             Granted { _seal: YesSeal }\n}\n",
            Flagged(&[1, 3]),
        ),
        (
            "P14 `impl Clone` split over two lines (it builds nothing; `impl Copy` is E0204)",
            YES_PARENT,
            "impl Clone\n    for OwnerYes\n{\n    fn clone(&self) -> Self {\n        todo!()\n    }\n}\n",
            Flagged(&[1]),
        ),
        (
            "P14 the same in the leaf",
            YES_LEAF,
            "impl Clone\n    for OwnerYes\n{\n    fn clone(&self) -> Self {\n        todo!()\n    }\n}\n",
            Flagged(&[1, 5]),
        ),
        (
            "P15 a use rename split over two lines",
            CALLER_FILE,
            "use crate::native_settings::{\n    OwnerYes\n    as Granted,\n};\nfn f() -> Granted { todo!() }\n",
            Flagged(&[1]),
        ),
        (
            "P16 a generic alias with a default (and E0071)",
            YES_PARENT,
            "type Granted<T = OwnerYes> = T;\npub(crate) fn sneaky() -> Granted {\n    Granted { 0: () }\n}\n",
            Flagged(&[1]),
        ),
        (
            "P16 the same in the leaf",
            YES_LEAF,
            "type Granted<T = OwnerYes> = T;\npub(crate) fn sneaky() -> Granted {\n    \
             Granted { _seal: YesSeal }\n}\n",
            Flagged(&[1, 3]),
        ),
        (
            "P17 `Self(u)` in a From impl",
            YES_PARENT,
            "impl From<()> for OwnerYes {\n    fn from(u: ()) -> Self {\n        Self(u)\n    }\n}\n",
            Refused("the `Self` constructor can only be used with tuple or unit structs"),
        ),
        (
            "P17 a From impl in the leaf",
            YES_LEAF,
            "impl From<YesSeal> for OwnerYes {\n    fn from(_seal: YesSeal) -> Self {\n        \
             Self { _seal }\n    }\n}\n",
            Flagged(&[1, 2, 3]),
        ),
        (
            "P18 a literal in another fn",
            YES_PARENT,
            "fn other() -> OwnerYes {\n    OwnerYes { 0: () }\n}\n",
            Refused("E0560: struct `OwnerYes` has no field named `0`"),
        ),
        (
            "P18 the literal in another fn of the leaf",
            YES_LEAF,
            "fn other() -> OwnerYes {\n    OwnerYes { _seal: YesSeal }\n}\n",
            Flagged(&[2]),
        ),
        (
            "P19 a re-export under another name",
            YES_PARENT,
            "pub(crate) use self::OwnerYes as Granted;\n",
            Flagged(&[1]),
        ),
        (
            "P19 the same in the leaf",
            YES_LEAF,
            "pub(crate) use self::OwnerYes as Granted;\n",
            Flagged(&[1]),
        ),
        (
            "P20 `Self` passed to map in the child",
            SIBLING,
            "impl super::OwnerYes {\n    pub(crate) fn sneaky() -> Option<Self> {\n        \
             Some(()).map(Self)\n    }\n}\n",
            Refused("the `Self` constructor can only be used with tuple or unit structs"),
        ),
    ];
    for (name, rel, plant, fate) in plants {
        let found = with_plant(YES, GATE, rel, plant);
        match fate {
            Flagged(lines) => assert_eq!(found, planted(rel, lines), "{name}"),
            Refused(why) => assert!(!why.is_empty(), "{name}"),
        }
    }
}

#[test]
fn a_door_ffi_name_is_declared_in_its_one_file() {
    let decl = "unsafe extern \"system\" {\n    fn CreateProcessW(a: *const u16) -> i32;\n}\n";
    let found = run(
        &FFI_DOOR,
        &[
            ("crates/aterm-pty/src/windows/ffi.rs", decl),
            ("crates/aterm-cli/src/win.rs", decl),
            (
                "crates/aterm-gui/src/app_mouse.rs",
                "fn open_url_windows() {\n    unsafe extern \"system\" { fn ShellExecuteW(); }\n}\n",
            ),
            (
                "crates/aterm-gpu/src/probe.rs",
                "unsafe extern \"C\" { fn MTLCreateSystemDefaultDevice() -> Id; }\n\
                 fn create_process_verdict() {}\nfn x() { let f = WinExec; }\n",
            ),
            (
                "crates/aterm-gui/src/launch.rs",
                "extern \"system\" { fn WinExec(); }\n",
            ),
            ("crates/aterm-uds/tests/windows_spawn_console.rs", decl),
            (
                "crates/aterm-cli/src/console.rs",
                &format!("#[cfg(test)]\nmod tests {{\n{decl}}}\n"),
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-cli/src/win.rs:2",
            "crates/aterm-gpu/src/probe.rs:1",
            "crates/aterm-gui/src/launch.rs:1",
        ]
    );
}

/// The boot id is read by name only in astream: an inline call, a `const` name, a
/// `sysctlnametomib` lookup and a test's own read are found.
#[test]
fn the_boot_id_is_read_by_astream_alone() {
    let inline = "pub fn boot_session() -> Option<String> {\n    let rc = unsafe {\n        \
                  libc::sysctlbyname(\n            c\"kern.bootsessionuuid\".as_ptr(),\n            \
                  buf.as_mut_ptr().cast(),\n        )\n    };\n}\n";
    let by_const = "const BOOT: &CStr = c\"kern.bootsessionuuid\";\n\
                    fn sysctl_text(name: &CStr) -> Option<String> {\n    \
                    unsafe { sysctlbyname(name.as_ptr(), p, l, n, 0) };\n}\n\
                    pub fn boot_id() -> Option<u128> { sysctl_text(BOOT)?.parse().ok() }\n";
    let by_mib = "fn mib() { sysctlnametomib(c\"kern.bootsessionuuid\".as_ptr(), m, l); }\n";
    let in_test = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        \
                   unsafe { libc::sysctlbyname(c\"kern.bootsessionuuid\".as_ptr(), p, l, n, 0) };\n    \
                   }\n}\n";
    let oracle = "fn kernel_text() -> String {\n    let mut sysctl = command(\"sysctl\");\n    \
                  sysctl.args([\"-n\", \"kern.bootsessionuuid\"]);\n    run(sysctl)\n}\n";
    let prose = "// Not kern.bootsessionuuid: that is astream's (astream_sys::boot_session).\n\
                 /// Unlike `kern.bootsessionuuid`, `kern.sleeptime` moves.\n\
                 fn slept() { unsafe { sysctlbyname(c\"kern.sleeptime\".as_ptr(), p, l, n, 0) }; }\n";
    let found = run(
        &BOOT_ID_DOOR,
        &[
            ("crates/aterm-uds/src/process.rs", inline),
            ("crates/aterm-acaffeine/src/instance.rs", by_const),
            ("crates/aterm-x/src/mib.rs", by_mib),
            ("crates/aterm-y/src/probe.rs", in_test),
            ("crates/aterm-link/src/durability.rs", oracle),
            ("crates/aterm-z/src/sleep.rs", prose),
            ("crates/aterm-census/src/guards/doors.rs", inline),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-acaffeine/src/instance.rs:1",
            "crates/aterm-uds/src/process.rs:4",
            "crates/aterm-x/src/mib.rs:1",
            "crates/aterm-y/src/probe.rs:5",
        ],
        "{found:#?}"
    );
}

#[test]
fn a_handoff_name_is_spelled_only_as_a_handoff_name() {
    let src = "pub(crate) const ENV_FDS: HandoffName = HandoffName::new(\"ATERM_SEAMLESS_FDS\");\n\
               const PARK: HandoffName =\n    HandoffName::new(\"ATERM_HANDOFF_PARK\");\n\
               fn t() { let ms = dev_seam!(\"ATERM_HANDOFF_READY_TIMEOUT_MS\"); }\n\
               fn r() { std::env::var_os(\"ATERM_SEAMLESS_NONCE\"); }\n\
               fn s() { let k = \"ATERM_HANDOFF_CLAIM\".to_string(); }\n\
               #[cfg(test)]\nmod tests {\n    const K: &str = \"ATERM_SEAMLESS_NONCE\";\n}\n";
    let found = run(
        &HANDOFF_NAME,
        &[
            ("crates/aterm-gui/src/seamless.rs", src),
            (HANDOFF_HOME, src),
            ("crates/aterm-gui/src/seamless_tests.rs", src),
            ("crates/aterm-cli/src/lib.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-gui/src/seamless.rs:5",
            "crates/aterm-gui/src/seamless.rs:6",
        ]
    );
}

#[test]
fn a_kill_never_names_a_core_signal() {
    let rust = "fn a() { unsafe { libc::kill(pid, libc::SIGABRT) }; }\n\
                fn b() { unsafe { libc::kill(pid, libc::SIGKILL) }; }\n\
                fn c() { Command::new(\"kill\").args([\"-SEGV\", &pid]); }\n\
                fn d() { Command::new(\"kill\").args([\"-KILL\", &pid]); }\n\
                fn e() { unsafe { libc::raise(sig) }; }\n\
                fn f() { let quit = libc::SIGQUIT; handle(quit); }\n\
                fn g() { sh(&format!(\"kill -SEGV {pid}\")); }\n\
                fn h() { Command::new(\"kill\").args([\"-s\", \"SEGV\", pid]); }\n\
                fn i() { Command::new(\"timeout\").args([\"-s\", \"QUIT\", \"5\", \"x\"]); }\n\
                fn j() { Command::new(\"head\").args([\"-n\", \"5\"]); }\n\
                fn k() { sh(\"kill -9 $pid; timeout -s KILL 5 x\"); }\n";
    let shell = "#!/bin/sh\nkill -ABRT $$\n# kill -KILL $$ is the way\npkill -s QUIT aterm\n\
                 killall -11 x\ntimeout --signal=SEGV 5 x\ngtimeout -s TRAP 5 x\n\
                 kill -TERM $pid\nskill -ABRT x\n";
    let python = "import os, signal, ctypes, faulthandler\nos.abort()\n\
                  from os import getpid, abort\nctypes.CDLL(None).abort()\n\
                  faulthandler._sigsegv()\nctypes.string_at(0)\n\
                  os.kill(pid, signal.SIGSEGV)\nos.kill(pid, 6)\nsignal.raise_signal(11)\n\
                  os.kill(pid, 9)\nos._exit(1)\nsignal.pthread_kill(t, signal.SIGQUIT)\n";
    let node = "process.abort();\nprocess.kill(pid, 'SIGILL');\nprocess.kill(pid, 'SIGKILL');\n";
    let found = run(
        &CORE_SIGNAL,
        &[
            ("crates/aterm-gui/tests/stand_in.rs", rust),
            ("tools/stand-in.sh", shell),
            ("tools/probe.py", python),
            ("tools/x.mjs", node),
            (
                "crates/aterm-census/src/guards/doors.rs",
                "fn a() { libc::kill(pid, libc::SIGABRT); }\n",
            ),
        ],
    );
    let at = |path: &str, lines: &[usize]| -> Vec<String> {
        lines.iter().map(|n| format!("{path}:{n}")).collect()
    };
    let expected: Vec<String> = [
        at("crates/aterm-gui/tests/stand_in.rs", &[1, 3, 7, 8, 9]),
        at("tools/probe.py", &[2, 3, 4, 5, 6, 7, 8, 9, 12]),
        at("tools/stand-in.sh", &[2, 4, 5, 6, 7]),
        at("tools/x.mjs", &[1, 2]),
    ]
    .concat();
    assert_eq!(spots(&found), expected, "{found:#?}");
}

/// A long file is read in runs of whole lines: a signal far into it is found on its own
/// line, and lines that name none give nothing.
#[test]
fn a_signal_deep_in_a_long_file_is_found_on_its_line() {
    let filler: String = (0..300)
        .map(|i| format!("fn quiet_{i}() {{ let x = {i}; }}\n"))
        .collect();
    let sends = "fn a() { unsafe { libc::kill(pid, libc::SIGABRT) }; }\n";
    let long = format!("{filler}{sends}{filler}{sends}");
    let found = run(&CORE_SIGNAL, &[("crates/x/src/lib.rs", &long)]);
    assert_eq!(
        spots(&found),
        ["crates/x/src/lib.rs:301", "crates/x/src/lib.rs:602"]
    );
    assert!(run(&CORE_SIGNAL, &[("crates/x/src/lib.rs", &filler)]).is_empty());
}
