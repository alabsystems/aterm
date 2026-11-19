// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! aterm's usage tap at launch: the block a managed agent program's twin and `bin/` shim carry,
//! so a Claude Code started inside an aterm tab carries aterm's status-line usage tap from its
//! start, with no relaunch and nothing for the person to do
//! (`docs/DESIGN-claude-accounts-v2-2026-10-15.md`). The judgment is aterm's own
//! (`aterm harness tap --launch`, which prints the `--settings` value or nothing); the block only
//! asks it and puts what it printed ahead of the launch's own arguments. The same verb first
//! settles the launch's folder trust (aterm's `harness::launch_trust`), so the block is also the
//! one step a launch may wait on: for a folder Claude Code holds no trust record for, and for
//! seconds at most.

use std::path::Path;

/// The programs whose twin carries the block ([`of`]).
const PROGRAMS: [&str; 1] = ["claude"];

/// Whether `program`'s twin carries the usage tap's block ([`sh_prelude`]).
#[must_use]
pub fn of(program: &str) -> bool {
    PROGRAMS.contains(&program)
}

/// What the block asks of the aterm it names, ahead of the launch's own arguments: aterm's
/// `harness tap --launch --`, which reads no stdin, prints the `--settings` value or nothing, and
/// exits 0. It returns at once where the folder's trust is recorded or is not aterm's to settle;
/// otherwise it waits, within a bound of its own, while Claude Code records it.
pub const VERB: [&str; 4] = ["harness", "tap", "--launch", "--"];

/// The variable the block sets for that one command: `1` where the launch's standard output is a
/// terminal, `0` where it is not (a pipe, a file, a tool's shell). aterm settles no folder trust
/// for a launch that says `0`: Claude Code prints and exits there and draws no dialog.
pub const STDOUT_TTY_ENV: &str = "ATERM_LAUNCH_STDOUT_TTY";

/// The note line ahead of the block.
const NOTE: &str = "# atpkg: aterm's usage tap — inside an aterm tab, `aterm harness tap --launch` \
     names the `--settings` value that hands this launch's usage figures to aterm's footer, or \
     nothing, and a launch it names none for runs as typed; before that, where aterm's policy \
     trusts this folder and Claude Code holds no trust record for it, it has Claude Code record \
     one, within a bound of seconds (aterm help harness).\n";

/// The aterm executable beside `atpkg` (`atpkg` and `aterm` are one program under two names, laid
/// side by side), or `None` where `atpkg` names no file (an empty path).
#[must_use]
pub fn aterm_beside(atpkg: &Path) -> Option<std::path::PathBuf> {
    atpkg
        .file_name()
        .is_some_and(|name| !name.is_empty())
        .then(|| atpkg.with_file_name(if cfg!(windows) { "aterm.exe" } else { "aterm" }))
}

/// THE BLOCK of `program`'s twin for the atpkg at `atpkg` (`None`, or a program [`of`] does not
/// name, or Windows, whose `.cmd` twin carries [`with_cmd_block`] instead: none). Inside an aterm
/// tab (`ATERM_PARENT_SESSION_ID` set) with the harness on for this launch (`ATERM_NO_HARNESS`
/// empty or unset: `claude --no-harness` sets it), it runs the aterm beside `atpkg` on the
/// launch's arguments, its stdin `/dev/null`, its stderr dropped and [`STDOUT_TTY_ENV`] saying
/// whether the launch's own standard output is a terminal; where that printed a value,
/// the launch runs with `--settings <value>` ahead of its own arguments, and where it printed
/// nothing, failed or is not there, the launch runs as typed. The launch waits for that aterm to
/// exit, which is bounded by aterm's own deadline for the folder's trust (the module's). It sits after the `--yolo` block,
/// which reads `--settings` as a posture of the person's own, and ends with the renderer block
/// ([`RENDERER_BLOCK`]).
#[must_use]
pub fn sh_prelude(program: &str, atpkg: Option<&Path>) -> String {
    sh_prelude_blocks(program, atpkg)
        .into_iter()
        .map(|(_, block)| block)
        .collect()
}

/// [`sh_prelude`]'s two blocks in the order it lays them, the usage tap's and the renderer's
/// ([`RENDERER_BLOCK`]), each tagged with the block a native restart's projection rule names
/// ([`crate::platform::TwinBlock`]); both empty where [`sh_prelude`] is.
#[must_use]
pub(crate) fn sh_prelude_blocks(
    program: &str,
    atpkg: Option<&Path>,
) -> [(crate::platform::TwinBlock, String); 2] {
    use crate::platform::TwinBlock;
    let none = || {
        [
            (TwinBlock::UsageTap, String::new()),
            (TwinBlock::Renderer, String::new()),
        ]
    };
    if cfg!(windows) || !of(program) {
        return none();
    }
    let Some(aterm) = atpkg.and_then(aterm_beside) else {
        return none();
    };
    let aterm = crate::platform::sh_quote_str(&aterm.to_string_lossy());
    let mut s = String::from(NOTE);
    s.push_str(
        "if [ -n \"${ATERM_PARENT_SESSION_ID-}\" ] && [ -z \"${ATERM_NO_HARNESS-}\" ] && [ -x ",
    );
    s.push_str(&aterm);
    s.push_str(" ]; then\n  if [ -t 1 ]; then __atpkg_tty=1; else __atpkg_tty=0; fi\n");
    s.push_str("  __atpkg_tap=$(");
    s.push_str(STDOUT_TTY_ENV);
    s.push_str("=$__atpkg_tty ");
    s.push_str(&aterm);
    for word in VERB {
        s.push(' ');
        s.push_str(word);
    }
    s.push_str(" \"$@\" </dev/null 2>/dev/null) || __atpkg_tap=\n");
    s.push_str(
        "  if [ -n \"$__atpkg_tap\" ]; then set -- --settings \"$__atpkg_tap\" \"$@\"; fi\n",
    );
    s.push_str("fi\n");
    [
        (TwinBlock::UsageTap, s),
        (TwinBlock::Renderer, RENDERER_BLOCK.to_string()),
    ]
}

/// THE RENDERER BLOCK, the last part of [`sh_prelude`]: inside an aterm tab with the harness on
/// for this launch, a Claude Code launch that names no renderer runs fullscreen
/// (`CLAUDE_CODE_NO_FLICKER=1`, which starts it ahead of every settings file). Claude Code
/// otherwise picks its renderer from a server flag it caches only after a start, so the first launch on a new Claude home drew its classic renderer
/// inline under the last command's output (no clear, no blank top row) and every later one
/// drew fullscreen (measured on 2.1.321, 2026-11-09). A launch names a renderer, and the block
/// leaves it alone, by setting `CLAUDE_CODE_NO_FLICKER` or
/// `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN` in its environment (to any value, empty included) or by a `"tui":` key in the person's own
/// `settings.json` under `$CLAUDE_CONFIG_DIR`, else `$HOME/.claude` (where `/tui` saves the
/// choice, and its relaunch drops the variable); a `tui` only a project's or a managed settings
/// file holds is overridden in a tab.
const RENDERER_BLOCK: &str = "# atpkg: Claude Code's renderer — inside an aterm tab, a launch \
     that names none (CLAUDE_CODE_NO_FLICKER and CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN unset, no \
     \"tui\" in your settings.json) runs fullscreen from its first start (aterm help pkg).\n\
     if [ -n \"${ATERM_PARENT_SESSION_ID-}\" ] && [ -z \"${ATERM_NO_HARNESS-}\" ] \
     && [ -z \"${CLAUDE_CODE_NO_FLICKER+x}\" ] && [ -z \"${CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN+x}\" ] \
     && ! grep -qs '\"tui\"[[:space:]]*:' \"${CLAUDE_CONFIG_DIR:-${HOME-}/.claude}/settings.json\"; then\n  \
     CLAUDE_CODE_NO_FLICKER=1; export CLAUDE_CODE_NO_FLICKER\n\
     fi\n";

/// What the token block asks of the aterm it names, between it and the build: aterm's token
/// wrapper for a launch a person typed (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.4).
pub const TOKEN_VERB: [&str; 5] = ["harness", "accounts", "token-run", "--auto", "--"];

/// The note line ahead of the token block.
const TOKEN_NOTE: &str = "# atpkg: aterm's token route — inside an aterm tab, `aterm harness accounts token-run \
     --auto` starts this launch on its account's vault token where that token is in use on this \
     Mac, and runs it exactly as typed anywhere else (aterm help harness).\n";

/// `body`, `program`'s `sh` twin for the atpkg at `atpkg`, with THE TOKEN BLOCK right before its
/// last line, the plain `exec '<build>' "$@"`: inside an aterm tab (`ATERM_PARENT_SESSION_ID` set), with
/// the harness on for this launch (`ATERM_NO_HARNESS` empty or unset) and the aterm beside
/// `atpkg` there, the launch runs as `<aterm> harness accounts token-run --auto -- <build>
/// <args…>` ([`TOKEN_VERB`]), which serves the account's token or runs the build as typed; with
/// that aterm missing, the last line runs as typed. The block sits after the tap's (in the
/// prelude) and after the twin's exports, so the build the wrapper execs holds them, and its
/// last line stays the plain exec, which `twin_binary` reads the build from. `body` comes back
/// as it is for a program [`of`] does not name, on Windows (whose `.cmd` twin carries no token
/// block: ATERM-KI-1117-31417), where `atpkg` names no file, or where its last line is not a
/// plain exec.
#[must_use]
pub fn with_token_block(program: &str, body: String, atpkg: Option<&Path>) -> String {
    if cfg!(windows) || !of(program) {
        return body;
    }
    let Some(aterm) = atpkg.and_then(aterm_beside) else {
        return body;
    };
    let trimmed = body.trim_end_matches('\n');
    let (head, last) = match trimmed.rsplit_once('\n') {
        Some((head, last)) => (head, last),
        None => return body,
    };
    let Some(build) = crate::platform::sh_exec_target(last.trim()) else {
        return body;
    };
    let mut s = String::with_capacity(body.len() + 512);
    s.push_str(head);
    s.push('\n');
    s.push_str(&token_block(&aterm, &build));
    s.push_str(last);
    s.push('\n');
    s
}

/// The token block alone ([`with_token_block`]) that `program`'s twin for the atpkg at `atpkg`
/// carries ahead of its plain `exec '<build>' "$@"`: empty where [`with_token_block`] lays none. A
/// native restart's projection reads it as one block ([`crate::platform::TwinBlock::TokenRoute`]),
/// which only a Unix shim carries, so it exists only where that projection does.
#[cfg(unix)]
#[must_use]
pub(crate) fn sh_token_block(program: &str, build: &Path, atpkg: Option<&Path>) -> String {
    if !of(program) {
        return String::new();
    }
    atpkg
        .and_then(aterm_beside)
        .map_or_else(String::new, |aterm| token_block(&aterm, build))
}

fn token_block(aterm: &Path, build: &Path) -> String {
    let aterm = crate::platform::sh_quote_str(&aterm.to_string_lossy());
    let build = crate::platform::sh_quote_str(&build.to_string_lossy());
    let mut s = String::from(TOKEN_NOTE);
    s.push_str(
        "if [ -n \"${ATERM_PARENT_SESSION_ID-}\" ] && [ -z \"${ATERM_NO_HARNESS-}\" ] && [ -x ",
    );
    s.push_str(&aterm);
    s.push_str(" ]; then\n  exec ");
    s.push_str(&aterm);
    for word in TOKEN_VERB {
        s.push(' ');
        s.push_str(word);
    }
    s.push(' ');
    s.push_str(&build);
    s.push_str(" \"$@\"\nfi\n");
    s
}

/// The flag of the `.cmd` twin's ask ([`with_cmd_block`]), between [`VERB`]'s `--launch` and its
/// `--`: `aterm harness tap --launch --into <folder> -- <args…>`. `<folder>` is one the twin made
/// for this one launch, empty where it could make none. The verb prints nothing: it writes the
/// twin's words for the value ([`cmd_words`]) to `<folder>\`[`INTO_VALUE`], or nothing, and,
/// its standard output being the launch's own, judges by it whether that is a terminal.
pub const INTO_FLAG: &str = "--into";

/// The file of the `--into` folder the verb writes the twin's words to ([`INTO_FLAG`]).
pub const INTO_VALUE: &str = "value";

/// The longest words [`cmd_words`] gives: well inside the 8191 characters cmd.exe takes for the
/// forward line they are expanded into, with the launch's own arguments beside them.
pub const CMD_WORDS_MAX: usize = 2048;

/// The characters cmd.exe's parse of a line reads as its own outside a quote: each of them a
/// value carries is put behind a caret in [`cmd_words`].
const CMD_SPECIAL: &str = "^&|<>()\"%!";

/// THE TWIN'S WORDS FOR A `--settings` VALUE: `--settings <value>` as the `.cmd` twin's forward
/// line expands them ahead of the launch's own arguments ([`with_cmd_block`]). The value is one
/// argument as a Windows program splits its command line: in quotes, a quote inside it as `\"`
/// with the backslashes before it doubled, and the backslashes ending it doubled. Then each
/// character cmd.exe's parse reads as its own (`^ & | < > ( ) " % !`) is put behind a caret,
/// which that parse takes off, so no character of the value opens a quote, a command, a
/// redirection or a variable there. `None` where the value holds a character outside printable
/// ASCII (the twin reads the file in the console's code page), or where the words would run
/// past [`CMD_WORDS_MAX`]: that launch runs as typed.
#[must_use]
pub fn cmd_words(value: &str) -> Option<String> {
    if !value.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return None;
    }
    let mut quoted = String::with_capacity(value.len() + 8);
    quoted.push('"');
    let mut backslashes = 0usize;
    for c in value.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', 2 * backslashes + 1));
                quoted.push('"');
                backslashes = 0;
            }
            c => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', 2 * backslashes));
    quoted.push('"');
    let mut words = String::from("--settings ");
    for c in quoted.chars() {
        if CMD_SPECIAL.contains(c) {
            words.push('^');
        }
        words.push(c);
    }
    (words.len() <= CMD_WORDS_MAX).then_some(words)
}

/// The note line ahead of the `.cmd` block (ASCII: cmd.exe reads a batch file in the console's
/// code page).
#[cfg(any(windows, test))]
const CMD_NOTE: &str = "@rem atpkg: aterm's usage tap - inside an aterm tab, `aterm harness tap \
     --launch` names the --settings value that hands this launch's usage figures to aterm's \
     footer, or nothing, and a launch it names none for runs as typed; before that, where \
     aterm's policy trusts this folder and Claude Code holds no trust record for it, it has \
     Claude Code record one, within a bound of seconds (aterm help harness).\r\n";

/// The `.cmd` block's labels: past the ask, at the read of its answer, past that read (an ask
/// that failed), at the plain forward, at each try of the folder's name, and past the folder's
/// make.
const CMD_LABEL_END: &str = "atpkg_tap_end";
#[cfg(any(windows, test))]
const CMD_LABEL_NAME: &str = "atpkg_tap_name";
#[cfg(any(windows, test))]
const CMD_LABEL_MADE: &str = "atpkg_tap_made";
#[cfg(any(windows, test))]
const CMD_LABEL_READ: &str = "atpkg_tap_read";
#[cfg(any(windows, test))]
const CMD_LABEL_DROP: &str = "atpkg_tap_drop";
#[cfg(any(windows, test))]
const CMD_LABEL_PLAIN: &str = "atpkg_tap_plain";

/// THE `.cmd` BLOCK (Windows), the twin's and `bin\` shim's counterpart of [`sh_prelude`]'s usage
/// tap: `body`, `program`'s framed `.cmd` twin (`crate::platform::cmd_shim_content_twin`), with
/// the ask after the frame and ahead of its `@set` lines, and the tapped forward right before its
/// last line, the plain forward, which still names the build. `body` comes back as it is off
/// Windows, for a program [`of`] does not name, where `atpkg` names no file or the aterm beside
/// it is no path a batch line can quote (`crate::platform::cmd_target_is_injection_safe`), and
/// for a body that is not a framed shim ending in its forward line.
///
/// Inside an aterm tab (`ATERM_PARENT_SESSION_ID` defined: cmd.exe reads an empty variable as
/// none) with the harness on for this launch (`ATERM_NO_HARNESS` undefined or empty) and the
/// aterm there, it makes a folder of its own under `%TEMP%` for this launch and runs that aterm
/// as `<aterm> harness tap --launch --into <folder> -- <args…>` ([`INTO_FLAG`]), its stdin `nul`
/// and its stderr dropped, its standard output the launch's. Where that exited 0 having written
/// the twin's words ([`cmd_words`]), the launch runs as `<build> <words> <args…>` (the read asks
/// that exit itself, so it holds where an open quote in the arguments took the ask's line's
/// `&&` and `||` into aterm's arguments); where it wrote none, failed or is not there, or no
/// folder could be made (`TEMP` undefined or unwritable, sixteen names taken), the launch runs
/// as typed. The folder's name is two `%RANDOM%` values and a count of the tries, and a name
/// another launch made is passed over for the next: cmd.exe seeds `%RANDOM%` from the clock's
/// second, so launches in one second pick the same values. Either way it runs on the forward's own line, which ends the batch
/// with the build's exit code; a line whose arguments leave a quote open runs on into the next
/// line, which ends the batch the same way, so no launch starts the build twice.
///
/// Every line that runs a program is one cmd.exe parses once, as the plain forward's: the
/// launch's arguments reach aterm and the build exactly as they reach the plain forward's build,
/// expanded once (`%*`), with delayed expansion off for the whole batch (`setlocal`), and never
/// through a `for /f` command or a `call`, which would expand them again; the words come from a
/// variable whose expansion is never parsed again for a variable. So what cmd.exe reads as its
/// own in a raw command line (an `&` outside a quote that no caller escaped) it reads on the
/// ask's line as on the forward's. The block starts no program but aterm and the build, each
/// sharing the console cmd.exe has, so no window opens. The ask's line, the one the batch waits
/// on before the build, goes on by its label (`goto`), never by its offset, so a twin re-laid
/// during that wait goes on at the same step of the twin laid then (a twin re-laid without the
/// block ends that launch with cmd.exe's missing-label error). Its variables and folder are gone
/// before the build starts: nothing of the block reaches the build's environment, and the
/// batch's `setlocal` keeps the twin's `@set` lines out of the shell that ran it. None of its
/// lines is an `@set "…"` line, which atpkg reads as the twin's environment
/// (`crate::platform::parse_cmd_shim_env`), or a forward line, the last line alone.
#[must_use]
pub fn with_cmd_block(program: &str, body: String, atpkg: Option<&Path>) -> String {
    #[cfg(windows)]
    if let Some(aterm) = atpkg.and_then(aterm_beside) {
        return cmd_block_for(program, body, &aterm);
    }
    #[cfg(not(windows))]
    let _ = (program, atpkg);
    body
}

/// [`with_cmd_block`] naming `aterm`, on every host a test runs on: the text the Windows twin
/// carries.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn cmd_block_for(program: &str, body: String, aterm: &Path) -> String {
    let aterm = aterm.to_string_lossy();
    if !of(program) || !crate::platform::cmd_target_is_injection_safe(Path::new(&*aterm)) {
        return body;
    }
    let frame = crate::platform::cmd_framed("");
    let Some(rest) = body.strip_prefix(frame.as_str()) else {
        return body;
    };
    let lines = rest.strip_suffix("\r\n").unwrap_or(rest);
    let (sets, last) = match lines.rsplit_once("\r\n") {
        Some((sets, last)) => (Some(sets), last),
        None => (None, lines),
    };
    let Some(target) = crate::platform::cmd_forward_target(last)
        .filter(|_| last.ends_with(crate::platform::CMD_FORWARD_TAIL))
    else {
        return body;
    };
    if sets.is_some_and(|sets| !sets.split("\r\n").all(|line| line.starts_with("@set \""))) {
        // A body carrying more than its `@set` lines ahead of the forward (a block already) is
        // not the plain twin the block wraps.
        return body;
    }
    let target = target.to_string_lossy();
    let mut s = String::with_capacity(body.len() + 2048);
    s.push_str(CMD_NOTE);
    s.push_str(
        "@setlocal DisableDelayedExpansion\r\n@set __atpkg_tap=\r\n@set __atpkg_tap_d=\r\n@set \
         __atpkg_tap_n=\r\n",
    );
    for skip in [
        "@if not defined ATERM_PARENT_SESSION_ID goto ".to_owned(),
        "@if defined ATERM_NO_HARNESS goto ".to_owned(),
        format!("@if not exist \"{aterm}\" goto "),
    ] {
        s.push_str(&skip);
        s.push_str(CMD_LABEL_END);
        s.push_str("\r\n");
    }
    // The launch's folder: a name cmd.exe's `%RANDOM%` picks and a count of the tries, made by
    // this launch alone. cmd.exe seeds `%RANDOM%` from the clock's second, so two launches in
    // one second pick the same two values; a name another launch made is passed over for the
    // next try's (the count tells it apart even where `RANDOM` is a variable the caller set),
    // and a make that fails for any other reason, or a sixteenth taken name, leaves no folder.
    s.push_str(&format!(
        "@set __atpkg_tap_n=0\r\n:{CMD_LABEL_NAME}\r\n\
         @if defined TEMP set /a __atpkg_tap_n+=1 >nul\r\n\
         @if defined TEMP set \"__atpkg_tap_d=%TEMP%\\atpkg-tap-%RANDOM%-%RANDOM%-%__atpkg_tap_n%\"\r\n\
         @if defined __atpkg_tap_d mkdir \"%__atpkg_tap_d%\" 2>nul && goto {CMD_LABEL_MADE}\r\n\
         @if defined __atpkg_tap_d if exist \"%__atpkg_tap_d%\" if %__atpkg_tap_n% lss 16 goto \
         {CMD_LABEL_NAME}\r\n\
         @set __atpkg_tap_d=\r\n:{CMD_LABEL_MADE}\r\n"
    ));
    s.push_str(&format!("@\"{aterm}\" <nul 2>nul"));
    for word in &VERB[..3] {
        s.push(' ');
        s.push_str(word);
    }
    s.push_str(&format!(
        " {INTO_FLAG} \"%__atpkg_tap_d%\" -- %* && goto {CMD_LABEL_READ} || goto \
         {CMD_LABEL_DROP}\r\n:{CMD_LABEL_READ}\r\n"
    ));
    let file = format!("\"%__atpkg_tap_d%\\{INTO_VALUE}\"");
    // The read asks aterm's exit itself: where the launch's arguments leave a quote open, the
    // ask's `&& goto … || goto …` is inside that quote, one more of aterm's arguments, and the
    // ask's line goes on to this one whatever aterm answered.
    s.push_str(&format!(
        "@if errorlevel 1 goto {CMD_LABEL_DROP}\r\n\
         @if not errorlevel 0 goto {CMD_LABEL_DROP}\r\n\
         @if defined __atpkg_tap_d if exist {file} for /f \"usebackq delims=\" %%L in ({file}) \
         do @set \"__atpkg_tap=%%L\"\r\n\
         :{CMD_LABEL_DROP}\r\n\
         @if defined __atpkg_tap_d del /q \"%__atpkg_tap_d%\\*\" 2>nul\r\n\
         @if defined __atpkg_tap_d rd \"%__atpkg_tap_d%\" 2>nul\r\n\
         @set __atpkg_tap_d=\r\n@set __atpkg_tap_n=\r\n:{CMD_LABEL_END}\r\n"
    ));
    if let Some(sets) = sets {
        s.push_str(sets);
        s.push_str("\r\n");
    }
    let tail = crate::platform::CMD_FORWARD_TAIL;
    let exit = &tail[" %* & ".len()..];
    s.push_str(&format!(
        "@if not defined __atpkg_tap goto {CMD_LABEL_PLAIN}\r\n\
         @set \"__atpkg_tap=\" & \"{target}\" %__atpkg_tap%{tail}\r\n\
         {exit}\r\n:{CMD_LABEL_PLAIN}\r\n"
    ));
    s.push_str(last);
    s.push_str("\r\n");
    crate::platform::cmd_framed(&s)
}

/// The atpkg a `.cmd` body carrying [`with_cmd_block`] names: the one beside the aterm its ask
/// runs (`atpkg.exe`, as [`aterm_beside`] lays the two), or `None` for a body that carries no
/// ask.
#[must_use]
pub fn cmd_named_atpkg(body: &str) -> Option<std::path::PathBuf> {
    let suffix = format!("\\aterm.exe\" goto {CMD_LABEL_END}");
    body.lines().find_map(|line| {
        let dir = line
            .strip_prefix("@if not exist \"")?
            .strip_suffix(&suffix)?;
        (!dir.is_empty()).then(|| std::path::PathBuf::from(format!("{dir}\\atpkg.exe")))
    })
}

#[cfg(test)]
#[path = "launch_tap_tests.rs"]
mod tests;
