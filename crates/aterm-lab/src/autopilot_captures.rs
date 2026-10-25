// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `autopilot-captures` ([`cmd_autopilot_captures`]): Autopilot's scenes on every harness, side by
//! side, as one captures page reads them (design §14.2's 2b). One private headless instance per
//! harness and look, launched in that look, so each program starts on the ground it is shot on: on
//! the four aterm supervises (Claude Code, agy, Muse Code, Gemini CLI) it drives the forever lab's
//! own chains against its stub ([`crate::forever`]: cruise-n's two needs, land-b's open hold,
//! land-c's repository hold, land-a's landing, Off); on codex-alab, which supervises itself, it
//! drives `codex-alab-proof`'s legs against its fake model ([`crate::codex_alab_proof`]: Cruise,
//! Land, landed, Off, a need). A scene this tree cannot show is a named skip ([`Plan::Skip`]).
//! After each scene settles it takes an `image` at rest, points at the chip for its hovered title,
//! and writes `chrome` and `status` beside each png, then `manifest.tsv` ([`Row`]). [`missing`]
//! names a row the plan wants and the manifest lacks, and [`faults`] a png row that shows something
//! wrong. With `--baseline` it also takes one paired Gemini CLI typing take ([`typing_pair`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::HoldLeg;
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
    pub const ALL: [Who; 5] = [
        Who::Agent(Harness::Claude),
        Who::Agent(Harness::Agy),
        Who::Agent(Harness::Muse),
        Who::Agent(Harness::Gemini),
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
            Who::Agent(_) => &SUPERVISED,
            Who::CodexAlab => &CODEX_ALAB,
        }
    }

    /// Why it draws no chip on this tree, so its rest shots have no hovered title beside them;
    /// `None` where it draws one on every scene, and a rest shot with no chip is a
    /// [`Fault::NoChip`]. The four supervised harnesses' answer is the forever lab's
    /// ([`forever::surfaces::footless`]). agy draws its chip under Autopilot Off too: only an off
    /// harness switch (the master's or its agent's) leaves its footer bare, and no scene turns one
    /// off.
    #[must_use]
    pub fn chipless(self) -> Option<&'static str> {
        match self {
            Who::Agent(h) if forever::surfaces::footless(h.name()) => Some(NO_CHIP_UNTIL_2C),
            Who::Agent(_) => None,
            Who::CodexAlab => Some(NO_CHIP_UNTIL_2D),
        }
    }

    fn rank(self) -> usize {
        Who::ALL
            .iter()
            .position(|w| *w == self)
            .unwrap_or(usize::MAX)
    }
}

/// Why Gemini CLI takes no hovered title: no chip line until 2c.
pub const NO_CHIP_UNTIL_2C: &str =
    "Gemini CLI draws no chip on this tree (chrome's why=no-footer) until 2c (§14.2, with 8a)";

/// Why codex-alab takes no hovered title: no chip line until 2d.
pub const NO_CHIP_UNTIL_2D: &str =
    "codex-alab draws no chip on this tree (chrome's why=no-footer) until 2d (§14.2, with 8b)";

/// A scene the captures show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    /// Cruise, its report naming two needs (cruise-n, N1).
    CruiseNeeds,
    /// Land held by open work its report names (land-b, L2).
    LandOpen,
    /// Land held by work in its repository, an untracked file (land-c, L4).
    LandRepo,
    /// Land landed.
    Landed,
    /// Autopilot Off.
    Off,
    /// Land held by the same reply, said (`same-land`, once 3a says the stop).
    SameLand,
    /// codex-alab under Cruise, at rest after a turn.
    Cruise,
    /// codex-alab landing: Land's instruction typed.
    Land,
    /// codex-alab's report naming a need.
    Need,
}

impl Scene {
    const ALL: [Scene; 9] = [
        Scene::CruiseNeeds,
        Scene::LandOpen,
        Scene::LandRepo,
        Scene::Landed,
        Scene::Off,
        Scene::SameLand,
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
            Scene::Cruise => "Cruise at rest after a turn",
            Scene::Land => "Land landing: Land's instruction typed (land@v1)",
            Scene::Need => "its report naming a NEEDS YOU need",
        }
    }

    fn rank(self) -> usize {
        [&SUPERVISED[..], &CODEX_ALAB[..]]
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

/// Why `same-land` is a skip. 2a brought its chain into the forever lab, and until 3a it runs
/// land-c's scene word for word (§14.2.1's departures), so the stop words 3a adds are what it
/// would show.
pub const SAME_LAND_WAITS: &str = "UNOBSERVED until 3a (§14.3, a stopped Autopilot says so) is \
    on main: this tree says no stop, and the forever lab's same-land chain runs land-c's scene \
    until then, so it shows nothing land-repo does not";

/// Why codex-alab's holds are skips: it rebuilds no Land hold from its own acts until A3.
pub const A3_WAITS: &str = "UNOBSERVED until A3 (§14.14, codex-alab's Land hold rebuilt from \
    its own acts, deferred to the round after 8b): codex-alab keeps nudging and holds no landing, \
    so there is no hold to show";

const SUPERVISED: [(Scene, Plan); 6] = [
    (Scene::CruiseNeeds, Plan::Chain(Chain::CruiseN)),
    (Scene::LandOpen, Plan::Chain(Chain::LandB)),
    (Scene::LandRepo, Plan::Chain(Chain::LandC)),
    (Scene::Landed, Plan::Chain(Chain::LandA)),
    (Scene::Off, Plan::Chain(Chain::Off)),
    (Scene::SameLand, Plan::Skip(SAME_LAND_WAITS)),
];

const CODEX_ALAB: [(Scene, Plan); 8] = [
    (Scene::Cruise, Plan::Leg("continue")),
    (Scene::Land, Plan::Leg("land")),
    (Scene::Landed, Plan::Leg("reland")),
    (Scene::Off, Plan::Leg("off")),
    (Scene::Need, Plan::Leg("needs")),
    (Scene::LandOpen, Plan::Skip(A3_WAITS)),
    (Scene::LandRepo, Plan::Skip(A3_WAITS)),
    (Scene::SameLand, Plan::Skip(A3_WAITS)),
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
        } else if who.chipless().is_none() {
            let why = caption_word(&r.caption, "why").unwrap_or_else(|| NO_LINE.to_string());
            flaw(Fault::NoChip(why));
        }
    }
    flaws
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
    /// The sheets alone, drawn again from the folder's manifest.
    sheets_only: bool,
}

/// The grid's columns: wide enough that the rule's slot holds a chip's whole title (its
/// `TitleFit::Full`), as v5's closing captures were; 240 left Cruise's two needs out of it.
const COLS: u16 = 400;
const LINES: u16 = 40;

const HELP: &str = "usage: targo --unverified lab autopilot-captures --harness all|claude|agy|muse|gemini|codex-alab
       [--scale 1|2] [--look day|night|both] [--out DIR] [--columns N] [--lines N] [--baseline ATERM]
   or: targo --unverified lab autopilot-captures --baseline ATERM [--out DIR]
   or: targo --unverified lab autopilot-captures --sheets [--out DIR]

Autopilot's scenes on every harness, side by side (design §14.2's 2b), on one private headless
instance per harness and look, launched in that look (its aterm.toml's theme), so every program
starts on the ground it is shot on. Claude Code, agy, Muse Code and Gemini CLI run the forever
lab's chains against its stub: cruise-needs (cruise-n's two needs), land-open (land-b's open hold),
land-repo (land-c's repository hold), landed (land-a's landing) and off; same-land is a named skip
until 3a. codex-alab runs codex-alab-proof's legs against its fake model: cruise, land, landed, off
and need; its holds are named skips until A3. Gemini CLI needs LAB_GEMINI; codex-alab, the package
codex-alab-proof would pick.

After each scene settles it takes an image at rest (<harness>-<scene>-<look>-<scale>x-rest.png),
then points at the chip and takes its hovered title (…-title.png, the rule's band beside it as
…-title-band.png) where a chip is drawn, each png with a .txt of the pointer, `status`, `chrome`
and the screen's text. A title the rule drew shorter than chrome's whole title is a FINDING.
manifest.tsv (harness, scene, look, scale, png, caption or the skip's reason) merges each run's
rows under a lock, so one folder holds 1x and 2x, Day and Night, from runs side by side. --scale is
the instance's (default 1); --columns and --lines its grid (400 x 40, so the rule's slot holds a
whole title). --out defaults to $LAB_STATE/autopilot-captures.

--baseline ATERM also takes one paired Gemini CLI typing take after the captures, or alone without
--harness (200 keys, then 200 backspaces, under one `video`, harness `continue = false`): on ATERM
first, then on this lab's aterm, back to back, the same lab binary, Gemini CLI, stub, keys and
pace (typing-gemini.txt). Take it on a quiet machine.

Each harness also gets <harness>-<scale>x-sheet.png (its frames at rest, a scene a row, Day then
Night) and <harness>-<scale>x-titles.png (its hovered titles' bands, stacked), drawn from the
folder's manifest; --sheets draws them again and runs nothing.

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
                            "unknown harness '{w}' (all, claude, agy, muse, gemini, codex-alab)"
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
            "--sheets" => a.sheets_only = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(usage(&format!("unknown option '{other}'"))),
        }
    }
    if a.whos.is_empty() && !a.typing_only && !a.sheets_only && a.baseline.is_none() {
        return Err(usage(
            "say --harness all|claude|agy|muse|gemini|codex-alab, or --baseline ATERM",
        ));
    }
    Ok(Some(a))
}

/// `autopilot-captures --harness all|claude|agy|muse|gemini|codex-alab [--scale 1|2] [--look
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
        return Ok(if typing_pair(&a, base, &out)? { 0 } else { 1 });
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
    if crate::run::is_run(&tag) {
        let _ = crate::lab::down(&tag, false);
    }
    let mut u = crate::lab::UpOpts::from_env();
    u.trust = true;
    u.lifeline = true;
    u.cols = a.cols.to_string();
    u.lines = a.lines.to_string();
    u.scale = Some(a.scale.to_string());
    u.aterm_toml = themed(&u.aterm_toml, look.theme());
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
    let script = forever::launcher(&run, h, &dirs, (false, false))?;
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
        let said = forever::launch_tab(&door, &sid, chain, &script)?;
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
    while tabs.iter().any(|(_, t)| !t.done) {
        for (chain, tab) in tabs.iter_mut().filter(|(_, t)| !t.done) {
            let settled = agent_settled(&door, tab.scene, &tab.sid, *chain);
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
            }
        }
        stop::sleep(Duration::from_secs(2))?;
    }
    Ok(())
}

/// Whether `scene`'s tab `sid` stands where its capture wants it, read off its `status` and screen.
fn agent_settled(door: &Door<'_>, scene: Scene, sid: &str, chain: Chain) -> bool {
    let st = forever::on(door, sid, &["status"]);
    let w = |k: &str| forever::word(&st, k).unwrap_or_default();
    let idle = w("agent") == "idle";
    let held = |leg: HoldLeg| w("land_held").split(',').any(|l| l == leg.word());
    match scene {
        Scene::CruiseNeeds => idle && w("needs_n") == "2",
        Scene::LandOpen => idle && held(HoldLeg::Open),
        Scene::LandRepo => idle && held(HoldLeg::Repo),
        Scene::Landed => w("task") == "landed",
        Scene::Off => {
            idle && w("autopilot") == chain.autopilot()
                && forever::on(door, sid, &["text"]).contains(forever::DONE_CLAIM)
        }
        Scene::SameLand | Scene::Cruise | Scene::Land | Scene::Need => false,
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
        Scene::CruiseNeeds | Scene::LandOpen | Scene::LandRepo | Scene::SameLand => false,
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
        let bytes = std::fs::read(png).ok()?;
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

/// `who`'s sheets in `out` from `rows` at `scale`, their names: its frames at rest as one grid
/// (`<harness>-<scale>x-sheet.png`, a scene a row, a look a column, each frame shrunk to at most
/// [`SHEET_CELL`] wide) and its hovered titles' bands stacked in the same order
/// (`<harness>-<scale>x-titles.png`).
fn sheets(out: &Path, who: Who, scale: u8, rows: &[Row]) -> Vec<String> {
    // Each of the harness's frames at `scale` of `kind`, read from the png its row names, or for
    // `band` its title's band: its scene, look and frame.
    let mine = |kind: Kind, band: bool| -> Vec<(Scene, Look, Frame)> {
        rows.iter()
            .filter(|r| r.kind() == Some(kind))
            .filter_map(|r| {
                let (w, scene, look, s) = r.keys()?;
                if w != who || s != scale {
                    return None;
                }
                let png = if band {
                    file_name(w, scene, look, s, kind, "-band.png")
                } else {
                    r.png.clone()
                };
                Some((scene, look, Frame::read(&out.join(png))?))
            })
            .collect()
    };
    let (gap, grey) = SHEET_GAP;
    let mut made = Vec::new();
    let rests = mine(Kind::Rest, false);
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
    let bands = mine(Kind::Title, true);
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

    /// How many cells the hovered title takes in the rule: from `lead` cells past the chip's
    /// (where the rule draws a title, past the chip's cells and two blanks) to the last cell that
    /// differs between `hovered` and `rest` before more than [`TITLE_GAP`] cells in a row that do
    /// not. 0 where none differs.
    fn title_cells(&self, hovered: &Frame, rest: &Frame, lead: usize) -> usize {
        let (y0, y1) = (pixel(self.y - self.ch / 2.0), pixel(self.y + self.ch / 2.0));
        let left = |i: usize| pixel(self.x + (cells_f(i) - 0.5) * self.cw);
        let mut last: Option<usize> = None;
        let mut i = lead.saturating_sub(1);
        while left(i) < hovered.w {
            let changed = hovered
                .changed(rest, (left(i), y0), (left(i + 1), y1))
                .unwrap_or(0)
                > 0;
            if changed {
                last = Some(i);
            } else if i > last.unwrap_or(lead) + TITLE_GAP {
                break;
            }
            i += 1;
        }
        last.map_or(0, |l| (l + 1).saturating_sub(lead))
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
fn raise(lab: &dyn Lab, label: &str) -> Result<String, String> {
    let strip = forever::surfaces::strip_tabs(&lab.said(&["chrome"]));
    let Some(at) = strip.iter().find(|t| t.label == label).map(|t| t.index) else {
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

/// The Gemini CLI typing take for 8a, paired: the baseline's arm first (this binary again, with
/// `--typing-only` and `LAB_ATERM` the baseline's aterm), then this lab's aterm, back to back;
/// both lines to `<out>/typing-gemini.txt`. Whether both arms were taken.
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
    let p = out.join("typing-gemini.txt");
    crate::sys::put(&p, 0o644, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("autopilot-captures: {}: {e}", p.display())))?;
    Ok(lines.iter().all(|l| l.starts_with(TAKEN)))
}

/// A taken arm's line starts so.
const TAKEN: &str = "typing gemini ";

/// An arm not taken starts so, and says why.
const NOT_TAKEN: &str = "typing NOT TAKEN:";

/// How the typing take is taken and paired, its file's first line.
const TYPING_SAYS: &str = "the Gemini CLI typing take for 8a: 200 keys (one `send` or `key \
    space` each, 50 ms apart) then 200 backspaces into the composer of a Gemini CLI under \
    Autopilot Cruise (harness continue = false, so nothing is typed into it), after the stub's \
    first reply, under one `video 60 keys` that must outlast the burst, on a private headless \
    lab 144x50 at 1x of its own; the baseline's arm first, then this tree's, back to back, the \
    same lab binary, Gemini CLI, stub, keys and pace, only the aterm differing; headless, so \
    frames are n_render with render_p*";

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

/// One Gemini CLI typing take on this process's aterm (`LAB_ATERM`): its `typing …` line.
fn typing_take(a: &Asked) -> Result<String, Fail> {
    let tag = if a.typing_only {
        "apcap-typing-base"
    } else {
        "apcap-typing"
    };
    if crate::run::is_run(tag) {
        let _ = crate::lab::down(tag, false);
    }
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
    let h = Harness::Gemini;
    let mut run = Run::load(tag)?;
    let (bin, ver) = h.bin_ver(&run);
    let (bin, ver) = (bin.to_string(), ver.to_string());
    if bin.is_empty() {
        return Err(Fail::exit(3, "no Gemini CLI in the lab (LAB_GEMINI at up)"));
    }
    run.agent = h.name().into();
    run.save();
    let chain = Chain::Cruise;
    let dir = forever::lay_repo(&run, chain).map_err(Fail::die)?;
    let dirs = std::slice::from_ref(&dir);
    let script = forever::launcher(&run, h, dirs, (false, false))?;
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
    let burst_over = AtomicBool::new(false);
    let (pct, video) = std::thread::scope(|s| {
        // One recording for the whole burst: it holds the window focused, as a person's typing
        // does. `video` answers when it ends: at `video stop` after the burst, or at once when
        // it is refused.
        let recording = s.spawn(|| {
            let said = door.ctl(&["video", "60", "keys"]).map_or_else(
                |e| format!("ERR {e}"),
                |r| format!("{}{}", r.stdout, r.stderr),
            );
            (said, burst_over.load(Ordering::Relaxed))
        });
        let typed = type_burst(&door, &sid);
        let pct = door.out(&["metrics", "percentiles"]);
        burst_over.store(true, Ordering::Relaxed);
        let _ = door.ctl(&["video", "stop"]);
        let video = recording
            .join()
            .unwrap_or_else(|_| ("ERR the recording's thread panicked".to_string(), false));
        typed.map(|()| (pct, video))
    })?;
    let said = recording_held(&video.0, video.1).map_err(Fail::die)?;
    let picked: Vec<String> = TAKE_KEYS
        .iter()
        .filter_map(|k| forever::word(&pct, k).map(|v| format!("{k}={v}")))
        .collect();
    Ok(format!(
        "{TAKEN}{ver} aterm {}: {}; video: {said}",
        run.aterm,
        picked.join(" ")
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

/// The burst: 200 keys, then 200 backspaces, 50 ms apart, after the metrics are reset.
fn type_burst(door: &Door<'_>, sid: &str) -> Result<(), Fail> {
    stop::sleep(Duration::from_secs(2))?;
    let _ = door.ctl(&["metrics", "reset"]);
    let keys = "the quick brown fox jumps over the lazy dog ";
    for c in keys.chars().cycle().take(200) {
        if c == ' ' {
            let _ = forever::on(door, sid, &["key", "space"]);
        } else {
            let _ = forever::on(door, sid, &["send", &c.to_string()]);
        }
        stop::sleep(Duration::from_millis(50))?;
    }
    for _ in 0..200 {
        let _ = forever::on(door, sid, &["key", "backspace"]);
        stop::sleep(Duration::from_millis(50))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "autopilot_captures_tests.rs"]
mod tests;
