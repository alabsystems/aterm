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
#
# Compatible with: Windows PowerShell 5.1 and PowerShell 7+ (pwsh) on any OS.
# ASCII only: aterm writes this file without a BOM, and Windows PowerShell 5.1
# decodes BOM-less source as ANSI, so non-ASCII bytes would be misread.

# Skip if already loaded
if ($env:ATERM_SHELL_INTEGRATION_INSTALLED) { return }
$env:ATERM_SHELL_INTEGRATION_INSTALLED = '1'

# Package bin directory
if ($HOME) {
    $__aterm_bin = Join-Path $HOME '.aterm/bin'
    if (Test-Path -LiteralPath $__aterm_bin) {
        $env:PATH = $__aterm_bin + [System.IO.Path]::PathSeparator + $env:PATH
    }

    # Source package shell hooks
    $__aterm_hooks = Join-Path $HOME '.aterm/shell.d'
    if (Test-Path -LiteralPath $__aterm_hooks) {
        foreach ($__aterm_hook in (Get-ChildItem -LiteralPath $__aterm_hooks -Filter '*.ps1' -ErrorAction SilentlyContinue)) {
            . $__aterm_hook.FullName
        }
    }
}

# Capture the capability nonce into a PowerShell variable so we can
# immediately drop it from the environment (#8015). Leaving
# ATERM_SHELL_NONCE in the exported env lets every child process (env,
# ssh SendEnv, docker, python subprocess, ...) read the 64-hex secret that
# would be used to bypass the #7960 nonce-enforcement defense. PowerShell
# (non-env) variables are not inherited by child processes, so the
# captured copy stays in this session.
#
# If the env var is missing or empty at source-time, the captured nonce
# stays empty and __aterm_id_suffix falls through to the unnonced form
# (pre-nonce compatibility for hosts that have not yet authorized a
# nonce), exactly like the bash/zsh/fish scripts.
$Global:__aterm_shell_nonce = if ($env:ATERM_SHELL_NONCE) { $env:ATERM_SHELL_NONCE } else { '' }
if (Test-Path Env:ATERM_SHELL_NONCE) { Remove-Item Env:ATERM_SHELL_NONCE }

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured variable - never
# from the environment - so the nonce is not inherited by subprocesses.
function Global:__aterm_id_suffix {
    if ($Global:__aterm_shell_nonce) { ";id=$($Global:__aterm_shell_nonce)" } else { '' }
}

# Percent-encode a filesystem path for a file:// URI (RFC 3986).
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; everything else
# is encoded byte-by-byte over UTF-8 as %XX, matching the bash/zsh
# encoders. ':' also passes through (valid pchar; keeps the RFC 8089
# drive-letter convention file://host/C:/...). Backslashes normalize to
# '/' and drive-letter paths gain a leading '/' (C:\Users\x -> /C:/Users//x)
# so the URI path is absolute.
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

# Encode a command line for OSC 633;E (VS Code convention): backslash-hex
# encode semicolons, backslashes, and control/space bytes, matching the
# bash/zsh/fish encoders so a raw ESC/BEL in the command line cannot
# break out of the OSC string.
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

# Preserve the user's prompt so ours wraps it instead of replacing it.
$Global:__aterm_original_prompt = $function:Prompt
# $null sentinel = "no prompt observed yet"; 0 = "prompt seen, history empty".
$Global:__aterm_last_history_id = $null

function Global:Prompt {
    # Capture $? / $LASTEXITCODE first: any statement below would clobber them.
    $__aterm_ok = $global:?
    $__aterm_exit = $global:LASTEXITCODE
    Set-StrictMode -Off
    $__aterm_esc = [char]27
    $__aterm_bel = [char]7
    $__aterm_suffix = __aterm_id_suffix
    $__aterm_out = ''

    # Mark command completion (OSC 133;D;exitcode) - only when a command
    # actually ran (a new history entry appeared since the last prompt),
    # so Ctrl+C / Enter-on-empty do not emit a stray D.
    $__aterm_history = Get-History -Count 1
    $__aterm_hid = 0
    if ($null -ne $__aterm_history) { $__aterm_hid = $__aterm_history.Id }
    if (($null -ne $Global:__aterm_last_history_id) -and ($__aterm_hid -ne $Global:__aterm_last_history_id)) {
        $__aterm_code = 0
        if (-not $__aterm_ok) {
            # A native command's failure lands in $LASTEXITCODE; a cmdlet
            # error leaves it untouched (possibly 0), so fall back to 1.
            if (($null -ne $__aterm_exit) -and ($__aterm_exit -ne 0)) { $__aterm_code = $__aterm_exit } else { $__aterm_code = 1 }
        }
        $__aterm_out += "$__aterm_esc]133;D;$__aterm_code$__aterm_suffix$__aterm_bel"
    }
    $Global:__aterm_last_history_id = $__aterm_hid

    # Report current working directory (OSC 7) - filesystem paths only
    # (skip registry/cert/etc. provider locations).
    $__aterm_loc = $ExecutionContext.SessionState.Path.CurrentLocation
    if ($__aterm_loc.Provider.Name -eq 'FileSystem') {
        $__aterm_uri = __aterm_osc7_path $__aterm_loc.ProviderPath
        $__aterm_out += "$__aterm_esc]7;file://$([System.Environment]::MachineName)$__aterm_uri$__aterm_bel"

        # Set tab title to abbreviated CWD (OSC 0). Strip control chars so a
        # crafted directory name cannot smuggle a nested escape sequence.
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

    # Mark prompt start (OSC 133;A)
    $__aterm_out += "$__aterm_esc]133;A$__aterm_suffix$__aterm_bel"

    # Run the user's original prompt with $LASTEXITCODE / $? restored so
    # error-aware prompts (starship, posh-git, oh-my-posh) render correctly.
    $global:LASTEXITCODE = $__aterm_exit
    if ($null -ne $Global:__aterm_original_prompt) {
        if (-not $__aterm_ok) { Write-Error 'aterm: restore $? for user prompt' -ErrorAction Ignore }
        $__aterm_out += [string]($Global:__aterm_original_prompt.Invoke())
    }
    else {
        $__aterm_out += "PS $__aterm_loc> "
    }

    # Mark command line start (OSC 133;B) - after prompt, before user input
    $__aterm_out += "$__aterm_esc]133;B$__aterm_suffix$__aterm_bel"
    $__aterm_out
}

# Mark command execution start (OSC 133;C) and report the command text
# (OSC 633;E). PSReadLine's readline shim is the only portable
# submit-time hook; without PSReadLine these two marks are skipped
# (prompt marks and cwd tracking above still work).
if (Get-Module -Name PSReadLine) {
    $Global:__aterm_original_readline = $function:PSConsoleHostReadLine
    function Global:PSConsoleHostReadLine {
        $__aterm_line = [string]($Global:__aterm_original_readline.Invoke())
        if ($__aterm_line -and $__aterm_line.Trim()) {
            $__aterm_esc = [char]27
            $__aterm_bel = [char]7
            $__aterm_suffix = __aterm_id_suffix
            # Report command text for session memory (OSC 633;E)
            $__aterm_out = "$__aterm_esc]633;E;$(__aterm_encode_cmd $__aterm_line)$__aterm_suffix$__aterm_bel"
            # Set tab title to running command (OSC 0).
            # Truncate to first 64 chars and strip control characters.
            if (-not $env:ATERM_DISABLE_PROMPT_TITLES) {
                $__aterm_title = $__aterm_line
                if ($__aterm_title.Length -gt 64) { $__aterm_title = $__aterm_title.Substring(0, 64) }
                $__aterm_title = $__aterm_title -replace '[\x00-\x1f\x7f]', ''
                $__aterm_out += "$__aterm_esc]0;$__aterm_title$__aterm_bel"
            }
            $__aterm_out += "$__aterm_esc]133;C$__aterm_suffix$__aterm_bel"
            [Console]::Write($__aterm_out)
        }
        $__aterm_line
    }
}
