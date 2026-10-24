<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

<h1 align="center">aterm</h1>

<p align="center">
  <strong>The batteries-included terminal for AI.</strong><br>
  A GPU terminal for people, an authenticated control surface for agents, and
  the <a href="https://alab.systems">ALab</a> verification toolchain one
  command away.
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

```sh
curl -fsSL https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh | bash
```

On macOS the script downloads the newest app release from github.com, checks
its SHA-256 against the release manifest, verifies the Developer ID signature
and notarization, puts `aterm.app` in `/Applications` (or `~/Applications`), and
links the `aterm` command and its man pages under `~/.local`. The ALab
toolchain is not in the download: it installs itself on first launch, with its
progress shown in the app.

Flags go after `bash -s --`; `--help` lists them (`--dry-run`, `--uninstall`,
`--no-toolchain`, …). `--no-toolchain` also writes `[packages] auto_install =
false` into `aterm.toml`, so the app does not install the toolchain either. The
toolchain adds a marked block to an existing `~/.zshrc`, `~/.bashrc`,
`~/.bash_profile` or fish config; a deleted block stays deleted until `aterm pkg
repair`.

The app is signed, notarized and universal (Apple silicon and Intel), for macOS
11+. The Trust compiler, its verifier backends and `clean` are published for
Apple silicon only, so an Intel Mac's toolchain installs without them.

Each app release on
[github.com/alabsystems/aterm/releases](https://github.com/alabsystems/aterm/releases)
carries the signed `aterm-appcast.toml` manifest and the same app in two
containers, each with a `.sha256` sidecar whose digest is also in the manifest:

- **`aterm-X.Y.0.dmg`** — a drag-install image. It also installs an air-gapped
  Mac; the toolchain always comes from the network.
- **`aterm-X.Y.0-mac.zip`** — what the in-app updater, the Homebrew cask and
  `install.sh` consume.

`aterm.dmg` and `aterm-mac.zip` carry the same bytes under permanent names, so
`releases/latest/download/aterm.dmg` always fetches the newest app. A release
with no app on it is a source-only cut. To check a download and the installed
app:

```sh
shasum -a 256 -c aterm-<version>.dmg.sha256
codesign --verify --strict --verbose=2 /Applications/aterm.app
spctl -a -t exec -vv /Applications/aterm.app   # must report source=Notarized Developer ID
```

### Linux

```sh
curl -fsSL https://alab.systems/install.sh | bash
```

This is the same installer, served by alab.systems as of the latest release;
[alab.systems/aterm](https://alab.systems/aterm) shows it to a Linux visitor
whose architecture the release serves. It supports `aarch64-unknown-linux-gnu`
and `x86_64-unknown-linux-gnu` when the release carries their signed
executables (`aterm-<version>-linux-aarch64`, `aterm-<version>-linux-x86_64`),
and says so when there is none for your architecture. It needs curl, and
`sha256sum` or `shasum`. It verifies the signature chain and the executable
before running anything, then installs the binary under `~/.local` and enrolls
it for signed updates. The check runs in a small static verifier that each
release publishes (`aterm-bootstrap-verify-<version>-linux-<arch>`);
install.sh pins it by SHA-256 and refuses one that differs. Until a release
publishes it, install.sh uses its embedded verifier, which needs OpenSSL with
Ed25519 support and Python 3.11+ (with `hashlib.file_digest` and
`os.memfd_create`) instead of the SHA-256 tool.
Once installed:

```sh
aterm --window
aterm update status
```

A raw executable you download yourself does not update itself.

### Homebrew

```sh
brew install --cask alabsystems/tap/aterm
```

The cask installs the same signed app from the release's `mac.zip` and declares
`auto_updates`, so the app's own updater keeps it current;
`brew upgrade --cask --greedy aterm` makes Homebrew do it instead. The `alab`
cask installs the same app under the toolchain's name and conflicts with
`aterm`.

### Staying current

Installed copies update themselves: the app checks the channel in the
background, verifies the new build and installs it; on macOS every window, tab
and live shell survives the handoff. Settings ▸ Software Update and
`aterm update status` show where it stands, and `aterm help update` covers the
rest (Linux adds `enable`, `apply` and `rollback`). `[update] enabled = false`
stops background checks; `[update] auto_apply = false` downloads updates but
leaves installing them to you. Windows has no published build and no updater.

### Build from source

This repository is a snapshot of the development line and pins the Trust
toolchain (`rust-toolchain.toml`); with aterm installed, `aterm pkg install
trust` provides it. atpkg ships Trust for Apple-silicon macOS only, so that is
where the snapshot builds. With the Xcode Command Line Tools:

```sh
git clone https://github.com/alabsystems/aterm.git && cd aterm
targo --unverified build --locked -p aterm
./target/debug/aterm --window
```

Running the tests also needs `targo --unverified verify provision` (or `aterm
init --develop`).

A source build is not the signed release and is not enrolled for updates. The
crates are not on crates.io. The crates.io `aterm`, the X11 `aterm` in MacPorts
and the aterm.ai/aterm.io domains are unrelated projects.

## Why aterm?

aterm keeps the familiar PTY and adds a second, authenticated surface that
software can read and drive. A person types normally while an agent observes
structured state, drives a turn, waits for a real transition or captures the
rendered result. Shells, TUIs, editors and agent CLIs still see an ordinary
terminal.

| Battery | What aterm adds | Boundary |
| --- | --- | --- |
| **Observe** | Plain text, lossless styled cells, cursor, modes, command blocks, session status, a lifecycle timeline, full-history search | Window and headless modes only |
| **Drive** | Text, keys, paste, mouse, focus, resize, selection, clipboard, tabs, menu actions, Settings | Signals are a separate operation, never faked keystrokes |
| **Coordinate** | Event-driven `await`, `ready`, `wait`, whole-turn settlement, drive leases | `turn` waits for the screen to settle; `wait` needs OSC 133 marks |
| **Stream** | `screen`, `cursor`, `cells`, `bytes`, `events`, `mail`, `sessions` subscriptions, several sessions on one stream | Gaps are marked, never hidden |
| **Capture** | PNG frames, full-window shots with native chrome (macOS), asciicast history, opt-in temporal replay, GPU video | What aterm rendered, not what the display showed |
| **Measure** | Render, frame, input and present counters with percentiles and startup phases | Software-side counters |
| **Extend** | Themes, typography, wallpaper, Toy Packs, keybindings, the engine source | Path dependencies only; no API stability yet |
| **Delight** | The rainbow kitty pet, per-program cats, cursor trails, Sparkle Words, Matrix rain, a typing-sound synth | Synth audio is macOS-only |

### One binary

| Invocation | Result | Control socket |
| --- | --- | --- |
| `aterm` in a TTY | Transparent PTY session | No |
| `aterm` with no TTY (a Finder launch), or `--window` | Terminal window | Yes |
| `aterm --headless` | The engine without a window | Yes |
| `aterm --session` | Force the PTY session in a pipe or CI | No |

The same binary answers the platform verbs (`ctl`, `pkg`, `fleet`, `drive`,
`update`, `agents`, …), `help`, the diagnostic subcommands (`doctor`,
`show-config`, …) and managed-tool dispatch; `aterm --help` lists them.
`aterm-ctl`, `atpkg`, `abrew`, `agh`, `aterm-fleet`, `aterm-drive` and `amacs`
(aterm's editor, `aterm edit`) are symlinks onto it. <!-- From aterm_types::argv0::ALIASES. -->
Window and headless modes serve the control socket by default
(`--no-control-sock` turns it off) and fail closed when a secure socket cannot
be created.

## Made for humans and agents

Agents speak a local control protocol outside the PTY byte stream. What they
send enters through the same input path as keystrokes, and the keyboard never
routes through the agent, so a person can type at any time, even mid-turn.

When a window opens a session, aterm keeps a short primer current in the global
context file of each coding agent it finds — Claude Code, Codex CLI, Gemini CLI
and OpenCode (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, …). Claude Code also
gets five skills (`drive-aterm`, `supervise-agent`, `rust-in-aterm`,
`aterm-themes`, `aterm-fabric`); the others get `aterm-fabric` as a command. The primer applies
only inside aterm (`TERM_PROGRAM=aterm`, `ATERM_CHILD=1`), where `aterm help`
prints the agent brief with the caller's session ID filled in. Keep your own
rules outside its markers. `aterm agents` shows status, `aterm agents remove`
uninstalls, and `agents_auto_prime = false` stops the window reinstalling it.
Each window start also sets six Claude Code defaults in `~/.claude/settings.json`,
each only where its key is unset: the top-effort mode, `xhigh` effort, the
session-quality survey and every rating survey off, the drafted-feedback card
quiet, and autocompaction at 400k tokens. A value you set is kept, and
`agents_auto_prime = false` turns these off too.

- **Env hygiene.** Every spawned shell has `CLAUDE*`, `ANTHROPIC_*`,
  `COPILOT_*`, `CODEX_*`, `ANTIGRAVITY_*`, `GEMINI_*`, `CURSOR_*`, `GROK_*`,
  `XAI_*`, `KIMI_*`, `MOONSHOT_*`, `AI_*` and `_DEVTOOL_*` stripped, and a window's shell gets a fresh session identity.
- **No MCP server.** The integration surfaces are the CLI (`aterm ctl`, `conn`,
  `drive`) and the Rust library API; `aterm fleet` is a machine's fleet membership,
  which `aterm ctl survey --fleet` reads through.
- **A session fabric**: stable session IDs, `@sid` routing across every
  same-user instance, `@self`, per-session `status` and `timeline`, settable
  `meta` (`role`, `attention`), turn leases, multi-session subscriptions,
  `aterm ctl survey` (what every session is doing, in one read), and `aterm
  conn` to wire one session to watch and type into another.

### CLI tour

```sh
aterm --window -e htop          # a window running one command (-d <dir>, --hold, --shell)
aterm ctl text                  # plain visible rows
aterm ctl screen                # lossless styled-grid JSON
aterm ctl status                # phase, outcome, and confidence for this session
aterm ctl image frame.png       # the rendered frame as PNG; the reply names where it was saved
aterm ctl window shot.png       # the whole window with its native chrome (macOS)
aterm ctl turn 'git status'     # type, submit, settle, and return the screen
aterm ctl wait [<ms>]           # block until the running command completes (OSC 133)
aterm ctl metrics               # live render and latency counters
aterm ctl help text             # one verb's entry; bare `help` is the catalog

aterm ctl ls                                   # every session of every live instance
sid=$(aterm ctl spawn window=1 | cut -d' ' -f2)
aterm ctl "@$sid" turn 'make test'
aterm ctl "@$sid" await match 'result:'        # block until a regex appears (one token)
aterm ctl "@$sid" await agent idle,prompt,question,wall   # a coding-agent peer's turn is over, a box waits, or a wall stops it
aterm ctl subscribe "@$sid" events,cursor      # add screen,cells,bytes as needed
aterm ctl "@$sid" close
```

`await` takes one predicate (`idle <ms>`, `seq [<n>]`, `match <re>`,
`gone <re>`, `block`, `agent <word>`, `inbox since=<id>`, …) and an optional
`timeout=<ms>`; a timeout exits 124. Without `--sock`/`--pid` a call drives the
instance hosting your terminal, else the newest instance; a private instance
binds its own path with `--control-sock`.

## Fun is a feature

The default cursor is the **rainbow kitty pet**: a full-body cat that walks,
runs and pounces along your line on a rainbow ribbon, with a music-box typing
sound on macOS. It cheers a green build, sulks at a failed one, chases your
mouse and purrs when stroked. Type `sit`, `kitty jump` or `good kitty` and a
space, and the word flashes rainbow while the cat does it (`aterm help kitty`).
Each launch generates a new kitty, each program that holds the foreground gets
its own cat, and View ▸ Favourite This Kitty pins one.

<p align="center">
  <img src="assets/aterm-rainbow-kitty.png" width="880" alt="Close-up from a real aterm frame: the rainbow kitty and its ribbon under a typed command">
</p>

- **Trails**: `rainbow kitty pet` (also `rainbow kitty`, `kitty`),
  `rainbow kitty underline`, `rainbow dog pet`, `rainbow kitty flying`,
  `rainbow kitty flat`, `phaser`, `fire`, `water`, `laser` (shown as
  Lightning), `comet`, `sparkle`, `lumen`, `beam`, `classic` or `off`.
  Settings ▸ Cursor & Motion picks the typing sound.
- **Sparkle Words** give typed words animated ink; TOML Toy Packs add your own.
  They are purely visual: copied text and logs read the exact bytes.
- **Matrix rain** in empty cells (View ▸ Matrix Rain, `aterm ctl rain toggle`).

`motion = reduced` (or the OS Reduce Motion setting) stops governed animation;
`serious_mode` mutes every effect sound and hides every decorative effect. On
Windows decorative effects are opt-in.

## A real terminal underneath

Unicode grapheme clusters, wide and combining characters, bidi, the kitty
keyboard protocol, OSC 8 hyperlinks, OSC 133 shell marks (injected for zsh,
bash, fish and PowerShell), styled underlines, 24-bit colour, synchronized
output, bracketed paste, OSC 52, and sixel, iTerm2 and kitty graphics.
Scrollback is tiered (RAM, then LZ4; 100,000 lines by default); predictive
local echo paints ahead of a slow shell; box drawing and Powerline glyphs are
generated from cell geometry; an idle event loop parks.

Rendering is aterm's own Metal backend on macOS and wgpu elsewhere, with the CPU
rasterizer as fallback or `--cpu`, held to the same frames by a parity suite.
Tabs, splits and windows share one process and are restored across relaunch;
Settings (Cmd-,), Markdown files and an editor open as tabs; Cmd-F searches
scrollback by regex; Shift-Cmd-P is the command palette; chords are rebindable
under `[keybindings]`. Themes built in: Default and Default Light, aterm's
signature pairs Aterm Night/Aterm Day and Hearth/Linen, and ports such as
Dracula, Nord, Solarized and Catppuccin (`aterm theme list`). Settings ▸ Themes
shows each as a live sample to try, tune and save. `theme = "dark:…,light:…"`
follows the OS appearance (macOS's, whatever `window_theme` forces on the
window chrome; the XDG desktop portal or GNOME's `color-scheme` on Linux; the
system setting on Windows), and `*.conf` files in a `themes` folder beside
`aterm.toml` (`~/.config/aterm/themes` on macOS and Linux) add your own:
`aterm help themes` has the format, `aterm theme import` converts another
terminal's, and [`contrib/themes/GUIDE.md`](contrib/themes/GUIDE.md) is the guide to
writing and sharing one ([`contrib/themes/`](contrib/themes/README.md)). Programs that ask whether the terminal is light or dark (DSR 996,
mode 2031) are told what the palette on screen is, not what the OS is set to.

## The ALab toolchain

aterm is the front door to ALab's verification toolchain: the `trust` compiler
(a Rust compiler that verifies what it compiles), the `ay` solver, the `ty`
specification checker, the `clean` theorem prover, and the `ny`/`nn`
neural-network tools.

```sh
aterm pkg install trust     # trustc, targo, tippy, …
aterm pkg install ay
aterm ay --help
aterm pkg list
```

`aterm <tool>` resolves against the managed store, never `$PATH`. On `$PATH` the
managed tools come last, `alab-<tool>` always names ALab's copy, and `aterm pkg
which <tool>` says which one runs. Inside an aterm tab `cargo`, `rustc` and
friends are rerouted to their Trust counterparts (`aterm help reroute`;
`--no-reroute` restores them). Packages ride the updater's trust chain: verified
before parsing, tree-hashed against the signed manifest, swapped in atomically
with rollback.

## Use aterm in your own project

**Control a running terminal.** Every result in the [CLI tour](#cli-tour) has a
protocol equivalent; from Rust, `aterm-agent` wraps it as `CtlClient` and
`RelayClient`.

**Embed the engine** when your application owns the grid and renderer:

```toml
[dependencies]
aterm-core = { path = "../aterm/crates/aterm-core" }
```

```rust
use aterm_core::terminal::TerminalBuilder;

let mut terminal = TerminalBuilder::new().size(24, 80).build();
terminal.process(b"\x1b[32mhello from aterm\x1b[0m");
assert!(terminal.visible_content().contains("hello from aterm"));
```

Two WebAssembly crates target `wasm32-unknown-unknown`: `aterm-wasm` (engine
plus CPU rasterizer into a canvas) and `aterm-gpu-web` (engine plus
`aterm-gpu`'s own GL renderer over WebGL2). Both export through
`aterm-wasm-abi`. Embedding excludes the native window, the control socket and
the sound palette. The crates are pre-1.0.

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
- Nothing on the network drives aterm unless its person names who may. The
  one network door is the opt-in fleet listener (`[fleet] listen`): it accepts
  loopback, private and link-local addresses only, admits a peer that proves
  the shared fleet secret with an HMAC over the TLS 1.3 exporter, and answers
  the read verbs marked `fleet` (`survey`, `version`), at `facts` or `content`
  scope (`[fleet] allow`), never as the owner. At `allow = "drive"` it also
  serves the worker's verbs to the machines `aterm fleet allow drive --from`
  named, each proved by the node key it held when it was named (the fleet
  secret alone drives nothing), and the person at the window wins: Take Over
  refuses every write from another machine. On the fleet's issuer (the machine
  that ran `aterm fleet init`) the same port also records a `join` from a peer
  that proves the join key, which is derived from the same secret: that record
  goes into this machine's book of peers and never reaches the control server.
  Its embedded certificate and key are public and identify nobody; the secret
  to guard is `<state>/fleet.key`.
- Capture paths are confined to server-managed runtime directories.
- Terminal-side powers other programs could abuse — window operations,
  notifications, palette reconfiguration, kitty-graphics reads of host files,
  OSC 52 clipboard reads (`allow_osc52_query`) — are off by default, and a
  multi-line paste into a shell without bracketed paste asks first.

**Release trust** — for the macOS and Linux app updaters and the toolchain
package index — is anchored by a **paper master key** that exists on no
computer. It signs only a machine roster: one signing key per publishing
machine, plus the deny-list that withdraws one. A release must carry a
master-signed roster, a manifest signed by a rostered, unrevoked machine, a
forward-only build number and a matching artifact digest. macOS additionally
verifies a Developer ID signature pinned to one Team ID, plus notarization;
native Linux verifies the exact ELF target, size and compiled identity before
replacement. The 64-byte `.sig` assets are those detached Ed25519 signatures
(`aterm-appcast.toml.sig` over the manifest, `aterm-machines.toml.sig` over the
roster). A stolen machine key is therefore revocable by an authority the thief
does not hold, and a revocation retracts even an already-staged build. What a
build trusts is a committed constant, never an environment variable —
`PAPER_MASTER_PUBKEYS` in `crates/aterm-update-core/src/pins.rs`, reproduced
here so it can be checked from more than one place:

```text
paper master public key (Ed25519, base64):
DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ=
```

The macOS installer roots trust in the Apple Developer ID chain (Team
`A66A9P66Z7`); the installed updater also verifies the Ed25519 chain. The Linux
installer requires the master-to-roster-to-manifest Ed25519 chain itself.

**Containment** is chosen at launch: `--containment <mode>` takes `user` (the
default), `safety` or `containment`, and
`--sandbox` is shorthand for the last. `containment` wraps the shell in the
macOS sandbox and fails closed if the wrapper is missing: no network; writes
only under the temp directories, `/dev` and the shell's history file; no access
to credential stores or private data directories; no debugger attach; and no
mach service lookup but user and group queries, so `open`, `osascript`,
`pbcopy` and the keychain do nothing from inside. A contained window tab gets
no brokered powers (clipboard, Kitty file reads, notifications), whatever the
`allow_*` keys say. Linux and Windows have no OS sandbox, so there `--sandbox`
refuses to start and names the gap. `safety` sets resource limits with no OS
sandbox, on every platform; they are soft on macOS and Linux (the shell can
raise them), and `--sandbox` sets the same limits as hard ones.
`aterm help containment` has the details.

Temporal recording is off by default, and pixel video starts only when
requested. Each session keeps a bounded in-memory asciicast output history.
Granting terminal control grants the ability to act with the authority of the
programs running in that session.

Report suspected vulnerabilities through [SECURITY.md](SECURITY.md), never a
public issue.

## Verification and performance evidence

Bounded protocols and state machines are written once as Rust-derived models
that generate both a checkable specification and an executable interpreter. An
embedded exhaustive checker verifies their invariants, deadlock-freedom and
transitions on every test run; the few function-valued models only `ty` can
check end `NotRun` there, never a pass. The release checks (`verify spec`,
`verify checkers`) give the same obligations a second, independent check under
`ty`, and `ay` re-checks the development line's SMT certificate bundles; both
fail closed without their tool, and a test whose tool is missing ends COULD
NOT RUN naming what provides it, never a pass.
Conformance tests project real subsystem state onto the models so the specs
stay tied to shipping code; a model with no such tie is labelled as one.

The development line, and the Apple-silicon slice of every release, is compiled
by the Trust toolchain (`trustc`/`targo`), but in-compilation verification
ratchets crate by crate rather than covering the workspace, so "compiled by a
verifying compiler" does not mean "verified". The public snapshot pins the same
toolchain and carries the embedded checker, the conformance, property and fuzz
tests and the CPU/GPU parity suites (the `ay` bundles stay internal). These
prove or test named, bounded contracts, not the whole emulator, renderer or OS.
Windows-only runtime tests (`windows_smoke`, the Windows half of the PowerShell
key tests) run in no check: `verify cells` type-checks them for both Windows
targets and runs none of them. `targo --unverified lab windows exec` runs them
by hand in a Windows 11 arm64 VM on an Apple-silicon Mac, x86_64 under Prism.
There is no hosted CI and no push gate: every release runs the release checks
(`targo --unverified verify release`) before it claims a build number.
[CONTRIBUTING.md](CONTRIBUTING.md) says what you can run here.

aterm makes no aggregate performance claim; `aterm ctl metrics` measures render
and input latency on your own workload.

## Help and configuration

```sh
aterm help                   # the manual (in an aterm window's tab: the agent brief)
aterm --window --help        # window options, keys, config keys
aterm help introspection     # the control-verb catalog, offline
aterm doctor
aterm --window --validate-config
```

Configuration is read from `$XDG_CONFIG_HOME/aterm/aterm.toml`, else
`%APPDATA%\aterm\aterm.toml` on Windows and `~/.config/aterm/aterm.toml`
elsewhere. Launch flags override it; no environment variable sets a key. The
window applies supported changes without a restart, and Settings ▸ Manual opens
the file in aterm's editor.

aterm contains no telemetry. By default it goes online for two things: the app
update check (eligible macOS installs and enrolled Linux installs) and the
toolchain package update pass (GitHub, plus each agent CLI's own vendor:
downloads.claude.ai for Claude Code, releases.openai.com for Codex CLI,
antigravity-cli-auto-updater-974169037036.us-central1.run.app and
storage.googleapis.com/antigravity-public for Antigravity CLI, and api.meta.ai
and lookaside.facebook.com for Muse Code, which aterm installs on macOS only).
Grok Build (registry.npmjs.org and
storage.googleapis.com/grok-build-public-artifacts, macOS and Linux) and Kimi
Code (cdn.kimi.com and github.com/MoonshotAI/kimi-code) are fetched only once
you install them by name.
aterm never mirrors or bundles an agent CLI. Only the window and
interactive sessions run them; explicit network commands always do their work.
When the pass lands a new Claude Code or Codex CLI you have used before, aterm
runs it once in the background so it refreshes your account's model list.
`[packages] enabled = false` stops the pass.

On macOS aterm turns Universal Control off for this Mac by default;
`[machine] universal_control = "leave"` in `aterm.toml` (Settings ▸ Security)
stops that, and `aterm pkg machine` prints the line that turns it back on. On
Apple silicon the same daily apply installs Rosetta 2 when it is missing, as you
and with no password (`softwareupdate --install-rosetta`); `[machine] rosetta =
false` stops that. Once a day that same pass also removes the Rust build-cache
folders under `~/Library/Caches/aterm/cargo-build.noindex` that nothing builds
into any more.

On macOS, builds and tests can stall while macOS checks each new program aterm
starts before its first run. `aterm doctor`'s `devtools:` row says whether that
happens: adding aterm under System Settings ▸ Privacy & Security ▸ Developer
Tools ends those checks. That choice is yours, since it also skips macOS's
first-run malware scan for everything run in aterm. Bare `aterm doctor` brings
you to that page when aterm is not on the list, and moves stale copies of aterm
to the Trash; `aterm doctor --read-only` changes nothing.

## Community and license

Focused fixes are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).
[PUBLICATION.md](PUBLICATION.md) describes this snapshot's boundary and
[VERSIONING.md](VERSIONING.md) the one `MAJOR.MINOR.0` version.

Unless a file says otherwise, aterm is licensed under the
[Apache License 2.0](LICENSE). MIT-licensed components use
[LICENSE-MIT](LICENSE-MIT); third-party material keeps its own terms
([NOTICE](NOTICE)).
