// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Themes page's demo terminal: a scripted shell session played through a
//! real [`aterm_core::terminal::Terminal`] and drawn by the shipping renderer.

use std::sync::Arc;

use aterm_core::render::RenderInput;

use crate::settings_preview::{PreviewAnimation, PreviewTerminalTheme};

/// One write to the demo terminal, `at_ms` into the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DemoEvent {
    pub(crate) at_ms: u32,
    pub(crate) bytes: &'static [u8],
}

/// A typed character's own event, this far after the previous one.
const CHAR_MS: u32 = 55;
/// Return, after the last typed character.
const ENTER_MS: u32 = 250;
/// Each output line, after the previous write.
const LINE_MS: u32 = 35;
/// The pause before the next command's prompt.
const BETWEEN_MS: u32 = 900;
/// The hold on the finished session before the loop clears and restarts.
const END_HOLD_MS: u32 = 3_000;
/// How long before the loop's end the selection starts sweeping.
const SWEEP_LEAD_MS: u64 = 1_200;
/// The caret's blink half-period.
const BLINK_HALF_MS: u64 = 530;
/// The longest word the selection sweeps (`FAILED`); the sweep's cell count
/// is capped here so the paint key does not tick on past it.
const SWEEP_MAX_CELLS: u16 = 6;

/// The zsh session's prompt (macOS, Linux): the directory, the branch and its
/// dirty marker, then the caret line.
const ZSH_PROMPT: &[u8] = "\u{1b}[1;34m~/src/aterm\u{1b}[0m\u{1b}[90m on \u{1b}[0m\u{1b}[1;35mmain\u{1b}[0m\u{1b}[33m ✚2\u{1b}[0m\r\n\u{1b}[95m❯\u{1b}[0m ".as_bytes();

/// The PowerShell session's prompt (Windows): pwsh's own, in the text colour.
const PWSH_PROMPT: &[u8] = b"PS C:\\src\\aterm> ";

/// The shell session the demo plays: the one a terminal on this platform
/// opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemoSession {
    /// pwsh with PSReadLine's colours: `PS C:\src\aterm>` prompts, git, a
    /// cargo test run, a source listing and a comment.
    PowerShell,
    /// zsh with a starship-style prompt: git, a test run, a source listing, a
    /// directory listing and a comment.
    Zsh,
}

impl DemoSession {
    /// The session this platform's demo plays.
    pub(crate) const PLATFORM: Self = if cfg!(windows) {
        Self::PowerShell
    } else {
        Self::Zsh
    };

    const fn steps(self) -> &'static [Step] {
        match self {
            Self::PowerShell => PWSH_STEPS,
            Self::Zsh => ZSH_STEPS,
        }
    }

    /// The prompt the session prints before each command.
    pub(crate) const fn prompt(self) -> &'static [u8] {
        match self {
            Self::PowerShell => PWSH_PROMPT,
            Self::Zsh => ZSH_PROMPT,
        }
    }

    /// The session's tape, in order of `at_ms`.
    pub(crate) const fn tape(self) -> &'static [DemoEvent] {
        match self {
            Self::PowerShell => &PWSH_BUILT.0,
            Self::Zsh => &ZSH_BUILT.0,
        }
    }

    /// The length of one loop: the last write plus the closing hold.
    pub(crate) const fn loop_ms(self) -> u64 {
        (match self {
            Self::PowerShell => PWSH_BUILT.1,
            Self::Zsh => ZSH_BUILT.1,
        }) as u64
    }

    /// The index of the prompt that follows the test run.
    pub(crate) const fn settled_event(self) -> usize {
        settled_event(self.steps())
    }
}

/// One step of the script.
#[derive(Clone, Copy)]
enum Step {
    /// The prompt, `BETWEEN_MS` after the previous write (at once when it is
    /// the first write of the loop).
    Prompt,
    /// Characters typed one per event.
    Type(&'static [u8]),
    /// Bytes written with the next event and no delay of their own (a style
    /// switch before typing).
    Style(&'static [u8]),
    /// Return: the style reset and a new line, after the typing.
    Enter,
    /// One output line.
    Line(&'static [u8]),
    /// Hold the previous write this long before the next.
    Hold(u32),
    /// The end of the script: hold, so the finished session reads.
    End,
}

/// The zsh session: git, a test run, a source listing, a directory listing, a
/// comment that wraps, and a resting prompt.
const ZSH_STEPS: &[Step] = &[
    Step::Prompt,
    Step::Type(b"git status -sb"),
    Step::Enter,
    Step::Line(b"\x1b[32m## main...origin/main\x1b[0m \x1b[31m[ahead 1]\x1b[0m\r\n"),
    Step::Line(b" \x1b[31mM\x1b[0m crates/gui/theme.rs\r\n"),
    Step::Line(b"\x1b[32mA\x1b[0m  docs/themes.md\r\n"),
    Step::Line(b"\x1b[91m??\x1b[0m notes.txt\r\n"),
    Step::Prompt,
    Step::Type(b"targo test -q theme"),
    Step::Enter,
    Step::Line(b"\x1b[1;92m   Compiling\x1b[0m aterm-gui v0.9.0\r\n"),
    Step::Hold(600),
    Step::Line(b"\x1b[2mrunning 4 tests\x1b[0m\r\n"),
    Step::Line(b"test theme::parse ........ \x1b[32mok\x1b[0m\r\n"),
    Step::Line(b"test theme::pair ......... \x1b[32mok\x1b[0m\r\n"),
    Step::Line(b"test theme::contrast ..... \x1b[1;91mFAILED\x1b[0m\r\n"),
    Step::Line(b"\x1b[1;93mwarning:\x1b[0m \x1b[37munused import `Rgb`\x1b[0m\r\n"),
    Step::Line(
        b"\x1b[94m  --> \x1b[0m\x1b]8;;https://docs.rs/aterm-gui/0.9.0/src/theme.rs.html\x1b\\\x1b[4msrc/theme.rs:4:5\x1b[24m\x1b]8;;\x1b\\\r\n",
    ),
    Step::Line(b"\x1b[30;42m PASS \x1b[0m 3 passed  \x1b[97;41m FAIL \x1b[0m 1 failed\r\n"),
    Step::Prompt,
    Step::Type(b"bat -p src/theme.rs"),
    Step::Enter,
    Step::Line(b"\x1b[90m 1 \xe2\x94\x82 \x1b[3m// Pick the side the OS asks for\x1b[0m\r\n"),
    Step::Line(
        b"\x1b[90m 2 \xe2\x94\x82 \x1b[0m\x1b[35mfn\x1b[0m \x1b[34mside\x1b[0m(dark: \x1b[36mbool\x1b[0m) -> &str {\r\n",
    ),
    Step::Line(
        b"\x1b[90m 3 \xe2\x94\x82 \x1b[0m    \x1b[95mif\x1b[0m dark { \x1b[32m\"Night\"\x1b[0m } \x1b[95melse\x1b[0m { \x1b[32m\"Day\"\x1b[0m }\r\n",
    ),
    Step::Line(b"\x1b[90m 4 \xe2\x94\x82 \x1b[0m}\r\n"),
    Step::Prompt,
    Step::Type(b"ls"),
    Step::Enter,
    Step::Line(
        b"\x1b[1;34mdocs/\x1b[0m  \x1b[1;32mbuild*\x1b[0m  \x1b[35mlogo.png\x1b[0m  \x1b[96mlink@\x1b[0m  README.md\r\n",
    ),
    Step::Prompt,
    Step::Style(b"\x1b[2;90m"),
    Step::Type(b"# one theme by default; Follow system pairs one for light and one for dark"),
    Step::Enter,
    Step::Prompt,
    Step::End,
];

/// The PowerShell session: git, a cargo test run (rustc's Windows paths, its
/// bright cyan `help`), a source listing, a comment that wraps, and a resting
/// prompt.
const PWSH_STEPS: &[Step] = &[
    Step::Prompt,
    Step::Type(b"\x1b[93mgit\x1b[0m status \x1b[90m-sb"),
    Step::Enter,
    Step::Line(b"\x1b[32m## main...origin/main\x1b[0m \x1b[31m[ahead 1]\x1b[0m\r\n"),
    Step::Line(b" \x1b[31mM\x1b[0m crates/gui/theme.rs\r\n"),
    Step::Line(b"\x1b[32mA\x1b[0m  docs/themes.md\r\n"),
    Step::Line(b"\x1b[91m??\x1b[0m notes.txt\r\n"),
    Step::Prompt,
    Step::Type(b"\x1b[93mcargo\x1b[0m test \x1b[90m-q\x1b[0m theme"),
    Step::Enter,
    Step::Line(b"\x1b[1;92m   Compiling\x1b[0m aterm-gui v0.9.0\r\n"),
    Step::Hold(600),
    Step::Line(b"\x1b[2mrunning 4 tests\x1b[0m\r\n"),
    Step::Line(b"test theme::parse ........ \x1b[32mok\x1b[0m\r\n"),
    Step::Line(b"test theme::pair ......... \x1b[32mok\x1b[0m\r\n"),
    Step::Line(b"test theme::contrast ..... \x1b[1;91mFAILED\x1b[0m\r\n"),
    Step::Line(b"\x1b[1;93mwarning:\x1b[0m \x1b[37munused import `Rgb`\x1b[0m\r\n"),
    Step::Line(
        b"\x1b[94m  --> \x1b[0m\x1b]8;;https://docs.rs/aterm-gui/0.9.0/src/theme.rs.html\x1b\\\x1b[4msrc\\theme.rs:4:5\x1b[24m\x1b]8;;\x1b\\\r\n",
    ),
    Step::Line(b"\x1b[1;96mhelp\x1b[0m: remove the unused import\r\n"),
    Step::Line(b"\x1b[30;42m PASS \x1b[0m 3 passed  \x1b[97;41m FAIL \x1b[0m 1 failed\r\n"),
    Step::Prompt,
    Step::Type(b"\x1b[93mbat\x1b[0m \x1b[90m-p\x1b[0m src\\theme.rs"),
    Step::Enter,
    Step::Line(b"\x1b[90m 1 \xe2\x94\x82 \x1b[3m// Pick the side the OS asks for\x1b[0m\r\n"),
    Step::Line(
        b"\x1b[90m 2 \xe2\x94\x82 \x1b[0m\x1b[35mfn\x1b[0m \x1b[34mside\x1b[0m(dark: \x1b[36mbool\x1b[0m) -> &\x1b[33mstr\x1b[0m {\r\n",
    ),
    Step::Line(
        b"\x1b[90m 3 \xe2\x94\x82 \x1b[0m    \x1b[95mif\x1b[0m dark { \x1b[32m\"Night\"\x1b[0m } \x1b[95melse\x1b[0m { \x1b[32m\"Day\"\x1b[0m }\r\n",
    ),
    Step::Line(b"\x1b[90m 4 \xe2\x94\x82 \x1b[0m}\r\n"),
    Step::Prompt,
    Step::Type(b"\x1b[32m# one theme by default; Follow system pairs one for light and one for dark"),
    Step::Enter,
    Step::Prompt,
    Step::End,
];

/// The comment line, the one line allowed to wrap.
#[cfg(test)]
const WRAPPING_TEXT: &str =
    "# one theme by default; Follow system pairs one for light and one for dark";

/// The clear that opens every loop: home and erase, as `clear` does.
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";
/// What Return writes: the typed style's reset and a new line.
const ENTER: &[u8] = b"\x1b[0m\r\n";

/// The length of the first typed event of `text`: one character, with any
/// style switches (`ESC [ … m`) before it.
const fn typed_head(text: &[u8]) -> usize {
    let mut at = 0;
    while at + 1 < text.len() && text[at] == 0x1b && text[at + 1] == b'[' {
        at += 2;
        while at < text.len() && !text[at].is_ascii_alphabetic() {
            at += 1;
        }
        at += 1;
    }
    if at < text.len() { at + 1 } else { text.len() }
}

/// The number of events typing `text` makes: one per character.
const fn typed_events(text: &[u8]) -> usize {
    let mut count = 0;
    let mut rest = text;
    while let [_, ..] = rest {
        let (_, tail) = rest.split_at(typed_head(rest));
        count += 1;
        rest = tail;
    }
    count
}

/// The number of events `steps` makes.
const fn event_count(steps: &[Step]) -> usize {
    let mut count = 1;
    let mut index = 0;
    while index < steps.len() {
        count += match steps[index] {
            Step::Prompt | Step::Enter | Step::Line(_) | Step::Style(_) => 1,
            Step::Type(text) => typed_events(text),
            Step::Hold(_) | Step::End => 0,
        };
        index += 1;
    }
    count
}

/// The tape `steps` make with `prompt`, and the loop's length.
const fn build_tape<const N: usize>(
    steps: &[Step],
    prompt: &'static [u8],
) -> ([DemoEvent; N], u32) {
    let mut events = [DemoEvent {
        at_ms: 0,
        bytes: CLEAR,
    }; N];
    let mut next = 1;
    let mut at = 0_u32;
    let mut gap = 0_u32;
    let mut index = 0;
    let mut end = 0;
    while index < steps.len() {
        match steps[index] {
            Step::Prompt => {
                at += if next == 1 { 0 } else { BETWEEN_MS };
                events[next] = DemoEvent {
                    at_ms: at,
                    bytes: prompt,
                };
                next += 1;
                gap = CHAR_MS * 4;
            }
            Step::Style(bytes) => {
                at += gap;
                events[next] = DemoEvent { at_ms: at, bytes };
                next += 1;
                gap = 0;
            }
            Step::Type(text) => {
                let mut rest = text;
                while let [_, ..] = rest {
                    let (head, tail) = rest.split_at(typed_head(rest));
                    at += if gap == 0 { CHAR_MS } else { gap };
                    events[next] = DemoEvent {
                        at_ms: at,
                        bytes: head,
                    };
                    next += 1;
                    gap = CHAR_MS;
                    rest = tail;
                }
            }
            Step::Enter => {
                at += ENTER_MS;
                events[next] = DemoEvent {
                    at_ms: at,
                    bytes: ENTER,
                };
                next += 1;
                gap = LINE_MS;
            }
            Step::Line(bytes) => {
                at += gap;
                events[next] = DemoEvent { at_ms: at, bytes };
                next += 1;
                gap = LINE_MS;
            }
            Step::Hold(ms) => gap = ms,
            Step::End => end = at + END_HOLD_MS,
        }
        index += 1;
    }
    (events, end)
}

const ZSH_COUNT: usize = event_count(ZSH_STEPS);
const ZSH_BUILT: ([DemoEvent; ZSH_COUNT], u32) =
    build_tape::<ZSH_COUNT>(ZSH_STEPS, DemoSession::Zsh.prompt());
const PWSH_COUNT: usize = event_count(PWSH_STEPS);
const PWSH_BUILT: ([DemoEvent; PWSH_COUNT], u32) =
    build_tape::<PWSH_COUNT>(PWSH_STEPS, DemoSession::PowerShell.prompt());

/// The demo session ([`DemoSession::PLATFORM`]), in order of `at_ms`.
pub(crate) const DEMO_TAPE: &[DemoEvent] = DemoSession::PLATFORM.tape();

/// The prompt the demo session prints before each command.
#[cfg(test)]
const PROMPT: &[u8] = DemoSession::PLATFORM.prompt();

/// The length of one loop: the last write plus the closing hold.
pub(crate) const DEMO_LOOP_MS: u64 = DemoSession::PLATFORM.loop_ms();

/// The index of the prompt that follows the test run: the frame a resting
/// demo shows, holding the git status, the results, the warning and the
/// badges, with the caret on a fresh prompt (never under the badges on a
/// line of its own, which reads as a command still running).
pub(crate) const DEMO_SETTLED_EVENT: usize = DemoSession::PLATFORM.settled_event();

const fn settled_event(steps: &[Step]) -> usize {
    let mut events = 1;
    let mut prompts = 0;
    let mut index = 0;
    while index < steps.len() {
        match steps[index] {
            Step::Prompt => {
                prompts += 1;
                if prompts == 3 {
                    return events;
                }
                events += 1;
            }
            Step::Enter | Step::Line(_) | Step::Style(_) => events += 1,
            Step::Type(text) => events += typed_events(text),
            Step::Hold(_) | Step::End => {}
        }
        index += 1;
    }
    events - 1
}

/// Where the tape stands when the page opens: on the settled frame, so the
/// demo opens on the git status, the test run and its badges rather than an
/// empty screen, plays on from there, and a demo that comes to rest shows the
/// frame it opened on.
pub(crate) const DEMO_OPEN_PHASE_MS: u64 = DEMO_TAPE[DEMO_SETTLED_EVENT].at_ms as u64;

/// The last event at or before `phase` (a loop phase, `< DEMO_LOOP_MS`).
pub(crate) fn event_index(phase: u64) -> usize {
    let phase = phase % DEMO_LOOP_MS;
    DEMO_TAPE
        .partition_point(|event| u64::from(event.at_ms) <= phase)
        .saturating_sub(1)
}

/// What a frame at `phase` shows: the last event fed, whether the caret is
/// lit, and how many cells of the closing selection are swept.
pub(crate) fn frame_key(phase: u64, playing: bool) -> (usize, bool, u16) {
    if !playing {
        return (DEMO_SETTLED_EVENT, true, 0);
    }
    let phase = phase % DEMO_LOOP_MS;
    let sweep_start = DEMO_LOOP_MS - SWEEP_LEAD_MS;
    let sweep = if phase >= sweep_start {
        (((phase - sweep_start) / CHAR_MS as u64) + 1).min(u64::from(SWEEP_MAX_CELLS)) as u16
    } else {
        0
    };
    (
        event_index(phase),
        (phase / BLINK_HALF_MS).is_multiple_of(2),
        sweep,
    )
}

/// The cadence a playing demo needs at `phase`: one wake at the next write,
/// caret edge or sweep step, whichever comes first.
pub(crate) fn demo_animation(phase: u64, playing: bool) -> PreviewAnimation {
    if !playing {
        return PreviewAnimation::None;
    }
    let phase = phase % DEMO_LOOP_MS;
    let next_event = DEMO_TAPE
        .get(event_index(phase) + 1)
        .map_or(DEMO_LOOP_MS, |event| u64::from(event.at_ms));
    let next_blink = (phase / BLINK_HALF_MS + 1) * BLINK_HALF_MS;
    let sweep_start = DEMO_LOOP_MS - SWEEP_LEAD_MS;
    let next_sweep = if phase < sweep_start {
        sweep_start
    } else {
        sweep_start + ((phase - sweep_start) / CHAR_MS as u64 + 1) * CHAR_MS as u64
    };
    let next = next_event.min(next_blink).min(next_sweep).min(DEMO_LOOP_MS);
    PreviewAnimation::BlinkEdge {
        after_ms: next.saturating_sub(phase).max(1),
    }
}

/// Whether a demo `idle_ms` past the last input on its page rests: three
/// whole loops with nothing touched, after which it shows the settled frame
/// and schedules nothing.
pub(crate) const fn demo_rests(idle_ms: u64) -> bool {
    idle_ms >= 3 * DEMO_LOOP_MS
}

/// The colours a demo frame is drawn in: the candidate's palette plus the
/// selection foreground the configuration asks for, and the theme-pair
/// contrast floor the terminal holds those colours to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DemoColors {
    pub(crate) theme: PreviewTerminalTheme,
    pub(crate) selection_foreground: Option<u32>,
    pub(crate) bold_is_bright: bool,
    /// `TerminalConfig::indexed_minimum_contrast` as its bits: the floor the
    /// live terminal applies to text in the theme's own colours
    /// (`Config::indexed_minimum_contrast_or_default`).
    floor_bits: u32,
}

impl DemoColors {
    /// The demo's colours, with the theme-pair floor `floor` (1.0 is off).
    pub(crate) const fn new(
        theme: PreviewTerminalTheme,
        selection_foreground: Option<u32>,
        bold_is_bright: bool,
        floor: f32,
    ) -> Self {
        let floor = if floor.is_finite() {
            floor.clamp(1.0, 21.0)
        } else {
            1.0
        };
        Self {
            theme,
            selection_foreground,
            bold_is_bright,
            floor_bits: floor.to_bits(),
        }
    }

    /// The theme-pair contrast floor the frame is drawn under.
    pub(crate) const fn floor(&self) -> f32 {
        f32::from_bits(self.floor_bits)
    }

    fn config(&self) -> aterm_core::config::TerminalConfig {
        let rgb = |packed: u32| {
            aterm_types::Rgb::new(
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            )
        };
        let mut palette = aterm_types::ColorPalette::new();
        for (index, color) in self.theme.ansi.into_iter().enumerate() {
            palette.set(index as u8, rgb(color));
        }
        aterm_core::config::TerminalConfig {
            default_foreground: rgb(self.theme.fg),
            default_background: rgb(self.theme.bg),
            cursor_color: Some(rgb(self.theme.cursor)),
            selection_background: Some(rgb(self.theme.selection)),
            selection_foreground: self.selection_foreground.map(rgb),
            bold_is_bright: self.bold_is_bright,
            indexed_minimum_contrast: self.floor(),
            custom_palette: Some(palette),
            cursor_style: aterm_core::terminal::CursorStyle::SteadyBlock,
            cursor_blink: false,
            ..aterm_core::config::TerminalConfig::default()
        }
    }

    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = aterm_hash::FxHasher::default();
        self.hash(&mut hash);
        hash.finish()
    }
}

/// The incrementally fed demo terminal, kept per thread.
struct DemoTape {
    colors: u64,
    /// The theme-pair floor the terminal was configured with; a rebase does
    /// not carry one, so another floor starts a new tape.
    floor_bits: u32,
    rows: usize,
    cols: usize,
    /// How many events have been written.
    fed: usize,
    term: aterm_core::terminal::Terminal,
    /// The last frame, by its key.
    frame: Option<(u64, Arc<RenderInput>)>,
}

impl DemoTape {
    fn new(colors: &DemoColors, rows: usize, cols: usize) -> Self {
        let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
        let _ = term.apply_config(&colors.config());
        Self {
            colors: colors.fingerprint(),
            floor_bits: colors.floor_bits,
            rows,
            cols,
            fed: 0,
            term,
            frame: None,
        }
    }
}

thread_local! {
    /// Separate from the Appearance specimen's memo, so the two pages never
    /// evict each other.
    static TAPE: std::cell::RefCell<Option<DemoTape>> = const { std::cell::RefCell::new(None) };
}

/// The key a frame is memoized under.
///
/// INVARIANT: every value the frame build below consults appears here — the
/// colours, the grid, the event fed, the caret and the sweep. Equal keys give
/// identical frames, so the memo returns the previous `Arc` for them.
fn memo_key(colors: &DemoColors, rows: usize, cols: usize, key: (usize, bool, u16)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = aterm_hash::FxHasher::default();
    colors.fingerprint().hash(&mut hash);
    rows.hash(&mut hash);
    cols.hash(&mut hash);
    key.hash(&mut hash);
    hash.finish() | 1
}

/// The demo frame at `phase` on a `rows × cols` grid, and its fingerprint.
pub(crate) fn demo_frame(
    colors: &DemoColors,
    phase: u64,
    playing: bool,
    rows: usize,
    cols: usize,
) -> (Arc<RenderInput>, u64) {
    let rows = rows.clamp(1, 64);
    let cols = cols.clamp(8, 160);
    let key = frame_key(phase, playing);
    let fingerprint = memo_key(colors, rows, cols, key);
    TAPE.with_borrow_mut(|slot| {
        if let Some((was, input)) = slot.as_ref().and_then(|tape| tape.frame.as_ref())
            && *was == fingerprint
        {
            return (Arc::clone(input), fingerprint);
        }
        let (index, caret_on, sweep) = key;
        let tape = match slot {
            Some(tape)
                if tape.rows == rows
                    && tape.cols == cols
                    && tape.floor_bits == colors.floor_bits
                    && tape.fed <= index + 1 =>
            {
                tape
            }
            _ => slot.insert(DemoTape::new(colors, rows, cols)),
        };
        let fingerprint_colors = colors.fingerprint();
        if tape.colors != fingerprint_colors {
            let baseline = aterm_core::terminal::ColorBaseline::of_config(&colors.config());
            let _ = tape.term.rebase_colors(&baseline);
            tape.colors = fingerprint_colors;
        }
        for event in &DEMO_TAPE[tape.fed..=index] {
            tape.term.process(event.bytes);
        }
        tape.fed = index + 1;
        let mut input = tape.term.cell_frame(rows, cols);
        input.cursor_visible = caret_on;
        if sweep > 0
            && let Some((row, col, len)) = sweep_target(&input)
        {
            use aterm_core::selection::{SelectionSide, SelectionType};
            let mut selection = aterm_core::selection::TextSelection::new();
            let last = col + sweep.min(len) - 1;
            selection.start_selection(row as i32, col, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(row as i32, last, SelectionSide::Right);
            selection.complete_selection();
            input.selection = selection;
        }
        let input = Arc::new(input);
        tape.frame = Some((fingerprint, Arc::clone(&input)));
        (input, fingerprint)
    })
}

/// Where the closing selection sweeps, read off the frame: the newest
/// `FAILED` on screen, else the newest `ok`, else the prompt's branch.
fn sweep_target(input: &RenderInput) -> Option<(usize, u16, u16)> {
    let find = |word: &str| {
        input
            .cells
            .iter()
            .enumerate()
            .rev()
            .find_map(|(row, cells)| {
                let text: String = cells.iter().map(|cell| cell.ch).collect();
                text.find(word).map(|byte| {
                    let col = text[..byte].chars().count();
                    (row, col as u16, word.chars().count() as u16)
                })
            })
    };
    find("FAILED")
        .or_else(|| find("ok"))
        .or_else(|| find("main"))
}

/// `session`'s tape with one event left out, so a test can show a check
/// notices what the event carried.
#[cfg(test)]
fn tape_without(session: DemoSession, byte_marker: &[u8]) -> Vec<DemoEvent> {
    session
        .tape()
        .iter()
        .copied()
        .filter(|event| {
            !event
                .bytes
                .windows(byte_marker.len())
                .any(|w| w == byte_marker)
        })
        .collect()
}

#[cfg(test)]
#[path = "settings_theme_demo_tests.rs"]
mod tests;
