#!/usr/bin/env bash
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# install.sh — the aterm install: the released aterm.app AND the `aterm`
# command (ONE name on PATH; it fronts every verb — aterm help / ctl / pkg /
# fleet / drive), in one command. Current macOS releases are lean apps; Linux
# releases provide one executable. The ALab toolchain installs separately: on a
# lean app's first launch with live progress when the installing user is at this
# Mac's console, else here (toolset_deferral), unless excluded. A run from a
# terminal at the console opens that app itself (app_opens_here).
#
# The download source is the public release repo (alabsystems/aterm; a run from
# a checkout derives the slug from its Cargo.toml, ATERM_REPO_SLUG names
# another), read only from github.com's release download host — the unmetered
# host the in-app updater reads — with curl, no credential, and no GitHub API
# request.
#
# The halves:
#   app — macOS: the released aterm.app, verified with a deliberately weaker
#         bootstrap tier than the installed updater (docs/RELEASING.md):
#           1. one HEAD of the channel's latest-release pointer
#              (…/releases/latest/download/aterm-appcast.toml, redirects
#              refused) names the head's vMAJOR.MINOR.PATCH tag (channel_head);
#              an explicit --version names the tag instead
#              (canonical_authority_tag)
#           2. fetch the manifest and one canonical container from that tag's
#              own download URLs — the lean aterm-<v>-mac.zip, or for a pinned
#              pre-lean release its bare aterm-<v>.dmg (elect_container) — each
#              held to the byte size the host names
#           3. bind tag == manifest version == container filename, then verify
#              the container's SHA-256 against that manifest
#           4. verify the bundle's code signature; on the official repo the
#              Developer-ID team is pinned in this script (OFFICIAL_TEAM_ID) and
#              a manifest that omits or contradicts it is refused; elsewhere the
#              pin comes from manifest team_id or $ATERM_TEAM_ID. A pinned team
#              requires the full Developer-ID requirement chain + notarization
#           5. swap aterm.app into /Applications — or ~/Applications when
#              /Applications isn't user-writable — replacing any existing copy
#         BOOTSTRAP TRUST BOUNDARY: releases publish Developer-ID-signed,
#         notarized builds plus Ed25519 .sig assets, but this script cannot
#         verify Ed25519 (macOS's stock LibreSSL has no support), so the
#         bootstrap root of trust is the APPLE CODE-SIGNING CHAIN via the
#         pinned Team ID, plus TLS to github.com and the manifest digest.
#         Full Ed25519 verification lives in the installed updater.
#         LINUX (aarch64 and x86_64): installs the elected release's raw ELF
#         aterm-<version>-linux-<arch> into the cli store below, fetched from the
#         same download host. The bootstrap verifier verifies the pinned
#         paper-master roster, live machine signature, signed target
#         name/size/hash, existing rollback floors, and ELF architecture before
#         any execution: the static aterm-bootstrap-verify asset of the release
#         pinned below, held to its SHA-256 (sha256sum or shasum), or, until a
#         release publishes it, the embedded one that Python 3.11+ and OpenSSL
#         run. A checksum-only legacy tarball is never an automatic fallback.
#         The installed runtime then independently verifies the proof and
#         enrolls this managed binary for signed updates. A source fallback is
#         NOT automatically enrolled.
#   cli — the `aterm` command: one executable serves the terminal modes and
#         dispatches product commands in-process. ONE name lands on PATH, per
#         [workspace.metadata.atpkg]; compatibility argv0 aliases name the same
#         executable. Preferred macOS source: Contents/MacOS/aterm in the
#         installed app. find_bundle_cli also accepts the previous bundle
#         layout, whose aterm-cli front door needs its four verb executables.
#         The symlink tracks app updates and needs no compiler or checkout.
#         A released Linux binary already installed in the private store is
#         reused. Otherwise the fallback builds this checkout's ONE executable
#         into ~/.local/lib/aterm/bin (override ATERM_STORE_DIR), with the
#         `aterm` symlink in ~/.local/bin. It needs the pinned `trust`
#         toolchain: the managed store's targo is preferred, else rustup's
#         proxy and a custom `trust` link (find_cargo_and_rustup). An unavailable
#         toolchain skips that fallback with a remedy. A PATH hint prints if
#         ~/.local/bin isn't on PATH.
#   toolset — `aterm pkg seed` + `pkg update` (a fresh lean install defers it
#         to the app's first launch when the installing user owns this Mac's
#         console, and runs it here otherwise: toolset_deferral); PATH — one
#         marker-fenced block in your shell profile. Each has its --no-… flag.
#
# Failsafe policy: each half is pre-flighted before any install work; a half
# that is impossible here (piped script with no checkout, an OS/arch with no
# released artifact, a private or unreadable repo, no pinned toolchain, no
# release yet or a head still being published, an unwritable destination) is
# SKIPPED with a loud reason and the rest still installs. Exit 1 when nothing
# was installed. A real mid-flight failure (download, SHA-256 / signature
# verification, build) always aborts non-zero. --version selects the app
# release only: the symlinked `aterm` follows whatever app is installed, and
# the source fallback always builds the checkout.
#
# Usage:
#   tools/install.sh                                  # the recommended install: aterm
#                                                     # opens immediately; the ALab
#                                                     # toolchain installs itself on
#                                                     # first launch with live progress
#   tools/install.sh --no-cli                         # exclude the `aterm` command
#   tools/install.sh --no-app                         # exclude the app
#   tools/install.sh --no-open                        # install the app, leave it closed
#   tools/install.sh --no-toolchain                   # lean zip, toolset excluded, and it
#                                                     # stays excluded: writes
#                                                     # `[packages] auto_install = false`
#                                                     # into ~/.config/aterm/aterm.toml
#                                                     # (never over a value already there)
#   tools/install.sh --no-path                        # skip this script's PATH block (the
#                                                     # toolset still adds its own to each
#                                                     # existing shell rc and your login
#                                                     # shell's, made when absent)
#   tools/install.sh --version 0.5.0                  # pin the app release
#   tools/install.sh --dry-run                        # print the whole plan, change nothing
#   tools/install.sh --uninstall                      # reverse everything it installed
#   tools/install.sh --uninstall --dry-run            # ...show what that would remove
#   ATERM_REPO_SLUG=my-org/aterm tools/install.sh     # fork / relocated repo
#   ATERM_INSTALL_DIR="$HOME/Applications" tools/install.sh
#   ATERM_BIN_DIR=/usr/local/bin tools/install.sh     # the `aterm` symlink elsewhere
#   ATERM_STORE_DIR=/opt/aterm tools/install.sh       # source-built toolset elsewhere
#   ATERM_TEAM_ID=ABCDE12345 tools/install.sh         # require this signing team
#   ATERM_NO_TOOLCHAIN=1 tools/install.sh             # as --no-toolchain (for CI)
#   ATERM_NO_PATH=1 tools/install.sh                  # as --no-path
#
# One-liner, no clone needed (the cli half symlinks from the just-installed
# app when its bundle has the current executable or a supported legacy CLI
# toolset; otherwise the cli half is skipped, since building needs a checkout):
#   curl -fsSL https://alab.systems/install.sh | bash
# alab.systems serves this file as of the released public commit (aterm's
# `ship site` publishes it); piped, it installs from alabsystems/aterm. The
# same file at the public main's tip:
#   curl -fsSL https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh | bash
# The whole-file guard (WHOLE-FILE EXECUTION GUARD, at main): this group runs
# to the file's last line, and bash runs none of it until all of it arrived.
{
# `sh install.sh` runs bash in POSIX mode (macOS's /bin/sh is bash 3.2), where
# a command substitution keeps set -e: a check run in one ends at its first
# failing command, before it prints what failed, and its caller reads the
# empty answer as "nothing failed". This script is written for bash's own
# mode, so it leaves POSIX mode before anything runs (bash 4.4 and later keep
# inherit_errexit on after that). dash stops before any of it runs: it
# cannot parse the whole-file group.
if [ -n "${BASH_VERSION:-}" ]; then
	set +o posix
	shopt -u inherit_errexit 2>/dev/null || :
fi
set -euo pipefail

# Ordinary exported command functions must not override root's fixed OS tools.
# This begins after Bash startup; it does not establish custody of the shell
# interpreter, BASH_ENV, or a replacement of the builtin dispatcher itself.
# The list comes in as a here-string: bash 3.2 in POSIX mode (macOS's sh)
# parses no process substitution, and `sh -n install.sh` parses the whole file
# in that mode, since it runs nothing to leave it.
discard_inherited_functions() {
	builtin local inherited
	while builtin read -r inherited; do
		# This definition already replaced any imported one and remains callable
		# by the private controlled-subshell fixture after a root library load.
		[[ -z "$inherited" || "$inherited" == discard_inherited_functions ]] ||
			builtin unset -f -- "$inherited"
	done <<<"$(builtin compgen -A function)"
}
if [[ "$EUID" == 0 ]]; then
	discard_inherited_functions
fi

# Bash sets EUID from the process and makes it readonly. The private library
# fixtures replace this seam; production never asks an inherited PATH's id.
installer_uid() { printf '%s\n' "$EUID"; }

# The OS's own program folders, which sudo itself comes from.
ROOT_SYSTEM_PATH=/usr/bin:/bin:/usr/sbin:/sbin

# Root's bootstrap utilities come from fixed OS folders. Keep the caller's
# PATH for reading candidate locations, never for running metadata programs.
# A host without these standard utilities can install as its account instead.
# Root's temporary folders go in /tmp, whatever TMPDIR said: `sudo -E`, doas's
# keepenv or a musl sudo keeps an account's own TMPDIR, where that account
# could rename the folder mktemp made for root and put one of its own in its
# place, between root checking a download and running it; /tmp's sticky bit
# lets no account rename root's.
root_metadata_path() {
	unset ROOT_LOOKUP_PATH
	[[ "$(installer_uid)" == 0 ]] || return 0
	ROOT_LOOKUP_PATH="$PATH"
	PATH="$ROOT_SYSTEM_PATH"
	export TMPDIR=/tmp
	local tool
	for tool in id uname ls find awk sed head dirname readlink env; do
		if ! builtin type -P -- "$tool" >/dev/null 2>&1; then
			echo "install.sh: root needs trusted system metadata tools ($tool is unavailable); run install.sh as your account, without sudo" >&2
			return 1
		fi
	done
	# Without getfacl, root cannot tell an entry with an ACL from one without,
	# so each check of what root may run or build from refuses, and says
	# another account can change it. The signed binary's verifier reads ACLs
	# itself, so that half still installs: a note, not a refusal.
	if [[ "$(uname -s)" == Linux ]] && ! linux_acl_tool >/dev/null; then
		tool="$(linux_acl_tools)"
		echo "install.sh: NOTE: root reads ACLs with getfacl, which this system lacks (${tool//$'\n'/ }), so it takes each file and folder it checks for one another account can change, and runs or builds from none — install the acl package (apt-get install acl, dnf install acl, apk add acl), or run install.sh as your account, without sudo" >&2
	fi
}

# Looking up a candidate executes nothing. Root still judges its absolute
# path with may_run before asking it; nonroot keeps ordinary shell lookup.
lookup_program() { # <name>
	if [[ "$(installer_uid)" == 0 ]]; then
		local p
		p="$(PATH="${ROOT_LOOKUP_PATH-$PATH}" builtin type -P -- "$1")" || return 1
		# Validation and execution can be separated by cd into the checkout.
		[[ "$p" == /* ]] || p="$PWD/$p"
		printf '%s\n' "$p"
	else
		command -v -- "$1"
	fi
}

# Is this file really on disk (vs piped/`bash -s`, where BASH_SOURCE is unusable —
# and on bash >= 5 points at the bash binary itself, so -r alone is not enough)?
self_on_disk() {
	[[ -r "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == *install.sh ]]
}

# The same page whether the script is on disk or piped. The header comment
# above is the design record for readers of this file, not the usage.
usage() {
	cat <<'USAGE'
usage: install.sh [--no-app] [--no-open] [--no-cli] [--no-toolchain] [--no-path] [--version X.Y.Z] [--dry-run]
       install.sh --uninstall [--dry-run]

Installs aterm.app (macOS) and the `aterm` command; the ALab toolset installs at
the first launch, or here with --no-app, over SSH and on Linux. Run at this
Mac's screen from a terminal, it opens the app it installed.
On Linux the app is the signed `aterm` binary, which updates itself (aterm update status).

  --no-app          leave the app alone (Linux: the signed `aterm` binary)
  --no-open         install the app but do not open it
  --no-cli          leave the `aterm` command alone
  --no-toolchain    skip the ALab toolset, and keep it skipped: writes
                    [packages] auto_install = false into aterm.toml
                    ($XDG_CONFIG_HOME/aterm, default ~/.config/aterm)
  --no-path         skip this script's PATH block (the toolset still adds its
                    own to each existing shell rc and to your login shell's,
                    which it makes when absent; delete it and it stays deleted;
                    `aterm pkg repair` puts it back)
  --version X.Y.Z   install that release instead of the latest
  --dry-run         print the plan and change nothing
  --uninstall       remove what install.sh installed (runs the installed
                    aterm's own sweep: aterm pkg self-uninstall)

env:
  ATERM_INSTALL_DIR    where aterm.app goes (default /Applications, else ~/Applications)
  ATERM_BIN_DIR        where the `aterm` command goes (default ~/.local/bin)
  ATERM_STORE_DIR      where the `aterm` binary goes when there is no app (Linux release
                       or source build; default ~/.local/lib/aterm/bin) — a directory
                       of its own, apart from ATERM_BIN_DIR and ~/.local/bin
  ATERM_MAN_DIR        where man pages go (default ~/.local/share/man)
  ATERM_REPO_SLUG      the release repo (default alabsystems/aterm)
  ATERM_TEAM_ID        require this signing team
  ATERM_NO_TOOLCHAIN=1 as --no-toolchain
  ATERM_NO_PATH=1      as --no-path
USAGE
}

# --- pure update-channel arbitration (shared with deterministic shell tests) ---

# Match the updater's u64 component domain without asking Bash 3.2 to perform
# signed-machine-word arithmetic. Components reach this already canonical (no
# leading zeroes), so equal-length decimal strings compare lexically == numerically.
numeric_component_in_u64() {
	local component="$1"
	local LC_ALL=C
	[[ "$component" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
	[[ "${#component}" -lt 20 ]] ||
		[[ "${#component}" -eq 20 && ( "$component" == 18446744073709551615 || "$component" < 18446744073709551615 ) ]]
}

# Compare a canonical unsigned decimal string against small canonical bounds
# without signed arithmetic or octal interpretation.
decimal_in_closed_range() {
	local value="$1" minimum="$2" maximum="$3"
	local LC_ALL=C
	[[ "$value" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
	if [[ "${#value}" -lt "${#minimum}" ]] ||
		[[ "${#value}" -eq "${#minimum}" && "$value" < "$minimum" ]]; then
		return 1
	fi
	if [[ "${#value}" -gt "${#maximum}" ]] ||
		[[ "${#value}" -eq "${#maximum}" && "$value" > "$maximum" ]]; then
		return 1
	fi
}

# Classify one published release tag, mirroring the updater's parse_numeric_tag
# (crates/aterm-update/src/github.rs). Sets TAG_KIND_RESULT to:
#   candidate — the current scheme: canonical vMAJOR.MINOR.PATCH, the
#               workspace [workspace.package] version as written
#               (VERSIONING.md). Only these are ever installed.
#   legacy    — two components, vMAJOR.MINOR: a retired pre-cut-over archive
#               tag, which is never installed (canonical_authority_tag refuses
#               it by kind), or a macOS product version such as 14.0, which the
#               min_os advisory compares.
# Everything else fails closed (returns 1): no v prefix, fewer than two or more
# than three components, an empty or non-numeric component, a leading-zero
# spelling, or a component outside the updater's u64 domain. Garbage in the tag
# namespace must stop the check, never silently narrow the candidate set.
parse_release_tag() {
	local tag="$1" body part
	# ASCII digits only, in every locale — the updater matches is_ascii_digit.
	local LC_ALL=C
	local -a parts
	TAG_KIND_RESULT=""
	[[ "$tag" == v* ]] || return 1
	body="${tag#v}"
	# Splitting discards a final empty field, so validate the whole grammar
	# first; this rejects both v0.55. and v0..55 before component parsing.
	[[ "$body" =~ ^[0-9]+(\.[0-9]+)+$ ]] || return 1
	# Split by word splitting on the dots, not `read -a <<<`: Bash 3.2 writes
	# every here-string to a temporary file, and this runs once per tag. The
	# body is ASCII digits and dots alone (just matched), so no field can glob;
	# IFS is this function's own, and nothing below expands unquoted.
	local IFS=.
	parts=($body)
	for part in "${parts[@]}"; do
		# Reject leading-zero spellings so a tag has exactly one canonical form
		# and two distinct tags can never share a numeric order. The u64 bound is
		# checked before the arity split, exactly as the updater does: an
		# overflowing two-component tag is malformed, not a tolerated legacy row.
		[[ "${#part}" -eq 1 || "${part:0:1}" != 0 ]] || return 1
		numeric_component_in_u64 "$part" || return 1
	done
	case "${#parts[@]}" in
	3) TAG_KIND_RESULT=candidate ;;
	2) TAG_KIND_RESULT=legacy ;;
	*) return 1 ;;
	esac
}

# A canonical numeric dotted version of either arity: the shape gate for the
# min_os advisory, whose operands are macOS product versions (14.0 has two
# components), not aterm tags. A release tag is canonical_authority_tag's.
canonical_numeric_tag() {
	parse_release_tag "$1"
}

# The authority contract: the release the channel elects is spelled exactly
# vMAJOR.MINOR.PATCH. A retired two-component release can never hold this
# position — the same refusal as the updater's canonical_authority_version.
canonical_authority_tag() {
	parse_release_tag "$1" && [[ "$TAG_KIND_RESULT" == candidate ]]
}

# Set TAG_COMPARE_RESULT to -1, 0, or 1 using decimal-string component
# comparison: shorter canonical decimal first, then lexical within a width, so
# 9 < 10 and 99 < 100 order numerically. macOS still ships Bash 3.2, so this
# deliberately avoids namerefs, associative arrays, and signed-machine-word
# arithmetic. Both operands must already be canonical (parse_release_tag), which
# is what makes "same length ⇒ lexical == numeric" hold.
compare_numeric_tags() {
	local lhs="$1" rhs="$2" i common l r
	local LC_ALL=C
	local -a left right
	parse_release_tag "$lhs" && parse_release_tag "$rhs" || return 2
	# Split as parse_release_tag splits (no here-string, no temporary file):
	# both bodies are ASCII digits and dots alone, so no field can glob.
	local IFS=.
	left=(${lhs#v})
	right=(${rhs#v})
	common="${#left[@]}"
	[[ "${#right[@]}" -lt "$common" ]] && common="${#right[@]}"
	for ((i = 0; i < common; i++)); do
		l="${left[$i]}"
		r="${right[$i]}"
		if [[ "${#l}" -lt "${#r}" ]]; then TAG_COMPARE_RESULT=-1; return 0; fi
		if [[ "${#l}" -gt "${#r}" ]]; then TAG_COMPARE_RESULT=1; return 0; fi
		if [[ "$l" < "$r" ]]; then TAG_COMPARE_RESULT=-1; return 0; fi
		if [[ "$l" > "$r" ]]; then TAG_COMPARE_RESULT=1; return 0; fi
	done
	if [[ "${#left[@]}" -lt "${#right[@]}" ]]; then
		TAG_COMPARE_RESULT=-1
	elif [[ "${#left[@]}" -gt "${#right[@]}" ]]; then
		TAG_COMPARE_RESULT=1
	else
		TAG_COMPARE_RESULT=0
	fi
}

# Bind tag, manifest version, local path shape, and digest before any DMG lookup,
# join, or download. The tag is vMAJOR.MINOR.PATCH, elected or pinned, and the
# tag, the manifest spelling, and aterm-<version>.dmg must agree exactly — the
# same identity bind the updater enforces.
validate_manifest_identity() {
	local tag="$1" version="$2" dmg_name="$3" sha256="$4"
	if ! canonical_authority_tag "$tag" || [[ "$tag" != "v$version" ]]; then
		echo "install.sh: manifest version $version does not exactly match release tag $tag" >&2
		return 2
	fi
	if [[ "$dmg_name" != "aterm-$version.dmg" ]]; then
		echo "install.sh: manifest DMG $dmg_name is not canonical aterm-$version.dmg" >&2
		return 2
	fi
	if [[ ! "$sha256" =~ ^[0-9a-fA-F]{64}$ ]]; then
		echo "install.sh: manifest sha256 is not exactly 64 hexadecimal digits" >&2
		return 2
	fi
}

# --- the one transport: github.com's release download host --------------------
#
# Every byte comes from `https://github.com/<owner>/<repo>/releases/…` with no
# credential — the unmetered host the in-app updater reads (crates/aterm-update
# github.rs, crates/aterm-update-core pointer.rs). Nothing here calls the GitHub
# API: no catalog walk, no asset metadata, no shared 60-requests/hour budget.
#
#   the head   one HEAD of …/releases/latest/download/aterm-appcast.toml with
#              redirects refused. GitHub answers 302 to the tag-specific URL of
#              the newest published release — drafts and prereleases excluded on
#              the server — and the release cut owns that pointer, so it names
#              the newest APP release. Every redirect code the updater reads
#              (301, 302, 303, 307, 308) is read alike. The Location is
#              accepted only when it is exactly the derived appcast URL of this
#              repository under a canonical vMAJOR.MINOR.PATCH tag; anything
#              else is refused, never followed.
#   the bytes  tag-specific …/releases/download/<tag>/<name> URLs, never `latest`
#              again, so a pointer that moves mid-install cannot mix two
#              releases. A HEAD that refuses redirects answers 302 when the asset
#              exists and 404 when it does not; a HEAD that follows them names
#              its size (the CDN's Content-Length); a GET streams it, bounded.
#
# Authenticity is not the transport's job: the manifest digest and the Apple
# code-signing chain decide what installs (install_app).

RELEASE_HOST="https://github.com"
# Upper bound for the manifest (a few KiB).
MANIFEST_MAX_BYTES=5000000

# Every request goes through here: https only, redirects included, a bounded
# connect, never a credential. One function, so the suite stubs the transport
# in one place.
web_curl() {
	curl --proto '=https' --proto-redir '=https' --connect-timeout 10 "$@"
}

# The tag-specific download URL of one asset, on stdout — or a refusal (status 2)
# for a tag or a name outside the allowlist, so a manifest can never name an
# arbitrary asset of the release.
release_asset_url() { # <tag> <name>
	local tag="$1" name="$2"
	canonical_authority_tag "$tag" || {
		echo "install.sh: refusing asset lookup for invalid release tag $tag" >&2
		return 2
	}
	case "$name" in
	aterm-appcast.toml | aterm-appcast.toml.sig | aterm-machines.toml | aterm-machines.toml.sig) ;;
	"aterm-appcast-$tag.toml" | "aterm-appcast-$tag.toml.sig") ;;
	*)
		# Two canonical macOS container names, both anchored and version-shaped:
		# the lean zip every current install comes from, and the bare DMG the
		# manifest's required `dmg` key binds (a pinned pre-lean release installs
		# from it). The retired Intel `-x86_64.dmg` is not here: a manifest naming
		# it names a foreign asset. The Linux rows are the released one binary, a
		# raw ELF per architecture, the static bootstrap verifier beside it, and
		# the retired checksum-only tarball pair, which stays recognizable for
		# archive diagnostics; recognizing a name does not authorize
		# installation. An explicit allowlist rather than a loosened pattern:
		# each suffix is exactly as constrained as `.dmg`.
		[[ "$name" =~ ^aterm-[0-9]+(\.[0-9]+)+\.dmg$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-mac\.zip$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-linux-(aarch64|x86_64)$ ||
			"$name" =~ ^aterm-bootstrap-verify-[0-9]+(\.[0-9]+)+-linux-(aarch64|x86_64)$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-linux-x86_64\.tar\.gz(\.sha256)?$ ]] || {
			echo "install.sh: refusing asset lookup for noncanonical name $name" >&2
			return 2
		}
		;;
	esac
	printf '%s/%s/releases/download/%s/%s\n' "$RELEASE_HOST" "$REPO_SLUG" "$tag" "$name"
}

# The channel head: the tag GitHub's `latest` names, from one HEAD with redirects
# refused. Sets HEAD_TAG (status 0), or HEAD_WHY and status 1 when there is
# nothing to install from it (no published release, an unreachable host, a head
# that is not an app release — a skip), or status 2 when the redirect is one this
# script refuses to interpret (a refusal, never "follow and hope"). Called
# directly, never in $(…), so the two globals reach the caller.
#
# Every redirect code the updater reads a pointer from is read here, and alike
# (crates/aterm-update-core pointer.rs classify: 301, 302, 303, 307, 308).
# github.com answers 302 today; a 301 or a 307 from the same host names the
# same release, so the code alone must never strand a fresh install on a
# channel every installed copy still follows. The Location decides.
HEAD_TAG=""
HEAD_WHY=""
channel_head() {
	local url="$RELEASE_HOST/$REPO_SLUG/releases/latest/download/aterm-appcast.toml"
	local answer code location rest tag rc=0
	HEAD_TAG=""
	HEAD_WHY=""
	answer="$(web_curl -sS -I -o /dev/null --max-time 30 -w '%{http_code} %{redirect_url}' "$url" 2>/dev/null)" || rc=$?
	if [[ "$rc" -ne 0 ]]; then
		HEAD_WHY="could not reach $url (curl exit $rc: the network, DNS or TLS)"
		return 1
	fi
	code="${answer%% *}"
	location="${answer#* }"
	case "$code" in
	301 | 302 | 303 | 307 | 308) ;;
	404)
		HEAD_WHY="$REPO_SLUG has no published release (or the repository is private, renamed or missing — github.com answers the same for all three)"
		return 1
		;;
	429 | 5[0-9][0-9])
		HEAD_WHY="github.com answered HTTP $code to $url — retry in a few minutes"
		return 1
		;;
	*)
		HEAD_WHY="github.com answered HTTP ${code:-000} to $url, which is not a release pointer (a proxy or a filtered host?)"
		return 1
		;;
	esac
	# The Location must be exactly <host>/<slug>/releases/download/<tag>/
	# aterm-appcast.toml with <tag> one path segment: strip the derived prefix
	# and suffix, and anything left over (another host, repository or asset, a
	# query, a second segment) refuses. What remains is then the derived URL for
	# <tag> by construction — every later request is addressed to that.
	rest="${location#"$RELEASE_HOST/$REPO_SLUG/releases/download/"}"
	tag="${rest%/aterm-appcast.toml}"
	if [[ "$rest" == "$location" || "$tag" == "$rest" || -z "$tag" || "$tag" == */* ]]; then
		HEAD_WHY="the latest-release pointer of $REPO_SLUG redirected somewhere this installer refuses to follow (not the tag-specific aterm-appcast.toml URL of this repository)"
		return 2
	fi
	if ! canonical_authority_tag "$tag"; then
		HEAD_WHY="the latest release of $REPO_SLUG is ${tag//[^A-Za-z0-9._-]/?}, which is not an app release (vMAJOR.MINOR.PATCH) — name one with --version"
		return 1
	fi
	HEAD_TAG="$tag"
}

# Is <name> published on release <tag>? One HEAD with redirects refused: 302 (to
# the asset CDN) is present, 404 is absent. Status 0 present, 1 absent, 2 no
# answer either way (ASSET_WHY says why). Called directly, never in $(…).
ASSET_WHY=""
release_asset_present() { # <tag> <name>
	local url code rc=0
	ASSET_WHY=""
	url="$(release_asset_url "$1" "$2")" || return 2
	code="$(web_curl -sS -I -o /dev/null --max-time 30 -w '%{http_code}' "$url" 2>/dev/null)" || rc=$?
	if [[ "$rc" -ne 0 ]]; then
		ASSET_WHY="could not reach $url (curl exit $rc: the network, DNS or TLS)"
		return 2
	fi
	case "$code" in
	302) return 0 ;;
	404) return 1 ;;
	*)
		ASSET_WHY="github.com answered HTTP ${code:-000} to $url"
		return 2
		;;
	esac
}

# The size, in bytes, the download host serves for <name> of <tag>: one HEAD that
# follows the redirect, whose final Content-Length is the answer. It must lie in
# 1..<max> (default 2 GiB); the download is then held to exactly that many bytes.
release_asset_size() { # <tag> <name> [max-bytes]
	local tag="$1" name="$2" max="${3:-2147483648}" url headers size
	url="$(release_asset_url "$tag" "$name")" || return 2
	headers="$(web_curl -fsSIL --max-time 60 "$url" 2>/dev/null)" || {
		echo "install.sh: could not read the size of $name in release $tag ($url)" >&2
		return 1
	}
	size="$(printf '%s\n' "$headers" | tr -d '\r' |
		awk 'tolower($1) == "content-length:" { v = $2 } END { print v }')"
	if ! decimal_in_closed_range "$size" 1 "$max"; then
		echo "install.sh: $name in release $tag has no size in 1..$max bytes (the download host said '${size:-none}')" >&2
		return 2
	fi
	printf '%s\n' "$size"
}

# Stream <name> of <tag> to stdout, at most <max> bytes plus one: the caller holds
# what arrived to the size it expects, so an overrunning body fails instead of
# filling the disk, and pipefail fails an overrunning producer too.
# --max-filesize lets curl refuse an oversize Content-Length before the body.
fetch_release_asset() { # <tag> <name> <max-bytes>
	local url
	url="$(release_asset_url "$1" "$2")" || return 2
	web_curl -fsSL --retry 2 --max-time 3600 --max-filesize "$3" "$url" | head -c "$(($3 + 1))"
}

# --- download progress ---------------------------------------------------------
# The transport is silent (`curl -fsS`, no meter), and minutes of silence in a
# `curl … | bash` read as a hang. The meter polls the destination file rather
# than the transport, so the transport's flags never change, and leans on the
# size the caller already bounded (release_asset_size). Terminal-only: under
# `curl … | bash` just stdin is the pipe, so stderr is still the tty; CI
# redirects stderr and gets clean logs.
DOWNLOAD_METER_PID=""

start_download_meter() {
	local destination="$1" total="$2"
	[[ -t 2 ]] || return 0
	# Below this a meter is noise: the manifest and its signature are a few
	# hundred bytes and complete before the first tick would ever fire.
	[[ "$total" -ge 8000000 ]] || return 0
	download_meter "$destination" "$total" "$$" &
	DOWNLOAD_METER_PID=$!
}

# The meter itself, a tick every 2 s while <run>, the run's own shell, lives. A
# run a Ctrl-C ends under bash 5 never reaches stop_download_meter, and this
# background job ignores the Ctrl-C (a script starts one with SIGINT ignored),
# so it ends by itself rather than draw on over the person's prompt.
download_meter() { # <destination> <total> <run>
	local got pct last=-1
	while sleep 2 && kill -0 "$3" 2>/dev/null; do
		got="$(wc -c 2>/dev/null <"$1" | tr -d '[:space:]')" || got=""
		[[ -n "$got" ]] || continue
		pct=$((got * 100 / $2))
		[[ "$pct" != "$last" ]] || continue
		last="$pct"
		printf '\r  %s MB / %s MB (%s%%)  ' \
			"$((got / 1000000))" "$(($2 / 1000000))" "$pct" >&2
	done
}

# Always safe to call, including when no meter was started. Clears the line so
# the next message never lands on top of a half-drawn percentage.
stop_download_meter() {
	[[ -n "$DOWNLOAD_METER_PID" ]] || return 0
	kill "$DOWNLOAD_METER_PID" 2>/dev/null || true
	wait "$DOWNLOAD_METER_PID" 2>/dev/null || true
	DOWNLOAD_METER_PID=""
	printf '\r%*s\r' 42 '' >&2
	return 0
}

# Download <name> of <tag> to <destination>, which must end up exactly
# <expected-size> bytes — the size release_asset_size read from the host.
download_release_asset() { # <tag> <name> <expected-size> <destination>
	local tag="$1" name="$2" expected_size="$3" destination="$4" actual_size rc=0
	if [[ -z "$destination" ]] || ! decimal_in_closed_range "$expected_size" 1 2147483648; then
		echo "install.sh: refusing malformed release asset download" >&2
		return 2
	fi
	# Read at most one byte beyond the expected size: pipefail fails a producer
	# that overruns the bound, and the exact byte-count check below catches
	# short and one-byte-overlong responses, capping disk exposure even if the
	# size header and the body disagree. The meter is stopped on every path
	# (`|| rc=$?` keeps `set -e` from orphaning it over the error message).
	start_download_meter "$destination" "$expected_size"
	fetch_release_asset "$tag" "$name" "$expected_size" >"$destination" || rc=$?
	stop_download_meter
	# Under bash 3.2 web_curl's subshell, the pipeline's first stage, exits 130
	# once curl dies of the Ctrl-C the whole run got, and the run goes on: end it
	# by the signal, never as a failed download. The run's INT trap does that
	# too, but not in a subshell (linux_verifier_ready downloads in one), which
	# starts with it reset.
	case "$rc" in
	129) die_of HUP 129 ;;
	130) die_of INT 130 ;;
	143) die_of TERM 143 ;;
	esac
	if [[ "$rc" -ne 0 ]]; then
		echo "install.sh: download of $name from release $tag failed" >&2
		return 1
	fi
	actual_size="$(wc -c <"$destination" | tr -d '[:space:]')"
	if [[ "$actual_size" != "$expected_size" ]]; then
		echo "install.sh: $name size mismatch (the download host said $expected_size bytes, $actual_size arrived)" >&2
		return 2
	fi
}

# Parse one emitted TOML string field. Required fields must occur exactly once;
# optional fields may occur zero or one time. This intentionally accepts only
# the release cutter's simple one-line string form rather than implementing a
# permissive shell TOML parser around security-sensitive identity fields.
# The file may be `-` — awk's standard stdin spelling — for a caller holding
# the bytes in a variable (the --dry-run plan, whose contract is zero writes).
toml_single_str() {
	local file="$1" key="$2" required="$3"
	awk -v wanted="$key" -v required="$required" '
		$0 ~ "^[[:space:]]*" wanted "[[:space:]]*=" {
			count += 1
			value = $0
			sub("^[[:space:]]*" wanted "[[:space:]]*=[[:space:]]*", "", value)
			if (value !~ /^"[^"]*"[[:space:]]*$/) {
				malformed = 1
			} else {
				sub(/^"/, "", value)
				sub(/"[[:space:]]*$/, "", value)
			}
		}
		END {
			if (malformed || count > 1 || (required == 1 && count != 1)) exit 2
			if (count == 1) print value
		}
	' "$file"
}

# The manifest's identity fields, parsed into their globals: one parse, shared
# by install_app and the --dry-run plan, so the two can never read different
# fields. Takes the manifest text rather than a path (the plan holds the bytes
# in a variable). Each toml_single_str call keeps its own required/optional
# contract; the first malformed or duplicate field short-circuits non-zero, and
# the caller owns the loud abort. The retired Intel pair's keys (`dmg_x86_64` /
# `dmg_x86_64_sha256`) are not read.
parse_manifest_identity_fields() { # <manifest-text>
	local manifest_text="$1"
	VERSION="$(toml_single_str - version 1 <<<"$manifest_text")" &&
		DMG_NAME="$(toml_single_str - dmg 1 <<<"$manifest_text")" &&
		SHA_WANT="$(toml_single_str - sha256 1 <<<"$manifest_text")" &&
		TEAM_MANIFEST="$(toml_single_str - team_id 0 <<<"$manifest_text")" &&
		MIN_OS="$(toml_single_str - min_os 0 <<<"$manifest_text")" &&
		ZIP_NAME="$(toml_single_str - zip 0 <<<"$manifest_text")" &&
		ZIP_SHA="$(toml_single_str - zip_sha256 0 <<<"$manifest_text")"
}

# --- publisher identity: the official channel's Developer-ID pin ---------------
#
# OFFICIAL_TEAM_ID is the Apple Developer team that signs official
# alabsystems/aterm releases: A66A9P66Z7, "ANDREW DONALD YATES" — the identity
# in the notarized bundle's Developer-ID certificate chain. Pinned in the
# script, not only in the manifest, because the manifest rides this bootstrap
# lane unsigned: a manifest free to omit (or swap) team_id would downgrade
# verification to any ad-hoc signature while the script still said "verified".
OFFICIAL_REPO_SLUG="alabsystems/aterm"
OFFICIAL_TEAM_ID="A66A9P66Z7"

# One bare GitHub name segment (an owner or a repo): the alphabet and bounds of
# crates/aterm-update-core/src/source.rs `is_valid_slug`. In the C locale, as
# the other validators here: under a glibc UTF-8 locale a bracket range follows
# collation order, and [A-Za-z] matches é and fullwidth Ａ, which the updater
# refuses.
valid_slug_segment() {
	local LC_ALL=C
	[[ "$1" =~ ^[A-Za-z0-9._-]{1,100}$ && "$1" != . && "$1" != .. ]]
}

# The update channel a workspace manifest declares, as OWNER/REPO on stdout:
# `[workspace.metadata.aterm] update_channel` when it is a valid slug inside
# that table, else the `[workspace.package] repository` URL. The shipped
# binary's compiled-in channel (aterm_update_core::DEFAULT_OWNER/DEFAULT_REPO)
# is held equal to that key by aterm-release's channel tests. A run from a
# checkout installs from where that checkout's binaries update from — the
# public channel every release is published to — and never from `repository`
# alone: in the development checkout that is the private repository the source
# lives in, which carries no release a tokenless machine can read. Status 1
# when neither rung yields a valid slug (the caller then keeps its default).
channel_slug_from_manifest() { # <Cargo.toml>
	local manifest="$1" slug
	[[ -r "$manifest" ]] || return 1
	slug="$(manifest_table_string "$manifest" workspace.metadata.aterm update_channel)"
	if ! valid_slug "$slug"; then
		slug="$(manifest_table_string "$manifest" workspace.package repository |
			sed -E 's#^https?://github\.com/##; s#^git@github\.com:##; s#\.git$##; s#/$##')"
		valid_slug "$slug" || return 1
	fi
	printf '%s\n' "$slug"
}

# Exactly two valid segments.
valid_slug() {
	local owner="${1%%/*}" repo="${1#*/}"
	[[ "$1" == */* && "$repo" != */* ]] && valid_slug_segment "$owner" && valid_slug_segment "$repo"
}

# One `key = "value"` string out of one `[table]` of a TOML file, on stdout (empty
# when absent): the line-oriented shape the release cutter's `table_string`
# (crates/aterm-release/src/channel.rs) reads, line for line — the exact
# header, then `key = "value"` with the value the first double-quoted run (a
# trailing `# comment` falls outside it), whitespace trimmed.
manifest_table_string() { # <file> <table> <key>
	awk -v header="[$2]" -v key="$3" '
		{ line = $0; sub(/^[ \t]+/, "", line); sub(/[ \t]+$/, "", line) }
		substr(line, 1, 1) == "[" { in_table = (line == header); next }
		!in_table { next }
		{
			eq = index(line, "="); if (eq == 0) next
			name = substr(line, 1, eq - 1); sub(/[ \t]+$/, "", name)
			if (name != key) next
			rest = substr(line, eq + 1); sub(/^[ \t]+/, "", rest)
			if (substr(rest, 1, 1) != "\"") exit
			rest = substr(rest, 2); close_at = index(rest, "\"")
			if (close_at > 0) {
				value = substr(rest, 1, close_at - 1)
				sub(/^[ \t]+/, "", value); sub(/[ \t]+$/, "", value)
				print value
			}
			exit
		}' "$1"
}

# The required signing team for one install, or a refusal. stdout: the Team ID
# to enforce ("" = no pin — the ad-hoc lane, for forks and dev builds only).
# Status 2 when the official channel's manifest omits or contradicts the
# compiled-in pin: that combination is a downgrade, never installable. An
# explicit ATERM_TEAM_ID outranks everything — it is the operator's own pin.
required_team_for() { # <repo-slug> <env-team> <manifest-team>
	local slug="$1" env_team="$2" manifest_team="$3"
	if [[ -n "$env_team" ]]; then
		printf '%s\n' "$env_team"
		return 0
	fi
	if [[ "$slug" == "$OFFICIAL_REPO_SLUG" ]]; then
		if [[ -z "$manifest_team" ]]; then
			echo "install.sh: the $OFFICIAL_REPO_SLUG manifest omits team_id, but official releases are Developer-ID signed by team $OFFICIAL_TEAM_ID — accepting a team-less manifest would silently downgrade verification to any ad-hoc signature. Refusing to install. (A fork redistributing its own builds should install with ATERM_REPO_SLUG=<owner>/<repo>, pinning its team via ATERM_TEAM_ID.)" >&2
			return 2
		fi
		if [[ "$manifest_team" != "$OFFICIAL_TEAM_ID" ]]; then
			echo "install.sh: the $OFFICIAL_REPO_SLUG manifest names signing team '$manifest_team', but official releases are signed by team $OFFICIAL_TEAM_ID — refusing the mismatch. (A fork should install with ATERM_REPO_SLUG=<owner>/<repo> and its own ATERM_TEAM_ID.)" >&2
			return 2
		fi
	fi
	printf '%s\n' "$manifest_team"
}

# One digest, two spellings: shasum prints lowercase, a manifest may carry
# uppercase, and 0xA9 == 0xa9. Shape (exactly 64 hex digits) is bound by the
# identity validators before any comparison reaches here; equality itself is
# case-blind. Empty operands refuse, so a parser miss can never "match".
sha256_equal() {
	local a b
	[[ -n "$1" && -n "$2" ]] || return 1
	a="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
	b="$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')"
	[[ "$a" == "$b" ]]
}

# The idempotence gate for the app half: an unpinned run that elected exactly
# the installed version has nothing to download. Pure — the caller reads the
# bundle's Info.plist and passes the fields. An explicit --version pin always
# reinstalls, and a foreign bundle at the path never short-circuits.
app_already_current() { # <elected_tag> <tag_explicit> <bundle_id> <installed_version>
	[[ "$2" -eq 0 ]] || return 1
	[[ "$3" == "com.aterm.aterm" ]] || return 1
	[[ -n "$4" && "v$4" == "$1" ]] || return 1
}

# `bounded SECS CMD [ARG...]`: CMD's own status, or 124 when it has not
# finished within SECS of the clock (its process group SIGKILLed), stdin
# /dev/null: the shell twin of aterm_uds::deadline, since this script runs alone
# through curl | bash and can source nothing. codesign waits on trustd and spctl
# also on a network lookup; while either does not answer it never returns. Every
# call is bounded at VERIFY_WAIT_SECS — a hang detector, never a latency budget:
# the in-app updater measured codesign at 0.09 s on 851 MB and spctl under half a
# second (crates/aterm-update/src/verify.rs), and a binary asked what it is
# (first_line_bounded) answers at once after its first exec's wait on the
# system's code assessment, seconds on a loaded machine. A Ctrl-C, hang-up or
# TERM to this script's process group (a terminal's Ctrl-C reaches it whole)
# while CMD runs SIGKILLs CMD's group and ends the run with that signal
# (die_of), as it would have ended without bounded, never as a status a
# caller could take for CMD's answer. The run then never reaches a caller's
# own rm, so bounded removes the files the caller names in BOUNDED_TEMPS, a
# local array a nested caller extends rather than shadows (bounded_cut).
# BOUNDED_OUT and BOUNDED_ERR, set on the call, name the files CMD's output
# and errors go to, opened in bounded's own subshell: a redirection of the call
# holds in this shell too, so the run's trap and its EXIT cleanup ran with
# their NOTEs going into that file.
VERIFY_WAIT_SECS=60
bounded() {
	local secs="$1"
	shift
	(
		[[ -z "${BOUNDED_OUT:-}" ]] || exec >"$BOUNDED_OUT"
		[[ -z "${BOUNDED_ERR:-}" ]] || exec 3>"$BOUNDED_ERR"
		unset BOUNDED_OUT BOUNDED_ERR
		set -m
		"$@" 2>&3 </dev/null &
		pid=$!
		set +m
		trap 'bounded_cut INT 130' INT
		trap 'bounded_cut TERM 143' TERM
		trap 'bounded_cut HUP 129' HUP
		trap 'bounded_cut QUIT 131' QUIT
		deadline=$((SECONDS + secs))
		looks=0
		while kill -0 "$pid" 2>/dev/null; do
			if [[ "$SECONDS" -ge "$deadline" ]]; then
				kill -KILL -- "-$pid" 2>/dev/null || true
				exit 124
			fi
			if [[ "$looks" -lt 10 ]]; then
				sleep 0.02
			else
				sleep 0.2
			fi
			looks=$((looks + 1))
		done
		status=0
		wait "$pid" || status=$?
		exit "$status"
	) 3>&2 2>/dev/null
}

# bounded's trap for <signal>: SIGKILL CMD's group, which bounded's $pid
# leads, remove the caller's BOUNDED_TEMPS, and end by the signal (die_of).
# set -e holds in a trap whatever its caller's context, so neither step may end
# it first: a group already gone (CMD ended just before the signal) answers
# kill with 1. A QUIT ends it by <status>, as run_cut ends the run: die_of's
# self-kill of it on bash 3.2 is a death by a core-dump signal, for which macOS
# writes a crash report, and the run's own shell, which the terminal sent the
# same Ctrl-\, ends the run (run_cut). A $(…) subshell between the two starts
# with its QUIT at the default, so on bash 3.2 it still dies of that Ctrl-\.
bounded_cut() { # <signal> <status>
	kill -KILL -- "-$pid" 2>/dev/null || true
	rm -f -- ${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"} 2>/dev/null || true
	[[ "$1" != QUIT ]] || exit "$2"
	die_of "$1" "$2"
}

# Ends the shell it runs in by <signal>, its trap reset first. The run's own
# shell runs its EXIT trap and dies of it. A subshell's death of it ends the
# shell waiting on it, which got the same signal (the terminal sends a Ctrl-C
# to its whole process group), too: bash ends a waiting shell whose child died
# of the SIGINT it got, but goes on after a child's ordinary exit
# (wait-and-cooperative-exit). sh's parent is this shell, whose pid bash 3.2
# cannot name in a subshell. <status> if it lives on.
die_of() { # <signal> <status>
	trap - "$1"
	kill -s "$1" "$(exec sh -c 'echo "$PPID"')" || true
	exit "$2"
}

# A Ctrl-C ends the run by it, whatever the child it lands in does with it. The
# terminal sends it to the whole foreground group, but bash ends a waiting
# shell only when its child died of it (wait-and-cooperative-exit), and some
# children catch it and exit: python3 while it starts up, hdiutil. After one,
# an `if` or a `||` read the exit as an answer and the run went on (a Linux run
# skipped its app as having no Python 3.11, then seeded the toolset), and
# elsewhere the run ended with the child's status, not by the Ctrl-C. bash runs
# this trap once the child returns, whatever its status. A subshell starts with
# it reset, as with any caught signal. main sets it first, and the test seam
# before it returns, so a suite's functions run under it as a run's do. While
# `curl | bash` still delivers the script none of it has run (the whole-file
# group), so a Ctrl-C that cuts the download short ends bash by the signal,
# no trap set yet; a trap set at the top level once ran as the script arrived,
# and such a Ctrl-C ended it with a syntax error or status 0. A Ctrl-\ (QUIT)
# in the run needs the trap too: bash 5 ignores it, so the run read a child's
# death of it as an answer, and bash 3.2 died of it without running its EXIT
# trap. One while the script still arrives under bash 5 ends it as a cut
# download does, with a syntax error and nothing run (status 0 when the cut
# falls in the comments above the group), not by the signal as a Ctrl-C there
# does: bash 5 ignores it and curl dies of it, and no trap can be set before
# the script has arrived, so that one exception stands.
end_on_ctrl_c() {
	trap 'run_cut INT 130' INT
	trap 'run_cut QUIT 131' QUIT
}

# The run's trap for <signal>: remove the BOUNDED_TEMPS of the step it cut
# short (a trap runs in the function it interrupted, so it sees that
# function's local array), then end the run by the signal (die_of). A QUIT
# ends it by <status> instead: die_of's self-kill does nothing on bash 5, which
# ignores QUIT, and on bash 3.2 kills the shell before its EXIT trap runs. A
# second QUIT that bash 3.2 takes while this runs skipped the EXIT trap as
# well (a key held down repeats it), so a QUIT is ignored from the first line.
run_cut() { # <signal> <status>
	[[ "$1" != QUIT ]] || trap '' QUIT
	rm -f -- ${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"} 2>/dev/null || true
	[[ "$1" != QUIT ]] || exit "$2"
	die_of "$1" "$2"
}

# <dir>/<prefix>.<16 hex digits>, eight bytes of /dev/urandom (od reads
# exactly its count, so pipefail sees no SIGPIPE), or status 1: a name nothing
# has made, for a file or folder its caller names before making it. Drawing
# it makes nothing and touches no folder. macOS's mktemp -u makes the file and
# removes it again (mkstemp, then unlink), so a signal between the two left a
# file nothing named, and run with the signals ignored to close that, a
# mktemp held up on its folder held a Ctrl-C until it returned.
temp_path() { # <dir> <prefix>
	local s
	s="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')" && [[ "$s" =~ ^[0-9a-f]{16}$ ]] || return 1
	printf '%s/%s.%s\n' "$1" "$2" "$s"
}

# Set <var> to a new private file in <dir> (TMPDIR when none is given), named
# in the caller's BOUNDED_TEMPS (a local array it declares first) before the
# file exists, so run_cut removes it whenever the signal lands. Made by mktemp
# and named after, one a Ctrl-C caught in between stayed (about one interrupt
# of a check in twelve). temp_path draws the name, and a noclobber redirection
# makes the file, with mode 0600 as mktemp would: bash refuses a regular file
# at the name (or a link to one), and opens a name where it finds nothing with
# O_EXCL, which fails on a dangling link or on anything made there in between.
# It opens anything else already there (a device, a FIFO, or a link to
# either), which only a guess at that random name could have put there.
probe_temp() { # <var> <prefix> [<dir>]
	local name
	name="$(temp_path "${3:-${TMPDIR:-/tmp}}" "$2")" || return 1
	BOUNDED_TEMPS+=("$name")
	(umask 077 && set -C && : >"$name") 2>/dev/null || return 1
	printf -v "$1" '%s' "$name"
}

# Set <var> to a new private folder (mode 0700) in <dir>, named before it is
# made, as probe_temp names a file, so the cleanup that removes <var>'s folder
# finds one a signal left. Made by mktemp -d and named after, one a Ctrl-C
# caught in between stayed. Status 1, <var> empty, when mkdir fails: it makes
# none where anything is already at the name, which is then never this run's.
temp_dir() { # <var> <dir> <prefix>
	local name
	name="$(temp_path "$2" "$3")" || return 1
	printf -v "$1" '%s' "$name"
	mkdir -m 700 "$name" || {
		printf -v "$1" '%s' ""
		return 1
	}
}

# Set TMP to the run's private download folder in TMPDIR, with <cleanup> as
# the EXIT trap that removes it. The trap is set before the folder is made,
# and the folder named before it exists (temp_dir): made by mktemp -d first,
# with the trap set after, one a Ctrl-C caught in between stayed.
make_download_dir() { # <cleanup>
	TMP=""
	trap "$1" EXIT
	temp_dir TMP "${TMPDIR:-/tmp}" aterm-install || {
		echo "install.sh: cannot create a private folder under ${TMPDIR:-/tmp}" >&2
		exit 1
	}
}

# The gate may only vouch for a bundle the fresh install would have accepted:
# same codesign seal, same Team-ID designated requirement, same Gatekeeper ask
# when a team is pinned. The pin here is the pre-flight one (ATERM_TEAM_ID, or
# the compiled-in official team for the official slug) — the manifest is not
# fetched yet, and for the official slug the manifest may not name any other
# team anyway (required_team_for). Matching version strings on an unverifiable
# bundle are not idempotence: a declined gate falls through to a full verified
# reinstall, which is the remedy, so this never refuses: it declines (1), or
# answers 124 when codesign did not answer and 125 when spctl did not.
installed_bundle_verified() { # <app-path> <team-or-empty>
	local app="$1" team="$2" s=0
	BOUNDED_OUT=/dev/null BOUNDED_ERR=/dev/null bounded "$VERIFY_WAIT_SECS" codesign --verify --deep --strict "$app" || s=$?
	[[ $s -eq 0 ]] || { [[ $s -eq 124 ]] && return 124; return 1; }
	[[ -n "$team" ]] || return 0
	[[ "$team" =~ ^[A-Za-z0-9]+$ ]] || return 1
	BOUNDED_OUT=/dev/null BOUNDED_ERR=/dev/null bounded "$VERIFY_WAIT_SECS" codesign --verify --deep --strict \
		-R "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$team\"" \
		"$app" || s=$?
	[[ $s -eq 0 ]] || { [[ $s -eq 124 ]] && return 124; return 1; }
	BOUNDED_OUT=/dev/null BOUNDED_ERR=/dev/null bounded "$VERIFY_WAIT_SECS" spctl -a -t exec "$app" || s=$?
	[[ $s -eq 0 ]] || { [[ $s -eq 124 ]] && return 125; return 1; }
}

# Free-space pre-flight: df's answer is advisory (a network mount can lie), so
# an unparseable answer never blocks; only a known shortfall does, before the
# first byte.
require_free_space() { # <dir> <bytes-needed> <what-for>
	local dir="$1" need="$2" what="$3" avail_kb need_kb
	avail_kb="$(df -Pk "$dir" 2>/dev/null | awk 'NR == 2 { print $4 }')"
	[[ "$avail_kb" =~ ^[0-9]+$ ]] || return 0
	need_kb=$((need / 1024))
	if [[ "$avail_kb" -lt "$need_kb" ]]; then
		echo "install.sh: not enough free space on the volume holding $dir for $what — need ~$((need / 1000000)) MB, have $((avail_kb / 1000)) MB" >&2
		return 1
	fi
}

# Whether `mkdir -p <dir> && [[ -w <dir> ]]` would succeed, without doing it:
# judged at the nearest existing path component, which must be a writable
# directory. A link that leads nowhere is such a component: `mkdir -p` cannot
# make a folder where it stands, nor below it. Only --dry-run consults this;
# the real pre-flight keeps `mkdir -p` itself as the probe.
would_create_writable() {
	local d="$1"
	while [[ -n "$d" && ! -e "$d" && ! -L "$d" ]]; do
		case "$d" in
		*/*) d="${d%/*}" ;;
		*) return 1 ;; # relative with no existing component: nothing to anchor to
		esac
	done
	[[ -n "$d" ]] || d=/
	[[ -d "$d" && -w "$d" ]]
}

# Destination pre-flight, shared by every half: create the dir(s) and prove them
# writable (`mkdir -p` succeeds on an existing unwritable dir). Under
# install-mode --dry-run nothing is created: would_create_writable gives the
# verdict, so a dry run reports the skips the real run would hit.
#
# What it makes, it makes writable by this user alone: the caller's umask with
# group and other write added. Ubuntu's user-private-group umask (002) would
# otherwise leave ~/.local, ~/.local/lib … 0775, a chain the Linux lane's own
# verifier (check_install_path) and the updater then judge as writable by
# others. Under a folder with a default ACL, Linux gives what mkdir makes the
# modes that ACL passes on, whatever the umask, so each folder it made then
# loses group and other write (chmod lowers the ACL's mask with them). Read
# bits stay the caller's, so a shared system store (ATERM_STORE_DIR=/opt/aterm)
# stays readable by the users who run it.
ensure_dirs_writable() { # <dir>...
	local d made=()
	if [[ "${DRY_RUN:-0}" -eq 1 ]]; then
		for d in "$@"; do
			would_create_writable "$d" || return 1
		done
		return 0
	fi
	for d in "$@"; do
		while [[ -n "$d" && ! -e "$d" && ! -L "$d" ]]; do
			made+=("$d")
			[[ "$d" == */* ]] || break
			d="${d%/*}"
		done
	done
	(umask "$(printf '%04o' $((8#$(umask) | 8#022)))" && mkdir -p "$@") 2>/dev/null || return 1
	[[ ${#made[@]} -eq 0 ]] || chmod -- go-w "${made[@]}" 2>/dev/null || return 1
	for d in "$@"; do
		[[ -w "$d" ]] || return 1
	done
}

# Six random bytes as hex. od reads exactly its byte count, so pipefail never
# sees a SIGPIPE here (a `head -c` over /dev/urandom would).
random_suffix() {
	local s
	s="$(od -An -N6 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')" || s=""
	[[ -n "$s" ]] || s="$$.$RANDOM"
	printf '%s' "$s"
}

# --- container election: which macOS release asset an install downloads -------
# Every release is one lean app. The lean container (`aterm-<v>-mac.zip`) is the
# default on every CPU: aterm opens immediately, and the ALab toolchain installs
# itself on first launch, per program, resumable, with live progress
# (docs/DESIGN-streaming-batteries-2026-08-23.md §7). The bare `aterm-<v>.dmg`
# is the SAME seed-stripped app in a disk image — the manifest's required
# `dmg` key still binds its name — and a current release never has it
# elected here. The batteries-included DMG pair (the bare DMG sealing the
# arm64 toolchain, `aterm-<v>-x86_64.dmg` the Intel one) is retired: no release
# cuts it. A release that names no zip in its manifest predates the lean
# container, so the default falls back to the bare DMG that release was cut
# for. Only a fork's such release installs: every official one (v0.6.0–v0.11.0)
# carries an empty team_id, which required_team_for refuses before the
# download, and with ATERM_TEAM_ID pinning the official team v0.11.0 was
# downloaded and refused at the signature check (measured 2026-10-20 in a
# macOS 27 guest).
#
# Pure decision logic, extracted from install_app so the whole election matrix
# is pinned by crates/aterm-update/tests/install_sh without a network. The
# election is CPU-independent (the universal app rides every container), so no
# hardware probe feeds it. Inputs are positional; outputs are globals:
#   CONTAINER_KIND      dmg | zip
#   ASSET_NAME/ASSET_SHA  the elected asset and its manifest digest
#   LEAN_REASON         "" | default | no-toolchain (which lean lane, if any)
#   TOOLCHAIN_DEFERRED  1 = the toolset half must not run synchronously here:
#                       first launch provisions it, and the GUI's own seed
#                       pass records adoption there — the audited consent
#                       seam (atpkg cmd_seed). This script never writes
#                       consent headlessly.
# Status 2 = the manifest cannot honestly serve the install (malformed
# identity fields). Every refusal names its cause; the caller aborts.
#
# The canonical-name bind stops a manifest naming some other asset in the
# release, and the 64-hex bind stops an empty or malformed field from turning
# the later digest comparison into a no-op. A pair travels together or not at
# all: proceeding on half a pair would be the one download whose digest check
# quietly degraded. The binds run whenever the fields are present, not only
# when their lane is elected: a malformed manifest is a loud abort on every
# CPU, and only absence is silent.
elect_container() { # <toolchain01> <version> <dmg> <dmg_sha> <zip> <zip_sha>
	local toolchain="$1" version="$2"
	local dmg_name="$3" dmg_sha="$4" zip_name="$5" zip_sha="$6"
	CONTAINER_KIND=dmg
	ASSET_NAME="$dmg_name"
	ASSET_SHA="$dmg_sha"
	LEAN_REASON=""
	TOOLCHAIN_DEFERRED=0

	if [[ -n "$zip_name" || -n "$zip_sha" ]]; then
		if [[ -z "$zip_name" || -z "$zip_sha" ]]; then
			echo "install.sh: manifest carries half a lean zip pair (zip/zip_sha256) — refusing" >&2
			return 2
		fi
		if [[ "$zip_name" != "aterm-$version-mac.zip" ]]; then
			echo "install.sh: manifest zip $zip_name is not canonical aterm-$version-mac.zip" >&2
			return 2
		fi
		if [[ ! "$zip_sha" =~ ^[0-9a-fA-F]{64}$ ]]; then
			echo "install.sh: manifest zip_sha256 is not exactly 64 hexadecimal digits" >&2
			return 2
		fi
	fi

	# The default lane: the lean zip on every CPU when the release ships one.
	if [[ -n "$zip_name" ]]; then
		CONTAINER_KIND=zip
		ASSET_NAME="$zip_name"
		ASSET_SHA="$zip_sha"
		if [[ "$toolchain" -eq 0 ]]; then
			# --no-toolchain: lean zip, and this run defers nothing — its own
			# toolset step is skipped, and persist_no_toolchain writes
			# `[packages] auto_install = false` so the app's first launch leaves
			# the toolset alone too.
			LEAN_REASON=no-toolchain
		else
			LEAN_REASON=default
			TOOLCHAIN_DEFERRED=1
		fi
		return 0
	fi

	# No zip in the manifest: a pre-lean-container release (reachable via an
	# explicit --version pin). Its bare DMG, elected above, is the one container
	# it has, and the universal app inside installs on either CPU. (Such a
	# release may still name the retired Intel DMG pair; those keys are not read.)
	return 0
}

# --- the toolset's deferral: a function of who holds this Mac's console -------
# docs/DESIGN-init-system-2026-10-20.md §4.2, "The SSH install". The lean default
# leaves the first seed to aterm's first launch, whose window shows the download
# per program and records adoption through the GUI's own seed pass — but only
# someone at this Mac's screen opens that window. When the installing user does
# not own /dev/console (an install over SSH while another login holds the screen;
# a box nobody is logged in to, where loginwindow's root owns it), no window
# follows, and a box only ever reached over SSH would never adopt the toolset. So
# that run seeds here, through install_toolchain: the same `aterm pkg seed` +
# `aterm pkg update` lane --no-app runs, under the same [packages] consent.
#
# Never as root: the toolset lives under the home of the account that uses it,
# and macOS's sudo keeps $HOME, so a root seed would leave root-owned stubs and
# a root-owned store in that person's tree, which their own passes then cannot
# write. The installing user is the one who typed `sudo` ($SUDO_UID, read only
# when this run is root): at the console, theirs is the ordinary deferral; anyone
# else's root run stays deferred and TOOLSET_AS_ROOT=1 names the command to run
# as that user. A root run over the home of the account that typed sudo never
# gets here: it ran again as that account (home_account).
#
# Pure, so crates/aterm-update/tests/install_sh pins it without a console:
# <console_uid> owns /dev/console (console_owner_uid), <euid> is this run's
# `id -u` and <sudo_uid> its $SUDO_UID. Over elect_container's outputs: a deferral
# (TOOLCHAIN_DEFERRED=1) stands for the console's owner and for root; for anyone
# else, or an owner that cannot be read, it is cleared and TOOLSET_NO_WINDOW=1
# says why the toolset installs here. A run that deferred nothing is left as it
# was.
toolset_deferral() { # <console_uid> <euid> <sudo_uid>
	TOOLSET_NO_WINDOW=0
	TOOLSET_AS_ROOT=0
	[[ "${TOOLCHAIN_DEFERRED:-0}" -eq 1 ]] || return 0
	local installing="$2"
	[[ "$2" == 0 && -n "$3" ]] && installing="$3"
	[[ -n "$1" && "$1" == "$installing" ]] && return 0
	if [[ "$2" == 0 ]]; then
		TOOLSET_AS_ROOT=1
		return 0
	fi
	TOOLCHAIN_DEFERRED=0
	TOOLSET_NO_WINDOW=1
}

# What a root run that left the toolset deferred says in its place.
ROOT_TOOLSET_LINE="not installed as root: it lives in the home of the account that uses it — run \`aterm pkg install --default-set\` as that user, or it installs on their first aterm launch at this Mac's console"

# The uid that owns /dev/console: the login at this Mac's screen, or root at the
# loginwindow; empty when it cannot be read. /usr/bin/stat by path, since a GNU
# stat first on PATH (Homebrew's coreutils) reads -f as --file-system.
# ATERM_INSTALL_CONSOLE_UID names it instead (an install_sh test's stand-in for
# another login at the screen); it moves only when the toolset installs, under
# the same consent.
console_owner_uid() {
	if [[ -n "${ATERM_INSTALL_CONSOLE_UID:-}" ]]; then
		printf '%s\n' "$ATERM_INSTALL_CONSOLE_UID"
		return 0
	fi
	/usr/bin/stat -f %u /dev/console 2>/dev/null || true
}

# --- opening the app the run installed ----------------------------------------
# The lean default's toolset installs at aterm's first launch, so a run that ends
# on a printed `open …` leaves the ALab tools waiting for a typed step. The run
# opens the app itself when a person at this Mac's screen ran it from a
# terminal: the installing account owns /dev/console (never root, whose `open`
# would start the app as root), stdout is a terminal (stdin is the pipe in
# `curl … | bash`), and no SSH session, CI or --no-open (NO_OPEN) says
# otherwise. Pure over its arguments and those four variables, so
# crates/aterm-update/tests/install_sh pins it without a console:
# <console_uid> as console_owner_uid reads it, <euid> as installer_uid, and
# <stdout_tty> 1 or 0.
app_opens_here() { # <console_uid> <euid> <stdout_tty>
	[[ "${NO_OPEN:-0}" -eq 0 && "$3" == 1 && "$2" != 0 && -n "$1" && "$1" == "$2" ]] &&
		[[ -z "${SSH_CONNECTION:-}" && -z "${SSH_TTY:-}" && -z "${CI:-}" ]]
}

# Opens <app> and says so in one line, naming the toolset when <toolset_coming>
# is 1 (the app's first launch installs it). An `open` that fails, or has not
# returned within VERIFY_WAIT_SECS (bounded: LaunchServices waits on the same
# code assessment codesign does), ends the run on the launch line it would
# otherwise have printed.
open_installed_app() { # <app> <toolset_coming>
	if BOUNDED_OUT=/dev/null BOUNDED_ERR=/dev/null bounded "$VERIFY_WAIT_SECS" open "$1"; then
		if [[ "$2" -eq 1 ]]; then
			echo "install.sh: aterm is opening; it installs the ALab tools now"
		else
			echo "install.sh: aterm is opening"
		fi
	else
		echo "install.sh: could not open aterm — launch:  open $(shell_word "$1")"
	fi
}

# The container election (elect_container) and the toolset's deferral over it
# (toolset_deferral), as one step: the plan and the install both run this, so
# a --dry-run's toolset line can never disagree with what the install does.
elect_install_lane() { # <toolchain01> <version> <dmg> <dmg_sha> <zip> <zip_sha>
	elect_container "$@" || return 1
	toolset_deferral "$(console_owner_uid)" "$(installer_uid)" "${SUDO_UID:-}"
}

# --- a root run over an account's home ---------------------------------------
#
# macOS's sudo keeps $HOME, so a root run would make that person's ~/.local,
# store, links, aterm.toml and toolset root's: their own aterm and atpkg could
# then not change them, and root would run what their account can replace
# (~/.local/bin/aterm, ~/.cargo/bin). So a root run over the home of the account
# that typed sudo, --uninstall's included, runs this file again as that account;
# one over any other account's home, or piped (no file to run again), says how
# to run it instead. A root run over a home root owns (its own, as under sudo -H,
# the way to install into folders only root can write; an MDM's) is unchanged,
# and so is one over a home whose owner is no account here, or that is outside
# its owner's own home (a CI container's HOME, owned by a host uid an image's
# account may share): no aterm of that owner's works in it to be stopped by it.
#
# Pure: <euid> is `id -u`, <sudo_uid> $SUDO_UID, <home_uid> the owner of $HOME,
# <home_account> 1 when HOME is in that account's own home (home_owner_named).
# HOME_ACCOUNT is the uid to run as (digits only: it rides `-u "#<uid>"`), or
# empty; HOME_REFUSED=1 when root must not act on this home at all.
home_account() { # <euid> <sudo_uid> <home_uid> <home_account>
	HOME_ACCOUNT=""
	HOME_REFUSED=0
	[[ "$1" == 0 && -n "$3" && "$3" != 0 ]] || return 0
	if [[ "$2" =~ ^[1-9][0-9]*$ && "$2" == "$3" ]]; then
		HOME_ACCOUNT="$2"
	elif [[ "$4" == 1 ]]; then
		HOME_REFUSED=1
	fi
}

# The uid that owns $HOME, empty when it cannot be read (`ls -n`: one spelling
# for BSD and GNU, where stat's flags differ), through a symlink, since the
# files land in the folder it names.
home_owner_uid() {
	ls_entry -ndL -- "$(home_anchor)" 2>/dev/null | awk '{print $3}' || true
}

# <path>, or when it does not exist yet the nearest folder above it that does
# (mkdir -p's anchor).
existing_anchor() { # <path>
	local p="$1"
	while [[ "$p" == */* && ! -e "$p" ]]; do
		p="${p%/*}"
	done
	echo "${p:-/}"
}

# HOME's existing_anchor: the files land under it, so a HOME still to be made
# in an account's own home is judged as that account's.
home_anchor() {
	existing_anchor "$HOME"
}

# `ls` by its fixed path, which PATH cannot shadow, else PATH's where there is
# none (NixOS, Guix).
ls_entry() {
	if [[ -x /bin/ls ]]; then /bin/ls "$@"; else command ls "$@"; fi
}

# The homes of the accounts whose uid is <uid>, one per line: getpwuid's, and
# every other record the account database lists with that uid (useradd -o; a
# local and a directory account sharing one), by uid, never by the name `ls`
# prints (BusyBox's cuts it to 8 characters) (a test replaces it). dscacheutil's
# `-a uid` query is getpwuid's one record, and its bare `-q user` the listing.
account_homes() { # <uid>
	if [[ -x /usr/bin/dscacheutil ]]; then
		{ /usr/bin/dscacheutil -q user -a uid "$1"; echo; /usr/bin/dscacheutil -q user; } 2>/dev/null |
			dscache_homes "$1" || true
	elif command -v getent >/dev/null 2>&1; then
		{ getent passwd "$1"; getent passwd; } 2>/dev/null | passwd_homes "$1" || true
	else
		passwd_homes "$1" </etc/passwd 2>/dev/null || true
	fi
}

# The home of each dscacheutil record on stdin (`name: …` lines, a blank line
# after each record) whose uid is <uid>. dscacheutil prints a uid signed
# (nobody's 4294967294 as -2), `ls -n` and `id -u` unsigned.
dscache_homes() { # <uid>
	awk -v u="$1" 'BEGIN { RS = "" } {
		uid = dir = ""
		n = split($0, l, "\n")
		for (i = 1; i <= n; i++) {
			if (l[i] ~ /^uid: /) uid = substr(l[i], 6)
			else if (l[i] ~ /^dir: /) dir = substr(l[i], 6)
		}
		if (uid == "" || dir == "") next
		if (uid + 0 < 0) uid += 4294967296
		if (uid + 0 == u + 0) print dir
	}'
}

# The home of each passwd(5) line on stdin whose uid is <uid>.
passwd_homes() { # <uid>
	awk -F: -v u="$1" '$3 == u { print $6 }'
}

# Whether folder <a> (a physical path) is folder <b> or inside it, by identity
# (-ef) at each level up, so another spelling of the same folder (a case variant
# on APFS, a leading //, a firmlink's /System/Volumes/Data path) is the same.
in_folder() { # <a> <b>
	local a="$1"
	while [[ -n "$a" ]]; do
		[[ "$a" -ef "$2" ]] && return 0
		[[ "$a" == */* ]] || return 1
		a="${a%/*}"
	done
	[[ / -ef "$2" ]]
}

# Whether HOME is in the home of an account here: `ls` prints its owner's name
# (for a uid no account here has, the bare number `ls -n` prints), and HOME
# (home_anchor's folder while it does not exist yet) is the own home of an
# account with that uid, or a folder in it. A uid an image's account happens to
# share (a CI container's HOME, owned by the host runner's uid) is not one. A
# named owner with no home found, or a HOME that cannot be entered, counts as
# one, so the run refuses.
home_owner_named() {
	local uid name here d found=0
	uid="$(home_owner_uid)"
	name="$(ls_entry -ldL -- "$(home_anchor)" 2>/dev/null | awk '{print $3}')" || return 1
	[[ -n "$name" && "$name" != "$uid" ]] || return 1
	here="$(cd -P -- "$(home_anchor)" 2>/dev/null && pwd)" || return 0
	while IFS= read -r d; do
		[[ -n "$d" ]] || continue
		found=1
		in_folder "$here" "$d" && return 0
	done <<<"$(account_homes "$uid")"
	[[ "$found" -eq 0 ]]
}

# The one call that runs as another account (a test replaces it): never
# returns on success, as the account's run is this run's exit. sudo keeps the
# variables <keep> names (comma-separated) in the environment, so their values
# stay out of the argv that ps and sudo's log show (a proxy URL can carry a
# password).
exec_as_account() { # <uid> <keep> <program> <arg>...
	local uid="$1" keep="$2"
	shift 2
	exec /usr/bin/sudo -n -H ${keep:+"--preserve-env=$keep"} -u "#$uid" -- "$@"
}

# Whether <uid> passes `test <op> <path>`, asked as that account, since root
# passes every one (a test replaces it).
account_test() { # <uid> <op> <path>
	/usr/bin/sudo -n -u "#$1" -- /bin/sh -c 'test "$1" "$2"' sh "$2" "$3" 2>/dev/null
}

# Run <program> as <user> (sudo's -u), as root runs a program another account
# can change: as nobody (a test replaces it).
run_as_user() { # <user> <program> <arg>...
	local user="$1"
	shift
	/usr/bin/sudo -n -u "$user" -- "$@"
}

# Run this install again as HOME_ACCOUNT with the same arguments, over the same
# HOME, with the root run's PATH and the settings it and its tools read (ATERM_*,
# XDG_*, CARGO_HOME, RUSTUP_HOME, curl's proxy and CA variables), which sudo -H
# would reset or drop. Or, before anything is written, say why not: no file to
# run again, a file that account cannot read, or an ATERM_*_DIR it cannot write
# (mkdir -p's anchor: the nearest folder that exists, the working directory for
# a relative path with none), which sudo -H fills as root instead.
run_as_home_account() { # <arg>...
	local owner="${HOME_ACCOUNT:-$(home_owner_uid)}" why script keep="" v d
	why="HOME ($HOME) is uid $owner's, and root would leave files there that its own aterm cannot change"
	if [[ "$HOME_REFUSED" -eq 1 ]] || ! self_on_disk ||
		! script="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)/${BASH_SOURCE[0]##*/}"; then
		echo "install.sh: not as root: $why — run it as that account, without sudo" >&2
		exit 1
	fi
	if ! account_test "$HOME_ACCOUNT" -r "$script"; then
		echo "install.sh: not as root: $why, and uid $HOME_ACCOUNT cannot read $script — run a copy it can read as that account, without sudo" >&2
		exit 1
	fi
	for v in ATERM_INSTALL_DIR ATERM_BIN_DIR ATERM_STORE_DIR ATERM_MAN_DIR; do
		[[ -n "${!v:-}" ]] || continue
		d="${!v}"
		while [[ ! -e "$d" ]]; do
			case "$d" in
			*/*) d="${d%/*}" ;;
			*) d=. ;;
			esac
			[[ -n "$d" ]] || d=/
		done
		if ! account_test "$HOME_ACCOUNT" -w "$d"; then
			echo "install.sh: not as root: $why, and uid $HOME_ACCOUNT cannot write $v (${!v}) — run it with sudo -H to install there as root, or name a folder that account can write" >&2
			exit 1
		fi
	done
	echo "install.sh: running as uid $HOME_ACCOUNT, not root: $why"
	for v in $(compgen -e); do
		case "$v" in
		ATERM_* | XDG_* | CARGO_HOME | RUSTUP_HOME | CURL_CA_BUNDLE | CURL_HOME | SSL_CERT_FILE | SSL_CERT_DIR | \
			http_proxy | https_proxy | HTTPS_PROXY | all_proxy | ALL_PROXY | no_proxy | NO_PROXY)
			keep="${keep:+$keep,}$v"
			;;
		esac
	done
	exec_as_account "$HOME_ACCOUNT" "$keep" /usr/bin/env "HOME=$HOME" "PATH=${ROOT_LOOKUP_PATH-$PATH}" /bin/bash "$script" "$@"
	exit $?
}

# Whether this account owns <path> or can write it (a test replaces it).
mine() { [[ -O "$1" || -w "$1" ]]; }

# FOREIGN_FOLDERS: the folders aterm writes below HOME (the link and store, the
# man page and, on Linux, the icons, which shell-setup rewrites in place, the
# desktop entry, the completions, aterm.toml, atpkg's hooks, prefix, state and
# logs)
# that exist and are not this account's, each the outermost such on its way
# down from HOME, once, and inside aterm's own trees (where every entry is the
# account's) each outermost entry another uid owns. An install under sudo
# before a root run handed itself to the account left them root's, and nothing
# this run does can change what is in them. Read only. None for root, which
# changes anything (a root run reaches here over its own home, or one no
# account here owns or that is outside its owner's own home), nor over a HOME
# this account does not own (sudo -u keeps the invoker's; a shared /tmp; an
# empty HOME or `/`): its folders are their owner's, not a sudo install's. The
# walk stays inside HOME as written: a path with a `..` component names
# nothing, and a symlink on the way ends HOME's own folders on that path
# (chown -R would change only the link).
foreign_home_folders() {
	local p f cfg="${XDG_CONFIG_HOME:-$HOME/.config}" data="${XDG_DATA_HOME:-$HOME/.local/share}"
	local -a trees
	FOREIGN_FOLDERS=()
	[[ "$(installer_uid)" != 0 && -O "$HOME" ]] || return 0
	local -a desktop=()
	if [[ "$(uname -s)" == Darwin ]]; then
		trees=("$HOME/Library/Application Support/aterm" "$HOME/Library/Logs/aterm")
	else
		trees=("$data/aterm" "$HOME/.local/share/aterm/pkg" "${XDG_STATE_HOME:-$HOME/.local/state}/aterm")
		desktop=("$data/applications/aterm.desktop" "$data"/icons/hicolor/*/apps/aterm.png)
	fi
	trees+=("$STORE_DIR" "$cfg/aterm" "$HOME/.aterm")
	for p in "$BIN_DIR" "${ATERM_MAN_DIR:-$HOME/.local/share/man}/man1/aterm.1" "$data/zsh/site-functions" \
		"$data/bash-completion/completions" "$cfg/fish/completions" ${desktop[@]+"${desktop[@]}"} "${trees[@]}"; do
		home_folder_top "$p" || continue
		[[ -z "$FOLDER_TOP" ]] || foreign_add "$FOLDER_TOP"
	done
	for p in "${trees[@]}"; do
		home_folder_top "$p" && [[ -z "$FOLDER_TOP" && "$FOLDER_LINKED" -eq 0 && -d "$p" ]] || continue
		while IFS= read -r f; do
			[[ -z "$f" ]] || foreign_add "$f"
		done <<<"$(foreign_inside "$p")"
	done
}

# FOLDER_TOP: the outermost folder on the way down from HOME to <path> that
# exists and is not this account's, or empty; a symlink on the way ends HOME's
# own folders below it, and FOLDER_LINKED=1 says one was there. Status 1 when
# <path> is not below HOME as written or has a `..` component.
home_folder_top() { # <path>
	local d="$1"
	FOLDER_TOP=""
	FOLDER_LINKED=0
	case "$1/" in */../*) return 1 ;; esac
	while [[ "$d" == "$HOME"/* ]]; do
		if [[ -L "$d" ]]; then
			FOLDER_TOP=""
			FOLDER_LINKED=1
		elif [[ -e "$d" ]] && ! mine "$d"; then
			FOLDER_TOP="$d"
		fi
		d="${d%/*}"
	done
	[[ "$1" == "$HOME"/* ]]
}

# The entries below <dir>, to atpkg's store/<pkg>/<build>, that another uid
# owns, each the outermost such, one per line; links aside, since the account
# can replace a link in a folder of its own, and nothing at or below a name
# with a newline, which would read as two paths (a test replaces it).
foreign_inside() { # <dir>
	find "$1" -mindepth 1 -maxdepth 4 \( -name "*"$'\n'"*" -prune \) -o \
		\( ! -type l ! -user "$(installer_uid)" -prune -print \) 2>/dev/null || true
}

# Add <path> to FOREIGN_FOLDERS unless one there holds it, and drop each one
# it holds.
foreign_add() { # <path>
	local f
	local -a kept=()
	for f in ${FOREIGN_FOLDERS[@]+"${FOREIGN_FOLDERS[@]}"}; do
		[[ "$1" != "$f" && "$1" != "$f"/* ]] || return 0
		[[ "$f" == "$1"/* ]] || kept+=("$f")
	done
	FOREIGN_FOLDERS=(${kept[@]+"${kept[@]}"} "$1")
}

# The NOTE that names FOREIGN_FOLDERS and the one command that gives them back,
# before any half tries to write into them; nothing when there are none.
foreign_home_note() {
	local list="" args="" it=them f
	[[ ${#FOREIGN_FOLDERS[@]} -gt 0 ]] || return 0
	[[ ${#FOREIGN_FOLDERS[@]} -gt 1 ]] || it=it
	for f in "${FOREIGN_FOLDERS[@]}"; do
		list+="${list:+, }$f"
		args+=" $(shell_word "$f")"
	done
	echo "install.sh: NOTE: not yours, so aterm cannot change what is in $it: $list (an install under sudo makes such folders root's) — give $it back with: sudo chown -R $(shell_word "$(id -un 2>/dev/null || id -u)")$args" >&2
}

# <word> spelled so bash, zsh and fish (the shells aterm wires) all read it back
# as one word: bare when it is plain, else in single quotes, which fish reads as
# the others do unless the word holds a quote, a backslash or a control
# character; such a word gets printf %q's spelling, which bash and zsh read.
shell_word() { # <word>
	case "$1" in
	*[!A-Za-z0-9_@%+=:,./-]* | "")
		case "$1" in
		*[\'\\]* | *[[:cntrl:]]*) printf '%q' "$1" ;;
		*) printf "'%s'" "$1" ;;
		esac
		;;
	*) printf '%s' "$1" ;;
	esac
}

# The way out for a folder ensure_dirs_writable refused: the NOTE's when one
# of FOREIGN_FOLDERS holds it, else the setting that moves it.
unwritable_remedy() { # <dir> <setting>
	local f
	for f in ${FOREIGN_FOLDERS[@]+"${FOREIGN_FOLDERS[@]}"}; do
		if [[ "$1" == "$f" || "$1" == "$f"/* ]]; then
			echo "$f is not yours: the NOTE above gives it back"
			return 0
		fi
	done
	echo "set $2 to a writable dir"
}

# The build's target dir and cargo's home, when an install under sudo left
# what the build writes there root's (or any other uid's), fail the build
# minutes in on a permission error with no way out named: the CLI pre-flight
# says so instead (CARGO_RESOLVE_REASON, status 1), with the chown that gives
# them back. Only what the build writes is asked: target/ itself and
# release/ in it (cargo_build_pinned runs the store's targo by its physical
# path, or rustup's proxy, never atpkg's targo shim, which would put a
# build.noindex/ beside them); cargo's home itself and the folders new
# downloads land in: registry/, its cache/, src/ and index/ and the per-index
# folder in each, and, when Cargo.lock names a git source or there is no
# Cargo.lock to say, git/, its db/ and checkouts/ and the folders in those. A
# crate or index file root's in a folder of the account's fails nothing, and
# cargo treats a lock or cache file it cannot write (.package-cache,
# .global-cache) as a cache to read. A folder is named by its physical path,
# since `chown -R` on a link changes only the link; a folder whose parent is
# not this account's is never named (it is someone else's to give). Under
# root the question is another one (root_build_preflight).
source_build_preflight() {
	local d p me stale="" f
	me="$(installer_uid)"
	if [[ "$me" == 0 ]]; then
		root_build_preflight
		return
	fi
	for d in "$ROOT/target" "${CARGO_HOME:-$HOME/.cargo}"; do
		p="$(cd -P -- "$d" 2>/dev/null && pwd -P)" || continue
		[[ -n "$p" && -O "${p%/*}" ]] || continue
		if [[ "$d" == "$ROOT/target" ]]; then
			f="$(not_owned_in "$me" "$p" release)"
		else
			set -- registry registry/cache registry/src registry/index
			[[ -f "$ROOT/Cargo.lock" ]] && ! grep -q '^source = "git+' "$ROOT/Cargo.lock" 2>/dev/null ||
				set -- "$@" git git/db git/checkouts
			f="$(folders_not_owned_in "$me" "$p" "$@")"
		fi
		[[ -n "$f" ]] || continue
		stale+=" $(shell_word "$p")"
	done
	[[ -n "$stale" ]] || return 0
	CARGO_RESOLVE_REASON="building from source writes in$stale, and an install under sudo left some of what is there root's, which cargo cannot change — give it back with: sudo chown -R $(shell_word "$(id -un 2>/dev/null || id -u)")$stale"
	return 1
}

# The first entry not <uid>'s (a link aside) of <dir> itself and of each
# <sub> under it with everything below that; nothing when there is none.
not_owned_in() { # <uid> <dir> <sub>...
	local uid="$1" dir="$2" sub
	shift 2
	{
		find "$dir" -maxdepth 0 ! -user "$uid" -print
		for sub in "$@"; do
			[[ -e "$dir/$sub" ]] || continue
			find "$dir/$sub" \( -name "*"$'\n'"*" -prune \) -o \( ! -type l ! -user "$uid" -print \)
		done
	} 2>/dev/null | head -n 1
}

# The first folder not <uid>'s of <dir> itself, of each <sub> under it and of
# the folders directly in each <sub>; nothing when there is none.
folders_not_owned_in() { # <uid> <dir> <sub>...
	local uid="$1" dir="$2" sub
	shift 2
	{
		find "$dir" -maxdepth 0 ! -user "$uid" -print
		for sub in "$@"; do
			[[ -e "$dir/$sub" ]] || continue
			find "$dir/$sub" -maxdepth 1 \( -name "*"$'\n'"*" -prune \) -o \( -type d ! -user "$uid" -print \)
		done
	} 2>/dev/null | head -n 1
}

# Under root, a source build runs what it builds: the checkout's build
# scripts and proc macros and every crate it compiles, what target/ already
# holds that cargo may reuse, git over the checkout's git dir (the version
# stamp, aterm's build script), the config cargo reads in a .cargo/ in each
# folder from / down to the checkout and in cargo's own home ([build] rustc,
# a linker, a rustc-wrapper), the programs it finds on PATH, and those its
# environment names. So it happens only when no other account can change what
# it reads or where it finds programs: the checkout, every .cargo/ above it,
# the git dir and common dir a .git file names, and cargo's home (while it
# does not exist, the folder it would be made in) pass root_tree_failing (a
# walk that ends without an answer refuses, naming the path it walked); on
# macOS, the developer folder and SDK that /usr/bin's cc and git run from pass
# root_xcode_judged; the folder of the lane's cargo, which the build puts
# first on PATH, passes root_bin_dir (root_build_path keeps the rest of PATH
# to such folders, and ROOT_GIT_ENV keeps git, from 2.32 on, to the
# checkout's own config). Of the
# programs root's own environment names, those that run in place of the
# compiler the toolchain refusal judges are asked too: each rustc wrapper,
# and on rustup's lane, which does not scrub them, RUSTC and
# CARGO_BUILD_RUSTC, is a program may_run allows, a bare name looked up as
# the build looks it up (the lane's folders, then root_build_path's). A CC,
# CXX or AR, a linker the environment names, and any program a config of
# root's names are root's word and are not asked. Else CARGO_RESOLVE_REASON
# names the first that fails, and status 1.
root_build_preflight() {
	local here d v w o="" home="${CARGO_HOME:-$HOME/.cargo}"
	local -a vars=(RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER)
	here="$(cd -P -- "$ROOT" 2>/dev/null && pwd)" || here=""
	o="$(root_tree_failing "$ROOT")" || o="${o:-$ROOT}"
	if [[ -z "$o" && -f "$here/.git" ]]; then
		d="$(sed -n 's/^gitdir: //p' "$here/.git" 2>/dev/null | head -n 1)"
		[[ "$d" == /* ]] || d="$here/$d"
		o="$(root_tree_failing "$d")" || o="${o:-$d}"
		if [[ -z "$o" && -f "$d/commondir" ]]; then
			v="$(head -n 1 "$d/commondir" 2>/dev/null)"
			[[ "$v" == /* ]] || v="$d/$v"
			o="$(root_tree_failing "$v")" || o="${o:-$v}"
		fi
	fi
	d="$here"
	while [[ -z "$o" && -n "$here" ]]; do
		if [[ -e "$d/.cargo" || -L "$d/.cargo" ]]; then
			o="$(root_tree_failing "$d/.cargo")" || o="${o:-$d/.cargo}"
		fi
		[[ -n "$d" && "$d" != / ]] || break
		d="${d%/*}"
	done
	[[ -z "$o" ]] || { root_build_refusal "$o" checkout; return 1; }
	[[ "$home" == /* ]] || home="$here/$home"
	o="$(root_tree_failing "$home")" || o="${o:-$home}"
	[[ -z "$o" ]] || { root_build_refusal "$o" "cargo home"; return 1; }
	if [[ "$(uname -s)" == Darwin ]]; then
		root_xcode_judged || return 1
	fi
	d="$(dirname "$CARGO_BIN")"
	if ! root_bin_dir "$d"; then
		CARGO_RESOLVE_REASON="root does not run a toolchain another account can change, and $d, which the build puts first on PATH, holds a program another account can change — run install.sh as the account whose toolchain it is, without sudo"
		return 1
	fi
	[[ "${CARGO_LANE:-}" == store ]] || vars+=(RUSTC CARGO_BUILD_RUSTC)
	for v in "${vars[@]}"; do
		w="${!v:-}"
		[[ -n "$w" ]] || continue
		case "$w" in
		/*) ;;
		*/*) w="$here/$w" ;;
		*)
			d="$(root_build_path)"
			d="$(build_path_lead)${d:+:$d}"
			w="$(PATH="$d" && type -P -- "$w")" || w="${!v}"
			;;
		esac
		root_may_run_toolchain "$w" || return 1
	done
}

# On macOS, /usr/bin's cc, ld, ar, as, git and the rest of Apple's developer
# programs are one xcrun, which runs the program of that name from the active
# developer folder or from its SDK: the folder DEVELOPER_DIR names (for an
# Xcode.app, its Contents/Developer), else the first of xcode_dirs that is
# there (the link `xcode-select -s` makes, Xcode's, the Command Line Tools'),
# and the SDK SDKROOT names, else that folder's macOS one (an SDKROOT that
# names an SDK as xcrun can, `macosx`, names no folder to judge, and refuses).
# With no SDK named,
# xcrun also hands the compiler /usr/local/include and the linker
# /usr/local/lib, which Homebrew under /usr/local gives an admin. So a root
# build runs them only when the way to each folder, and everything in it (for
# a developer folder that is a bundle's Contents/Developer, as an Xcode.app's
# is, in the bundle, whatever its name, since its tools load its
# SharedFrameworks too), are root's alone (root_entries_offender; their links
# root made, and lead where root chose, as a program a config of root's names
# does): ROOT_DEVELOPER_DIR
# and ROOT_SDKROOT are then where each ends, which the build pins
# (root_build_env). Else CARGO_RESOLVE_REASON names what fails, and status 1.
root_xcode_judged() {
	local dev sdk d e="" o r walked="" what=developer how
	dev="${DEVELOPER_DIR:-}"
	if [[ -z "$dev" ]]; then
		while IFS= read -r d; do
			dev="$d"
			[[ ! -e "$d" ]] || break
		done <<<"$(xcode_dirs)"
	fi
	[[ ! -d "$dev/Contents/Developer" ]] || dev="$dev/Contents/Developer"
	sdk="${SDKROOT:-}"
	if [[ -n "$sdk" && "$sdk" != */* ]]; then
		CARGO_RESOLVE_REASON="root runs no compiler another account can change, and SDKROOT=$sdk names an SDK, not its folder, so root cannot judge it — set SDKROOT to the SDK's folder (xcrun --show-sdk-path prints it) or unset it, or run install.sh as your account, without sudo"
		return 1
	fi
	if [[ -z "$sdk" ]]; then
		sdk="$dev/SDKs/MacOSX.sdk"
		[[ -e "$sdk" ]] || sdk="$dev/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"
	fi
	ROOT_DEVELOPER_DIR="" ROOT_SDKROOT=""
	for d in "$dev" "$sdk"; do
		o="$(folder_failing sealed_entry "$d")"
		[[ -n "$o" ]] || e="$(cd -P -- "$d" 2>/dev/null && pwd)" || o="$d"
		r="$e"
		[[ "$r" != */Contents/Developer ]] || r="${r%/Contents/Developer}"
		if [[ -z "$o" ]] && ! { [[ -n "$walked" ]] && in_folder "$e" "$walked"; }; then
			o="$(root_entries_offender "$r")"
			walked="$r"
		fi
		if [[ -n "$o" ]]; then
			if [[ "$what" == SDK ]]; then
				how="/usr/bin's cc and ld build against $d, the SDK"
			else
				how="/usr/bin's cc, ld and git run from $d, the developer folder"
			fi
			if [[ "$what" == SDK && -n "${SDKROOT:-}" ]]; then
				how="$how SDKROOT names — unset SDKROOT, or set it to one only root can change"
			elif [[ -n "${DEVELOPER_DIR:-}" ]]; then
				[[ "$what" != SDK ]] || how="$how of the developer folder"
				how="$how DEVELOPER_DIR names — unset DEVELOPER_DIR, or set it to one only root can change"
			else
				how="$how — for this run, put DEVELOPER_DIR=/Library/Developer/CommandLineTools after sudo; for every account, sudo xcode-select -s /Library/Developer/CommandLineTools (either once the Command Line Tools are installed)"
			fi
			CARGO_RESOLVE_REASON="root runs no compiler another account can change, and $o is such (it is not there, or not root's alone): $how, or run install.sh as your account, without sudo"
			ROOT_DEVELOPER_DIR=""
			return 1
		fi
		if [[ -z "$ROOT_DEVELOPER_DIR" ]]; then
			ROOT_DEVELOPER_DIR="$e" what=SDK
		else
			ROOT_SDKROOT="$e"
		fi
	done
}

# The developer folders xcrun takes when DEVELOPER_DIR names none, in its
# order (a test replaces it).
xcode_dirs() {
	printf '%s\n' /var/db/xcode_select_link /Applications/Xcode.app/Contents/Developer \
		/Library/Developer/CommandLineTools
}

# The environment a root build and its git run in beside PATH, as env's
# operands (ROOT_BUILD_ENV): ROOT_GIT_ENV's, and on macOS the developer folder
# and SDK root_xcode_judged found root's, pinned, with TOOLCHAINS, which picks
# xcrun another toolchain, unset. Status 1 when there are none to pin.
root_build_env() {
	ROOT_BUILD_ENV=("${ROOT_GIT_ENV[@]}")
	[[ "$(uname -s)" == Darwin ]] || return 0
	[[ -n "${ROOT_DEVELOPER_DIR:-}" ]] || root_xcode_judged || return 1
	ROOT_BUILD_ENV=(-u TOOLCHAINS "${ROOT_GIT_ENV[@]}" "DEVELOPER_DIR=$ROOT_DEVELOPER_DIR" "SDKROOT=$ROOT_SDKROOT")
}

# root_build_preflight's refusal of <path>, part of what the build reads that
# is <whose> (the checkout, or cargo's home).
root_build_refusal() { # <path> <whose>
	CARGO_RESOLVE_REASON="root builds nothing another account can change, and $1 is such (it, or a folder on its way from /, is not root's alone, or it is a link that leads out of the folder root checked) — run install.sh as the account whose $2 it is, without sudo"
}

# --- the cargo that honours the pin (source-build lane) ----------------------
#
# Two lanes, the aterm-managed one first. A machine provisioned the product's
# way (`aterm pkg install trust`) has no rustup: the atpkg store holds the Trust
# toolchain at <prefix>/store/trust/<build>/bin/, and that targo is channel
# `trust`, the pin rust-toolchain.toml names, so it honours the pin by
# construction (find_managed_targo; it builds on targo's explicit `--unverified`
# lane). A stock pin never takes that lane. When no managed store applies,
# rustup's lane runs.
#
# The rustup lane. Of the cargos rustup knows, only its proxy honours
# rust-toolchain.toml: a plain cargo (Homebrew's `rust`, a distro package)
# compiles with PATH's rustc. On a Mac the proxy is the one that loses:
# path_helper puts the system dirs first in every login shell's inherited PATH,
# so a nested shell finds /usr/local/bin/cargo before ~/.cargo/bin/cargo, and a
# shell with ~/.cargo/bin off PATH has no cargo at all. So rustup is found first
# ($CARGO_HOME/bin, then PATH) and the cargo is taken from beside it: rustup
# lays its proxies beside its own binary wherever that is (rustup-init's
# ~/.cargo/bin or a custom $CARGO_HOME/bin, Homebrew's keg-only `rustup`, a
# distro's /usr/bin). Then the result is proved to dispatch to the pin:
#   * rustup must resolve this root to the file's pin: `show active-toolchain`
#     names the file it read, and it must be this checkout's
#     rust-toolchain.toml, compared by inode (-ef), never by string (rustup
#     spells the path as getcwd() does, while bash's builtin `pwd -P` keeps the
#     case as typed, and a case-insensitive volume lets any spelling reach the
#     checkout). An ambient RUSTUP_TOOLCHAIN steers rustup and the proxy alike,
#     so it is scrubbed (env -u) from every probe and from the build. What
#     survives the scrub is a `rustup override` on this very directory, refused
#     by name; a parent directory's override never outranks the checkout's own
#     file;
#   * the cargo's `--version` must equal that of the toolchain cargo rustup
#     names for this root (the proxy execs exactly that binary), so a cargo of
#     a different build cannot match; a plain cargo of the same release can,
#     and would build with PATH's rustc;
#   * it must refuse RUSTUP_TOOLCHAIN=<absent name>, which only the proxy reads;
#   * the rustc the build runs must be rustup's proxy too. cargo (RUSTC and
#     build.rustc unset) runs PATH's `rustc`, and rustup adds $CARGO_HOME/bin
#     to the front of the PATH it hands the toolchain's cargo only when PATH
#     lacks it, never the dir the proxy was invoked from. So the build puts
#     $CARGO_HOME/bin and then the proved cargo's own dir first on PATH, and the
#     first rustc there must refuse an absent RUSTUP_TOOLCHAIN as the cargo did;
#     rustup exports RUSTUP_TOOLCHAIN=<pin> to the cargo it runs, so that proxy
#     resolves to the pin.
#
# FAILSAFE POLICY applies: every outcome here is a pre-flight skip reason,
# never a mid-build abort. crates/aterm-update/tests/install_sh pins their behaviour.
#
# RUSTUP_AUTO_INSTALL=0 on every probe: rustup 1.29.0 otherwise treats a
# pinned-but-absent toolchain as an order to DOWNLOAD it (`rustup which cargo`
# began "syncing channel updates", measured 2026-09-06), and a pre-flight —
# read-only under --dry-run by contract — must never start a multi-hundred-MB
# fetch.
#
# Read-only: no file is created, so the --dry-run zero-mutation contract holds.
# One file's physical path: symlinks followed hop by hop with readlink (the
# installer's floor is macOS's stock bash 3.2, and `realpath` is not on every
# Mac), the directory spelled as getcwd() spells it. Empty, exit 1, when the
# file does not exist.
gate_physical_path() { # <path>
	local p="$1" t
	while [[ -L "$p" ]]; do
		t="$(readlink "$p")" || return 1
		case "$t" in
		/*) p="$t" ;;
		*) p="$(dirname "$p")/$t" ;;
		esac
	done
	[[ -e "$p" ]] || return 1
	printf '%s/%s\n' "$(cd "$(dirname "$p")" && env pwd -P)" "$(basename "$p")"
}

# The channel rust-toolchain.toml pins: its first uncommented `channel = "…"`.
# Read from the file, never from rustup: the managed lane has no rustup to ask,
# and a stock pin must never take that lane.
gate_pinned_channel() { # <root>
	sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$1/rust-toolchain.toml" 2>/dev/null | head -n 1
}

# The managed-store lane. atpkg holds the Trust toolchain at
# <prefix>/store/trust/<build>/bin/ (targo, trustc, siblings),
# <prefix>/store/trust/current selects the build, and <prefix>/bin/targo is the
# shim on PATH (its last line `exec '<prefix>/store/trust/<build>/bin/targo'
# "$@"`), with `alab-targo` beside it: atpkg's alias shim, laid so the managed
# copy can be named when a foreign `targo` sits ahead of the managed bin/ on
# PATH (crates/atpkg/src/store.rs ALIAS_PREFIX). That targo runs the store's
# trustc by absolute path, so PATH cannot steer the compiler (an ambient RUSTC
# or CARGO_BUILD_RUSTC can, and cargo_build_pinned scrubs both). The store is
# resolved three ways, in order:
#   1. `aterm pkg which targo` when `aterm` is in reach (and, under root, is
#      one may_run allows): its last `→` arm, up to the ` — ` note. A targo
#      ahead of the managed bin/ on PATH makes atpkg answer the SHADOWED shape
#      (`targo → <stray> — managed <build> — SHADOWED by <stray> — type
#      alab-targo for the managed one`), so a SHADOWED answer is re-asked as
#      `alab-targo`; an alias that is not laid falls through.
#   2. PATH, scanned dir by dir for a `targo` or `alab-targo` — a shim followed
#      to its exec target — taking the first that is the store's: the shape
#      proof below admits a candidate, never its PATH rank. When none is the
#      store's, the first targo seen is what the decline names.
#   3. (the same scan) PATH's `targo` itself, symlinks resolved — a store bin
#      dir on PATH, or one reached through `current`.
# Whatever answered, the physical path must lie at
# <prefix>/store/trust/<build>/bin/targo; anything else never takes this lane.
# The lane needs the checkout root (its pin proof reads the file), and the pin
# must name `trust`. Anything short of that is "no managed store": the reason
# is kept in MANAGED_DECLINE for the refusal, and the rustup lane runs.

# The exec target of an atpkg launcher shim, else the file itself. A shim is a
# `#!/bin/sh` file of at most GATE_SHIM_MAX_BYTES whose last command (blank and
# comment lines aside) is `exec '<absolute path>' "$@"`, on a line no earlier
# line continues into (a trailing `\`, `|` or `&&`), in a file with no
# here-document and no other `exec`: the word anywhere on a command line before
# it refuses the file. Most shims are that one command (the `alab-<tool>` alias
# has the same body); on macOS atpkg's `targo` and `targo-tippy` shims first run
# a block of shell builtins that may put `--config build.build-dir=…` in front
# of "$@", or export CARGO_BUILD_BUILD_DIR for `targo trust`
# (crates/atpkg/src/build_cache.rs), so their exec is the last of about a
# hundred and twenty lines. Following such a shim is sound: it is read here, never run,
# only to LOCATE the store file, which gate_store_targo must then prove and every
# build runs by its physical path; a prelude runs only when the shim itself is
# executed, so it cannot change what is selected. A second `exec` (atpkg's
# compat route guard among them: that shim may run another file than the one
# its last line names) or any shape not recognised here is not followed. A
# Mach-O targo on PATH is not read past its magic (`head -c 2`).
GATE_SHIM_MAX_BYTES=16384
gate_shim_target() { # <file>
	local line="" cmd="" last="" prev="" target="" size="" first=1 bad=""
	local word='(^|[^[:alnum:]_])exec([^[:alnum:]_]|$)'
	if [[ -f "$1" && "$(head -c 2 "$1" 2>/dev/null)" == '#!' ]]; then
		size="$(wc -c <"$1" 2>/dev/null)" || size=""
		size="${size//[!0-9]/}"
		[[ -n "$size" && "$size" -le "$GATE_SHIM_MAX_BYTES" ]] || bad=1
		while [[ -z "$bad" ]] && { IFS= read -r line || [[ -n "$line" ]]; }; do
			if [[ -n "$first" ]]; then
				first=""
				[[ "$line" == '#!/bin/sh' ]] || bad=1
				continue
			fi
			cmd="${line#"${line%%[![:space:]]*}"}"
			cmd="${cmd%"${cmd##*[![:space:]]}"}"
			[[ -z "$cmd" || "$cmd" == '#'* ]] && continue
			# The previous command line was not the last one: it is prelude.
			if [[ -n "$last" ]]; then
				if [[ "$last" =~ $word || "$last" == *'<<'* ]]; then
					bad=1
				fi
				prev="$last"
			fi
			last="$cmd"
		done <"$1"
		case "$prev" in *'\' | *'|' | *'&&') bad=1 ;; esac
		case "$last" in
		"exec '"*"' \"\$@\"")
			target="${last#exec \'}"
			target="${target%\' \"\$@\"}"
			;;
		esac
		[[ -z "$bad" && "$target" == /* && "$target" != *"'"* ]] || target=""
	fi
	printf '%s\n' "${target:-$1}"
}

# The shape proof: <candidate> resolves, physically, to
# <prefix>/store/trust/<build>/bin/targo and is executable. Sets
# MANAGED_TARGO/MANAGED_PREFIX/MANAGED_BUILD, or MANAGED_DECLINE with the reason.
gate_store_targo() { # <candidate>
	local p="" prefix="" build=""
	MANAGED_DECLINE=""
	if ! p="$(gate_physical_path "$1")" || [[ -z "$p" ]]; then
		MANAGED_DECLINE="the targo in reach, $1, does not exist"
		return 1
	fi
	case "$p" in
	*/store/trust/*/bin/targo)
		prefix="${p%/store/trust/*}"
		build="${p#"$prefix"/store/trust/}"
		build="${build%/bin/targo}"
		;;
	esac
	if [[ -z "$prefix" || -z "$build" || "$build" == */* ]]; then
		MANAGED_DECLINE="the targo in reach, $p, is not in an aterm-managed store (<prefix>/store/trust/<build>/bin/targo), so it is not the toolchain \`aterm pkg install trust\` lays"
		return 1
	fi
	if [[ ! -x "$p" ]]; then
		MANAGED_DECLINE="the store's targo at $p is not executable (aterm pkg doctor)"
		return 1
	fi
	MANAGED_TARGO="$p"
	MANAGED_PREFIX="$prefix"
	MANAGED_BUILD="$build"
	return 0
}

find_managed_targo() { # <root>
	local root="$1" aterm="" out="" cand="" first="" pin="" d="" name="" f="" c="" dirs
	MANAGED_TARGO=""
	MANAGED_PREFIX=""
	MANAGED_BUILD=""
	MANAGED_DECLINE=""
	MANAGED_SEEN=""
	MANAGED_ROOT_REFUSED=""
	pin="$(gate_pinned_channel "$root")"
	if [[ "$pin" != trust ]]; then
		MANAGED_DECLINE="rust-toolchain.toml pins '${pin:-nothing}', not 'trust', so the aterm-managed Trust store is not this checkout's toolchain"
		return 1
	fi
	if aterm="$(lookup_program aterm 2>/dev/null)" && [[ -n "$aterm" ]] && ! may_run "$aterm"; then
		# Root does not ask it; the PATH walk below only reads files. Kept for
		# the refusal when nothing else is in reach (find_cargo_and_rustup).
		MANAGED_ROOT_REFUSED="$aterm"
	elif [[ -n "$aterm" ]]; then
		out="$("$aterm" pkg which targo 2>/dev/null | head -n 1 || true)"
		case "$out" in
		*" — SHADOWED by "*)
			# A foreign targo ahead of the managed bin/ on PATH: the alias
			# names the managed copy (route 1 above).
			out="$("$aterm" pkg which alab-targo 2>/dev/null | head -n 1 || true)"
			;;
		esac
		case "$out" in
		*" → "*)
			cand="${out##* → }"
			cand="${cand%% — *}"
			;;
		esac
	fi
	if [[ -z "$cand" ]]; then
		# Routes 2 and 3: PATH, dir by dir, the first targo (or alias) that
		# proves to be the store's; else the first seen, for the decline.
		IFS=: read -ra dirs <<<"${ROOT_LOOKUP_PATH-${PATH:-}}"
		for d in ${dirs[@]+"${dirs[@]}"}; do
			[[ -n "$d" ]] || continue
			for name in targo alab-targo; do
				f="$d/$name"
				[[ -f "$f" && -x "$f" ]] || continue
				c="$(gate_shim_target "$f")"
				if gate_store_targo "$c"; then
					cand="$c"
					break 2
				fi
				[[ -n "$first" ]] || first="$c"
			done
		done
		[[ -n "$cand" ]] || cand="$first"
	fi
	if [[ -z "$cand" ]]; then
		MANAGED_DECLINE="no aterm-managed Trust store is in reach (no \`aterm\` to ask, no \`targo\` on PATH)"
		return 1
	fi
	MANAGED_SEEN=1
	gate_store_targo "$cand"
}

# The trust pin's one remedy when no rustup is installed: `aterm pkg install
# trust` where an `aterm` this run may run (may_run) is in reach to run it (on
# PATH, or install.sh's $BIN_DIR), else the one path left on a machine with no
# such `aterm` — rustup plus the hand-linked Trust tarballs, since rustup alone
# cannot download the pin.
# Trust is published for macOS only (atpkg's trust package and the toolchain
# tarballs alike), so elsewhere the remedy is rustup and a Trust built from
# source (trust_link_remedy).
gate_trust_remedy() {
	local a
	a="$(lookup_program aterm 2>/dev/null)" || a=""
	if [[ "$(uname -s)" != Darwin ]]; then
		printf '%s\n' "install rustup (https://rustup.rs), then $(trust_link_remedy)"
	elif { [[ -n "$a" ]] && may_run "$a"; } ||
		{ [[ -n "${BIN_DIR:-}" && -x "${BIN_DIR:-}/aterm" ]] && may_run "$BIN_DIR/aterm"; }; then
		printf '%s\n' "aterm pkg install trust"
	else
		printf '%s\n' "install rustup (https://rustup.rs), then unpack the Trust toolchain tarballs (https://github.com/alabsystems/trust/releases) into one prefix and: rustup toolchain link trust <prefix>"
	fi
}

# How a machine with rustup gets the `trust` toolchain <name> it pins: on macOS
# the product's way (`aterm pkg install trust`, for `trust` only) or the published
# Trust toolchain tarballs, hand-linked; elsewhere no Trust build is published
# (no atpkg package, no tarball), so it is built from the trust repository's
# source and linked.
trust_link_remedy() { # [name]
	local name="${1:-trust}"
	if [[ "$(uname -s)" != Darwin ]]; then
		printf '%s\n' "build the Trust toolchain from source (https://github.com/alabsystems/trust publishes no build for $(uname -s)) and: rustup toolchain link $name <prefix>"
	elif [[ "$name" == trust ]]; then
		printf '%s\n' "aterm pkg install trust; or unpack the Trust toolchain tarballs (https://github.com/alabsystems/trust/releases) into one prefix, then: rustup toolchain link trust <prefix>"
	else
		printf '%s\n' "unpack the Trust toolchain tarballs (https://github.com/alabsystems/trust/releases) into one prefix, then: rustup toolchain link $name <prefix>"
	fi
}

# Which cargo, and which lane. With a <root>, the managed store is tried first
# (CARGO_LANE=store: CARGO_BIN is the store's targo and RUSTUP_BIN is empty —
# there is none to consult); without one, or when no managed store applies,
# rustup's lane runs (CARGO_LANE=rustup). Under the trust pin a refusal with no
# rustup installed names one remedy (gate_trust_remedy); with rustup installed,
# the product's remedy comes first, then rustup's link. Under a stock pin,
# which the store cannot serve, rustup's remedy alone. Under root, a cargo or
# rustup another account can change is refused before it runs
# (root_may_run_toolchain), as cargo_honours_pin refuses the cargo, rustc or
# trustc it would run next; and when nothing root may run is in reach, but an
# `aterm` root would not ask was (MANAGED_ROOT_REFUSED), the refusal names
# that aterm, never `aterm pkg install trust`, which would run it as root.
find_cargo_and_rustup() { # [root]
	local root="${1:-}" home="${CARGO_HOME:-$HOME/.cargo}" beside="" seen managed=1 declined=""
	if [[ "$(installer_uid)" == 0 ]]; then
		if [[ "$home" != /* ]]; then
			local here="$PWD"
			[[ -z "$root" ]] || here="$(cd -P -- "$root" && pwd)" || return 1
			home="$here/$home"
		fi
		# Preflight already interprets a relative cargo home at the checkout.
		# Discovery, probes and the later build must read that same directory.
		[[ -z "${CARGO_HOME:-}" ]] || export CARGO_HOME="$home"
	fi
	CARGO_BIN=""
	RUSTUP_BIN=""
	CARGO_LANE=""
	CARGO_RESOLVE_REASON=""
	MANAGED_DECLINE=""
	MANAGED_SEEN=""
	MANAGED_ROOT_REFUSED=""
	if [[ -n "$root" ]]; then
		if find_managed_targo "$root"; then
			CARGO_LANE=store
			CARGO_BIN="$MANAGED_TARGO"
			root_may_run_toolchain "$CARGO_BIN"
			return
		fi
		[[ "$(gate_pinned_channel "$root")" == trust ]] || managed=0
	fi
	# A targo that was in reach but is not the store's is worth naming in a
	# refusal: it is the hijack this lane refuses to build under.
	[[ -z "$MANAGED_SEEN" ]] || declined="; the targo in reach was declined: $MANAGED_DECLINE"
	CARGO_LANE=rustup
	# rustup first: its proxies sit beside it.
	if [[ -x "$home/bin/rustup" ]]; then
		RUSTUP_BIN="$home/bin/rustup"
	elif ! RUSTUP_BIN="$(lookup_program rustup 2>/dev/null)" || [[ -z "$RUSTUP_BIN" ]]; then
		RUSTUP_BIN=""
	fi
	if [[ -n "$RUSTUP_BIN" ]]; then
		beside="$(dirname "$RUSTUP_BIN")/cargo"
	fi
	if [[ -n "$beside" && -x "$beside" ]]; then
		CARGO_BIN="$beside"
	elif [[ -x "$home/bin/cargo" ]]; then
		CARGO_BIN="$home/bin/cargo"
	elif ! CARGO_BIN="$(lookup_program cargo 2>/dev/null)" || [[ -z "$CARGO_BIN" ]]; then
		CARGO_BIN=""
		if [[ -n "$MANAGED_ROOT_REFUSED" ]] && { [[ -z "$RUSTUP_BIN" ]] || ! may_run "$RUSTUP_BIN"; }; then
			# Under root, with nothing root may run in reach: the toolchain is
			# whatever the aterm root did not ask would have named.
			root_may_run_toolchain "$MANAGED_ROOT_REFUSED"
			CARGO_RESOLVE_REASON+="$declined"
		elif [[ -n "$RUSTUP_BIN" && "$managed" -eq 1 ]]; then
			CARGO_RESOLVE_REASON="building from source needs a toolchain that honours rust-toolchain.toml, and none is in reach: no aterm-managed Trust store (aterm pkg install trust lays one), and no cargo — no proxy beside $RUSTUP_BIN (at $beside), none at $home/bin/cargo, none on PATH — reinstall rustup (https://rustup.rs), which lays its proxies beside its own binary$declined"
		elif [[ -n "$RUSTUP_BIN" ]]; then
			CARGO_RESOLVE_REASON="building from source needs cargo, and none is in reach: no proxy beside $RUSTUP_BIN (at $beside), none at $home/bin/cargo, none on PATH — reinstall rustup (https://rustup.rs), which lays its proxies beside its own binary$declined"
		elif [[ "$managed" -eq 1 ]]; then
			CARGO_RESOLVE_REASON="building from source needs a toolchain that honours rust-toolchain.toml, and none is in reach — $(gate_trust_remedy)$declined"
		else
			CARGO_RESOLVE_REASON="building from source needs cargo — install rustup (https://rustup.rs), whose proxy at $home/bin/cargo is the one cargo that honours rust-toolchain.toml$declined"
		fi
		return 1
	fi
	root_may_run_toolchain ${RUSTUP_BIN:+"$RUSTUP_BIN"} "$CARGO_BIN" || return 1
	if [[ -z "$RUSTUP_BIN" ]]; then
		seen="$(env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 "$CARGO_BIN" --version 2>/dev/null | head -n 1 || true)"
		if [[ "$managed" -eq 1 ]]; then
			CARGO_RESOLVE_REASON="$CARGO_BIN (${seen:-no version}) is not rustup's proxy and no rustup is installed, so rust-toolchain.toml's pin cannot be honoured — $(gate_trust_remedy)$declined"
		else
			CARGO_RESOLVE_REASON="$CARGO_BIN (${seen:-no version}) is not rustup's proxy and no rustup is installed, so rust-toolchain.toml's pin cannot be honoured — install rustup (https://rustup.rs); a Homebrew rust never honours a pin (brew uninstall rust)$declined"
		fi
		return 1
	fi
	return 0
}

# Under root, whether this run may run each <program> (may_run); else
# CARGO_RESOLVE_REASON names the first it may not, and status 1. The probes
# and the build run the toolchain as root, and one another account can change
# would run that account's code as root.
root_may_run_toolchain() { # <program>...
	local p
	for p in "$@"; do
		may_run "$p" && continue
		CARGO_RESOLVE_REASON="root does not run a toolchain another account can change, and $p is one (it, a folder on its way from /, or a link it passes through is not root's alone) — run install.sh as the account whose toolchain it is, without sudo"
		return 1
	done
}

cargo_honours_pin() {
	local root="$1" pinned here pinfile named proxies remedy theirs why d pin trustc
	CARGO_VERSION=""
	CARGO_TOOLCHAIN=""
	CARGO_RUSTC=""
	CARGO_RESOLVE_REASON=""
	here="$(cd "$root" && env pwd -P 2>/dev/null)" || here="$root"
	pinfile="$here/rust-toolchain.toml"
	if [[ "${CARGO_LANE:-}" == store ]]; then
		# The managed lane's proof: the file pins `trust` (read again here, as
		# the proof step), the store's targo reports itself as targo, and the
		# trustc beside it, which it runs by absolute path, is there. A store
		# that fails these is damaged, not absent: the remedy is the product's,
		# never rustup's.
		pin="$(gate_pinned_channel "$root")"
		if [[ "$pin" != trust ]]; then
			CARGO_RESOLVE_REASON="$pinfile pins '${pin:-nothing}', not 'trust' — the aterm-managed store at $MANAGED_PREFIX holds only the Trust toolchain, so this checkout builds under rustup's proxy (https://rustup.rs)"
			return 1
		fi
		CARGO_VERSION="$(env -u RUSTUP_TOOLCHAIN "$CARGO_BIN" --version 2>/dev/null | head -n 1 || true)"
		case "$CARGO_VERSION" in
		"targo "*) ;;
		*)
			CARGO_RESOLVE_REASON="$CARGO_BIN (${CARGO_VERSION:-no version}) sits in the aterm-managed store but does not report itself as targo — store build $MANAGED_BUILD under $MANAGED_PREFIX is damaged: aterm pkg doctor, then aterm pkg install trust"
			return 1
			;;
		esac
		trustc="$(dirname "$CARGO_BIN")/trustc"
		if [[ ! -x "$trustc" ]]; then
			CARGO_RESOLVE_REASON="$CARGO_BIN ($CARGO_VERSION) has no trustc beside it at $trustc — targo runs the compiler beside itself, so store build $MANAGED_BUILD under $MANAGED_PREFIX is incomplete: aterm pkg doctor, then aterm pkg install trust"
			return 1
		fi
		root_may_run_toolchain "$trustc" || return 1
		CARGO_RUSTC="$trustc"
		CARGO_TOOLCHAIN="trust (pinned by '$pinfile'; aterm-managed store build $MANAGED_BUILD under $MANAGED_PREFIX)"
		return 0
	fi
	if ! pinned="$(cd "$root" && env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 "$RUSTUP_BIN" which cargo 2>/dev/null)" || [[ -z "$pinned" ]]; then
		why="$(cd "$root" && env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 "$RUSTUP_BIN" which cargo 2>&1 >/dev/null | head -n 1 || true)"
		CARGO_RESOLVE_REASON="rustup cannot resolve this checkout's pinned toolchain (rust-toolchain.toml): ${why:-$RUSTUP_BIN which cargo failed}"
		# A `trust` pin rustup cannot serve, with no aterm-managed store in reach
		# either: the product's remedy first — `aterm pkg install trust` lays the
		# store (and, where rustup is installed, the ~/.rustup/toolchains/trust
		# link rustup resolves the pin through, crates/atpkg/src/seam.rs) — and
		# the hand-linked tarballs second, for a machine with no `aterm` to run it.
		if [[ "$(gate_pinned_channel "$root")" == trust && -z "${MANAGED_SEEN:-}" ]]; then
			CARGO_RESOLVE_REASON="$CARGO_RESOLVE_REASON — $(trust_link_remedy)"
		fi
		return 1
	fi
	# rustup's proxies run the pin's own cargo and, for the build, the rustc
	# beside it.
	d="${pinned%/cargo}/rustc"
	[[ -e "$d" || -L "$d" ]] || d=""
	root_may_run_toolchain "$pinned" ${d:+"$d"} || return 1
	# What rustup resolved must be the file's pin (the lane header): `show
	# active-toolchain` prints `<name> (overridden by '<file>')`, and <file> must
	# be this checkout's rust-toolchain.toml, by inode. The messages spell the
	# root as the getcwd-backed external pwd does (`env pwd -P`, not the
	# builtin), which is rustup's spelling and the path a `rustup override` on it
	# is keyed by; such an override prints `<name> (directory override for
	# '<dir>')`.
	CARGO_TOOLCHAIN="$(cd "$root" && env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 "$RUSTUP_BIN" show active-toolchain 2>/dev/null | head -n 1 || true)"
	named="${CARGO_TOOLCHAIN#*"(overridden by '"}"
	named="${named%"')"*}"
	if [[ "$named" == "$CARGO_TOOLCHAIN" || ! "$named" -ef "$root/rust-toolchain.toml" ]]; then
		case "$CARGO_TOOLCHAIN" in
		*"directory override"*)
			CARGO_RESOLVE_REASON="rustup resolves this checkout to $CARGO_TOOLCHAIN, not to the pin in $pinfile — a \`rustup override\` on the checkout directory outranks its rust-toolchain.toml: rustup override unset --path $here"
			;;
		*)
			CARGO_RESOLVE_REASON="rustup resolves this checkout to ${CARGO_TOOLCHAIN:-nothing it can name}, not to the pin in $pinfile (\`rustup show active-toolchain\` run there must name that file)"
			;;
		esac
		return 1
	fi
	CARGO_VERSION="$(cd "$root" && env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 "$CARGO_BIN" --version 2>/dev/null | head -n 1 || true)"
	theirs="$(env -u RUSTUP_TOOLCHAIN "$pinned" --version 2>/dev/null | head -n 1 || true)"
	# The remedy for a cargo that is not the proxy. find_cargo_and_rustup took
	# the cargo beside rustup first, so a refusal below means either that cargo
	# is not rustup's proxy (a rustup linked in beside another toolchain's
	# cargo), or nothing sits beside rustup and a stranger was taken; until a
	# proxy is back beside rustup no PATH order supplies one, so the remedy names
	# that dir, never a $CARGO_HOME/bin holding none.
	proxies="$(dirname "$RUSTUP_BIN")"
	if [[ "$CARGO_BIN" == "$proxies/cargo" ]]; then
		remedy="the cargo beside $RUSTUP_BIN is not its proxy (a rustup symlinked in beside another toolchain's cargo?) — rustup lays its proxies beside its own binary: reinstall it (https://rustup.rs), or remove the shadowing toolchain (Homebrew: brew uninstall rust)"
	else
		remedy="$proxies (beside $RUSTUP_BIN) holds no cargo proxy — reinstall rustup (https://rustup.rs), which lays one there, and put that dir first: export PATH=\"$proxies:\$PATH\" as a hard prepend in your shell rc (macOS path_helper reorders an inherited PATH, and \`. ~/.cargo/env\` skips a dir already present); or remove the shadowing toolchain (Homebrew: brew uninstall rust)"
	fi
	if [[ -z "$CARGO_VERSION" || "$CARGO_VERSION" != "$theirs" ]]; then
		CARGO_RESOLVE_REASON="$CARGO_BIN (${CARGO_VERSION:-no version}) ignores rust-toolchain.toml, whose pin is $pinned ($theirs) — $remedy"
		return 1
	fi
	# The same line proves only the same release: a plain cargo built from the
	# pin's commit prints it too. The proxy alone reads RUSTUP_TOOLCHAIN: named a
	# toolchain that is not installed it refuses (exit 1, no fetch under
	# RUSTUP_AUTO_INSTALL=0) where a plain cargo prints its version and exits 0.
	# The exit status is the discriminator, never the wording.
	if (cd "$root" && RUSTUP_AUTO_INSTALL=0 RUSTUP_TOOLCHAIN=aterm-pin-probe-absent "$CARGO_BIN" --version >/dev/null 2>&1); then
		CARGO_RESOLVE_REASON="$CARGO_BIN ($CARGO_VERSION) prints the pin's version line but is not rustup's proxy (it ignores RUSTUP_TOOLCHAIN): a build under it takes PATH's rustc, not the pinned toolchain at ${pinned%/bin/cargo} — $remedy"
		return 1
	fi
	# The rustc the build runs (the lane header): cargo_build_pinned puts
	# $CARGO_HOME/bin and then this cargo's own dir first on PATH, so the first
	# rustc in those two dirs, in that order, is the one the build resolves. It
	# must be rustup's proxy too: present, and refusing an absent
	# RUSTUP_TOOLCHAIN as the cargo did.
	for d in "${CARGO_HOME:-$HOME/.cargo}/bin" "$(dirname "$CARGO_BIN")"; do
		if [[ -x "$d/rustc" ]]; then
			CARGO_RUSTC="$d/rustc"
			break
		fi
	done
	if [[ -n "$CARGO_RUSTC" ]] && ! root_may_run_toolchain "$CARGO_RUSTC"; then
		CARGO_RUSTC=""
		return 1
	fi
	if [[ -z "$CARGO_RUSTC" ]]; then
		why="no rustc sits beside it (nor in ${CARGO_HOME:-$HOME/.cargo}/bin), so the build would take PATH's"
	elif (cd "$root" && RUSTUP_AUTO_INSTALL=0 RUSTUP_TOOLCHAIN=aterm-pin-probe-absent "$CARGO_RUSTC" --version >/dev/null 2>&1); then
		why="the rustc the build would take first, $CARGO_RUSTC, is not rustup's proxy (it ignores RUSTUP_TOOLCHAIN), so the build would compile with it"
		CARGO_RUSTC=""
	else
		return 0
	fi
	CARGO_RESOLVE_REASON="$CARGO_BIN is rustup's proxy, but $why, not the pinned toolchain's ${pinned%/cargo}/rustc — reinstall rustup (https://rustup.rs), which lays its proxies (cargo and rustc among them) side by side"
	return 1
}

# The build, under exactly what cargo_honours_pin proved.
#
# The managed lane: the store's targo on its explicit `--unverified` lane (a
# bare `targo build` refuses to make an implicitly unverified artifact), which
# appends `-Ztrust-verify=off` to every unit of that invocation;
# .cargo/config.toml carries no off-switch (its header says why).
# The store's bin dir goes first on PATH, never rustup's, so a build script
# that runs a bare `rustc` finds the store's sibling. An ambient RUSTC or
# CARGO_BUILD_RUSTC would steer the compiler, so both are scrubbed: the
# compiler the gate proved ($CARGO_RUSTC, trustc beside targo) is the one that
# runs, unless a cargo config's [build] rustc or a rustc wrapper names another
# (under root, root_build_preflight holds every config cargo reads to one no
# other account can change, and a wrapper the environment names to a program
# may_run allows; what a config of root's names is root's word).
#
# rustup's lane: the same cargo from the same root, with RUSTUP_TOOLCHAIN
# scrubbed as from every probe, and $CARGO_HOME/bin then the cargo's own dir
# first on PATH: the two dirs the rustc proof searched, in its order (this
# prepend, not rustup, puts the proved rustc first). rustup's cargo takes no
# `--unverified`, so this lane names the same switch the other way:
# CARGO_UNVERIFIED_LANE, one `--config` that appends `-Ztrust-verify=off` to the
# Trust-lane table for this invocation. Without it the Trust compiler verifies
# every unit of the graph at its default strict policy, dependencies and build
# scripts included, and fails closed. The table is `cfg(trust_verify)`-scoped,
# so a stock toolchain matches nothing and ignores the line.
#
# Both lanes: no --target triple, and CARGO_BUILD_TARGET unset — an explicit
# target makes cargo withhold .cargo/config.toml's [target.*] rustflags (and
# CARGO_UNVERIFIED_LANE with them) from host units. Under root, PATH after the
# lane's own folders keeps only those root_build_path allows: the build runs
# cc, ar and ld from it, and a root run's PATH may be the account's that ran
# sudo; and the build, its git (aterm's build script's among them) and on
# macOS its cc, run in root_build_env's environment.
CARGO_UNVERIFIED_LANE="target.'cfg(trust_verify)'.rustflags=[\"-Ztrust-verify=off\"]"
cargo_build_pinned() { # <root>
	local rest=":$PATH"
	local -a genv=()
	if [[ "$(installer_uid)" == 0 ]]; then
		rest="$(root_build_path)"
		rest="${rest:+:$rest}"
		root_build_env || return 1
		genv=("${ROOT_BUILD_ENV[@]}")
	fi
	if [[ "${CARGO_LANE:-}" == store ]]; then
		(cd "$1" && env -u CARGO_BUILD_TARGET -u RUSTUP_TOOLCHAIN -u RUSTC -u CARGO_BUILD_RUSTC ${genv[@]+"${genv[@]}"} PATH="$(build_path_lead)$rest" "$CARGO_BIN" --unverified build --release -p aterm --target-dir "$1/target")
	else
		(cd "$1" && env -u CARGO_BUILD_TARGET -u RUSTUP_TOOLCHAIN ${genv[@]+"${genv[@]}"} PATH="$(build_path_lead)$rest" "$CARGO_BIN" build --release -p aterm --target-dir "$1/target" --config "$CARGO_UNVERIFIED_LANE")
	fi
}
# The rustup lane in the block above serves a tree whose pin names a stock channel,
# or a `trust` link rustup resolves with no aterm-managed store in reach. The public
# snapshot pins `trust` like the dev tree, so on a machine with aterm both take
# the store lane. A dev build of the tree is `targo --unverified ship
# dev-app`, on the Trust toolchain the release cutter resolves.

# The folders a source build puts first on PATH, its lane's: the store's bin
# dir, or $CARGO_HOME/bin and then the proved cargo's own dir.
build_path_lead() {
	if [[ "${CARGO_LANE:-}" == store ]]; then
		dirname "$CARGO_BIN"
	else
		printf '%s\n' "${CARGO_HOME:-$HOME/.cargo}/bin:$(dirname "$CARGO_BIN")"
	fi
}

# What a root run's git reads of git's config: with git 2.32 or later, the
# checkout's own alone, which root_build_preflight holds to root's.
# GIT_CONFIG_GLOBAL=/dev/null keeps it from ~/.gitconfig and
# $XDG_CONFIG_HOME/git/config (HOME may be another account's, and a
# core.fsmonitor or a filter there names a program git runs), and
# GIT_CONFIG_NOSYSTEM from its system config (a git found on PATH looks for
# one under its own prefix).
ROOT_GIT_ENV=(GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1)
# What root_xcode_judged found, never what the environment says.
unset ROOT_DEVELOPER_DIR ROOT_SDKROOT ROOT_BUILD_ENV

# git over the checkout, as the build runs it: under root, the first git in
# root_build_path's folders, run in root_build_env's environment, and none
# when there are no such folders or no such environment; as the account,
# PATH's.
checkout_git() { # <arg>...
	local d
	if [[ "$(installer_uid)" != 0 ]]; then
		git "$@"
		return
	fi
	d="$(root_build_path)"
	[[ -n "$d" ]] && root_build_env || return 1
	env "${ROOT_BUILD_ENV[@]}" PATH="$d" git "$@"
}

# Does <name> parse as a rustup dist toolchain, one `rustup toolchain install`
# can fetch? rustup's grammar (`rustup help toolchain`, 1.29.1):
#   <channel>[-<date>][-<host>]
#   <channel>    = stable|beta|nightly|<versioned>[-<prerelease>]
#   <versioned>  = <major.minor>|<major.minor.patch>
#   <prerelease> = beta[.<number>]
#   <date>       = YYYY-MM-DD
# Anything else (the dev tree's `trust`) is a custom name, which only
# `rustup toolchain link` makes. The source lane's absent-pin skip names the
# remedy by this test.
rustup_dist_channel() { # <name>
	local re='^(stable|beta|nightly|[0-9]+\.[0-9]+(\.[0-9]+)?(-beta(\.[0-9]+)?)?)(-[0-9]{4}-[0-9]{2}-[0-9]{2})?(-[A-Za-z0-9_.]+)*$'
	[[ "$1" =~ $re ]]
}

# The release architecture for <uname -s> <uname -m>; status 1 where no Linux
# release exists.
linux_release_arch() {
	case "$1:$2" in
	Linux:x86_64) echo x86_64 ;;
	Linux:aarch64 | Linux:arm64) echo aarch64 ;;
	*) return 1 ;;
	esac
}

# The Linux target's remote pre-flight, head-only: no release catalog is listed —
# the head comes from channel_head, and everything below is one
# redirect-refusing HEAD per asset (release_asset_present).
#
# <tag> is installable when it is at or below <head> (a native target is
# authorized only under the head's current appcast and roster), it publishes
# <asset> — the signed raw binary for this architecture, so presence is decided
# per platform — and, when it is older than the head, it still carries an
# appcast to bind that binary: the current name, else the archived
# aterm-appcast-<tag>.toml the cutter re-attaches when a newer release takes
# the head. The head's own appcast is read later, from the head.
#
# Sets LINUX_TARGET_SKIP (empty when installable, else the reason the half is
# skipped) and LINUX_MANIFEST_ASSET (the appcast name to fetch from <tag>).
# Called directly, never in $(…), so both globals reach the caller.
LINUX_TARGET_SKIP=""
linux_target_preflight() { # <tag> <head-tag> <asset>
	local tag="$1" head="$2" asset="$3" present
	LINUX_TARGET_SKIP=""
	LINUX_MANIFEST_ASSET=aterm-appcast.toml
	if ! compare_numeric_tags "$tag" "$head" || [[ "$TAG_COMPARE_RESULT" -gt 0 ]]; then
		LINUX_TARGET_SKIP="release $tag is newer than the latest release $head — pin $head or an older release with --version"
		return 0
	fi
	present=0
	release_asset_present "$tag" "$asset" || present=$?
	case "$present" in
	1)
		# Unpinned, <tag> IS the head: the only release this run inspected, so
		# it can say no more than that one publishes none — and `--version`
		# names an older release, which helps only if one has a binary.
		if [[ "${TAG_EXPLICIT:-0}" -eq 1 ]]; then
			LINUX_TARGET_SKIP="release $tag has no $asset — pin a release that has one with --version"
		else
			LINUX_TARGET_SKIP="the latest release $tag publishes no Linux ${asset##*-linux-} executable ($asset) — nothing to install on this machine yet"
		fi
		return 0
		;;
	2)
		LINUX_TARGET_SKIP="could not inspect release $tag for $asset ($ASSET_WHY)"
		return 0
		;;
	esac
	[[ "$tag" != "$head" ]] || return 0
	present=0
	release_asset_present "$tag" aterm-appcast.toml || present=$?
	if [[ "$present" -eq 1 ]]; then
		present=0
		release_asset_present "$tag" "aterm-appcast-$tag.toml" || present=$?
		[[ "$present" -ne 0 ]] || LINUX_MANIFEST_ASSET="aterm-appcast-$tag.toml"
	fi
	case "$present" in
	1) LINUX_TARGET_SKIP="release $tag has no signed manifest that covers its Linux binary, so it cannot be installed — pin another release with --version" ;;
	2) LINUX_TARGET_SKIP="could not inspect release $tag for its signed manifest ($ASSET_WHY)" ;;
	esac
}

# Does the authenticated Linux candidate <exe> say it is release <tag>? Its
# `--version` FIRST line must be `aterm X.Y.Z` for vX.Y.Z (`+<build metadata>` or
# ` <note>` after it is the same release). Only that line is the identity:
# `aterm --version` prints the origin line, the build, which copy runs and the
# trust roots under it (aterm-cli's version_text), and comparing the whole report
# refused every release. Status 0 yes, 1 it runs and names another version, 2 it
# cannot run here (a non-zero exit, or cut off by bounded's hang detector; what it
# printed on stderr, a missing glibc symbol say, reaches the person);
# LINUX_CANDIDATE_SAID is the line it gave. It runs through bounded (on /dev/null,
# its process group cut off at VERIFY_WAIT_SECS) and its answer is read from a
# file, as first_line_bounded runs every other binary this script asks what it
# is. Called directly, never in $(…).
LINUX_CANDIDATE_SAID=""
linux_candidate_answers_tag() { # <exe> <tag>
	local out s=0
	LINUX_CANDIDATE_SAID=""
	local BOUNDED_TEMPS=(${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"})
	probe_temp out aterm-probe || return 2
	BOUNDED_OUT="$out" bounded "$VERIFY_WAIT_SECS" "$1" --version || s=$?
	[[ "$s" -ne 0 ]] || LINUX_CANDIDATE_SAID="$(head -n 1 "$out")"
	rm -f "$out"
	[[ "$s" -eq 0 ]] || return 2
	case "$LINUX_CANDIDATE_SAID" in
	"aterm ${2#v}" | "aterm ${2#v}+"* | "aterm ${2#v} "*) return 0 ;;
	*) return 1 ;;
	esac
}

# --- the Linux bootstrap verifier ---------------------------------------------
# Linux has no Apple bootstrap chain, so install.sh verifies the same
# paper-master -> live machine -> appcast chain as update-core itself before it
# reads an artifact claim. The verifier is crates/aterm-bootstrap-verify, a
# static executable each release publishes per architecture as
# aterm-bootstrap-verify-<version>-linux-<arch>. The four lines below pin the
# repository and release it comes from and each architecture's SHA-256.
# `targo --unverified ship pin-verifier` writes them from the release's own
# receipts, with crates/aterm-bootstrap-verify/pinned.toml, and that crate's
# tests hold the two to each other and to RELEASES.ledger. They stay empty
# until a release publishes the asset; until then, and on an architecture with
# no digest, the embedded Python verifier below runs. Once pinned, the fetched
# file must match its digest before it runs, and nothing falls back to Python.
LINUX_VERIFIER_REPO=""
LINUX_VERIFIER_TAG=""
LINUX_VERIFIER_SHA256_X86_64=""
LINUX_VERIFIER_SHA256_AARCH64=""
# The verifier executable linux_verifier_ready made ready (empty for the
# embedded one), the private directory it was fetched into, why it is not
# ready, and, in a dry run, the pinned asset a real install would fetch and its
# size (nothing is fetched, so the store pre-flight waits for that install).
LINUX_VERIFIER=""
LINUX_VERIFIER_DIR=""
LINUX_VERIFIER_WHY=""
LINUX_VERIFIER_PLANNED=""
LINUX_VERIFIER_PLANNED_SIZE=""

# The test seam's verifier: ATERM_INSTALL_VERIFIER, a test's build held to
# ATERM_INSTALL_VERIFIER_SHA256. It is read only when a suite sourced this file
# (ATERM_INSTALL_LIBRARY_ONLY=1), so a whole run never reads it.
linux_verifier_seam() {
	[[ "${ATERM_INSTALL_LIBRARY_ONLY:-0}" == 1 ]] || return 0
	printf '%s' "${ATERM_INSTALL_VERIFIER:-}"
}

# <arch>'s pinned verifier digest; empty when it has none.
linux_verifier_pin() { # <arch>
	case "$1" in
	x86_64) printf '%s' "$LINUX_VERIFIER_SHA256_X86_64" ;;
	aarch64) printf '%s' "$LINUX_VERIFIER_SHA256_AARCH64" ;;
	esac
}

# Whether this host has a SHA-256 reader file_sha256 can use.
sha256_reader_here() {
	command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1
}

# The lowercase SHA-256 of <file>; status 1 when no tool here reads one.
file_sha256() { # <file>
	local out
	if command -v sha256sum >/dev/null 2>&1; then
		out="$(sha256sum -- "$1" 2>/dev/null)" || return 1
	elif command -v shasum >/dev/null 2>&1; then
		out="$(shasum -a 256 -- "$1" 2>/dev/null)" || return 1
	else
		return 1
	fi
	# GNU's escape mark, printed before the digest of a name it had to escape.
	out="${out#\\}"
	out="${out%% *}"
	[[ "$out" =~ ^[0-9a-f]{64}$ ]] || return 1
	printf '%s\n' "$out"
}

# Whether <file> is a regular file whose SHA-256 is <sha>, 64 lowercase hex
# digits; LINUX_VERIFIER_WHY says why not.
linux_verifier_pinned() { # <file> <sha>
	local got
	if ! [[ "$2" =~ ^[0-9a-f]{64}$ ]]; then
		LINUX_VERIFIER_WHY="the bootstrap verifier $1 has no pinned SHA-256 to be held to — refusing to run it"
		return 1
	fi
	if [[ -L "$1" || ! -f "$1" ]]; then
		LINUX_VERIFIER_WHY="the bootstrap verifier $1 is not a regular file — refusing to run it"
		return 1
	fi
	got="$(file_sha256 "$1")" || {
		LINUX_VERIFIER_WHY="cannot read the SHA-256 of the bootstrap verifier $1 — refusing to run it"
		return 1
	}
	[[ "$got" == "$2" ]] && return 0
	LINUX_VERIFIER_WHY="the bootstrap verifier $1 has SHA-256 $got, not the pinned $2 — refusing to run it"
	return 1
}

# Remove the directory the verifier was fetched into. Always safe to call.
linux_verifier_cleanup() {
	[[ -n "$LINUX_VERIFIER_DIR" ]] || return 0
	rm -rf -- "$LINUX_VERIFIER_DIR"
	LINUX_VERIFIER_DIR=""
	LINUX_VERIFIER=""
}

# install_linux_app's EXIT trap, over the TMP it sets. It replaces the
# verifier's own trap, so it removes the verifier's directory too. Neither
# removal may be cut short, as in install_app_cleanup: a Ctrl-C in the first
# used to end the run before the second.
install_linux_app_cleanup() {
	set +e
	trap '' INT HUP TERM QUIT
	linux_verifier_cleanup
	[[ -z "$TMP" ]] || rm -rf "$TMP"
}

# Make <arch>'s verifier ready. Under the test seam that is ATERM_INSTALL_VERIFIER,
# a test's build of the verifier, held to ATERM_INSTALL_VERIFIER_SHA256 as the pin
# holds a release's. Pinned, it is the release asset of the pin's own
# repository, fetched into a private directory (removed on exit) and held to its
# digest; a dry run only sizes it (LINUX_VERIFIER_PLANNED). Otherwise it is the
# embedded Python verifier, when Python 3.11+ and OpenSSL are here. Status 0
# ready (LINUX_VERIFIER names the executable, empty for the embedded one and in
# a dry run); 1 the half is skipped (no tools, or no fetch); 2 the verifier is
# refused (it is not the pinned file). LINUX_VERIFIER_WHY says why. Called
# directly, never in $(…), so the globals reach the caller.
linux_verifier_ready() { # <arch>
	local seam pinned at size
	LINUX_VERIFIER=""
	LINUX_VERIFIER_WHY=""
	LINUX_VERIFIER_PLANNED=""
	LINUX_VERIFIER_PLANNED_SIZE=""
	seam="$(linux_verifier_seam)"
	pinned="$(linux_verifier_pin "$1")"
	if [[ -n "$seam$pinned" ]] && ! sha256_reader_here; then
		# The tool is an environment fact, as Python is for the embedded verifier:
		# the half is skipped, never refused.
		LINUX_VERIFIER_WHY="the pinned bootstrap verifier is held to its SHA-256, and this host has neither sha256sum nor shasum to read one — install coreutils and re-run"
		return 1
	fi
	if [[ -n "$seam" ]]; then
		linux_verifier_pinned "$seam" "${ATERM_INSTALL_VERIFIER_SHA256:-}" || return 2
		LINUX_VERIFIER="$seam"
		return 0
	fi
	if [[ -z "$pinned" ]]; then
		if command -v openssl >/dev/null 2>&1 &&
			python3 -I -c 'import tomllib, hashlib, os; assert hasattr(os, "memfd_create"); assert hasattr(hashlib, "file_digest")' >/dev/null 2>&1; then
			return 0
		fi
		LINUX_VERIFIER_WHY="signed Linux bootstrap needs Python 3.11+ and OpenSSL with Ed25519 support — install them and re-run"
		return 1
	fi
	# The release that minted the pin publishes the verifier, whichever
	# repository this install reads; the digest, not the host, authenticates it.
	at="aterm-bootstrap-verify-${LINUX_VERIFIER_TAG#v}-linux-$1"
	size="$(REPO_SLUG="$LINUX_VERIFIER_REPO" release_asset_size "$LINUX_VERIFIER_TAG" "$at" 67108864)" || {
		LINUX_VERIFIER_WHY="could not read the bootstrap verifier $at in release $LINUX_VERIFIER_TAG of $LINUX_VERIFIER_REPO; retry in a few minutes"
		return 1
	}
	if [[ "${DRY_RUN:-0}" -eq 1 ]]; then
		LINUX_VERIFIER_PLANNED="$at"
		LINUX_VERIFIER_PLANNED_SIZE="$size"
		return 0
	fi
	linux_verifier_cleanup
	trap linux_verifier_cleanup EXIT
	temp_dir LINUX_VERIFIER_DIR "${TMPDIR:-/tmp}" aterm-verifier || {
		LINUX_VERIFIER_WHY="cannot create a private directory for the bootstrap verifier under ${TMPDIR:-/tmp}"
		return 1
	}
	(REPO_SLUG="$LINUX_VERIFIER_REPO" && download_release_asset "$LINUX_VERIFIER_TAG" "$at" "$size" "$LINUX_VERIFIER_DIR/$at") || {
		LINUX_VERIFIER_WHY="could not download the bootstrap verifier $at from release $LINUX_VERIFIER_TAG of $LINUX_VERIFIER_REPO; retry in a few minutes"
		return 1
	}
	linux_verifier_pinned "$LINUX_VERIFIER_DIR/$at" "$pinned" || return 2
	chmod 700 "$LINUX_VERIFIER_DIR/$at" || {
		LINUX_VERIFIER_WHY="cannot make the bootstrap verifier $LINUX_VERIFIER_DIR/$at executable"
		return 1
	}
	LINUX_VERIFIER="$LINUX_VERIFIER_DIR/$at"
}

# The verifier's one interface: `--store-path <store>`, or
# `<dir> <tag> <slug> <arch> <store> [<candidate>]`, which prints the signed
# target's `name digest size build roster_seq` line. A refusal exits 1 with one
# `install.sh: Linux verification refused: <why>` line on stderr. It runs what
# linux_verifier_ready made ready. With a pin (or the seam) and nothing ready it
# refuses, so it never falls back to the embedded verifier.
linux_release_verify() {
	if [[ -n "$LINUX_VERIFIER" ]]; then
		"$LINUX_VERIFIER" "$@" </dev/null
		return
	fi
	if [[ -n "$(linux_verifier_seam)$(linux_verifier_pin "${LINUX_ARCH:-}")" ]]; then
		echo "install.sh: Linux verification refused: the pinned bootstrap verifier is not ready" >&2
		return 1
	fi
	linux_release_verify_python "$@"
}

# The embedded verifier, until a release pins the static one. It lives in the
# one-file installer so the curl|bash path fetches no executable helper; this
# heredoc is its only copy. Python parses TOML, OpenSSL performs Ed25519, and
# ctypes reads the two facts the standard library lacks (statfs, getpwent_r).
# crates/aterm-update/tests/install_sh runs this interpreter over Rust-signed
# synthetic releases (swapping only the pin, never through a production
# override) and holds it to the updater's chain and to the committed root pin.
linux_release_verify_python() {
	# -I: isolated, so neither the directory the installer runs in (a piped
	# install's cwd is wherever the person typed it) nor PYTHON* variables nor
	# user site-packages can put a module of their own ahead of the stdlib.
	python3 -I - "$@" <<'ATERM_LINUX_VERIFY_PY'
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
"""Read-only signed release verification for the Linux bootstrap.

Its one source is this heredoc in tools/install.sh, which the one-file curl|bash
installer runs; crates/aterm-update/tests/install_sh holds it to the updater's
chain and root pin.
"""

import base64
import datetime
import errno
import grp
import hashlib
import os
import pathlib
import pwd
import re
import stat
import subprocess
import sys
import tempfile
import time
import tomllib

MASTER_PUBKEYS = ("DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ=",)
MAX_BINARY = 512 * 1024 * 1024
U64_MAX = 18446744073709551615


def require(condition, message):
    if not condition:
        raise ValueError(message)


def uint(value, name, maximum=U64_MAX):
    require(type(value) is int and 0 <= value <= maximum, "invalid " + name)
    return value


def read_bounded(path, maximum):
    with open(path, "rb") as stream:
        data = stream.read(maximum + 1)
    require(len(data) <= maximum, "oversized " + str(path))
    return data


def verify_ed25519(key, data, signature):
    try:
        raw = base64.b64decode(key, validate=True)
    except (ValueError, TypeError):
        return False
    if len(raw) != 32 or base64.b64encode(raw).decode("ascii") != key or len(signature) != 64:
        return False
    # Anonymous in-memory descriptors: no temp files, including during a dry
    # diagnostic, and no shell interpretation of keys or authenticated bytes.
    descriptors = []
    temporary = None
    try:
        payloads = (bytes.fromhex("302a300506032b6570032100") + raw, data, signature)
        if hasattr(os, "memfd_create"):
            for payload in payloads:
                fd = os.memfd_create("aterm-bootstrap", os.MFD_CLOEXEC)
                descriptors.append(fd)
                os.write(fd, payload)
                os.lseek(fd, 0, os.SEEK_SET)
            key_path, data_path, sig_path = [f"/proc/self/fd/{fd}" for fd in descriptors]
        else:
            # Website sync also runs on macOS publishers. Only public bytes
            # touch this private temporary directory; no installed-user state.
            temporary = tempfile.TemporaryDirectory(prefix="aterm-signature-")
            paths = [pathlib.Path(temporary.name) / name for name in ("key", "data", "sig")]
            for path, payload in zip(paths, payloads):
                path.write_bytes(payload)
            key_path, data_path, sig_path = map(str, paths)
        result = subprocess.run(
            ["openssl", "pkeyutl", "-verify", "-pubin", "-keyform", "DER",
             "-inkey", key_path, "-rawin", "-in", data_path, "-sigfile", sig_path],
            pass_fds=descriptors, stdout=subprocess.DEVNULL,
            # VERIFY_WAIT_SECS, the hang detector: OpenSSL answers or is wedged.
            stderr=subprocess.DEVNULL, timeout=60, check=False,
        )
        return result.returncode == 0
    finally:
        for fd in descriptors:
            os.close(fd)
        if temporary is not None:
            temporary.cleanup()


def deadline(value):
    require(isinstance(value, str) and re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", value),
            "invalid roster expiry")
    return datetime.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(
        tzinfo=datetime.timezone.utc).timestamp()


def withdrawn_floor(recorded, sources, revoked):
    """The apply floor once every revoked machine's part is withdrawn: Rust's
    FloorSources::step with nothing raised (crates/aterm-update/src/floor_sources.rs).
    A recorded floor its sources do not explain was raised without provenance and
    is kept whole (round eight of the update audit, finding 66)."""
    if not sources:
        return recorded
    require(isinstance(sources, dict), "invalid floor sources")
    base = uint(sources.get("base", 0), "floor base")
    machines = sources.get("machines", {})
    require(isinstance(machines, dict) and all(isinstance(key, str) and key for key in machines),
            "invalid floor sources")
    machines = {key: uint(value, "floor source") for key, value in machines.items()}
    if max([base, *machines.values()]) < recorded:
        return recorded
    kept = [min(value, recorded) for key, value in machines.items() if key not in revoked]
    return max([min(base, recorded), *kept])


def user_private_group(uid, gid, account, group, primaries):
    """Whether group `gid` can hold no account but `uid`: the user-private group
    Ubuntu's useradd makes (USERGROUPS_ENAB), whose umask 002 leaves this user's
    directories group-writable. `account` is uid's passwd (name, primary gid),
    `group` the group's (name, members), each None when unknown; `primaries` the
    uids of every passwd entry whose primary group is `gid`. The rule of
    crates/aterm-uds/src/shared_write.rs `user_private_group`, held equal to it
    by crates/aterm-update/tests/install_sh."""
    if uid == 0 or account is None or group is None:
        return False
    name, primary = account
    group_name, members = group
    return (primary == gid and group_name == name
            and all(member == name for member in members)
            and all(other == uid for other in primaries))


def libc():
    """(ctypes, this process's C library), for the two calls the standard
    library does not make (statfs, getpwent_r), or None when ctypes cannot
    load it; each caller then answers what grants no exemption."""
    try:
        import ctypes
        return ctypes, ctypes.CDLL(None, use_errno=True)
    except (ImportError, OSError):
        return None


def primary_members(gid):
    """The uids of every passwd entry whose primary group is `gid`, or None
    when the enumeration fails. getpwent_r, since pwd.getpwall's getpwent
    ends at a failure as at the end: 0 is an entry, ENOENT the end, ERANGE
    a buffer grown to at most 1 MiB, anything else no answer.
    crates/aterm-uds/src/shared_write.rs `nss::walk` is the updater's same walk."""
    loaded = libc()
    if loaded is None:
        return None
    ctypes, c = loaded

    class Passwd(ctypes.Structure):
        _fields_ = [("pw_name", ctypes.c_char_p), ("pw_passwd", ctypes.c_char_p),
                    ("pw_uid", ctypes.c_uint32), ("pw_gid", ctypes.c_uint32),
                    ("pw_gecos", ctypes.c_char_p), ("pw_dir", ctypes.c_char_p),
                    ("pw_shell", ctypes.c_char_p)]

    try:
        getpwent_r = c.getpwent_r
    except AttributeError:
        return None
    getpwent_r.argtypes = (ctypes.POINTER(Passwd), ctypes.c_char_p, ctypes.c_size_t,
                           ctypes.POINTER(ctypes.POINTER(Passwd)))
    getpwent_r.restype = ctypes.c_int
    uids, size = [], 4096
    buffer = ctypes.create_string_buffer(size)
    c.setpwent()
    try:
        while True:
            entry, found = Passwd(), ctypes.POINTER(Passwd)()
            ret = getpwent_r(ctypes.byref(entry), buffer, size, ctypes.byref(found))
            if ret == 0 and found:
                if entry.pw_gid == gid:
                    uids.append(entry.pw_uid)
            elif ret == errno.ENOENT:
                return uids
            elif ret == errno.ERANGE and size < 1 << 20:
                size *= 2
                buffer = ctypes.create_string_buffer(size)
            else:
                return None
    finally:
        c.endpwent()


def private_group(gid):
    """`user_private_group` over this machine's accounts (NSS). The passwd
    enumeration runs last, only for a group every other fact already admits,
    and one that fails admits nothing."""
    uid = os.getuid()
    try:
        entry = pwd.getpwuid(uid)
        account = (entry.pw_name, entry.pw_gid)
    except KeyError:
        account = None
    try:
        entry = grp.getgrgid(gid)
        group = (entry.gr_name, tuple(entry.gr_mem))
    except KeyError:
        group = None
    if not user_private_group(uid, gid, account, group, ()):
        return False
    primaries = primary_members(gid)
    return primaries is not None and user_private_group(uid, gid, account, group, primaries)


# The extended-ACL attributes Linux exposes: the POSIX access ACL, and the
# NFSv4 ACL an NFSv4 mount reads from its server.
ACL_ATTRIBUTES = ("system.posix_acl_access", "system.nfs4_acl")


def access_acl(path):
    """Whether `path` itself (a final symlink not followed) may carry an
    extended access ACL, POSIX or NFSv4, whose mask its group bits then are:
    only "no such attribute" and "this filesystem keeps none" from every
    attribute answer no.
    crates/aterm-uds/src/shared_write.rs `acl_carried` is the updater's rule."""
    for name in ACL_ATTRIBUTES:
        try:
            os.getxattr(path, name, follow_symlinks=False)
        except OSError as error:
            if error.errno in (errno.ENODATA, errno.ENOTSUP, errno.EOPNOTSUPP):
                continue
        return True
    return False


# statfs f_type of the network and FUSE filesystems, where a server or daemon
# decides access: NFS, SMB/CIFS (two), Ceph, AFS (two), 9P, Lustre, GPFS, FUSE.
REMOTE_FILESYSTEMS = frozenset((0x6969, 0xFF534D42, 0xFE534D42, 0x00C36400, 0x6B414653,
                                0x5346414F, 0x01021997, 0x0BD00BD0, 0x47504653, 0x65735546))


def remote_filesystem_type(magic):
    """Whether a filesystem of type `magic` is a network or FUSE one.
    crates/aterm-uds/src/shared_write.rs `remote_filesystem` is the rule."""
    return magic in REMOTE_FILESYSTEMS


def filesystem_type(path):
    """The f_type of the filesystem `path` is on (statfs, a final symlink
    followed: only real directories are asked), or None when it cannot be
    read. glibc's struct statfs on x86_64 and aarch64, the two the installer
    runs this verifier on, starts with f_type, a 64-bit long."""
    loaded = libc()
    if loaded is None:
        return None
    ctypes, c = loaded
    buffer = (ctypes.c_int64 * 32)()
    try:
        if c.statfs(os.fsencode(path), buffer) != 0:
            return None
    except AttributeError:
        return None
    return buffer[0] & 0xFFFFFFFF


def remote_filesystem(path):
    """Whether `path` is on a network or FUSE filesystem, or one whose type
    cannot be read."""
    magic = filesystem_type(path)
    return magic is None or remote_filesystem_type(magic)


def shared_write(path, meta):
    """Whether another account can write the directory `path`, which `meta`
    describes: any other-write bit, or a group-write bit unless the group is
    this user's private one and nothing past the mode widens it, neither an
    access ACL nor a remote filesystem.
    crates/aterm-uds/src/shared_write.rs `others_can_write_as` is the rule."""
    return meta.st_mode & 0o022 != 0 and not (
        meta.st_mode & 0o002 == 0 and meta.st_uid == os.getuid()
        and not access_acl(path) and not remote_filesystem(path)
        and private_group(meta.st_gid))


def check_install_path(store):
    """Refuse a store another account could replace: every component from / down
    a real directory of root's or this user's, and none writable by another
    account until a private (0700) directory of this user's shields the rest.
    A group-write bit is no other account's when the group is this user's
    private one (`private_group`) and nothing past the mode widens it: no
    access ACL (`access_acl`), no remote filesystem (`remote_filesystem`).
    crates/aterm-update/src/linux/ancestry.rs
    `refusal` is the updater's same walk."""
    store = pathlib.Path(store).absolute()
    private_boundary = False
    for ancestor in reversed((store, *store.parents)):
        meta = ancestor.lstat()
        require(stat.S_ISDIR(meta.st_mode) and meta.st_uid in (0, os.getuid()),
                "unsafe install path component: " + str(ancestor))
        require(private_boundary or not shared_write(ancestor, meta),
                "writable unshielded install path component: " + str(ancestor) + (
                    " (another account can write it; run: chmod go-w " + str(ancestor) + ")"
                    if meta.st_uid == os.getuid() else
                    " (another account can write it; set ATERM_STORE_DIR to a directory of yours)"))
        # A user-owned private ancestor prevents another uid from traversing
        # into the store, even when a child retains a group-writable umask.
        private_boundary |= meta.st_uid == os.getuid() and meta.st_mode & 0o077 == 0


def preflight_store(store):
    """The installer's pre-flight, before any download: `check_install_path` over
    the components of `store` that exist. Those the installer still has to make
    it makes with no group or other write, so they pass the same walk."""
    store = pathlib.Path(store).absolute()
    check_install_path(next(path for path in (store, *store.parents) if os.path.lexists(path)))


def read_install_policy(store):
    check_install_path(store)
    store = pathlib.Path(store).absolute()
    path = pathlib.Path(store) / ".aterm-update" / "state.toml"
    try:
        parent = path.parent.lstat()
    except FileNotFoundError:
        return {"floors": (0, 0, 0), "revoked_machines": (), "rejected_build": 0}
    require(stat.S_ISDIR(parent.st_mode) and parent.st_uid == os.getuid()
            and parent.st_mode & 0o077 == 0, "unsafe update-state directory")
    try:
        meta = path.lstat()
    except FileNotFoundError:
        return {"floors": (0, 0, 0), "revoked_machines": (), "rejected_build": 0}
    require(stat.S_ISREG(meta.st_mode) and meta.st_uid == os.getuid()
            and meta.st_mode & 0o077 == 0, "unsafe update-state file")
    state = tomllib.loads(read_bounded(path, 262144).decode("utf-8"))
    require(state.get("schema") == 1, "unsupported update-state schema")
    target = store / "aterm"
    require(state.get("target") == str(target),
            "update-state target mismatch")
    installed = state.get("installed")
    require(isinstance(installed, dict) and isinstance(installed.get("sha256"), str),
            "invalid installed identity")
    installed_build = uint(installed.get("build"), "installed build")
    revoked = state.get("revoked_machines", [])
    require(isinstance(revoked, list) and all(isinstance(item, str) and item for item in revoked),
            "invalid durable revoked machines")
    policy = {
        "floors": tuple(uint(state.get(key), key) for key in ("high_water", "min_build", "roster_floor")),
        "revoked_machines": revoked,
        "rejected_build": uint(state.get("rejected_build", 0), "rejected build"),
        "min_build_sources": state.get("min_build_sources"),
    }
    try:
        target_meta = target.lstat()
    except FileNotFoundError:
        require(state.get("enabled") is False and state.get("trial") is None,
                "enrolled executable is absent or an update trial needs recovery")
        require(re.fullmatch(r"[0-9a-f]{64}", installed["sha256"]) or
                (installed["sha256"] == "" and installed_build == 0), "invalid fresh installed identity")
        # First install may have persisted authenticated policy before probing or
        # renaming the first executable. Runtime owns this disabled-state retry;
        # preserve all floors instead of mistaking absence for a clean slate.
        return policy
    require(re.fullmatch(r"[0-9a-f]{64}", installed["sha256"]), "invalid installed identity")
    require(stat.S_ISREG(target_meta.st_mode) and target_meta.st_uid == os.getuid()
            and target_meta.st_mode & 0o6022 == 0 and target_meta.st_size <= MAX_BINARY,
            "unsafe installed executable")
    with open(target, "rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    rejected = policy["rejected_build"]
    trial = state.get("trial")
    if trial:
        require(isinstance(trial, dict), "invalid durable update trial")
        old, new = trial.get("old"), trial.get("new")
        require(all(isinstance(identity, dict) and isinstance(identity.get("sha256"), str)
                    and re.fullmatch(r"[0-9a-f]{64}", identity["sha256"]) for identity in (old, new)),
                "invalid durable trial identity")
        phase = trial.get("phase")
        prepared_old = phase == "Prepared" and actual == old["sha256"] == installed["sha256"]
        healthy_new = phase == "Installed" and trial.get("healthy") is True and actual == new["sha256"] == installed["sha256"]
        rolling_back = phase == "RollbackPrepared" and actual in (old["sha256"], new["sha256"])
        require(prepared_old or healthy_new or rolling_back,
                "an unresolved update trial requires a healthy launch or runtime recovery")
        if rolling_back:
            rejected = max(rejected, uint(new.get("build"), "rejected trial build"))
        # Runtime owns recovery under its lock. This read-only preflight admits
        # recoverable/healthy states without clearing intent or lowering floors.
    else:
        require(actual == installed["sha256"],
                "installed executable disagrees with durable update identity")
    policy["rejected_build"] = rejected
    return policy


def verify_manifest(directory, tag, slug, arch=None, floors=(0, 0, 0), now=None,
                    revoked_machines=(), rejected_build=0, manifest_name="aterm-appcast.toml",
                    min_build_sources=None):
    require(arch is None or arch in ("x86_64", "aarch64"), "unsupported Linux architecture")
    require(re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag),
            "noncanonical release tag")
    require(all(int(part) <= U64_MAX for part in tag[1:].split(".")), "release tag overflow")
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", slug), "invalid repository")
    root = pathlib.Path(directory)
    roster_bytes = read_bounded(root / "aterm-machines.toml", 65536)
    roster_sig = read_bounded(root / "aterm-machines.toml.sig", 64)
    require(any(verify_ed25519(key, roster_bytes, roster_sig) for key in MASTER_PUBKEYS),
            "roster is not signed by the pinned paper master")
    roster = tomllib.loads(roster_bytes.decode("utf-8"))
    require(uint(roster.get("schema"), "roster schema", 0xffffffff) <= 1, "newer roster schema")
    # Schema 1's key set is closed, as in aterm-update-core's roster.rs: TOML gives a key
    # written after the [[machine]] tables to the last machine, so an unread key is a
    # dropped instruction (a misplaced `revoked` reads as nothing revoked). Refuse it.
    require(set(roster) <= {"schema", "roster_seq", "valid_until", "revoked", "machine"},
            "unknown roster key")
    seq = uint(roster.get("roster_seq"), "roster sequence")
    require(seq >= floors[2], "roster rollback")
    now = time.time() if now is None else now
    require(now < deadline(roster.get("valid_until")), "roster expired")
    machines, revoked = roster.get("machine", []), roster.get("revoked", [])
    require(isinstance(machines, list) and len(machines) <= 16, "invalid machine roster")
    require(isinstance(revoked, list) and all(isinstance(item, str) and item for item in revoked),
            "invalid revoked machines")
    ids, keys = set(), set()
    for machine in machines:
        require(isinstance(machine, dict), "invalid machine")
        require(set(machine) <= {"id", "pubkey", "added_at", "not_after"}, "unknown machine key")
        require(isinstance(machine.get("added_at", ""), str), "invalid machine added_at")
        identity, key = machine.get("id"), machine.get("pubkey")
        require(isinstance(identity, str) and identity and identity not in ids,
                "empty or duplicate machine identity")
        require(isinstance(key, str) and key and key not in keys, "empty or duplicate machine key")
        try:
            decoded = base64.b64decode(key, validate=True)
        except (ValueError, TypeError) as error:
            raise ValueError("invalid roster machine key") from error
        require(len(decoded) == 32 and base64.b64encode(decoded).decode("ascii") == key,
                "noncanonical roster machine key")
        ids.add(identity)
        keys.add(key)
    require(manifest_name in ("aterm-appcast.toml", "aterm-policy-appcast.toml"), "invalid manifest proof name")
    manifest_bytes = read_bounded(root / manifest_name, 5_000_000)
    manifest_sig = read_bounded(root / (manifest_name + ".sig"), 64)
    signer = None
    for machine in machines:
        if machine["id"] in revoked or machine["id"] in revoked_machines:
            continue
        if "not_after" in machine and now >= deadline(machine["not_after"]):
            continue
        if verify_ed25519(machine["pubkey"], manifest_bytes, manifest_sig):
            signer = machine["id"]
            break
    require(signer is not None, "appcast has no authorized live machine signature")
    manifest = tomllib.loads(manifest_bytes.decode("utf-8"))
    require(uint(manifest.get("schema"), "manifest schema", 0xffffffff) <= 1, "newer manifest schema")
    require(manifest.get("machine_id") == signer, "appcast signer attribution mismatch")
    require(uint(manifest.get("roster_seq"), "appcast roster sequence") <= seq,
            "appcast requires newer roster")
    require(manifest.get("version") == tag[1:], "release tag/appcast version mismatch")
    require(re.fullmatch(r"[0-9a-f]{40}", manifest.get("commit", "")), "invalid source commit")
    require(isinstance(manifest.get("sha256"), str), "appcast has no dmg digest")
    dmg = f"aterm-{tag[1:]}.dmg"
    require(manifest.get("dmg") == dmg and manifest.get("url") ==
            f"https://github.com/{slug}/releases/download/{tag}/{dmg}", "signed release origin mismatch")
    build = uint(manifest.get("build_number"), "build number")
    require(build > 0 and build > rejected_build, "zero or previously rejected release build")
    minimum = uint(manifest.get("min_build", 0), "minimum build")
    floor = withdrawn_floor(floors[1], min_build_sources, set(revoked) | set(revoked_machines))
    require(build >= max(floors[0], floor, minimum), "release build rollback or below apply floor")
    for target in ("x86_64", "aarch64"):
        field = "linux_" + target
        present = [field + suffix in manifest for suffix in ("", "_sha256", "_size")]
        require(not any(present) or all(present), "incomplete Linux artifact triple")
        if not any(present):
            continue
        require(manifest[field] == f"aterm-{tag[1:]}-linux-{target}", "noncanonical Linux asset")
        require(isinstance(manifest[field + "_sha256"], str) and
                re.fullmatch(r"[0-9a-f]{64}", manifest[field + "_sha256"]), "invalid Linux digest")
        size = uint(manifest[field + "_size"], "Linux size", MAX_BINARY)
        require(size >= 64, "Linux artifact too small")
    if arch is None:
        return manifest
    field = "linux_" + arch
    require(field in manifest, "signed release has no Linux artifact for " + arch)
    return (manifest[field], manifest[field + "_sha256"], manifest[field + "_size"], build, seq)


def verify_binary(path, arch, digest, size):
    meta = pathlib.Path(path).lstat()
    require(stat.S_ISREG(meta.st_mode) and meta.st_size == size, "Linux artifact size/type mismatch")
    with open(path, "rb") as stream:
        header = stream.read(64)
        require(len(header) == 64 and header[:7] == b"\x7fELF\x02\x01\x01", "not a 64-bit little-endian ELF")
        require(int.from_bytes(header[16:18], "little") in (2, 3), "ELF is not an executable")
        require(int.from_bytes(header[20:24], "little") == 1 and
                int.from_bytes(header[52:54], "little") == 64, "invalid ELF version/header size")
        require(int.from_bytes(header[18:20], "little") == {"x86_64": 62, "aarch64": 183}[arch],
                "Linux artifact ELF architecture mismatch")
        stream.seek(0)
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    require(actual == digest, "Linux artifact SHA-256 mismatch")


def main(args):
    if args[:1] == ["--store-path"]:
        require(len(args) == 2, "invalid Linux verifier invocation")
        preflight_store(args[1])
        return
    require(len(args) in (5, 6), "invalid Linux verifier invocation")
    directory, tag, slug, arch, store = args[:5]
    policy = read_install_policy(store)
    head_path = pathlib.Path(directory) / "aterm-policy-appcast.toml"
    head_sig = head_path.with_name(head_path.name + ".sig")
    if head_path.exists() or head_sig.exists():
        require(head_path.exists() and head_sig.exists(), "incomplete latest-policy proof pair")
        # The version here is only a candidate parser input; the complete chain
        # and signed origin are checked before any policy value is consumed.
        head = tomllib.loads(read_bounded(head_path, 5_000_000).decode("utf-8"))
        head = verify_manifest(directory, "v" + str(head.get("version", "")), slug,
                               manifest_name="aterm-policy-appcast.toml", **policy)
        require(tuple(map(int, head["version"].split("."))) >= tuple(map(int, tag[1:].split("."))),
                "native candidate is newer than authenticated current APP head")
        roster = tomllib.loads(read_bounded(pathlib.Path(directory) / "aterm-machines.toml", 65536).decode("utf-8"))
        policy["revoked_machines"] = tuple(set(policy["revoked_machines"]) | set(roster.get("revoked", [])))
        # The durable floor less what revoked machines raised, then the head's own.
        floor = withdrawn_floor(policy["floors"][1], policy.get("min_build_sources"),
                                set(policy["revoked_machines"]))
        policy["floors"] = (policy["floors"][0], max(floor, head.get("min_build", 0)),
                            max(policy["floors"][2], roster["roster_seq"]))
        policy["min_build_sources"] = None
    result = verify_manifest(directory, tag, slug, arch, **policy)
    if len(args) == 6:
        verify_binary(args[5], arch, result[1], result[2])
    print("\t".join(map(str, result)))


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print("install.sh: Linux verification refused: " + str(error), file=sys.stderr)
        sys.exit(1)
ATERM_LINUX_VERIFY_PY
}

# The verifier's first check, the store path's ancestry, is an environment
# fact: judged before any download, with the verifier's remedy, rather than as
# an abort after the proofs. Sets LINUX_STORE_SKIP (empty when the verifier
# admits <store>, else the reason the half is skipped) and returns 1 on a skip.
# Called directly, never in $(…), so the global reaches the caller.
LINUX_STORE_SKIP=""
linux_store_preflight() { # <store>
	local why
	LINUX_STORE_SKIP=""
	why="$(linux_release_verify --store-path "$1" 2>&1 >/dev/null)" && return 0
	LINUX_STORE_SKIP="another account could replace $1/aterm: ${why#install.sh: Linux verification refused: }"
	return 1
}

# --- ownership: one set of tests for both directions ---------------------------
#
# The installer replaces, and the uninstaller deletes, only what is aterm's.
# These are the uninstaller's own tests, shared so the install path asks the
# same questions of $DEST/aterm.app and $BIN_DIR/aterm and prints the same
# SKIPPED line (docs/AUDIT-distribution-channel-2026-08-31.md). Defined above
# the library guard so crates/aterm-update/tests/install_sh pins them.

# The one SKIPPED line both directions print for an entry that is not ours.
ownership_skip() { # <path> <reason>
	echo "install.sh: SKIPPED $1 — $2" >&2
}

# The bundle's CFBundleIdentifier, or nothing when it has none readable.
bundle_identifier() { # <app-dir>
	local plist="$1/Contents/Info.plist"
	[[ -f "$plist" ]] || return 0
	defaults read "$plist" CFBundleIdentifier 2>/dev/null || true
}

# Whether <app-dir> is an aterm bundle — the only kind either direction touches.
bundle_is_ours() { # <app-dir>
	[[ "$(bundle_identifier "$1")" == "com.aterm.aterm" ]]
}

# What may happen at the app destination <app-dir> (normally $DEST/aterm.app),
# one word (then a reason) — the install path's question, as bin_aterm_verdict
# is the bin entry's:
#   absent   nothing there — install
#   ours     an aterm bundle (bundle_is_ours) — set aside and replaced
#   damaged  a real bundle directory with no readable CFBundleIdentifier whose
#            own Contents/MacOS/aterm answers `--version` as aterm (run bounded
#            and stdin-less): an interrupted copy or a lost Info.plist. It is
#            aterm's, so a re-run repairs it — set aside and replaced, and said
#            so. (The uninstaller stays on bundle_is_ours: deleting asks more.)
#   foreign <reason>  anything else — left in place; the reason names the
#            remedy when the bundle might be a damaged aterm that cannot answer
app_dest_verdict() { # <app-dir>
	local app="$1" id exe
	if [[ ! -e "$app" && ! -L "$app" ]]; then
		echo absent
		return 0
	fi
	if bundle_is_ours "$app"; then
		echo ours
		return 0
	fi
	id="$(bundle_identifier "$app")"
	if [[ -n "$id" ]]; then
		echo "foreign not an aterm bundle (CFBundleIdentifier=$id)"
		return 0
	fi
	exe="$app/Contents/MacOS/aterm"
	if [[ -d "$app" && ! -L "$app" && -f "$exe" && -x "$exe" ]] &&
		first_line_bounded "$exe" --version | grep -q '^aterm '; then
		echo "damaged no readable CFBundleIdentifier, but its binary answers as aterm"
		return 0
	fi
	echo "foreign no readable CFBundleIdentifier and no binary that answers as aterm — if it is a damaged aterm install, move it aside (mv '$app' '$app.broken') and re-run"
}

# Whether a root run must not install into folder <dest>: it carries an ACL
# (entry_without_acl), which may let an account other than root change what
# is in it. The swap leaves <dest>/aterm.app free for a moment, and an
# account that may add an entry there could make a folder of its own at that
# name, which `mv` would then move the app into, and one that may remove
# entries could take the app away. Its owner and the groups that can write it
# are another matter: they could replace the app in any case, and
# /Applications is admin's to write on purpose (own_bundle_as_root). A <dest>
# that is a link is judged by the folder it leads to, which the staging and
# the swap write. A <dest> a dry run has not made yet is judged as `mkdir -p`
# would make it: by the entries its existing_anchor passes on (acl_passed_on).
# Whether, under root on macOS, the volume <dest> is on (while it does not
# exist, the one mkdir -p makes it on) ignores ownership: `mount` lists it
# noowners, as for an exFAT or FAT volume and an external one with "Ignore
# ownership on this volume" on. Every entry there reads as the mount's owner,
# and an exFAT or FAT one keeps no ACL either, so stage_bundle cannot make its
# staging folder root's alone, and would refuse only after the download. A
# volume df or the mount table does not name is not judged, nor is any while
# that table is not read in time (macos_mounts). A Ctrl-C, hang-up or TERM
# while it is read ends the run (bounded).
root_dest_keeps_no_owner() { # <dest>
	[[ "$(installer_uid)" == 0 && "$(uname -s)" == Darwin ]] || return 1
	local dev table
	dev="$(df -P -- "$(existing_anchor "$1")" 2>/dev/null | awk 'NR == 2 { print $1 }')"
	[[ -n "$dev" ]] || return 1
	table="$(macos_mounts 2>/dev/null)" || return 1
	awk -v d="$dev on " 'index($0, d) == 1 && /[(,] *noowners *[,)]/ { found = 1 } END { exit !found }' <<<"$table"
}

root_dest_acl_open() { # <dest>
	[[ "$(installer_uid)" == 0 ]] || return 1
	if [[ "${DRY_RUN:-0}" -eq 1 && ! -e "$1" ]]; then
		local dest="${1%"${1##*[!/]}"}" parent
		parent="${dest%/*}"
		[[ "$dest" == */* ]] || parent=.
		if [[ -e "${parent:-/}" ]]; then
			acl_passed_on "$(existing_anchor "$dest")" 1
		else
			acl_passed_on "$(existing_anchor "$dest")" 2
		fi
		return
	fi
	! entry_without_acl "$1/"
}

# Whether a folder `mkdir -p` makes <depth> levels below folder <dir> starts
# with an ACL: it inherits each entry of <dir>'s that folders inherit
# (directory_inherit), but one that reaches only <dir>'s own children
# (limit_inherit) when <depth> is more than 1. macOS's ls lists these; the app
# half that asks installs on macOS alone. A listing that cannot be read
# proves nothing, so it counts as one that passes an entry on.
acl_passed_on() { # <dir> <depth>
	local listing
	listing="$(ls_entry -ndle -- "$1/" 2>/dev/null)" || return 0
	[[ -n "$listing" ]] || return 0
	printf '%s\n' "$listing" | awk -v deep="$(($2 > 1))" '
		/^[[:blank:]]+[0-9]+:/ {
			n = split($NF, flag, ",")
			inherits = limited = 0
			for (i = 1; i <= n; i++) {
				if (flag[i] == "directory_inherit") inherits = 1
				if (flag[i] == "limit_inherit") limited = 1
			}
			if (inherits && !(deep && limited)) found = 1
		}
		END { exit !found }'
}

# Whether <bundle> and everything in it is root's, with no file in it that a
# group other than root's can write (root_entry), and nothing every account
# can write or that carries an ACL.
bundle_is_roots() { # <bundle>
	bundle_is_only 0 "$1"
}

# Whether <bundle> and everything in it is <uid>'s, with nothing in it (a link
# aside) that every account can write, no file a group other than gid 0 can
# write, and, on macOS, no entry with an ACL. own_bundle_as_root strips every
# ACL, so any ACL there is one no root run left, and one entry can give
# write, append, delete or a chmod (writesecurity) to anyone whatever the
# modes say: telling a harmless one from those is not this check's to do.
bundle_is_only() { # <uid> <bundle>
	[[ -d "$2" && -z "$(find "$2" \( ! -user "$1" -o \( ! -type l -perm -002 \) -o \
		\( -type f -perm -020 ! -group 0 \) \) -print 2>/dev/null | head -n 1)" ]] || return 1
	# `ls -le` prints each entry's ACL as ` <n>: <entry>` lines below it.
	[[ "$(uname -s)" == Linux ]] ||
		ls_entry -leRA -- "$2" 2>/dev/null | awk '/^ [0-9]+: / { n = 1 } END { exit n }'
}

# Make the staged bundle <stage> a root run's: root's, and as writable as
# <dest> and no more. Under root, ditto keeps the owner the container recorded
# (the release builder's, 501:staff), which on this Mac may be any account or
# none, and that account could change the app every account and root run. Its
# folders take <dest>'s group and, when that group can write <dest>, its write
# (/Applications: admin, as an admin's updater renames and removes the bundle);
# its files stay root's alone to write, so no group changes what root runs. No
# entry keeps an ACL, a link's own among them, which `chmod -R` does not reach:
# one the archive carries would grant what the modes do not. A <dest> that is
# a link gives the group and write of the folder it leads to.
own_bundle_as_root() { # <stage> <dest>
	local mode links uid gid rest
	read -r mode links uid gid rest <<<"$(ls_entry -nd -- "$2/")"
	chown -R "0:${gid:-0}" "$1" && chmod -R go-w "$1" || return 1
	[[ "$(uname -s)" == Linux ]] || { chmod -R -N "$1" && find "$1" -type l -exec chmod -h -N {} +; } || return 1
	[[ "${mode:5:1}" != w ]] || find "$1" -type d -exec chmod g+w {} +
}

# Copy the verified bundle <app> into <dest> as STAGE, the copy the swap
# renames into place. As this account, under the visible, unpredictable name
# install_app's swap explains: what ditto makes is this account's, which is
# whose the app is. Under root, ditto keeps the container's owner
# (own_bundle_as_root), so for the length of the copy every folder of the
# copy is that account's, and it could change the app before root makes it
# root's: the copy is made inside STAGE_DIR, a folder made in <dest> under a
# random name (temp_dir), and made root's there. An inheritable ACL entry on
# <dest> (SIP keeps one off /Applications, not off another folder;
# root_dest_acl_open refuses a <dest> with one, so this is for one added
# since) passes down to STAGE_DIR,
# and until it is stripped an account it names may enter, replace, chmod or
# take the folder, or hold a descriptor that later looks up names in it
# without the search right. So STAGE_DIR loses its ACL (with -h, never
# through a link put in its place) and must then be staging_folder_sealed's,
# and the copy goes into a second folder mktemp makes in it: made after the
# strip, that one inherits nothing, and no descriptor on STAGE_DIR can search
# it. Status 1 when any of it fails; drop_stage removes what this made.
stage_bundle() { # <app> <dest>
	local d
	STAGE=""
	STAGE_DIR=""
	if [[ "$(installer_uid)" != 0 ]]; then
		STAGE="$2/aterm.app.installing.$(random_suffix)"
		ditto "$1" "$STAGE"
		return
	fi
	temp_dir STAGE_DIR "$2" aterm.app.installing || return 1
	[[ "$(uname -s)" == Linux ]] || chmod -h -N "$STAGE_DIR" || return 1
	staging_folder_sealed "$STAGE_DIR" || return 1
	d="$(mktemp -d "$STAGE_DIR/copy.XXXXXX")" || return 1
	STAGE="$d/aterm.app"
	ditto "$1" "$STAGE" && own_bundle_as_root "$STAGE" "$2"
}

# Whether <dir>, a root run's staging folder, is root's alone with mode 0700,
# holds no entry, and on macOS carries no ACL. `ls -l` shows a `+` for an ACL
# only on an entry with no extended attribute, so the ACL is read from the
# entries `ls -le` lists, and an entry named with a newline still prints a
# path for find.
staging_folder_sealed() { # <dir>
	local mode links uid rest
	read -r mode links uid rest <<<"$(ls_entry -nld -- "$1" 2>/dev/null)"
	[[ "${mode:0:10}" == drwx------ && "$uid" == 0 ]] || return 1
	[[ -z "$(find "$1" -mindepth 1 -maxdepth 1 -print 2>/dev/null | head -n 1)" ]] || return 1
	[[ "$(uname -s)" == Linux ]] ||
		ls_entry -lde -- "$1" 2>/dev/null | awk '/^ [0-9]+: / { n = 1 } END { exit n }'
}

# Rename STAGE onto <dest>/aterm.app, the bundle there set aside first (OLD)
# and removed last, and remove STAGE_DIR, which holds nothing of the copy
# once it is renamed. A bundle there that may not be replaced (may_replace)
# stays, and so does STAGE, for the caller to drop.
swap_staged_bundle() { # <dest>
	if [[ -e "$1/aterm.app" ]]; then
		# Asked again here: another run may have placed it since install_app asked.
		may_replace "$1/aterm.app" || return 1
		OLD="$1/.aterm.app.old.$(random_suffix)"
		mv "$1/aterm.app" "$OLD"
	fi
	mv "$STAGE" "$1/aterm.app"
	STAGE=""
	if [[ -n "${STAGE_DIR:-}" ]]; then
		rm -rf "$STAGE_DIR"
		STAGE_DIR=""
	fi
	if [[ -n "${OLD:-}" ]]; then
		rm -rf "$OLD" 2>/dev/null ||
			echo "install.sh: NOTE: could not fully remove the previous copy (left at $OLD; remove with: $(note_command --sudo rm -rf "$OLD"))" >&2
		OLD=""
	fi
}

# Remove what stage_bundle made and the swap did not take: an interrupted or
# failed run's copy, and under root the folder it was made in.
drop_stage() {
	[[ -z "${STAGE:-}" ]] || rm -rf "$STAGE"
	[[ -z "${STAGE_DIR:-}" ]] || rm -rf "$STAGE_DIR"
	STAGE=""
	STAGE_DIR=""
}

# A command a NOTE names for the person to paste: each word spelled to read
# back as one (shell_word), after `sudo` when acting on <path> needs it, in a
# root run or a folder (or a file the command rewrites) this account cannot
# write, or always with --sudo, for a command this account just ran and
# failed. A cp back over a file asks of the file: asked of its folder, which
# need take no new name, the NOTE said sudo to one this account can write.
note_command() { # <path>|--sudo <word>...
	local out="" word
	[[ "$1" != --sudo && "$(installer_uid)" != 0 && -w "$1" ]] || out="sudo "
	shift
	for word in "$@"; do
		out+="$(shell_word "$word") "
	done
	printf '%s\n' "${out% }"
}

# TERM and HUP to run_cut, from the container step (unpack_container) to the
# end of the swap (place_bundle). A TERM or a HUP sent to this shell alone (a
# `kill` of its pid, a supervisor) ends it through bash's own handler at once,
# which ran install_app_cleanup while ditto still expanded the zip into MNT or
# copied into STAGE, an attach still mounted the image, or a rename of the swap
# was in flight: what the step made after the cleanup stayed (in TMPDIR, or a
# partial copy in DEST), or the cleanup found no aterm.app between the
# renames, or restored the old one under the new. Held, bash ends the run once
# the step's child returns (a bounded check between them within its 60 s).
# Set after install_app's EXIT trap: set before it, die_of's self-kill skips
# that trap (bash 3.2 and 5.3). The toolset seed after the swap runs for
# minutes, so release_kills drops them once the swap is done.
hold_kills() {
	trap 'run_cut TERM 143' TERM
	trap 'run_cut HUP 129' HUP
}

release_kills() {
	trap - TERM HUP
}

# Expand the zip, or mount the DMG, <asset> at <mnt>, TERM and HUP held from
# here (hold_kills).
unpack_container() { # <asset> <mnt>
	hold_kills
	if [[ "$CONTAINER_KIND" == zip ]]; then
		mkdir -p "$2"
		ditto -x -k "$1" "$2" || {
			echo "install.sh: could not expand ${1##*/}" >&2
			exit 1
		}
	else
		hdiutil attach "$1" -nobrowse -readonly -mountpoint "$2" -quiet
	fi
}

# Stage <bundle> next to <dest> and swap it in, TERM and HUP held (taken
# again here, for a caller that unpacked nothing) and then dropped.
place_bundle() { # <bundle> <dest>
	hold_kills
	stage_bundle "$1" "$2" || {
		echo "install.sh: could not stage aterm.app in $2 — nothing was installed" >&2
		exit 1
	}
	swap_staged_bundle "$2"
	release_kills
}

# install_app's EXIT trap, over the TMP, MNT, STAGE, STAGE_DIR and OLD it sets
# and the run's DEST and CONTAINER_KIND.
install_app_cleanup() {
	# Best-effort only, and never let a cleanup failure rewrite the exit status.
	set +e
	# A Ctrl-C, a hangup or a TERM while this runs must not cut it short, and its
	# children start with them ignored: before, a Ctrl-C here (a second one, or
	# the first after a refusal) ended the run at the step it killed, and left a
	# folder with no aterm.app, a staging copy or a set-aside old copy nothing
	# later removes. A run a Ctrl-C ended still dies of it once this returns.
	trap '' INT HUP TERM QUIT
	# If the swap was interrupted after the old bundle was set aside, restore it.
	if [[ -n "$OLD" && -d "$OLD" && ! -e "$DEST/aterm.app" ]]; then
		mv "$OLD" "$DEST/aterm.app"
	fi
	drop_stage
	# The set-aside copy goes only once an aterm.app is in place again.
	if [[ -n "$OLD" && -d "$OLD" ]]; then
		if [[ -e "$DEST/aterm.app" ]]; then
			rm -rf "$OLD"
		else
			echo "install.sh: NOTE: could not put the previous aterm.app back (it is at $OLD; restore it with: $(note_command "$DEST" mv "$OLD" "$DEST/aterm.app"))" >&2
		fi
	fi
	# Only a DMG is mounted; the lean zip is expanded into this same path, and
	# `rm -rf "$TMP"` below is all it needs. A detach can wedge, so it alone
	# takes a Ctrl-C, which cuts it and goes on to the -force fallback.
	local mounted=0
	if [[ "${CONTAINER_KIND:-dmg}" == dmg && -d "$MNT" ]]; then
		trap : INT HUP TERM QUIT
		hdiutil detach "$MNT" -quiet >/dev/null 2>&1 ||
			{ sleep 2; hdiutil detach "$MNT" -force -quiet >/dev/null 2>&1; }
		trap '' INT HUP TERM QUIT
		# A mount point with anything in it is the image still mounted: the
		# -force detach failed too, or a Ctrl-C cut it. rm would walk its
		# read-only tree, an error for each file, so it stays, and says how to
		# detach it (as the account that attached it, which owns TMP); the rest of
		# the download goes.
		if [[ -n "$(ls -A "$MNT" 2>/dev/null)" ]]; then
			mounted=1
			echo "install.sh: NOTE: the disk image is still mounted at $MNT (detach it with: $(note_command "$TMP" hdiutil detach -force "$MNT"))" >&2
			find "$TMP" -mindepth 1 -maxdepth 1 ! -name "${MNT##*/}" -exec rm -rf {} +
		fi
	fi
	[[ -z "$TMP" || "$mounted" == 1 ]] || rm -rf "$TMP"
	# A run that ends after install_app left the launch line to the opening
	# (APP_TO_OPEN) and before that opening ran still prints it.
	[[ -z "${APP_TO_OPEN:-}" ]] || echo "  launch:  open $(shell_word "$APP_TO_OPEN")"
}

# The value at <keypath> in <plist> as plutil prints it raw (an array's count),
# or nothing.
plist_raw() { # <plist> <keypath>
	plutil -extract "$2" raw -o - -- "$1" 2>/dev/null || true
}

# Whether a bin entry is a symlink this installer places: into an aterm.app
# bundle, or into the store.
bin_entry_is_ours() { # <path> <store-dir>
	[[ -L "$1" ]] || return 1
	case "$(readlink "$1" 2>/dev/null || true)" in
	*/aterm.app/Contents/MacOS/* | "$2"/*) return 0 ;;
	esac
	return 1
}

# What may happen at the `aterm` bin entry <path>, one word (then a reason):
#   absent     nothing there — write it
#   ours       our symlink (bundle or store) — re-point it
#   pre-store  a regular file that answers `--version` as aterm: a pre-store
#              install. The installer replaces it, and says so — it would shadow
#              the store. (The uninstaller still leaves it: removing a binary is
#              not the same act as superseding one.)
#   foreign <reason>  anything else — the installer leaves it and says SKIPPED
bin_aterm_verdict() { # <path> <store-dir>
	local p="$1"
	if [[ ! -e "$p" && ! -L "$p" ]]; then
		echo absent
	elif bin_entry_is_ours "$p" "$2"; then
		echo ours
	elif [[ -L "$p" ]]; then
		echo "foreign symlink points outside an aterm bundle or store ($(readlink "$p" 2>/dev/null || true))"
	elif [[ -f "$p" && -x "$p" ]] && ! may_run "$p"; then
		echo "foreign another account can change it, so root does not run it to ask what it is"
	elif [[ -f "$p" && -x "$p" ]] && first_line_bounded "$p" --version | grep -q '^aterm '; then
		echo pre-store
	else
		echo "foreign not a symlink — a hand-built binary this installer did not place"
	fi
}

# Whether this run may run <program>: a run as anyone but root may run any;
# root only one no other account can change, so the file and every folder on
# its way from /, as written and through each link it passes, are root's, and
# none has an ACL or lets another account write in it, sticky folders included.
# Over a HOME another account owns (a CI container's, a folder outside its
# owner's own home), root would otherwise run, as root, what that account left
# there (~/.local/bin/aterm, ~/Applications/aterm.app).
may_run() { # <program>
	[[ "$(installer_uid)" == 0 ]] || return 0
	local p="$1"
	[[ "$p" == /* ]] || p="$PWD/$p"
	p="$(sealed_end "$p")" && [[ -f "$p" ]] && sealed_entry "$p"
}

# Where <path> ends, by the way the kernel takes: each name looked up in turn
# from / (from the working folder, for a relative path), and each link met
# anywhere on the way, at most 40 in all, followed, the rest of the way then
# taken from where it leads. Each folder a name is looked up in must pass
# <test>, but those in <tree>, a physical folder the way to which and all it
# holds were already found root's, and the folders above it. On Linux a
# link's own owner and mode change nothing: no account rewrites a link in
# place, nor removes it but through its folder. On macOS its owner can give
# the link an ACL that lets them remove it, so there each link met must pass
# <test> too, but those in <tree>. Prints the end, which need not exist when
# the folder that would hold it does. Status 2, printing the first that fails
# <test>, or, once the way is taken, the first folder on it (or the end)
# way_moved names; status 3, printing nothing, when a 41st link stands on the
# way, every folder before it having passed and none moved, a way the kernel
# follows to no end (it stops at 40 links, macOS's at 32); status 1, printing
# nothing, when the way does not resolve: a name on it that does not exist
# with more of the way after it, since an account can be the one to make it
# (any account that attaches a disk image gets a folder made for it in
# /Volumes, as udisks makes one in /media), or a newline in <path>, in the
# working folder's (for a relative one) or in a link's target (a command
# substitution drops a trailing one, and with it the name the kernel takes).
way_end() { # <test> <path> [<tree>]
	local rest="$2" cur="" c t hops=0 seen=()
	[[ "$rest" != *$'\n'* ]] || return 1
	if [[ "$rest" != /* ]]; then
		c="$(pwd -P && printf x)" || return 1
		c="${c%$'\n'x}"
		[[ "$c" != *$'\n'* ]] || return 1
		rest="$c/$rest"
	fi
	if [[ -n "${3:-}" && "$rest" == "$3"/* ]]; then
		cur="$3"
		rest="${rest#"$3"/}"
	fi
	while [[ -n "$rest" ]]; do
		c="${rest%%/*}"
		if [[ "$rest" == */* ]]; then rest="${rest#*/}"; else rest=""; fi
		case "$c" in
		"" | .) continue ;;
		..)
			cur="${cur%/*}"
			continue
			;;
		esac
		way_open "${cur:-/}" || { printf '%s\n' "${cur:-/}"; return 2; }
		way_passes "$1" "${cur:-/}" "${3:-}" || { printf '%s\n' "${cur:-/}"; return 2; }
		cur="$cur/$c"
		if [[ -L "$cur" ]]; then
			[[ "$OSTYPE" != darwin* ]] || way_passes "$1" "$cur" "${3:-}" || {
				printf '%s\n' "$cur"
				return 2
			}
			hops=$((hops + 1))
			if [[ "$hops" -gt 40 ]]; then
				c="$(way_moved "${seen[@]}")"
				[[ -z "$c" ]] || { printf '%s\n' "$c"; return 2; }
				return 3
			fi
			t="$(readlink -n "$cur" && printf x)" || return 1
			t="${t%x}"
			[[ -n "$t" && "$t" != *$'\n'* ]] || return 1
			if [[ "$t" == /* ]]; then cur=""; else cur="${cur%/*}"; fi
			rest="$t${rest:+/$rest}"
		elif [[ ! -e "$cur" && -n "${rest//\//}" ]]; then
			return 1
		fi
	done
	if [[ -d "${cur:-/}" || -f "${cur:-/}" ]]; then
		way_open "${cur:-/}" || { printf '%s\n' "${cur:-/}"; return 2; }
	fi
	if [[ ${#seen[@]} -gt 0 ]]; then
		c="$(way_moved "${seen[@]}")"
		if [[ -n "$c" ]]; then
			printf '%s\n' "$c"
			return 2
		fi
	fi
	printf '%s\n' "${cur:-/}"
}

# way_end's record of <path>, the next folder on its way (or its end): unless
# its seen holds it already (a way of links can pass one again), added to
# seen and held open, the first on file descriptor 30 and each next one on the
# next, before it is judged; status 1 past 200 of them, or when it cannot be
# opened. way_end runs in a command substitution, which closes them.
way_open() { # <path>
	local f
	for f in ${seen[@]+"${seen[@]}"}; do
		[[ "$f" != "$1" ]] || return 0
	done
	[[ ${#seen[@]} -lt 200 ]] || return 1
	seen+=("$1")
	# bash 3.2 keeps a descriptor open that exec opens again: close it first.
	{ eval "exec $((29 + ${#seen[@]}))<&- $((29 + ${#seen[@]}))<\"\$1\""; } 2>/dev/null
}

# The first of <path>..., way_end's folders held open on descriptor 30 on in
# turn, that foreign_on_way names, or that is no longer the one held open: a
# volume on it when it was opened that is gone, or one put on it since, leaves
# another folder at its name. Nothing when there is none. A descriptor is
# compared to a new one, not to the name (same_held).
way_moved() { # <path>...
	local f n=30
	f="$(foreign_on_way "$@")"
	if [[ -n "$f" ]]; then
		printf '%s\n' "$f"
		return 0
	fi
	for f in "$@"; do
		{ exec 29<&- 29<"$f"; } 2>/dev/null && same_held "$n" 29 || {
			printf '%s\n' "$f"
			return 0
		}
		n=$((n + 1))
	done
}

# Whether descriptors <a> and <b> hold one file: their /dev/fd names stat
# alike (dev_fd_alike), or, as macOS's /dev/fd stat fails now and then while
# other processes stat theirs (about one walk in a hundred with a dozen
# walking at once on macOS 27), stat itself reads the device and inode each
# one holds, through its standard input.
same_held() { # <a> <b>
	local a b
	dev_fd_alike "$1" "$2" && return 0
	if [[ "$OSTYPE" == darwin* ]]; then
		a="$(stat -f %d:%i <&"$1")" && b="$(stat -f %d:%i <&"$2")" || return 1
	else
		a="$(stat -L -c %d:%i /dev/stdin <&"$1")" && b="$(stat -L -c %d:%i /dev/stdin <&"$2")" || return 1
	fi
	[[ -n "$a" && "$a" == "$b" ]]
}

# Whether the /dev/fd names of descriptors <a> and <b> stat alike. macOS's
# /dev/fd gives each the device of /dev and the file's own inode, so a name is
# never compared to one (a test replaces it).
dev_fd_alike() { # <a> <b>
	[[ /dev/fd/$1 -ef /dev/fd/$2 ]]
}

# The first of <path>... (way_end's folders and its end, each one's folder
# among those before it, or /, but for a tree way_end starts in) on a
# filesystem whose owners and modes do not say who may change what it holds,
# but for the one / is on, which root already runs this script from (a
# container's FUSE or virtio-fs root): on Linux one statfs reads as network
# or FUSE (remote_filesystem_type), where a server, or the daemon of whichever
# account mounted it, decides that and shows what owners it likes; on macOS
# the root of a volume (a device not its folder's) that is not from a disk
# only a local mount no account made names (macos_disk_local). Only what is on the way is looked at, and no mount's
# folder: on Linux no mount table is read, and on macOS a way that crosses no
# volume reads none. Nothing when there is none; when a look fails (a folder
# gone, a mount table that cannot be read), the first it fails on.
foreign_on_way() { # <path>...
	local f p i j d list IFS=$' \t\n'
	local -a ways=(/ "$@") ids types roots=() up
	ids=($(folder_ids "${ways[@]}" 2>/dev/null || :))
	if [[ ${#ids[@]} -ne $((2 * ${#ways[@]})) ]]; then
		look_failing folder_ids "$@"
		return 0
	fi
	if [[ "$(uname -s)" != Darwin ]]; then
		types=($(folder_types "${ways[@]}" 2>/dev/null || :))
		if [[ ${#types[@]} -ne ${#ways[@]} ]]; then
			look_failing folder_types "$@"
			return 0
		fi
		for ((i = 1; i < ${#ways[@]}; i++)); do
			if [[ "${ids[2 * i]}" != "${ids[0]}" ]] && remote_filesystem_type "${types[i]}"; then
				printf '%s\n' "${ways[i]}"
				return 0
			fi
		done
		return 0
	fi
	for ((i = 1; i < ${#ways[@]}; i++)); do
		f="${ways[i]}"
		[[ "$f" != / ]] || continue
		p="${f%/*}"
		p="${p:-/}"
		for ((j = 0; j < ${#ways[@]}; j++)); do
			[[ "${ways[j]}" != "$p" ]] || break
		done
		if [[ $j -lt ${#ways[@]} ]]; then
			d="${ids[2 * j]}"
		else
			# A tree way_end starts in, whose folder is not on the way: one
			# whose folder cannot be read is a volume's root.
			up=($(folder_ids "$p" 2>/dev/null || :))
			d="${up[0]:-}"
		fi
		[[ "${ids[2 * i]}" != "$d" ]] || continue
		roots+=("$i")
	done
	[[ ${#roots[@]} -gt 0 ]] || return 0
	i="${roots[0]}"
	if ! list="$(macos_mount_disks)"; then
		printf '%s\n' "${ways[i]}"
		return 0
	fi
	for i in "${roots[@]}"; do
		macos_disk_local "${ids[2 * i]}" "$list" && continue
		printf '%s\n' "${ways[i]}"
		return 0
	done
}

# Whether a volume of device <dev> is from a disk that a local mount no
# account made names and no other mount does, by the disks <list>
# (macos_mount_disks'). A volume an account attached or mounted is not, nor
# a network share's or a FUSE daemon's, which is no disk's, nor one put over
# a local volume, whose device is its own, nor one no line names (a line no
# parse reads hides nothing); a FUSE daemon that names itself after a disk
# makes that disk's volumes refuse, never pass. A graft (macOS's
# /System/Cryptexes/App/usr) shows the device of the volume it is from,
# Preboot's.
macos_disk_local() { # <dev> <list>
	local l there=""
	while IFS= read -r l; do
		[[ "${l#? }" == "$1" ]] || continue
		[[ "$l" == l* ]] || return 1
		there=1
	done <<<"$2"
	[[ -n "$there" ]]
}

# Each disk macOS has a volume mounted from, `<kind> <device>` on a line
# apiece: the device number of the /dev/disk node a mount line starts with
# (disk_devices), which a file on that volume shows, and kind `f` for a line
# macos_mount_foreign flags, `l` for any other; and the kernel's own devfs, by
# the device /dev shows when a line has it at /dev (a fixed folder, so a line
# forged to say so vouches for nothing else). A line that starts with no
# /dev/disk node (a network share's, a FUSE daemon's own name) gives none, so
# no server is asked, and nor, for any line that names it, does a node no
# stat reads (a disk image detached since the table was printed): each node
# is read once, so no line's refusal of a disk is lost while another line
# names it local, and a volume of that node, whose device no other node's is,
# refuses as one no line names. Status 1 when the mount table, or /dev for a
# devfs line, cannot be read.
macos_mount_disks() {
	local table l s i j d devfs="" IFS=$' \t\n'
	local -a kinds=() nodes=() devs
	table="$(macos_mounts 2>/dev/null)" && [[ -n "$table" ]] || return 1
	while IFS= read -r l; do
		s="${l%%" on "*}"
		if [[ "$l" == "devfs on /dev ("* ]]; then
			if macos_mount_foreign "${l##* (}"; then devfs+=f; else devfs+=l; fi
			continue
		fi
		[[ "$s" == /dev/disk[0-9]* && "${s#/dev/disk}" != *[!0-9s]* ]] || continue
		if macos_mount_foreign "${l##* (}"; then kinds+=(f); else kinds+=(l); fi
		nodes+=("$s")
	done <<<"$table"
	if [[ -n "$devfs" ]]; then
		devs=($(folder_ids /dev 2>/dev/null || :))
		[[ ${#devs[@]} -eq 2 ]] || return 1
		for ((i = 0; i < ${#devfs}; i++)); do
			printf '%s %s\n' "${devfs:i:1}" "${devs[0]}"
		done
	fi
	devs=()
	for ((i = 0; i < ${#nodes[@]}; i++)); do
		for ((j = 0; j < i; j++)); do
			[[ "${nodes[j]}" != "${nodes[i]}" ]] || break
		done
		if [[ $j -lt $i ]]; then
			d="${devs[j]}"
		else
			d="$(disk_devices "${nodes[i]}" 2>/dev/null)" || d=""
		fi
		devs[i]="$d"
		[[ -z "$d" ]] || printf '%s %s\n' "${kinds[i]}" "$d"
	done
}

# Each /dev/disk <node>'s device number on a line apiece (a test replaces
# it).
disk_devices() { # <node>...
	stat -f %r -- "$@"
}

# Each <path>'s device and inode, `<dev> <ino>` on a line apiece (a test
# replaces it).
folder_ids() { # <path>...
	if [[ "$OSTYPE" == darwin* ]]; then
		stat -f '%d %i' -- "$@"
	else
		stat -c '%d %i' -- "$@"
	fi
}

# Each <path>'s filesystem type as statfs reads it, in hex, on a line apiece:
# Linux's (a test replaces it).
folder_types() { # <path>...
	stat -f -c %t -- "$@"
}

# The first <path> that <look> fails on, else the first: what a look at them
# all that failed names.
look_failing() { # <look> <path>...
	local look="$1" f
	shift
	for f in "$@"; do
		"$look" "$f" >/dev/null 2>&1 || {
			printf '%s\n' "$f"
			return 0
		}
	done
	printf '%s\n' "$1"
}

# The first folder or file below folder <dir> where a mount
# foreign_mount_points lists is, its folder at or below <dir> (on macOS by
# identity at each level up, in_folder; on Linux by text, text_in_folder), a
# walk of what <dir> holds going into it (an automount there mounts once the
# walk enters <dir>): on Linux one statfs reads as network or FUSE, on macOS
# the root of a volume (a device not its folder's). A mount at <dir> itself is the end of the walk that
# judged it (foreign_on_way's), and a mount elsewhere is never looked at, but
# on macOS for its folder. <dir> when the mount table cannot be read; nothing
# when there is none.
foreign_mount_in() { # <dir>
	local m p list IFS=$' \t\n'
	local -a ids
	if ! list="$(foreign_mount_points)"; then
		printf '%s\n' "$1"
		return 0
	fi
	[[ -n "$list" ]] || return 0
	while IFS= read -r m; do
		[[ "$m" == /?* ]] || continue
		p="${m%/*}"
		if [[ "$(uname -s)" == Darwin ]]; then
			in_folder "${p:-/}" "$1" || continue
			# One that cannot be read is named: root may not stat a FUSE
			# volume mounted without allow_root.
			ids=($(folder_ids "$m" "${p:-/}" 2>/dev/null || :))
			[[ ${#ids[@]} -ne 4 || "${ids[0]}" != "${ids[2]}" ]] || continue
		else
			text_in_folder "${p:-/}" "$1" || continue
			remote_filesystem_type "$(folder_types "$m" 2>/dev/null)" || continue
		fi
		printf '%s\n' "$m"
		return 0
	done <<<"$list"
}

# Whether <a>, a folder as Linux's mount table spells it, is folder <b> or
# inside it, by text: the table and a folder cd -P reached spell a folder
# alike, but for the leading // such a cd keeps, so no folder is stat'd,
# which on a mount whose server stopped answering (an NFSv4 one's submount's
# folder) would hang.
text_in_folder() { # <a> <b>
	local b="$2"
	while [[ "$b" == //* ]]; do b="${b#/}"; done
	[[ "$b" == / || "$1" == "$b" || "$1" == "$b"/* ]]
}

# Each folder (or file) where a filesystem may be mounted whose owners and
# modes do not say who may change what it holds, one per line, read from the
# mount table alone: on macOS each that `mount` lists as not local, or as
# mounted by an account, which that account can take away and attach another
# in place of, and which shows root the owners its image carries; on Linux
# each linux_mounts lists, whose type statfs reads (foreign_mount_in). Status
# 1 when the mount table cannot be read.
foreign_mount_points() {
	local table l m
	if [[ "$(uname -s)" == Darwin ]]; then
		table="$(macos_mounts 2>/dev/null)" && [[ -n "$table" ]] || return 1
		while IFS= read -r l; do
			macos_mount_foreign "${l##* (}" || continue
			# `<device> on <folder> (<type>, <flag>, …)`, but a device's own
			# name (a FUSE one's, which its account picks) can hold ` on ` or
			# end in ` on`, and a folder can hold ` on `: each text up to the
			# line's last ` (` that starts with `/` after an ` on `, the
			# overlapping ones too.
			m="${l% (*}"
			while [[ "$m" == *" on "* ]]; do
				m="${m#*" on"}"
				[[ "$m" != " /"* ]] || printf '%s\n' "${m# }"
			done
		done <<<"$table"
	else
		linux_mounts 2>/dev/null || return 1
	fi
}

# Whether <flags>, a macOS mount line's text after its last ` (`, are a
# volume's that is not local or that an account mounted.
macos_mount_foreign() { # <flags>
	local flags=", ${1%)*},"
	[[ "$flags" != *", local,"* || "$flags" == *", mounted by "* ]]
}

# macOS's mounts, as mount_table lists them, or status 124 when it has not
# listed them within VERIFY_WAIT_SECS (what it printed by then answers
# nothing): /sbin/mount asks each mount's folder for its volume's
# capabilities, and one in a share whose server stopped answering never
# answers. Its lines reach the read through cat, inside the run bounded cuts
# off, so the read ends at the cut-off: a program in a file system's call that
# does not heed SIGKILL lives on until that call returns, but holds only its
# pipe to cat, which the SIGKILL ends (a test replaces it). No file holds
# them, so a full disk cannot cut the table short and a signal leaves nothing.
macos_mounts() {
	bounded "$VERIFY_WAIT_SECS" mount_table_relayed
}

# mount_table through cat, in one process group; under pipefail its status is
# mount_table's.
mount_table_relayed() {
	mount_table | cat
}

# The mount table, as /sbin/mount (by its fixed path, which PATH cannot
# shadow) prints it (a test replaces it).
mount_table() {
	/sbin/mount
}

# The folders (or files) Linux has a filesystem mounted at that could be a
# network or FUSE one, one per line, from the kernel's table (<mountinfo>, by
# default this process's): every mount but those of the device / is on, and
# those of a type that is a disk's, memory's or the kernel's own. A newline in
# a name stays \012, which names nothing. Status 1 for a table with no line.
linux_mounts() { # [<mountinfo>]
	awk 'BEGIN {
		n = split("autofs binfmt_misc bpf btrfs cgroup cgroup2 configfs debugfs devpts devtmpfs efivarfs erofs exfat ext2 ext3 ext4 f2fs fusectl hfs hfsplus hugetlbfs iso9660 jfs mqueue msdos nilfs2 nsfs ntfs ntfs3 overlay proc pstore ramfs reiserfs rpc_pipefs securityfs selinuxfs squashfs sysfs tmpfs tracefs udf vfat xfs zfs", w, " ")
		for (i = 1; i <= n; i++) own[w[i]] = 1
	}
	{
		p = $5
		gsub(/\\040/, " ", p)
		gsub(/\\011/, "\t", p)
		gsub(/\\134/, "\\", p)
		for (i = 7; i <= NF && $i != "-"; i++) continue
		dev[NR] = $3
		type[NR] = $(i + 1)
		point[NR] = p
		if (p == "/") root = $3
	}
	END {
		if (NR == 0) exit 1
		for (i = 1; i <= NR; i++) if (dev[i] != root && !(type[i] in own)) print point[i]
	}' "${1:-/proc/self/mountinfo}"
}

# Whether statfs type <hex> (stat -f -c %t's) is a network or FUSE
# filesystem's: NFS, SMB/CIFS (two), Ceph, AFS (two), 9P, Lustre, GPFS and
# FUSE, the types crates/aterm-uds/src/shared_write.rs REMOTE_FILESYSTEMS
# lists (on one of them the updater and the Linux verifier take a folder its
# group can write for one any account in that group may change); one stat
# could not read counts as such.
remote_filesystem_type() { # <hex>
	case "$1" in
	6969 | ff534d42 | fe534d42 | c36400 | 6b414653 | 5346414f | 1021997 | bd00bd0 | 47504653 | 65735546) return 0 ;;
	"" | *[!0-9a-f]*) return 0 ;;
	esac
	return 1
}

# Whether <folder>, on a way way_end takes, passes <test>, as one in <tree>,
# or a folder above it, does.
way_passes() { # <test> <folder> <tree>
	[[ -n "$3" && ( "$2" == "$3" || "$2" == "$3"/* || "$2" == / || "$3" == "$2"/* ) ]] ||
		"$1" "$2"
}

# Where <path> ends (way_end's), when no account but root can change the way
# there: each folder on it passes sealed_entry. Status 3 for a way of more
# than 40 links through folders that passed (way_end's), else 1, and nothing
# printed, otherwise.
sealed_end() { # <path> [<tree>]
	local e s=0
	e="$(way_end sealed_entry "$1" "${2:-}")" || s=$?
	case "$s" in
	0) printf '%s\n' "$e" ;;
	3) return 3 ;;
	*) return 1 ;;
	esac
}

# The first folder on the way to folder <dir> (way_end's), or where that way
# ends, that fails <test>, or <dir> when the way does not resolve;
# nothing when each one passes.
folder_failing() { # <test> <dir>
	local o s=0
	o="$(way_end "$1" "${2:-/}")" || s=$?
	case "$s" in
	0) "$1" "$o" || printf '%s\n' "$o" ;;
	2) printf '%s\n' "$o" ;;
	*) printf '%s\n' "${2:-/}" ;;
	esac
}

# folder_failing's, for a folder <dir> root may yet make: while it does not
# exist, the folder that would hold it is the end that must pass (way_end
# judged it), so a name missing with more of the way after it, which an
# account can be the one to make, fails as <dir>; a link that leads nowhere
# at its name fails as where it leads, where `mkdir -p` makes nothing.
to_make_failing() { # <test> <dir>
	local o s=0
	o="$(way_end "$1" "${2:-/}")" || s=$?
	case "$s" in
	0) [[ ! -e "$o" && ! -L "${2:-/}" ]] || "$1" "$o" || printf '%s\n' "$o" ;;
	2) printf '%s\n' "$o" ;;
	*) printf '%s\n' "${2:-/}" ;;
	esac
}

# Whether <path> is root's and, unless it is a link, no account but root can
# add to it or rewrite it: no group but root's can write it, folder or file,
# and no account can through a sticky bit, which lets every account add an
# entry (a .cargo/ among them) to a folder. A source build under root reads
# what such folders hold for minutes, so root_entry's room for a folder a
# group can write is not left here.
sealed_entry() { # <path>
	local mode links uid gid rest
	read -r mode links uid gid rest <<<"$(ls_entry -nd -- "$1" 2>/dev/null)"
	[[ "$uid" == 0 && ( "${mode:0:1}" == l || ( "${mode:8:1}" != w && ( "${mode:5:1}" != w || "$gid" == 0 ) ) ) ]] &&
		entry_without_acl "$1"
}

# Read BSD ACL metadata successfully, then refuse every extended ACL: its
# write, delete or security rights need not appear in the mode bits. ls lists
# the ACEs even when an extended attribute hides the mode's `+`. No output
# is no proof.
acl_listing_clear() { # <ls-arg>...
	local listing
	listing="$(ls_entry "$@" 2>/dev/null)" || return 1
	[[ -n "$listing" ]] || return 1
	printf '%s\n' "$listing" | awk '
		/^[[:blank:]]+[0-9]+:/ { bad = 1 }
		/^[-bcdlps]/ && length($1) >= 10 && substr($1, 11) ~ /[+?]/ { bad = 1 }
		END { exit bad }'
}

entry_without_acl() { # <path>
	if [[ "$(uname -s)" == Darwin ]]; then
		acl_listing_clear -ndle -- "$1"
	else
		linux_acl_clear -cpn -- "$1"
	fi
}

# A mode-only ls may have been built without ACL support. A successful
# getfacl query supplies the actual access and default ACLs; if no fixed OS
# query tool exists, root has no authority proof. POSIXLY_CORRECT otherwise
# hides default ACLs, so it is removed for this one read.
linux_acl_query() { # <getfacl-arg>...
	local tool
	tool="$(linux_acl_tool)" || return 1
	env -u POSIXLY_CORRECT "$tool" "$@"
}

# The fixed OS folders' getfacl, the first of linux_acl_tools that runs.
linux_acl_tool() {
	local tool
	while IFS= read -r tool; do
		[[ -x "$tool" ]] || continue
		printf '%s\n' "$tool"
		return 0
	done <<<"$(linux_acl_tools)"
	return 1
}

# Where a Linux system keeps getfacl (the acl package's), in the order root
# asks (a test replaces it).
linux_acl_tools() {
	printf '%s\n' /usr/bin/getfacl /bin/getfacl
}

linux_acl_clear() { # <getfacl-arg>...
	local listing
	listing="$(linux_acl_query "$@" 2>/dev/null)" || return 1
	[[ -n "$listing" ]] || return 1
	printf '%s\n' "$listing" | awk '
		/^user::[r-][w-][x-]$/ { users++; next }
		/^group::[r-][w-][x-]$/ { groups++; next }
		/^other::[r-][w-][x-]$/ { others++; next }
		/^# flags: [s-][s-][t-]$/ || /^[[:space:]]*$/ { next }
		{ bad = 1 }
		END { exit (bad || !users || users != groups || users != others) }'
}

# The first entry at or below folder <dir> (a physical path) that another
# account could change: one that is not root's (links on Linux aside), or a
# non-link that every account or a group but root's can write, or <dir> when an
# ACL is on any or the ACLs cannot be read whole, or a folder in it with a filesystem mounted
# whose owners say nothing (foreign_mount_in's); nothing when there is none.
# A name with a newline, which would read as two paths, is named by the folder
# that holds it.
root_entries_offender() { # <dir>
	local f link_owner=-false
	[[ "$OSTYPE" != darwin* ]] || link_owner=-true
	f="$(find "$1" \( -name "*"$'\n'"*" -prune -exec dirname {} \; \) -o \
		\( \( \( ! -type l -o "$link_owner" \) ! -user 0 \) -o \
		\( ! -type l \( -perm -002 -o \( -perm -020 ! -group 0 \) \) \) \) -print 2>/dev/null |
		head -n 1)"
	if [[ -n "$f" ]]; then
		printf '%s\n' "$f"
		return 0
	fi
	# The mode walk does not see an ACL's authority. Read the complete tree,
	# including dot entries, and name its root if the metadata is not ACL-free.
	if [[ "$(uname -s)" == Darwin ]]; then
		acl_listing_clear -lneRA -- "$1"
	else
		linux_acl_clear -RcpnP -- "$1"
	fi || {
		printf '%s\n' "$1"
		return 0
	}
	foreign_mount_in "$1"
}

# root_entries_offender's of folder <dir> (a physical path), else the first
# link in it that leads anywhere but into <dir>, or nowhere, or on its way
# passes a folder sealed_end refuses; nothing when there is none.
root_tree_offender() { # <dir>
	local f l t
	f="$(root_entries_offender "$1")"
	if [[ -n "$f" ]]; then
		printf '%s\n' "$f"
		return 0
	fi
	while IFS= read -r l; do
		[[ -n "$l" ]] || continue
		t="$(sealed_end "$l" "$1")" || t=""
		if [[ -d "$t" ]]; then
			t="$(cd -P -- "$t" 2>/dev/null && pwd)" || t=""
		elif [[ -e "$t" ]]; then
			t="$(cd -P -- "${t%/*}/" 2>/dev/null && pwd)" || t=""
		else
			t=""
		fi
		[[ -n "$t" ]] && in_folder "$t" "$1" && continue
		printf '%s\n' "$l"
		return 0
	done <<<"$(find "$1" \( -name "*"$'\n'"*" -prune \) -o \( -type l -print \) 2>/dev/null)"
}

# What a root source build asks of <path>, a folder it reads (or, while it
# does not exist, would make): the first folder on its way from / that is not
# sealed_entry's (to_make_failing's), else the first root_tree_offender in
# it; nothing when there is none.
root_tree_failing() { # <path>
	local p o
	o="$(to_make_failing sealed_entry "$1")"
	if [[ -n "$o" || ! -e "$1" ]]; then
		[[ -z "$o" ]] || printf '%s\n' "$o"
		return 0
	fi
	p="$(cd -P -- "$1" 2>/dev/null && pwd)" || p=""
	if [[ -z "$p" ]]; then
		printf '%s\n' "$1"
		return 0
	fi
	root_tree_offender "$p"
}

# Whether root may take programs from folder <dir> by name, as a build takes
# cc, ar, ld or git from PATH: one of ROOT_SYSTEM_PATH's, which root already
# runs its own tools from (macOS's sealed /usr/bin and /usr/sbin hold files of
# system accounts, _uucp's cu and uucico, that no account can change on that
# read-only volume, and a build without them has no cc or git; their cc, ld
# and git run the developer folder's, which root_xcode_judged asks of); else
# sealed ancestry prevents replacing an entry after this walk, files have
# root's modes and no ACL, no filesystem foreign_mount_in names is mounted in
# it (on Linux one can be on a file, and shows find its own owners), and
# every link, whatever it leads to now, passes
# sealed_end on its way to where it ends, a file there sealed_entry's (a link
# that leads to nothing yet in a folder another account can write would run
# what that account puts there); a link of more than 40 hops that only root
# can change, which leads the kernel nowhere, is passed over, so the walks of
# the files follow no link (GNU and BusyBox find -L fail on such a loop).
root_bin_dir() { # <dir>
	local found links link s
	case ":$ROOT_SYSTEM_PATH:" in *":${1%"${1##*[!/]}"}:"*) return 0 ;; esac
	[[ "$1" == /* && -z "$(folder_failing sealed_entry "$1")" ]] || return 1
	found="$(find "$1/" -mindepth 1 -maxdepth 1 \( -name "*"$'\n'"*" -print -prune \) -o \
		\( -type f \( ! -user 0 -o -perm -002 -o \( -perm -020 ! -group 0 \) \) -print \) 2>/dev/null)" || return 1
	[[ -z "$found" ]] || return 1
	if [[ "$(uname -s)" == Darwin ]]; then
		acl_listing_clear -lneAH -- "$1"
	else
		# A trailing slash resolves an admitted /bin-style directory alias;
		# -P still avoids following links found inside the directory.
		linux_acl_clear -RcpnP -- "$1/"
	fi || return 1
	[[ -z "$(foreign_mount_in "$1")" ]] || return 1
	links="$(find "$1/" -mindepth 1 -maxdepth 1 -type l -print 2>/dev/null)" || return 1
	while IFS= read -r link; do
		[[ -n "$link" ]] || continue
		s=0
		link="$(sealed_end "$link")" || s=$?
		[[ "$s" -ne 3 ]] || continue
		[[ "$s" -eq 0 ]] || return 1
		[[ ! -f "$link" ]] || sealed_entry "$link" || return 1
	done <<<"$links"
}

# PATH's folders that root_bin_dir allows, in PATH's order: the PATH a root
# source build runs on after the folders its lane puts first.
root_build_path() {
	local d out="" dirs
	IFS=: read -ra dirs <<<"${ROOT_LOOKUP_PATH-${PATH:-}}"
	for d in ${dirs[@]+"${dirs[@]}"}; do
		root_bin_dir "$d" && out+="${out:+:}$d"
	done
	printf '%s\n' "$out"
}

# Whether <path> itself is root's and, unless it is a sticky folder or a link,
# not one every account can write, nor, when it is a file, one a group other
# than root's can write (that group could rewrite it in place; a folder a group
# can write lets it move entries, but what it moves in is either not root's or
# a root file it brought from elsewhere on the volume, renamed from a folder it
# can write or hard-linked from any folder, which macOS does not stop, and it
# still cannot rewrite that file: root then runs a program root's own file
# holds, though not always the one it named). A
# link's mode is nothing (Linux shows every link as lrwxrwxrwx, /bin among
# them): no account rewrites a link in place, only through its folder, and the
# walks judge that folder and where it leads.
root_entry() { # <path>
	local mode links uid gid rest
	read -r mode links uid gid rest <<<"$(ls_entry -nd -- "$1" 2>/dev/null)"
	[[ "$uid" == 0 && ( "${mode:0:1}" == l || "${mode:8:1}" != w || "${mode:9:1}" == [tT] ) ]] &&
		[[ "${mode:0:1}" != - || "${mode:5:1}" != w || "$gid" == 0 ]]
}

# first_line_bounded <exe> [args…] -> the first line a run that finished printed,
# or nothing: what a binary says it is (`--version`), asked of one this installer
# placed or one it did not. It runs through bounded: on /dev/null (under
# `curl … | bash` the inherited stdin is the rest of this script, which a binary
# that reads it would swallow), in its own process group, and cut off at
# VERIFY_WAIT_SECS. A run cut off answered nothing, whatever it printed first.
# Every caller runs it in a subshell, a $(…) or a pipeline's, which starts
# with the run's traps reset, so it sets them again (end_on_ctrl_c): a Ctrl-C
# then removes its file there, which the run's own shell never named.
first_line_bounded() {
	local out s=0
	may_run "$1" || return 0
	end_on_ctrl_c
	local BOUNDED_TEMPS=(${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"})
	probe_temp out aterm-probe || return 0
	BOUNDED_OUT="$out" BOUNDED_ERR=/dev/null bounded "$VERIFY_WAIT_SECS" "$@" || s=$?
	[[ "$s" -eq 124 ]] || head -n 1 "$out"
	rm -f "$out"
}

# Whether <exe> has shown it ran, asked `--version` through bounded; when it
# has not, UNPROVEN says why. An exec waits on macOS's check of its file, so a
# run that wrote a line, or ended other than as bounded ends one (124 at the
# deadline; 129, 130, 131 or 143 when HUP, INT, QUIT or TERM cut it short),
# is past that check: by its own exit, or a signal it or macOS raised. A run
# cut off may still be in it, and macOS 27.0.1's syspolicyd crashes when the
# file it is checking at a first exec is removed or renamed, stranding every
# later exec until a restart.
# A path that is no executable file never ran, and off macOS there is no such
# check. An end of 126 or 127 with no line may be the shell's own failure to
# exec it (denied, or a missing interpreter), which shows nothing but a kernel
# refusal it names (exec_refused). Root runs a program it may not run (may_run)
# only as nobody, through a shell that writes `ended <status>` once the
# program's run ends: bash, whose words exec_refused reads (macOS's /bin/sh may
# be zsh, which says a refusal otherwise), reading no startup file (--norc, the
# .bashrc sshd's SSH_CLIENT has it read) or BASH_ENV (-p), so only the program
# and that line write to what is read. Sudo's own refusal, or a file nobody may
# not run, writes none: then root could not ask, and UNPROVEN_THEN says what
# helps instead.
showed_it_ran() { # <exe>
	local s=0 out err said line nobody=""
	local self="run install.sh without sudo, as the account whose aterm it is, which asks it itself"
	UNPROVEN=""
	UNPROVEN_THEN="run this again in 10 minutes, or after a restart"
	[[ "$(uname -s)" == Darwin && -f "$1" && -x "$1" ]] || return 0
	local BOUNDED_TEMPS=(${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"})
	if ! probe_temp out aterm-probe || ! probe_temp err aterm-probe; then
		rm -f -- ${out:+"$out"} ${err:+"$err"}
		UNPROVEN="$1 could not be asked (no temporary file)"
		return 1
	fi
	if may_run "$1"; then
		BOUNDED_OUT="$out" BOUNDED_ERR="$err" bounded "$VERIFY_WAIT_SECS" "$1" --version || s=$?
		[[ ! -s "$out" ]] || s=0
	else
		nobody=1
		BOUNDED_OUT="$out" BOUNDED_ERR="$err" bounded "$VERIFY_WAIT_SECS" run_as_user nobody /bin/bash --norc -p -c \
			'test -x "$0" || exit 1; "$0" --version; echo "ended $?"' "$1" || s=$?
		said="$(tail -n 1 "$out")"
		if [[ "$said" =~ ended\ ([0-9]+)$ ]]; then
			# Any byte before the shell's own `ended <status>` line is the program's.
			if (($(wc -c <"$out") > ${#BASH_REMATCH[0]} + 1)); then
				s=0
			else
				case "${BASH_REMATCH[1]}" in
				126 | 127) s="${BASH_REMATCH[1]}" ;;
				*) s=0 ;;
				esac
			fi
		elif [[ -s "$out" ]]; then
			s=0
		else
			case "$s" in
			124 | 129 | 130 | 131 | 143) ;;
			*) s=unrun ;;
			esac
		fi
	fi
	case "$s" in
	126 | 127) exec_refused "$err" "$1" && s=0 ;;
	esac
	line="$({ grep -v '^[[:space:]]*$' "$err" || true; } | tail -n 1 | LC_ALL=C tr -cd '[:print:]')"
	rm -f "$out" "$err"
	case "$s" in
	124) UNPROVEN="$1 did not answer within ${VERIFY_WAIT_SECS}s" ;;
	129 | 130 | 131 | 143) UNPROVEN="$1 was interrupted before it answered" ;;
	126 | 127)
		UNPROVEN="$1 may not have started${nobody:+ as nobody} (exit $s${line:+, saying \`$line\`})"
		if [[ -n "$nobody" ]]; then
			UNPROVEN_THEN="$self"
		else
			UNPROVEN_THEN="after a restart, when macOS is checking nothing, remove it yourself and run this again"
		fi
		;;
	unrun)
		UNPROVEN="$1 could not be run as nobody to ask it${line:+, saying \`$line\`}"
		UNPROVEN_THEN="$self"
		;;
	*) return 0 ;;
	esac
	return 1
}

# Whether <err>, what a run of <exe> said, has the shell name the kernel's
# refusal to exec <exe>: no format or CPU type this Mac runs, which it refuses
# before macOS's check, so no exec of the file waits on one.
exec_refused() { # <err> <exe>
	local line
	while IFS= read -r line || [[ -n "$line" ]]; do
		case "$line" in
		*": $2: Bad CPU type in executable" | *": $2: Bad executable (or shared library)" | \
			*": $2: Malformed Mach-o file" | *": $2: Exec format error" | \
			*": $2: cannot execute binary file" | *": $2: cannot execute binary file: Exec format error")
			return 0
			;;
		esac
	done <"$1"
	return 1
}

# Whether each program in the aterm.app at <app> has shown it ran: every file
# in Contents/MacOS but a link, since a toolset generation's aterm-cli starts
# its siblings (atpkg for `pkg`).
bundle_showed_it_ran() { # <app>
	local f inventory dir="$1/Contents/MacOS"
	UNPROVEN="$dir could not be inventoried"
	UNPROVEN_THEN="make its program folder readable and searchable, then run this again"
	# A failed glob looks just like a folder with no programs. Require an actual
	# directory and a complete, successful inventory before asking any program.
	[[ -d "$dir" && ! -L "$dir" ]] || return 1
	local BOUNDED_TEMPS=(${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"})
	probe_temp inventory aterm-programs || return 1
	if ! find "$dir" -mindepth 1 -maxdepth 1 -type f -print0 >"$inventory" 2>/dev/null; then
		rm -f "$inventory"
		return 1
	fi
	while IFS= read -r -d '' f; do
		showed_it_ran "$f" || { rm -f "$inventory"; return 1; }
	done <"$inventory"
	rm -f "$inventory"
	UNPROVEN=""
}

# Say that <path> was left where it is, for UNPROVEN (showed_it_ran).
say_unproven() { # <path>
	echo "install.sh: $1 was left as it is: $UNPROVEN, so macOS may still be checking its first start, and replacing it now could stall every program start on this Mac until a restart — $UNPROVEN_THEN" >&2
}

# Whether <path>, an aterm.app or a store's aterm, may be removed or renamed
# over: nothing is there (a link is no program), or what in it aterm's tools
# run has shown it ran. Says why not.
may_replace() { # <path>
	if [[ -L "$1" || ! -e "$1" ]]; then
		return 0
	elif [[ -d "$1" ]]; then
		bundle_showed_it_ran "$1" && return 0
	else
		showed_it_ran "$1" && return 0
	fi
	say_unproven "$1"
	return 1
}

# The plan's line for the one `aterm` on PATH, per bin_aterm_verdict — the same
# question the install path asks (claim_bin_aterm), so --dry-run promises only what a
# real run would do: link it, replace a pre-store file first, or leave a foreign one.
bin_aterm_plan() { # <path> <store-dir>
	local verdict
	verdict="$(bin_aterm_verdict "$1" "$2")"
	case "$verdict" in
	absent | ours) echo "install.sh: cli: ONE command on PATH — the $1 symlink (bundle-backed when the installed app ships the toolset; otherwise a source build into $2), plus man pages/completions where writable" ;;
	pre-store) echo "install.sh: cli: would REPLACE $1 — a regular file from a pre-store install that answers as aterm — with the ONE symlink (bundle-backed, or a source build into $2), plus man pages/completions where writable" ;;
	*) echo "install.sh: cli: $1 would be SKIPPED — ${verdict#foreign } (left exactly as it is; the command would be installed but not on PATH)" ;;
	esac
}

# The first installed bundle whose toolset can back the `aterm` symlink,
# echoed as the exact target to link. Two generations qualify:
#   one-binary (current): Contents/MacOS/aterm is everything — it identifies
#     itself as "aterm <version>" (a GUI-only executable in that slot says
#     "aterm-gui <version>" and must not be linked: it has no session mode).
#   toolset (previous):   aterm-cli + the four verb siblings all present.
# Candidates: the app half's fresh install first, then the env override, then
# the standard app dirs. The --version probes filter truncated/foreign copies,
# each run bounded (a bundle found there may be anyone's, or wedged).
find_bundle_cli() {
	[[ "$(uname -s)" == "Darwin" ]] || return 0
	local c m b ok
	while IFS= read -r c; do
		m="$c/aterm.app/Contents/MacOS"
		if [[ -x "$m/aterm" ]] && first_line_bounded "$m/aterm" --version | grep -q '^aterm '; then
			echo "$m/aterm"
			return 0
		fi
		ok=1
		for b in aterm-cli aterm-ctl atpkg aterm-fleet aterm-drive; do
			[[ -x "$m/$b" ]] || { ok=0; break; }
		done
		if [[ "$ok" -eq 1 ]] && may_run "$m/aterm-cli" && BOUNDED_OUT=/dev/null BOUNDED_ERR=/dev/null bounded "$VERIFY_WAIT_SECS" "$m/aterm-cli" --version; then
			echo "$m/aterm-cli"
			return 0
		fi
	done <<<"$(bundle_dirs)"
	return 0
}

# The folders find_bundle_cli asks for an aterm.app, in its order.
bundle_dirs() {
	printf '%s\n' ${DEST:+"$DEST"} ${ATERM_INSTALL_DIR:+"$ATERM_INSTALL_DIR"} /Applications "$HOME/Applications"
}

# Under root on macOS, the first folder another account can change on the way
# to every aterm.app find_bundle_cli asks, in its order, so that may_run lets
# root run none of them: for the app this run installs in <dest>, <dest> (or,
# while it does not exist, the folder that would hold it: to_make_failing's),
# whose group and group write the new bundle's folders take
# (own_bundle_as_root); for an installed one, its Contents/MacOS.
# /Applications, which the admin group writes, is such a folder. Nothing when
# one passes, none is there, or this is not such a run.
root_bundle_unsealed() { # [<dest>]
	[[ "$(installer_uid)" == 0 && "$(uname -s)" == Darwin ]] || return 0
	local c m o first=""
	while IFS= read -r c; do
		m="$c/aterm.app/Contents/MacOS"
		if [[ -n "${1:-}" && "$c" == "$1" ]]; then
			o="$(to_make_failing sealed_entry "$c")"
		elif [[ -x "$m/aterm" || -x "$m/aterm-cli" ]]; then
			o="$(folder_failing sealed_entry "$m")"
		else
			continue
		fi
		[[ -n "$o" ]] || return 0
		first="${first:-$o}"
	done <<<"$(bundle_dirs)"
	[[ -z "$first" ]] || printf '%s\n' "$first"
}

# Why a root run links no app's aterm, root_bundle_unsealed having named
# <folder>; with <remedy> 1, what to do instead.
root_bundle_refusal() { # <folder> [<remedy>]
	printf '%s' "root runs no aterm another account can change, and $1, on the way to the app's, is such (it is not root's, a group but root's or every account can write it, it carries an ACL, or it is on a volume an account mounted or that is not local)"
	[[ "${2:-0}" -ne 1 ]] || printf '%s' " — install the CLI as your account (tools/install.sh --no-app, without sudo), or set ATERM_INSTALL_DIR to a folder only root can write"
	printf '\n'
}

# The aterm install_toolchain drives, as `run <path>`: the `aterm` in BIN_DIR,
# else an installed bundle's; or, as `skip <reason>`, why it drives none.
toolset_driver() {
	local a="$BIN_DIR/aterm"
	[[ -x "$a" ]] || a="$(find_bundle_cli)"
	if [[ -z "$a" || ! -x "$a" ]]; then
		echo "skip no installed \`aterm\` to drive it"
	elif ! may_run "$a"; then
		echo "skip another account can change $a (or a folder or link on its way), so root does not run it — run the install as that account, or set ATERM_BIN_DIR and ATERM_STORE_DIR to folders root owns"
	else
		echo "run $a"
	fi
}

# Earlier installs (and the pre-one-command layout) exposed `aterm-ctl` on
# PATH next to `aterm`. The front door supersedes it (`aterm ctl …`), so
# retire our previous copy — and only ours: a symlink into an aterm.app
# bundle or the store, or a regular file that identifies itself as aterm-ctl.
# Anything else under that name is left alone.
retire_exposed_siblings() {
	local p="$BIN_DIR/aterm-ctl" tgt=""
	[[ -e "$p" || -L "$p" ]] || return 0
	if [[ -L "$p" ]]; then
		tgt="$(readlink "$p")"
		case "$tgt" in
		*/aterm.app/Contents/MacOS/*ctl | "$STORE_DIR"/*) ;;
		*) return 0 ;;
		esac
	elif ! first_line_bounded "$p" --version | grep -q '^aterm-ctl '; then
		return 0
	fi
	rm -f "$p"
	echo "install.sh: removed $p — use \`aterm ctl\`"
}

# The aterm that runs the work that follows placement (`aterm pkg shell-setup`,
# `aterm pkg self-uninstall`): the one this run placed, else this account's —
# the `aterm` link in <bin-dir> when it is ours (bin_entry_is_ours: never a
# foreign `aterm`, the X11 terminal's name for decades), an installed bundle's,
# the store's. Each must answer --version as aterm and know <verb>, both asked
# bounded, and once: a candidate that is a file already asked (the link, the
# bundle and the placed path are often one binary) is not asked again. Prints
# nothing when none does.
setup_aterm() { # <verb> <bin-dir> <store-dir> <app-dir>...
	local verb="$1" bin="$2" store="$3" c d a asked=()
	shift 3
	local cands=(${PLACED_ATERM:+"$PLACED_ATERM"})
	bin_entry_is_ours "$bin/aterm" "$store" && cands+=("$bin/aterm")
	for d in "$@"; do
		bundle_is_ours "$d/aterm.app" && cands+=("$d/aterm.app/Contents/MacOS/aterm")
	done
	cands+=("$store/aterm")
	for c in "${cands[@]}"; do
		[[ -x "$c" ]] || continue
		for a in ${asked[@]+"${asked[@]}"}; do
			if [[ "$c" -ef "$a" ]]; then continue 2; fi
		done
		asked+=("$c")
		first_line_bounded "$c" --version | grep -q '^aterm ' || continue
		BOUNDED_OUT=/dev/null BOUNDED_ERR=/dev/null bounded "$VERIFY_WAIT_SECS" "$c" pkg help "$verb" || continue
		echo "$c"
		return 0
	done
	return 0
}

# The argv0 aliases the store lanes link beside the store's `aterm`
# (expose_store_binary), each a symlink whose target is exactly `aterm` — which
# is how the uninstall sweep knows one, whatever its name. A hand copy of
# aterm_types::argv0::ALIASES (crates/aterm-types/src/argv0.rs): a name added
# there is added here.
STORE_ALIASES="aterm-cli aterm-ctl atpkg abrew atpkg-abrew agh aterm-fleet aterm-drive aterm-askpass aterm-link aterm-gui amacs"

# _dir_split <path> <head-var> <tail-var>: the deepest directory on <path> that
# exists, resolved (cd -P), and the part below it still to be created, without
# empty, `.` or `..` components — what `mkdir -p <path>` would make. A `..`
# below a directory still to be created undoes that component, as mkdir -p's
# walk does; the result, `..`-free, is split again, since what it names may
# exist after all (`new/../bin` IS `bin`). Fails only when that directory
# cannot be entered.
_dir_split() {
	local p="$1" t="" part out="" up=0
	[[ "$p" == /* ]] || p="$PWD/$p"
	while [[ ! -d "$p" ]]; do
		t="/${p##*/}$t"
		p="${p%/*}"
		[[ -n "$p" ]] || p=/
	done
	p="$(cd -P "$p" 2>/dev/null && pwd -P)" || return 1
	t="${t#/}"
	while [[ -n "$t" ]]; do
		part="${t%%/*}"
		if [[ "$t" == */* ]]; then t="${t#*/}"; else t=""; fi
		case "$part" in
		"" | .) ;;
		..)
			up=1
			if [[ -n "$out" ]]; then
				out="${out%/*}"
			else
				p="${p%/*}"
				[[ -n "$p" ]] || p=/
			fi
			;;
		*) out="$out/$part" ;;
		esac
	done
	if [[ "$up" -eq 1 ]]; then
		p="${p%/}$out"
		_dir_split "${p:-/}" "$2" "$3"
		return
	fi
	printf -v "$2" '%s' "$p"
	printf -v "$3" '%s' "$out"
}

# Whether directory <a> IS <b> or lies beneath it, once both exist. Decided by
# identity where <b> exists: each physical ancestor of <a> is compared with it
# by device and inode (`-ef`), so a trailing slash, a symlinked spelling or a
# case variant cannot hide that two names are one directory. Where <b> does not
# exist yet, <a> can only lie in it through the same deepest existing directory
# and a spelling of what is still to be created that starts with <b>'s.
dir_within() { # <a> <b>
	local ah at bh bt
	_dir_split "$1" ah at || return 1
	_dir_split "$2" bh bt || return 1
	if [[ -z "$bt" ]]; then
		while :; do
			[[ "$ah" -ef "$bh" ]] && return 0
			[[ "$ah" == / ]] && return 1
			ah="${ah%/*}"
			[[ -n "$ah" ]] || ah=/
		done
	fi
	[[ "$ah" -ef "$bh" ]] && [[ "$at" == "$bt" || "$at" == "$bt"/* ]]
}

# env_twin_on <VAR> -> 1 when the env twin of a --no-* flag leaves the half ON, 0 when it
# opts out. Only `1` opts out — what the usage documents (`ATERM_NO_TOOLCHAIN=1`,
# `ATERM_NO_PATH=1`); unset, empty and `0` leave it on; anything else is refused (exit 2)
# rather than guessed, since the toolset opt-out persists (persist_no_toolchain).
env_twin_on() { # <VAR>
	local value="${!1:-}"
	case "$value" in
	"" | 0) echo 1 ;;
	1) echo 0 ;;
	*)
		echo "install.sh: $1='$value' is not 1 or 0 — set $1=1 to opt out, leave it unset (or 0) to keep it" >&2
		return 2
		;;
	esac
}

# --- --no-toolchain persists -------------------------------------------------------
#
# The flag writes `[packages] auto_install = false`, the install consent atpkg
# reads (crates/atpkg/src/config.rs `auto_install`), so the app's first launch
# does not install the set anyway (docs/GOLDEN-INSTALL-PATH.md §3.1).
# It never overwrites a value the person set: an existing `auto_install` (or its
# retired spelling `seed_install`) under [packages], or a [packages] this
# line-oriented edit cannot read safely (an inline table, a dotted `packages.`
# key), is left as it is and named.

# What the persisted key does, in the one sentence the plan and the write both
# print. It is atpkg's install consent (crates/atpkg/src/cli.rs
# `should_complete_set`), so it reaches past a fresh machine: on one that already
# runs the toolset it stops the set being completed — a program added to the
# default set later is never installed unattended — while what is installed keeps
# updating. A re-run with --no-toolchain only to reinstall the app says so.
NO_TOOLCHAIN_EFFECT="the app will not install the ALab toolset on its own; an already-installed toolset keeps updating but gets no new default-set member (\`aterm pkg install --default-set\` installs the whole set when you want it; deleting the key undoes the exclusion)"

# The config file atpkg and the app read (config.rs `config_path`).
aterm_config_path() {
	echo "${XDG_CONFIG_HOME:-$HOME/.config}/aterm/aterm.toml"
}

# What persisting the exclusion would do to <cfg>, one word (then detail):
#   create   no file — it would be created with the key
#   append   a file with no [packages] table — the table would be appended
#   insert   a [packages] table without the key — it would gain the line
#   keep <what>   a value is already there (or cannot be read safely) — untouched
no_toolchain_plan() { # <cfg>
	local cfg="$1"
	[[ -e "$cfg" ]] || {
		echo create
		return 0
	}
	awk '
		function strip(s) { sub(/[ \t]*#.*$/, "", s); sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); return s }
		{
			line = strip($0)
			if (line ~ /^packages[ \t]*=/ || line ~ /^packages\./) { odd = 1 }
			if (line ~ /^\[/) { inpk = (line ~ /^\[[ \t]*packages[ \t]*\]$/); if (inpk) table = 1; next }
			if (inpk && line ~ /^(auto_install|seed_install)[ \t]*=/) { have = line }
		}
		END {
			if (odd) { print "keep [packages] is written in a form this installer does not edit"; exit }
			if (have != "") { print "keep " have; exit }
			print (table ? "insert" : "append")
		}' "$cfg"
}

# Whether persist_no_toolchain's write of <cfg> under <plan> would open it,
# asked without writing a byte, so --dry-run says what the run does: the very
# open each write makes, a read-write one for the insert's refill (the plan
# has read the file, and one without O_TRUNC asks what the refill's does), an
# appending one for the append, and for a create the folder mkdir -p and the
# redirection need (would_create_writable). -w asked the mode and the ACLs,
# not what an open meets beyond them: Linux's append-only attribute (chattr
# +a) refuses a write open without O_APPEND, which -w allows, a macOS ACL or
# the uappnd flag can allow appending and deny writing, or deny appending
# alone, and -w allows a folder at the path, which no open for writing does.
config_write_opens() { # <cfg> <plan>
	case "$2" in
	create) would_create_writable "$(dirname "$1")" ;;
	insert) (: 1<>"$1") 2>/dev/null ;;
	append) (: >>"$1") 2>/dev/null ;;
	*) return 1 ;;
	esac
}

# Persist the exclusion into <cfg> per no_toolchain_plan. Writes through a
# symlinked config (a dotfiles repo keeps its link) and keeps the file's mode.
persist_no_toolchain() { # <cfg>
	local cfg="$1" plan tmp
	# A file awk cannot read (its mode, its owner) has no plan, and the caller
	# says to set the key by hand: the empty plan fell through to the line
	# saying the key was written, over a file that still said otherwise.
	plan="$(no_toolchain_plan "$cfg")" || return 1
	case "$plan" in
	create)
		mkdir -p "$(dirname "$cfg")" || return 1
		printf '[packages]\nauto_install = false\n' >"$cfg" || return 1
		;;
	append)
		# A file whose last byte is not a newline would glue the header to it.
		if [[ -s "$cfg" && -n "$(tail -c 1 "$cfg")" ]]; then printf '\n' >>"$cfg" || return 1; fi
		printf '\n[packages]\nauto_install = false\n' >>"$cfg" || return 1
		;;
	insert)
		# cat's redirection empties the file before cat refills it, so cat runs
		# with the signals that would cut it ignored: a Ctrl-C there ends the
		# run once the file is whole again (run_cut, which removes the copy).
		# A TERM or HUP to this shell alone is held to run_cut the same way
		# (hold_kills), from before the copy is made until it is gone: bash's
		# own handler ended the run at once, the copy left. A refill that
		# fails (a full volume, a quota) can leave the file empty or cut
		# short, its copy then its only whole text: the copy stays, out of
		# BOUNDED_TEMPS, and a NOTE says where. It went with the rest, and
		# the run said only that it could not write the file. A Ctrl-C or
		# kill during the very refill that fails still takes it: its trap
		# runs run_cut as the refill ends, before its status is read. The
		# copy sits next to the file, not in TMPDIR, which the OS empties (at
		# boot, or of files a few days old) maybe before its cp runs. A
		# folder that takes no new file (its mode) puts it in TMPDIR after
		# all: the file in it can still be rewritten in place, and the run
		# left it unwritten. A volume too full for the copy fails its write
		# first, the file untouched. A file this run can read but not open to
		# write (its mode, an ACL, a file flag, a link into a read-only store)
		# is refused before any copy (config_write_opens): the refill's open
		# failed with the file untouched, and the NOTE said it might be empty,
		# with a cp that failed too. Asked by an append that wrote nothing, an
		# ACL that allows appending alone let a file through to that NOTE, and
		# one that denies appending alone refused a file the refill writes;
		# asked by -w, Linux's append-only attribute let one through.
		config_write_opens "$cfg" insert || return 1
		local BOUNDED_TEMPS=(${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"})
		hold_kills
		probe_temp tmp .aterm-config "$(dirname "$cfg")" || probe_temp tmp aterm-config || {
			release_kills
			return 1
		}
		if ! awk '
			{ print }
			!done {
				line = $0; sub(/[ \t]*#.*$/, "", line); gsub(/^[ \t]+|[ \t]+$/, "", line)
				if (line ~ /^\[[ \t]*packages[ \t]*\]$/) { print "auto_install = false"; done = 1 }
			}' "$cfg" >"$tmp"; then
			rm -f "$tmp"
			release_kills
			return 1
		fi
		if ! (trap '' INT HUP TERM QUIT && exec cat "$tmp" >"$cfg"); then
			unset 'BOUNDED_TEMPS[${#BOUNDED_TEMPS[@]}-1]'
			release_kills
			echo "install.sh: NOTE: $cfg may now be empty or cut short; its whole text, with \`auto_install = false\` added, is in $tmp (put it back with: $(note_command "$cfg" cp "$tmp" "$cfg"))" >&2
			return 1
		fi
		rm -f "$tmp"
		release_kills
		;;
	keep*)
		echo "install.sh: --no-toolchain: $cfg already says \`${plan#keep }\` — left as it is"
		return 0
		;;
	esac
	echo "install.sh: --no-toolchain: wrote \`[packages] auto_install = false\` into $cfg — $NO_TOOLCHAIN_EFFECT"
}

# Land one binary in the private store and expose it: the argv0 verb siblings
# beside it, the single `aterm` symlink on PATH. Shared by both producers — the
# source build and the released Linux binary — so the two lanes cannot drift
# into different layouts, and --uninstall's ownership checks match either.
# Callers pre-flight STORE_DIR and BIN_DIR before any build or download work.
place_store_binary() {
	# An unsigned source fallback cannot replace enrolled bytes and leave the
	# durable signed identity pointing at a different executable.
	if [[ "$(uname -s)" == Linux && ( -e "$STORE_DIR/.aterm-update/state.toml" || -L "$STORE_DIR/.aterm-update/state.toml" ) ]]; then
		echo "install.sh: source fallback cannot overwrite an enrolled Linux install; choose a separate ATERM_STORE_DIR for development" >&2
		return 1
	fi
	# A running executable must never be truncated in place. Publish a complete
	# same-filesystem sibling atomically; the old inode stays valid for users
	# whose terminal is already running.
	# The rename below replaces the aterm there.
	may_replace "$STORE_DIR/aterm" || return 1
	# Named before it is made (probe_temp), so a Ctrl-C during the copy
	# removes it: made by mktemp, the partial copy stayed in STORE_DIR.
	# probe_temp says nothing when it fails, where mktemp said why.
	local staged
	local BOUNDED_TEMPS=(${BOUNDED_TEMPS[@]+"${BOUNDED_TEMPS[@]}"})
	probe_temp staged .aterm-install "$STORE_DIR" || {
		echo "install.sh: could not create a file in $STORE_DIR to stage the aterm binary — nothing was installed there" >&2
		return 1
	}
	if ! install -m 755 "$1" "$staged" || ! mv -f "$staged" "$STORE_DIR/aterm"; then
		rm -f "$staged"
		return 1
	fi
	expose_store_binary
}

# The test seam: a suite sources this file with ATERM_INSTALL_LIBRARY_ONLY=1 to
# drive the functions above without parsing arguments or touching the host.
if [[ "${ATERM_INSTALL_LIBRARY_ONLY:-0}" == 1 ]]; then
	end_on_ctrl_c
	return 0 2>/dev/null || exit 0
fi

root_metadata_path || exit 1

# --- uninstall: the installed aterm's own sweep ------------------------------

# --uninstall: the installed aterm sweeps what this script placed (`aterm pkg
# self-uninstall`, every removal ownership-checked; its --dry-run changes
# nothing), told the same overrides. With no runnable aterm there is nothing to
# sweep with: refuse, and list what is there to remove by hand.
uninstall_everything() {
	local bin="${ATERM_BIN_DIR:-$HOME/.local/bin}" store="${ATERM_STORE_DIR:-$HOME/.local/lib/aterm/bin}"
	local dirs=(/Applications "$HOME/Applications") aterm args=() p
	[[ -n "${ATERM_INSTALL_DIR:-}" ]] && dirs=("$ATERM_INSTALL_DIR")
	aterm="$(setup_aterm self-uninstall "$bin" "$store" "${dirs[@]}")"
	if [[ -n "$aterm" ]]; then
		[[ "$DRY_RUN" -eq 1 ]] && args+=(--dry-run)
		[[ -n "${ATERM_INSTALL_DIR:-}" ]] && args+=(--install-dir "$ATERM_INSTALL_DIR")
		[[ -n "${ATERM_BIN_DIR:-}" ]] && args+=(--bin-dir "$ATERM_BIN_DIR")
		[[ -n "${ATERM_STORE_DIR:-}" ]] && args+=(--store-dir "$ATERM_STORE_DIR")
		[[ -n "${ATERM_MAN_DIR:-}" ]] && args+=(--man-root "$ATERM_MAN_DIR")
		self_on_disk && [[ -d "$(dirname "${BASH_SOURCE[0]}")/../man" ]] &&
			args+=(--man "$(cd "$(dirname "${BASH_SOURCE[0]}")/../man" && pwd)")
		exec "$aterm" pkg self-uninstall ${args[@]+"${args[@]}"} </dev/null
	fi
	echo "install.sh: --uninstall runs the installed aterm's own sweep (aterm pkg self-uninstall), and no runnable aterm that has it is here — nothing was removed. What install.sh places, where it is, to remove by hand:" >&2
	local paths=("${dirs[@]/%//aterm.app}" "$bin/aterm" "$bin/atpkg")
	[[ "$bin" == "$HOME/.local/bin" ]] || paths+=("$HOME/.local/bin/aterm" "$HOME/.local/bin/atpkg")
	for p in "${paths[@]}" "$store" "${ATERM_MAN_DIR:-$HOME/.local/share/man}/man1/aterm.1"; do
		[[ -e "$p" || -L "$p" ]] && echo "  $p" >&2
	done
	for p in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile" \
		"${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"; do
		grep -qF '>>> aterm ALab toolset' "$p" 2>/dev/null && echo "  $p: the marker-fenced aterm block" >&2
	done
	return 1
}

# --- install-mode --dry-run: the whole plan, with zero mutations ---------------
# The full plan from the same pre-flight state the real run acts on — the
# elected release, the exact container asset and its size, every destination,
# the shell-profile edit, and every endpoint the run would contact — then the
# dispatch exits 0. No mkdir, no file writes, no rc edit: read-only network
# lookups are the only I/O (the destination probes ran through
# ensure_dirs_writable's non-mutating arm, and the manifest is held in a
# variable, never a temp file). Anything that cannot be resolved read-only
# aborts loudly, as the real run would.
print_install_plan() {
	local tmp_hint="${TMPDIR:-/tmp}/aterm-install.<random>"
	local asset_size manifest_size manifest_text team_want rc

	echo "install.sh: DRY RUN — the full plan; nothing is created, written, or edited"
	if [[ "$DO_APP" -eq 0 ]]; then
		echo "install.sh: app: excluded (--no-app)"
	elif [[ -n "$APP_SKIP" ]]; then
		echo "install.sh: app: would be SKIPPED: $APP_SKIP"
	elif [[ -n "$APP_ALREADY" ]]; then
		echo "install.sh: app: aterm $APP_ALREADY already installed at $DEST/aterm.app and signature-verified — nothing to download (--version $APP_ALREADY forces a reinstall)"
	elif [[ "$LINUX_RELEASE" -eq 1 ]]; then
		# The pre-flight proved the raw ELF is published; its size is the one
		# install_linux_app holds the download to (after the signed size agrees).
		asset_size="$(release_asset_size "$TAG" "$LINUX_ASSET" 536870912)" || exit 1
		echo "install.sh: app: $REPO_SLUG $TAG (linux-$LINUX_ARCH)"
		echo "  download: $LINUX_ASSET — $((asset_size / 1000000)) MB -> $tmp_hint/ (deleted after install)"
		echo "  verify:   aterm's release signatures, then the binary's size, SHA-256 and CPU architecture; never a build older than one already installed or below the latest release's minimum"
		[[ -z "$LINUX_VERIFIER_PLANNED" ]] ||
			echo "  verifier: $LINUX_VERIFIER_PLANNED from $LINUX_VERIFIER_REPO $LINUX_VERIFIER_TAG — $(((LINUX_VERIFIER_PLANNED_SIZE + 999) / 1000)) kB; a real install fetches it into a private temp dir, runs it only if its SHA-256 is the pinned one (first on $STORE_DIR), and removes it on exit"
		echo "  note:     this dry-run reads sizes only; signatures are verified before a real install"
		echo "  install:  aterm -> $STORE_DIR/aterm, linked as $BIN_DIR/aterm"
	else
		# The same read-only resolution install_app performs — one manifest,
		# the shared container election, one container size — with the
		# manifest's bytes as the only thing fetched.
		manifest_size="$(release_asset_size "$TAG" aterm-appcast.toml "$MANIFEST_MAX_BYTES")" || exit 1
		# At most one byte past the size the host named — the cap
		# download_release_asset enforces, applied to memory.
		manifest_text="$(fetch_release_asset "$TAG" aterm-appcast.toml "$manifest_size")" || {
			echo "install.sh: download of aterm-appcast.toml from release $TAG failed" >&2
			exit 1
		}
		if ! parse_manifest_identity_fields "$manifest_text"; then
			echo "install.sh: release $TAG has a malformed or duplicate manifest identity field" >&2
			exit 1
		fi
		validate_manifest_identity "$TAG" "$VERSION" "$DMG_NAME" "$SHA_WANT" || exit 1
		team_want="$(required_team_for "$REPO_SLUG" "${ATERM_TEAM_ID:-}" "$TEAM_MANIFEST")" || exit 1
		# The same shared election install_app runs, so the plan can never
		# name a different container than the install downloads.
		elect_install_lane "$DO_TOOLCHAIN" "$VERSION" \
			"$DMG_NAME" "$SHA_WANT" "$ZIP_NAME" "$ZIP_SHA" || exit 1
		asset_size="$(release_asset_size "$TAG" "$ASSET_NAME")" || exit 1
		echo "install.sh: app: $REPO_SLUG $TAG"
		echo "  download: $ASSET_NAME — $((asset_size / 1000000)) MB -> $tmp_hint/$ASSET_NAME (deleted after the swap)"
		if [[ -n "$team_want" ]]; then
			echo "  verify:   manifest SHA-256, then the Developer-ID chain for team $team_want + notarization"
		else
			echo "  verify:   manifest SHA-256 and the code seal only — no pinned Team ID (UNVERIFIED publisher)"
		fi
		echo "  install:  aterm.app -> $DEST/aterm.app (staged swap; an existing copy is replaced)"
	fi

	# Under root, where root may run no app's aterm (root_bundle_unsealed, judged
	# as install_cli will judge it once the app half has run), the CLI is the
	# source build, or nothing; with nothing placed, the toolset and PATH halves
	# drive what is already installed, as install_toolchain and wire_shell_path
	# will.
	local into="" unsealed="" cli_none=0 driver
	[[ "$DO_APP" -eq 0 || -n "$APP_SKIP" || -n "$APP_ALREADY" || "$LINUX_RELEASE" -eq 1 ]] || into="$DEST"
	if [[ "$DO_CLI" -eq 0 ]]; then
		echo "install.sh: cli: excluded (--no-cli)"
	elif [[ -n "$CLI_SKIP" ]]; then
		echo "install.sh: cli: would be SKIPPED: $CLI_SKIP"
	elif unsealed="$(root_bundle_unsealed "$into")" && [[ -n "$unsealed" && -n "$CLI_CARGO_SKIP" ]]; then
		cli_none=1
		echo "install.sh: cli: would be SKIPPED: $(root_bundle_refusal "$unsealed" 1), and $CLI_CARGO_SKIP"
	else
		bin_aterm_plan "$BIN_DIR/aterm" "$STORE_DIR"
		[[ -z "$unsealed" ]] || echo "  bundle:   not linked: $(root_bundle_refusal "$unsealed"); the CLI is the source build below"
		# The skip first: every refusal found after a cargo was (no rustup
		# behind it, the pinned toolchain absent, a cargo that ignores the
		# pin, a rustc beside it that is not the proxy, an unwritable store,
		# a target dir or cargo home the build cannot write)
		# leaves CARGO_BIN set to the very cargo this line must not announce
		# as the build's.
		if [[ -n "$CLI_CARGO_SKIP" ]]; then
			echo "  cargo:    the source build would be SKIPPED: $CLI_CARGO_SKIP"
		elif [[ -n "$CARGO_BIN" ]]; then
			echo "  toolchain ${CARGO_TOOLCHAIN%% (*}: $CARGO_VERSION ($CARGO_BIN)"
		fi
	fi

	if [[ "$DO_TOOLCHAIN" -eq 0 ]]; then
		echo "install.sh: toolset: excluded (--no-toolchain / ATERM_NO_TOOLCHAIN=1)"
		local cfg plan
		cfg="$(aterm_config_path)"
		plan="$(no_toolchain_plan "$cfg")" || plan=unreadable
		case "$plan" in
		unreadable) echo "  config:   cannot read $cfg — the run would say to set \`[packages] auto_install = false\` there by hand" ;;
		create | append | insert)
			# Asked as the run's write asks (config_write_opens): this said it
			# creates the file in a folder the run cannot write, and adds the key
			# to a link into a read-only store.
			if ! config_write_opens "$cfg" "$plan"; then
				echo "  config:   cannot write $cfg — the run would say to set \`[packages] auto_install = false\` there by hand"
			elif [[ "$plan" == create ]]; then
				echo "  config:   creates $cfg with \`[packages] auto_install = false\` — the exclusion persists: $NO_TOOLCHAIN_EFFECT"
			else
				echo "  config:   adds \`auto_install = false\` to [packages] in $cfg — the exclusion persists: $NO_TOOLCHAIN_EFFECT"
			fi
			;;
		keep*) echo "  config:   $cfg already says \`${plan#keep }\` — left as it is" ;;
		esac
	elif [[ "${TOOLSET_AS_ROOT:-0}" -eq 1 ]]; then
		# Root, for someone not at this Mac's console: never seeded as root
		# (toolset_deferral).
		echo "install.sh: toolset: $ROOT_TOOLSET_LINE"
	elif [[ "$cli_none" -eq 1 && "${TOOLCHAIN_DEFERRED:-0}" -ne 1 && "${CONTAINER_KIND:-}" != zip ]] &&
		driver="$(toolset_driver)" && [[ "$driver" != "run "* ]]; then
		echo "install.sh: toolset: would be SKIPPED: ${driver#skip }"
	elif [[ "${TOOLSET_NO_WINDOW:-0}" -eq 1 ]]; then
		# A lean install nobody at this Mac's console will open: no window
		# follows, so the toolset installs here (toolset_deferral).
		echo "install.sh: toolset: aterm pkg seed + pkg update here — ${TOOLSET_SIZE:-size shown before download}${TOOLSET_SIZE:+ under your home directory} (you are not at this Mac's console, so no aterm window will install it)"
	elif [[ "${TOOLCHAIN_DEFERRED:-0}" -eq 1 || "${CONTAINER_KIND:-}" == zip ]]; then
		if [[ -n "$TOOLSET_SIZE" ]]; then
			echo "install.sh: toolset: installs on aterm's first launch ($TOOLSET_SIZE on disk when finished)"
		else
			echo "install.sh: toolset: installs on aterm's first launch, which shows its size first"
		fi
	elif [[ "${CONTAINER_KIND:-}" == dmg ]]; then
		# Only a pinned pre-lean release elects the DMG now, and those images
		# did seal the payload — so the seed really is local there.
		echo "install.sh: toolset: aterm pkg seed + pkg update — ${TOOLSET_SIZE:-size shown before download}${TOOLSET_SIZE:+ under your home directory} (the app reclaims its ~1 GB sealed payload copy afterwards)"
	else
		# No container election ran (--no-app, or an already-current app):
		# where the builds come from is `pkg seed`'s to decide, exactly as in
		# install_toolchain.
		echo "install.sh: toolset: aterm pkg seed + pkg update — ${TOOLSET_SIZE:-size shown before download}${TOOLSET_SIZE:+ under your home directory}"
	fi

	if [[ "$DO_PATH" -eq 0 ]]; then
		echo "install.sh: PATH: excluded (--no-path / ATERM_NO_PATH=1)"
	elif [[ "$cli_none" -eq 1 && -z "$(setup_aterm shell-setup "$BIN_DIR" "$STORE_DIR" ${DEST:+"$DEST"})" ]]; then
		echo "install.sh: PATH: would be SKIPPED: no installed aterm can wire your shell profile — run \`aterm pkg shell-setup --path\` once one is installed"
	else
		echo "install.sh: PATH: the installed aterm appends at most ONE marker-fenced block to your shell profile — the toolset hook + $BIN_DIR (\`aterm pkg shell-setup --path --dry-run\` names the file; skip with --no-path or ATERM_NO_PATH=1)"
	fi

	if [[ "$DO_APP" -eq 0 ]]; then
		echo "install.sh: network: no GitHub traffic — the app half is excluded"
	elif [[ "$APP_NET" -eq 0 ]]; then
		# An unreleased platform or no curl: the half was skipped before any
		# request.
		echo "install.sh: network: no GitHub traffic — the app half is skipped before any request"
	else
		echo "install.sh: network: github.com's release download host over TLS — no credential and no GitHub API request (the latest-release pointer, then that tag's own asset URLs), following its redirects to the release-asset CDN"
	fi
	if [[ "$DO_TOOLCHAIN" -eq 1 ]]; then
		echo "  and: the ALab toolset's signed index, from atpkg's configured source"
	fi
	echo "install.sh: dry run complete — nothing was installed, written, or edited"
}

# --- WHOLE-FILE EXECUTION GUARD ------------------------------------------------
# The run executes ONLY through the `main "$@"; }` on this file's LITERAL LAST
# LINE, whose brace closes the group the file opens above its first command.
# The documented invocation is `curl … | bash`, and bash runs a pipe as it
# arrives: unwrapped, a transfer truncated after the argument loop would
# execute a PREFIX of the run (`head -n <pre-dispatch> install.sh | bash`
# created ~/.local/bin, spent anonymous GitHub API budget, and exited 0).
# Keeping every DESTRUCTIVE operation in a function with the dispatch as the
# tail is not enough: the top-level pre-flight between the two has side
# effects too. Wrapping main alone was not enough either: a cut just after
# `main` on a bare `main "$@"` ran a whole install with no arguments, and one
# inside a top-level word ran that word's first letters as a command (`make`
# from make_download_dir builds the Makefile where the run was started,
# `bundle` from bundle_identifier runs bundle install). So the whole file is
# one group, and bash executes none of a group before its closing brace has
# arrived: a cut anywhere inside it is bash's syntax error and runs nothing,
# and a cut in the comments above it runs nothing either. Do not remove it.
# Tested at every unindented line, every byte of each one's first word, and
# every byte of the last line by crates/aterm-update/tests/install_sh.
# Neither main's body nor the file's group is indented ON PURPOSE:
# re-indenting would be mechanical churn and risk pushing literal tabs into
# multi-line strings.
main() {
end_on_ctrl_c
ARGV=("$@")
TAG=""
TAG_INPUT=""
TAG_EXPLICIT=0
DO_APP=1
NO_OPEN=0
DO_CLI=1
# The toolset is the product, not an add-on (docs/GOLDEN-INSTALL-PATH.md §1.3).
# Default on; the opt-out exists for CI, which wants the app without the
# toolset's expansion. "~4.4 GiB" is the signed sum of the registry's
# per-program `[cost].disk_installed` (4,680,384,013 B at index 15), rendered as
# atpkg's `cost::human_bytes` renders it; re-derive it from the pinned pkg
# manifests when the published set moves, never re-estimate.
DO_TOOLCHAIN="$(env_twin_on ATERM_NO_TOOLCHAIN)" || exit 2
# That sum is the Apple-silicon set's: trust, clean and the Trust backends
# publish no Intel build, so on any other machine the set differs and the
# lines leave the size to atpkg, which shows this machine's sum first.
TOOLSET_SIZE=""
if [[ "$(uname -s)" == Darwin && "$(sysctl -n hw.optional.arm64 2>/dev/null)" == 1 ]]; then
	TOOLSET_SIZE="~4.4 GiB"
fi
# PATH wiring for the user's own shell (wire_shell_path says what atpkg's own
# block leaves out). Opt out with ATERM_NO_PATH=1.
DO_PATH="$(env_twin_on ATERM_NO_PATH)" || exit 2
DO_UNINSTALL=0
DRY_RUN=0
while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help)
		usage
		exit 0
		;;
	-v | --version)
		# Pins a release; it is not a version query. `-v` reads like "print the
		# version" everywhere else, so each error says what the flag does and
		# where the installed version is read.
		if [[ -z "${2:-}" ]]; then
			echo "install.sh: --version pins a release and needs one (X.Y.Z); the installed version: aterm --version" >&2
			exit 2
		fi
		TAG_INPUT="$2"
		TAG="v${2#v}"
		TAG_EXPLICIT=1
		shift 2
		;;
	--no-cli)
		DO_CLI=0
		shift
		;;
	--no-app)
		DO_APP=0
		shift
		;;
	--no-open)
		NO_OPEN=1
		shift
		;;
	--no-toolchain)
		DO_TOOLCHAIN=0
		shift
		;;
	--no-path)
		DO_PATH=0
		shift
		;;
	--uninstall)
		DO_UNINSTALL=1
		shift
		;;
	--dry-run)
		# With --uninstall: preview the removals (dispatched just below).
		# Alone: the install plan, dispatched after the pre-flights, which is
		# when its facts (lane, elected release, per-half skips) first exist.
		DRY_RUN=1
		shift
		;;
	*)
		echo "install.sh: unknown argument: $1 (try --help)" >&2
		exit 2
		;;
	esac
done
# home_owner_named lists the account database, so it is asked only when
# home_account reads its answer: root, over a home neither root's nor sudo's
# invoker's.
HOME_OWNER="$(home_owner_uid)"
HOME_NAMED=0
if [[ "$(installer_uid)" == 0 && -n "$HOME_OWNER" && "$HOME_OWNER" != 0 && "$HOME_OWNER" != "${SUDO_UID:-}" ]] &&
	home_owner_named; then
	HOME_NAMED=1
fi
home_account "$(installer_uid)" "${SUDO_UID:-}" "$HOME_OWNER" "$HOME_NAMED"
if [[ -n "$HOME_ACCOUNT" || "$HOME_REFUSED" -eq 1 ]]; then
	run_as_home_account ${ARGV[@]+"${ARGV[@]}"}
fi
if [[ "$DO_UNINSTALL" -eq 1 ]]; then
	uninstall_everything
	exit $?
fi
# Install-mode --dry-run deliberately has no dispatch here: the pre-flights
# below are read-only under DRY_RUN (ensure_dirs_writable), and the plan
# prints once they have decided everything — see print_install_plan.
# Refuse only when every half is out, so a toolchain-only repair (--no-app
# --no-cli against an installed app) still runs.
if [[ "$DO_APP" -eq 0 && "$DO_CLI" -eq 0 && "$DO_TOOLCHAIN" -eq 0 && "$DO_PATH" -eq 0 ]]; then
	echo "install.sh: every half is excluded (--no-app --no-cli --no-toolchain --no-path) — nothing to do" >&2
	exit 2
fi
if [[ "$TAG_EXPLICIT" -eq 1 ]] && ! canonical_authority_tag "$TAG"; then
	echo "install.sh: --version pins a release and '$TAG_INPUT' is not one — expected X.Y.Z" >&2
	exit 2
fi

# Repo slug: env override first; else, run from a checkout, the update channel its
# manifest declares (channel_slug_from_manifest — the shipped binary's own
# derivation); else, piped (`… | bash`, no script path), the public channel.
checkout_channel_slug() {
	local root
	self_on_disk || return 0
	root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || return 0
	channel_slug_from_manifest "$root/Cargo.toml" || true
}
REPO_SLUG="${ATERM_REPO_SLUG:-$(checkout_channel_slug)}"
: "${REPO_SLUG:=$OFFICIAL_REPO_SLUG}"

# --- pre-flight: decide what each half can do here (fail fast, skip loud) ------
BIN_DIR="${ATERM_BIN_DIR:-$HOME/.local/bin}"
# The private store for the ONE executable, from a source build or Linux
# release, plus compatibility argv0 symlinks. Only `aterm` is exposed on PATH,
# as a symlink into this store, like the app-backed command on macOS.
# Defined before the app pre-flight because on Linux the store IS the app
# half's destination.
STORE_DIR="${ATERM_STORE_DIR:-$HOME/.local/lib/aterm/bin}"
foreign_home_folders
foreign_home_note
APP_SKIP=""
APP_FATAL=""
APP_ALREADY=""
# Whether the app half reached the network (the plan's network line says so).
APP_NET=0
DEST=""
# The app half's Linux shape: no bundle, so the artifact is the signed raw
# binary for this architecture, landed in the same store layout the
# source-build fallback fills.
LINUX_RELEASE=0
LINUX_ARCH="$(linux_release_arch "$(uname -s)" "$(uname -m)" || true)"
[[ -n "$LINUX_ARCH" ]] && LINUX_RELEASE=1
LINUX_ASSET=""
# The channel head Linux authorizes an older native target under (its current
# appcast and roster), and the appcast name the target release carries.
LINUX_HEAD_TAG=""
LINUX_MANIFEST_ASSET=aterm-appcast.toml
# The store needs a directory of its own. With ATERM_STORE_DIR naming
# ATERM_BIN_DIR, the store's `aterm` and the PATH link would be one file:
# claim_bin_aterm would remove the binary just placed as a "pre-store" leftover
# and link `aterm` to itself, while the run said "ONE command on PATH". A store
# inside the PATH directory, or around it, mixes the store's entries with PATH's
# the same way. Refused up front, before any network or write, whenever a half
# that may fill the store is on: the cli half (its source-build lane) or Linux's
# app half. The PATH directories are the ones the uninstall sweep walks —
# ATERM_BIN_DIR, and ~/.local/bin, where the app plants its own links whatever
# ATERM_BIN_DIR says — so no install lays down a store the uninstall must leave
# behind as part of a PATH directory.
if [[ "$DO_CLI" -eq 1 || ("$DO_APP" -eq 1 && "$LINUX_RELEASE" -eq 1) ]]; then
	for OVERLAP_DIR in "$BIN_DIR" "$HOME/.local/bin"; do
		if dir_within "$STORE_DIR" "$OVERLAP_DIR" || dir_within "$OVERLAP_DIR" "$STORE_DIR"; then
			echo "install.sh: ATERM_STORE_DIR ($STORE_DIR) overlaps the PATH directory $OVERLAP_DIR — the store needs a directory of its own, neither a PATH directory nor one inside or around it (default ~/.local/lib/aterm/bin)" >&2
			exit 2
		fi
	done
fi
if [[ "$DO_APP" -eq 1 ]]; then
	if [[ "$(uname -s)" != "Darwin" && "$LINUX_RELEASE" -eq 0 ]]; then
		APP_SKIP="no supported release target for $(uname -s)/$(uname -m) — Linux supports aarch64 and x86_64; the app bundle supports macOS"
	elif ! command -v curl >/dev/null 2>&1; then
		APP_SKIP="needs curl to read the release download host (github.com)"
	else
		APP_NET=1
		# The tag: an explicit --version, else the channel head — one HEAD of
		# the latest-release pointer (channel_head). No catalog is listed. Linux
		# resolves the head under --version too: its current appcast and roster
		# are the policy an older native target is authorized under.
		if [[ -z "$TAG" || "$LINUX_RELEASE" -eq 1 ]]; then
			HEAD_STATUS=0
			channel_head || HEAD_STATUS=$?
			case "$HEAD_STATUS" in
			0)
				LINUX_HEAD_TAG="$HEAD_TAG"
				[[ -n "$TAG" ]] || TAG="$HEAD_TAG"
				;;
			2) APP_FATAL="$HEAD_WHY" ;;
			*) APP_SKIP="$HEAD_WHY" ;;
			esac
		fi
		if [[ -n "$APP_FATAL$APP_SKIP" ]]; then
			:
		elif [[ "$LINUX_RELEASE" -eq 1 ]]; then
			# On Linux the destination is the cli store itself, and the artifact may
			# not exist (a release before the first Linux cut is macOS-only): both
			# environment facts, so both skip the half before any download.
			LINUX_ASSET="aterm-${TAG#v}-linux-$LINUX_ARCH"
			# So is the verifier: the pinned one fetched and held to its
			# digest (one that is not is refused outright), or the embedded
			# one's tools. A dry run only sizes a pinned one, so its store
			# pre-flight waits for the real install, which runs it.
			LINUX_VERIFIER_STATUS=0
			linux_verifier_ready "$LINUX_ARCH" || LINUX_VERIFIER_STATUS=$?
			if [[ "$LINUX_VERIFIER_STATUS" -eq 2 ]]; then
				APP_FATAL="$LINUX_VERIFIER_WHY"
			elif [[ "$LINUX_VERIFIER_STATUS" -ne 0 ]]; then
				APP_SKIP="$LINUX_VERIFIER_WHY"
			elif ! ensure_dirs_writable "$STORE_DIR" "$BIN_DIR"; then
				LINUX_REMEDY="$(unwritable_remedy "$STORE_DIR" ATERM_STORE_DIR)"
				[[ "$LINUX_REMEDY" != set* ]] || LINUX_REMEDY="$(unwritable_remedy "$BIN_DIR" ATERM_BIN_DIR)"
				[[ "$LINUX_REMEDY" != set* ]] || LINUX_REMEDY="set ATERM_STORE_DIR / ATERM_BIN_DIR to writable dirs"
				APP_SKIP="cannot create/write $STORE_DIR or $BIN_DIR ($LINUX_REMEDY)"
			elif [[ -z "$LINUX_VERIFIER_PLANNED" ]] && ! linux_store_preflight "$STORE_DIR"; then
				APP_SKIP="$LINUX_STORE_SKIP"
			else
				# At or below the head, this architecture's signed binary
				# published, and an appcast (current or archived) to bind it.
				linux_target_preflight "$TAG" "$LINUX_HEAD_TAG" "$LINUX_ASSET"
				APP_SKIP="$LINUX_TARGET_SKIP"
			fi
		else
			# The manifest must be published on the tag before anything else is
			# decided: the head's release can be mid-publication, and a pinned
			# tag may not exist at all — both skips, never a mid-install abort.
			PRESENT=0
			release_asset_present "$TAG" aterm-appcast.toml || PRESENT=$?
			if [[ "$PRESENT" -eq 2 ]]; then
				APP_SKIP="could not inspect release $TAG ($ASSET_WHY)"
			elif [[ "$PRESENT" -eq 1 && "$TAG_EXPLICIT" -eq 1 ]]; then
				APP_SKIP="release $TAG publishes no aterm-appcast.toml in $REPO_SLUG — no such app release (or the repository is private, renamed or missing: github.com answers the same for each)"
			elif [[ "$PRESENT" -eq 1 ]]; then
				APP_SKIP="the channel head $TAG has no app manifest yet — its release is still being published; retry in a few minutes, or name an earlier release with --version"
			fi
		fi
		if [[ -z "$APP_FATAL$APP_SKIP" && "$LINUX_RELEASE" -eq 0 ]]; then
			# Destination: explicit env wins; else /Applications, else ~/Applications.
			# The in-app updater never relocates (it defers when its location is not
			# writable, docs/RELEASING.md), so a user-writable dir here keeps
			# self-update working for non-admin users. Checked here so a bad
			# destination skips the half before the download.
			DEST="${ATERM_INSTALL_DIR:-}"
			if [[ -z "$DEST" ]]; then
				if [[ -w /Applications ]]; then DEST=/Applications; else DEST="$HOME/Applications"; fi
			fi
			if ! ensure_dirs_writable "$DEST"; then
				APP_SKIP="cannot create/write $DEST (set ATERM_INSTALL_DIR to a writable dir)"
			elif root_dest_acl_open "$DEST"; then
				if [[ -e "$DEST" ]]; then
					APP_SKIP="$DEST carries an ACL, which may let an account other than root change what is in it, so a root run does not install aterm.app there (that account could put a folder of its own in the app's place as it is swapped in) — ls -led '$DEST/' lists the entries; remove them (sudo chmod -N '$DEST'), or set ATERM_INSTALL_DIR to a folder without one"
				else
					APP_SKIP="$DEST would be made with the ACL $(existing_anchor "$DEST") passes on to the folders made in it, which may let an account other than root change what is in it, so a root run would not install aterm.app there (that account could put a folder of its own in the app's place as it is swapped in) — ls -led '$(existing_anchor "$DEST")/' lists the entries; make $DEST without them (sudo mkdir -p '$DEST' && sudo chmod -N '$DEST'), or set ATERM_INSTALL_DIR to a folder without one"
				fi
			elif root_dest_keeps_no_owner "$DEST"; then
				APP_SKIP="$DEST is on a volume that ignores ownership (mount lists it noowners, as for an exFAT or FAT volume, or an external one with \"Ignore ownership on this volume\" on), so a root run cannot keep aterm.app's folders root's alone there — set ATERM_INSTALL_DIR to a folder on a volume that keeps owners, or install as your account, without sudo"
			elif APP_DEST_VERDICT="$(app_dest_verdict "$DEST/aterm.app")" &&
				[[ "$APP_DEST_VERDICT" == foreign* ]]; then
				# Ownership, the uninstaller's own test: a bundle that is not
				# aterm's is never moved aside and replaced — the same entry
				# --uninstall refuses to delete. A damaged aterm (no readable
				# identifier, a binary that answers as aterm) is repaired below.
				APP_SKIP="$DEST/aterm.app — ${APP_DEST_VERDICT#foreign }; left in place"
			elif [[ "$APP_DEST_VERDICT" == damaged* ]]; then
				# A damaged aterm: nothing to compare a version against, so the
				# install below replaces it — the repair a re-run is for.
				if [[ "$DRY_RUN" -eq 1 ]]; then
					echo "install.sh: app: $DEST/aterm.app is a damaged aterm install (${APP_DEST_VERDICT#damaged }) — would be set aside and replaced"
				else
					echo "install.sh: $DEST/aterm.app is a damaged aterm install (${APP_DEST_VERDICT#damaged }) — setting it aside and replacing it"
				fi
			elif [[ -f "$DEST/aterm.app/Contents/Info.plist" ]] && app_already_current "$TAG" "$TAG_EXPLICIT" \
				"$(defaults read "$DEST/aterm.app/Contents/Info.plist" CFBundleIdentifier 2>/dev/null || true)" \
				"$(defaults read "$DEST/aterm.app/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || true)"; then
				# Idempotent re-run: the elected release is the installed bundle,
				# so the app half has nothing to download — the cli/toolset/PATH
				# halves still run. An explicit --version pin always reinstalls.
				# Version strings alone vouch for nothing: the bundle must still
				# pass the signature gate a fresh install enforces, else the re-run
				# a user reaches for as the remedy would bless a tampered copy. The
				# pin: env override, or the official team on the official slug (a
				# fork without a pin verifies the seal only).
				SKIP_TEAM="${ATERM_TEAM_ID:-}"
				if [[ -z "$SKIP_TEAM" && "$REPO_SLUG" == "$OFFICIAL_REPO_SLUG" ]]; then
					SKIP_TEAM="$OFFICIAL_TEAM_ID"
				fi
				SKIP_STATUS=0
				installed_bundle_verified "$DEST/aterm.app" "$SKIP_TEAM" || SKIP_STATUS=$?
				if [[ "$SKIP_STATUS" -eq 0 && "$(installer_uid)" == 0 ]] && ! bundle_is_roots "$DEST/aterm.app"; then
					# A root run's bundle is root's (own_bundle_as_root): one in
					# which an earlier root run left the builder's uid, files its
					# folder's group can write, an entry every account can write
					# or an ACL, is installed again.
					echo "install.sh: installed aterm matches the elected version but another account owns files in it, or a group, every account or an ACL can change them — reinstalling it as root's" >&2
				elif [[ "$SKIP_STATUS" -eq 0 ]]; then
					APP_ALREADY="${TAG#v}"
				elif [[ "$SKIP_STATUS" -eq 124 ]]; then
					APP_SKIP="macOS's code-signing service (trustd) did not answer within $VERIFY_WAIT_SECS s, so the installed aterm was not checked — run install.sh again once it answers (a restart clears a wedged trustd)"
				elif [[ "$SKIP_STATUS" -eq 125 ]]; then
					APP_SKIP="Gatekeeper (spctl) did not answer within $VERIFY_WAIT_SECS s, so the installed aterm was not checked — run install.sh again once it answers"
				else
					echo "install.sh: installed aterm matches the elected version but FAILED signature verification — reinstalling" >&2
				fi
			fi
		fi
	fi
fi

if [[ -n "$APP_FATAL" ]]; then
	echo "install.sh: REFUSED the app: $APP_FATAL" >&2
	exit 1
fi

CLI_SKIP=""
CLI_CARGO_SKIP=""
ROOT=""
# Filled by the source-lane pre-flight below (find_cargo_and_rustup +
# cargo_honours_pin); empty whenever that lane is skipped.
CARGO_BIN=""
RUSTUP_BIN=""
CARGO_LANE=""
CARGO_VERSION=""
CARGO_TOOLCHAIN=""
CARGO_RUSTC=""
if [[ "$DO_CLI" -eq 1 ]]; then
	# The destination gates both sources (symlink and build), so it is checked
	# first.
	if ! ensure_dirs_writable "$BIN_DIR"; then
		CLI_SKIP="cannot create/write $BIN_DIR ($(unwritable_remedy "$BIN_DIR" ATERM_BIN_DIR))"
	else
		# Feasibility of the source fallback only. The preferred source, an
		# installed bundle shipping the tools, is probed at install time
		# (install_cli): the app half has not run yet and may be about to
		# install exactly that bundle.
		if self_on_disk; then
			ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || ROOT=""
		fi
		if [[ -z "$ROOT" || ! -f "$ROOT/Cargo.toml" ]] && [[ "$(uname -s)" != Darwin ]]; then
			# A clone alone does not help here: the checkout pins Trust, which
			# publishes no build for this OS (trust_link_remedy). So the line
			# names Trust first and the clone as the step that follows it.
			CLI_CARGO_SKIP="building from source needs a checkout (a piped script has none) and the Trust toolchain, which has no published build for $(uname -s) (https://github.com/alabsystems/trust builds from source); once Trust is built and linked (rustup toolchain link trust <prefix>): git clone https://github.com/$REPO_SLUG && cd ${REPO_SLUG##*/} && tools/install.sh"
		elif [[ -z "$ROOT" || ! -f "$ROOT/Cargo.toml" ]]; then
			CLI_CARGO_SKIP="building from source needs a checkout (a piped script has none) — git clone https://github.com/$REPO_SLUG && cd ${REPO_SLUG##*/} && tools/install.sh"
		elif ! find_cargo_and_rustup "$ROOT"; then
			# Two lanes, in order: the aterm-managed Trust store's targo
			# (CARGO_LANE=store, no rustup consulted), else rustup's proxy by
			# its own path, else PATH's cargo — and no store, no cargo, or a
			# cargo with no rustup behind it, is a skip that names one remedy
			# (gate_trust_remedy), never `brew install rust` (a plain cargo
			# cannot honour the pin).
			CLI_CARGO_SKIP="$CARGO_RESOLVE_REASON"
		elif [[ "$CARGO_LANE" != store ]] &&
			PINNED_CHANNEL="$(toml_single_str "$ROOT/rust-toolchain.toml" channel 0 2>/dev/null)" &&
			[[ -n "$PINNED_CHANNEL" ]] &&
			! env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 "$RUSTUP_BIN" toolchain list 2>/dev/null | awk '{print $1}' | grep -Eq "^${PINNED_CHANNEL}(-|$)"; then
			# RUSTUP_AUTO_INSTALL=0 here too: for a downloadable channel that is
			# absent, `rustup toolchain list` installs it first, a fetch a
			# read-only pre-flight must never start.
			#
			# A build without the pinned toolchain cannot succeed, so it is a loud
			# skip here naming how to get it, never a mid-flight abort:
			#   * a custom name (`trust`, which rustup cannot download): reached
			#     only when the store lane found no aterm-managed store, so
			#     `aterm pkg install trust` comes first and the hand-linked Trust
			#     tarballs second;
			#   * a rustup dist channel, which one `rustup toolchain install`
			#     fetches.
			if rustup_dist_channel "$PINNED_CHANNEL"; then
				CLI_CARGO_SKIP="building from source needs the pinned '$PINNED_CHANNEL' rustup toolchain, which is not installed (this pre-flight never downloads one) — rustup toolchain install $PINNED_CHANNEL"
			elif [[ "$PINNED_CHANNEL" == trust ]]; then
				CLI_CARGO_SKIP="building from source needs the pinned 'trust' rustup toolchain, which rustup cannot download, and no aterm-managed Trust store is in reach either — $(trust_link_remedy)"
			else
				CLI_CARGO_SKIP="building from source needs the pinned '$PINNED_CHANNEL' rustup toolchain, which rustup cannot download — $(trust_link_remedy "$PINNED_CHANNEL")"
			fi
		elif ! cargo_honours_pin "$ROOT"; then
			# The toolchain is installed, but the cargo in hand does not
			# dispatch to it (a Homebrew cargo, taken because no proxy sits
			# beside rustup), or rustup resolves the checkout to something
			# else (a `rustup override` on it): either way a build would
			# silently use the wrong compiler.
			CLI_CARGO_SKIP="$CARGO_RESOLVE_REASON"
		elif ! ensure_dirs_writable "$STORE_DIR"; then
			# Pre-flighted here, never discovered after the multi-minute build.
			CLI_CARGO_SKIP="cannot create/write $STORE_DIR ($(unwritable_remedy "$STORE_DIR" ATERM_STORE_DIR))"
		elif ! source_build_preflight; then
			# The same: an earlier install under sudo left what the build
			# writes root's, so it would fail on a permission error.
			CLI_CARGO_SKIP="$CARGO_RESOLVE_REASON"
		fi
	fi
fi

# Install-mode --dry-run (the uninstall's own is dispatched above): every
# decision now exists — lane, elected release, per-half skip reasons,
# destinations — and nothing has been mutated (the destination probes ran
# through ensure_dirs_writable's non-mutating arm). Print the plan and stop,
# before the run phase it describes. A fatal head (APP_FATAL) already exited 1
# above: a dry run reports skips, never launders a refusal into exit 0.
if [[ "$DRY_RUN" -eq 1 ]]; then
	print_install_plan
	exit 0
fi

INSTALLED_ANY=0
# Set by install_linux_app: on Linux the app half fills the same store the cli
# half's source build would, so the cli half must know it already happened.
LINUX_APP_INSTALLED=0
# Coordination for the PATH story: install_cli requests the hand-edit hint,
# wire_shell_path records whether the managed block (which carries $BIN_DIR)
# exists, and the final dispatch prints the hint only when no block does.
# install_toolchain records a completed seed or a stand-down
# (ATPKG_CONTENDED_EXIT), so a toolchain-only repair run can exit 0 without
# touching INSTALLED_ANY's meaning.
CLI_PATH_HINT_WANTED=0
# Whether $BIN_DIR/aterm is this run's: expose_store_binary (the store lanes)
# and install_cli's bundle lane set it; 0 only when claim_bin_aterm left a
# foreign entry there, which say_store_command and the completions then honour.
BIN_ATERM_EXPOSED=1
PATH_BLOCK_WROTE=0
# The app install_app placed and opens once every other half is done
# (app_opens_here), so it never races this run's CLI link or PATH block;
# cleared once opened, so install_app_cleanup prints its launch line only for a
# run that ended first.
APP_TO_OPEN=""
# The aterm this run placed: what runs the work after placement (setup_aterm).
PLACED_ATERM=""
TOOLCHAIN_RAN=0
# atpkg's EX_TEMPFAIL contention code (crates/atpkg/src/lock.rs CONTENDED_EXIT):
# another atpkg run holds the store lock, so this script stands aside.
ATPKG_CONTENDED_EXIT=75
# atpkg's EX_UNAVAILABLE offline code (crates/aterm-update-core/src/pkg_check.rs
# PASS_OFFLINE_EXIT): an update pass that no host answered.
ATPKG_OFFLINE_EXIT=69

# --- the app half: released aterm.app, verified, swapped into place ------------
install_app() {
	echo "install.sh: installing $REPO_SLUG $TAG"
	# The swap sets aside and removes the bundle there: asked first here, so a
	# bundle that stays costs no download.
	may_replace "$DEST/aterm.app" || exit 1

	MNT=""
	STAGE=""
	STAGE_DIR=""
	OLD=""
	make_download_dir install_app_cleanup
	MNT="$TMP/mnt"

	# The manifest, from the tag's own URL, held to the size the host names.
	MANIFEST_SIZE="$(release_asset_size "$TAG" aterm-appcast.toml "$MANIFEST_MAX_BYTES")" || exit 1
	download_release_asset "$TAG" aterm-appcast.toml "$MANIFEST_SIZE" "$TMP/aterm-appcast.toml" || exit 1
	# One parse, shared with the --dry-run plan, fed the size-verified download.
	if ! parse_manifest_identity_fields "$(cat "$TMP/aterm-appcast.toml")"; then
		echo "install.sh: release $TAG has a malformed or duplicate manifest identity field" >&2
		exit 1
	fi
	validate_manifest_identity "$TAG" "$VERSION" "$DMG_NAME" "$SHA_WANT" || exit 1
	# Fail-closed on the official channel (required_team_for): the unsigned
	# manifest can never downgrade verification to ad-hoc.
	TEAM_WANT="$(required_team_for "$REPO_SLUG" "${ATERM_TEAM_ID:-}" "$TEAM_MANIFEST")" || exit 1

	# --- pick the container: the LEAN zip; a pinned pre-lean release, its DMG --
	# The decision lives in elect_container (with the election rationale and
	# every manifest-identity bind), extracted so
	# crates/aterm-update/tests/install_sh pins the whole matrix without a
	# network. It sets CONTAINER_KIND/
	# ASSET_NAME/ASSET_SHA/LEAN_REASON/TOOLCHAIN_DEFERRED; a refusal already
	# named its cause. The toolset is deferred to the window only when the
	# installing user holds the console, or this run is root (toolset_deferral).
	elect_install_lane "$DO_TOOLCHAIN" "$VERSION" \
		"$DMG_NAME" "$SHA_WANT" "$ZIP_NAME" "$ZIP_SHA" || exit 1
	# The election is not narrated: the download line below names the
	# container, and --no-toolchain's one caveat rides that plan.

	# The appcast's Ed25519 .sig is not fetched: this lane cannot verify it (the
	# header's bootstrap trust boundary); the installed updater does.

	# Soft OS floor advisory. Validate both operands before comparison so
	# manifest text never enters Bash's arithmetic-expression evaluator.
	OS_VER="$(sw_vers -productVersion)"
	if [[ -n "$MIN_OS" ]]; then
		if ! canonical_numeric_tag "v$MIN_OS"; then
			echo "install.sh: release $TAG has a malformed min_os value '$MIN_OS'" >&2
			exit 1
		elif canonical_numeric_tag "v$OS_VER"; then
			compare_numeric_tags "v$OS_VER" "v$MIN_OS" || exit 1
			if [[ "$TAG_COMPARE_RESULT" -eq -1 ]]; then
				echo "install.sh: WARNING: macOS $OS_VER is below the release's minimum ($MIN_OS); the app may not launch" >&2
			fi
		else
			echo "install.sh: WARNING: cannot interpret local macOS version '$OS_VER'; skipping the minimum-OS advisory" >&2
		fi
	fi

	# Identity validation above makes this the canonical basename before it is
	# joined to TMP. Its size, from the host, bounds the download.
	ASSET_SIZE="$(release_asset_size "$TAG" "$ASSET_NAME")" || exit 1
	# Disk pre-flight, before the first byte: ~2.5x the container covers the
	# download, the staged bundle copy, and mount/expansion slack — checked on
	# both volumes, since TMPDIR and the destination may not share one.
	SPACE_NEED=$((ASSET_SIZE * 5 / 2))
	require_free_space "$TMP" "$SPACE_NEED" "downloading and staging $ASSET_NAME" || exit 1
	require_free_space "$DEST" "$SPACE_NEED" "installing aterm.app" || exit 1
	# Say the whole plan before any of it happens: what comes down the wire and
	# what it becomes on disk are the two numbers that decide whether someone
	# waits or reaches for ^C.
	if [[ "$CONTAINER_KIND" == dmg ]]; then
		# A pinned pre-lean release: its DMG did seal the toolset, so the
		# "nothing else is downloaded" promise below still holds for it.
		echo "install.sh: downloading $ASSET_NAME — $((ASSET_SIZE / 1000000)) MB"
		echo "  the ALab toolset rides inside the app, so nothing else is downloaded to install it."
		if [[ "$DO_TOOLCHAIN" -eq 1 ]]; then
			echo "  then: aterm.app -> $DEST, and the toolset unpacks to ${TOOLSET_SIZE:-a size shown before download}${TOOLSET_SIZE:+ under your home directory}."
			echo "  the app reclaims its ~1 GB copy of the payload as soon as the toolset is in place."
		else
			echo "  then: aterm.app -> $DEST. Toolset skipped (--no-toolchain); \`aterm pkg seed\` installs it later."
		fi
	else
		echo "install.sh: downloading $ASSET_NAME — $((ASSET_SIZE / 1000000)) MB"
		if [[ "$LEAN_REASON" == default && "${TOOLSET_NO_WINDOW:-0}" -eq 1 ]]; then
			# No window follows (toolset_deferral): the toolset installs here.
			echo "  then: aterm.app -> $DEST, and the toolset installs here (${TOOLSET_SIZE:-a size shown before download}${TOOLSET_SIZE:+ under your home directory}): you are not at this Mac's console, so no aterm window will"
		elif [[ "$LEAN_REASON" == default ]]; then
			# The small download is the whole wait: the toolset arrives on
			# first launch, which the run's last toolset line says.
			echo "  then: aterm.app -> $DEST"
		else
			echo "  then: aterm.app -> $DEST; toolset excluded (--no-toolchain)"
		fi
	fi
	# THE ONE WRITE OUTSIDE THE INSTALL DIRS RIDES THE SAME PLAN, said here where
	# ^C still costs nothing: the installed aterm wires the PATH block after
	# placement (aterm pkg shell-setup --path), which names the file it edits.
	if [[ "$DO_PATH" -eq 1 ]]; then
		echo "  and: the installed aterm appends a PATH block to your shell profile (skip: --no-path)"
	fi
	download_release_asset "$TAG" "$ASSET_NAME" "$ASSET_SIZE" "$TMP/$ASSET_NAME" || exit 1
	SHA_GOT="$(shasum -a 256 "$TMP/$ASSET_NAME" | awk '{print $1}')"
	# Case-insensitive on purpose: an uppercase manifest spelling is the same
	# digest, not a mismatch (sha256_equal; the 64-hex shape was bound above).
	if ! sha256_equal "$SHA_GOT" "$ASSET_SHA"; then
		echo "install.sh: SHA-256 MISMATCH for $ASSET_NAME — refusing to install" >&2
		echo "  manifest: $ASSET_SHA" >&2
		echo "  download: $SHA_GOT" >&2
		exit 1
	fi
	echo "install.sh: sha256 verified (${SHA_GOT:0:12}…)"

	# Both containers land the bundle at "$MNT/aterm.app", so every check below —
	# codesign, the Team-ID designated requirement, Gatekeeper, the staged swap —
	# is identical for either.
	unpack_container "$TMP/$ASSET_NAME" "$MNT"
	[[ -d "$MNT/aterm.app" ]] || { echo "install.sh: no aterm.app inside $ASSET_NAME" >&2; exit 1; }

	# A verifier that did not answer is not a verdict on the bundle: say so
	# and stop, never "FAILED".
	verify_bundle() { # <unchecked property> <cmd> [arg...]
		local what="$1" s=0
		shift
		bounded "$VERIFY_WAIT_SECS" "$@" || s=$?
		if [[ $s -eq 124 ]]; then
			if [[ "$1" == spctl ]]; then
				echo "install.sh: Gatekeeper (spctl) did not answer within $VERIFY_WAIT_SECS s; the download was verified by SHA-256 but not $what, and nothing was installed — run install.sh again once it answers" >&2
			else
				echo "install.sh: macOS's code-signing service (trustd) did not answer within $VERIFY_WAIT_SECS s; the download was verified by SHA-256 but not $what, and nothing was installed — run install.sh again once it answers (a restart clears a wedged trustd)" >&2
			fi
			exit 1
		fi
		return $s
	}
	verify_bundle "its signature" codesign --verify --deep --strict "$MNT/aterm.app" || {
		echo "install.sh: code-signature verification FAILED — refusing to install" >&2
		exit 1
	}
	if [[ -n "$TEAM_WANT" ]]; then
		# Pin the whole Developer-ID chain for the team, not just a printed string —
		# the same designated requirement the in-app updater enforces (aterm-update).
		[[ "$TEAM_WANT" =~ ^[A-Za-z0-9]+$ ]] || {
			echo "install.sh: pinned team_id '$TEAM_WANT' is not alphanumeric — refusing to install" >&2
			exit 1
		}
		REQ="=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$TEAM_WANT\""
		verify_bundle "its Developer ID team" codesign --verify --deep --strict -R "$REQ" "$MNT/aterm.app" || {
			echo "install.sh: bundle is not Developer-ID signed by team $TEAM_WANT — refusing to install" >&2
			exit 1
		}
		verify_bundle "its notarization" spctl -a -t exec "$MNT/aterm.app" || {
			echo "install.sh: Gatekeeper assessment (notarization) FAILED — refusing to install" >&2
			exit 1
		}
		echo "install.sh: signature verified (Developer ID, Team ID $TEAM_WANT, notarized)"
	else
		# Never say "verified" here: an ad-hoc signature proves code integrity
		# since signing, not who published; with no pinned team, anyone's bundle
		# passes this check.
		echo "install.sh: UNVERIFIED publisher — ad-hoc signature accepted; no pinned Team ID."
		echo "install.sh:   Code integrity was checked, but nothing binds this bundle to a publisher."
	fi

	# Stage next to the destination, set the old bundle aside, rename the stage in
	# and delete the old copy last. For the instant between the two renames
	# nothing is at $DEST/aterm.app: an interrupt there restores the old one
	# (install_app_cleanup), though a SIGKILL or a crash leaves it at its hidden
	# set-aside name.
	# The staging name must be visible (no leading dot): Spotlight never indexes
	# content under dot-paths, and the final rename does not backfill the index.
	# And unpredictable: /Applications is shared, admin-writable space, so a
	# guessable staging name could be pre-created by another local user to ambush
	# the swap. Same rule for the set-aside path, and for the folder a root run
	# stages in (stage_bundle).
	place_bundle "$MNT/aterm.app" "$DEST"

	# Nudge Spotlight (a run as the account staged a folder whose name is no
	# bundle's, indexed as a plain directory until the rename made it an .app
	# bundle; a root run staged aterm.app in a folder of its own, and the
	# rename moved it) and LaunchServices, so the app is findable at its final
	# path the moment the installer exits.
	mdimport "$DEST/aterm.app" >/dev/null 2>&1 || true
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
		-f "$DEST/aterm.app" >/dev/null 2>&1 || true

	# Read here, not inside a command substitution, whose stdout is its pipe.
	local running=0 stdout_tty=0
	[[ -t 1 ]] && stdout_tty=1
	if pgrep -f '/aterm\.app/Contents/MacOS/aterm' >/dev/null 2>&1; then
		running=1
		echo "install.sh: NOTE: aterm is currently running — the new version takes effect on next launch"
	fi

	# Release the container and its mount now rather than at EXIT, which the
	# multi-minute toolset seed downstream would hold open. The EXIT trap repeats
	# this harmlessly.
	if [[ "${CONTAINER_KIND:-dmg}" == dmg && -d "$MNT" ]]; then
		hdiutil detach "$MNT" -quiet >/dev/null 2>&1 ||
			{ sleep 2; hdiutil detach "$MNT" -force -quiet >/dev/null 2>&1; } || true
	fi
	rm -rf "$TMP" 2>/dev/null || true

	echo "install.sh: installed aterm $VERSION -> $DEST/aterm.app"
	# A running aterm is not opened again: it takes the new version on its
	# next launch, as the NOTE above says.
	if [[ "$running" -eq 0 ]] &&
		app_opens_here "$(console_owner_uid)" "$(installer_uid)" "$stdout_tty"; then
		APP_TO_OPEN="$DEST/aterm.app"
	else
		echo "  launch:  open $(shell_word "$DEST/aterm.app")"
	fi
	# Checks and staging run in the app and in `aterm` terminal sessions, but the
	# apply (a re-exec) rides the window entry, so the line names the window.
	echo "  updates: automatic, installed while an aterm window is open — status: aterm update status (off: Settings ▸ Software Update)"
	INSTALLED_ANY=1
}

# --- the app half on Linux: the released binary, signature-verified --------
# Same raw ELF contract as the runtime updater; no archive extraction surface.
# Every byte comes from the release download host by its tag-specific URL, with
# no credential and no GitHub API request; the host's size and the signed
# appcast's must agree before placement. The release and source lanes share the
# canonical store layout.
install_linux_app() {
	echo "install.sh: installing $REPO_SLUG $TAG (linux-$LINUX_ARCH)"
	make_download_dir install_linux_app_cleanup

	local asset minimum maximum proof verified remote_asset source_tag host_size
	local name digest size build roster_seq
	# fetch_bounded <tag> <name> <min> <max> <destination>: the host's size, held to
	# <min>..<max>, then exactly that many bytes. A signature is exactly 64.
	fetch_bounded() {
		local from="$1" what="$2" low="$3" high="$4" to="$5" bytes
		bytes="$(release_asset_size "$from" "$what" "$high")" || exit 1
		decimal_in_closed_range "$bytes" "$low" "$high" || {
			echo "install.sh: $what in release $from is $bytes bytes, outside $low..$high — refusing" >&2
			exit 1
		}
		download_release_asset "$from" "$what" "$bytes" "$to" || exit 1
		chmod 600 "$to"
	}
	for asset in aterm-machines.toml aterm-machines.toml.sig aterm-appcast.toml aterm-appcast.toml.sig; do
		minimum=1
		maximum=5000000
		case "$asset" in
		*.sig) minimum=64; maximum=64 ;;
		aterm-machines.toml) maximum=65536 ;;
		esac
		source_tag="$TAG"; remote_asset="$asset"
		case "$asset" in
		aterm-machines.toml*) source_tag="${LINUX_HEAD_TAG:-$TAG}" ;;
		aterm-appcast.toml) remote_asset="${LINUX_MANIFEST_ASSET:-aterm-appcast.toml}" ;;
		aterm-appcast.toml.sig) remote_asset="${LINUX_MANIFEST_ASSET:-aterm-appcast.toml}.sig" ;;
		esac
		fetch_bounded "$source_tag" "$remote_asset" "$minimum" "$maximum" "$TMP/$asset"
	done
	if [[ "${LINUX_HEAD_TAG:-$TAG}" != "$TAG" ]]; then
		for asset in aterm-appcast.toml aterm-appcast.toml.sig; do
			minimum=1; maximum=5000000
			[[ "$asset" != *.sig ]] || { minimum=64; maximum=64; }
			proof="${asset/aterm-appcast/aterm-policy-appcast}"
			fetch_bounded "$LINUX_HEAD_TAG" "$asset" "$minimum" "$maximum" "$TMP/$proof"
		done
	fi

	# Authentication and existing durable floors precede the binary download.
	# An older native candidate is accepted only beneath the authenticated current
	# policy + current roster. Bad signatures never trigger another fallback.
	verified="$(linux_release_verify "$TMP" "$TAG" "$REPO_SLUG" "$LINUX_ARCH" "$STORE_DIR")" || exit 1
	IFS=$'\t' read -r name digest size build roster_seq <<<"$verified"
	[[ "$name" == "$LINUX_ASSET" ]] || { echo "install.sh: Linux asset identity changed" >&2; exit 1; }
	host_size="$(release_asset_size "$TAG" "$name" 536870912)" || exit 1
	[[ "$host_size" == "$size" ]] || { echo "install.sh: the download host's size for $name disagrees with the signed Linux size" >&2; exit 1; }
	echo "install.sh: downloading authenticated $name ($((size / 1000000)) MB)"
	download_release_asset "$TAG" "$name" "$size" "$TMP/$name" || exit 1

	# Recheck floors immediately before placement, then verify the exact file's
	# hash/size/ELF architecture before executing it. No archive parser is used.
	linux_release_verify "$TMP" "$TAG" "$REPO_SLUG" "$LINUX_ARCH" "$STORE_DIR" "$TMP/$name" >/dev/null || exit 1
	chmod 755 "$TMP/$name"
	local answered=0
	linux_candidate_answers_tag "$TMP/$name" "$TAG" || answered=$?
	case "$answered" in
	0) ;;
	1)
		echo "install.sh: signed binary reports a different release version (\"${LINUX_CANDIDATE_SAID//[^A-Za-z0-9 ._+-]/?}\", expected aterm ${TAG#v}); existing install kept" >&2
		exit 1
		;;
	*)
		echo "install.sh: signed Linux binary cannot run on this userland; existing install kept" >&2
		exit 1
		;;
	esac
	# The authenticated candidate owns the replacement/enrollment transaction.
	# It acquires the runtime lock, rechecks all durable policy, records rollback
	# identity and atomically replaces the target. Shell placement followed by
	# `enable` would discard the old binary if enrollment failed or raced updates.
	"$TMP/$name" update install --target "$STORE_DIR/aterm" --proof-dir "$TMP" --candidate "$TMP/$name" </dev/null || {
		echo "install.sh: install refused by the updater — see: aterm update status" >&2
		exit 1
	}
	expose_store_binary
	echo "install.sh: installed aterm ${TAG#v} -> $STORE_DIR/aterm"
	say_store_command ""
	INSTALLED_ANY=1
	LINUX_APP_INSTALLED=1
}

# --- the cli half: the one `aterm` command, preferring the installed bundle ----
# Current releases use Contents/MacOS/aterm for the session, window and product
# commands. The preferred install symlinks that executable; the previous separate
# CLI toolset is accepted below for compatibility. Neither bundle lane builds
# source, and the link tracks app updates at the same path.

install_cli() {
	local target="" o
	[[ "$LINUX_APP_INSTALLED" -eq 1 ]] || target="$(find_bundle_cli)"
	if [[ "$LINUX_APP_INSTALLED" -eq 1 ]]; then
		# The app half already landed the released Linux binary in the store and
		# exposed the `aterm` symlink (place_store_binary); a source build here
		# would replace that verified release with an unverified dev build.
		PLACED_ATERM="$STORE_DIR/aterm"
	elif [[ -n "$target" ]]; then
		PLACED_ATERM="$target"
		# One name on PATH (the [workspace.metadata.atpkg] expose declaration):
		# `aterm` alone — a symlink at the bundle's one binary (or, against a
		# previous-generation bundle, its aterm-cli front door).
		claim_bin_aterm || return 0
		ln -sfn "$target" "$BIN_DIR/aterm"
		BIN_ATERM_EXPOSED=1
		echo "install.sh: linked $(first_line_bounded "$BIN_DIR/aterm" --version) -> $BIN_DIR/aterm (follows the app's updates)"
	elif [[ -n "$CLI_CARGO_SKIP" ]]; then
		# Neither source is available here: no installed bundle ships the
		# toolset (older release, or no app), and the build fallback is
		# impossible for the pre-flighted reason.
		o="$(root_bundle_unsealed)"
		if [[ -n "$o" ]]; then
			echo "install.sh: SKIPPED the CLI (aterm): $(root_bundle_refusal "$o" 1), and $CLI_CARGO_SKIP" >&2
		elif [[ "$(uname -s)" == Darwin ]]; then
			echo "install.sh: SKIPPED the CLI (aterm): no installed aterm.app ships the toolset (newer releases do — update or reinstall the app first), and $CLI_CARGO_SKIP" >&2
		else
			# No bundle exists off macOS: the released binary is the app half's,
			# and this run did not install it (its own line says why).
			echo "install.sh: SKIPPED the CLI (aterm): this run installed no released aterm (see the app line), and $CLI_CARGO_SKIP" >&2
		fi
		return 0
	else
		o="$(root_bundle_unsealed)"
		[[ -z "$o" ]] || echo "install.sh: the CLI is built from source: $(root_bundle_refusal "$o")"
		install_cli_from_source
	fi
	retire_exposed_siblings
	setup_completions_and_man
	# The hand-edit PATH hint waits for the end of the run, which prints it only
	# when no managed PATH block exists (wire_shell_path edits that profile).
	CLI_PATH_HINT_WANTED=1
	INSTALLED_ANY=1
}

install_cli_from_source() {
	echo "install.sh: building aterm from $ROOT at $(checkout_git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null || echo '(not a git checkout)')"
	# Pin the target dir (a global CARGO_TARGET_DIR / build.target-dir redirect
	# would strand the fresh binaries elsewhere) but NEVER a --target triple:
	# cargo withholds [target.*] rustflags from HOST units (build scripts, proc
	# macros, and their dependencies) whenever an explicit target is set — the
	# flag, CARGO_BUILD_TARGET, or [build] target — and this repo compiles with
	# Trust, whose rustup lane carries its off-switch in exactly those rustflags
	# (CARGO_UNVERIFIED_LANE). Under a pinned triple the host units
	# lose them, and on rustup's lane verify strictly and fail closed (first
	# casualty: shlex, under cc). A plain host build applies them to every unit
	# and lands the binary
	# at the ONE deterministic path target/release/. CARGO_BUILD_TARGET is
	# unset for the same reason; a [build] target redirect in user cargo
	# config cannot be neutralized from here, so the freshness proof below
	# turns that into a loud abort instead of a stale (or missing) install.
	local rel="$ROOT/target/release"
	# The rename at the end replaces the store's aterm: asked here first, so one
	# that stays costs no build; place_store_binary asks again before the rename.
	may_replace "$STORE_DIR/aterm" || exit 1
	# Existence-after-build IS the freshness proof: cargo always re-links a
	# deleted output, so a redirected build can never leave a previous build's
	# binary here to be silently installed.
	rm -f "$rel/aterm"
	# ONE shipped binary carries the session, window and product commands
	# in-process (`-p aterm` pulls their library graph). Development-only tools
	# retain separate executables. cargo_build_pinned
	# runs the pre-flighted $CARGO_BIN (find_cargo_and_rustup +
	# cargo_honours_pin) on the lane the pre-flight took: the aterm-managed
	# store's targo on its explicit `--unverified` lane, the store's bin dir
	# first on PATH and RUSTC/CARGO_BUILD_RUSTC scrubbed, so its compiler is
	# the trustc beside it ($CARGO_RUSTC); else rustup's proxy with
	# RUSTUP_TOOLCHAIN scrubbed as the probes had it, and $CARGO_HOME/bin
	# then its own dir first on PATH so that cargo's `rustc` is the proxy
	# proved with it ($CARGO_RUSTC). It never runs PATH's bare `cargo` or
	# `rustc`, which differ on a Mac whose login shell put Homebrew's ahead
	# of rustup's.
	echo "install.sh: toolchain ${CARGO_TOOLCHAIN%% (*}: $CARGO_VERSION ($CARGO_BIN)"
	local built=0
	cargo_build_pinned "$ROOT" || built=$?
	if [[ "$built" -ne 0 ]]; then
		# Under `set -e` a failed build would end the run with cargo's own
		# status and no line of this script's: say what it means here.
		echo "install.sh: the source build failed (cargo exit $built, its error above) — the \`aterm\` command was not installed" >&2
		exit 1
	fi
	if [[ ! -x "$rel/aterm" ]]; then
		echo "install.sh: the build finished but produced no $rel/aterm — a [build] target in your cargo config redirected it; remove that setting (or install the released app instead: tools/install.sh --no-cli)" >&2
		exit 1
	fi
	# STORE_DIR was pre-flighted (created + writability-checked) before the build.
	place_store_binary "$rel/aterm"
	PLACED_ATERM="$STORE_DIR/aterm"
	echo "install.sh: installed $(first_line_bounded "$STORE_DIR/aterm" --version) -> $STORE_DIR"
	say_store_command ""
}

expose_store_binary() {
	# argv0 compatibility aliases point to the same executable, as in the app
	# bundle, so older scripts invoking aterm-ctl, atpkg, aterm-fleet or
	# aterm-drive still reach their in-process entry points.
	local alias
	for alias in $STORE_ALIASES; do
		ln -sfn aterm "$STORE_DIR/$alias"
	done
	# The entry on PATH only when it is ours to write (claim_bin_aterm): a
	# leftover regular file from a pre-store install is replaced — it would
	# shadow the store — and anything foreign is left, SKIPPED. Which of the two
	# happened is BIN_ATERM_EXPOSED, so no caller claims a PATH entry it left.
	BIN_ATERM_EXPOSED=0
	claim_bin_aterm || return 0
	ln -sfn "$STORE_DIR/aterm" "$BIN_DIR/aterm"
	BIN_ATERM_EXPOSED=1
}

# The store lanes' PATH line, true either way: the one command when
# expose_store_binary linked it, else where the command is and why PATH's is not it.
say_store_command() { # <what the command is>
	if [[ "$BIN_ATERM_EXPOSED" -eq 1 ]]; then
		echo "  ONE command on PATH: $BIN_DIR/aterm$1"
	else
		echo "  NOT on PATH: $BIN_DIR/aterm is not this installer's (SKIPPED above) — run $STORE_DIR/aterm, or move that file aside and re-run"
	fi
}

# Whether $BIN_DIR/aterm may be (re)written, per bin_aterm_verdict: absent or
# ours, yes; a pre-store aterm file, yes — removed first, and said; anything
# else, no — SKIPPED with the uninstaller's line and left exactly as it is.
claim_bin_aterm() {
	local p="$BIN_DIR/aterm" verdict
	verdict="$(bin_aterm_verdict "$p" "$STORE_DIR")"
	case "$verdict" in
	absent | ours) return 0 ;;
	pre-store)
		rm -f "$p" || return 1
		echo "install.sh: replaced $p — a regular file from a pre-store install that answers as aterm; left in place it would shadow the managed command"
		return 0
		;;
	esac
	ownership_skip "$p" "${verdict#foreign }"
	return 1
}

# Man pages, shell completions and the Linux desktop entry, by the aterm just
# placed (`aterm pkg shell-setup`): completions only for the `aterm` PATH
# resolves to (BIN_ATERM_EXPOSED — otherwise they would complete someone else's
# binary), man pages and icons from a checkout, else (a piped run has none —
# Linux delivery is one signed binary) the ones the binary carries: its icons
# whenever no --icons is named, its page by --own-man, asked only of a binary
# whose shell-setup names the flag (an older one would refuse the whole call).
# Best effort, never fatal: the binary is in place either way.
setup_completions_and_man() {
	local aterm want=0 own_man=0
	local args=(pkg shell-setup --bin-dir "$BIN_DIR" --man-root "${ATERM_MAN_DIR:-$HOME/.local/share/man}")
	if [[ "$BIN_ATERM_EXPOSED" -eq 1 ]]; then
		args+=(--completions)
		want=1
	else
		echo "install.sh: shell completions: skipped — $BIN_DIR/aterm is not this installer's"
	fi
	if [[ -n "$ROOT" && -d "$ROOT/man" ]]; then
		args+=(--man "$ROOT/man")
		want=1
	else
		own_man=1
		want=1
	fi
	if [[ "$(uname -s)" == Linux ]]; then
		args+=(--desktop)
		want=1
		[[ -n "$ROOT" && -d "$ROOT/assets/linux/icons/hicolor" ]] &&
			args+=(--icons "$ROOT/assets/linux/icons/hicolor")
	fi
	[[ "$want" -eq 1 ]] || return 0
	aterm="$(setup_aterm shell-setup "$BIN_DIR" "$STORE_DIR" ${DEST:+"$DEST"})"
	if [[ -z "$aterm" ]]; then
		echo "install.sh: NOTE: no installed aterm has \`aterm pkg shell-setup\` yet — completions, man pages and the desktop entry come with it: run \`aterm pkg shell-setup --completions\` after its next update" >&2
		return 0
	fi
	if [[ "$own_man" -eq 1 ]]; then
		local usage
		usage="$("$aterm" pkg help shell-setup </dev/null 2>/dev/null)" || usage=""
		[[ "$usage" == *--own-man* ]] && args+=(--own-man)
	fi
	"$aterm" "${args[@]}" </dev/null || true
}

cli_path_hint() {
	local rc_name f
	case ":${ROOT_LOOKUP_PATH-$PATH}:" in
	*":$BIN_DIR:"*) ;;
	*)
		case "${SHELL##*/}" in
		zsh) rc_name="$HOME/.zshrc" ;;
		bash)
			rc_name="$HOME/.bashrc"
			# macOS terminals spawn login shells, and login bash reads
			# ~/.bash_profile (then .bash_login, then .profile) — never
			# .bashrc. Pointing a macOS bash user at .bashrc names a file no
			# new terminal will source.
			if [[ "$(uname -s)" == Darwin ]]; then
				rc_name="$HOME/.bash_profile"
				for f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
					[[ -f "$f" ]] && { rc_name="$f"; break; }
				done
			fi
			;;
		*) rc_name="your shell profile" ;;
		esac
		echo "install.sh: NOTE: $BIN_DIR is not on your PATH — add it in $rc_name, e.g.:" >&2
		echo "  export PATH=$(shell_word "$BIN_DIR"):\"\$PATH\"" >&2
		;;
	esac
}

# --- the toolset half: what "installing aterm" is for ---------------------------
# docs/GOLDEN-INSTALL-PATH.md §1: "Installing aterm installs all those packages."
#
# `atpkg seed` makes every decision, not this script: it records adoption, lays
# the pending stubs, and answers every "cannot seed" case as success with a
# stable line (`seed-unusable:` for a machine the signed index publishes nothing
# for, a plain note for [packages].auto_install = false or a set a previous
# `uninstall --all` declined). So a non-zero exit is a real failure, with one
# exception, ATPKG_CONTENDED_EXIT: another atpkg run holds the store lock
# (usually the open app's launch-time pass), and the script stands aside and
# says so.
install_toolchain() {
	local aterm_bin
	aterm_bin="$(toolset_driver)"
	if [[ "$aterm_bin" != "run "* ]]; then
		echo "install.sh: SKIPPED the ALab toolset: ${aterm_bin#skip }" >&2
		return 0
	fi
	aterm_bin="${aterm_bin#run }"

	# One line on every lane: where the builds come from (a pinned pre-v0.63
	# app's sealed payload, else the signed network index) and whether this
	# machine has any is `pkg seed`'s to decide and to say.
	echo "install.sh: installing the ALab toolset (aterm pkg seed)"
	local rc=0
	"$aterm_bin" pkg seed || rc=$?
	if [[ $rc -eq $ATPKG_CONTENDED_EXIT ]]; then
		# Stood aside: another atpkg run holds the store lock — usually the
		# app's own launch-time pass, which finishes the job, but a person's
		# `aterm pkg` holds the same lock and install.sh cannot tell which, so
		# the line names no holder. A stand-down is not "nothing was
		# installed": a wrapper keyed on the exit code must not read it as a
		# failed install.
		echo "install.sh: another aterm pkg run is using the ALab toolset — rerun later: $aterm_bin pkg seed" >&2
		TOOLCHAIN_RAN=1
		return 0
	elif [[ $rc -ne 0 ]]; then
		# Non-fatal: the terminal is installed and working, and `aterm pkg seed`
		# is re-runnable at any time.
		echo "install.sh: aterm is installed; the ALab toolset did not install (exit $rc) — rerun: $aterm_bin pkg seed" >&2
		return 0
	fi
	TOOLCHAIN_RAN=1

	# Bring the seeded builds current now: the GUI does so on its own loop, which
	# never runs for someone who installed from a terminal and never opens the
	# app. Only what drifted is fetched; an offline machine keeps what it has.
	echo "install.sh: updating the ALab toolset (aterm pkg update)"
	rc=0
	"$aterm_bin" pkg update || rc=$?
	if [[ $rc -eq $ATPKG_CONTENDED_EXIT ]]; then
		# Stood aside, as for the seed above: no holder is named.
		echo "install.sh: another aterm pkg run is using the ALab toolset — rerun later: $aterm_bin pkg update" >&2
	elif [[ $rc -eq $ATPKG_OFFLINE_EXIT ]]; then
		echo "install.sh: NOTE: could not reach the index to check for newer builds." >&2
		echo "  anything already installed keeps working; retry later with: $aterm_bin pkg update" >&2
	elif [[ $rc -ne 0 ]]; then
		echo "install.sh: NOTE: the toolset update failed (exit $rc) — anything already installed keeps working." >&2
		echo "  retry with: $aterm_bin pkg update" >&2
	fi
	# Deliberately does not set INSTALLED_ANY. This half runs on top of an app
	# the other halves (or a previous run) placed, and on an already-seeded
	# machine it correctly does nothing — claiming an install here would turn
	# the "nothing was installed" failure into a false success.
}

# --- PATH, for shells that are not aterm ------------------------------------------
# atpkg writes a per-shell hook into ~/.aterm/shell.d on every install, and its
# seed, install and update passes append a block sourcing it to each EXISTING
# ~/.zshrc, ~/.bashrc, ~/.bash_profile or ~/.config/fish/config.fish, creating
# only the login shell's own rc when it is absent (the account record's shell,
# never $SHELL). The installed aterm's `pkg shell-setup --path` adds ONE
# marker-fenced block — the hook, and $BIN_DIR when PATH lacks it — to the rc
# that same login shell reads (a macOS login bash's first of ~/.bash_profile,
# ~/.bash_login and ~/.profile that exists; fish's under $XDG_CONFIG_HOME), made
# when absent, which `self-uninstall` undoes. PATH_BLOCK_WROTE says a block
# stands; without a runnable aterm nothing is written, and the hand-edit hint
# below says what to add.
wire_shell_path() {
	local aterm args=(pkg shell-setup --path --bin-dir "$BIN_DIR")
	[[ "${TOOLCHAIN_DEFERRED:-0}" -eq 1 ]] && args+=(--toolset-coming)
	aterm="$(setup_aterm shell-setup "$BIN_DIR" "$STORE_DIR" ${DEST:+"$DEST"})"
	if [[ -z "$aterm" ]]; then
		echo "install.sh: NOTE: no installed aterm can wire your shell profile — run \`aterm pkg shell-setup --path\` once one is installed" >&2
		return 0
	fi
	if "$aterm" "${args[@]}" </dev/null; then
		PATH_BLOCK_WROTE=1
	fi
}

# --- run what's possible, skip the rest loudly, fail only if nothing ran -------
if [[ "$DO_APP" -eq 1 ]]; then
	if [[ -n "$APP_SKIP" ]]; then
		echo "install.sh: SKIPPED the app: $APP_SKIP" >&2
	elif [[ -n "$APP_ALREADY" ]]; then
		# Satisfied, not skipped: the elected release is already at the
		# destination, so the run must not report "nothing was installed".
		echo "install.sh: aterm $APP_ALREADY already installed — skipping download (pass --version $APP_ALREADY to force)"
		INSTALLED_ANY=1
	elif [[ "$LINUX_RELEASE" -eq 1 ]]; then
		install_linux_app
	else
		install_app
	fi
fi
if [[ "$DO_CLI" -eq 1 ]]; then
	if [[ -n "$CLI_SKIP" ]]; then
		echo "install.sh: SKIPPED the CLI (aterm): $CLI_SKIP" >&2
	else
		install_cli
	fi
fi
# After the CLI half: the seed lane is driven through the installed `aterm`, so
# it needs the symlink (or the bundle) to exist. Before the PATH half, which
# points at a hook that exists only once atpkg has laid the toolset down.
#
# Not gated on the app half having run: `--no-app` is the normal way to say
# "the app is already installed", and install_toolchain proves an `aterm`
# exists and skips with a reason when it does not.
if [[ "$DO_TOOLCHAIN" -eq 1 ]]; then
	if [[ "${TOOLCHAIN_DEFERRED:-0}" -eq 1 ]]; then
		# The lean default (docs/DESIGN-streaming-batteries-2026-08-23.md §7):
		# first launch provisions the toolset per program with live progress,
		# and the GUI's own seed pass records adoption there — the audited
		# consent seam (atpkg cmd_seed). A headless `pkg seed` here would block
		# on the full download and move the consent write out of that seam.
		# TOOLCHAIN_DEFERRED is set only by a fresh lean-default app install
		# whose user holds this Mac's console (toolset_deferral); every other
		# lane (--no-app, an already-current app, a pinned pre-lean DMG,
		# Linux, an install over SSH) runs install_toolchain here. A root run
		# for someone else stays deferred: no toolset is ever seeded as root.
		if [[ "${TOOLSET_AS_ROOT:-0}" -eq 1 ]]; then
			echo "install.sh: toolset: $ROOT_TOOLSET_LINE"
		elif [[ -z "$APP_TO_OPEN" ]]; then
			# An app this run opens says it in open_installed_app's line.
			echo "install.sh: toolset: installs on aterm's first launch (from a terminal instead: aterm pkg install --default-set)"
		fi
	else
		if [[ "${TOOLSET_NO_WINDOW:-0}" -eq 1 ]]; then
			echo "install.sh: toolset: you are not at this Mac's console, so no aterm window will install it — installing it here"
		fi
		install_toolchain
	fi
else
	# --no-toolchain persists (persist_no_toolchain): the app's first launch
	# reads the same key and leaves the toolset alone. A write that fails is
	# said, never fatal — the halves already installed stand.
	persist_no_toolchain "$(aterm_config_path)" ||
		echo "install.sh: NOTE: could not write $(aterm_config_path); set \`[packages] auto_install = false\` there by hand to keep the toolset out" >&2
fi
if [[ "$DO_PATH" -eq 1 ]]; then
	wire_shell_path
fi
# The deferred PATH hint: when the managed block (which carries $BIN_DIR)
# exists, telling the user to hand-edit the same profile is noise; every path
# where no block does — --no-path, no hook laid down (nothing installable for
# this machine, failed seed), an unrecognised shell, an unwritable rc — still
# ends with the actionable NOTE. cli_path_hint itself re-checks PATH
# membership, so it stays silent when $BIN_DIR is already reachable.
if [[ "$CLI_PATH_HINT_WANTED" -eq 1 && "$PATH_BLOCK_WROTE" -eq 0 ]]; then
	cli_path_hint
fi
if [[ -n "$APP_TO_OPEN" ]]; then
	open_installed_app "$APP_TO_OPEN" \
		"$([[ "$DO_TOOLCHAIN" -eq 1 && "${TOOLCHAIN_DEFERRED:-0}" -eq 1 ]] && echo 1 || echo 0)"
	APP_TO_OPEN=""
fi
if [[ "$INSTALLED_ANY" -eq 0 ]]; then
	# A toolchain/PATH-only invocation (--no-app --no-cli) installs nothing by
	# INSTALLED_ANY's deliberately narrow definition even when it did exactly
	# what was asked; a completed seed or a standing managed PATH block is that
	# run's success, so it must not exit "nothing was installed".
	if [[ "$DO_APP" -eq 0 && "$DO_CLI" -eq 0 ]] &&
		[[ "$TOOLCHAIN_RAN" -eq 1 || "$PATH_BLOCK_WROTE" -eq 1 ]]; then
		exit 0
	fi
	echo "install.sh: nothing was installed" >&2
	exit 1
fi
}

# The guard's other half (see the whole-file execution guard above). This call
# is the file's last line on purpose, and its brace closes the group the file
# opens above its first command: nothing runs unless every byte of the file
# did, so a truncated `curl … | bash` is inert.
main "$@"; }
