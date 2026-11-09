// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `autopilot-captures` ([`cmd_autopilot_captures`]): Autopilot's scenes on every harness, side by
//! side, as one captures page reads them (design §14.2's 2b). One private headless instance per
//! harness and look, launched in that look, so each program starts on the ground it is shot on: on
//! the three aterm supervises (Claude Code, agy, Muse Code) it drives the forever lab's
//! own chains against its stub ([`crate::forever`]: cruise-n's two needs, land-b's open hold,
//! land-c's repository hold, land-e's landing, Off, same-land's stop, and on Claude Code
//! switch-over-held's ask at the switch to Land), Claude Code on an owner's own login with the
//! usage tap on its line, so its glass shows line E ([`scene_launcher`]); on codex-alab, which
//! supervises itself, it drives `codex-alab-proof`'s legs against its fake model
//! ([`crate::codex_alab_proof`]: Cruise, Land, landed, Off, a need). A scene this tree cannot
//! show is a named skip ([`Plan::Skip`]).
//! After each scene settles (Claude Code's once its line E says what its pane allows,
//! [`line_e_unsaid`]) it takes an `image` at rest, points at the chip for its hovered title,
//! and writes `chrome` and `status` beside each png, then `manifest.tsv` ([`Row`]). [`missing`]
//! names a row the plan wants and the manifest lacks, and [`faults`] a png row that shows something
//! wrong. With `--baseline` it also takes one paired agy typing take ([`typing_pair`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::{HeldBy, HoldLeg};
use aterm_agent::harness::footer::text_width;
use aterm_agent::supervise::policy::turn_end::RULE_LAND;

use crate::codex_alab_proof::{self, Proof};
use crate::door::Door;
use crate::forever::{self, Chain, Harness};
use crate::run::Run;
use crate::stop;
use crate::{Fail, Res};

/// A harness the captures run on: one aterm supervises, or codex-alab, which supervises itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Agent(Harness),
    CodexAlab,
}

impl Who {
    /// Every harness, in the order `all` runs them and the manifest lists them.
    pub const ALL: [Who; 4] = [
        Who::Agent(Harness::Claude),
        Who::Agent(Harness::Agy),
        Who::Agent(Harness::Muse),
        Who::CodexAlab,
    ];

    /// The verb's word and the manifest's `harness`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Who::Agent(h) => h.name(),
            Who::CodexAlab => "codex-alab",
        }
    }

    fn of(word: &str) -> Option<Who> {
        Who::ALL.into_iter().find(|w| w.name() == word)
    }

    /// Its scenes, in the manifest's order, each with how it is shown.
    #[must_use]
    pub const fn scenes(self) -> &'static [(Scene, Plan)] {
        match self {
            Who::Agent(Harness::Claude) => &SUPERVISED_CLAUDE,
            Who::Agent(_) => &SUPERVISED,
            Who::CodexAlab => &CODEX_ALAB,
        }
    }

    /// Why its rest shots may show no chip, so no hovered title beside them; `None` where it
    /// draws one on every scene, and a rest shot with no chip is a [`Fault::NoChip`]. Each of the
    /// three supervised harnesses draws a footer, so a chip, on every scene. agy draws its chip
    /// under Autopilot Off too: only an off harness switch (the master's or its agent's) leaves
    /// its footer bare, and no scene turns one off.
    #[must_use]
    pub const fn chipless(self) -> Option<&'static str> {
        match self {
            Who::Agent(_) => None,
            Who::CodexAlab => Some(OWN_LINE),
        }
    }

    fn rank(self) -> usize {
        Who::ALL
            .iter()
            .position(|w| *w == self)
            .unwrap_or(usize::MAX)
    }
}

/// Codex owns its composer, including when its native interface leaves the pad blank.
/// Each caption quotes that pad; aterm reports `why=no-footer` and draws no chip or title.
pub const OWN_LINE: &str =
    "codex-alab owns its controls; aterm draws no footer (chrome's why=no-footer)";

/// A scene the captures show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    /// Cruise, its report naming two needs (cruise-n, N1).
    CruiseNeeds,
    /// Land held by open work its report names (land-b, L2), quoted (4b): its rest and hovered
    /// title, and beside them the Overview and Settings ▸ Harness ([`Shots::held_extras`]).
    LandOpen,
    /// Land held by work in its repository, an untracked file (land-c, L4), named (4b): as
    /// [`Self::LandOpen`].
    LandRepo,
    /// Land landed (land-e, L7: a task over `aterm ctl`, so the landing's Done for you row and its
    /// tip stand): its rest and hovered title, and beside them the Overview and Settings ▸
    /// Harness, whose state line says what aterm checked (slice 9c, [`Shots::held_extras`]).
    Landed,
    /// Autopilot Off.
    Off,
    /// Land held by the same reply, the stop said (`same-land`, 3a): its rest and hovered title,
    /// and beside them the Overview and Settings ▸ Harness ([`Shots::held_extras`]).
    SameLand,
    /// Cruise held by the same reply, switched to Land: Land's one ask standing (`switch-over-held`,
    /// v6 §14.17 item 3), its rest and hovered title, and beside them the Overview and Settings ▸
    /// Harness, whose state line says it ([`Shots::held_extras`]).
    SwitchAsked,
    /// codex-alab under Cruise, at rest after a turn.
    Cruise,
    /// codex-alab landing: Land's instruction typed.
    Land,
    /// codex-alab's report naming a need.
    Need,
}

impl Scene {
    const ALL: [Scene; 10] = [
        Scene::CruiseNeeds,
        Scene::LandOpen,
        Scene::LandRepo,
        Scene::Landed,
        Scene::Off,
        Scene::SameLand,
        Scene::SwitchAsked,
        Scene::Cruise,
        Scene::Land,
        Scene::Need,
    ];

    /// The manifest's `scene`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scene::CruiseNeeds => "cruise-needs",
            Scene::LandOpen => "land-open",
            Scene::LandRepo => "land-repo",
            Scene::Landed => "landed",
            Scene::Off => "off",
            Scene::SameLand => "same-land",
            Scene::SwitchAsked => "switch-asked",
            Scene::Cruise => "cruise",
            Scene::Land => "land",
            Scene::Need => "need",
        }
    }

    fn of(word: &str) -> Option<Scene> {
        Scene::ALL.into_iter().find(|s| s.name() == word)
    }

    /// What it shows, for the captions.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Scene::CruiseNeeds => "Cruise, its report naming two needs (cruise-n, N1)",
            Scene::LandOpen => "Land held by open work its report names (land-b, L2)",
            Scene::LandRepo => "Land held by an untracked file in its repository (land-c, L4)",
            Scene::Landed => "Land landed",
            Scene::Off => "Autopilot Off: its reply read, nothing typed",
            Scene::SameLand => "Land held by the same reply, the stop said",
            Scene::SwitchAsked => "Cruise's repeated reply switched to Land: Land asked once more",
            Scene::Cruise => "Cruise at rest after a turn",
            Scene::Land => "Land landing: Land's instruction typed (land@v1)",
            Scene::Need => "its report naming a NEEDS YOU need",
        }
    }

    fn rank(self) -> usize {
        [&SUPERVISED_CLAUDE[..], &CODEX_ALAB[..]]
            .iter()
            .find_map(|s| s.iter().position(|(sc, _)| *sc == self))
            .unwrap_or(usize::MAX)
    }
}

/// How a scene is shown on a harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The forever lab's chain, in a tab of its own.
    Chain(Chain),
    /// `codex-alab-proof`'s leg, in a tab of its own.
    Leg(&'static str),
    /// Not on this tree: the reason the manifest names.
    Skip(&'static str),
}

/// Why codex-alab's holds are skips: the scenes are the forever lab's chains on aterm's own loop,
/// and codex-alab's hold is its own.
pub const HOLDS_UNSHOT: &str = "UNOBSERVED here: these scenes are the forever lab's chains on \
    aterm's own loop; codex-alab's Land hold is its own, told from its build 13942 on (A3's \
    half, §14.14, read by codex-alab-proof --window-only), and its shots are the round after 8b's";

/// Why Land's ask at the switch is shot on Claude Code alone: it is said by the window, the state
/// line's words beside the row, and drawn the same for every harness aterm supervises.
pub const SWITCH_ON_CLAUDE: &str = "UNSHOT here: Land's ask at the switch is said by the window \
    (Settings ▸ Harness's state line), drawn the same for every harness aterm supervises, and \
    shot on Claude Code; forever's switch-over-held judges it on this harness";

const SUPERVISED: [(Scene, Plan); 7] = [
    (Scene::CruiseNeeds, Plan::Chain(Chain::CruiseN)),
    (Scene::LandOpen, Plan::Chain(Chain::LandB)),
    (Scene::LandRepo, Plan::Chain(Chain::LandC)),
    // land-e, not land-a: its task over `aterm ctl` leaves a Done for you row, whose tip says what
    // aterm checked (slice 9c); land-a's quick landing leaves none (no task, no end).
    (Scene::Landed, Plan::Chain(Chain::LandE)),
    (Scene::Off, Plan::Chain(Chain::Off)),
    (Scene::SameLand, Plan::Chain(Chain::SameLand)),
    (Scene::SwitchAsked, Plan::Skip(SWITCH_ON_CLAUDE)),
];

/// Claude Code's scenes: [`SUPERVISED`]'s, and Land's ask at the switch shot.
const SUPERVISED_CLAUDE: [(Scene, Plan); 7] = [
    (Scene::CruiseNeeds, Plan::Chain(Chain::CruiseN)),
    (Scene::LandOpen, Plan::Chain(Chain::LandB)),
    (Scene::LandRepo, Plan::Chain(Chain::LandC)),
    (Scene::Landed, Plan::Chain(Chain::LandA)),
    (Scene::Off, Plan::Chain(Chain::Off)),
    (Scene::SameLand, Plan::Chain(Chain::SameLand)),
    (Scene::SwitchAsked, Plan::Chain(Chain::SwitchOverHeld)),
];

const CODEX_ALAB: [(Scene, Plan); 9] = [
    (Scene::Cruise, Plan::Leg("continue")),
    (Scene::Land, Plan::Leg("land")),
    (Scene::Landed, Plan::Leg("reland")),
    (Scene::Off, Plan::Leg("off")),
    (Scene::Need, Plan::Leg("needs")),
    (Scene::LandOpen, Plan::Skip(HOLDS_UNSHOT)),
    (Scene::LandRepo, Plan::Skip(HOLDS_UNSHOT)),
    (Scene::SameLand, Plan::Skip(HOLDS_UNSHOT)),
    (Scene::SwitchAsked, Plan::Skip(HOLDS_UNSHOT)),
];

/// A look: the theme the instance, and so every program in it, is launched in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Day,
    Night,
}

impl Look {
    pub const BOTH: [Look; 2] = [Look::Day, Look::Night];

    /// The manifest's `look`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Look::Day => "day",
            Look::Night => "night",
        }
    }

    /// The theme its instance's `aterm.toml` names.
    #[must_use]
    pub const fn theme(self) -> &'static str {
        match self {
            Look::Day => "Aterm Day",
            Look::Night => "Aterm Night",
        }
    }

    fn named(word: &str) -> Option<Look> {
        Look::BOTH.into_iter().find(|l| l.name() == word)
    }

    fn of(word: &str) -> Option<Vec<Look>> {
        match word {
            "both" => Some(Look::BOTH.to_vec()),
            w => Look::named(w).map(|l| vec![l]),
        }
    }

    /// A frame of this look's mean luminance `luma` is drawn on its ground: Day's light, Night's
    /// dark.
    #[must_use]
    pub fn ground_holds(self, luma: f64) -> bool {
        match self {
            Look::Day => luma > 0.5,
            Look::Night => luma < 0.5,
        }
    }
}

/// `toml` with `theme` as its top-level `theme`: a `theme` line of its own before any table
/// left out, so the file names one.
fn themed(toml: &str, theme: &str) -> String {
    let mut in_table = false;
    let kept: Vec<&str> = toml
        .lines()
        .filter(|l| {
            in_table |= l.trim_start().starts_with('[');
            in_table || l.split_once('=').is_none_or(|(k, _)| k.trim() != "theme")
        })
        .collect();
    format!("theme = {theme:?}\n{}", kept.join("\n"))
}

/// What a png shows of the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The pointer away.
    Rest,
    /// The pointer on the chip, its title drawn in the rule.
    Title,
}

impl Kind {
    const fn word(self) -> &'static str {
        match self {
            Kind::Rest => "rest",
            Kind::Title => "title",
        }
    }

    /// What a png of it shows, for its caption and `.txt`.
    const fn shows(self) -> &'static str {
        match self {
            Kind::Rest => "at rest, the pointer away",
            Kind::Title => "the pointer on the chip",
        }
    }
}

/// A scene's file in the manifest's folder, `<harness>-<scene>-<look>-<scale>x-<kind><tail>`:
/// its png ([`png_name`]), the `.txt` beside it and a title's `-band.png`.
fn file_name(who: Who, scene: Scene, look: Look, scale: u8, kind: Kind, tail: &str) -> String {
    format!(
        "{}-{}-{}-{scale}x-{}{tail}",
        who.name(),
        scene.name(),
        look.name(),
        kind.word()
    )
}

/// The png of one harness × scene × look × scale of `kind`: the name every shot is written
/// under and every row's png is held to ([`Row::kind`]).
#[must_use]
pub fn png_name(who: Who, scene: Scene, look: Look, scale: u8, kind: Kind) -> String {
    file_name(who, scene, look, scale, kind, ".png")
}

/// The manifest's columns, its first line.
pub const HEAD: &str = "harness\tscene\tlook\tscale\tpng\tcaption";

/// A skip row's caption: its reason follows.
pub const SKIPPED: &str = "skipped: ";

/// A planned row that has no png: why follows.
pub const NOT_CAPTURED: &str = "NOT CAPTURED: ";

/// A caption's word for something its png shows wrong (a look's ground, a title that did not
/// show or was cut short).
pub const FINDING: &str = "FINDING: ";

/// One `manifest.tsv` row: a png (beside its `.txt`), or a skip (`png` `-`, the caption the
/// reason).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub harness: String,
    pub scene: String,
    pub look: String,
    /// `1` or `2`.
    pub scale: String,
    /// The png's file name in the manifest's folder, `-` for none.
    pub png: String,
    pub caption: String,
}

impl Row {
    fn new(who: Who, scene: Scene, look: Look, scale: u8, png: &str, caption: String) -> Row {
        Row {
            harness: who.name().to_string(),
            scene: scene.name().to_string(),
            look: look.name().to_string(),
            scale: scale.to_string(),
            png: png.to_string(),
            caption,
        }
    }

    /// Its line: tab-separated, no tab or line break inside a field.
    #[must_use]
    pub fn line(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        [
            &self.harness,
            &self.scene,
            &self.look,
            &self.scale,
            &self.png,
            &self.caption,
        ]
        .map(|f| clean(f))
        .join("\t")
    }

    fn parse(line: &str) -> Option<Row> {
        let mut f = line.splitn(6, '\t');
        let mut next = || f.next().map(str::to_string);
        Some(Row {
            harness: next()?,
            scene: next()?,
            look: next()?,
            scale: next()?,
            png: next()?,
            caption: next()?,
        })
    }

    fn of(&self, who: Who, scene: Scene, look: Look, scale: u8) -> bool {
        self.harness == who.name()
            && self.scene == scene.name()
            && self.look == look.name()
            && self.scale == scale.to_string()
    }

    /// Its harness, scene, look and scale, `None` where one names none the captures know.
    fn keys(&self) -> Option<(Who, Scene, Look, u8)> {
        Some((
            Who::of(&self.harness)?,
            Scene::of(&self.scene)?,
            Look::named(&self.look)?,
            self.scale.parse().ok()?,
        ))
    }

    /// Which of its own cell's pngs it is ([`png_name`]), `None` for no png or one named for
    /// another cell.
    fn kind(&self) -> Option<Kind> {
        let (who, scene, look, scale) = self.keys()?;
        [Kind::Rest, Kind::Title]
            .into_iter()
            .find(|&k| self.png == png_name(who, scene, look, scale, k))
    }

    fn order(&self) -> (usize, usize, String, String, String) {
        let who = Who::of(&self.harness).map_or(usize::MAX, Who::rank);
        let scene = Scene::of(&self.scene).map_or(usize::MAX, Scene::rank);
        (
            who,
            scene,
            self.scale.clone(),
            self.look.clone(),
            self.png.clone(),
        )
    }
}

/// A manifest's rows, its head and any line that is no row left out.
#[must_use]
pub fn parse_manifest(text: &str) -> Vec<Row> {
    text.lines()
        .filter(|l| *l != HEAD && !l.trim().is_empty())
        .filter_map(Row::parse)
        .collect()
}

/// A manifest's text: its head, then `rows` by harness, scene, scale, look and png.
#[must_use]
pub fn manifest_text(rows: &[Row]) -> String {
    let mut rows = rows.to_vec();
    rows.sort_by_key(Row::order);
    let mut s = format!("{HEAD}\n");
    for r in rows {
        s.push_str(&r.line());
        s.push('\n');
    }
    s
}

/// What a manifest lacks of one harness × scene × look × scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lack {
    /// No row names it.
    NoRow,
    /// Its plan skips it, and no row names the skip's reason.
    Unreasoned,
    /// Its plan shows it, and its rows are skips: no png.
    Skipped,
    /// Its plan shows it, and its row says why it has no png.
    NotCaptured(String),
}

/// One harness × scene × look × scale a manifest lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    pub who: Who,
    pub scene: Scene,
    pub look: Look,
    pub scale: u8,
    pub lack: Lack,
}

impl std::fmt::Display for Gap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {} {}x: ",
            self.who.name(),
            self.scene.name(),
            self.look.name(),
            self.scale
        )?;
        match &self.lack {
            Lack::NoRow => write!(f, "no row"),
            Lack::Unreasoned => write!(f, "no row skips it with its reason"),
            Lack::Skipped => write!(f, "no png, only a skip, though its plan shows it"),
            Lack::NotCaptured(why) => write!(f, "no png ({why})"),
        }
    }
}

/// What a manifest lacks of `whos` × their scenes × `looks` × `scales`: a scene its plan shows
/// with no png row, and a skip with no row naming its reason. Empty when every row is there.
#[must_use]
pub fn missing(rows: &[Row], whos: &[Who], looks: &[Look], scales: &[u8]) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for &who in whos {
        for &(scene, plan) in who.scenes() {
            for &scale in scales {
                for &look in looks {
                    let here: Vec<&Row> = rows
                        .iter()
                        .filter(|r| r.of(who, scene, look, scale))
                        .collect();
                    let reasoned = |r: &&Row| {
                        r.png == "-"
                            && r.caption
                                .strip_prefix(SKIPPED)
                                .is_some_and(|why| !why.trim().is_empty())
                    };
                    let lack = match plan {
                        _ if here.is_empty() => Some(Lack::NoRow),
                        Plan::Skip(_) if !here.iter().any(reasoned) => Some(Lack::Unreasoned),
                        Plan::Chain(_) | Plan::Leg(_) if !here.iter().any(|r| r.png != "-") => {
                            Some(
                                here.iter()
                                    .find_map(|r| r.caption.strip_prefix(NOT_CAPTURED))
                                    .map_or(Lack::Skipped, |why| {
                                        Lack::NotCaptured(why.to_string())
                                    }),
                            )
                        }
                        _ => None,
                    };
                    if let Some(lack) = lack {
                        gaps.push(Gap {
                            who,
                            scene,
                            look,
                            scale,
                            lack,
                        });
                    }
                }
            }
        }
    }
    gaps
}

/// Something a png row shows wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Its caption carries a [`FINDING`]: what follows the word.
    Finding(String),
    /// Its png is not named for its own row ([`png_name`]): the png.
    Misnamed(String),
    /// A chip is drawn at rest, and its cell has no title png: the cell's why, where a row
    /// gives one.
    NoTitle(String),
    /// No chip drawn on a harness and scene that draws one ([`Who::chipless`]): chrome's why.
    NoChip(String),
}

/// One row's fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flaw {
    /// The row's harness, scene, look and scale, as its line has them.
    pub at: String,
    pub fault: Fault,
}

impl std::fmt::Display for Flaw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: ", self.at)?;
        match &self.fault {
            Fault::Finding(what) => write!(f, "{FINDING}{what}"),
            Fault::Misnamed(png) => write!(f, "{png} is not this row's png"),
            Fault::NoTitle(why) => write!(f, "a chip drawn at rest, and no hovered title ({why})"),
            Fault::NoChip(why) => write!(f, "no chip drawn where its harness draws one ({why})"),
        }
    }
}

/// What a manifest's png rows show wrong: a [`FINDING`] in a caption, a png named for another
/// cell, a drawn chip with no hovered title beside it, and no chip on a harness and scene that
/// draws one. Empty when every png row stands. The chip's words are read back by key off the
/// caption [`chip_words`] wrote.
#[must_use]
pub fn faults(rows: &[Row]) -> Vec<Flaw> {
    let mut flaws = Vec::new();
    for r in rows {
        let at = format!("{} {} {} {}x", r.harness, r.scene, r.look, r.scale);
        let mut flaw = |fault| {
            flaws.push(Flaw {
                at: at.clone(),
                fault,
            });
        };
        if let Some((_, what)) = r.caption.split_once(FINDING) {
            flaw(Fault::Finding(what.to_string()));
        }
        if r.png == "-" {
            continue;
        }
        let Some(kind) = r.kind() else {
            flaw(Fault::Misnamed(r.png.clone()));
            continue;
        };
        let Some((who, scene, look, scale)) = r.keys().filter(|_| kind == Kind::Rest) else {
            continue;
        };
        let cell: Vec<&Row> = rows
            .iter()
            .filter(|o| o.of(who, scene, look, scale))
            .collect();
        if drawn_of(&r.caption).is_some() {
            if !cell.iter().any(|o| o.kind() == Some(Kind::Title)) {
                let why = cell
                    .iter()
                    .find_map(|o| o.caption.strip_prefix(NOT_CAPTURED))
                    .unwrap_or("no title row");
                flaw(Fault::NoTitle(why.to_string()));
            }
        } else if who.chipless().is_none() || !beside_its_own_line(&r.caption, scene) {
            let why = caption_word(&r.caption, "why").unwrap_or_else(|| NO_LINE.to_string());
            flaw(Fault::NoChip(why));
        }
    }
    flaws
}

/// Accept native ownership, including a blank pad. Older capture manifests retain their
/// measured `not-drawn` and Off `switch-only` evidence.
fn beside_its_own_line(caption: &str, scene: Scene) -> bool {
    let pad = caption
        .split_once(PAD_SAID)
        .map(|(_, rest)| rest.trim_start());
    let own = pad.is_some_and(|p| p.starts_with('"') && !p.starts_with("\"\""));
    match caption_word(caption, "why").as_deref() {
        Some("no-footer") => true,
        Some("not-drawn") => own,
        Some("switch-only") => scene == Scene::Off,
        _ => false,
    }
}

/// The rows of a scene its plan skips.
fn skip_row(who: Who, scene: Scene, why: &str, look: Look, scale: u8) -> Row {
    Row::new(who, scene, look, scale, "-", format!("{SKIPPED}{why}"))
}

/// The row of a scene that could not be shown, and why.
fn uncaptured(who: Who, scene: Scene, why: &str, look: Look, scale: u8) -> Row {
    Row::new(who, scene, look, scale, "-", format!("{NOT_CAPTURED}{why}"))
}

/// What the verb was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    whos: Vec<Who>,
    scale: u8,
    looks: Vec<Look>,
    out: Option<PathBuf>,
    cols: u16,
    lines: u16,
    /// The typing take's other aterm.
    baseline: Option<String>,
    /// The typing take alone, on this process's aterm (the baseline's arm).
    typing_only: bool,
    /// The typing pair's program: codex-alab (`--typing codex-alab`), else agy.
    typing_codex: bool,
    /// The sheets alone, drawn again from the folder's manifest.
    sheets_only: bool,
}

/// The grid's columns: wide enough that the rule's slot holds a chip's whole title (its
/// `TitleFit::Full`), as v5's closing captures were; 240 left Cruise's two needs out of it.
const COLS: u16 = 400;
const LINES: u16 = 40;

const HELP: &str = "usage: targo --unverified lab autopilot-captures --harness all|claude|agy|muse|codex-alab
       [--scale 1|2] [--look day|night|both] [--out DIR] [--columns N] [--lines N] [--baseline ATERM]
       [--typing agy|codex-alab]
   or: targo --unverified lab autopilot-captures --baseline ATERM [--typing agy|codex-alab] [--out DIR]
   or: targo --unverified lab autopilot-captures --sheets [--out DIR]

Autopilot's scenes on every harness, side by side (design §14.2's 2b), on one private headless
instance per harness and look, launched in that look (its aterm.toml's theme), so every program
starts on the ground it is shot on. Claude Code, agy and Muse Code run the forever lab's
chains against its stub: cruise-needs (cruise-n's two needs), land-open (land-b's open hold),
land-repo (land-c's repository hold), landed (land-e's landing, its Done for you row standing),
off, and same-land (the same-reply hold, the stop said); beside same-land's, land-open's and
land-repo's shots the Overview, Settings ▸ Harness turned to its row (the state line) and the
Messages rows it raised; Claude Code also switch-asked (switch-over-held's repeat under Cruise,
switched to Land by the person's press on Settings ▸ Harness, shot while Land's one ask stands,
its shots beside it as same-land's), which agy and Muse Code name a skip. Claude Code runs as an
owner's managed session does (`lab claude --own`): on the default home's own claude.ai login with
aterm's usage tap on its line and a roster naming it `main` (its lab up with LAB_ROUTE_SEAM=1),
the stub reporting the week 80% used, resetting 41 h after the launch, on every reply, so every
scene shows the whole glass, line E's quota row included, in its palette (under Off the week's
reading alone, as Off's recovery refuses forecasts); each is shot once line E's Account node
says that row's projection, or Off's reading (on an aterm whose chrome reports no Account node,
a release build or an older one under LAB_ATERM, at once, its caption saying line E was not
judged). codex-alab runs codex-alab-proof's legs against its fake model: cruise, land, landed,
off and need; its holds are named skips. codex-alab needs the package codex-alab-proof would
pick.

After each scene settles it takes an image at rest (<harness>-<scene>-<look>-<scale>x-rest.png),
then points at the chip and takes its hovered title (…-title.png, the rule's band beside it as
…-title-band.png) where a chip is drawn, each png with a .txt of the pointer, `status`, `chrome`
and the screen's text; codex-alab's rest shots also cut its pad row at full scale
(…-rest-pad-band.png). A title the rule drew shorter than chrome's whole title is a FINDING.
manifest.tsv (harness, scene, look, scale, png, caption or the skip's reason) merges each run's
rows under a lock, so one folder holds 1x and 2x, Day and Night, from runs side by side. --scale is
the instance's (default 1); --columns and --lines its grid (400 x 40, so the rule's slot holds a
whole title). --out defaults to $LAB_STATE/autopilot-captures.

--baseline ATERM also takes one paired agy typing take after the captures, or alone without
--harness (200 keys, then 200 backspaces, under one `video`, harness `continue = false`, its Cruise
chip drawn, then idle 20 s): on ATERM first, then on this lab's aterm, back to back, the same lab
binary, agy, stub, keys and pace (typing-agy.txt). --typing codex-alab takes it on codex-alab instead, in codex-alab-proof's `off`
leg under Autopilot Off at rest, each take then idle 20 s, four takes in the order baseline, this
tree, this tree, baseline, so neither arm always runs second; each line names its slot
(typing-codex-alab.txt). Each take also reads aterm's own counters (proc_pid_rusage): its CPU
time and retired instructions from the burst's metrics reset to its percentiles read, per
rendered frame (cpu_ms, cpu_us_per_render, kinsn_per_render), its main thread's CPU time and
instructions a frame (main_us_per_render, main_kinsn_per_render: the headless frame's thread,
apart from the recording's conversion), and its CPU time across the idle wait (idle_cpu_ms);
other sessions' load moves these far less than a frame's wall time. Take it on a quiet machine.

Each harness also gets <harness>-<scale>x-sheet.png (its frames at rest, a scene a row, Day then
Night), <harness>-<scale>x-titles.png (its hovered titles' bands, stacked) and, for codex-alab,
<harness>-<scale>x-pads.png (its pad bands, stacked at full scale), drawn from the folder's
manifest; --sheets draws them again and runs nothing.

Exit: 0 every row there, no fault and the typing take taken where asked; 1 a gap, a fault or a
take not taken; 2 usage; 3 a harness could not run.
";

fn parse_args(args: &[String]) -> Result<Option<Asked>, Fail> {
    let usage = |m: &str| Fail::usage(format!("autopilot-captures: {m}"));
    let mut a = Asked {
        whos: Vec::new(),
        scale: 1,
        looks: Look::BOTH.to_vec(),
        out: None,
        cols: COLS,
        lines: LINES,
        baseline: None,
        typing_only: false,
        typing_codex: false,
        sheets_only: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |what: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| usage(&format!("{arg} needs {what}")))
        };
        match arg.as_str() {
            "--harness" => {
                let w = value("a word")?;
                a.whos = if w == "all" {
                    Who::ALL.to_vec()
                } else {
                    vec![Who::of(&w).ok_or_else(|| {
                        usage(&format!(
                            "unknown harness '{w}' (all, claude, agy, muse, codex-alab)"
                        ))
                    })?]
                };
            }
            "--scale" => {
                a.scale = match value("1 or 2")?.as_str() {
                    "1" => 1,
                    "2" => 2,
                    w => return Err(usage(&format!("--scale takes 1 or 2, not '{w}'"))),
                };
            }
            "--look" => {
                let w = value("day, night or both")?;
                a.looks = Look::of(&w)
                    .ok_or_else(|| usage(&format!("--look takes day, night or both, not '{w}'")))?;
            }
            "--out" => {
                a.out = Some(PathBuf::from(crate::paths::from_caller(&value(
                    "a folder",
                )?)))
            }
            "--columns" | "--lines" => {
                let n: u16 = value("a number")?
                    .parse()
                    .ok()
                    .filter(|n| (20..=500).contains(n))
                    .ok_or_else(|| usage(&format!("{arg} takes 20 to 500")))?;
                if arg == "--columns" {
                    a.cols = n;
                } else {
                    a.lines = n;
                }
            }
            "--baseline" => a.baseline = Some(crate::paths::from_caller(&value("an aterm")?)),
            "--typing-only" => a.typing_only = true,
            "--typing" => {
                a.typing_codex = match value("agy or codex-alab")?.as_str() {
                    "agy" => false,
                    "codex-alab" => true,
                    other => {
                        return Err(usage(&format!(
                            "--typing takes agy or codex-alab, not '{other}'"
                        )));
                    }
                }
            }
            "--sheets" => a.sheets_only = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(usage(&format!("unknown option '{other}'"))),
        }
    }
    if a.whos.is_empty() && !a.typing_only && !a.sheets_only && a.baseline.is_none() {
        return Err(usage(
            "say --harness all|claude|agy|muse|codex-alab, or --baseline ATERM",
        ));
    }
    Ok(Some(a))
}

/// `autopilot-captures --harness all|claude|agy|muse|codex-alab [--scale 1|2] [--look
/// day|night|both] [--out DIR] [--columns N] [--lines N] [--baseline ATERM]`.
/// # Errors
/// Usage, a folder that could not be made, or an interrupt.
pub fn cmd_autopilot_captures(args: &[String]) -> Res {
    let Some(a) = parse_args(args)? else {
        print!("{HELP}");
        return Ok(0);
    };
    stop::install();
    if a.typing_only {
        let line = typing_take(&a)?;
        println!("{line}");
        return Ok(0);
    }
    let out = match &a.out {
        Some(o) => o.clone(),
        None => crate::paths::paths()
            .need_state()
            .map_err(Fail::die)?
            .join("autopilot-captures"),
    };
    std::fs::create_dir_all(&out)
        .map_err(|e| Fail::die(format!("autopilot-captures: {}: {e}", out.display())))?;
    let out = crate::paths::physical(&out);
    if a.sheets_only {
        let rows = parse_manifest(&crate::util::scratch_text(&out.join("manifest.tsv")));
        for who in Who::ALL {
            for scale in [1u8, 2] {
                for name in sheets(&out, who, scale, &rows) {
                    println!("autopilot-captures: {}", out.join(name).display());
                }
            }
        }
        return Ok(0);
    }
    if a.whos.is_empty() {
        let base = a.baseline.as_deref().unwrap_or_default();
        let taken = if a.typing_codex {
            codex_typing_pair(base, &out)?
        } else {
            typing_pair(&a, base, &out)?
        };
        return Ok(if taken { 0 } else { 1 });
    }
    println!(
        "autopilot-captures: {} at {}x in {}, each launched in its look -> {}",
        a.whos
            .iter()
            .map(|w| w.name())
            .collect::<Vec<_>>()
            .join(", "),
        a.scale,
        a.looks
            .iter()
            .map(|l| l.theme())
            .collect::<Vec<_>>()
            .join(" and "),
        out.display()
    );
    let mut rows = Vec::new();
    let mut could_not: Vec<&str> = Vec::new();
    for &who in &a.whos {
        for &look in &a.looks {
            let mut mine = Vec::new();
            for &(scene, plan) in who.scenes() {
                if let Plan::Skip(why) = plan {
                    mine.push(skip_row(who, scene, why, look, a.scale));
                }
            }
            let shot = match who {
                Who::Agent(h) => agent_scenes(who, h, look, &a, &out, &mut mine),
                Who::CodexAlab => codex_scenes(look, &a, &out, &mut mine),
            };
            if let Err(f) = shot {
                if f.stopped.is_some() {
                    return Err(f);
                }
                let why = f.msg.unwrap_or_default();
                println!(
                    "autopilot-captures: {} in {} could not run: {why}",
                    who.name(),
                    look.theme()
                );
                if f.code == 3 && !could_not.contains(&who.name()) {
                    could_not.push(who.name());
                }
                for &(scene, plan) in who.scenes() {
                    let shown = mine.iter().any(|r| r.scene == scene.name());
                    if !shown && !matches!(plan, Plan::Skip(_)) {
                        mine.push(uncaptured(who, scene, &why, look, a.scale));
                    }
                }
            }
            rows.extend(mine);
        }
    }
    rows.sort_by_key(Row::order);
    let drawn = merge_manifest(&out, &a, &rows)?;
    let typed = match &a.baseline {
        Some(base) if a.typing_codex => codex_typing_pair(base, &out)?,
        Some(base) => typing_pair(&a, base, &out)?,
        None => {
            println!(
                "autopilot-captures: no typing take (--baseline ATERM pairs one with this tree's)"
            );
            true
        }
    };
    let gaps = missing(&rows, &a.whos, &a.looks, &[a.scale]);
    let flaws = faults(&rows);
    println!(
        "autopilot-captures: the manifest ({})",
        out.join("manifest.tsv").display()
    );
    for r in &rows {
        println!(
            "  {:<10} {:<12} {:<5} {}x {} {}",
            r.harness,
            r.scene,
            r.look,
            r.scale,
            r.png,
            crate::util::one_line(&r.caption, 160)
        );
    }
    for &who in &a.whos {
        let mine = |kind: Kind| {
            rows.iter()
                .filter(|r| r.harness == who.name() && r.kind() == Some(kind))
                .count()
        };
        println!(
            "autopilot-captures: {}: {} png(s) at rest, {} with the chip's title hovered; {}",
            who.name(),
            mine(Kind::Rest),
            mine(Kind::Title),
            drawn
                .iter()
                .filter(|n| n.starts_with(&format!("{}-", who.name())))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for g in &gaps {
        println!("autopilot-captures: GAP {g}");
    }
    for f in &flaws {
        println!("autopilot-captures: FAULT {f}");
    }
    println!(
        "autopilot-captures: {} row(s), {} gap(s), {} fault(s){}{}",
        rows.len(),
        gaps.len(),
        flaws.len(),
        if typed {
            ""
        } else {
            ", the typing take NOT TAKEN"
        },
        if could_not.is_empty() {
            String::new()
        } else {
            format!(", could not run: {}", could_not.join(", "))
        }
    );
    Ok(if !could_not.is_empty() {
        3
    } else if gaps.is_empty() && flaws.is_empty() && typed {
        0
    } else {
        1
    })
}

/// `rows` merged into `<out>/manifest.tsv` in place of the rows of this run's harnesses, looks
/// and scale, and the sheets of this run's harnesses at its scale drawn again from the merged
/// rows, all under the folder's lock (`manifest.lock`), so runs side by side (Day and Night, 1x
/// and 2x) keep each other's rows. The sheets' names.
fn merge_manifest(out: &Path, a: &Asked, rows: &[Row]) -> Result<Vec<String>, Fail> {
    let _held =
        crate::aos_learn::lock_within(&out.join("manifest.lock"), Duration::from_secs(60), &|d| {
            stop::sleep(d).map_err(|_| "interrupted".to_string())
        })
        .map_err(|e| Fail::die(format!("autopilot-captures: {e}")))?;
    let path = out.join("manifest.tsv");
    let kept: Vec<Row> = parse_manifest(&crate::util::scratch_text(&path))
        .into_iter()
        .filter(|r| {
            !(r.scale == a.scale.to_string()
                && a.looks.iter().any(|l| l.name() == r.look)
                && a.whos.iter().any(|w| w.name() == r.harness))
        })
        .collect();
    let all: Vec<Row> = kept.into_iter().chain(rows.iter().cloned()).collect();
    crate::sys::put(
        &path,
        0o644,
        &crate::sys::PutSrc::Bytes(manifest_text(&all).as_bytes()),
    )
    .map_err(|e| Fail::die(format!("autopilot-captures: {}: {e}", path.display())))?;
    Ok(a.whos
        .iter()
        .flat_map(|&who| sheets(out, who, a.scale, &all))
        .collect())
}

/// One lab's control channel, as the shots read it.
trait Lab {
    /// `ctl <args>`'s standard output, else its standard error (an `ERR` line).
    fn said(&self, args: &[&str]) -> String;
    /// Where `ctl image <name>` writes.
    fn images(&self) -> PathBuf;
    /// The instance's aterm process, whose counters a typing take reads ([`spent`]); `None`
    /// where it is not known.
    fn aterm_pid(&self) -> Option<i32>;
}

impl Lab for Door<'_> {
    fn said(&self, args: &[&str]) -> String {
        match self.ctl(args) {
            Ok(r) if r.stdout.trim().is_empty() => r.stderr,
            Ok(r) => r.stdout,
            Err(e) => format!("ERR {e}"),
        }
    }

    fn images(&self) -> PathBuf {
        crate::paths::physical(Path::new(&self.run().sockdir)).join("images")
    }

    /// A headless lab's instance pid is aterm's own; a windowed one's may be its keeper's, so
    /// it is not read.
    fn aterm_pid(&self) -> Option<i32> {
        (!crate::lab::windowed(self.run())).then_some(self.run().instance_pid)
    }
}

impl Lab for Proof {
    fn said(&self, args: &[&str]) -> String {
        let r = self.rig_ctl(args);
        if r.stdout.trim().is_empty() {
            r.stderr
        } else {
            r.stdout
        }
    }

    fn images(&self) -> PathBuf {
        crate::paths::physical(&self.images_dir())
    }

    fn aterm_pid(&self) -> Option<i32> {
        self.instance_aterm_pid()
    }
}

fn on(lab: &dyn Lab, sid: &str, args: &[&str]) -> String {
    let at = format!("@{sid}");
    let mut v = vec![at.as_str()];
    v.extend_from_slice(args);
    lab.said(&v)
}

/// A scene in a tab of its own, waiting to settle.
struct Tab {
    scene: Scene,
    sid: String,
    /// When its first prompt went in.
    t0: Instant,
    /// Its hang detector.
    limit: Duration,
    /// When it first read as settled (a scene settles twice in a row before it is shot).
    seen: Option<Instant>,
    done: bool,
    /// Its opening is done (codex-alab's Off leg: Off set once codex-alab holds its claim).
    armed: bool,
}

/// How long a scene must read as settled before it is shot: its report's last row drawn.
const SETTLED_FOR: Duration = Duration::from_secs(3);

/// The four supervised harnesses' scenes in `look`, in one private headless lab of `h`'s own
/// launched in it.
fn agent_scenes(
    who: Who,
    h: Harness,
    look: Look,
    a: &Asked,
    out: &Path,
    rows: &mut Vec<Row>,
) -> Result<(), Fail> {
    let tag = format!("apcap-{}-{}-{}x", h.name(), look.name(), a.scale);
    clear_lab(&tag)?;
    let u = up_opts(crate::lab::UpOpts::from_env(), h, look, a);
    if let Some(why) = crate::lab::opts_refused(&u) {
        return Err(Fail::usage(format!("autopilot-captures: {why}")));
    }
    crate::lab::up(&tag, &u).map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            Fail::exit(
                3,
                format!("the lab did not come up: {}", f.msg.unwrap_or_default()),
            )
        }
    })?;
    let shot = agent_lab(&tag, (who, h, look), a, out, rows);
    let _ = crate::lab::down(&tag, false);
    shot
}

/// The lab `h`'s scenes in `look` come up in: `env`'s (`LAB_*`) with the trust and the lifeline,
/// the verb's grid and scale, and `look`'s theme. Claude Code's also admits the stub's base URL
/// as its default route (`LAB_ROUTE_SEAM=1`), which its own login needs ([`scene_launcher`]).
fn up_opts(env: crate::lab::UpOpts, h: Harness, look: Look, a: &Asked) -> crate::lab::UpOpts {
    let mut u = env;
    u.trust = true;
    u.lifeline = true;
    u.cols = a.cols.to_string();
    u.lines = a.lines.to_string();
    u.scale = Some(a.scale.to_string());
    u.aterm_toml = themed(&u.aterm_toml, look.theme());
    u.route_seam |= h == Harness::Claude;
    u
}

/// Claude Code's week as its scenes' stub reports it on every reply
/// (`anthropic-ratelimit-unified-7d-*`): its utilization, and its reset this many seconds after
/// the launch. 80% used 127 h into the week runs out about 32 h on at the week's own pace, 9 h
/// before its reset, so line E lays every part of its chart (the measured step, `▕`, the projected
/// line, `╳` and `↺`) and its tail says the gap in the warn ink: one row that judges each of the
/// glass's tokens under each palette (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §7 step 5).
/// Under Autopilot Off, whose recovery refuses forecasts, the row lays the reading alone: the
/// step, `▕`, `┄` ahead and the reset ([`line_e_due`]).
pub const WEEK: (&str, i64) = ("0.80", 41 * 3600);

/// Claude Code's 5-hour window as its scenes' stub reports it: its utilization and its reset
/// after the launch, half-used at its reset at the window's own pace, so it walls nothing.
pub const FIVE: (&str, i64) = ("0.20", 3 * 3600);

/// The script every scene's tab of `h` runs. Claude Code's runs as an owner's managed session
/// does: on the default home's own claude.ai login with aterm's usage tap on its line and a roster
/// naming it (`lab claude --own`, [`crate::lab::ready_own`]), the scenes' folders trusted in that
/// home's profile, and its stub reporting [`FIVE`] and [`WEEK`] from `now_s` (Unix seconds) on
/// every reply, so the glass claims Claude Code's statusLine row as line E and every scene shows
/// the whole glass in its palette. On the forever lab's own launch (an API key) Claude Code draws
/// no statusLine row and its footer names no account, so line E never showed. agy's and Muse
/// Code's are the forever lab's.
/// # Errors
/// The run cannot take an own login ([`crate::lab::own_refused`]: a usage error), or a file
/// could not be written.
fn scene_launcher(
    run: &mut Run,
    h: Harness,
    dirs: &[PathBuf],
    now_s: i64,
) -> Result<PathBuf, Fail> {
    if h != Harness::Claude {
        return forever::launcher(run, h, dirs, false);
    }
    let launch = crate::lab::ready_own(run)?;
    forever::trust_for_claude(run, dirs)?;
    crate::verbs::write_headers(
        &run.w,
        crate::stub::chosen_resets((FIVE.0, now_s + FIVE.1), (WEEK.0, now_s + WEEK.1)),
        true,
    )?;
    forever::Launcher::Claude(launch).shared(run)
}

/// Lab `tag` cleared for a capture run to bring up: a lab of that name a run that died left is
/// taken down; one whose aterm still runs is another capture run's (another aterm under
/// `LAB_ATERM`, the same harness, look and scale, or the same typing arm), and taking it down
/// would end that run's scenes without a word, so this run refuses instead ([`still_up`]).
fn clear_lab(tag: &str) -> Result<(), Fail> {
    if crate::run::is_run(tag) {
        if let Some(why) = crate::run::Run::read(tag)
            .ok()
            .and_then(|run| still_up(&run, crate::sys::alive))
        {
            return Err(Fail::exit(3, why));
        }
        let _ = crate::lab::down(tag, false);
    }
    Ok(())
}

/// Why a capture run may not take lab `run` down before its own: its aterm still runs (`alive`
/// of its pid), so it is another capture run's, of another aterm under `LAB_ATERM` with the same
/// harness, look and scale, whose scenes a `down` would end without a word. `None` for a lab a
/// run that died left behind, which is taken down.
fn still_up(run: &crate::run::Run, alive: impl Fn(i32) -> bool) -> Option<String> {
    alive(run.instance_pid).then(|| {
        format!(
            "a lab named {} is up, its aterm (pid {}, {}) still running: another \
             autopilot-captures run of this harness, look and scale; it was left alone (wait for \
             it, or `aterm-lab down {}`)",
            run.tag, run.instance_pid, run.aterm, run.tag
        )
    })
}

fn agent_lab(
    tag: &str,
    (who, h, look): (Who, Harness, Look),
    a: &Asked,
    out: &Path,
    rows: &mut Vec<Row>,
) -> Result<(), Fail> {
    let mut run = Run::load(tag)?;
    let (bin, ver) = h.bin_ver(&run);
    let (bin, ver) = (bin.to_string(), ver.to_string());
    if bin.is_empty() || !crate::sys::executable(&bin) {
        return Err(Fail::exit(
            3,
            format!(
                "no {} in lab '{tag}' (LAB_{} at up)",
                h.name(),
                h.name().to_ascii_uppercase()
            ),
        ));
    }
    crate::procs::need_witness(&run).map_err(|e| Fail::exit(3, e))?;
    run.agent = h.name().into();
    run.save();
    let planned: Vec<(Scene, Chain)> = who
        .scenes()
        .iter()
        .filter_map(|&(s, p)| match p {
            Plan::Chain(c) => Some((s, c)),
            _ => None,
        })
        .collect();
    let chains: Vec<Chain> = planned.iter().map(|&(_, c)| c).collect();
    let mut dirs = Vec::new();
    for &c in &chains {
        dirs.push(
            forever::lay_repo(&run, c).map_err(|e| Fail::exit(3, format!("{}: {e}", c.name())))?,
        );
    }
    let script = scene_launcher(&mut run, h, &dirs, crate::util::now_secs())?;
    crate::verbs::set_custom_mode(&run, forever::MODE, &forever::spec_on(&chains, h, &dirs))?;
    let door = Door::new(&run)?;
    println!(
        "autopilot-captures: {} {ver} ({bin}) in lab '{tag}', aterm {}, {}x{} at {}x, launched in {}",
        h.name(),
        run.aterm,
        a.cols,
        a.lines,
        a.scale,
        look.theme()
    );
    let mut tabs = Vec::new();
    for (&(scene, chain), dir) in planned.iter().zip(&dirs) {
        let sid = forever::spawn_tab(&door, dir)?;
        let titled = forever::on(&door, &sid, &["meta", "set", "title", chain.name()]);
        let said = forever::launch_tab(&door, &sid, chain, &script)?.said;
        println!(
            "  tab  {} @{sid} ({}, titled: {}): {said}, launched",
            scene.name(),
            chain.name(),
            titled.trim()
        );
        tabs.push((
            chain,
            Tab {
                scene,
                sid,
                t0: Instant::now(),
                limit: Duration::from_millis(u64::try_from(chain.limit_ms()).unwrap_or(0)),
                seen: None,
                done: false,
                armed: true,
            },
        ));
    }
    for (chain, tab) in &tabs {
        if let Some(why) = forever::empty_composer(&door, &tab.sid, h.name())?.why(h.name()) {
            return Err(Fail::exit(3, format!("{why}, in {}", chain.name())));
        }
    }
    for (chain, tab) in &mut tabs {
        stop::sleep(Duration::from_secs(1))?;
        if !forever::tab_turn(&door, &tab.sid, h.name(), &chain.prompt())? {
            return Err(Fail::exit(
                3,
                format!(
                    "the composer of {} did not take the first prompt",
                    chain.name()
                ),
            ));
        }
        tab.t0 = Instant::now();
        println!(
            "  prompt {} @{}: \"{}\"",
            tab.scene.name(),
            tab.sid,
            chain.prompt()
        );
    }
    let head = format!("{} {ver}", h.name());
    let shots = Shots {
        who,
        look,
        head: &head,
        out,
        a,
    };
    let drive = run.state().join("drive");
    while tabs.iter().any(|(_, t)| !t.done) {
        for (chain, tab) in tabs.iter_mut().filter(|(_, t)| !t.done) {
            let settled = agent_settled(&door, &drive, tab.scene, &tab.sid, *chain);
            if let Some(why) = settle(tab, settled) {
                let why = format!(
                    "{why}; it read {}",
                    seen_words(&forever::on(&door, &tab.sid, &["status"]), &[])
                );
                rows.push(uncaptured(who, tab.scene, &why, look, a.scale));
                println!("  {} {}: NOT CAPTURED: {why}", h.name(), tab.scene.name());
                continue;
            }
            if tab.done {
                rows.extend(shots.scene(&door, tab.scene, &tab.sid, chain.name())?);
                if matches!(
                    tab.scene,
                    Scene::SameLand
                        | Scene::SwitchAsked
                        | Scene::Landed
                        | Scene::LandOpen
                        | Scene::LandRepo
                ) {
                    shots.held_extras(&door, tab.scene, &tab.sid, chain.name())?;
                }
            }
        }
        stop::sleep(Duration::from_secs(2))?;
    }
    Ok(())
}

/// Whether `scene`'s tab `sid` stands where its capture wants it, read off its `status` and screen.
fn agent_settled(door: &Door<'_>, drive: &Path, scene: Scene, sid: &str, chain: Chain) -> bool {
    let st = forever::on(door, sid, &["status"]);
    let w = |k: &str| forever::word(&st, k).unwrap_or_default();
    let idle = w("agent") == "idle";
    let held = |leg: HoldLeg| w("land_held").split(',').any(|l| l == leg.word());
    match scene {
        Scene::CruiseNeeds => idle && w("needs_n") == "2",
        Scene::LandOpen => idle && held(HoldLeg::Open),
        Scene::LandRepo => idle && held(HoldLeg::Repo),
        // Stopped: the same-reply hold standing (3a's `status held=`).
        Scene::SameLand => idle && w("held") == HeldBy::SameReply.word(),
        // Its replies repeated under Cruise (Cruise's recovery wait since the continuous-forward
        // design of 2026-11-01, or an older build's hold: the session's journal,
        // [`forever::switch::repeat_journaled`]), the person's switch to Land is made here, their
        // press on Settings ▸ Harness's row (no `meta set`, which is no person's and asks
        // nothing); shot while Land's one ask stands (`status land_asked=switch`), its answer
        // slow.
        Scene::SwitchAsked => {
            if idle
                && w("autopilot") == "cruise"
                && forever::switch::repeat_journaled(&crate::util::scratch_text(
                    &drive.join(format!("{sid}.journal.jsonl")),
                ))
            {
                let _ = forever::surfaces::press_word(door, None, sid, "land");
            }
            w("land_asked") == "switch"
        }
        Scene::Landed => w("task") == "landed",
        Scene::Off => {
            idle && w("autopilot") == chain.autopilot()
                && forever::on(door, sid, &["text"]).contains(forever::DONE_CLAIM)
        }
        Scene::Cruise | Scene::Land | Scene::Need => false,
    }
}

/// One look at a waiting tab: marks it done once it read as settled [`SETTLED_FOR`] (`settled`
/// this look), or gives why it never settled within its hang detector.
fn settle(tab: &mut Tab, settled: bool) -> Option<String> {
    if settled {
        let since = *tab.seen.get_or_insert_with(Instant::now);
        if since.elapsed() >= SETTLED_FOR {
            tab.done = true;
        }
        return None;
    }
    tab.seen = None;
    if tab.t0.elapsed() > tab.limit {
        tab.done = true;
        return Some(format!(
            "it did not settle within {} s of its prompt (the hang detector)",
            tab.limit.as_secs()
        ));
    }
    None
}

/// What a tab that never settled read at its hang detector: its `status` words a scene is told
/// by, and the acts codex-alab typed.
fn seen_words(status: &str, acts: &[String]) -> String {
    let words: Vec<String> = [
        "agent",
        "autopilot",
        "task",
        "needs_n",
        "needs",
        "land_held",
        "held",
        "land_asked",
    ]
    .iter()
    .filter_map(|k| forever::word(status, k).map(|v| format!("{k}={v}")))
    .collect();
    let acts = if acts.is_empty() {
        String::new()
    } else {
        format!(", acts {}", acts.join(" "))
    };
    format!("status {}{acts}", words.join(" "))
}

/// codex-alab's scenes in `look`, on `codex-alab-proof`'s rig launched in it.
fn codex_scenes(look: Look, a: &Asked, out: &Path, rows: &mut Vec<Row>) -> Result<(), Fail> {
    let args = [
        "--columns".to_string(),
        a.cols.to_string(),
        "--lines".to_string(),
        a.lines.to_string(),
        "--scale".to_string(),
        a.scale.to_string(),
    ]
    .to_vec();
    let mut rig = Proof::captures_rig(None, args, look.theme())?;
    let shot = codex_rig_scenes(&mut rig, look, a, out, rows);
    rig.finish();
    shot
}

/// How long a codex-alab leg may take to reach its scene: its back-off (2 min) and the model's
/// slowest turn (130 s) before Land's instruction or its landing, with room.
const CODEX_LIMIT: Duration = Duration::from_secs(8 * 60);

fn codex_rig_scenes(
    rig: &mut Proof,
    look: Look,
    a: &Asked,
    out: &Path,
    rows: &mut Vec<Row>,
) -> Result<(), Fail> {
    let who = Who::CodexAlab;
    let head = format!("codex-alab {}", rig.package_version());
    println!(
        "autopilot-captures: {head} on codex-alab-proof's rig, {}x{} at {}x, launched in {}",
        a.cols,
        a.lines,
        a.scale,
        look.theme()
    );
    let mut tabs = Vec::new();
    for &(scene, plan) in who.scenes() {
        let Plan::Leg(leg) = plan else { continue };
        let sid = rig.open_scene(leg)?;
        let titled = rig.on(&sid, &["meta", "set", "title", leg]).first();
        println!("  tab  {} @{sid} ({leg}): {titled}", scene.name());
        tabs.push((
            leg,
            Tab {
                scene,
                sid,
                t0: Instant::now(),
                limit: CODEX_LIMIT,
                seen: None,
                done: false,
                armed: scene != Scene::Off,
            },
        ));
    }
    let shots = Shots {
        who,
        look,
        head: &head,
        out,
        a,
    };
    while tabs.iter().any(|(_, t)| !t.done) {
        for (leg, tab) in tabs.iter_mut().filter(|(_, t)| !t.done) {
            if !tab.armed && rig.proven_pid(&tab.sid).is_some() {
                // As the proof does: Off once codex-alab holds its claim, while the first turn
                // (130 s of real work) runs.
                let off = rig.on(&tab.sid, &["meta", "set", "autopilot", "off"]);
                println!(
                    "  {leg}: Autopilot Off while its first turn runs ({})",
                    off.first()
                );
                tab.armed = true;
            }
            let settled = tab.armed && codex_settled(rig, tab.scene, &tab.sid);
            if let Some(why) = settle(tab, settled) {
                let why = format!(
                    "{why}; it read {}",
                    seen_words(&rig.status(&tab.sid), &rig.acts(&tab.sid))
                );
                rows.push(uncaptured(who, tab.scene, &why, look, a.scale));
                println!("  codex-alab {}: NOT CAPTURED: {why}", tab.scene.name());
                continue;
            }
            if tab.done {
                rows.extend(shots.scene(rig, tab.scene, &tab.sid, leg)?);
            }
        }
        stop::sleep(Duration::from_secs(2))?;
    }
    Ok(())
}

/// Whether codex-alab's `scene` tab `sid` stands where its capture wants it.
fn codex_settled(rig: &Proof, scene: Scene, sid: &str) -> bool {
    use crate::codex_alab_model as model;
    let st = rig.status(sid);
    let screen = || rig.on(sid, &["text"]).stdout;
    match scene {
        Scene::Cruise => rig.proven_pid(sid).is_some() && screen().contains(model::STEP_ONE),
        Scene::Land => rig.acts(sid).iter().any(|act| act == RULE_LAND),
        Scene::Landed => codex_alab_proof::task_face(&st) == "landed",
        Scene::Off => {
            forever::word(&st, "autopilot").as_deref() == Some("off")
                && rig.proven_pid(sid).is_none()
                && screen().contains(model::STEP_ONE)
        }
        Scene::Need => codex_alab_proof::needs_raised(&rig.on(sid, &["meta"]).first()),
        Scene::CruiseNeeds
        | Scene::LandOpen
        | Scene::LandRepo
        | Scene::SameLand
        | Scene::SwitchAsked => false,
    }
}

/// What one harness's shots in one look share.
struct Shots<'a> {
    who: Who,
    /// The look its instance was launched in.
    look: Look,
    /// `<program> <version>`, for the headers.
    head: &'a str,
    out: &'a Path,
    a: &'a Asked,
}

/// A frame decoded: RGBA, its width and height.
pub(crate) struct Frame {
    rgba: Vec<u8>,
    w: usize,
    h: usize,
}

impl Frame {
    fn read(png: &Path) -> Option<Frame> {
        let bytes = crate::util::read_regular_file(png).ok()?;
        let (rgba, w, h) = aterm_png::decode_rgba8(&bytes, 1 << 15)?;
        Some(Frame { rgba, w, h })
    }

    /// Its mean luminance, 0 (black) to 1 (white).
    pub(crate) fn luma(&self) -> f64 {
        let (px, _) = self.rgba.as_chunks::<4>();
        #[expect(
            clippy::cast_precision_loss,
            reason = "a frame's pixel count, exact in an f64"
        )]
        let n = px.len().max(1) as f64;
        let sum: f64 = px
            .iter()
            .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
            .sum();
        sum / n / 255.0
    }

    /// How many pixels of the box `(x0, y0)..(x1, y1)` differ from `other`'s, `None` where the
    /// two frames differ in size.
    fn changed(
        &self,
        other: &Frame,
        (x0, y0): (usize, usize),
        (x1, y1): (usize, usize),
    ) -> Option<usize> {
        if self.w != other.w || self.h != other.h {
            return None;
        }
        let mut n = 0;
        for y in y0..y1.min(self.h) {
            for x in x0..x1.min(self.w) {
                let i = (y * self.w + x) * 4;
                let d: u32 = (0..3)
                    .map(|k| u32::from(self.rgba[i + k].abs_diff(other.rgba[i + k])))
                    .sum();
                if d > 96 {
                    n += 1;
                }
            }
        }
        Some(n)
    }

    /// A `w` x `h` frame of one colour.
    fn blank(w: usize, h: usize, px: [u8; 4]) -> Frame {
        Frame {
            rgba: px.repeat(w * h),
            w,
            h,
        }
    }

    /// It shrunk `f` times each way, each pixel the mean of the `f` x `f` it stands for.
    fn shrink(&self, f: usize) -> Frame {
        let f = f.max(1);
        let (w, h) = (self.w / f, self.h / f);
        let mut out = Frame::blank(w, h, [0, 0, 0, 255]);
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0u32; 4];
                for dy in 0..f {
                    for dx in 0..f {
                        let i = ((y * f + dy) * self.w + x * f + dx) * 4;
                        for (k, s) in sum.iter_mut().enumerate() {
                            *s += u32::from(self.rgba[i + k]);
                        }
                    }
                }
                let o = (y * w + x) * 4;
                for (k, s) in sum.iter().enumerate() {
                    out.rgba[o + k] =
                        u8::try_from(s / u32::try_from(f * f).unwrap_or(1)).unwrap_or(u8::MAX);
                }
            }
        }
        out
    }

    /// `other` drawn into it with its top left at `(x, y)`, clipped to it.
    fn paste(&mut self, other: &Frame, (x, y): (usize, usize)) {
        for row in 0..other.h.min(self.h.saturating_sub(y)) {
            let n = other.w.min(self.w.saturating_sub(x)) * 4;
            let to = ((y + row) * self.w + x) * 4;
            let from = row * other.w * 4;
            self.rgba[to..to + n].copy_from_slice(&other.rgba[from..from + n]);
        }
    }

    /// The box `(x0, y0)..(x1, y1)` of it, clipped to it.
    fn crop(&self, (x0, y0): (usize, usize), (x1, y1): (usize, usize)) -> Frame {
        let (x0, y0) = (x0.min(self.w), y0.min(self.h));
        let (w, h) = (
            x1.min(self.w).saturating_sub(x0),
            y1.min(self.h).saturating_sub(y0),
        );
        let mut out = Frame::blank(w, h, [0, 0, 0, 255]);
        for row in 0..h {
            let from = ((y0 + row) * self.w + x0) * 4;
            out.rgba[row * w * 4..(row + 1) * w * 4]
                .copy_from_slice(&self.rgba[from..from + w * 4]);
        }
        out
    }

    /// Rows `y0..y1` of it, as a PNG.
    fn band_png(&self, y0: usize, y1: usize) -> Option<Vec<u8>> {
        let (y0, y1) = (y0.min(self.h), y1.min(self.h));
        if y1 <= y0 {
            return None;
        }
        let rows = &self.rgba[y0 * self.w * 4..y1 * self.w * 4];
        let mut png = Vec::new();
        let mut enc = aterm_png::Encoder::new(
            &mut png,
            u32::try_from(self.w).ok()?,
            u32::try_from(y1 - y0).ok()?,
        );
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rows).ok()?;
        writer.finish().ok()?;
        Some(png)
    }
}

/// How wide a sheet's cell is, at most, in pixels.
const SHEET_CELL: usize = 860;

/// The gap between a sheet's cells, and its colour.
const SHEET_GAP: (usize, [u8; 4]) = (8, [128, 128, 128, 255]);

/// A codex-alab rest shot's pad band ([`Shots::pad_band`]), after its png's stem.
const PAD_BAND: &str = "-pad-band.png";

/// `who`'s sheets in `out` from `rows` at `scale`, their names: its frames at rest as one grid
/// (`<harness>-<scale>x-sheet.png`, a scene a row, a look a column, each frame shrunk to at most
/// [`SHEET_CELL`] wide), its hovered titles' bands stacked in the same order
/// (`<harness>-<scale>x-titles.png`), and codex-alab's pad bands stacked at full scale, Day then
/// Night a scene (`<harness>-<scale>x-pads.png`).
fn sheets(out: &Path, who: Who, scale: u8, rows: &[Row]) -> Vec<String> {
    // Each of the harness's frames at `scale` of `kind`, read from the png its row names, or for
    // `band` the band of that name beside it: its scene, look and frame.
    let mine = |kind: Kind, band: Option<&str>| -> Vec<(Scene, Look, Frame)> {
        rows.iter()
            .filter(|r| r.kind() == Some(kind))
            .filter_map(|r| {
                let (w, scene, look, s) = r.keys()?;
                if w != who || s != scale {
                    return None;
                }
                let png = band.map_or_else(
                    || r.png.clone(),
                    |tail| file_name(w, scene, look, s, kind, tail),
                );
                Some((scene, look, Frame::read(&out.join(png))?))
            })
            .collect()
    };
    let (gap, grey) = SHEET_GAP;
    let mut made = Vec::new();
    let rests = mine(Kind::Rest, None);
    let scenes: Vec<Scene> = rests.iter().fold(Vec::new(), |mut v, (s, _, _)| {
        if !v.contains(s) {
            v.push(*s);
        }
        v
    });
    if let Some((_, _, first)) = rests.first() {
        let f = first.w.div_ceil(SHEET_CELL);
        let (cw, ch) = (first.w / f, first.h / f);
        let looks = Look::BOTH.len();
        let mut sheet = Frame::blank(
            looks * cw + (looks + 1) * gap,
            scenes.len() * ch + (scenes.len() + 1) * gap,
            grey,
        );
        for (scene, look, frame) in &rests {
            let row = scenes.iter().position(|s| s == scene).unwrap_or(0);
            let col = Look::BOTH.iter().position(|l| l == look).unwrap_or(0);
            sheet.paste(
                &frame.shrink(f),
                (gap + col * (cw + gap), gap + row * (ch + gap)),
            );
        }
        let name = format!("{}-{scale}x-sheet.png", who.name());
        if let Some(png) = sheet.band_png(0, sheet.h) {
            let _ = crate::sys::put(&out.join(&name), 0o644, &crate::sys::PutSrc::Bytes(&png));
            made.push(name);
        }
    }
    let bands = mine(Kind::Title, Some("-band.png"));
    if let Some((_, _, first)) = bands.first() {
        let f = usize::from(scale).max(1);
        let w = first.w / f;
        let h: usize = bands.iter().map(|(_, _, b)| b.h / f + gap).sum::<usize>() + gap;
        let mut stack = Frame::blank(w, h, grey);
        let mut y = gap;
        for (_, _, band) in &bands {
            let small = band.shrink(f);
            stack.paste(&small, (0, y));
            y += small.h + gap;
        }
        let name = format!("{}-{scale}x-titles.png", who.name());
        if let Some(png) = stack.band_png(0, stack.h) {
            let _ = crate::sys::put(&out.join(&name), 0o644, &crate::sys::PutSrc::Bytes(&png));
            made.push(name);
        }
    }
    let mut pads = mine(Kind::Rest, Some(PAD_BAND));
    pads.sort_by_key(|(scene, look, _)| {
        (scenes.iter().position(|s| s == scene), *look == Look::Night)
    });
    if !pads.is_empty() {
        let w = pads.iter().map(|(_, _, b)| b.w).max().unwrap_or(0) + 2 * gap;
        let h: usize = pads.iter().map(|(_, _, b)| b.h + gap).sum::<usize>() + gap;
        let mut stack = Frame::blank(w, h, grey);
        let mut y = gap;
        for (_, _, band) in &pads {
            stack.paste(band, (gap, y));
            y += band.h + gap;
        }
        let name = format!("{}-{scale}x-pads.png", who.name());
        if let Some(png) = stack.band_png(0, stack.h) {
            let _ = crate::sys::put(&out.join(&name), 0o644, &crate::sys::PutSrc::Bytes(&png));
            made.push(name);
        }
    }
    made
}

/// A frame coordinate as a pixel index, 0 left of or above the frame.
fn pixel(v: f64) -> usize {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a coordinate on the frame, clamped at 0"
    )]
    let p = v.max(0.0) as usize;
    p
}

/// A count of cells as an f64, for a frame coordinate.
fn cells_f(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Where the chip was found: its cell, and its centre and one cell's size in frame pixels.
#[derive(Clone, Copy, Debug)]
struct ChipAt {
    row: usize,
    col: usize,
    x: f64,
    y: f64,
    cw: f64,
    ch: f64,
}

/// How many cells in a row the rule may leave as they were inside a hovered title: a word's
/// blanks and an em dash drawn where the rule's line was (` — `).
const TITLE_GAP: usize = 3;

/// How many cells past the chip's a hovered title may begin: the rule keeps another chip
/// between them where one is drawn (the permission chip, Claude Code's mode, as `⏵⏵ auto`), and
/// the hover leaves it as it was.
const TITLE_LEAD_SKIP: usize = 24;

/// How many cells short of chrome's whole title a drawn title may measure and still be whole:
/// the chip's first cell found one off, and the blank the rule draws after the title. A leaner
/// form gives up a whole part, ` · ` and its words, more than this.
const TITLE_SLACK: usize = 2;

impl ChipAt {
    /// The share of the rule's band past the chip (20 to 70 cells on from it, one cell high) that
    /// differs between `hovered` and `rest`: a hovered title's glyphs, where one shows. The cursor
    /// pet follows the pointer onto the chip, short of the band.
    fn title_share(&self, hovered: &Frame, rest: &Frame) -> f64 {
        let (x0, x1) = (self.x + 20.0 * self.cw, self.x + 70.0 * self.cw);
        let (y0, y1) = (self.y - self.ch / 2.0, self.y + self.ch / 2.0);
        let changed = hovered
            .changed(rest, (pixel(x0), pixel(y0)), (pixel(x1), pixel(y1)))
            .unwrap_or(0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a band's pixel count, exact in an f64"
        )]
        let changed = changed as f64;
        changed / ((x1 - x0) * (y1 - y0)).max(1.0)
    }

    /// How many cells the hovered title takes in the rule: from its first cell that differs
    /// between `hovered` and `rest`, no nearer than `lead` cells past the chip's (where the rule
    /// draws a title, past the chip's cells and two blanks) and no further than
    /// [`TITLE_LEAD_SKIP`] past that, to the last that differs before more than [`TITLE_GAP`]
    /// cells in a row that do not. 0 where none differs.
    fn title_cells(&self, hovered: &Frame, rest: &Frame, lead: usize) -> usize {
        let (y0, y1) = (pixel(self.y - self.ch / 2.0), pixel(self.y + self.ch / 2.0));
        let left = |i: usize| pixel(self.x + (cells_f(i) - 0.5) * self.cw);
        let mut first: Option<usize> = None;
        let mut last: Option<usize> = None;
        let mut i = lead.saturating_sub(1);
        while left(i) < hovered.w {
            let changed = hovered
                .changed(rest, (left(i), y0), (left(i + 1), y1))
                .unwrap_or(0)
                > 0;
            if changed {
                first.get_or_insert(i);
                last = Some(i);
            } else if i > last.unwrap_or(lead + TITLE_LEAD_SKIP) + TITLE_GAP {
                break;
            }
            i += 1;
        }
        first
            .zip(last)
            .map_or(0, |(f, l)| (l + 1).saturating_sub(f.max(lead)))
    }
}

/// `pointer move`'s `px=X,Y`: the cell's centre, in window points.
fn px_of(reply: &str) -> Option<(f64, f64)> {
    let px = forever::word(reply, "px")?;
    let (x, y) = px.split_once(',')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

/// The rows of a screen's text that hold a rule (twelve `─` or more), bottom first.
fn rule_rows(text: &str) -> Vec<usize> {
    let rows: Vec<&str> = text.lines().collect();
    (0..rows.len())
        .rev()
        .filter(|&i| rows[i].chars().filter(|&c| c == '─').count() >= 12)
        .collect()
}

/// The columns the chip is looked for at on its rule: the column every supervised harness's is
/// drawn at (4), then its neighbours.
const CHIP_COLS: [usize; 5] = [4, 3, 5, 2, 6];

/// How much of the rule's band past the chip a hovered title changes, at the least: a title's
/// glyphs ink about a sixth of the cells they take.
pub const TITLE_INK: f64 = 0.04;

/// A title shot's measure of the rule's band against the shot at rest.
#[derive(Clone, Copy, Debug)]
struct Hovered {
    /// The share of the band past the chip that changed ([`ChipAt::title_share`]).
    share: f64,
    /// The cells the drawn title takes ([`ChipAt::title_cells`]).
    cells: usize,
}

/// One png's facts, for its caption.
struct Shot<'a> {
    scene: Scene,
    /// What the png shows of the pointer.
    kind: Kind,
    /// The tab's `autopilot` line in `chrome`.
    chip_line: &'a str,
    frame: &'a Frame,
    /// A title shot's measure.
    hovered: Option<Hovered>,
}

/// The words a caption carries where `chrome` printed no `autopilot` line for the tab.
const NO_LINE: &str = "no autopilot line in chrome";

impl Shots<'_> {
    /// A scene's shots in this look: its rows. A tab that is not in front, or a shot that gave
    /// no frame, is a NOT CAPTURED row naming why.
    fn scene(&self, lab: &dyn Lab, scene: Scene, sid: &str, label: &str) -> Result<Vec<Row>, Fail> {
        let (who, look, scale) = (self.who, self.look, self.a.scale);
        let row = |kind: Option<Kind>, caption: String| {
            let png =
                kind.map_or_else(|| "-".to_string(), |k| png_name(who, scene, look, scale, k));
            Row::new(who, scene, look, scale, &png, caption)
        };
        let raised = raise(lab, label);
        println!(
            "  {} {} ({}): settled; {}",
            who.name(),
            scene.name(),
            look.name(),
            match &raised {
                Ok(said) | Err(said) => said,
            }
        );
        if let Err(why) = raised {
            return Ok(vec![row(None, format!("{NOT_CAPTURED}{why}"))]);
        }
        let _ = lab.said(&["pointer", "leave"]);
        stop::sleep(Duration::from_secs(2))?;
        let undrawn = if who == Who::Agent(Harness::Claude) {
            let probe = format!("{}-{}-line-e-probe.png", who.name(), look.name());
            line_e_unsaid(lab, sid, LINE_E_HANG, || {
                stop::sleep(Duration::from_millis(500))?;
                let _ = self.shoot(lab, sid, &probe);
                let _ = std::fs::remove_file(self.out.join(&probe));
                Ok(())
            })?
        } else {
            None
        };
        let rest_png = png_name(who, scene, look, scale, Kind::Rest);
        let Some(rest) = self.shoot(lab, sid, &rest_png) else {
            return Ok(vec![row(
                None,
                format!("{NOT_CAPTURED}`image` gave no frame of @{sid}"),
            )]);
        };
        let chip_line = forever::surfaces::chip_of(&lab.said(&["chrome"]), sid).unwrap_or_default();
        self.sidecar(lab, sid, scene, Kind::Rest, "-");
        let mut caption = self.caption(
            lab,
            sid,
            &Shot {
                scene,
                kind: Kind::Rest,
                chip_line: &chip_line,
                frame: &rest,
                hovered: None,
            },
        );
        if let Some(unsaid) = &undrawn {
            caption.push_str(&unsaid.noted());
        }
        if who == Who::CodexAlab {
            self.pad_band(lab, sid, scene, &rest);
        }
        let Some(drawn) = drawn_of(&chip_line) else {
            return Ok(vec![row(Some(Kind::Rest), caption)]);
        };
        let Some(c) = self.find_chip(lab, sid, &rest)? else {
            caption.push_str(&format!(
                "; {FINDING}a chip is drawn, yet no cell of its rule showed a title when hovered"
            ));
            return Ok(vec![row(Some(Kind::Rest), caption)]);
        };
        let mut rows = vec![row(Some(Kind::Rest), caption)];
        let pointer = lab
            .said(&["pointer", "move", &c.row.to_string(), &c.col.to_string()])
            .trim()
            .to_string();
        stop::sleep(Duration::from_millis(1500))?;
        let title_png = png_name(who, scene, look, scale, Kind::Title);
        let shot = self.shoot(lab, sid, &title_png);
        if shot.is_some() {
            self.sidecar(lab, sid, scene, Kind::Title, &pointer);
        }
        let _ = lab.said(&["pointer", "leave"]);
        let Some(hovered) = shot else {
            rows.push(row(
                None,
                format!(
                    "{NOT_CAPTURED}`image` gave no frame of @{sid} with the pointer on the chip ({pointer})"
                ),
            ));
            return Ok(rows);
        };
        let measure = Hovered {
            share: c.title_share(&hovered, &rest),
            cells: c.title_cells(&hovered, &rest, text_width(&drawn) + 2),
        };
        if let Some(band) = hovered.band_png(pixel(c.y - 1.5 * c.ch), pixel(c.y + 1.5 * c.ch)) {
            let p = self
                .out
                .join(file_name(who, scene, look, scale, Kind::Title, "-band.png"));
            let _ = crate::sys::put(&p, 0o644, &crate::sys::PutSrc::Bytes(&band));
        }
        rows.push(row(
            Some(Kind::Title),
            self.caption(
                lab,
                sid,
                &Shot {
                    scene,
                    kind: Kind::Title,
                    chip_line: &chip_line,
                    frame: &hovered,
                    hovered: Some(measure),
                },
            ),
        ));
        Ok(rows)
    }

    /// THE STOP BESIDE ITS TAB (3a), THE HOLD'S QUOTE (4b), LAND'S ASK AT THE SWITCH OVER IT
    /// (10b), AND WHAT ATERM CHECKED AT A LANDING (9c): with `scene`'s tab in front (same-land's,
    /// land-open's, land-repo's, switch-over-held's, or land-e's landed), the Overview
    /// (`…-overview.png`) and Settings ▸ Harness turned to its row (`…-page.png`), each opened on
    /// the front window, shot with `image` once it shows the session ([`HELD_HANG`] the hang
    /// detector of each wait) and closed again, and `…-held.txt` with what they, the tab's hover,
    /// its `status`'s `landed_*` fields, its `statusmenu` rows and `ctl messages tag=harness`
    /// read: the stop's words, the hold's quote, the ask, or what aterm checked, on every surface a
    /// person reads them on. A surface that never showed the session is NOT CAPTURED by name; a
    /// sidecar that cannot be written fails the run.
    fn held_extras(&self, lab: &dyn Lab, scene: Scene, sid: &str, label: &str) -> Result<(), Fail> {
        let (who, look, scale) = (self.who, self.look, self.a.scale);
        let name = |tail: &str| {
            format!(
                "{}-{}-{}-{scale}x{tail}",
                who.name(),
                scene.name(),
                look.name()
            )
        };
        let mut said = vec![format!(
            "# {}: {} on {}, {}; sid={sid}; at {}",
            name("-held.txt"),
            scene.what(),
            self.head,
            look.theme(),
            crate::util::now_ms()
        )];
        let status = on(lab, sid, &["status"]);
        said.push(format!(
            "## status (slice 9c)\n{}",
            forever::checked::FIELDS
                .iter()
                .map(|k| format!(
                    "{k}={}",
                    forever::word(&status, k).unwrap_or_else(|| "?".into())
                ))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        let chrome = lab.said(&["chrome"]);
        let strip = forever::surfaces::strip_tabs(&chrome);
        let hover = strip
            .iter()
            .find(|t| t.label == label)
            .and_then(forever::surfaces::StripTab::driven)
            .unwrap_or_else(|| "-".into());
        said.push(format!("## the tab's hover\n{hover}"));
        said.push(format!(
            "## what a screen reader is handed (`chrome`'s `a11y` lines)\n{}",
            forever::surfaces::ax_of(&chrome, sid)
        ));
        said.push(format!(
            "## the menu bar's rows naming it, with their tips\n{}",
            chrome
                .lines()
                .filter(|l| l.starts_with("statusmenu ") && l.contains(label))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        let overview = name("-overview.png");
        let opened = lab.said(&["open", "overview"]);
        let listed = |read: &str| {
            read.lines().any(|l| {
                l.starts_with("overview chip ") && forever::word(l, "sid").as_deref() == Some(sid)
            })
        };
        let kept = if crate::wait_until(HELD_HANG, || listed(&lab.said(&["controls", "overview"])))
        {
            self.shoot_front(lab, &overview)
        } else {
            Err(format!(
                "`controls overview` listed no @{sid} within {} s",
                HELD_HANG.as_secs()
            ))
        };
        said.push(self.extra_said(
            scene,
            "overview",
            &opened,
            &kept,
            &lab.said(&["controls", "overview"]),
        ));
        let _ = lab.said(&["open", "overview", "close"]);
        let _ = raise(lab, label);
        let page = name("-page.png");
        let opened = lab.said(&["open", "app", "settings", "/harness"]);
        let kept = turn_to_row(lab, label).and_then(|_| self.shoot_front(lab, &page));
        said.push(self.extra_said(
            scene,
            "prefs",
            &opened,
            &kept,
            &lab.said(&["controls", "prefs"]),
        ));
        let _ = lab.said(&["pointer", "leave"]);
        let _ = lab.said(&["open", "prefs", "close"]);
        let _ = raise(lab, label);
        said.push(format!(
            "## ctl messages tag=harness\n{}",
            lab.said(&["messages", "tag=harness"]).trim_end()
        ));
        let p = self.out.join(name("-held.txt"));
        crate::sys::put(
            &p,
            0o644,
            &crate::sys::PutSrc::Bytes(format!("{}\n", said.join("\n")).as_bytes()),
        )
        .map_err(|e| Fail::exit(3, format!("cannot write {}: {e}", p.display())))
    }

    /// One extra's part of `…-held.txt`, and its line on the run's output: what opening it said,
    /// the png kept (or why none was) and what `controls <target>` read.
    fn extra_said(
        &self,
        scene: Scene,
        target: &str,
        opened: &str,
        kept: &Result<String, String>,
        read: &str,
    ) -> String {
        let kept = kept
            .as_ref()
            .map_or_else(|why| format!("{NOT_CAPTURED}{why}"), Clone::clone);
        println!(
            "  {} {} ({}): {target} {kept}",
            self.who.name(),
            scene.name(),
            self.look.name()
        );
        format!(
            "## {target}: {} -> {kept}\n{}",
            opened.trim(),
            read.trim_end()
        )
    }

    /// `image` of the front window, moved to `<out>/<file>`: the file, or why none was kept.
    fn shoot_front(&self, lab: &dyn Lab, file: &str) -> Result<String, String> {
        let r = lab.said(&["image", file]);
        let w: Vec<&str> = r.split_whitespace().collect();
        let src = (w.len() >= 4 && w[0] == "OK")
            .then(|| PathBuf::from(w[3]))
            .ok_or_else(|| format!("`image` said {:?}", r.trim()))?;
        if crate::paths::physical(&src) != lab.images().join(file) {
            return Err(format!("`image` wrote {} elsewhere", src.display()));
        }
        crate::sys::put(&self.out.join(file), 0o644, &crate::sys::PutSrc::File(&src))
            .map_err(|e| format!("cannot keep {}: {e}", src.display()))?;
        let _ = std::fs::remove_file(&src);
        Ok(file.to_string())
    }

    /// `image` of the window showing `sid`, moved to `<out>/<file>`: its frame.
    fn shoot(&self, lab: &dyn Lab, sid: &str, file: &str) -> Option<Frame> {
        let r = on(lab, sid, &["image", file]);
        let w: Vec<&str> = r.split_whitespace().collect();
        let src = (w.len() >= 4 && w[0] == "OK").then(|| PathBuf::from(w[3]))?;
        if crate::paths::physical(&src) != lab.images().join(file) {
            return None;
        }
        let dst = self.out.join(file);
        crate::sys::put(&dst, 0o644, &crate::sys::PutSrc::File(&src)).ok()?;
        let _ = std::fs::remove_file(&src);
        Frame::read(&dst)
    }

    /// The chip's cell on its rule: hovered at each candidate until the rule's band past it
    /// changes as a title's glyphs change it ([`TITLE_INK`]; `rest` the frame with the pointer
    /// away).
    fn find_chip(&self, lab: &dyn Lab, sid: &str, rest: &Frame) -> Result<Option<ChipAt>, Fail> {
        let text = on(lab, sid, &["text"]);
        let at = |row: usize, col: usize| {
            px_of(&lab.said(&["pointer", "move", &row.to_string(), &col.to_string()]))
        };
        let probe = format!("{}-{}-probe.png", self.who.name(), self.look.name());
        for row in rule_rows(&text).into_iter().take(3) {
            for col in CHIP_COLS {
                // The cell's neighbours first, so the pointer ends on the cell itself.
                let (Some((x1, _)), Some((_, y1)), Some((x0, y0))) =
                    (at(row, col + 1), at(row + 1, col), at(row, col))
                else {
                    continue;
                };
                let (cw, ch) = (x1 - x0, y1 - y0);
                if cw <= 0.0 || ch <= 0.0 {
                    continue;
                }
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a frame's width in pixels, exact in an f64"
                )]
                let k = (rest.w as f64 / (f64::from(self.a.cols) * cw))
                    .round()
                    .max(1.0);
                let c = ChipAt {
                    row,
                    col,
                    x: x0 * k,
                    y: y0 * k,
                    cw: cw * k,
                    ch: ch * k,
                };
                stop::sleep(Duration::from_millis(1200))?;
                let frame = self.shoot(lab, sid, &probe);
                let _ = lab.said(&["pointer", "leave"]);
                let _ = std::fs::remove_file(self.out.join(&probe));
                let Some(frame) = frame else {
                    continue;
                };
                let share = c.title_share(&frame, rest);
                if share >= TITLE_INK {
                    println!(
                        "  {} chip: row {row}, column {col} ({:.0}% of the rule's band past it changed when hovered)",
                        self.who.name(),
                        share * 100.0
                    );
                    return Ok(Some(c));
                }
                stop::sleep(Duration::from_millis(500))?;
            }
        }
        println!(
            "  {} chip: no cell of its rule showed a title when hovered",
            self.who.name()
        );
        Ok(None)
    }

    /// codex-alab's pad row cut from its frame at rest at full scale, two cells either side of
    /// what the program drew there (`…-rest-pad-band.png`): the line its own Autopilot words stand
    /// on, which the sheet shrinks past reading (`<harness>-<scale>x-pads.png` stacks them).
    /// Nothing where the screen shows no prompt row or the pointer names no cell.
    fn pad_band(&self, lab: &dyn Lab, sid: &str, scene: Scene, rest: &Frame) {
        let text = on(lab, sid, &["text"]);
        let rows: Vec<&str> = text.lines().collect();
        let Some(row) = rows
            .iter()
            .rposition(|r| r.starts_with('\u{203a}'))
            .and_then(|prompt| prompt.checked_sub(1))
        else {
            return;
        };
        let line = rows[row].trim_end();
        let first = line.chars().take_while(|c| *c == ' ').count();
        let width = text_width(line).max(first + 1);
        let at = |row: usize, col: usize| {
            px_of(&lab.said(&["pointer", "move", &row.to_string(), &col.to_string()]))
        };
        let cells = (at(row, 1), at(row + 1, 0), at(row, 0));
        let _ = lab.said(&["pointer", "leave"]);
        let (Some((x1, _)), Some((_, y1)), Some((x0, y0))) = cells else {
            return;
        };
        let (cw, ch) = (x1 - x0, y1 - y0);
        if cw <= 0.0 || ch <= 0.0 {
            return;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a frame's width in pixels, exact in an f64"
        )]
        let k = (rest.w as f64 / (f64::from(self.a.cols) * cw))
            .round()
            .max(1.0);
        let x = |col: f64| pixel((x0 + col * cw) * k);
        let band = rest.crop(
            (x(cells_f(first) - 2.5), pixel((y0 - ch) * k)),
            (x(cells_f(width) + 1.5), pixel((y0 + ch) * k)),
        );
        if let Some(png) = band.band_png(0, band.h) {
            let p = self.out.join(file_name(
                self.who,
                scene,
                self.look,
                self.a.scale,
                Kind::Rest,
                PAD_BAND,
            ));
            let _ = crate::sys::put(&p, 0o644, &crate::sys::PutSrc::Bytes(&png));
        }
    }

    /// The `.txt` beside a png: what it shows, the pointer, `status`, `chrome` and the screen.
    fn sidecar(&self, lab: &dyn Lab, sid: &str, scene: Scene, kind: Kind, pointer: &str) {
        let (who, look, scale) = (self.who, self.look, self.a.scale);
        let text = format!(
            "# {}: {} on {}, {}; {}, the instance launched in it; {}x{} at {scale}x; sid={sid}; at {}\n## pointer: {pointer}\n## status\n{}\n## chrome\n{}\n## the screen's text\n{}\n",
            png_name(who, scene, look, scale, kind),
            scene.what(),
            self.head,
            kind.shows(),
            look.theme(),
            self.a.cols,
            self.a.lines,
            crate::util::now_ms(),
            on(lab, sid, &["status"]).trim_end(),
            lab.said(&["chrome"]).trim_end(),
            on(lab, sid, &["text"]).trim_end(),
        );
        let p = self
            .out
            .join(file_name(who, scene, look, scale, kind, ".txt"));
        let _ = crate::sys::put(&p, 0o644, &crate::sys::PutSrc::Bytes(text.as_bytes()));
    }

    /// A png's caption: the scene, the shot, what `status` and the chip's `chrome` line read
    /// ([`chip_words`]), and the frame's ground; a [`FINDING`] where the look's ground is wrong, or
    /// the hovered title did not show or was drawn shorter than chrome's whole title.
    fn caption(&self, lab: &dyn Lab, sid: &str, shot: &Shot<'_>) -> String {
        let st = on(lab, sid, &["status"]);
        let fields: Vec<String> = ["autopilot", "task", "needs_n", "land_held", "land_nudges"]
            .iter()
            .filter_map(|k| forever::word(&st, k).map(|v| format!("{k}={v}")))
            .collect();
        let luma = shot.frame.luma();
        let mut c = format!(
            "{} on {}: {}; status {}; {}; {}, launched in it (mean luminance {luma:.2})",
            shot.scene.what(),
            self.head,
            shot.kind.shows(),
            fields.join(" "),
            chip_words(shot.chip_line),
            self.look.theme()
        );
        if self.who == Who::CodexAlab {
            let pad = codex_alab_proof::pad_of(&on(lab, sid, &["text"]));
            let says_mode = codex_alab_proof::dev_build(self.head)
                .is_some_and(|b| b >= codex_alab_proof::SAYS_ITS_MODE);
            c.push_str(&pad_words(shot.chip_line, pad.as_deref(), says_mode));
        }
        if let Some(h) = shot.hovered {
            c.push_str("; ");
            c.push_str(&title_verdict(h, shot.chip_line));
        }
        if !self.look.ground_holds(luma) {
            c.push_str(&format!(
                "; {FINDING}a {} frame of mean luminance {luma:.2}",
                self.look.name()
            ));
        }
        c
    }
}

/// What a title shot's caption says of the hovered title, against chrome's whole title on
/// `chip_line`: drawn whole, or a [`FINDING`] where it did not show ([`TITLE_INK`]) or the rule drew
/// it more than [`TITLE_SLACK`] cells short of the whole (its short or a lean fit).
fn title_verdict(h: Hovered, chip_line: &str) -> String {
    let whole = forever::surfaces::split_title(chip_line)
        .1
        .unwrap_or_else(|| "-".to_string());
    let want = text_width(&whole);
    let pct = h.share * 100.0;
    if h.share < TITLE_INK {
        format!("{FINDING}the hovered title did not show ({pct:.0}% of the rule's band changed)")
    } else if h.cells + TITLE_SLACK < want {
        format!(
            "{FINDING}the hovered title cut short: the rule drew {} of chrome's {want} cells (a short or lean fit); chrome's title: {whole:?}",
            h.cells
        )
    } else {
        format!(
            "the hovered title drawn whole in the rule ({} cells, chrome's {want}; {pct:.0}% of its band changed); chrome's title: {whole:?}",
            h.cells
        )
    }
}

/// The chip's words in a caption, off its `chrome` line: `chip face=… drawn="…"`, or why no
/// chip was drawn (`no chip drawn (chrome face=… drawn=- why=…)`). [`faults`] reads them back
/// by key.
fn chip_words(chip_line: &str) -> String {
    let face = forever::word(chip_line, "face").unwrap_or_else(|| "-".to_string());
    if chip_line.is_empty() {
        NO_LINE.to_string()
    } else if let Some(cells) = drawn_of(chip_line) {
        format!("chip face={face} drawn={cells:?}")
    } else {
        format!(
            "no chip drawn (chrome face={face} drawn=- why={})",
            forever::word(chip_line, "why").unwrap_or_default()
        )
    }
}

/// How a caption quotes codex-alab's pad ([`pad_words`]).
const PAD_SAID: &str = "; its pad: ";

/// What a codex-alab caption says of its composer pad (`pad`, as the program drew it:
/// [`codex_alab_proof::pad_of`]) beside the chip's `chrome` line: the pad quoted, and a
/// [`FINDING`] where aterm drew any Codex chip. Where the chip stood down beside that line, a build that
/// names its mode there (`says_mode`, [`codex_alab_proof::SAYS_ITS_MODE`]) must name the face
/// ([`forever::surfaces::face_words`]), else a [`FINDING`]; an older build's line is said to name
/// no mode.
fn pad_words(chip_line: &str, pad: Option<&str>, says_mode: bool) -> String {
    let mut w = format!(
        "{PAD_SAID}{}",
        pad.map_or_else(|| "no prompt row".to_string(), |p| format!("{p:?}"))
    );
    let own = pad.filter(|p| !p.is_empty());
    if drawn_of(chip_line).is_some() {
        w.push_str(&format!(
            "; {FINDING}aterm drew a chip on codex-alab's native composer"
        ));
    } else if let Some(p) = own {
        let face = forever::word(chip_line, "face").unwrap_or_else(|| "-".into());
        match forever::surfaces::face_words(&face) {
            Some(cells) if says_mode && !p.contains(cells) => w.push_str(&format!(
                "; {FINDING}codex-alab's own line names no {face} ({cells:?} not on it)"
            )),
            Some(_) if !says_mode => w.push_str(&format!(
                "; its line names no mode (a build before {} lights the mode's button only)",
                codex_alab_proof::SAYS_ITS_MODE
            )),
            _ => {}
        }
    }
    w
}

/// The value of `key=` among a caption's words, the `;` or `)` its piece ends with left off.
fn caption_word(caption: &str, key: &str) -> Option<String> {
    forever::word(caption, key).map(|v| v.trim_end_matches([';', ')', ',']).to_string())
}

/// The chip's drawn cells off its `chrome` line or a caption, `None` when none were drawn
/// (`drawn=-`).
fn drawn_of(line: &str) -> Option<String> {
    forever::surfaces::quoted(line, "drawn")
}

/// The tab labelled `label` brought to the front of its window: what `tab` said, or why it is not
/// in front (no such tab in the strip, `tab` refused, or the strip not selecting it after).
/// How long each of the held scene's extras waits for what it shows: a hang detector, never a
/// budget ([`Shots::held_extras`]).
const HELD_HANG: Duration = Duration::from_secs(60);

/// How many steps of the wheel Settings ▸ Harness's list may take to bring a row on its screen.
const PAGE_STEPS: usize = 60;

/// `rect=x,y,w,h` of a `controls` line.
fn rect_of(line: &str) -> Option<[f64; 4]> {
    let v: Vec<f64> = forever::word(line, "rect")?
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    v.try_into().ok()
}

/// TAB `label`'S STATE LINE ON SETTINGS ▸ HARNESS'S SCREEN, from `controls prefs`: its session row
/// (`harness/session/<n>`, labelled `<label>: …`) and that row's `/state` line laid out, neither
/// `offscreen=true`, and the state line inside `settings/list`'s rect. The state line's words, or
/// `None` while the list has not brought them there.
#[must_use]
pub fn held_row_shown(controls: &str, label: &str) -> Option<String> {
    let rows: Vec<(String, &str)> = controls
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            Some((forever::surfaces::quoted(l, "key")?, l))
        })
        .collect();
    let head = format!("{label}: ");
    let (key, _) = rows.iter().find(|(k, l)| {
        k.strip_prefix("harness/session/")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            && forever::surfaces::quoted(l, "label").is_some_and(|v| v.starts_with(&head))
            && forever::word(l, "offscreen").as_deref() != Some("true")
    })?;
    let state = format!("{key}/state");
    let (_, line) = rows.iter().find(|(k, _)| *k == state)?;
    let [_, y, _, h] = rect_of(line)?;
    let [_, ly, _, lh] = rect_of(rows.iter().find(|(k, _)| k == "settings/list")?.1)?;
    (forever::word(line, "offscreen").as_deref() != Some("true") && y >= ly && y + h <= ly + lh)
        .then(|| forever::surfaces::quoted(line, "label").unwrap_or_default())
}

/// TURNS SETTINGS ▸ HARNESS, open on the front window, TO TAB `label`'S ROW: the wheel over its
/// list turned to the top, then down a step at a time, as a person scrolls, until the row's state
/// line is on the list's screen ([`held_row_shown`]). Each turn waits for the list to move
/// ([`HELD_HANG`]); a step that does not move it is the list's end. The state line, or why it was
/// not brought there.
fn turn_to_row(lab: &dyn Lab, label: &str) -> Result<String, String> {
    let controls = || lab.said(&["controls", "prefs"]);
    let scroll = |read: &str| forever::word(read, "scroll");
    let mut read = String::new();
    if !crate::wait_until(HELD_HANG, || {
        read = controls();
        read.contains("key=\"settings/page/harness\"")
    }) {
        return Err(format!(
            "Settings \u{25b8} Harness was not on the front window within {} s",
            HELD_HANG.as_secs()
        ));
    }
    let (x, y) = forever::surfaces::list_middle(&read)
        .ok_or("the page drew no list to turn the wheel over")?;
    let xy = format!("xy={x},{y}");
    let turn = |dy: &str| {
        let r = lab.said(&["pointer", "wheel", dy, &xy]);
        if r.starts_with("OK") {
            Ok(())
        } else {
            Err(format!("the wheel refused: {}", r.trim()))
        }
    };
    turn("dy=-10000")?;
    if !crate::wait_until(HELD_HANG, || scroll(&controls()).as_deref() == Some("0")) {
        return Err(format!(
            "the list did not come to its top within {} s",
            HELD_HANG.as_secs()
        ));
    }
    for _ in 0..PAGE_STEPS {
        read = controls();
        if let Some(line) = held_row_shown(&read, label) {
            return Ok(line);
        }
        let before = scroll(&read);
        turn("dy=1")?;
        if !crate::wait_until(HELD_HANG, || scroll(&controls()) != before) {
            return Err(format!(
                "the list's end at scroll={}, and no {label} row with its state line on its screen",
                before.as_deref().unwrap_or("?")
            ));
        }
    }
    Err(format!(
        "{PAGE_STEPS} steps of the wheel brought no {label} row with its state line on its screen"
    ))
}

fn raise(lab: &dyn Lab, label: &str) -> Result<String, String> {
    let strip = forever::surfaces::strip_tabs(&lab.said(&["chrome"]));
    let Some(at) = strip.iter().find(|t| t.label == label).map(|t| t.index + 1) else {
        return Err(format!("no tab labelled {label} in the strip"));
    };
    let r = lab.said(&["tab", &at.to_string()]);
    let front = forever::surfaces::strip_tabs(&lab.said(&["chrome"]))
        .iter()
        .any(|t| t.label == label && t.selected);
    if r.trim_start().starts_with("OK") && front {
        Ok(format!("tab {at} ({label}) in front: {}", r.trim()))
    } else {
        Err(format!(
            "tab {at} ({label}) is not in front: `tab {at}` said {:?}, and the strip {} it",
            r.trim(),
            if front { "selects" } else { "does not select" }
        ))
    }
}

/// What line E's quota row says once its projection is drawn: the week's chain, as its Account
/// chip's node speaks it (`quota_view`'s `work_said`: `Next 7 days at 15 percent a day: …`).
/// Before it, just after Claude Code's first reading, the row says the lead's figure alone, or
/// the reset with `┄` ahead: the outlook's memo replays at the store's next look.
pub const LINE_E_PROJECTED: &str = "Next 7 days";

/// What line E's quota row says once the outlook's replay has read this tab's week: the reset
/// that reading carries, in the this-tab clause of its Account node (`quota_view`'s
/// `named_said`: `main, this tab, 80 percent of its week used, resets Tuesday 16:28.`). The
/// lead's figure alone, which the footer reads off the tap before the replay, says none.
pub const LINE_E_READ: &str = ", resets ";

/// How long a Claude Code scene's line E may take to say what its pane allows once its tab is in
/// front ([`line_e_due`]): a hang detector. The footer reads the tap's row every 2 s and the
/// outlook replays at the store's next look, every 10 s, so the projection stands about 15 s
/// after the first reply (measured 2026-11-08: a scene shot 12 s after it showed none, one shot
/// 16 s after it did).
pub const LINE_E_HANG: Duration = Duration::from_secs(60);

/// What tab's Account node says once its line E has said what its pane allows, by the tab's
/// `status`: under Autopilot Off the pane's recovery refuses forecasts
/// (`FooterFacts::account_forecast_refusal`, `Refusal::SessionPaused`; the glass design's §5.1),
/// so the row shows the replay's reading alone, the reset with `┄` ahead ([`LINE_E_READ`]);
/// any other pane draws the week's projection ([`LINE_E_PROJECTED`]).
#[must_use]
pub fn line_e_due(status: &str) -> &'static str {
    if forever::word(status, "autopilot").as_deref() == Some("off") {
        LINE_E_READ
    } else {
        LINE_E_PROJECTED
    }
}

/// How line E's Account node's label begins: its quota view's spoken head (`QuotaView::spoken`,
/// `Accounts. main, this tab, …`). Line D's fallback unit, drawn where line E is not, is an Account
/// node too and can carry the same sentences, its projection's `Next 7 days` included, but its
/// label is the account fact's (`claude_facts::fact_spoken_of`: `Account: main, this tab, …`), so
/// a node whose label begins otherwise is not line E's.
pub const LINE_E_HEAD: &str = "Accounts. ";

/// A caption's note where a Claude Code scene's line E could not be judged
/// ([`Unsaid::NotJudged`]): why follows. It is no [`FINDING`].
pub const LINE_E_NOT_JUDGED: &str = "line E not judged: ";

/// Why a Claude Code scene's line E was not seen to say what its pane allows
/// ([`line_e_unsaid`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsaid {
    /// Line E drew nothing due within the hang, or a projection under Autopilot Off: what, with
    /// what its node said last. The caption carries it as a [`FINDING`].
    Finding(String),
    /// This aterm's `chrome` reports no Account node ([`reports_account_nodes`] of a read that
    /// answered for the tab, [`answered_for`]), so no read tells what line E drew: the caption
    /// says so plainly ([`LINE_E_NOT_JUDGED`]), and the scene is shot without a wait.
    NotJudged(String),
}

impl Unsaid {
    /// What a scene's caption carries for it, from its `; `.
    #[must_use]
    pub fn noted(&self) -> String {
        match self {
            Self::Finding(why) => format!("; {FINDING}{why}"),
            Self::NotJudged(why) => format!("; {LINE_E_NOT_JUDGED}{why}"),
        }
    }
}

/// Whether a `chrome` read's aterm reports Account nodes: its `a11y kinds=…` line, which a
/// development build prints once per read whatever any window shows (an overlay's included),
/// names `account`. A release build prints no `a11y` line, and one from before the glass
/// design's §7 step 5 no Account node; another aterm under `LAB_ATERM` may be either. Judge it
/// only of a read that answered ([`answered_for`]): a failed one says neither.
#[must_use]
pub fn reports_account_nodes(chrome: &str) -> bool {
    chrome
        .lines()
        .filter_map(|l| l.strip_prefix("a11y kinds="))
        .any(|kinds| kinds.split(',').any(|k| k == "account"))
}

/// Whether a `chrome` read answered for tab `sid`: it carries the tab's `autopilot sid=<sid>`
/// line ([`forever::surfaces::chip_of`]), which every aterm this verb drives prints for a
/// supervised pane, a release build's included. A read that failed (the client's error words, or
/// `ERR …` from the main thread) carries none, and says nothing of what the build reports.
#[must_use]
pub fn answered_for(chrome: &str, sid: &str) -> bool {
    forever::surfaces::chip_of(chrome, sid).is_some()
}

/// Tab `sid`'s Account chip nodes in a `chrome` read (a development build's `a11y window=…
/// account sid=<sid> role=… label="…" description=…` lines), each from its `role=`, in the tree's
/// order: line E's ([`LINE_E_HEAD`]) or line D's fallback unit.
fn account_nodes<'a>(chrome: &'a str, sid: &str) -> impl Iterator<Item = &'a str> {
    let head = format!("account sid={sid} ");
    chrome
        .lines()
        .filter_map(|l| l.strip_prefix("a11y "))
        .filter_map(move |l| l.split_once(' ')?.1.strip_prefix(head.as_str()))
}

/// Tab `sid`'s line E node in a `chrome` read: its Account node whose label begins
/// [`LINE_E_HEAD`], from its `role=`; `None` where `chrome` printed none (line E not drawn, line
/// D's unit standing alone, or an aterm that reports no Account node).
#[must_use]
pub fn line_e_node(chrome: &str, sid: &str) -> Option<String> {
    account_nodes(chrome, sid)
        .find(|node| {
            node.split_once(" label=\"")
                .is_some_and(|(_, label)| label.starts_with(LINE_E_HEAD))
        })
        .map(str::to_string)
}

/// Wait, looking at `lab`'s `chrome` after each `draw`, until tab `sid`'s line E says what its
/// pane allows ([`line_e_due`] of its `status`, read once: a scene's mode is set before it
/// settles). `draw` paces the looks and draws the frame each one reads: the Account node is
/// laid by the window's last frame (`claude_chips_a11y` reads its hits), and a headless window
/// draws none of its own, so a look with no frame drawn since the tab came to the front finds no
/// node of it. Only line E's node counts ([`line_e_node`]): line D's fallback unit says the same
/// sentences where line E never showed. `None` once that node says it, and the pane is not Off
/// with a projection drawn. Else why: [`Unsaid::NotJudged`] at the first look that answered for
/// the tab ([`answered_for`]) and whose aterm reports no Account node ([`reports_account_nodes`]),
/// with no wait (a look that failed is a look with no node yet); or an [`Unsaid::Finding`], with
/// what line E's node said last (`-` for none, and line D's unit where it stood instead): nothing due
/// within `hang`, or a projection under Autopilot Off, whose recovery refuses forecasts.
/// # Errors
/// `draw`'s (an interrupt).
fn line_e_unsaid(
    lab: &dyn Lab,
    sid: &str,
    hang: Duration,
    mut draw: impl FnMut() -> Result<(), Fail>,
) -> Result<Option<Unsaid>, Fail> {
    let due = line_e_due(&on(lab, sid, &["status"]));
    let end = Instant::now() + hang;
    loop {
        draw()?;
        let chrome = lab.said(&["chrome"]);
        if answered_for(&chrome, sid) && !reports_account_nodes(&chrome) {
            return Ok(Some(Unsaid::NotJudged(
                "this aterm's `chrome` reports no Account node (no `a11y kinds=` line names \
                 `account`: a release build, or one from before the glass design's §7 step 5), \
                 so nothing reads what line E drew"
                    .to_string(),
            )));
        }
        let node = line_e_node(&chrome, sid);
        match node.as_deref() {
            Some(n) if due == LINE_E_READ && n.contains(LINE_E_PROJECTED) => {
                return Ok(Some(Unsaid::Finding(format!(
                    "line E drew a projection under Autopilot Off, whose recovery refuses \
                     forecasts (its Account node: {n})"
                ))));
            }
            Some(n) if n.contains(due) => return Ok(None),
            _ => {}
        }
        if Instant::now() >= end {
            let what = if due == LINE_E_READ {
                "no reading of its week"
            } else {
                "no projection"
            };
            let unit = node
                .is_none()
                .then(|| account_nodes(&chrome, sid).next())
                .flatten()
                .map(|n| format!("; line D's account unit stood instead: {n}"))
                .unwrap_or_default();
            return Ok(Some(Unsaid::Finding(format!(
                "line E drew {what} within {} s of its tab in front (its Account node: {}{unit})",
                hang.as_secs(),
                node.as_deref().unwrap_or("-")
            ))));
        }
    }
}

/// The agy typing take, paired: the baseline's arm first (this binary again, with
/// `--typing-only` and `LAB_ATERM` the baseline's aterm), then this lab's aterm, back to back;
/// both lines to `<out>/typing-agy.txt`. Whether both arms were taken.
fn typing_pair(a: &Asked, base: &str, out: &Path) -> Result<bool, Fail> {
    let exe = std::env::current_exe()
        .map_err(|e| Fail::die(format!("autopilot-captures: this program's path: {e}")))?;
    let r = aterm_uds::spawnfd::command(&exe)
        .args(["autopilot-captures", "--typing-only"])
        .env("LAB_ATERM", base)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| Fail::die(format!("autopilot-captures: the baseline's take: {e}")))?;
    let said = String::from_utf8_lossy(&r.stdout).into_owned();
    let mut lines = vec![said.lines().rfind(|l| l.starts_with(TAKEN)).map_or_else(
        || {
            format!(
                "{NOT_TAKEN} baseline {base} (exit {:?}): {}",
                r.status.code(),
                crate::util::one_line(
                    &format!("{said} {}", String::from_utf8_lossy(&r.stderr)),
                    400
                )
            )
        },
        str::to_string,
    )];
    lines.push(
        typing_take(a)
            .unwrap_or_else(|f| format!("{NOT_TAKEN} this tree: {}", f.msg.unwrap_or_default())),
    );
    let text = format!("# {TYPING_SAYS}\n{}\n", lines.join("\n"));
    for l in &lines {
        println!("autopilot-captures: {l}");
    }
    let p = out.join("typing-agy.txt");
    crate::sys::put(&p, 0o644, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("autopilot-captures: {}: {e}", p.display())))?;
    Ok(lines.iter().all(|l| l.starts_with(TAKEN)))
}

/// A taken arm's line starts so.
const TAKEN: &str = "typing agy ";

/// An arm not taken starts so, and says why.
const NOT_TAKEN: &str = "typing NOT TAKEN:";

/// How the typing take is taken and paired, its file's first line.
const TYPING_SAYS: &str = "the agy typing take: 200 keys (one `send` or `key \
    space` each, 50 ms apart) then 200 backspaces into the composer of an agy under \
    Autopilot Cruise (harness continue = false, so nothing is typed into it), after the stub's \
    first reply, under one `video 60 keys` that must outlast the burst, then an idle window of \
    20 s, on a private headless lab 144x50 at 1x of its own; the baseline's arm first, then this tree's, back to back, the \
    same lab binary, agy, stub, keys and pace, only the aterm differing; headless, so \
    frames are n_render with render_p*; cpu_ms, cpu_us_per_render and kinsn_per_render are \
    aterm's own CPU time and retired instructions (proc_pid_rusage) from the burst's metrics \
    reset to its percentiles read, per rendered frame, main_us_per_render and \
    main_kinsn_per_render its main thread's CPU time and instructions a frame, and idle_cpu_ms \
    its CPU time across the idle wait";

/// The `metrics percentiles` keys a take quotes.
const TAKE_KEYS: [&str; 12] = [
    "n_input",
    "input_p50_ms",
    "input_p95_ms",
    "input_p99_ms",
    "n_present",
    "present_p50_ms",
    "present_p95_ms",
    "present_p99_ms",
    "n_render",
    "render_p50_ms",
    "render_p95_ms",
    "render_p99_ms",
];

/// One agy typing take on this process's aterm (`LAB_ATERM`): its `typing …` line.
fn typing_take(a: &Asked) -> Result<String, Fail> {
    let tag = if a.typing_only {
        "apcap-typing-base"
    } else {
        "apcap-typing"
    };
    clear_lab(tag)?;
    let mut u = crate::lab::UpOpts::from_env();
    u.trust = true;
    u.lifeline = true;
    u.cols = "144".into();
    u.lines = "50".into();
    u.scale = None;
    u.harness_keys = format!("{}\ncontinue = false", u.harness_keys)
        .trim()
        .to_string();
    crate::lab::up(tag, &u)?;
    let took = typing_in(tag);
    let _ = crate::lab::down(tag, false);
    took
}

fn typing_in(tag: &str) -> Result<String, Fail> {
    let h = Harness::Agy;
    let mut run = Run::load(tag)?;
    let (bin, ver) = h.bin_ver(&run);
    let (bin, ver) = (bin.to_string(), ver.to_string());
    if bin.is_empty() {
        return Err(Fail::exit(3, "no agy in the lab (LAB_AGY at up)"));
    }
    run.agent = h.name().into();
    run.save();
    let chain = Chain::Cruise;
    let dir = forever::lay_repo(&run, chain).map_err(Fail::die)?;
    let dirs = std::slice::from_ref(&dir);
    let script = forever::launcher(&run, h, dirs, false)?;
    crate::verbs::set_custom_mode(&run, forever::MODE, &forever::spec_on(&[chain], h, dirs))?;
    let door = Door::new(&run)?;
    let sid = forever::spawn_tab(&door, &dir)?;
    let _ = forever::launch_tab(&door, &sid, chain, &script)?;
    if let Some(why) = forever::empty_composer(&door, &sid, h.name())?.why(h.name()) {
        return Err(Fail::die(why));
    }
    if !forever::tab_turn(&door, &sid, h.name(), &chain.prompt())? {
        return Err(Fail::die("the composer did not take the first prompt"));
    }
    let replied = crate::wait_until(Duration::from_secs(60), || {
        forever::on(&door, &sid, &["text"]).contains(forever::DONE_CLAIM)
            && forever::word(&forever::on(&door, &sid, &["status"]), "agent").as_deref()
                == Some("idle")
    });
    if !replied {
        return Err(Fail::die(
            "the stub's first reply did not stand within 60 s",
        ));
    }
    stop::sleep(Duration::from_secs(3))?;
    let (picked, said) = burst_taken(&door, &sid)?;
    let idle = idle_taken(&door)?;
    Ok(format!(
        "{TAKEN}{ver} aterm {}: {picked}; {idle}; video: {said}",
        run.aterm
    ))
}

/// The typing take's recording judged: what `video 60 keys` said, and whether the burst was over
/// when it answered. It held the burst only where it answered `OK` after it; else why the take is
/// not taken.
fn recording_held(said: &str, outlasted: bool) -> Result<String, String> {
    let said = crate::util::one_line(said.trim(), 200);
    if said.starts_with("OK") && outlasted {
        Ok(said)
    } else {
        Err(format!(
            "the recording {} the burst: `video 60 keys` said {said:?}",
            if outlasted {
                "failed during"
            } else {
                "ended before"
            }
        ))
    }
}

/// What the instance's aterm had spent at one reading (`proc_pid_rusage`): its CPU time, user and
/// system together, in the kernel's ticks, and the instructions its threads retired; and its main
/// thread's id, CPU time in ns ([`crate::sys::thread_cpu_ns`], the least thread id) and retired
/// instructions ([`crate::sys::thread_instructions`]), where it draws a headless frame, apart from
/// the threads a `video` recording converts frames on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Spent {
    ticks: u64,
    insn: u64,
    main: Option<MainSpent>,
}

/// The main thread's part of a [`Spent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MainSpent {
    tid: u64,
    ns: u64,
    insn: Option<u64>,
}

/// `pid`'s [`Spent`] now, or `None` when it cannot be read.
fn spent(pid: i32) -> Option<Spent> {
    if pid <= 1 {
        return None;
    }
    let main = crate::sys::thread_cpu_ns(pid).and_then(|threads| {
        threads.first().map(|t| MainSpent {
            tid: t.tid,
            ns: t.ns,
            insn: crate::sys::thread_instructions(pid, t.tid),
        })
    });
    crate::sys::rusage(pid).map(|u| Spent {
        ticks: u.ri_user_time.saturating_add(u.ri_system_time),
        insn: u.ri_instructions,
        main,
    })
}

/// `v` tenths as `<whole>.<tenth>`.
fn tenths(v: u64) -> String {
    format!("{}.{}", v / 10, v % 10)
}

/// The window between two [`Spent`] readings, at the kernel's timebase `tb`: `cpu_ms=`, its CPU
/// time, and, over its `frames` rendered frames, `cpu_us_per_render=` and `kinsn_per_render=`
/// (thousands of instructions a frame), and `main_us_per_render=` and `main_kinsn_per_render=`,
/// the main thread's CPU time and instructions a frame. A word reads `-` where a reading, the
/// timebase, a frame or a retired-instruction count
/// is missing, a counter went backwards (another process), or the main thread is not the same one.
/// The first three count every thread of the process, so a frame's share is the whole take's work
/// over its frames, a recording's frame conversion included. Other sessions' load moves all of
/// them far less than a frame's wall time: CPU time still moves with which core ran the work,
/// retired instructions hardly at all.
fn spent_words(
    from: Option<Spent>,
    to: Option<Spent>,
    frames: Option<u64>,
    tb: Option<(u32, u32)>,
) -> String {
    let d = spent_between(from, to);
    let ns = cpu_ns(d, tb);
    let frames = frames.filter(|&f| f > 0);
    let dash = || "-".to_string();
    let main = from
        .and_then(|a| a.main)
        .zip(to.and_then(|b| b.main))
        .filter(|(a, b)| a.tid == b.tid);
    let main_ns = main.filter(|(a, b)| b.ns >= a.ns).map(|(a, b)| b.ns - a.ns);
    let main_insn = main
        .and_then(|(a, b)| a.insn.zip(b.insn))
        .filter(|(a, b)| b > a)
        .map(|(a, b)| b - a);
    format!(
        "cpu_ms={} cpu_us_per_render={} kinsn_per_render={} main_us_per_render={} \
         main_kinsn_per_render={}",
        ns.map_or_else(dash, |ns| tenths(ns / 100_000)),
        ns.zip(frames)
            .map_or_else(dash, |(ns, f)| tenths(ns / f.saturating_mul(100))),
        d.map(|(_, i)| i)
            .filter(|&i| i > 0)
            .zip(frames)
            .map_or_else(dash, |(i, f)| tenths(i / f.saturating_mul(100))),
        main_ns
            .zip(frames)
            .map_or_else(dash, |(ns, f)| tenths(ns / f.saturating_mul(100))),
        main_insn
            .zip(frames)
            .map_or_else(dash, |(i, f)| tenths(i / f.saturating_mul(100)))
    )
}

/// The ticks and instructions spent from `from` to `to`; `None` where either is missing or a
/// counter went backwards.
fn spent_between(from: Option<Spent>, to: Option<Spent>) -> Option<(u64, u64)> {
    from.zip(to)
        .filter(|(a, b)| b.ticks >= a.ticks && b.insn >= a.insn)
        .map(|(a, b)| (b.ticks - a.ticks, b.insn - a.insn))
}

/// [`spent_between`]'s ticks in nanoseconds at the timebase `tb`.
fn cpu_ns(d: Option<(u64, u64)>, tb: Option<(u32, u32)>) -> Option<u64> {
    d.zip(tb)
        .map(|((t, _), (n, dn))| aterm_sysprobe::procs::ticks_to_ns(t, n, dn))
}

/// The burst: 200 keys, then 200 backspaces, 50 ms apart, after the metrics are reset; what the
/// instance had spent just after the reset ([`spent`]).
fn type_burst(lab: &dyn Lab, sid: &str) -> Result<Option<Spent>, Fail> {
    stop::sleep(Duration::from_secs(2))?;
    let _ = lab.said(&["metrics", "reset"]);
    let from = lab.aterm_pid().and_then(spent);
    let keys = "the quick brown fox jumps over the lazy dog ";
    for c in keys.chars().cycle().take(200) {
        if c == ' ' {
            let _ = on(lab, sid, &["key", "space"]);
        } else {
            let _ = on(lab, sid, &["send", &c.to_string()]);
        }
        stop::sleep(Duration::from_millis(50))?;
    }
    for _ in 0..200 {
        let _ = on(lab, sid, &["key", "backspace"]);
        stop::sleep(Duration::from_millis(50))?;
    }
    Ok(from)
}

/// The burst ([`type_burst`]) under one `video 60 keys` that must outlast it, then `metrics
/// percentiles`: the take's [`TAKE_KEYS`] words, what aterm spent from the reset to that read
/// ([`spent_words`] over its `n_render` frames), and what the recording said, or why it is not
/// taken ([`recording_held`]).
fn burst_taken(lab: &(dyn Lab + Sync), sid: &str) -> Result<(String, String), Fail> {
    let burst_over = AtomicBool::new(false);
    let (pct, (from, to), video) = std::thread::scope(|s| {
        // One recording for the whole burst: it holds the window focused, as a person's typing
        // does. `video` answers when it ends: at `video stop` after the burst, or at once when
        // it is refused.
        let recording = s.spawn(|| {
            let said = lab.said(&["video", "60", "keys"]);
            (said, burst_over.load(Ordering::Relaxed))
        });
        let typed = type_burst(lab, sid);
        let pct = lab.said(&["metrics", "percentiles"]);
        let to = lab.aterm_pid().and_then(spent);
        burst_over.store(true, Ordering::Relaxed);
        let _ = lab.said(&["video", "stop"]);
        let video = recording
            .join()
            .unwrap_or_else(|_| ("ERR the recording's thread panicked".to_string(), false));
        typed.map(|from| (pct, (from, to), video))
    })?;
    let said = recording_held(&video.0, video.1).map_err(Fail::die)?;
    let mut picked: Vec<String> = TAKE_KEYS
        .iter()
        .filter_map(|k| forever::word(&pct, k).map(|v| format!("{k}={v}")))
        .collect();
    picked.push(spent_words(
        from,
        to,
        forever::word(&pct, "n_render").and_then(|n| n.parse().ok()),
        aterm_sysprobe::procs::kernel_timebase(),
    ));
    Ok((picked.join(" "), said))
}

/// The idle window's words after a take: its opening counts once the settle after its `metrics
/// reset` has passed ([`forever::surfaces::open_idle_count`]), [`forever::surfaces::IDLE_MS`]
/// with no read, the closing counts; what moved in it
/// (`timer_wakes`, `deadline_arms_by_owner`, `wakes_by_variant`, `n_render`), the CPU time aterm
/// spent across that wait (`idle_cpu_ms`) and, where the window did not rest, why
/// ([`forever::surfaces::idle_counts`]).
fn idle_taken(lab: &dyn Lab) -> Result<String, Fail> {
    let (m0, p0) = forever::surfaces::open_idle_count(|args| lab.said(args), stop::sleep)?;
    let from = lab.aterm_pid().and_then(spent);
    stop::sleep(Duration::from_millis(
        u64::try_from(forever::surfaces::IDLE_MS).unwrap_or(20_000),
    ))?;
    let to = lab.aterm_pid().and_then(spent);
    let (m1, p1) = (
        lab.said(&["metrics"]),
        lab.said(&["metrics", "percentiles"]),
    );
    let m = forever::surfaces::idle_metrics_between(&m0, &m1);
    let p = forever::surfaces::idle_percentiles_between(&p0, &p1);
    let mut said: Vec<String> = ["timer_wakes", "deadline_arms_by_owner", "wakes_by_variant"]
        .iter()
        .map(|k| format!("{k}={}", forever::word(&m, k).unwrap_or_else(|| "-".into())))
        .collect();
    said.push(format!(
        "n_render={}",
        forever::word(&p, "n_render").unwrap_or_else(|| "-".into())
    ));
    said.push(format!(
        "idle_cpu_ms={}",
        cpu_ns(
            spent_between(from, to),
            aterm_sysprobe::procs::kernel_timebase()
        )
        .map_or_else(|| "-".to_string(), |ns| tenths(ns / 100_000))
    ));
    let bad = forever::surfaces::idle_counts(&m, &p);
    Ok(format!(
        "idle {} s: {}; {}",
        forever::surfaces::IDLE_MS / 1000,
        said.join(" "),
        if bad.is_empty() {
            "at rest".to_string()
        } else {
            format!("NOT at rest: {}", bad.join("; "))
        }
    ))
}

/// A taken codex-alab arm's line starts so.
const CODEX_TAKEN: &str = "typing codex-alab ";

/// How the codex-alab typing pair is taken, its file's first line.
const CODEX_TYPING_SAYS: &str = "the codex-alab typing take: 200 keys (one `send` or `key \
    space` each, 50 ms apart) then 200 backspaces into the composer of codex-alab-proof's `off` \
    leg (Autopilot Off, its claim given back and its reply standing, so nothing is typed into \
    it), under one `video 60 keys` that must outlast the burst, then an idle window of 20 s; each \
    arm a rig of its own, 144x50 at 1x in Aterm Night, the baseline's aterm first, then this \
    lab's, back to back, the same lab binary, package, fake model, keys and pace, only the aterm \
    differing; headless, so frames are n_render with render_p*";

/// One codex-alab typing take on `aterm` (else this lab's): its `typing codex-alab …` line.
fn codex_typing_take(aterm: Option<&str>) -> Result<String, Fail> {
    let args = ["--columns", "144", "--lines", "50", "--scale", "1"]
        .map(str::to_string)
        .to_vec();
    let mut rig = Proof::captures_rig(aterm.map(str::to_string), args, Look::Night.theme())?;
    let took = codex_typing_in(&mut rig);
    rig.finish();
    took
}

fn codex_typing_in(rig: &mut Proof) -> Result<String, Fail> {
    let sid = rig.open_scene("off")?;
    let claimed = crate::wait_until(CODEX_LIMIT, || rig.proven_pid(&sid).is_some());
    if !claimed {
        return Err(Fail::die("the off leg's codex-alab never held its claim"));
    }
    let off = on(rig, &sid, &["meta", "set", "autopilot", "off"]);
    let rested = crate::wait_until(CODEX_LIMIT, || codex_settled(rig, Scene::Off, &sid));
    if !rested {
        return Err(Fail::die(format!(
            "the off leg did not rest under Off within {} s ({})",
            CODEX_LIMIT.as_secs(),
            off.trim()
        )));
    }
    stop::sleep(Duration::from_secs(3))?;
    let (picked, video) = burst_taken(rig, &sid)?;
    let idle = idle_taken(rig)?;
    Ok(format!(
        "{CODEX_TAKEN}{} aterm {}: {picked}; {idle}; video: {video}",
        rig.package_version(),
        rig.aterm_path().display()
    ))
}

/// The order of the codex-alab typing takes: the baseline (`true`) and this lab's aterm twice
/// each, ABBA, so each arm runs once in each place of a pair and neither always runs second, on a
/// machine whose load moves between takes.
const CODEX_TYPING_ORDER: [bool; 4] = [true, false, false, true];

/// The codex-alab typing takes ([`CODEX_TYPING_ORDER`]), back to back, in this process; each line,
/// its slot and arm named, to `<out>/typing-codex-alab.txt`. Whether every take was taken.
fn codex_typing_pair(base: &str, out: &Path) -> Result<bool, Fail> {
    let mut lines = Vec::new();
    let n = CODEX_TYPING_ORDER.len();
    for (i, baseline) in CODEX_TYPING_ORDER.into_iter().enumerate() {
        let (arm, aterm) = if baseline {
            ("baseline", Some(base))
        } else {
            ("this tree", None)
        };
        let slot = format!("[slot {}/{n}, {arm}] ", i + 1);
        lines.push(match codex_typing_take(aterm) {
            Ok(line) => line.replacen(CODEX_TAKEN, &format!("{CODEX_TAKEN}{slot}"), 1),
            Err(f) if f.stopped.is_some() => return Err(f),
            Err(f) => format!("{NOT_TAKEN} {slot}{}", f.msg.unwrap_or_default()),
        });
    }
    let text = format!("# {CODEX_TYPING_SAYS}\n{}\n", lines.join("\n"));
    for l in &lines {
        println!("autopilot-captures: {l}");
    }
    let p = out.join("typing-codex-alab.txt");
    crate::sys::put(&p, 0o644, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("autopilot-captures: {}: {e}", p.display())))?;
    Ok(lines.iter().all(|l| l.starts_with(CODEX_TAKEN)))
}

#[cfg(test)]
#[path = "autopilot_captures_tests.rs"]
mod tests;
