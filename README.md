<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

<h1 align="center">aterm</h1>

<p align="center">
  <strong>The batteries-included terminal for AI.</strong><br>
  A real GPU terminal for people, an authenticated control surface for agents,
  the <a href="https://alab.systems">ALab</a> verification toolchain one
  command away — and a cat.
</p>

<p align="center">
  <a href="https://github.com/alabsystems/aterm/releases"><img alt="latest release" src="https://img.shields.io/github/v/release/alabsystems/aterm?filter=v*&label=release"></a>
  <a href="LICENSE"><img alt="license" src="https://img.shields.io/github/license/alabsystems/aterm"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#why-aterm">Why aterm?</a> ·
  <a href="#made-for-humans-and-agents">Humans + agents</a> ·
  <a href="#fun-is-a-feature">Fun</a> ·
  <a href="#the-alab-toolchain">Toolchain</a> ·
  <a href="#use-aterm-in-your-own-project">Embed it</a> ·
  <a href="#security-model">Security</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-reduced-motion: reduce)" srcset="assets/aterm-ai-workbench.png">
    <img src="assets/aterm-ai-workbench.gif" width="1000" alt="A real aterm window: a coloured banner above a shell prompt, with the rainbow kitty running ahead of typed text on its rainbow ribbon">
  </picture>
  <br><sub>Captured by aterm itself — <code>aterm ctl window</code> and <code>aterm ctl video</code>.</sub>
</p>

## Install

The recommended install is one line:

```sh
curl -fsSL https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh | bash
```

**~27 MB download. aterm opens immediately; the ALab toolchain installs itself
on first launch with live progress** — each program downloads individually and
resumably, only the builds for your machine, visible end to end in the app.

The script picks the newest app release, checks the download's SHA-256 against
the release manifest, verifies the app's Developer ID signature and
notarization, puts `aterm.app` in `/Applications` (or `~/Applications`), and
links the `aterm` command and its man pages under `~/.local`. `--no-toolchain` keeps the packages
off (`aterm pkg install --default-set` later), `--no-path` leaves your shell
profile untouched, `--dry-run` prints the whole plan — elected release, asset,
every destination, every edit — and writes nothing, and `--uninstall` reverses
everything it installed.

aterm ships for macOS 11+ as a signed, notarized universal app (Apple silicon
and Intel), and for Linux x86_64 as a tarball on the same releases, from the
public release channel at
[github.com/alabsystems/aterm/releases](https://github.com/alabsystems/aterm/releases).
The channel carries two kinds of cut: **app releases** ship the containers
below plus the signed `aterm-appcast.toml` manifest, and **source releases**
(e.g. v0.62.0, v0.64.0) carry only a signed source manifest — the installer
and the in-app updater elect the newest app release and skip source cuts.
Every macOS app release is the same app in two containers:

- **`aterm-X.Y.0.dmg` (~30 MB) — the download.** The signed, notarized app
  alone, as a drag-install image: drag `aterm.app` into Applications.
- **`aterm-X.Y.0-mac.zip` (~27 MB) — the same app, zipped.** The container the
  in-app updater, the Homebrew cask, and `install.sh` consume.

There is one image, not a family: the batteries-included seeded images, the
Intel-only DMG, and `install.sh --batteries` were retired by owner decision on
2026-08-26, and the flag now refuses. An air-gapped machine can still install the
app from a DMG, but the toolchain comes from the network index — offline
provisioning is not offered. Already-published releases keep the assets they
shipped with: `aterm-<version>.dmg` is the ~1.07 GB seeded image in every
release through **v0.63.0** (through v0.61.0 the lean image shipped beside it as
`aterm-<version>-lite.dmg`), so the first lean `aterm-<version>.dmg` is the next
cut from `main`.

A release from the one-download lane also carries the
`releases/latest/download/` names `aterm.dmg` and `aterm-mac.zip` — but those
resolve only while an app release holds GitHub's `latest` pointer; when a
source release holds it (as v0.64.0 does today) they return 404, so reach for
the versioned assets on the newest app release, or `install.sh`, which elects
that release itself. Every container has a `.sha256` sidecar whose digest also
appears in that release's `aterm-appcast.toml` (v0.63.0, cut by an older
cutter, carries `aterm.dmg` but no `aterm-mac.zip`). With the sidecar beside
the asset, and the app in place:

```sh
shasum -a 256 -c aterm-<version>.dmg.sha256
codesign --verify --strict --verbose=2 /Applications/aterm.app
spctl -a -t exec -vv /Applications/aterm.app
```

The last must report `source=Notarized Developer ID`.

On Linux x86_64 the same command installs the release's
`aterm-<version>-linux-x86_64.tar.gz`, checked against its `.sha256` digest
asset before anything is unpacked. The tarball is built from the release commit
on upstream stable Rust; it sits outside the release manifest (the appcast is
macOS-only for now) and has no Developer ID or notarization counterpart, so its
trust is TLS plus that digest — a weaker chain than the macOS one. There is no
self-updater on Linux: re-run the installer to update.

### Homebrew

The cask route installs the same signed, notarized app from the release's
`mac.zip`, via ALab's tap:

```sh
brew install --cask alabsystems/tap/aterm
```

The cask declares `auto_updates`, because the installed app keeps itself current
through its own verified updater (see [Staying current](#staying-current));
`brew upgrade --cask --greedy aterm` makes Homebrew do it instead.
`brew install --cask alabsystems/tap/alab` lands the same app under the
toolchain's name — install one or the other, not both.

### Staying current

Installed copies keep themselves current: the app checks the release channel in
the background, verifies the new build, and swaps it in at a quiet moment —
every window, tab, split, and live shell survives, and if the handoff cannot
complete the update lands at the next launch. Settings ▸ Software Update shows
what is staged plus the release notes; `aterm ctl update status` says the same
on the command line, and `aterm update` is the headless lane for a machine with
no window open. `ATERM_NO_AUTO_UPDATE=1` turns the updater off;
`[update] auto_apply = false` in `aterm.toml` stages the build and leaves
applying it to you (Software Update, `aterm ctl update apply`, or the next
launch). This lane is macOS-only; a Linux install stays current by re-running
the installer.

### Build from source

This repository is a snapshot of the private development line, cut with each
release, and pins a stock Rust toolchain. On macOS with the Xcode Command Line
Tools installed:

```sh
git clone https://github.com/alabsystems/aterm.git
cd aterm
cargo build --locked -p aterm
./target/debug/aterm --window
```

`./target/debug/aterm --version` should agree with `[workspace.package]
version` in the root `Cargo.toml`. Build from the workspace — aterm's crates are
not on crates.io, so there is nothing to `cargo install`. Linux and Windows
build from source too and have in-tree build lanes and tests; Linux x86_64 also
has a released tarball behind the same installer, but only macOS has signed,
notarized binaries and the self-updater, and a source build is a real aterm, not
the byte-identical notarized release.

The name has neighbours, so aim carefully: the crates.io package named `aterm`,
the `aterm` in MacPorts and old `brew install aterm` guides (an X11 terminal
from 2007), and the aterm.ai and aterm.io domains are all unrelated projects.
This aterm is [ALab](https://alab.systems)'s terminal — the code lives here, and
the releases live on this repository's
[Releases](https://github.com/alabsystems/aterm/releases) page.

## Why aterm?

Most terminals are excellent human interfaces. aterm keeps that familiar PTY and
adds a second, authenticated surface that software can understand: text, styled
cells, pixels, events, sessions, input, synchronization, and live metrics. That
makes the terminal a shared workbench — a person types normally while an agent
observes structured state, drives a turn, waits for a real transition, or
captures the rendered result, and shells, TUIs, REPLs, editors, and agent CLIs
still see an ordinary terminal.

```mermaid
flowchart LR
    H[Human] <-->|keyboard · mouse · pixels| A[aterm]
    G[AI agent] <-->|authenticated local control| A
    A <-->|ordinary PTY bytes| P[Shells · TUIs · agent CLIs]
    A -->|dispatch installed packages| T[Compilers · solvers · verifiers]
    A --> E[aterm engine]
    O[Your project] -->|Rust · C · WASM embed| E
```

**And it is fun.** The window looks alive, and the cursor has a companion with
opinions about your typing. ALab, the group behind aterm, also ships its
compilers, solvers, and verification tools through it — one `aterm pkg install`
away.

| Battery | What aterm adds | Important boundary |
| --- | --- | --- |
| **Observe** | Plain text, lossless styled cells, cursor, modes, shell command blocks, session status, a lifecycle timeline, and full-history search | Window and headless modes only |
| **Drive** | Text, keys, paste, mouse, focus, resize, selection, clipboard, tabs, menu actions, and native Settings | Signals are a separate operation, never faked keystrokes |
| **Coordinate** | Event-driven `await`, `ready`, `wait`, whole-turn settlement, and cooperative drive leases | `turn` waits for the screen to settle, not for the process to exit; `wait` needs OSC 133 marks |
| **Stream** | `screen`, `cursor`, `cells`, `bytes`, `events`, and `sessions` subscriptions, plus fleet-wide NDJSON | Gaps are marked, never hidden |
| **Capture** | PNG frames, full-window artifacts with native chrome (macOS), bounded asciicast history, opt-in temporal replay, and GPU swapchain-tap video | Records what aterm rendered, not what the display showed |
| **Measure** | Live render, frame, input, and application-present-return counters with percentiles and startup phases | Software-side counters; GPU completion and the display are outside |
| **Extend** | Themes, typography, wallpaper, Trail Packs, Toy Packs, keybindings, and the Rust/C/WASM engine source | Path dependencies only — no crates.io, no API stability yet |
| **Delight** | The rainbow kitty pet, per-program cats, Robi the helper robot, a shelf of cursor trails, Sparkle Words, Matrix rain, and a typing-sound synth | Synth audio is macOS-only; decorative effects are opt-in on Windows; some halos need the GPU |

### One binary, distinct modes

aterm ships as one executable. Invocation chooses the surface:

| Invocation | Result | External control socket |
| --- | --- | --- |
| `aterm` in a TTY | Transparent PTY session | No |
| `aterm` with no TTY on stdin (a Finder launch) | Terminal window | Yes, by default |
| `aterm --window` | Terminal window, explicitly | Yes, by default |
| `aterm --headless` | Terminal engine without a window | Yes, by default |
| `aterm --session` | Force the PTY session in a pipe or CI | No |

The same binary answers the front-door verbs — `ctl`, `conn`, `pkg`, `fleet`,
`drive`, `update`, `agents`, `new-tab`, `new-window`, `split-pane` — plus
`aterm help`, the diagnostic words (`doctor`, `show-config`, `validate-config`,
`list-themes`, …), and managed-tool dispatch. Compatibility names such as
`aterm-ctl`, `atpkg`, `aterm-fleet`, and `aterm-drive` are symlinks onto that
binary, not separate products, and the app bundle ships them beside it.

The plain TTY session deliberately serves no socket — use window or headless mode
when another process needs to observe or drive a session. Those modes enable
control by default, disable it explicitly with `--no-control-sock`, and fail
closed if a secure socket cannot be created.

## Made for humans and agents

aterm does not make an agent impersonate a person by scraping pixels and hoping
the timing works. Agents speak a local control protocol outside the PTY byte
stream; the text and keys they send enter through the same input path a person's
keystrokes do, and the person's keyboard never routes through the agent — so a
human can type at any time, even mid-turn.

Coding agents learn that aterm exists without anyone running anything: each
session aterm opens detects the coding agents on the machine — Claude Code,
Codex CLI, Gemini CLI, and OpenCode — and keeps a short primer current in each
one's global context file (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, …).
The primer only activates when the agent finds itself inside aterm; Claude Code
also gets bundled `drive-aterm` and `supervise-agent` skills. An agent launched inside an aterm
window or headless instance then knows to detect it (`TERM_PROGRAM=aterm` /
`ATERM_CHILD=1`), to run `aterm help` — which there prints the agent operating
brief with the caller's own session ID already filled in — and to list the
windows and sessions and read a peer's `status` before typing into it.

```sh
aterm agents install          # the same pass on demand; name an agent to force it
```

`aterm agents` shows status, `aterm agents remove` uninstalls,
`agents_auto_prime = false` in `aterm.toml` stops the automatic pass, and
`aterm help agents` explains the mechanism.

Two facts agent builders ask about first:

- **Env hygiene is unconditional.** Every shell aterm spawns has `CLAUDE*`,
  `ANTHROPIC_*`, `COPILOT_*`, `CODEX_*`, `CURSOR_*`, `AI_*`, and `_DEVTOOL_*`
  stripped, along with aterm's own socket, containment, and network selectors,
  so an outer agent's context never leaks into an inner session.
- **There is no MCP server, by design.** The integration surfaces are the CLI
  (`aterm ctl`, `aterm conn`, `aterm drive`, `aterm fleet`) and the Rust library
  API.

Beyond the batteries listed under [Why aterm?](#why-aterm), the control-enabled
window and headless modes provide:

- **Session state without reading the screen:** a per-session `status`
  (idle / running / quiet / exited, with confidence), a `timeline` of lifecycle
  events, and user-settable `meta` such as a typed `role` and a needs-human
  `attention` flag.
- **A session fabric:** stable session IDs, exact `@sid` routing across every
  same-user instance, `@self` for your own session, a hard per-session lease
  during a `turn` and a cooperative `lease` for raw drivers, multi-session
  subscriptions, and `aterm fleet events` / `aterm fleet exec` to federate a
  fleet over one descriptor. `aterm conn` goes further and wires one session to
  pull another's screen and push keystrokes into it as standing wiring, so a
  supervisor session supervises any interactive tool, unmodified.

### CLI tour

Open a window — with your shell, or running one command:

```sh
aterm --window
aterm --window -e htop        # -d <dir>, --hold, --shell pwsh also exist
```

From another shell, inspect and drive its active session:

```sh
aterm ctl text                  # plain visible rows
aterm ctl screen                # lossless styled-grid JSON
aterm ctl status                # phase, outcome, and confidence for this session
aterm ctl image frame.png       # the rendered client frame, written on the aterm host
aterm ctl window shot.png       # the whole window, with its native chrome (macOS)
aterm ctl turn 'git status'     # type, submit, settle, and return the screen
aterm ctl wait                  # block until the running command completes (OSC 133)
aterm ctl metrics               # live render and latency counters
aterm ctl help text             # one verb's full entry; bare `help` is the short catalog, `--full` all of it
```

A verb with no session address targets the active tab. From inside an aterm
session, `@self` is your own pane; for automation, pick a session by ID or mint
one:

```sh
aterm ctl windows                              # one row per window: which sessions sit on its active tab
aterm ctl ls                                   # every session of every live instance, with window= and detail= (what it runs)
#   wfocus=1 marks aterm's most recently focused window (a minimize or the app deactivating does not clear it)
sid=$(aterm ctl spawn window=1 | cut -d' ' -f2)   # a fresh tab in window 1, immediately addressable — not raised
#   (an unknown id is refused by name; a --headless instance owns logical window 0, the one `ls` and `windows` show)
aterm ctl "@$sid" turn 'make test'
aterm ctl "@$sid" await match 'result:'        # block until a regex appears (one token)
aterm ctl subscribe "@$sid" events,cursor      # targets follow the verb here; add screen,cells,bytes as needed
aterm ctl "@$sid" close
```

`turn` types, submits, verifies the submit landed, settles, and returns the
screen plus a deterministic hash. `await` takes one of four predicates —
`idle <ms>`, `seq [<n>]`, `match <re>`, `block` — with an optional
`timeout=<ms>`; `await seq` is level-triggered, so `await seq <n> timeout=0` is
a cheap one-shot "did anything change?" check, and any timeout exits with code
124 so a script can tell "not yet" from "failed".

Socket discovery is automatic for ordinary local use: `--sock`, `--pid`,
`$ATERM_CONTROL_SOCK`, then the instance hosting your own terminal, then the
per-user socket that always points at the newest instance. `aterm ctl help`
prints the short verb catalog from a running instance, `aterm ctl help <verb>`
one verb's full entry, and `aterm help introspection` the full catalog
anywhere. All of them are generated from the one typed verb table the server
answers from, so they cannot drift. Coding agents are primed automatically:
every session aterm opens installs the primer for each detected agent
(`agents_auto_prime = false` in `aterm.toml` turns that off).

## Fun is a feature

The default cursor is the **rainbow kitty pet**: a full-body cat that walks,
runs, and pounces along your line, trailing a banded rainbow ribbon (with a
glass-bell typing sound on macOS). It cheers a green build, sulks at a failed
one, and chases your mouse. A new kitty is generated every time aterm starts, and
any program that holds the foreground for more than a few seconds earns its own
cat — Claude Code, Codex, and everything else you run gets a look of its own.
View ▸ Favourite This Kitty pins the one you like.

<p align="center">
  <img src="assets/aterm-rainbow-kitty.png" width="880" alt="Close-up from a real aterm frame: the rainbow kitty and its ribbon under a typed command">
</p>

The pet answers to `rainbow kitty pet`, `rainbow kitty`, and bare `kitty` — all
three are the same animal; the original flying head has its own name now,
`rainbow kitty flying` (the historical `nyan` aliases still select it). The rest
of the shelf: `rainbow dog pet`, the ribbon geometries `rainbow kitty underline`
and `rainbow kitty tall`, then `phaser`, `comet`, `lumen`, `sparkle`, `fire`,
`laser`, `water`, `beam`, and `off` — or load a Trail Pack, a TOML cursor trail
composed from the built-in beam, crown, particle, and ramp primitives, no restart
needed. Every trail has a signature typing sound on macOS, and
Settings ▸ Cursor & Motion ▸ Sound picks any instrument — glass bell, droplet,
typewriter, marimba, felt — regardless of the trail on screen, or leaves it on
`auto` to follow it.

The rest of the roster:

- **Robi**, a tip-sharing helper robot, walks your typed row, does jumping jacks
  while showing tips, and swings across the tab bar. Off by default — invite him
  with `robi = true` in `aterm.toml` (or the Settings toggle). Once invited, type
  `robi` and he hustles over with a fresh tip; click him to retire him again.
- **Sparkle Words** give typed words animated ink. Profanity gets rainbow ink
  with a bonk (and sometimes escalates to a nova); hundreds of animal words —
  in English, Chinese, Japanese, Korean, and more — show their animal on the
  word; `kitty` summons a cat, and `dog`, once you have typed enough, summons a
  dog. Toy Packs add your own word effects from TOML. Everything is purely
  visual: copied text, logs, and recordings read exact bytes.
- **Matrix rain** falls only in empty cells under the text, follows what the
  session is doing, and toggles per session from View ▸ Matrix Rain or
  `aterm ctl rain toggle`. Off by default.
- **Sing-along:** hold a key on the rainbow trail and the cat goes full chorus —
  maximal ribbon flow, a dancing singing face, rising notes, and an original
  chiptune riff that crossfades away when you stop. Every key sings its own
  verse; switch keys and the song modulates onto the new one.

Fun remains controllable. `motion = auto|full|reduced` follows the OS Reduce
Motion setting and, under reduced motion, stops every governed animation
outright. `serious_mode` (View ▸ Serious Mode) mutes every effect sound and hides
every decorative effect in one switch without changing the terminal's functional
behavior, and each trail, sound, and toy has its own switch. The ambient sound
bed is opt-in and off by default, and on Windows the decorative effects are
opt-in as a whole (`cursor_trail = true`).

## A real terminal underneath

The engine handles modern shell and TUI workloads: Unicode grapheme clusters,
emoji sequences, wide and combining characters, bidi visual reordering, the
kitty keyboard protocol, OSC 8 hyperlinks (Cmd-click on macOS, Ctrl-click
elsewhere; hovering discloses the destination before you can open it), OSC 133
shell marks (injected automatically for zsh, bash, fish, and PowerShell), styled
underlines, 24-bit colour, synchronized output, bracketed paste, OSC 52
clipboard writes, and inline graphics — sixel, iTerm2 images, and the core kitty
graphics paths.

Scrollback is a tiered store (hot RAM, warm LZ4, cold zstd) that defaults to
100,000 lines under a byte budget. Predictive local echo (mosh-style) paints
typed glyphs ahead of a slow shell's echo — ssh, a loaded box — and stays out of
alt-screen and password contexts. Box drawing, blocks, braille, and Powerline
separators are generated from cell geometry so adjacent shapes meet cleanly.
When nothing is animating and no deadline is pending, the event loop parks.

The window renders through wgpu — Metal on macOS, Vulkan on Linux, DX12 on
Windows — with the CPU rasterizer as automatic fallback or explicit `--cpu`
choice; a parity suite renders the same frames both ways and holds them to a
small channel tolerance. On macOS the titlebar is the tab strip, the menu bar is
a real menu bar, and a `❯` status item in the system menu bar gives an operator
glance across sessions and instances.

Tabs, split panes, and multiple windows live in one process; tabs carry busy and
attention badges (a failed command flags its tab), the focused pane is marked in
the split divider, and the workspace is restored across quit and relaunch. Cmd-,
opens native Settings as a tab (Appearance, Wallpaper, Text & Fonts, Cursor &
Motion, Cursor Kitty, Window, Tab Color, Keyboard & Input, Terminal, Security,
Software Update, Packages, About); Markdown files and a native Editor open as
tabs too. Cmd-F searches screen and scrollback with regex, Shift-Cmd-P opens the
command palette, and the built-in chords are rebindable through `[keybindings]`
(`aterm --window --list-actions` prints the set). Twelve colour themes are built
in (`aterm list-themes`), `theme = "dark:…,light:…"` follows the OS appearance,
and `~/.config/aterm/themes/*.conf` adds your own. Typography covers ligatures,
OpenType features, variable-font weight, ordered fallback fonts, and bundled
display faces.

Accessibility is honest about its cost: a Linux build carries the AccessKit tree
unconditionally, so a screen reader gets the grid and the Settings tree there by
default. On macOS and Windows that tree left the default build in August 2026 —
it was the largest third-party dependency surface aterm could retire — and stays
available as a build feature (`a11y-accesskit`, or the aterm-owned
NSAccessibility publisher `a11y-appkit` on macOS).

## The ALab toolchain

aterm is the front door to ALab's self-owned verification toolchain: the `trust`
compiler (a Rust compiler that verifies what it compiles), the `ay` solver, the
`ty` specification checker, the `clean` theorem prover, and the `ny` and `nn`
neural-network tools. The same binary owns package management and dispatch, so
`aterm ay` means the same tool on every machine:

```sh
aterm pkg install ay        # from the signed public index
aterm pkg install trust     # the compiler bundle: trustc, targo, tippy, …
aterm ay --help
aterm trustc --help
aterm pkg list
```

`aterm <tool>` resolves against the managed store — never `$PATH` — so aterm's
own verbs cannot be shadowed. On `$PATH` itself the managed tools come last, so
a `trust`, `ty` or `clean` you already had (Homebrew's p11-kit ships a `trust`;
Homebrew core has formulae named `ty` and `clean`) keeps winning — `alab-<tool>`
(`alab-trust`, `alab-ty`, …) always names ALab's copy, and `aterm pkg which
<tool>` says which one runs. Settings ▸ Packages ▸ Install ALab Toolset (or
`aterm pkg install --default-set`) fetches the whole set at once, and the
windowed app keeps installed packages current on a six-hour loop.

Packages ride the same trust chain as the app updater (see
[Security model](#security-model)): every download is verified before it is
parsed, the tree hash of the extracted files must match the signed manifest, and
installs swap in atomically with rollback. `aterm help` lists the tools.

## Use aterm in your own project

### 1. Control a running terminal

Use the authenticated local protocol when your program should share a real PTY
session with a person: every result in the [CLI tour](#cli-tour) has a
protocol-level equivalent, and `aterm ctl screen` returns full per-cell JSON
(glyph, colours, attributes) when text is not enough. It is the right boundary
for an agent orchestrator, test harness, development environment, or tool that
needs observation and control without owning the terminal engine. From Rust, the
`aterm-agent` crate wraps the same protocol as `CtlClient`, `RelayClient`, and
`Turn`.

### 2. Embed the engine

Use the engine surfaces when your application owns the terminal grid and
renderer. Point your workspace at the checked-out source:

```toml
[dependencies]
aterm-core = { path = "../aterm/crates/aterm-core" }
```

The Rust shape is intentionally small:

```rust
use aterm_core::terminal::TerminalBuilder;

let mut terminal = TerminalBuilder::new()
    .size(24, 80) // rows, columns
    .build();

terminal.process(b"\x1b[32mhello from aterm\x1b[0m");
let visible = terminal.visible_content(); // the plain-text projection
assert!(visible.contains("hello from aterm"));
```

Beyond Rust: `aterm-ffi` builds a static or dynamic library with a small
panic-safe C ABI (`crates/aterm-core/include/aterm.h`), and three WebAssembly
crates target `wasm32-unknown-unknown` — `aterm-wasm` (engine plus CPU
rasterizer into a canvas), `aterm-gpu-web` (engine plus wgpu over WebGL2), and
`aterm-effects-web` (the Matrix-rain overlay for an external terminal grid).
[Orca ALab Edition](https://github.com/alabsystems/orca-alab) is the reference
integration: it uses aterm for terminal state, parsing, search, selection,
scrollback, and the CPU/GPU WebAssembly renderers, and its
[`orca-terminal` adapter](https://github.com/alabsystems/orca-alab/tree/main/rust/crates/orca-terminal)
is a concrete example.

Embedding the engine does not include the native window, the control socket, or
the macOS sound palette. The workspace crates are public but pre-1.0; expect API
changes between releases.

## Security model

The control surface can read terminal output and inject input, so aterm treats
it as privileged:

- Every control-enabled window or headless instance mints a fresh per-launch
  capability token, and every connection must present it.
- Unix runtime directories, sockets, and tokens are user-private, and every
  connection verifies the peer's user ID. Windows uses a hardened user-private
  ACL plus the launch token; SYSTEM and platform administrators remain
  privileged.
- Scoped capabilities can grant a single kind of operation on a single session
  without handing over the instance's owner token; owner-only verbs stay
  owner-only.
- Network driving is opt-in: a TLS 1.3 relay with a pinned certificate
  fingerprint and channel-bound tokens, never the default control path.
- Capture paths are confined to server-managed runtime directories.
- Terminal-side powers other programs could abuse — window operations,
  notifications, palette reconfiguration, kitty-graphics reads of host files,
  OSC 52 clipboard reads — are opt-in and off by default (clipboard reads answer
  only under `allow_osc52_query = true`, and an authorized query is always
  answered rather than left hanging), and a multi-line paste into a shell without
  bracketed paste asks first (macOS and Windows).

**Release trust** — for the macOS app updater and the toolchain package index —
is anchored by a **paper master key** that exists on no computer. It signs only a
machine roster: one signing key per publishing machine, plus the deny-list that
withdraws one. A release must carry a master-signed roster, a manifest signed by
a rostered, unrevoked machine, a forward-only build number, a matching DMG
digest, and a Developer ID signature pinned to one Team ID plus notarization; the
64-byte `.sig` assets are those detached Ed25519 signatures
(`aterm-appcast.toml.sig` over the manifest, `aterm-machines.toml.sig` over the
roster). A stolen machine key is therefore revocable by an authority the thief
does not hold, and a revocation retracts even an already-staged build. What a
build trusts is a committed constant, never an environment variable —
`PAPER_MASTER_PUBKEYS` in `crates/aterm-update-core/src/pins.rs`, reproduced here
so it can be checked from more than one place:

```text
paper master public key (Ed25519, base64):
DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ=
```

Bootstrap trust is tiered: the installer roots trust in the Apple Developer ID
chain (Team `A66A9P66Z7`), and the installed updater additionally verifies this
Ed25519 chain, whose anchors are compiled into the binary it updates.

Containment is a launch-time choice: `--containment <mode>` with modes `master`,
`user` (default), `safety`, and `containment`. `--sandbox` is shorthand for the
last and wraps the shell in the macOS sandbox: no network, and no reads or
writes of credential stores or private data directories, failing closed if the
wrapper is missing. Linux currently enforces resource limits plus the capability
gate, and Windows the capability gate; both disclose the difference at startup
and should not be assumed to provide the same OS confinement as macOS.

Temporal recording is off by default, and pixel video starts only when requested.
Each session retains a bounded in-memory asciicast output history. Granting
terminal control grants the ability to act with the authority of programs running
in that session.

Report suspected vulnerabilities through [SECURITY.md](SECURITY.md), never a
public issue.

## Verification and performance evidence

Bounded protocols and state machines are written once as Rust-derived models —
hundreds of them — that generate both a checkable specification and an executable
interpreter. An embedded exhaustive checker verifies their invariants,
deadlock-freedom, and transitions on every `cargo test` (a handful of
function-valued models can only be checked by `ty`, and say so); where `ty` is
installed the same obligations get a second, independent check, and `ay`
re-checks the development line's hand-encoded SMT certificate bundles. A missing
tool prints a prominent "did not run" notice rather than passing silently, and a
few certificate gates fail outright instead. Conformance tests project real
subsystem state back onto the models so the specs stay tied to shipping code; a
model with no such tie only proves something about itself, and the repository
labels those.

The development line — and the Apple-silicon slice of every release — is compiled
by the Trust toolchain (`trustc`/`targo`), but in-compilation verification is not
yet enabled workspace-wide, so "compiled by a verifying compiler" is not
"verified": that campaign ratchets crate by crate. The public snapshot builds on
stock Rust and carries the embedded exhaustive checker, the conformance,
property, and fuzz tests, the CPU/GPU parity suites, and the differential oracle
against `alacritty_terminal` (the `ay` bundles stay internal). These prove or
test named, bounded contracts — not the whole emulator, renderer, or OS. Run
`cargo run -q -p xtask -- gate counts` for the live inventory; totals are
computed from source, never maintained in prose. Every gate is a local command —
a pinned pre-push hook runs the temporal-safety and lint gates, the full ladder
runs by hand and at a gated release cut, and there is no hosted CI.

aterm makes no aggregate performance claim. The reproducible cross-engine
measurements are engine-only and in-process — throughput via
`cargo bench -p aterm-bench --bench comparative` and retained heap via
`cargo test -p aterm-bench --test memory`, aterm vs `alacritty_terminal`, no
rendering or GPU. No on-glass head-to-head has yet been run on a shipped build
under a protocol fit to publish, so none is quoted. `aterm ctl metrics` exists so
you can measure render and latency on the workload that matters to you.

## Help and configuration

The executable carries its current command reference:

```sh
aterm --help                 # the daily-driver session
aterm --window --help        # window options, keys, and the main config keys by group
aterm help                   # the manual — or, inside a session, the agent brief
aterm help introspection     # the control-verb catalog, offline
aterm ctl --help             # the client: socket resolution, exit codes, payloads
aterm doctor
aterm --window --diagnose    # compiler, renderer, features, advertised capabilities
aterm --window --show-config
aterm --window --write-config
aterm --window --validate-config
aterm --window --list-keybinds
aterm list-themes
```

Configuration is loaded from `$XDG_CONFIG_HOME/aterm/aterm.toml`, falling back to
`%APPDATA%\aterm\aterm.toml` on Windows and `~/.config/aterm/aterm.toml`
elsewhere. Explicit flags override environment variables, which override the
file. The window watches the configuration and applies supported changes without
a restart, and Settings ▸ Manual opens the file in the native editor.

By default the windowed app talks to GitHub for at most two things — the app
update check (macOS builds) and the toolchain package update pass — and contains
no telemetry. Descriptive tab titles use a local summarizer unless you opt into a
remote provider. Headless and command-line use makes no automatic network calls.

## Community and license

Focused fixes and well-scoped improvements are welcome — it is a best-effort
project. See [CONTRIBUTING.md](CONTRIBUTING.md) for the build and review boundary
and [SECURITY.md](SECURITY.md) for private vulnerability reporting. Release notes
live on the [Releases](https://github.com/alabsystems/aterm/releases) page and
inside the app under Settings ▸ Software Update.

[PUBLICATION.md](PUBLICATION.md) describes the snapshot boundary between this
repository and the private development line, and [VERSIONING.md](VERSIONING.md)
the one `MAJOR.MINOR.0` version that names the app, the tag, the DMG, and the
snapshot.

Unless a file says otherwise, aterm is licensed under the
[Apache License 2.0](LICENSE). MIT-licensed project components use
[LICENSE-MIT](LICENSE-MIT), and bundled or derived third-party material retains
its own terms. See [NOTICE](NOTICE) for the distribution inventory.
