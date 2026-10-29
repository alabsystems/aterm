# aterm_shell_integration.ps1 - Shell integration for aTerm
#
# Copyright 2026 Andrew Yates
# Author: Andrew Yates
# Licensed under the Apache License, Version 2.0
#
# Dot-source this file from your PowerShell profile, or let aterm inject it:
#   pwsh -NoExit -Command ". (Join-Path $env:ATERM_SHELL_INTEGRATION_DIR 'aterm_shell_integration.ps1')"
#
# Features enabled:
# - Directory tracking (OSC 7): tab title updates, "Open Terminal Here" support
# - Command tracking (OSC 133): command history indexing, timing, notifications
# - Live managed PATH: an already-running session shell resolves claude/codex
#   to atpkg's <prefix>/agents twins as soon as atpkg lays them
# - Key bindings: readline chords (Ctrl+A/E/J/K/U/W/Y, Alt+B/F, ...) layered
#   over PSReadLine's Windows edit mode in an aterm tab; user bindings take
#   precedence, and $env:ATERM_DISABLE_READLINE_KEYS = '1' disables the layer
#
# Compatible with: Windows PowerShell 5.1 and PowerShell 7+ (pwsh) on any OS.
# ASCII only: aterm writes this file without a BOM, and Windows PowerShell 5.1
# decodes BOM-less source as ANSI.

# Remove any legacy `~/.aterm/bin` PATH entry added by an older script that set
# `$__aterm_bin` at initial load (if the directory predates this process).
if ((Test-Path -LiteralPath Variable:\__aterm_bin) -and ($__aterm_bin -is [string])) {
    $__aterm_old = $__aterm_bin
    Remove-Variable -Name __aterm_bin -ErrorAction SilentlyContinue
    Remove-Variable -Name __aterm_bin -Scope Global -ErrorAction SilentlyContinue
    $__aterm_item = Get-Item -LiteralPath $__aterm_old -Force -ErrorAction SilentlyContinue
    if ($__aterm_item) {
        $__aterm_born = $__aterm_item.CreationTimeUtc
        if ($__aterm_born -le [DateTime]::new(1970, 1, 1, 0, 0, 0, [DateTimeKind]::Utc)) {
            $__aterm_born = $__aterm_item.LastWriteTimeUtc
        }
        $__aterm_start = [System.Diagnostics.Process]::GetCurrentProcess().StartTime
        if ($__aterm_born -lt $__aterm_start.ToUniversalTime()) {
            $__aterm_sep = [string][System.IO.Path]::PathSeparator
            $__aterm_parts = [System.Collections.Generic.List[string]]::new(
                [string[]]($env:PATH -split [regex]::Escape($__aterm_sep)))
            $__aterm_at = $__aterm_parts.IndexOf($__aterm_old)
            if ($__aterm_at -ge 0) {
                $__aterm_parts.RemoveAt($__aterm_at)
                $env:PATH = $__aterm_parts -join $__aterm_sep
            }
        }
    }
}

# Enable agy's fast prompt editor by default so U+202F narrow no-break spaces
# in macOS screenshot filenames do not desynchronize Bubble Tea's cursor.
if (-not (Test-Path Env:AGY_CLI_FAST_PROMPT_EDITOR)) {
    $env:AGY_CLI_FAST_PROMPT_EDITOR = 'true'
}

# Skip if already loaded
if ($env:ATERM_SHELL_INTEGRATION_INSTALLED) { return }
$env:ATERM_SHELL_INTEGRATION_INSTALLED = '1'

# Use UTF-8 for native program pipes and console output in an aterm tab on
# Windows (default OEM code page 437 corrupts UTF-8 CLI/git output in pipelines).
if (($env:ATERM_CHILD -or $env:ATERM_SESSION_ID) -and
    ([System.Environment]::OSVersion.Platform -eq 'Win32NT')) {
    try {
        $__aterm_utf8 = New-Object System.Text.UTF8Encoding $false
        $Global:OutputEncoding = $__aterm_utf8
        [Console]::OutputEncoding = $__aterm_utf8
    }
    catch { }
}

# Source package shell hooks (`~/.aterm/shell.d/*.ps1`).
if ($HOME) {
    $__aterm_hooks = Join-Path $HOME '.aterm/shell.d'
    if (Test-Path -LiteralPath $__aterm_hooks) {
        foreach ($__aterm_hook in (Get-ChildItem -LiteralPath $__aterm_hooks -Filter '*.ps1' -ErrorAction SilentlyContinue)) {
            . $__aterm_hook.FullName
        }
    }
}

# Set `$env:EDITOR` to `$env:ATERM_DEFAULT_EDITOR` (aterm's own editor, amacs)
# only when neither EDITOR nor VISUAL is set once the profiles have run (aterm
# dot-sources this file after them) and the word names a program on PATH; drop
# the hint either way, so no child inherits it.
$__aterm_word = $env:ATERM_DEFAULT_EDITOR
Remove-Item -LiteralPath Env:ATERM_DEFAULT_EDITOR -ErrorAction SilentlyContinue
if ($__aterm_word -and -not $env:EDITOR -and -not $env:VISUAL -and
    (Get-Command -Name $__aterm_word -CommandType Application -ErrorAction SilentlyContinue)) {
    $env:EDITOR = $__aterm_word
}
Remove-Variable -Name __aterm_word -ErrorAction SilentlyContinue

# Move `$env:ATPKG_AGENTS` and `$env:ATERM_REROUTE_DIR` to the front of PATH
# (reroute first, then agents) so user profile additions cannot shadow them, and
# record the expected prefix in `$Global:__aterm_managed_want`.
$Global:__aterm_managed_want = @()
$Global:__aterm_managed_bin_on = $false
$Global:__aterm_managed_agents_on = $false
$Global:__aterm_managed_reroute_on = $false
function Global:__aterm_reroute_path_front {
    $Global:__aterm_managed_want = @()
    $Global:__aterm_managed_bin_on = $false
    $Global:__aterm_managed_agents_on = $false
    $Global:__aterm_managed_reroute_on = $false
    if ($env:ATPKG_AGENTS -and $env:ATPKG_BIN -and ([System.IO.Path]::GetDirectoryName($env:ATPKG_BIN.TrimEnd('/\')) -eq [System.IO.Path]::GetDirectoryName($env:ATPKG_AGENTS.TrimEnd('/\'))) -and (Test-Path -LiteralPath $env:ATPKG_BIN -PathType Container)) {
        $__aterm_sep = [string][System.IO.Path]::PathSeparator
        $__aterm_rest = @(($env:PATH -split [regex]::Escape($__aterm_sep)) | Where-Object { $_ -ne $env:ATPKG_BIN })
        $env:PATH = (@($env:ATPKG_BIN) + $__aterm_rest) -join $__aterm_sep
        $Global:__aterm_managed_want = @($env:ATPKG_BIN)
        $Global:__aterm_managed_bin_on = $true
    }
    if ($env:ATPKG_AGENTS -and (Test-Path -LiteralPath $env:ATPKG_AGENTS -PathType Container)) {
        $__aterm_sep = [string][System.IO.Path]::PathSeparator
        $__aterm_rest = @(($env:PATH -split [regex]::Escape($__aterm_sep)) | Where-Object { $_ -ne $env:ATPKG_AGENTS })
        $env:PATH = (@($env:ATPKG_AGENTS) + $__aterm_rest) -join $__aterm_sep
        $Global:__aterm_managed_want = @($env:ATPKG_AGENTS) + $Global:__aterm_managed_want
        $Global:__aterm_managed_agents_on = $true
    }
    if ($env:ATERM_REROUTE_DIR -and (Test-Path -LiteralPath $env:ATERM_REROUTE_DIR -PathType Container)) {
        $__aterm_sep = [string][System.IO.Path]::PathSeparator
        $__aterm_rest = @(($env:PATH -split [regex]::Escape($__aterm_sep)) | Where-Object { $_ -ne $env:ATERM_REROUTE_DIR })
        $env:PATH = (@($env:ATERM_REROUTE_DIR) + $__aterm_rest) -join $__aterm_sep
        $Global:__aterm_managed_want = @($env:ATERM_REROUTE_DIR) + $Global:__aterm_managed_want
        $Global:__aterm_managed_reroute_on = $true
    }
}
__aterm_reroute_path_front

# --- Live managed PATH updates ---
#
# Re-assert managed PATH directories on every prompt and PSReadLine submit:
# dot-source `~/.aterm/shell.d/00-atpkg.ps1` when its LastWriteTimeUtc changes,
# derive `$env:ATERM_REROUTE_DIR` from `<dir of ATPKG_AGENTS>/reroute` when
# passthrough is not engaged, re-probe previously absent dirs, and restore the
# PATH prefix on mismatch.
$Global:__aterm_atpkg_hook = $null
if ($HOME) { $Global:__aterm_atpkg_hook = Join-Path $HOME '.aterm/shell.d/00-atpkg.ps1' }
$Global:__aterm_atpkg_hook_seen = $null
$Global:__aterm_managed_live = [bool]($env:ATERM_CHILD -or $env:ATERM_SESSION_ID)
if ($Global:__aterm_managed_live -and $env:ATPKG_AGENTS -and $Global:__aterm_atpkg_hook -and [System.IO.File]::Exists($Global:__aterm_atpkg_hook)) {
    $Global:__aterm_atpkg_hook_seen = [System.IO.File]::GetLastWriteTimeUtc($Global:__aterm_atpkg_hook).Ticks
}

function Global:__aterm_managed_derive_reroute {
    if ($env:ATERM_REROUTE_DIR -or -not $env:ATPKG_AGENTS) { return }
    if ($env:__ATERM_REROUTE_PASSTHROUGH -and ($env:__ATERM_REROUTE_PASSTHROUGH -ne '0')) { return }
    $__aterm_parent = Split-Path -Parent $env:ATPKG_AGENTS
    if (-not $__aterm_parent) { return }
    $__aterm_dir = Join-Path $__aterm_parent 'reroute'
    if (Test-Path -LiteralPath $__aterm_dir -PathType Container) { $env:ATERM_REROUTE_DIR = $__aterm_dir }
}

function Global:__aterm_managed_path_live {
    if (-not $Global:__aterm_managed_live -or -not $Global:__aterm_atpkg_hook) { return }
    # 1. Re-source hook if modified.
    if ([System.IO.File]::Exists($Global:__aterm_atpkg_hook)) {
        $__aterm_stamp = [System.IO.File]::GetLastWriteTimeUtc($Global:__aterm_atpkg_hook).Ticks
        if ((-not $env:ATPKG_AGENTS) -or ($__aterm_stamp -ne $Global:__aterm_atpkg_hook_seen)) {
            $Global:__aterm_atpkg_hook_seen = $__aterm_stamp
            . $Global:__aterm_atpkg_hook
            __aterm_managed_derive_reroute
            __aterm_reroute_path_front
            return
        }
    }
    # 2. Re-probe any managed dir that was previously absent.
    $__aterm_refront = $false
    if ((-not $Global:__aterm_managed_bin_on) -and $env:ATPKG_AGENTS -and $env:ATPKG_BIN -and ([System.IO.Path]::GetDirectoryName($env:ATPKG_BIN.TrimEnd('/\')) -eq [System.IO.Path]::GetDirectoryName($env:ATPKG_AGENTS.TrimEnd('/\'))) -and (Test-Path -LiteralPath $env:ATPKG_BIN -PathType Container)) { $__aterm_refront = $true }
    if ((-not $Global:__aterm_managed_agents_on) -and $env:ATPKG_AGENTS -and (Test-Path -LiteralPath $env:ATPKG_AGENTS -PathType Container)) { $__aterm_refront = $true }
    if (-not $Global:__aterm_managed_reroute_on) {
        if ($env:ATERM_REROUTE_DIR) {
            if (Test-Path -LiteralPath $env:ATERM_REROUTE_DIR -PathType Container) { $__aterm_refront = $true }
        } else {
            __aterm_managed_derive_reroute
            if ($env:ATERM_REROUTE_DIR) { $__aterm_refront = $true }
        }
    }
    if ($__aterm_refront) { __aterm_reroute_path_front; return }
    # 3. Restore PATH order on mismatch.
    $__aterm_n = $Global:__aterm_managed_want.Count
    if ($__aterm_n -eq 0) { return }
    $__aterm_sep = [string][System.IO.Path]::PathSeparator
    $__aterm_head = @(($env:PATH -split [regex]::Escape($__aterm_sep)) | Select-Object -First $__aterm_n)
    if ($__aterm_head.Count -eq $__aterm_n) {
        $__aterm_same = $true
        for ($__aterm_i = 0; $__aterm_i -lt $__aterm_n; $__aterm_i++) {
            if ($__aterm_head[$__aterm_i] -cne $Global:__aterm_managed_want[$__aterm_i]) { $__aterm_same = $false; break }
        }
        if ($__aterm_same) { return }
    }
    __aterm_reroute_path_front
}

# Capture the capability nonce into a PowerShell variable and remove
# `Env:ATERM_SHELL_NONCE` immediately so child processes cannot inherit it.
$Global:__aterm_shell_nonce = if ($env:ATERM_SHELL_NONCE) { $env:ATERM_SHELL_NONCE } else { '' }
if (Test-Path Env:ATERM_SHELL_NONCE) { Remove-Item Env:ATERM_SHELL_NONCE }

# Capability-nonce suffix (`;id=<64-hex>` or empty when unnonced).
function Global:__aterm_id_suffix {
    if ($Global:__aterm_shell_nonce) { ";id=$($Global:__aterm_shell_nonce)" } else { '' }
}

# Percent-encode a filesystem path for a file:// URI (RFC 3986 / RFC 8089
# `/C:/...` drive-letter convention).
function Global:__aterm_osc7_path([string]$path) {
    $p = $path -replace '\\', '/'
    if (-not $p.StartsWith('/')) { $p = '/' + $p }
    $sb = New-Object System.Text.StringBuilder
    foreach ($b in [System.Text.Encoding]::UTF8.GetBytes($p)) {
        if (($b -ge 0x41 -and $b -le 0x5A) -or ($b -ge 0x61 -and $b -le 0x7A) -or
            ($b -ge 0x30 -and $b -le 0x39) -or
            $b -eq 0x2D -or $b -eq 0x2E -or $b -eq 0x2F -or $b -eq 0x3A -or
            $b -eq 0x5F -or $b -eq 0x7E) {
            [void]$sb.Append([char]$b)
        }
        else {
            [void]$sb.Append('%')
            [void]$sb.Append($b.ToString('X2'))
        }
    }
    $sb.ToString()
}

# Backslash-hex encode semicolons, backslashes, and control/space bytes for OSC 633;E.
function Global:__aterm_encode_cmd([string]$cmd) {
    $sb = New-Object System.Text.StringBuilder
    foreach ($ch in $cmd.ToCharArray()) {
        $code = [int]$ch
        if ($ch -eq '\') { [void]$sb.Append('\\') }
        elseif ($ch -eq ';') { [void]$sb.Append('\x3b') }
        elseif ($code -le 0x20 -or $code -eq 0x7F) { [void]$sb.Append('\x' + $code.ToString('x2')) }
        else { [void]$sb.Append($ch) }
    }
    $sb.ToString()
}

$Global:__aterm_original_prompt = $function:Prompt
# $null = no prompt observed yet; 0 = prompt seen, history empty.
$Global:__aterm_last_history_id = $null

# --- Exit code resolution for failed lines ---
#
# Native programs set `$LASTEXITCODE`, whereas cmdlet errors and `throw` leave it
# unchanged. Snapshot `$LASTEXITCODE` and `$Error[0]` before each line runs and
# resolve a failed line's status in order:
#  1. Non-zero `$LASTEXITCODE` that changed from the pre-line snapshot.
#  2. `STATUS_CONTROL_C_EXIT` (0xC000013A / -1073741510) if `ExecutionStatus` is `Stopped`.
#  3. 0 if the history entry is not `Failed` and parses to zero statements (comment-only).
#  4. Unchanged non-zero `$LASTEXITCODE` if no new `$Error` record was added or the
#     newest record is a native-command error (`cmd /c exit 7` run twice in a row).
#  5. 1 for a cmdlet failure or parse error (`ExecutionStatus -eq 'Failed'`).
$Global:__aterm_lec_before = $null
$Global:__aterm_err_before = $null
function Global:__aterm_mark_line_start {
    $Global:__aterm_lec_before = $global:LASTEXITCODE
    $Global:__aterm_err_before = $null
    if ($global:Error.Count -gt 0) { $Global:__aterm_err_before = $global:Error[0] }
}
function Global:__aterm_history_failed($history) {
    ($null -ne $history) -and ([string]$history.ExecutionStatus -eq 'Failed')
}
function Global:__aterm_failed_code($exit, $history) {
    $moved = ($null -ne $exit) -and ($exit -ne 0) -and ($exit -ne $Global:__aterm_lec_before)
    if ($moved) { return $exit }
    if (($null -ne $history) -and ([string]$history.ExecutionStatus -eq 'Stopped')) { return -1073741510 }
    if (($null -ne $history) -and -not (__aterm_history_failed $history)) {
        $ast = [System.Management.Automation.Language.Parser]::ParseInput([string]$history.CommandLine, [ref]$null, [ref]$null)
        if (($null -eq $ast.EndBlock) -or ($ast.EndBlock.Statements.Count -eq 0)) { return 0 }
    }
    $new_error = ($global:Error.Count -gt 0) -and -not ($global:Error[0] -eq $Global:__aterm_err_before)
    if (($null -ne $exit) -and ($exit -ne 0)) {
        if (-not $new_error) { return $exit }
        $newest = $global:Error[0]
        if (($newest -is [System.Management.Automation.ErrorRecord]) -and
            ([string]$newest.FullyQualifiedErrorId -match '^(NativeCommandError|NativeCommandErrorMessage|ProgramExitedWithNonZeroCode)(,|$)')) { return $exit }
    }
    1
}

function Global:Prompt {
    # Capture $? / $LASTEXITCODE first: any statement below clobbers them.
    $__aterm_ok = $global:?
    $__aterm_exit = $global:LASTEXITCODE
    Set-StrictMode -Off
    __aterm_managed_path_live
    $__aterm_esc = [char]27
    $__aterm_bel = [char]7
    $__aterm_suffix = __aterm_id_suffix
    $__aterm_out = ''

    # Emit OSC 133;D;exitcode only when a new history entry appeared since the last prompt,
    # written at once: the user's prompt below may run a program (starship, oh-my-posh), and
    # a program heard after D is never the command's.
    $__aterm_history = Get-History -Count 1
    $__aterm_hid = 0
    if ($null -ne $__aterm_history) { $__aterm_hid = $__aterm_history.Id }
    if (($null -ne $Global:__aterm_last_history_id) -and ($__aterm_hid -ne $Global:__aterm_last_history_id)) {
        $__aterm_code = 0
        if ((-not $__aterm_ok) -or (__aterm_history_failed $__aterm_history)) { $__aterm_code = __aterm_failed_code $__aterm_exit $__aterm_history }
        [Console]::Write("$__aterm_esc]133;D;$__aterm_code$__aterm_suffix$__aterm_bel")
    }
    $Global:__aterm_last_history_id = $__aterm_hid

    # Report filesystem CWD (signed 633;P;AtermCwd + plain OSC 7) and abbreviated tab title (OSC 0).
    $__aterm_loc = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($__aterm_loc.Provider.Name -eq 'FileSystem') {
        $__aterm_uri = "file://$([System.Environment]::MachineName)$(__aterm_osc7_path $__aterm_loc.ProviderPath)"
        if ($__aterm_suffix) {
            $__aterm_out += "$__aterm_esc]633;P;AtermCwd=$__aterm_uri$__aterm_suffix$__aterm_bel"
        }
        $__aterm_out += "$__aterm_esc]7;$__aterm_uri$__aterm_bel"

        if (-not $env:ATERM_DISABLE_PROMPT_TITLES) {
            $__aterm_title = $__aterm_loc.ProviderPath
            if ($HOME -and ($__aterm_title -eq $HOME)) {
                $__aterm_title = '~'
            }
            elseif ($HOME -and $__aterm_title.StartsWith([string]$HOME + [System.IO.Path]::DirectorySeparatorChar)) {
                $__aterm_title = '~' + $__aterm_title.Substring(([string]$HOME).Length)
            }
            $__aterm_title = $__aterm_title -replace '[\x00-\x1f\x7f]', ''
            $__aterm_out += "$__aterm_esc]0;$__aterm_title$__aterm_bel"
        }
    }

    # Reset bracketed paste (2004) and in-band resize reports (2048) in case a
    # killed child process left them enabled.
    $__aterm_out += $__aterm_esc + '[?2004l' + $__aterm_esc + '[?2048l'

    # Mark prompt start (OSC 133;A)
    $__aterm_out += "$__aterm_esc]133;A$__aterm_suffix$__aterm_bel"

    # Run the user's original prompt with $LASTEXITCODE / $? restored.
    $global:LASTEXITCODE = $__aterm_exit
    if ($null -ne $Global:__aterm_original_prompt) {
        if (-not $__aterm_ok) { Write-Error 'aterm: restore $? for user prompt' -ErrorAction Ignore }
        $__aterm_out += [string]($Global:__aterm_original_prompt.Invoke())
    }
    else {
        $__aterm_out += "PS $__aterm_loc> "
    }

    # Mark command line start (OSC 133;B)
    $__aterm_out += "$__aterm_esc]133;B$__aterm_suffix$__aterm_bel"
    # Re-wrap PSConsoleHostReadLine if `Import-Module PSReadLine -Force` replaced our shim.
    $__aterm_readline = $function:PSConsoleHostReadLine
    if (($null -ne $__aterm_readline) -and ($null -ne $__aterm_readline.Module) -and ($__aterm_readline.Module.Name -eq 'PSReadLine')) {
        __aterm_install_readline_shim
    }
    __aterm_mark_line_start
    $__aterm_out
}

# Wrap `PSConsoleHostReadLine` to emit OSC 633;E, running-command OSC 0 title,
# and OSC 133;C on command submission.
function Global:__aterm_install_readline_shim {
    $Global:__aterm_original_readline = $function:PSConsoleHostReadLine
    function Global:PSConsoleHostReadLine {
        $__aterm_line = [string]($Global:__aterm_original_readline.Invoke())
        __aterm_managed_path_live
        __aterm_mark_line_start
        if ($__aterm_line -and $__aterm_line.Trim()) {
            $__aterm_esc = [char]27
            $__aterm_bel = [char]7
            $__aterm_suffix = __aterm_id_suffix
            $__aterm_out = "$__aterm_esc]633;E;$(__aterm_encode_cmd $__aterm_line)$__aterm_suffix$__aterm_bel"
            # Truncate running-command title to 256 UTF-16 code units (avoiding
            # splitting surrogate pairs) with a trailing ellipsis.
            if (-not $env:ATERM_DISABLE_PROMPT_TITLES) {
                $__aterm_title = $__aterm_line
                if ($__aterm_title.Length -gt 256) {
                    $__aterm_cut = 255
                    if ([char]::IsHighSurrogate($__aterm_title[$__aterm_cut - 1])) { $__aterm_cut-- }
                    $__aterm_title = $__aterm_title.Substring(0, $__aterm_cut) + [char]0x2026
                }
                $__aterm_title = $__aterm_title -replace '[\x00-\x1f\x7f]', ''
                $__aterm_out += "$__aterm_esc]0;$__aterm_title$__aterm_bel"
            }
            $__aterm_out += "$__aterm_esc]133;C$__aterm_suffix$__aterm_bel"
            [Console]::Write($__aterm_out)
        }
        $__aterm_line
    }
}
if (Get-Module -Name PSReadLine) { __aterm_install_readline_shim }

# --- Readline key bindings over PSReadLine Windows mode ---
#
# Layer readline chords over PSReadLine's Windows edit mode while keeping
# Windows bindings for Tab, Ctrl+Z/V/C/X, Ctrl+Left/Right, Ctrl+Backspace/Delete,
# Escape, and Home/End. Each chord is bound only when currently unbound or still
# bound to its stock Windows-mode function (and not a prefix of a two-key chord).
#
# If EditMode is Emacs solely because an older aterm setup wrote
# `$__aterm_legacy_emacs_block` into `$PROFILE` (and no profile mentions
# `EditMode`, `KeyHandler`, or `Chord` elsewhere or binds custom script blocks),
# switch back to Windows mode before applying the layer.
#
# On PSReadLine 2.0.0 (Windows PowerShell 5.1), provide fallback script blocks
# for `UpcaseWord`, `DowncaseWord`, and `CapitalizeWord`, and fall back from
# `BackwardKillInput` to `BackwardKillLine`. Where `AcceptSuggestion` exists
# (pwsh 7), `Ctrl+e` accepts an inline suggestion when the cursor is already at
# the end of the line, and otherwise moves to the end of the line.
$Global:__aterm_legacy_emacs_block = @(
    '# aterm: emacs line-editing so Ctrl+A/Ctrl+E/Ctrl+K/Ctrl+U... move the cursor',
    '# (Windows PowerShell 5.1''s PSReadLine defaults to "Windows" mode, where Ctrl+A =',
    '# SelectAll and Ctrl+E is unbound). Remove this block to revert.',
    'if (Get-Module -ListAvailable PSReadLine) { Set-PSReadLineOption -EditMode Emacs }')
function Global:__aterm_legacy_emacs {
    if ($null -eq $global:PROFILE) { return $false }
    $texts = @()
    foreach ($name in 'AllUsersAllHosts', 'AllUsersCurrentHost', 'CurrentUserAllHosts', 'CurrentUserCurrentHost') {
        $path = $global:PROFILE.PSObject.Properties[$name]
        if ((-not $path) -or (-not $path.Value) -or (-not [System.IO.File]::Exists([string]$path.Value))) { continue }
        $texts += [System.IO.File]::ReadAllText([string]$path.Value)
    }
    if (-not ([string]::Join("`n", [string[]]$texts)).Contains($Global:__aterm_legacy_emacs_block[0])) { return $false }
    $block = "`n" + [string]::Join("`n", [string[]]$Global:__aterm_legacy_emacs_block) + "`n"
    $found = $false
    foreach ($text in $texts) {
        $lines = $text.Replace("`r`n", "`n").Replace("`r", "`n").Split("`n")
        for ($i = 0; $i -lt $lines.Length; $i++) { $lines[$i] = $lines[$i].TrimEnd() }
        $text = "`n" + [string]::Join("`n", $lines) + "`n"
        $rest = $text
        do { $length = $rest.Length; $rest = $rest.Replace($block, "`n") } while ($rest.Length -ne $length)
        if ($rest.Length -ne $text.Length) { $found = $true }
        if ($rest -match 'EditMode|KeyHandler|Chord') { return $false }
    }
    if (-not $found) { return $false }
    $rl = [Microsoft.PowerShell.PSConsoleReadLine]
    foreach ($handler in @(Get-PSReadLineKeyHandler -Bound)) {
        if ($rl.GetMember([string]$handler.Function).Length -eq 0) { return $false }
    }
    $true
}
function Global:__aterm_setup_keybindings {
    $ErrorActionPreference = 'Stop'
    if (-not (Get-Module -Name PSReadLine)) { return }
    $__aterm_mode = [string](Get-PSReadLineOption).EditMode
    if (($__aterm_mode -eq 'Emacs') -and (__aterm_legacy_emacs)) {
        Set-PSReadLineOption -EditMode Windows
        $__aterm_mode = [string](Get-PSReadLineOption).EditMode
    }
    if ($__aterm_mode -ne 'Windows') { return }
    $__aterm_bound = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([System.StringComparer]::Ordinal)
    foreach ($__aterm_h in @(Get-PSReadLineKeyHandler -Bound)) {
        $__aterm_k = [string]$__aterm_h.Key
        $__aterm_bound[$__aterm_k] = [string]$__aterm_h.Function
        # Mark the first key of a multi-key chord as bound (`[^+]` avoids splitting `Alt+,`).
        if ($__aterm_k -match '^(.+?[^+]),.') { $__aterm_bound[$Matches[1]] = 'ChordFirstKey' }
    }
    $__aterm_rl = [Microsoft.PowerShell.PSConsoleReadLine]
    $__aterm_kill_input = if ($__aterm_rl.GetMethod('BackwardKillInput')) { 'BackwardKillInput' } else { 'BackwardKillLine' }
    $__aterm_end_of_line = 'EndOfLine'
    if ($__aterm_rl.GetMethod('AcceptSuggestion')) {
        $__aterm_end_of_line = @{
            BriefDescription = 'AcceptSuggestionAndEndOfLine'
            Description      = 'At the end of the line, accept the inline suggestion if one shows; elsewhere, move the cursor to the end of the line (aterm readline keys)'
            ScriptBlock      = {
                param($key, $arg)
                $line = $null
                $cursor = $null
                [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
                if ($cursor -eq $line.Length) {
                    [Microsoft.PowerShell.PSConsoleReadLine]::AcceptSuggestion($key, $arg)
                }
                [Microsoft.PowerShell.PSConsoleReadLine]::EndOfLine($key, $arg)
            }
        }
    }
    function Global:__aterm_case_word([string]$how) {
        $line = $null
        $cursor = $null
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
        $delimiters = (Get-PSReadLineOption).WordDelimiters
        $end = $cursor
        while ($end -lt $line.Length -and ([char]::IsWhiteSpace($line[$end]) -or $delimiters.IndexOf($line[$end]) -ge 0)) { $end++ }
        while ($end -lt $line.Length -and -not ([char]::IsWhiteSpace($line[$end]) -or $delimiters.IndexOf($line[$end]) -ge 0)) { $end++ }
        if ($end -le $cursor) { return }
        $word = $line.Substring($cursor, $end - $cursor)
        if ($how -eq 'Upcase') { $word = $word.ToUpper() }
        elseif ($how -eq 'Downcase') { $word = $word.ToLower() }
        else {
            $first = 0
            while ($first -lt $word.Length -and -not [char]::IsLetter($word[$first])) { $first++ }
            if ($first -lt $word.Length) {
                $word = $word.Substring(0, $first) + $word.Substring($first, 1).ToUpper() + $word.Substring($first + 1).ToLower()
            }
        }
        [Microsoft.PowerShell.PSConsoleReadLine]::Replace($cursor, $end - $cursor, $word)
        [Microsoft.PowerShell.PSConsoleReadLine]::SetCursorPosition($end)
    }
    $__aterm_upcase = 'UpcaseWord'
    if (-not $__aterm_rl.GetMethod('UpcaseWord')) {
        $__aterm_upcase = @{
            BriefDescription = 'UpcaseWord'
            Description      = 'Upcase from the cursor to the end of the word (aterm readline keys)'
            ScriptBlock      = { param($key, $arg) __aterm_case_word 'Upcase' }
        }
    }
    $__aterm_downcase = 'DowncaseWord'
    if (-not $__aterm_rl.GetMethod('DowncaseWord')) {
        $__aterm_downcase = @{
            BriefDescription = 'DowncaseWord'
            Description      = 'Downcase from the cursor to the end of the word (aterm readline keys)'
            ScriptBlock      = { param($key, $arg) __aterm_case_word 'Downcase' }
        }
    }
    $__aterm_capitalize = 'CapitalizeWord'
    if (-not $__aterm_rl.GetMethod('CapitalizeWord')) {
        $__aterm_capitalize = @{
            BriefDescription = 'CapitalizeWord'
            Description      = 'Capitalize the word from the cursor and move past it (aterm readline keys)'
            ScriptBlock      = { param($key, $arg) __aterm_case_word 'Capitalize' }
        }
    }
    # Columns: chord, target handler, replaceable stock Windows-mode function ('' = unbound).
    foreach ($__aterm_row in @(
            @('Ctrl+a', 'BeginningOfLine', 'SelectAll'),
            @('Ctrl+e', $__aterm_end_of_line, ''),
            @('Ctrl+b', 'BackwardChar', ''),
            @('Ctrl+f', 'ForwardChar', ''),
            @('Ctrl+k', 'KillLine', ''),
            @('Ctrl+u', $__aterm_kill_input, ''),
            @('Ctrl+w', 'UnixWordRubout', 'BackwardKillWord'),
            @('Ctrl+p', 'PreviousHistory', ''),
            @('Ctrl+n', 'NextHistory', ''),
            @('Ctrl+d', 'DeleteCharOrExit', ''),
            @('Ctrl+t', 'SwapCharacters', ''),
            @('Ctrl+y', 'Yank', 'Redo'),
            @('Ctrl+_', 'Undo', ''),
            @('Ctrl+j', 'AcceptLine', ''),
            @('Alt+b', 'BackwardWord', ''),
            @('Alt+f', 'ForwardWord', ''),
            @('Alt+Backspace', 'BackwardKillWord', ''),
            @('Alt+LeftArrow', 'BackwardWord', ''),
            @('Alt+RightArrow', 'ForwardWord', ''),
            @('Alt+u', $__aterm_upcase, ''),
            @('Alt+l', $__aterm_downcase, ''),
            @('Alt+c', $__aterm_capitalize, ''))) {
        $__aterm_how = $__aterm_row[1]
        if ($__aterm_how -is [string]) {
            if (-not $__aterm_rl.GetMethod($__aterm_how)) { continue }
            $__aterm_how = @{ Function = $__aterm_how }
        }
        $__aterm_cur = $null
        $null = $__aterm_bound.TryGetValue($__aterm_row[0], [ref]$__aterm_cur)
        if ((-not $__aterm_cur) -or ($__aterm_cur -ceq $__aterm_row[2])) {
            Set-PSReadLineKeyHandler -Chord $__aterm_row[0] @__aterm_how
        }
    }
}
if ((Get-Module -Name PSReadLine) -and ($env:ATERM_CHILD -or $env:ATERM_SESSION_ID) -and
    (-not $env:ATERM_DISABLE_READLINE_KEYS)) {
    try { __aterm_setup_keybindings } catch { }
}
