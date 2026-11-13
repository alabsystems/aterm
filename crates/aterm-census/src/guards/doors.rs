// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The door guards: an authority the tree reaches through exactly one door,
//! where no type, visibility or clippy.toml row can say so (a public fn's
//! callers, a local `extern` declaration, a literal's shape).

use std::collections::{BTreeMap, BTreeSet};

use super::graph::{attribute, fn_name, fn_name_at};
use super::rules::{CENSUS_CRATE, PRODUCT, use_leaves};
use super::{Bound, Finding, Line, Rule, SourceFile, Tree, calls, has, hits, is_word};
use crate::lex::Lexed;

/// Product source this crate does not own: the census spells every name its
/// rules ban, in its tables and its planted cases.
fn product<'a>(tree: &Tree<'a>) -> impl Iterator<Item = &'a SourceFile> {
    tree.corpus
        .under(PRODUCT)
        .filter(|f| !f.rel.starts_with(CENSUS_CRATE))
}

/// Where the change runner, the tool's path and its argv builders are defined.
const DEVTOOLS_HOME: &str = "crates/aterm-containment/src/consent.rs";
/// The file of the one function that may run a change.
const DEVTOOLS_FLOW: &str = "crates/aterm-gui/src/debugger_access.rs";
/// Setup's one early elevation: its root half, run as root under the
/// person's own Mac password, turns developer mode on (`DeveloperMode::apply`). Its one shipped
/// spelling of the tool is the path constant's definition ([`ELEVATION_PATH_LINE`]).
const ELEVATION: &str = "crates/aterm-machines/src/root_steps.rs";
/// That definition, whole, as [`squeeze_code`] reads it.
const ELEVATION_PATH_LINE: &str = r#"pubconstDEVTOOLS_SECURITY:&str="/usr/sbin/DevToolsSecurity";"#;
/// That function: the live arm that runs a change for a yes, the owner's in aterm's alert.
const DEVTOOLS_WORKER: &str = "spawn_devtools_worker";
/// The runner: public, so aterm-gui can reach it.
const DEVTOOLS_RUNNER: &str = "run_devtools_change";
const DEVTOOLS_PATH: &str = "/usr/sbin/DevToolsSecurity";
/// The constant consent.rs defines the path as.
const DEVTOOLS_PATH_CONST: &str = "DEVTOOLSSECURITY_PATH";
/// The flags a printed or probed path may carry: neither changes anything.
const DEVTOOLS_QUIET_FLAGS: &[&str] = &["-status", "-disable"];
/// Words that name the tool where a process is built, beside its name and a
/// `devtools_<x>_command` builder ([`names_the_tool`]): its path constant, the
/// revert line (copy for a human, never spawned) and the change type.
const DEVTOOLS_TOOL_WORDS: &[&str] = &[
    "DEVTOOLSSECURITY_PATH",
    "DEBUGGER_ACCESS_REVERT",
    "DebuggerChange",
];
/// The calls that build or run a process: an argv, `posix_spawn`, the `exec`
/// family and consent.rs's runners ([`builds_a_process`] adds `Command::new` and
/// `spawnfd::command`).
const SPAWN_CALLS: &[&str] = &[
    "arg",
    "args",
    "posix_spawn",
    "posix_spawnp",
    "execv",
    "execve",
    "execvp",
    "execl",
    "execle",
    "execlp",
    "run_bounded",
    "run_bounded_exit",
    "run_to_exit_keeping_heads",
];
/// The authorization-database and privileged-helper APIs, by name.
const AUTHZ_NAMES: &[&str] = &[
    "AuthorizationRightSet",
    "AuthorizationCopyRights",
    "SMJobBless",
    "daemonServiceWithPlistName",
];
/// The requests for administrator rights a literal carries.
const AUTHZ_LITERALS: &[&str] = &["authorizationdb write", "with administrator privileges"];
/// The owner's yes a change spends, and the leaf module that alone can build one: the type,
/// its private mark ([`Token::mark`]) and its doors.
const OWNER_YES: &str = "OwnerYes";
const OWNER_YES_LEAF: &str = "crates/aterm-gui/src/native_settings_owner_yes.rs";
/// The permission gate's leaf, which records the window's own admission of a setup plan and a
/// person's press ([`FENCES`]).
const ADMISSION_LEAF: &str = "crates/aterm-gui/src/workstation_setup_gate.rs";
/// The module name the gate's parent mounts its leaf under.
const GATE_MODULE: &str = "permission_gate";

/// A yes token, which the compiler keeps to its leaf: a braced struct whose
/// fields are private to a module with no children, so only that file can
/// build one, and with no constructor function to pass around. The census
/// holds the leaf to the shape that keeps it so, and the rest of the product
/// to what privacy cannot stop.
struct Token {
    ty: &'static str,
    /// The private unit struct the token's field holds, defined in the leaf and named nowhere
    /// else, so nothing outside the leaf can build the token; being neither `Clone` nor `Copy`,
    /// it lets no `impl Copy` of the token compile. In the leaf a value of it (outside its
    /// definition and the token's field) is a mint; outside it, only a macro's body could name it.
    mark: &'static str,
    /// The token, its mark, its doors, and nothing that could build one
    /// beside them.
    leaf: &'static str,
    /// The one file that mounts the leaf (`#[path = "<leaf>"] mod ..;`).
    parent: &'static str,
    /// The functions that may build one.
    doors: &'static [YesDoor],
}

/// A function of a token's leaf that may build the token: the first fn of its
/// name at the leaf's top level (`within` `None`) or directly inside an
/// inherent `impl` of `within`, whose [`DoorShape`] holds. A fn of its name
/// anywhere else (a second one, a trait's or a trait impl's method, one in a
/// nested block) is no door.
#[derive(Clone, Copy)]
struct YesDoor {
    name: &'static str,
    within: Option<&'static str>,
    shape: DoorShape,
}

/// What a door's signature and body must hold for it to be the door.
#[derive(Clone, Copy)]
enum DoorShape {
    /// The alert: a `&<gestures>` parameter (`gestures: &ConsentGestures`)
    /// whose alert arm the body asks through the arm's own type
    /// (`ConfirmArm::ask(gestures.confirm, ..)`), so the arm the gestures hold
    /// is that type, whatever a parent's struct says, or nothing compiles. The
    /// arm is defined in the leaf, aterm's alert or nobody ([`arm_findings`]).
    Asks {
        gestures: &'static str,
        arm: &'static str,
    },
    /// A mint for tests: `#[cfg(test)]` on it.
    TestOnly,
}

/// The token and its doors: the owner's yes in aterm's alert (the free fn
/// `owner_confirms_with`), and its `for_test` under `#[cfg(test)]`.
const TOKENS: [Token; 1] = [Token {
    ty: OWNER_YES,
    mark: "YesSeal",
    leaf: OWNER_YES_LEAF,
    parent: "crates/aterm-gui/src/native_settings.rs",
    doors: &[
        YesDoor {
            name: "owner_confirms_with",
            within: None,
            shape: DoorShape::Asks {
                gestures: "ConsentGestures",
                arm: "ConfirmArm",
            },
        },
        YesDoor {
            name: "for_test",
            within: Some(OWNER_YES),
            shape: DoorShape::TestOnly,
        },
    ],
}];

/// The derives a token or its mark may carry: none makes or copies one.
const TOKEN_DERIVES: &[&str] = &["Debug", "PartialEq", "Eq"];
/// The derives any other type in a leaf may carry, by their bare names: std's,
/// which expand into impls of the traits they name and nothing else.
const STD_DERIVES: &[&str] = &[
    "Debug",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Default",
];
/// The attributes a leaf may carry besides `derive`: none expands into code.
/// `cfg_attr` is not one: it applies any attribute.
const LEAF_ATTRIBUTES: &[&str] = &[
    "cfg", "default", "must_use", "inline", "allow", "expect", "doc",
];
/// The paths a leaf may invoke a macro by: from the extern prelude, which no
/// `macro_rules!` of the crate can shadow. A bare `matches!` resolves through
/// textual scope first, where a macro defined above the leaf's `mod` line in
/// its parent would expand in the leaf, with the leaf's privacy.
const LEAF_MACRO_ROOTS: &[&str] = &["::std::", "::core::", "::alloc::", "::aterm_log::"];
/// The macros that bring source from another file into the leaf.
const SOURCE_MACROS: &[&str] = &["include"];
/// The names a leaf may not import: an import of one would shadow the
/// prelude's derive or the extern crate a macro path starts at.
const LEAF_SHADOWS: &[&str] = &["std", "core", "alloc", "aterm_log"];
/// The traits that would make a second token from one, or one from nothing.
const TOKEN_COPIES: &[&str] = &["Clone", "Copy", "Default"];
/// What forges a value with no constructor, or copies one past its type: the
/// unsafe reads no privacy stops, by word, or by substring for a path or a
/// raw pointer type.
const UNSAFE_READS: &[(&str, Bound)] = &[
    ("transmute", Bound::Word),
    ("transmute_copy", Bound::Word),
    ("zeroed", Bound::Word),
    ("MaybeUninit", Bound::Word),
    ("assume_init", Bound::Word),
    ("ptr::read", Bound::Sub),
    ("read_unaligned", Bound::Word),
    ("read_volatile", Bound::Word),
    ("ptr::copy", Bound::Sub),
    ("copy_nonoverlapping", Bound::Word),
    ("*const", Bound::Sub),
    ("*mut", Bound::Sub),
];

/// The rule. The compiler keeps each token to its leaf (a private field nothing outside the
/// leaf can name); this guard keeps the leaf to the shape that makes that so
/// ([`leaf_findings`]), the alert's arm to a plain enum of aterm's alert or nobody, each a unit
/// ([`arm_findings`]), the rest of the product to what privacy does not stop
/// ([`outside_findings`]), and what the permission gate trusts (the window's own admission and
/// a person's press, which alone open a page of a grant only a person can give) to their fenced
/// callers ([`FENCES`], [`caller_findings`], [`gate_macro_findings`]). A
/// file is test code only while no shipped line loads it ([`TestCode`](super::rules::TestCode)).
///
/// It is lexical, and does not see an unsafe read under another name (a generic helper, a
/// renamed import) or with an inferred token type, or a token an `extern` fn returns; what a
/// door's body hands out beside its token (a door's return type is not read); what the alert's
/// arm and the fenced callers' own bodies do; what reaches the outermost callers (the window's
/// event dispatch, `App::on_user_event`'s worker replies, the page's queue of actions); a
/// transition that a proc macro's expansion or a macro from outside the product calls in the
/// gate's parent; a consumer that takes another type where a yes belongs (the yes reaches
/// `DevToolsSecurity` through aterm-gui's `ChangeTicket` and `DevtoolsJob`); a crate-root
/// `#[macro_use] extern crate` whose derive or attribute takes a std name a leaf uses bare; or
/// what a std, core or alloc macro expands to (`aterm_log` is product source, so it is read). A
/// leaf that is gone is not read: its token, defined anywhere else, is a namesake finding.
pub(super) const DEVTOOLS_DOOR: Rule = Rule {
    id: "devtools-door",
    why: "DevToolsSecurity changes the debug right for the whole Mac and ENABLES when run with no \
          flag: no shipped line builds a process naming it, its path constant and argv live in \
          consent.rs's builders alone, only a yes runs `run_devtools_change`, through aterm-gui's \
          one worker `spawn_devtools_worker`; the yes (`OwnerYes`) is a braced struct with a \
          field of a private mark (`YesSeal`) in a leaf module of its own, which nothing \
          else can build, and the leaf holds no `mod`, macro definition, macro but by an \
          extern-prelude path, alias, glob or shadowing import, `unsafe`, trait impl for the \
          token, attribute that expands into code or function type on a shipped line, and a \
          value of the mark only in a door's body — the free `owner_confirms_with`, which asks \
          its `ConsentGestures`' arm through `ConfirmArm::ask` (an enum of aterm's alert or \
          nobody, a stand-in only under `#[cfg(test)]`), and its `#[cfg(test)]` `for_test`; \
          outside its leaf no line names a mark, no type takes a token's name, and nothing \
          aliases one, copies one by an impl, reads one through an unsafe read or a raw \
          pointer, or mounts the leaf again; the admission and the press the gate trusts are \
          taken only by their fenced callers, each one `App` method defined once, from the \
          window's events and the worker's replies in, the gate's parent brings in no source \
          and no product macro takes a gate transition; no shipped line holds the bare tool \
          path, and \
          aterm calls no authorization-database or privileged-helper API. The one other door is \
          setup's one early elevation: aterm-machines' root_steps.rs, \
          whose root half runs `DevToolsSecurity -enable` as root under the person's own Mac \
          password, defines the path once, as its `DEVTOOLS_SECURITY` constant",
    scope: &[
        DEVTOOLS_HOME,
        DEVTOOLS_FLOW,
        OWNER_YES_LEAF,
        ADMISSION_LEAF,
        GUI_LIB,
        SETUP,
        HOST,
        ACCESS,
        NATIVE_EVENTS,
        MOUSE_EVENTS,
        ELEVATION,
    ],
    check: devtools_door,
};

fn devtools_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    // `devtools_` stands for the runner too, whose name holds it.
    let mut needles = vec!["DevToolsSecurity", "devtools_"];
    needles.extend(DEVTOOLS_TOOL_WORDS);
    needles.extend(AUTHZ_NAMES);
    needles.extend(AUTHZ_LITERALS);
    let definition = format!("fn {DEVTOOLS_RUNNER}");
    for file in product(tree) {
        if !file.mentions(&needles) || tree.corpus.test_code().holds(&file.rel) {
            continue;
        }
        let home = file.rel == DEVTOOLS_HOME;
        // Each reason a line can give is one its file's views spell: a reason they do not
        // is left unasked, and a file that spells none is left unread.
        let Lexed { code, text } = file.lexed();
        let names: Vec<&str> = AUTHZ_NAMES
            .iter()
            .copied()
            .filter(|n| code.contains(n))
            .collect();
        let literals: Vec<&str> = AUTHZ_LITERALS
            .iter()
            .copied()
            .filter(|l| text.contains(l))
            .collect();
        let tool = text.contains("DevToolsSecurity")
            || DEVTOOLS_TOOL_WORDS.iter().any(|w| text.contains(w))
            || text.contains("devtools_");
        let runner = code.contains(DEVTOOLS_RUNNER);
        let path_const = code.contains(DEVTOOLS_PATH_CONST);
        let path = text.contains(DEVTOOLS_PATH);
        if names.is_empty() && literals.is_empty() && !tool && !runner && !path_const && !path {
            continue;
        }
        let shipped: BTreeSet<usize> = file.shipped_lines().map(|l| l.no).collect();
        for at in attribute(file) {
            let line = at.line;
            if at.in_test || !shipped.contains(&line.no) {
                continue;
            }
            let func = at.func.unwrap_or("-");
            let worker = file.rel == DEVTOOLS_FLOW && func == DEVTOOLS_WORKER;
            let why = if let Some(name) = names.iter().find(|n| has(line.code, n, Bound::Word)) {
                Some(format!("names `{name}`"))
            } else if let Some(lit) = literals.iter().find(|l| line.text.contains(*l)) {
                Some(format!("asks for `{lit}`"))
            } else if tool && builds_a_process(line.code) && names_the_tool(line.text) {
                Some("builds a process from DevToolsSecurity".to_string())
            } else if runner
                && has(line.code, DEVTOOLS_RUNNER, Bound::Word)
                && !has(line.code, &definition, Bound::Word)
                && !worker
            {
                Some(format!(
                    "names `{DEVTOOLS_RUNNER}` in `{func}`, not the gesture worker"
                ))
            } else if path_const
                && has(line.code, DEVTOOLS_PATH_CONST, Bound::Word)
                && !(home && path_const_allowed(line, func))
            {
                Some(if home {
                    format!(
                        "`{DEVTOOLS_PATH_CONST}` in `{func}`, outside its definition and its argv \
                         builders"
                    )
                } else {
                    format!("`{DEVTOOLS_PATH_CONST}` outside the consent module")
                })
            } else if path {
                bare_path(
                    line.text,
                    (home && has(line.code, DEVTOOLS_PATH_CONST, Bound::Word))
                        || (file.rel == ELEVATION
                            && squeeze_code(line.text) == ELEVATION_PATH_LINE),
                )
            } else {
                None
            };
            if let Some(why) = why {
                out.push(file.finding(DEVTOOLS_DOOR.id, line.no, why));
            }
        }
    }
    yes_tokens(tree, out);
}

/// The yes tokens' half of the rule: each leaf's shape ([`leaf_findings`]),
/// then every other product file ([`outside_findings`]).
fn yes_tokens(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for token in &TOKENS {
        if let Some(leaf) = tree.corpus.get(token.leaf) {
            leaf_findings(leaf, token, out);
        }
    }
    let mut needles: Vec<&str> = TOKENS.iter().map(|t| t.ty).collect();
    needles.extend(TOKENS.iter().map(|t| t.mark));
    needles.extend(TOKENS.iter().map(|t| file_name(t.leaf)));
    // A test's file too: a name that reads as a test's (a `_tests.rs`) is no
    // proof the build that ships leaves it out.
    for file in product(tree) {
        if TOKENS.iter().any(|t| t.leaf == file.rel) || !file.mentions(&needles) {
            continue;
        }
        outside_findings(file, out);
    }
    fence_findings(tree, out);
}

/// A path's file name.
fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// The column of a function type in a line of code, if one: a pointer type
/// (`fn(&str) -> bool`, `extern "C" fn(..)`) or a closure trait (`Fn(..)`,
/// `FnMut(..)`, `FnOnce(..)`, under `impl`, `dyn` or a bound). A fn item
/// (`fn name(`) is not one.
fn function_type(code: &str) -> Option<usize> {
    ["fn", "Fn", "FnMut", "FnOnce"]
        .iter()
        .flat_map(|word| {
            hits(code, word, Bound::Word)
                .filter(move |&at| code[at + word.len()..].trim_start().starts_with('('))
        })
        .min()
}

/// aterm-gui's source, where every fenced function is defined: each is
/// `pub(crate)` or narrower, so no other crate can call one.
const GUI_SRC: &str = "crates/aterm-gui/src/";
const GUI_LIB: &str = "crates/aterm-gui/src/lib.rs";
const SETUP: &str = "crates/aterm-gui/src/workstation_setup.rs";
const HOST: &str = "crates/aterm-gui/src/system_state_host.rs";
const ACCESS: &str = "crates/aterm-gui/src/system_state_access.rs";
const KEY_EVENTS: &str = "crates/aterm-gui/src/app_input.rs";
const NATIVE_EVENTS: &str = "crates/aterm-gui/src/app_native.rs";
const MOUSE_EVENTS: &str = "crates/aterm-gui/src/app_mouse.rs";

/// A function the permission gate's input passes through: what the window's own admission is
/// read from (the page's Apply, its acceptance, the status naming the plan) and a person's press.
/// The gate trusts what these hand it, so each is named only in the bodies of `callers`; a
/// caller anywhere else could hand it a made-up admission or a press nobody made
/// (`open_access_from_system_state(access, true)`).
struct Fence {
    name: &'static str,
    /// The file that defines it (`fn <name>`): a fence whose function is gone
    /// from there fences nothing.
    home: &'static str,
    /// `Some(file)`: a method of the permission gate, `pub(super)` in its
    /// leaf, so callable from `file` alone, where its name is read as a word
    /// (a call, a path taken as a value, a macro's argument). `None`: an `App`
    /// function, named as a word anywhere in aterm-gui.
    gate: Option<&'static str>,
    /// The (file, fn) pairs whose bodies may name it: each fn a method of
    /// [`CALLER_HOST`], defined once in its file ([`caller_findings`]), so
    /// no namesake there takes its leave.
    callers: &'static [(&'static str, &'static str)],
}

/// The type every fenced caller is an inherent method of.
const CALLER_HOST: &str = "App";

/// The fences, outermost (where the window's events and the worker's replies
/// come in) to innermost (the gate's own transitions).
const FENCES: &[Fence] = &[
    // Where the window's events and the worker's replies enter the host.
    Fence {
        name: "interact_system_state",
        home: HOST,
        gate: None,
        callers: &[
            (KEY_EVENTS, "on_key_native_mode"),
            (KEY_EVENTS, "fence_menu_action"),
            (NATIVE_EVENTS, "native_input_event"),
        ],
    },
    Fence {
        name: "interact_system_state_view",
        home: HOST,
        gate: None,
        callers: &[
            (HOST, "interact_system_state"),
            (NATIVE_EVENTS, "dispatch_native_target_event"),
            (MOUSE_EVENTS, "mouse_input_then_fold"),
            (MOUSE_EVENTS, "on_mouse_wheel_inner"),
        ],
    },
    Fence {
        name: "receive_system_state_status",
        home: HOST,
        gate: None,
        callers: &[(GUI_LIB, "on_user_event")],
    },
    Fence {
        name: "resolve_system_state_apply",
        home: HOST,
        gate: None,
        callers: &[
            (GUI_LIB, "on_user_event"),
            (HOST, "receive_system_state_status"),
            (HOST, "tick_system_state"),
        ],
    },
    Fence {
        name: "drain_system_state_actions",
        home: HOST,
        gate: None,
        callers: &[
            (NATIVE_EVENTS, "dispatch_native_target_event"),
            (HOST, "tick_system_state"),
        ],
    },
    Fence {
        name: "run_system_state_action",
        home: HOST,
        gate: None,
        callers: &[(HOST, "drain_system_state_actions")],
    },
    Fence {
        name: "run_system_state_action_with_handlers",
        home: HOST,
        gate: None,
        callers: &[(HOST, "run_system_state_action")],
    },
    // A person's press on a Permissions row's button.
    Fence {
        name: "open_access_from_system_state",
        home: ACCESS,
        gate: None,
        callers: &[(HOST, "run_system_state_action_with_handlers")],
    },
    // The App's way into the gate.
    Fence {
        name: "note_system_state_touch",
        home: SETUP,
        gate: None,
        callers: &[(HOST, "interact_system_state_view")],
    },
    Fence {
        name: "take_system_state_press",
        home: SETUP,
        gate: None,
        callers: &[
            (SETUP, "note_system_state_apply"),
            (HOST, "run_system_state_action_with_handlers"),
        ],
    },
    Fence {
        name: "note_system_state_apply",
        home: SETUP,
        gate: None,
        callers: &[(HOST, "run_system_state_action_with_handlers")],
    },
    Fence {
        name: "accepted_initial_setup_apply",
        home: SETUP,
        gate: None,
        callers: &[(HOST, "resolve_system_state_apply")],
    },
    Fence {
        name: "observe_initial_setup_permissions",
        home: SETUP,
        gate: None,
        callers: &[(HOST, "receive_system_state_status")],
    },
    // The gate's own transitions, each behind its one App function.
    Fence {
        name: "note_touch",
        home: ADMISSION_LEAF,
        gate: Some(SETUP),
        callers: &[(SETUP, "note_system_state_touch")],
    },
    Fence {
        name: "take_press",
        home: ADMISSION_LEAF,
        gate: Some(SETUP),
        callers: &[(SETUP, "take_system_state_press")],
    },
    Fence {
        name: "apply_started",
        home: ADMISSION_LEAF,
        gate: Some(SETUP),
        callers: &[(SETUP, "note_system_state_apply")],
    },
    Fence {
        name: "accepted",
        home: ADMISSION_LEAF,
        gate: Some(SETUP),
        callers: &[(SETUP, "accepted_initial_setup_apply")],
    },
    Fence {
        name: "observe",
        home: ADMISSION_LEAF,
        gate: Some(SETUP),
        callers: &[(SETUP, "observe_initial_setup_permissions")],
    },
];

/// The gate's inputs, fenced ([`Fence`]). Findings: in aterm-gui's shipped code (test code is a
/// `#[cfg(test)]` item, or a file its parent declares test-only or its path names for tests that
/// no shipped line loads: [`TestCode`](super::rules::TestCode)), a
/// fenced function named outside its callers' bodies; a caller defined twice or outside
/// `impl App` ([`caller_findings`]); in the gate's parent, a module beside the gate's leaf or an
/// `include!`; in any product macro's body, a gate transition ([`gate_macro_findings`]); and a
/// fence whose function its home does not define.
fn fence_findings(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    // Over a tree that holds every home (a planted one holds a few twins).
    let whole = FENCES.iter().all(|f| tree.corpus.get(f.home).is_some());
    for fence in FENCES.iter().filter(|_| whole) {
        if let Some(home) = tree.corpus.get(fence.home)
            && !home
                .shipped_lines()
                .any(|l| has(l.code, &format!("fn {}", fence.name), Bound::Word))
        {
            out.push(home.finding(
                DEVTOOLS_DOOR.id,
                1,
                format!(
                    "defines no `fn {}`, which a fence names: renamed? fence its new name",
                    fence.name
                ),
            ));
        }
    }
    caller_findings(tree, out);
    // The gate's parent declares no shipped module but the gate's leaf, and
    // brings in no source: a child of it, or a file it includes, could call
    // the gate's `pub(super)` transitions, on the real gate or a fresh one,
    // where the fences do not read.
    let gate_parents: BTreeSet<&str> = FENCES.iter().filter_map(|f| f.gate).collect();
    for parent in gate_parents.iter().filter_map(|rel| tree.corpus.get(rel)) {
        let code = parent.shipped_code();
        let mut flags = Flags::new(&code);
        for at in hits(&code, "mod", Bound::Word).filter(|&at| ident_follows(&code, at + 3)) {
            let name = code[at + 3..]
                .trim_start()
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                .unwrap_or("");
            if name != GATE_MODULE {
                flags.at(
                    at,
                    format!(
                        "declares `mod {name}` beside the gate's leaf: a child here could call \
                         the gate's transitions outside the fences"
                    ),
                );
            }
        }
        for (at, path) in macro_calls(&code) {
            let name = path.rsplit("::").next().unwrap_or(path);
            if SOURCE_MACROS.contains(&name) {
                flags.at(
                    at,
                    format!(
                        "invokes `{path}!` in the gate's parent: the source it brings in could \
                         call the gate's transitions where no line here spells them"
                    ),
                );
            }
        }
        flags.push(parent, out);
    }
    gate_macro_findings(tree, out);
    let test_code = tree.corpus.test_code();
    for file in tree.corpus.under(&[GUI_SRC]) {
        // A line names a fence only where its file may (a gate's transition in its one file,
        // an App function anywhere) and its code spells it: the other fences are left unread,
        // and so is a file that spells none it may name.
        let may: Vec<&Fence> = FENCES
            .iter()
            .filter(|f| f.gate.is_none_or(|only| only == file.rel))
            .collect();
        let needles: Vec<&str> = may.iter().map(|f| f.name).collect();
        if !file.mentions(&needles) || test_code.holds(&file.rel) {
            continue;
        }
        let code = &file.lexed().code;
        let named: Vec<&Fence> = may.into_iter().filter(|f| code.contains(f.name)).collect();
        if named.is_empty() {
            continue;
        }
        let shipped: BTreeSet<usize> = file.shipped_lines().map(|l| l.no).collect();
        for at in attribute(file) {
            let line = at.line;
            if at.in_test || !shipped.contains(&line.no) {
                continue;
            }
            let func = at.func.unwrap_or("-");
            let crossed = named.iter().find(|fence| {
                // A gate transition is named in its one file as any word: a
                // call, a path taken as a value (`let f = PermissionGate::accepted;`)
                // or a macro's argument.
                let named = fence.gate.is_none_or(|only| only == file.rel)
                    && hits(line.code, fence.name, Bound::Word)
                        .any(|at| word_before(line.code, at) != "fn");
                named
                    && !fence
                        .callers
                        .iter()
                        .any(|&(rel, by)| rel == file.rel && by == func)
            });
            if let Some(fence) = crossed {
                let callers: Vec<String> = fence
                    .callers
                    .iter()
                    .map(|(rel, by)| format!("{}::{by}", file_name(rel)))
                    .collect();
                out.push(file.finding(
                    DEVTOOLS_DOOR.id,
                    line.no,
                    format!(
                        "names `{}` in `{func}`: the gate's input, taken only in `{}`",
                        fence.name,
                        callers.join("`, `")
                    ),
                ));
            }
        }
    }
}

/// Each fence's caller is one inherent method of [`CALLER_HOST`], defined once in its file's
/// shipped code, at an `impl App`'s top. A fenced name is let through by the name of the fn its
/// line sits in, so a second `fn` of a caller's name there (a free fn, a nested one, another
/// impl's or a trait's) would carry the caller's leave to whatever calls it. A caller gone from
/// its file lets nothing through.
fn caller_findings(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut by_file: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for &(rel, by) in FENCES.iter().flat_map(|f| f.callers) {
        by_file.entry(rel).or_default().insert(by);
    }
    for (rel, names) in by_file {
        let Some(file) = tree.corpus.get(rel) else {
            continue;
        };
        let code = file.shipped_code();
        let depth = depths(&code);
        let blocks = impl_blocks(&code);
        let mut flags = Flags::new(&code);
        for name in names {
            let mut defined = false;
            for at in fn_definitions(&code, name) {
                let method = depth[at] == 1
                    && blocks.iter().any(|b| {
                        depth[b.at] == 0
                            && impl_self(&b.head) == Some((CALLER_HOST, false))
                            && b.open < at
                            && at < b.close
                    });
                if method && !defined {
                    defined = true;
                    continue;
                }
                let how = if method {
                    "a second time".to_string()
                } else {
                    format!("outside an inherent `impl {CALLER_HOST}`")
                };
                flags.at(
                    at,
                    format!(
                        "defines `fn {name}` {how}: a fence's caller is one method of \
                         `{CALLER_HOST}`, defined once, or a namesake any line can call takes the \
                         fenced input under the caller's name"
                    ),
                );
            }
        }
        flags.push(file, out);
    }
}

/// Each `fn <name>` of `code` (any whitespace between, a newline too).
fn fn_definitions<'a>(code: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    hits(code, "fn", Bound::Word).filter(move |&at| {
        let rest = &code[at + 2..];
        let ident = rest.trim_start();
        ident.len() < rest.len()
            && ident
                .strip_prefix(name)
                .is_some_and(|tail| tail.bytes().next().is_none_or(|b| !is_word(b)))
    })
}

/// A product macro whose body (`macro_rules!` or a `macro` item) names a gate transition as a
/// method or a path (`$g.accepted()`, `PermissionGate::observe`): invoked in the gate's parent
/// it expands there, with that file's privacy, into a call no line of the file spells.
fn gate_macro_findings(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let transitions: Vec<&str> = FENCES
        .iter()
        .filter(|f| f.gate.is_some())
        .map(|f| f.name)
        .collect();
    for file in product(tree) {
        if !file.mentions(&["macro"]) || !file.mentions(&transitions) {
            continue;
        }
        // A macro and a transition its body names are spelt in the code itself.
        let lexed = &file.lexed().code;
        if !lexed.contains("macro") || !transitions.iter().any(|t| lexed.contains(t)) {
            continue;
        }
        let code = file.shipped_code();
        let mut flags = Flags::new(&code);
        for (from, to) in macro_bodies(&code) {
            let body = &code[from..=to];
            for name in &transitions {
                let taken = hits(body, name, Bound::Word).find(|&at| {
                    let before = body[..at].trim_end();
                    before.ends_with('.') || before.ends_with("::")
                });
                if let Some(at) = taken {
                    flags.at(
                        from + at,
                        format!(
                            "a macro's body takes the gate's `{name}`: expanded in the gate's \
                             parent, it calls the transition where no line there spells it"
                        ),
                    );
                }
            }
        }
        flags.push(file, out);
    }
}

/// The span of each macro definition of `code`: from `macro_rules!` (or a
/// `macro` item's keyword) through the bracket that closes its rules (a
/// `macro`'s parameters, then its body).
fn macro_bodies(code: &str) -> Vec<(usize, usize)> {
    let rules = hits(code, "macro_rules", Bound::Word)
        .filter(|&at| code[at + 11..].trim_start().starts_with('!'));
    let items = hits(code, "macro", Bound::Word).filter(|&at| ident_follows(code, at + 5));
    let mut out = Vec::new();
    for at in rules.chain(items) {
        let Some(open) = code[at..].find(['{', '(', '[']).map(|b| at + b) else {
            continue;
        };
        let Some(mut close) = closing(code, open) else {
            continue;
        };
        if code.as_bytes()[open] == b'(' {
            let rest = &code[close + 1..];
            let body = rest.trim_start();
            if body.starts_with('{')
                && let Some(end) = closing(code, code.len() - body.len())
            {
                close = end;
            }
        }
        out.push((at, close));
    }
    out
}

/// One finding per line, the first reason seen there, over a file's bytes.
struct Flags {
    /// The byte offset each line starts at.
    starts: Vec<usize>,
    found: BTreeMap<usize, String>,
}

impl Flags {
    fn new(code: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(code.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        Self {
            starts,
            found: BTreeMap::new(),
        }
    }

    /// The 1-based line of byte `at`.
    fn line(&self, at: usize) -> usize {
        self.starts.partition_point(|&start| start <= at)
    }

    fn at(&mut self, at: usize, why: String) {
        let line = self.line(at);
        self.found.entry(line).or_insert(why);
    }

    fn push(self, file: &SourceFile, out: &mut Vec<Finding>) {
        out.extend(
            self.found
                .into_iter()
                .map(|(line, why)| file.finding(DEVTOOLS_DOOR.id, line, why)),
        );
    }
}

/// Every line of a token's leaf (a `#[cfg(test)]` item's too): nothing in it reaches the
/// private field from beside the leaf or expands into code the lines do not spell, the token
/// and its mark keep their shape, and the token is built only inside its doors' bodies.
fn leaf_findings(file: &SourceFile, token: &Token, out: &mut Vec<Finding>) {
    let code = file.lexed().code.as_str();
    let ty = token.ty;
    let mark = token.mark;
    let depth = depths(code);
    let mut flags = Flags::new(code);
    // Nothing that reaches the field from beside the leaf, or expands here.
    for at in hits(code, "mod", Bound::Word).filter(|&at| ident_follows(code, at + 3)) {
        flags.at(
            at,
            format!("declares a module in `{ty}`'s leaf: a child would reach its private field"),
        );
    }
    for at in hits(code, "macro_rules", Bound::Word)
        .chain(hits(code, "macro", Bound::Word).filter(|&at| ident_follows(code, at + 5)))
    {
        flags.at(
            at,
            format!("defines a macro in `{ty}`'s leaf: what it expands to here no line spells"),
        );
    }
    for (at, path) in macro_calls(code) {
        let name = path.rsplit("::").next().unwrap_or(path);
        if name == "macro_rules" {
            continue;
        }
        if !LEAF_MACRO_ROOTS.iter().any(|root| path.starts_with(root))
            || SOURCE_MACROS.contains(&name)
        {
            flags.at(
                at,
                format!(
                    "invokes `{path}!` in `{ty}`'s leaf: only a macro named from the extern \
                     prelude (`::std::..!`) cannot be one this crate defines, and none may bring \
                     in source"
                ),
            );
        }
    }
    for (at, attr) in attributes(code) {
        if let Some(why) = leaf_attribute(&attr) {
            flags.at(at, why);
        }
    }
    for at in hits(code, "type", Bound::Word).filter(|&at| ident_follows(code, at + 4)) {
        flags.at(
            at,
            format!("declares a type alias in `{ty}`'s leaf: a literal under another name"),
        );
    }
    for (at, stmt) in use_statements(code) {
        for leaf in use_leaves(stmt) {
            let bound = leaf.alias.or(leaf.path.last().copied()).unwrap_or("");
            let why = if leaf.glob {
                "imports a glob, whose names would shadow the prelude's here"
            } else if leaf.alias.is_some() {
                "renames an import"
            } else if LEAF_SHADOWS.contains(&bound) || STD_DERIVES.contains(&bound) {
                "imports a name over the prelude's derive or a macro path's crate"
            } else {
                continue;
            };
            flags.at(at, format!("{why} in `{ty}`'s leaf"));
        }
    }
    for at in hits(code, "extern", Bound::Word)
        .filter(|&at| code[at + 6..].trim_start().starts_with("crate"))
    {
        flags.at(at, format!("declares an extern crate in `{ty}`'s leaf"));
    }
    for at in hits(code, "unsafe", Bound::Word) {
        flags.at(at, format!("`unsafe` in `{ty}`'s leaf"));
    }
    for line in file.shipped_lines() {
        if let Some(col) = function_type(line.code) {
            flags.at(
                flags.starts[line.no - 1] + col,
                format!(
                    "a function type on a shipped line of `{ty}`'s leaf: an arm its caller \
                     supplies, which could answer for the owner (a stand-in is `#[cfg(test)]`)"
                ),
            );
        }
    }
    // The token and its mark.
    let body = token_struct(code, ty, mark, &mut flags);
    mark_struct(code, ty, mark, &mut flags);
    let blocks = impl_blocks(code);
    for block in &blocks {
        match impl_self(&block.head) {
            Some((own, true)) if own == ty => flags.at(
                block.at,
                format!(
                    "implements a trait for `{ty}` in its leaf: a mint or a copy no door spells"
                ),
            ),
            Some((own, _)) if own == mark => flags.at(
                block.at,
                format!("implements for `{mark}`, `{ty}`'s mark: a value no door spells"),
            ),
            _ => {}
        }
    }
    // The arm the alert's door asks through.
    for door in token.doors {
        if let DoorShape::Asks { arm, .. } = door.shape {
            arm_findings(file, code, arm, &mut flags);
        }
    }
    // The doors, and every mint outside them.
    let doors = door_spans(code, &depth, &blocks, token, &mut flags);
    let mut mints: Vec<usize> = hits(code, mark, Bound::Word)
        .filter(|&at| word_before(code, at) != "struct")
        .filter(|&at| !body.is_some_and(|(open, close)| open < at && at < close))
        .collect();
    mints.extend(build_hits(code, ty));
    for block in blocks
        .iter()
        .filter(|b| impl_self(&b.head).is_some_and(|(own, _)| own == ty))
    {
        let inside = &code[block.open..=block.close];
        mints.extend(build_hits(inside, "Self").map(|at| block.open + at));
    }
    let funcs: Vec<Option<&str>> = attribute(file).iter().map(|l| l.func).collect();
    let names: Vec<&str> = token.doors.iter().map(|d| d.name).collect();
    for at in mints {
        if doors.iter().any(|&(from, to)| from <= at && at <= to) {
            continue;
        }
        let func = funcs
            .get(flags.line(at) - 1)
            .copied()
            .flatten()
            .unwrap_or("-");
        flags.at(
            at,
            format!(
                "builds a `{ty}` in `{func}`, outside its doors (`{}`)",
                names.join("`, `")
            ),
        );
    }
    flags.push(file, out);
}

/// The variants the alert's arm may hold on a shipped line, each a unit:
/// aterm's alert, and nobody.
const ARM_VARIANTS: &[&str] = &["Alert", "Nobody"];
/// The arm's one other variant, a test's stand-in for the owner, which only a
/// test build holds.
const ARM_STAND_IN: &str = "StandIn";

/// The alert's arm in the yes's leaf ([`DoorShape::Asks`]) is a plain `enum` whose shipped
/// variants are [`ARM_VARIANTS`], each a unit, and whose other variant is [`ARM_STAND_IN`]
/// under a test `cfg`. A variant that carries a value on a shipped line (`Fixed(Option<bool>)`)
/// is an answer whoever builds the arm picks, and holds no function type for the leaf's other
/// rule to see.
fn arm_findings(file: &SourceFile, code: &str, arm: &str, flags: &mut Flags) {
    let mut enums = Vec::new();
    for at in hits(code, arm, Bound::Word) {
        match word_before(code, at) {
            "enum" => enums.push(at),
            kind @ ("struct" | "union" | "type" | "trait") => flags.at(
                at,
                format!("defines a {kind} `{arm}`: the arm is the leaf's one plain enum"),
            ),
            _ => {}
        }
    }
    if enums.is_empty() {
        flags.at(
            0,
            format!("defines no `enum {arm}`: the door asks `{arm}::ask` of whatever that names"),
        );
    }
    let shipped: BTreeSet<usize> = file.shipped_lines().map(|l| l.no).collect();
    for at in enums {
        let after = &code[at + arm.len()..];
        let open = code.len() - after.trim_start().len();
        if code.as_bytes().get(open) != Some(&b'{') {
            flags.at(
                at,
                format!("`{arm}` is no plain enum: a parameter or a bound could carry an answer"),
            );
            continue;
        }
        if let Some(close) = closing(code, open) {
            arm_variants(code, (open, close), arm, &shipped, flags);
        } else {
            flags.at(at, format!("unclosed `enum {arm}`"));
        }
    }
}

/// Each variant between the arm's braces (`open`, `close`) that
/// [`arm_findings`] does not allow, by whether its line ships (`shipped`).
fn arm_variants(
    code: &str,
    (open, close): (usize, usize),
    arm: &str,
    shipped: &BTreeSet<usize>,
    flags: &mut Flags,
) {
    let mut from = open + 1;
    for chunk in top_level_split(&code[open + 1..close], ',') {
        let start = from;
        from += chunk.len() + 1;
        // Past the variant's attributes to its name.
        let mut rest = chunk.trim_start();
        let mut malformed = false;
        while rest.starts_with("#[") {
            let base = start + (chunk.len() - rest.len());
            let Some(end) = closing(code, base + 1).filter(|&end| end < start + chunk.len()) else {
                flags.at(base, format!("unclosed or split attribute in `{arm}`"));
                malformed = true;
                break;
            };
            rest = code[end + 1..start + chunk.len()].trim_start();
        }
        if malformed {
            continue;
        }
        let name_len = rest.bytes().take_while(|&b| is_word(b)).count();
        if name_len == 0 {
            continue;
        }
        let name_at = start + chunk.len() - rest.len();
        let name = &rest[..name_len];
        let unit =
            rest[name_len..].trim().is_empty() || rest[name_len..].trim_start().starts_with('=');
        let on_shipped = shipped.contains(&flags.line(name_at));
        let why = match (on_shipped, name) {
            (true, n) if ARM_VARIANTS.contains(&n) && unit => continue,
            (false, ARM_STAND_IN) => continue,
            (true, n) if ARM_VARIANTS.contains(&n) => "carries a value",
            (true, ARM_STAND_IN) => "is on a shipped line",
            (false, _) => "is another variant, under a test `cfg`",
            (true, _) => "is another variant, on a shipped line",
        };
        flags.at(
            name_at,
            format!(
                "`{arm}::{name}` {why}: the arm is aterm's alert or nobody, each a unit, and a \
                 test's `{ARM_STAND_IN}` only in a test build"
            ),
        );
    }
}

/// Why a leaf's attribute (the text between its brackets) could expand into
/// code there, if it could: a derive of anything but std's by its bare name,
/// or any attribute outside [`LEAF_ATTRIBUTES`].
fn leaf_attribute(attr: &str) -> Option<String> {
    let attr = attr.trim();
    let name_end = attr
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(attr.len());
    let name = &attr[..name_end];
    if name == "derive" {
        return derive_names(attr)
            .into_iter()
            .find(|d| !STD_DERIVES.contains(d))
            .map(|d| format!("derives `{d}` in a leaf: only std's derives, by their bare names"));
    }
    (!LEAF_ATTRIBUTES.contains(&name))
        .then(|| format!("carries `#[{name}]` in a leaf: an attribute that may expand into code"))
}

/// The names in every `derive(..)` list of `text`.
fn derive_names(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for at in hits(text, "derive", Bound::Word) {
        let rest = text[at + "derive".len()..].trim_start();
        let Some(list) = rest.strip_prefix('(') else {
            continue;
        };
        let list = list.split(')').next().unwrap_or(list);
        out.extend(list.split(',').map(str::trim).filter(|d| !d.is_empty()));
    }
    out
}

/// The token's definition in its leaf: a braced struct, every field private,
/// one of them its mark, deriving only [`TOKEN_DERIVES`]. Its body's
/// braces, when it is braced.
fn token_struct(code: &str, ty: &str, mark: &str, flags: &mut Flags) -> Option<(usize, usize)> {
    let Some(at) = hits(code, ty, Bound::Word).find(|&at| word_before(code, at) == "struct") else {
        flags.at(0, format!("defines no `{ty}`: the token lives in its leaf"));
        return None;
    };
    for d in derive_names(item_head(code, at)) {
        if !TOKEN_DERIVES.contains(&d) {
            flags.at(at, format!("`{ty}` derives `{d}`: one token is spent once"));
        }
    }
    let open =
        at + ty.len() + (code[at + ty.len()..].len() - code[at + ty.len()..].trim_start().len());
    if code.as_bytes().get(open) != Some(&b'{') {
        flags.at(
            at,
            format!(
                "`{ty}` is not a braced struct: a tuple's constructor is a function, a unit's a \
                 value, either one passed where no line spells a build"
            ),
        );
        return None;
    }
    let close = closing(code, open)?;
    let fields = &code[open + 1..close];
    if has(fields, "pub", Bound::Word) {
        flags.at(
            at,
            format!("a field of `{ty}` is not private: any line could build one"),
        );
    }
    if !has(fields, mark, Bound::Word) {
        flags.at(
            at,
            format!("`{ty}` holds no `{mark}` field: nothing keeps `impl Copy` from compiling"),
        );
    }
    Some((open, close))
}

/// The token's mark in its leaf: defined, private, deriving only
/// [`TOKEN_DERIVES`].
fn mark_struct(code: &str, ty: &str, mark: &str, flags: &mut Flags) {
    let Some(at) = hits(code, mark, Bound::Word).find(|&at| word_before(code, at) == "struct")
    else {
        flags.at(0, format!("defines no `{mark}`, `{ty}`'s private mark"));
        return;
    };
    let head = item_head(code, at);
    if has(head, "pub", Bound::Word) {
        flags.at(
            at,
            format!("`{mark}`, `{ty}`'s mark, is not private: any line could build one"),
        );
    }
    for d in derive_names(head) {
        if !TOKEN_DERIVES.contains(&d) {
            flags.at(at, format!("`{mark}`, `{ty}`'s mark, derives `{d}`"));
        }
    }
}

/// The text before the keyword at `at` back to the end of the item before
/// it: the item's attributes and visibility.
fn item_head(code: &str, at: usize) -> &str {
    let start = code[..at].rfind(['}', ';', '{']).map_or(0, |b| b + 1);
    &code[start..at]
}

/// The byte spans of `token`'s doors in its leaf, from each door's `fn` to the
/// brace that closes its body ([`YesDoor`]). A door whose shape fails is
/// flagged and is no door; so is a fn nested in a door's body.
fn door_spans(
    code: &str,
    depth: &[i64],
    blocks: &[ImplBlock],
    token: &Token,
    flags: &mut Flags,
) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for door in token.doors {
        let within = |at: usize| match door.within {
            None => depth[at] == 0,
            Some(host) => {
                depth[at] == 1
                    && blocks.iter().any(|b| {
                        depth[b.at] == 0
                            && impl_self(&b.head) == Some((host, false))
                            && b.open < at
                            && at < b.close
                    })
            }
        };
        let Some(at) = hits(code, "fn", Bound::Word)
            .find(|&at| fn_name_at(&code[at..]) == Some(door.name) && within(at))
        else {
            continue;
        };
        let Some(open) = body_open(code, at) else {
            continue;
        };
        let Some(close) = closing(code, open) else {
            continue;
        };
        let (signature, body) = (&code[at..open], &code[open..=close]);
        if let Err(why) = door_shape(door, item_head(code, at), signature, body) {
            flags.at(at, format!("`{}` is no door: {why}", door.name));
            continue;
        }
        for nested in hits(body, "fn", Bound::Word).filter(|&n| fn_name_at(&body[n..]).is_some()) {
            flags.at(
                open + nested,
                format!(
                    "a fn inside the door `{}`: a mint there is not the door's",
                    door.name
                ),
            );
        }
        spans.push((at, close));
    }
    spans
}

/// Whether a door's attributes (`head`), signature and body hold its
/// [`DoorShape`], or why not.
fn door_shape(door: &YesDoor, head: &str, signature: &str, body: &str) -> Result<(), String> {
    let params: Vec<&str> = signature
        .find('(')
        .and_then(|open| closing(signature, open).map(|close| &signature[open + 1..close]))
        .map(|list| top_level_split(list, ','))
        .unwrap_or_default()
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    match door.shape {
        DoorShape::TestOnly => folded(head)
            .contains("#[cfg(test)]")
            .then_some(())
            .ok_or_else(|| "it carries no `#[cfg(test)]`".to_string()),
        DoorShape::Asks { gestures, arm } => {
            let asked = params.iter().find_map(|p| {
                let (name, t) = p.split_once(':')?;
                let t = t.trim().strip_prefix('&')?.trim_start();
                plain_path_to(t, gestures).then(|| name.trim())
            });
            asked
                .filter(|name| folded(body).contains(&format!("{arm}::ask({name}.confirm,")))
                .map(|_| ())
                .ok_or_else(|| {
                    format!(
                        "it asks no `&{gestures}` parameter's `confirm` arm through \
                         `{arm}::ask`, so it may ask nobody, or anything"
                    )
                })
        }
    }
}

/// Is `t` a plain path (no reference, generics, tuple or slice) whose last
/// segment is `ty`?
fn plain_path_to(t: &str, ty: &str) -> bool {
    t.bytes().all(|b| is_word(b) || b == b':') && t.rsplit("::").next() == Some(ty)
}

/// `s` with every run of whitespace removed.
fn folded(s: &str) -> String {
    s.split_whitespace().collect()
}

/// `s` with every run of whitespace made one space.
fn one_spaced(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, w) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// `s` split at `sep` outside any bracket.
fn top_level_split(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut delim, mut angle, mut start) = (0i64, 0i64, 0usize);
    let b = s.as_bytes();
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => delim += 1,
            ')' | ']' | '}' => delim = (delim - 1).max(0),
            '<' if delim == 0 => angle += 1,
            '>' if delim == 0 && (i == 0 || b[i - 1] != b'-') => angle = (angle - 1).max(0),
            c if c == sep && delim == 0 && angle == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// The brace depth before each byte of `code`, and after its last.
fn depths(code: &str) -> Vec<i64> {
    let mut out = Vec::with_capacity(code.len() + 1);
    let mut depth = 0i64;
    for b in code.bytes() {
        out.push(depth);
        match b {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
    }
    out.push(depth);
    out
}

/// The offset of the bracket that closes the one at `open` (`(`, `[`, `{` or
/// `<`, where a `->`'s `>` closes nothing), if it closes.
fn closing(code: &str, open: usize) -> Option<usize> {
    let b = code.as_bytes();
    let (o, c) = match b.get(open)? {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
        b'<' => (b'<', b'>'),
        _ => return None,
    };
    let mut depth = 0usize;
    for (i, &x) in b.iter().enumerate().skip(open) {
        if x == o {
            depth += 1;
        } else if x == c && !(c == b'>' && b[i - 1] == b'-') {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The `{` that opens the body of the fn whose keyword is at `at`, past its
/// signature's brackets; `None` for a declaration (`;` first).
fn body_open(code: &str, at: usize) -> Option<usize> {
    let mut nest = 0i64;
    for (i, b) in code.bytes().enumerate().skip(at) {
        match b {
            b'(' | b'[' => nest += 1,
            b')' | b']' => nest -= 1,
            b'{' if nest == 0 => return Some(i),
            b';' if nest == 0 => return None,
            _ => {}
        }
    }
    None
}

/// The word before byte `at`, past whitespace.
fn word_before(code: &str, at: usize) -> &str {
    let before = code[..at].trim_end();
    let start = before
        .bytes()
        .rposition(|b| !is_word(b))
        .map_or(0, |i| i + 1);
    &before[start..]
}

/// Does an identifier follow byte `at`, past at least one space?
fn ident_follows(code: &str, at: usize) -> bool {
    let rest = &code[at..];
    let trimmed = rest.trim_start();
    trimmed.len() < rest.len()
        && trimmed
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
}

/// `s` without a trailing word `w`, if it ends with that word.
fn strip_word_suffix<'a>(s: &'a str, w: &str) -> Option<&'a str> {
    let rest = s.strip_suffix(w)?;
    rest.bytes()
        .next_back()
        .is_none_or(|b| !is_word(b))
        .then_some(rest)
}

/// Does the keyword at `at` open an item: nothing before it but qualifiers,
/// a visibility and the end of an attribute, another item or a block's start?
fn at_item(code: &str, at: usize) -> bool {
    let mut before = code[..at].trim_end();
    for qualifier in ["unsafe", "default"] {
        if let Some(rest) = strip_word_suffix(before, qualifier) {
            before = rest.trim_end();
        }
    }
    if let Some(rest) = strip_word_suffix(before, "pub") {
        before = rest.trim_end();
    } else if before.ends_with(')')
        && let Some(open) = before.rfind('(')
        && let Some(rest) = strip_word_suffix(before[..open].trim_end(), "pub")
    {
        before = rest.trim_end();
    }
    before.is_empty() || before.ends_with([';', '}', '{', ']'])
}

/// An `impl` item: where its keyword is, its head (the keyword to its `{`,
/// whitespace made single spaces) and its body's braces.
struct ImplBlock {
    at: usize,
    head: String,
    open: usize,
    close: usize,
}

/// Every `impl` item of `code`, at any depth (an `impl` in a fn body applies
/// crate-wide).
fn impl_blocks(code: &str) -> Vec<ImplBlock> {
    hits(code, "impl", Bound::Word)
        .filter(|&at| at_item(code, at))
        .filter_map(|at| {
            let open = at + code[at..].find(['{', ';'])?;
            if code.as_bytes()[open] != b'{' {
                return None;
            }
            Some(ImplBlock {
                at,
                head: one_spaced(&code[at..open]),
                open,
                close: closing(code, open)?,
            })
        })
        .collect()
}

/// An impl head's self type, by its path's last segment, and whether the impl
/// is of a trait.
fn impl_self(head: &str) -> Option<(&str, bool)> {
    let rest = head.trim_start();
    let rest = rest.strip_prefix("unsafe").map_or(rest, str::trim_start);
    let rest = rest.strip_prefix("impl")?;
    if rest.bytes().next().is_some_and(is_word) {
        return None;
    }
    let (self_ty, of_trait) = match rest.rsplit_once(" for ") {
        Some((_, self_ty)) => (self_ty, true),
        None => (skip_generics(rest.trim_start()), false),
    };
    let self_ty = self_ty.trim_start();
    let end = self_ty
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(self_ty.len());
    self_ty[..end]
        .rsplit("::")
        .next()
        .map(|own| (own, of_trait))
}

/// Each `use` statement of `code`: its keyword's offset and its text through
/// its `;`.
fn use_statements(code: &str) -> Vec<(usize, &str)> {
    hits(code, "use", Bound::Word)
        .filter(|&at| at_item(code, at))
        .filter_map(|at| {
            let end = at + code[at..].find(';')?;
            Some((at, &code[at..=end]))
        })
        .collect()
}

/// Each macro invocation of `code`: where its path starts, and the path
/// (`::std::matches`, `todo`).
fn macro_calls(code: &str) -> Vec<(usize, &str)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for (bang, _) in code.match_indices('!') {
        if bang == 0 || !is_word(b[bang - 1]) {
            continue;
        }
        if !code[bang + 1..].trim_start().starts_with(['(', '[', '{']) {
            continue;
        }
        let mut start = bang;
        loop {
            while start > 0 && is_word(b[start - 1]) {
                start -= 1;
            }
            if start >= 2 && &code[start - 2..start] == "::" {
                start -= 2;
            } else {
                break;
            }
        }
        out.push((start, &code[start..bang]));
    }
    out
}

/// Each attribute of `code` (`#[..]`, `#![..]`): its offset and the text
/// between its brackets.
fn attributes(code: &str) -> Vec<(usize, String)> {
    let b = code.as_bytes();
    code.match_indices('#')
        .filter_map(|(at, _)| {
            let open = at + 1 + usize::from(b.get(at + 1) == Some(&b'!'));
            (b.get(open) == Some(&b'[')).then_some(())?;
            let close = closing(code, open)?;
            Some((at, one_spaced(&code[open + 1..close])))
        })
        .collect()
}

/// Every other product file (a test's too), its shipped lines, for what no privacy stops: a
/// line naming a token's mark (only a macro's body could, and a macro the leaf invoked would
/// expand it there with the leaf's privacy); a type taking a token's name, or an import binding
/// one to it; an `impl` of [`TOKEN_COPIES`] for a token; an alias renaming one (a `type` naming
/// it in its generics or at the top of its right side, a `use` renaming it), since the unsafe
/// reads are found by the name; an [`UNSAFE_READS`] read in a `fn`, `const` or `static` that
/// names a token; a line other than its parent's one `#[path]` naming a leaf's file; and an
/// attribute on that mount that could put another file in its place ([`mount_attributes`]).
fn outside_findings(file: &SourceFile, out: &mut Vec<Finding>) {
    let code = file.shipped_code();
    let mut flags = Flags::new(&code);
    for token in &TOKENS {
        let (ty, mark) = (token.ty, token.mark);
        for at in hits(&code, mark, Bound::Word) {
            flags.at(
                at,
                format!(
                    "names `{mark}`, `{ty}`'s private mark, outside its leaf {}: a macro's body \
                     that names it builds a `{ty}` wherever the leaf expands it",
                    token.leaf
                ),
            );
        }
        for at in hits(&code, ty, Bound::Word) {
            let kind = word_before(&code, at);
            if ["struct", "enum", "union", "trait", "type"].contains(&kind) {
                flags.at(
                    at,
                    format!(
                        "defines a {kind} `{ty}` outside its leaf {}: its doors' callers would \
                         take it",
                        token.leaf
                    ),
                );
            }
        }
    }
    for block in impl_blocks(&code) {
        for ty in TOKENS.iter().map(|t| t.ty) {
            if let Some(copy) = copy_trait(&block.head, ty) {
                flags.at(
                    block.at,
                    format!("implements `{copy}` for `{ty}`: one yes is spent once"),
                );
            }
        }
    }
    for at in hits(&code, "type", Bound::Word).filter(|&at| ident_follows(&code, at + 4)) {
        if let Some(ty) = aliased_token(&code[at..]) {
            flags.at(
                at,
                format!("aliases `{ty}`: an unsafe read under another name names no token"),
            );
        }
    }
    for (at, stmt) in use_statements(&code) {
        for leaf in use_leaves(stmt) {
            let last = leaf.path.last().copied().unwrap_or("");
            match leaf.alias {
                Some(alias)
                    if alias != "_" && alias != last && TOKENS.iter().any(|t| t.ty == last) =>
                {
                    flags.at(
                        at,
                        format!(
                            "renames `{last}`: an unsafe read under another name names no token"
                        ),
                    );
                }
                Some(alias) if alias != last && TOKENS.iter().any(|t| t.ty == alias) => {
                    flags.at(
                        at,
                        format!("binds `{last}` under `{alias}`'s name outside its leaf"),
                    );
                }
                _ => {}
            }
        }
    }
    unsafe_reads(&code, &mut flags);
    for token in &TOKENS {
        let leaf = file_name(token.leaf);
        let mut mounted = false;
        for line in file.shipped_lines() {
            if !line.text.contains(leaf) {
                continue;
            }
            let mount = file.rel == token.parent && !mounted && line.code.contains("#[path");
            mounted |= mount;
            if !mount {
                flags.at(
                    flags.starts[line.no - 1],
                    format!("names `{leaf}` outside its one mount: a second copy of the leaf"),
                );
            } else if let Some(at) = hits(&code, "mod", Bound::Word)
                .find(|&at| at >= flags.starts[line.no - 1] && ident_follows(&code, at + 3))
            {
                mount_attributes(&code, at, leaf, &mut flags);
            }
        }
    }
    flags.push(file, out);
}

/// The attributes on a leaf's one mount (the `mod` at `at`): its `#[path]`,
/// and nothing that could mount another file in its place or expand the
/// module into code no line spells — a `cfg_attr` (`path = ..` under it wins
/// over the plain one), an attribute macro, anything outside
/// [`LEAF_ATTRIBUTES`].
fn mount_attributes(code: &str, at: usize, leaf: &str, flags: &mut Flags) {
    let start = code[..at].rfind(['}', ';', '{']).map_or(0, |b| b + 1);
    for (offset, attr) in attributes(&code[start..at]) {
        if attr.split([' ', '=']).next() == Some("path") {
            continue;
        }
        if let Some(why) = leaf_attribute(&attr) {
            flags.at(start + offset, format!("on `{leaf}`'s mount, {why}"));
        }
    }
}

/// The token a `type` item at the start of `s` aliases, if one: named in
/// its generics (`type G<T = OwnerYes> = T;`), or on its right side outside
/// any other type's `<..>` (`type G = (OwnerYes);`; `Option<OwnerYes>` holds
/// one and renames none).
fn aliased_token(s: &str) -> Option<&'static str> {
    let item = &s[..s.find(';')?];
    let rest = item.strip_prefix("type")?.trim_start();
    let name_end = rest.bytes().position(|b| !is_word(b)).unwrap_or(rest.len());
    let rest = rest[name_end..].trim_start();
    let (generics, rest) = if rest.starts_with('<') {
        let close = closing(rest, 0)?;
        (&rest[..=close], &rest[close + 1..])
    } else {
        ("", rest)
    };
    let rhs = &rest[rest.find('=')? + 1..];
    TOKENS.iter().map(|t| t.ty).find(|ty| {
        has(generics, ty, Bound::Word)
            || hits(rhs, ty, Bound::Word).any(|at| {
                let before = &rhs[..at];
                let opened = before.matches('<').count();
                let closed = before.matches('>').count() - before.matches("->").count();
                opened <= closed
            })
    })
}

/// Each `fn`, `const` and `static` item of `code` that names a token and
/// holds an [`UNSAFE_READS`] read is flagged at the read: a forged token, or a
/// copy of a spent one, which no privacy stops. A line outside every such item
/// is read alone.
fn unsafe_reads(code: &str, flags: &mut Flags) {
    let lines: Vec<&str> = code.lines().collect();
    let token = |i: usize| {
        TOKENS
            .iter()
            .map(|t| t.ty)
            .find(|ty| has(lines[i], ty, Bound::Word))
    };
    let read = |i: usize| {
        UNSAFE_READS
            .iter()
            .find(|(read, bound)| has(lines[i], read, *bound))
            .map(|(read, _)| *read)
    };
    let mut covered = vec![false; lines.len()];
    let mut spans: Vec<(usize, usize)> = item_frames(&lines);
    for &(first, last) in &spans {
        covered[first..=last].iter_mut().for_each(|c| *c = true);
    }
    spans.extend((0..lines.len()).filter(|&i| !covered[i]).map(|i| (i, i)));
    for (first, last) in spans {
        let Some(ty) = (first..=last).find_map(token) else {
            continue;
        };
        if let Some((i, read)) = (first..=last).find_map(|i| read(i).map(|r| (i, r))) {
            flags.at(
                flags.starts[i],
                format!("`{read}` in an item that names `{ty}`: a forged or a second token"),
            );
        }
    }
}

/// The lines of each `fn`, `const` and `static` item of `lines`, nested ones
/// too, as (first, last) indexes, by brace depth over the code view.
fn item_frames(lines: &[&str]) -> Vec<(usize, usize)> {
    struct Frame {
        first: usize,
        depth: i64,
        function: bool,
        body: bool,
    }
    let mut frames = Vec::new();
    let mut open: Vec<Frame> = Vec::new();
    let mut depth = 0i64;
    for (i, code) in lines.iter().enumerate() {
        let function = fn_name(code).is_some();
        let head = code.trim_start();
        let head = head.strip_prefix("pub").map_or(head, |rest| {
            let rest = rest.trim_start();
            rest.strip_prefix('(')
                .and_then(|r| r.split_once(')'))
                .map_or(rest, |(_, r)| r.trim_start())
        });
        if function || head.starts_with("const ") || head.starts_with("static ") {
            open.push(Frame {
                first: i,
                depth,
                function,
                body: false,
            });
        }
        let opens = code.matches('{').count() as i64;
        if let Some(top) = open.last_mut()
            && top.function
            && opens > 0
        {
            top.body = true;
        }
        depth += opens - code.matches('}').count() as i64;
        let ends = code.trim_end().ends_with(';');
        while let Some(top) = open.last() {
            let done =
                depth < top.depth || (depth == top.depth && (ends || (top.function && top.body)));
            if !done {
                break;
            }
            frames.push((top.first, i));
            open.pop();
        }
    }
    frames
}

/// The [`TOKEN_COPIES`] trait an `impl <Trait> for <ty>` head implements, if
/// one (the trait and the type matched by their paths' last segments).
fn copy_trait(head: &str, ty: &str) -> Option<&'static str> {
    let (own, of_trait) = impl_self(head)?;
    if own != ty || !of_trait {
        return None;
    }
    let rest = head.trim_start();
    let rest = rest.strip_prefix("unsafe").map_or(rest, str::trim_start);
    let rest = skip_generics(rest.strip_prefix("impl")?.trim_start());
    let (trait_part, _) = rest.rsplit_once(" for ")?;
    let path = trait_part.trim();
    let path = path.split('<').next().unwrap_or(path).trim();
    let last = path.rsplit("::").next().unwrap_or(path);
    TOKEN_COPIES.iter().copied().find(|copy| *copy == last)
}

/// `s` past a leading `<..>` (nested), or `s` itself.
fn skip_generics(s: &str) -> &str {
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0usize;
    for (at, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &s[at + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

/// Where `code` builds a `name` — a tuple `name(..)` or a literal
/// `name { .. }` — rather than declares, implements or returns the type.
fn build_hits<'a>(code: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    hits(code, name, Bound::Word).filter(move |&at| {
        // Past the path that qualifies the name (`for crate::m::Ty {`).
        let before = code[..at]
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == ':');
        let before = before.trim_end();
        let after = code[at + name.len()..].trim_start();
        (after.starts_with('(') || after.starts_with('{'))
            && !before.ends_with("->")
            && !["struct", "enum", "impl", "for", "fn"].iter().any(|k| {
                before
                    .strip_suffix(k)
                    .is_some_and(|rest| rest.bytes().next_back().is_none_or(|b| !is_word(b)))
            })
    })
}

/// Does a line of code build or run a process?
fn builds_a_process(code: &str) -> bool {
    has(code, "Command::new", Bound::Sub)
        || calls(code).iter().any(|c| SPAWN_CALLS.contains(c))
        || code.contains("spawnfd::command")
}

/// Does a line's text name the tool (its name in a literal counts)?
fn names_the_tool(text: &str) -> bool {
    text.contains("DevToolsSecurity")
        || DEVTOOLS_TOOL_WORDS
            .iter()
            .any(|w| has(text, w, Bound::Word))
        || hits(text, "devtools_", Bound::Sub).any(|at| {
            (at == 0 || !is_word(text.as_bytes()[at - 1]))
                && text[at + "devtools_".len()..]
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
                    .is_some_and(|rest| {
                        rest.strip_suffix("_command").is_some_and(|x| {
                            !x.is_empty() && x.bytes().all(|b| b.is_ascii_lowercase())
                        })
                    })
        })
}

/// In consent.rs, may this line name the path constant?
fn path_const_allowed(line: Line<'_>, func: &str) -> bool {
    if has(
        line.code,
        &format!("const {DEVTOOLS_PATH_CONST}"),
        Bound::Word,
    ) {
        return true;
    }
    match path_argv_flag(line.text) {
        Some("status") => true,
        Some(flag @ ("enable" | "disable")) => func == format!("devtools_{flag}_command"),
        _ => false,
    }
}

/// The flag of a `[DEVTOOLSSECURITY_PATH, "-<flag>"]` argv on a line, if one.
fn path_argv_flag(text: &str) -> Option<&str> {
    hits(text, DEVTOOLS_PATH_CONST, Bound::Word).find_map(|at| {
        text[..at]
            .trim_end_matches([' ', '\t'])
            .ends_with('[')
            .then_some(())?;
        let rest = text[at + DEVTOOLS_PATH_CONST.len()..].trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix(',')?.trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix("\"-")?;
        let (flag, rest) = rest.split_once('"')?;
        rest.trim_start_matches([' ', '\t'])
            .starts_with(']')
            .then_some(flag)
    })
}

/// A full tool path with neither quiet flag after it, unless the line defines
/// the path constant (`defines`).
/// `text` with every whitespace character dropped: a definition's spelling, however wrapped.
fn squeeze_code(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn bare_path(text: &str, defines: bool) -> Option<String> {
    hits(text, DEVTOOLS_PATH, Bound::Sub).find_map(|start| {
        let after = text[start + DEVTOOLS_PATH.len()..].trim_start_matches([' ', '\t']);
        (!defines && !DEVTOOLS_QUIET_FLAGS.iter().any(|f| after.starts_with(f))).then(|| {
            format!(
                "`{DEVTOOLS_PATH}` with no {} after it",
                DEVTOOLS_QUIET_FLAGS.join(" or ")
            )
        })
    })
}

/// The foreign functions a local `extern` block may declare in one file only:
/// the name's prefix, that file (`None`: nowhere), and what the call opens.
const FFI_DOORS: &[(&str, Option<&str>, &str)] = &[
    (
        "MTLCreateSystemDefaultDevice",
        Some("crates/aterm-gpu/src/metal/ffi.rs"),
        "a WindowServer connection, and a switch to the discrete GPU",
    ),
    (
        "CreateProcess",
        Some("crates/aterm-pty/src/windows/ffi.rs"),
        "a Windows process, whose console only the ConPTY seam decides",
    ),
    (
        "ShellExecute",
        Some("crates/aterm-gui/src/app_mouse.rs"),
        "a Windows process through the shell, which only the URL opener starts",
    ),
    ("WinExec", None, "a Windows process with a console window"),
];

pub(super) const FFI_DOOR: Rule = Rule {
    id: "ffi-door",
    why: "a WindowServer or process-creation call no clippy path can name (a local `extern` \
          declaration) is declared in shipped code in its one file, behind the privacy that \
          keeps its callers to the one door that decides its console or its WindowServer \
          connection",
    scope: &[
        "crates/aterm-gpu/src/metal/ffi.rs",
        "crates/aterm-pty/src/windows/ffi.rs",
        "crates/aterm-gui/src/app_mouse.rs",
    ],
    check: ffi_door,
};

fn ffi_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let needles: Vec<&str> = FFI_DOORS.iter().map(|(name, ..)| *name).collect();
    for file in product(tree) {
        if tree.corpus.test_code().holds(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        for line in file.shipped_lines() {
            for name in declared(line.code) {
                for (prefix, home, opens) in FFI_DOORS {
                    if name.starts_with(prefix) && *home != Some(file.rel.as_str()) {
                        out.push(file.finding(
                            FFI_DOOR.id,
                            line.no,
                            format!(
                                "declares `{name}` ({opens}) outside {}",
                                home.unwrap_or("every file: it has no door")
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// Every name a line of code declares with `fn`.
fn declared(code: &str) -> impl Iterator<Item = &str> {
    hits(code, "fn", Bound::Word).filter_map(move |at| {
        let rest = &code[at + 2..];
        let name = rest.trim_start_matches([' ', '\t']);
        let end = name.bytes().take_while(|&b| is_word(b)).count();
        (name.len() < rest.len() && end > 0).then(|| &name[..end])
    })
}

/// The macOS boot id's sysctl name.
const BOOT_ID_SYSCTL: &str = "kern.bootsessionuuid";
/// The calls that read a sysctl by its name: the read itself, and the name's
/// lookup a `sysctl(2)` by MIB reads it through.
const SYSCTL_BY_NAME: &[&str] = &["sysctlbyname", "sysctlnametomib"];

pub(super) const BOOT_ID_DOOR: Rule = Rule {
    id: "boot-id-door",
    why: "the macOS boot id has one sysctl reader, `astream_sys::boot_session` (16 bytes, \
          read through `parse_boot_id`, which refuses the all-zero id a damaged answer reads \
          as, so two damaged answers never pass as one boot); a `sysctlbyname` of \
          kern.bootsessionuuid elsewhere is a second reader without that judge, and twice one \
          came back (30ae1a1185, f6ae36dfb4) after D1 had moved the tree onto astream's",
    scope: &[],
    check: boot_id_door,
};

/// A file of the tree that reads a sysctl by name ([`SYSCTL_BY_NAME`], the call on the code
/// view) and holds the boot id's name in a literal (the text view, so a comment naming it is no
/// read): each such literal, wherever the call takes its name from (inline, a `const`, a helper
/// of the same file).
fn boot_id_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in product(tree) {
        if !file.mentions(&[BOOT_ID_SYSCTL]) || !file.mentions(SYSCTL_BY_NAME) {
            continue;
        }
        let reads = file
            .lines()
            .any(|line| calls(line.code).iter().any(|c| SYSCTL_BY_NAME.contains(c)));
        if !reads {
            continue;
        }
        for line in file.lines() {
            if has(line.text, BOOT_ID_SYSCTL, Bound::Sub) {
                out.push(file.finding(
                    BOOT_ID_DOOR.id,
                    line.no,
                    format!(
                        "names `{BOOT_ID_SYSCTL}` in a file that reads a sysctl by name: read \
                         the boot with `astream_sys::boot_session` (and its text with \
                         `astream_sys::parse_boot_id`)"
                    ),
                ));
            }
        }
    }
}

/// aterm-gui's source: the window crate's and its split-out libraries'
/// (`crates/aterm-gui-*`).
const HANDOFF_SCOPE: &[&str] = &["crates/aterm-gui/src/", "crates/aterm-gui-"];
/// The module that owns the names' environment.
const HANDOFF_HOME: &str = "crates/aterm-gui/src/handoff_env.rs";
const HANDOFF_PREFIXES: &[&str] = &["\"ATERM_SEAMLESS_", "\"ATERM_HANDOFF_"];
/// Where such a literal may stand: as a `HandoffName`, or a dev seam's name (a
/// timeout knob that vanishes from release builds, no authority).
const HANDOFF_DOORS: &[&str] = &["HandoffName::new(", "dev_seam!("];

pub(super) const HANDOFF_NAME: Rule = Rule {
    id: "handoff-name",
    why: "a live-upgrade handoff variable carries the successor's authority (its descriptors, \
          nonce, rendezvous); spelled as a bare string it can be read or handed on past \
          `HandoffName`, which pairs every name with its value",
    scope: &[HANDOFF_HOME],
    check: handoff_name,
};

fn handoff_name(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let needles: Vec<&str> = HANDOFF_PREFIXES.iter().map(|p| &p[1..]).collect();
    for file in tree.corpus.under(HANDOFF_SCOPE) {
        if file.rel == HANDOFF_HOME
            || tree.corpus.test_code().holds(&file.rel)
            || !file.mentions(&needles)
        {
            continue;
        }
        for line in file.shipped_lines() {
            for prefix in HANDOFF_PREFIXES {
                for at in hits(line.text, prefix, Bound::Sub) {
                    let before = line.code[..at].trim_end_matches([' ', '\t']);
                    if !HANDOFF_DOORS.iter().any(|d| before.ends_with(d)) {
                        out.push(file.finding(
                            HANDOFF_NAME.id,
                            line.no,
                            format!("spells `{}…` outside `HandoffName::new`", &prefix[1..]),
                        ));
                    }
                }
            }
        }
    }
}

/// The signals whose death makes macOS write a crash report (and, for an app
/// bundle, put a "quit unexpectedly" dialog on the owner's screen).
const CORE_SIGNALS: &[&str] = &[
    "ABRT", "IOT", "SEGV", "BUS", "ILL", "FPE", "QUIT", "TRAP", "SYS", "EMT", "XCPU", "XFSZ",
];
/// Their numbers macOS and Linux agree on.
const CORE_NUMBERS: &[&str] = &["3", "4", "5", "6", "7", "8", "11"];
/// The Rust calls that send one.
const SENDERS: &[&str] = &["kill", "killpg", "raise", "pthread_kill"];
/// A word every shape [`core_shape`] matches holds: a line without one is skipped.
const CORE_WORDS: &[&str] = &[
    "abort",
    "faulthandler",
    "string_at",
    "kill",
    "raise",
    "timeout",
    "\"-",
    "'-",
];

pub(super) const CORE_SIGNAL: Rule = Rule {
    id: "core-signal",
    why: "a process killed by a core-dump signal makes macOS write a crash report, and an app \
          bundle's (Homebrew's python3 is one) puts a \"quit unexpectedly\" dialog on the \
          owner's screen; a stand-in that must die dies by SIGKILL (kill -KILL, os.kill(pid, 9), \
          os._exit(n), aterm_uds::process::kill_self_now) or by TERM where a wrapper re-raises it",
    scope: &[],
    check: core_signal,
};

fn core_signal(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in product(tree) {
        if !file.mentions(&["SIG"]) && !file.mentions(CORE_WORDS) {
            continue;
        }
        // A line sends a signal by name only where its code spells `SIG`, and has a shape only
        // where its text holds a core word: only those lines are read.
        for line in lines_holding(file, &["SIG"], CORE_WORDS) {
            let sends = calls(line.code).iter().any(|c| SENDERS.contains(c));
            let named = CORE_SIGNALS
                .iter()
                .find(|sig| sends && has(line.code, &format!("SIG{sig}"), Bound::Word));
            if let Some(why) = named
                .map(|sig| format!("sends SIG{sig}"))
                .or_else(|| core_shape(line.text))
            {
                out.push(file.finding(CORE_SIGNAL.id, line.no, why));
            }
        }
    }
    for script in tree.corpus.scripts() {
        if !CORE_WORDS.iter().any(|w| script.raw.contains(w)) {
            continue;
        }
        for (no, line) in script.lines() {
            if let Some(why) = core_shape(line) {
                out.push(script.finding(CORE_SIGNAL.id, no, why));
            }
        }
    }
}

/// Each line of `file` ([`SourceFile::lines`]) whose CODE view holds one of `in_code` or whose
/// TEXT view holds one of `in_text`, in order; no needle holds a line break. Both views break
/// lines at the same offsets, so a run of whole lines is read line by line only where the run
/// holds a needle.
fn lines_holding<'a>(file: &'a SourceFile, in_code: &[&str], in_text: &[&str]) -> Vec<Line<'a>> {
    const RUN: usize = 1024;
    let Lexed { code, text } = file.lexed();
    let holds = |code: &str, text: &str| {
        in_code.iter().any(|n| code.contains(n)) || in_text.iter().any(|n| text.contains(n))
    };
    let mut out = Vec::new();
    let (mut from, mut no) = (0, 1);
    while from < code.len() {
        let probe = (from + RUN).min(code.len());
        let to = code.as_bytes()[probe..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(code.len(), |nl| probe + nl + 1);
        let (run_code, run_text) = (&code[from..to], &text[from..to]);
        if holds(run_code, run_text) {
            for (at, (code, text)) in run_code.lines().zip(run_text.lines()).enumerate() {
                if holds(code, text) {
                    out.push(Line {
                        no: no + at,
                        code,
                        text,
                    });
                }
            }
        }
        no += run_code.bytes().filter(|&b| b == b'\n').count();
        from = to;
    }
    out
}

/// The first shape on `line` that ends a process by a core-dump signal, named.
fn core_shape(line: &str) -> Option<String> {
    if !CORE_WORDS.iter().any(|w| line.contains(w)) {
        return None;
    }
    if aborts(line) {
        return Some("an abort() call".to_string());
    }
    if line.contains("faulthandler._")
        && hits(line, "faulthandler._", Bound::Sub).any(|at| {
            let rest = &line[at + "faulthandler._".len()..];
            let name = rest
                .bytes()
                .take_while(|b| b.is_ascii_lowercase() || *b == b'_')
                .count();
            name > 0 && rest[name..].starts_with('(')
        })
    {
        return Some("a private faulthandler crash helper".to_string());
    }
    if hits(line, "ctypes.string_at(", Bound::Sub).any(|at| {
        let rest = line[at + "ctypes.string_at(".len()..].trim_start();
        rest.strip_prefix('0')
            .is_some_and(|r| r.trim_start().starts_with([',', ')']))
    }) {
        return Some("a NULL ctypes.string_at".to_string());
    }
    if let Some(why) = signal_call(line) {
        return Some(why);
    }
    if let Some(why) = shell_kill(line) {
        return Some(why);
    }
    argv_signal(line)
}

/// `SIG` (optional) and a core signal at the start of `s` — or, with
/// `numbers`, one of [`CORE_NUMBERS`] — ending at a word edge: its name.
fn core_at(s: &str, numbers: bool) -> Option<&'static str> {
    let bare = s.strip_prefix("SIG").unwrap_or(s);
    let names = CORE_SIGNALS
        .iter()
        .chain(if numbers { CORE_NUMBERS } else { &[] });
    names.copied().find(|name| {
        bare.strip_prefix(name)
            .is_some_and(|rest| rest.bytes().next().is_none_or(|b| !is_word(b)))
    })
}

/// A Python or Node abort: `os.abort(`, `__import__("os").abort(`,
/// `CDLL(…).abort(`, `process.abort(`, `from os import … abort`.
fn aborts(line: &str) -> bool {
    let call = hits(line, ".abort(", Bound::Sub).any(|at| {
        let before = &line[..at];
        let word_ends = |w: &str| {
            before
                .strip_suffix(w)
                .is_some_and(|b| b.bytes().next_back().is_none_or(|c| !is_word(c)))
        };
        let cdll = before.rfind("CDLL(").is_some_and(|open| {
            let inner = &before[open + "CDLL(".len()..];
            inner.strip_suffix(')').is_some_and(|i| !i.contains(')'))
        });
        word_ends("os")
            || word_ends("process")
            || before.ends_with("\"os\")")
            || before.ends_with("'os')")
            || cdll
    });
    call || hits(line, "from", Bound::Word).any(|at| {
        let rest = &line[at + 4..];
        let Some(rest) = spaced(rest).and_then(|r| r.strip_prefix("os")) else {
            return false;
        };
        let Some(rest) = spaced(rest).and_then(|r| r.strip_prefix("import")) else {
            return false;
        };
        let names = rest.split('#').next().unwrap_or(rest);
        has(names, "abort", Bound::Word)
    })
}

/// `s` past one or more spaces or tabs, `None` when it does not start with one.
fn spaced(s: &str) -> Option<&str> {
    let rest = s.trim_start_matches([' ', '\t']);
    (rest.len() < s.len()).then_some(rest)
}

/// A kill call naming a core signal: `os.kill`, `os.killpg`,
/// `signal.raise_signal`, `signal.pthread_kill`, `process.kill` or
/// `libc::kill`/`killpg`/`raise` with `SIG<core>` among its arguments; Python's
/// with a core signal's number as its last.
fn signal_call(line: &str) -> Option<String> {
    const BY_NAME: &[&str] = &[
        "os.kill(",
        "os.killpg(",
        "signal.raise_signal(",
        "signal.pthread_kill(",
        "process.kill(",
        "libc::kill(",
        "libc::killpg(",
        "libc::raise(",
    ];
    const BY_NUMBER: &[&str] = &["os.kill(", "os.killpg(", "signal.pthread_kill("];
    for call in BY_NAME {
        for at in hits(line, call, Bound::Sub) {
            let args = arguments(&line[at + call.len()..]);
            for sig in hits(args, "SIG", Bound::Sub) {
                if let Some(name) = core_at(&args[sig..], false) {
                    return Some(format!("sends SIG{name}"));
                }
            }
            let last = if *call == "signal.raise_signal(" {
                Some(args)
            } else if BY_NUMBER.contains(call) {
                args.rsplit_once(',').map(|(_, last)| last)
            } else {
                None
            };
            if let Some(n) = last.map(str::trim)
                && CORE_NUMBERS.contains(&n)
                && line[at + call.len() + args.len()..].starts_with(')')
            {
                return Some(format!("sends signal {n}"));
            }
        }
    }
    None
}

/// A call's arguments: up to its closing parenthesis (one nested level), a
/// `#` comment or the line's end.
fn arguments(s: &str) -> &str {
    let mut depth = 0usize;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'#' => return &s[..i],
            b'(' => depth += 1,
            b')' if depth == 0 => return &s[..i],
            b')' => depth -= 1,
            _ => {}
        }
    }
    s
}

/// `kill`, `pkill` or `killall` as a shell word given a core signal
/// (`-ABRT`, `-SIGSEGV`, `-s QUIT`, `-n 6`, `-11`), or `timeout -s <core>` /
/// `--signal=<core>`.
fn shell_kill(line: &str) -> Option<String> {
    let b = line.as_bytes();
    for at in hits(line, "kill", Bound::Sub) {
        let start = if at > 0 && b[at - 1] == b'p' {
            at - 1
        } else {
            at
        };
        if start > 0 && (is_word(b[start - 1]) || matches!(b[start - 1], b'.' | b'-')) {
            continue;
        }
        let word_end = if line[at + 4..].starts_with("all") {
            at + 7
        } else {
            at + 4
        };
        let Some(rest) = spaced(&line[word_end..]).and_then(|r| r.strip_prefix('-')) else {
            continue;
        };
        let rest = ["s", "n"]
            .iter()
            .find_map(|f| rest.strip_prefix(f).and_then(spaced))
            .unwrap_or(rest);
        if let Some(name) = core_at(rest, true) {
            return Some(format!("{} -{name}", &line[start..word_end]));
        }
    }
    for at in hits(line, "timeout", Bound::Sub) {
        let Some(rest) = spaced(&line[at + "timeout".len()..]) else {
            continue;
        };
        let flag = rest
            .strip_prefix("--signal")
            .map(|r| r.strip_prefix('=').or_else(|| spaced(r)).unwrap_or(r))
            .or_else(|| {
                rest.strip_prefix("-s")
                    .map(|r| r.trim_start_matches([' ', '\t']))
            });
        if let Some(name) = flag.and_then(|r| core_at(r, false)) {
            return Some(format!("timeout -s {name}"));
        }
    }
    None
}

/// A core signal as an argv string: `"-ABRT"` (or `'-SIGSEGV'`), or a `"-s"` /
/// `"-n"` element followed by `"<core>"`.
fn argv_signal(line: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let open = format!("{quote}-");
        for at in hits(line, &open, Bound::Sub) {
            let rest = &line[at + 2..];
            if let Some(name) = core_at(rest, false) {
                let after = rest.strip_prefix("SIG").unwrap_or(rest);
                if after[name.len()..].starts_with(quote) {
                    return Some(format!("hands a kill the argument {quote}-{name}{quote}"));
                }
            }
            let Some(rest) = ["s", "n"]
                .iter()
                .find_map(|f| rest.strip_prefix(f).and_then(|r| r.strip_prefix(quote)))
            else {
                continue;
            };
            let rest = rest.trim_start_matches([' ', '\t']);
            let Some(rest) = rest
                .strip_prefix(',')
                .map(|r| r.trim_start_matches([' ', '\t']))
            else {
                continue;
            };
            if let Some(rest) = rest.strip_prefix(quote)
                && let Some(name) = core_at(rest, false)
                && rest.strip_prefix("SIG").unwrap_or(rest)[name.len()..].starts_with(quote)
            {
                return Some(format!("hands a kill the arguments -s {name}"));
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "doors_tests.rs"]
mod tests;
