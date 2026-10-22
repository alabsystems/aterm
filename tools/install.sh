#!/usr/bin/env bash
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# install.sh — the aterm install: the released aterm.app AND the `aterm`
# command (ONE name on PATH; it fronts every verb — aterm help / ctl / pkg /
# fleet / drive), in one command. Current macOS releases are lean apps; Linux
# releases provide one executable. The ALab toolchain installs separately, on a
# lean app's first launch with live progress unless excluded.
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
#         same download host. Python 3.11+ and OpenSSL verify the pinned
#         paper-master roster, live machine signature, signed target
#         name/size/hash, existing rollback floors, and ELF architecture before
#         any execution. A checksum-only legacy tarball is never an automatic
#         fallback. The installed runtime then independently verifies the proof
#         and enrolls this managed binary for signed updates. A source fallback
#         is NOT automatically enrolled.
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
#         to the app's first launch); PATH — one marker-fenced block in your
#         shell profile. Each has its --no-… flag.
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
#   tools/install.sh --no-toolchain                   # lean zip, toolset excluded, and it
#                                                     # stays excluded: writes
#                                                     # `[packages] auto_install = false`
#                                                     # into ~/.config/aterm/aterm.toml
#                                                     # (never over a value already there)
#   tools/install.sh --no-path                        # skip this script's PATH block (the
#                                                     # toolset still adds its own to an
#                                                     # existing shell rc)
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
set -euo pipefail

# Is this file really on disk (vs piped/`bash -s`, where BASH_SOURCE is unusable —
# and on bash >= 5 points at the bash binary itself, so -r alone is not enough)?
self_on_disk() {
	[[ -r "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == *install.sh ]]
}

# The same page whether the script is on disk or piped. The header comment
# above is the design record for readers of this file, not the usage.
usage() {
	cat <<'USAGE'
usage: install.sh [--no-app] [--no-cli] [--no-toolchain] [--no-path] [--version X.Y.Z] [--dry-run]
       install.sh --uninstall [--dry-run]

Installs aterm.app (macOS), the `aterm` command, and the ALab toolset.
On Linux the app is the signed `aterm` binary, which updates itself (aterm update status).

  --no-app          leave the app alone (Linux: the signed `aterm` binary)
  --no-cli          leave the `aterm` command alone
  --no-toolchain    skip the ALab toolset, and keep it skipped: writes
                    [packages] auto_install = false into aterm.toml
                    ($XDG_CONFIG_HOME/aterm, default ~/.config/aterm)
  --no-path         skip this script's PATH block (the toolset still adds its
                    own to an existing shell rc; delete it and it stays deleted;
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
		# raw ELF per architecture, and the retired checksum-only tarball pair,
		# which stays recognizable for archive diagnostics; recognizing a name
		# does not authorize installation. An explicit allowlist rather than a
		# loosened pattern: each suffix is exactly as constrained as `.dmg`.
		[[ "$name" =~ ^aterm-[0-9]+(\.[0-9]+)+\.dmg$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-mac\.zip$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-linux-(aarch64|x86_64)$ ||
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
	(
		got=0 pct=0 last=-1
		while :; do
			sleep 2
			got="$(wc -c <"$destination" 2>/dev/null | tr -d '[:space:]')" || got=""
			[[ -n "$got" ]] || continue
			pct=$((got * 100 / total))
			[[ "$pct" != "$last" ]] || continue
			last="$pct"
			printf '\r  %s MB / %s MB (%s%%)  ' \
				"$((got / 1000000))" "$((total / 1000000))" "$pct" >&2
		done
	) &
	DOWNLOAD_METER_PID=$!
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

# One bare GitHub name segment (an owner OR a repo): the alphabet and bounds of
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
# system's code assessment, seconds on a loaded machine.
VERIFY_WAIT_SECS=60
bounded() {
	local secs="$1"
	shift
	(
		set -m
		"$@" 2>&3 </dev/null &
		pid=$!
		set +m
		trap 'kill -KILL -- "-$pid" 2>/dev/null; exit 130' INT
		trap 'kill -KILL -- "-$pid" 2>/dev/null; exit 143' TERM
		trap 'kill -KILL -- "-$pid" 2>/dev/null; exit 129' HUP
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
	bounded "$VERIFY_WAIT_SECS" codesign --verify --deep --strict "$app" >/dev/null 2>&1 || s=$?
	[[ $s -eq 0 ]] || { [[ $s -eq 124 ]] && return 124; return 1; }
	[[ -n "$team" ]] || return 0
	[[ "$team" =~ ^[A-Za-z0-9]+$ ]] || return 1
	bounded "$VERIFY_WAIT_SECS" codesign --verify --deep --strict \
		-R "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$team\"" \
		"$app" >/dev/null 2>&1 || s=$?
	[[ $s -eq 0 ]] || { [[ $s -eq 124 ]] && return 124; return 1; }
	bounded "$VERIFY_WAIT_SECS" spctl -a -t exec "$app" >/dev/null 2>&1 || s=$?
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
# directory. Only --dry-run consults this; the real pre-flight keeps `mkdir -p`
# itself as the probe.
would_create_writable() {
	local d="$1"
	while [[ -n "$d" && ! -e "$d" ]]; do
		case "$d" in
		*/*) d="${d%/*}" ;;
		*) return 1 ;; # relative with no existing component: nothing to anchor to
		esac
	done
	[[ -n "$d" ]] || d=/
	[[ -d "$d" && -w "$d" ]]
}

# Destination pre-flight, shared by every half: create the dir(s) and prove
# them writable. `mkdir -p` succeeds on an EXISTING unwritable dir, hence the
# explicit -w pass. Under install-mode --dry-run — whose contract is ZERO
# mutations — nothing is created: the verdict comes from would_create_writable
# instead, so a dry run reports the same skip reasons the real run would hit
# without manufacturing the directories along the way.
#
# What it makes, it makes writable by this user alone: the caller's umask with
# group and other write added. Ubuntu's user-private-group umask (002) would
# otherwise leave ~/.local, ~/.local/lib … 0775, a chain the Linux lane's own
# verifier (check_install_path) and the updater then judge as writable by
# others. Read bits stay the caller's, so a shared system store
# (ATERM_STORE_DIR=/opt/aterm) stays readable by the users who run it.
ensure_dirs_writable() { # <dir>...
	local d
	if [[ "${DRY_RUN:-0}" -eq 1 ]]; then
		for d in "$@"; do
			would_create_writable "$d" || return 1
		done
		return 0
	fi
	(umask "$(printf '%04o' $((8#$(umask) | 8#022)))" && mkdir -p "$@") 2>/dev/null || return 1
	for d in "$@"; do
		[[ -w "$d" ]] || return 1
	done
}

# Six random bytes as hex. od reads EXACTLY its byte count, so pipefail never
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
# arm64 toolchain, `aterm-<v>-x86_64.dmg` the Intel one) was RETIRED
# 2026-08-26: no release cuts the pair. A release that names no zip in its
# manifest predates the lean container, so the default falls back to the bare DMG
# that release was cut for. Only a fork's such release installs: every
# official one (v0.6.0–v0.11.0) carries an empty team_id, which
# required_team_for refuses before the download, and with ATERM_TEAM_ID pinning
# the official team v0.11.0 was downloaded and refused at the signature check
# (measured 2026-10-20 in a macOS 27 guest).
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
# One file's PHYSICAL path: symlinks followed hop by hop with readlink (the
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
#   1. `aterm pkg which targo` when `aterm` is in reach: its last `→` arm, up
#      to the ` — ` note. A targo ahead of the managed bin/ on PATH makes atpkg
#      answer the SHADOWED shape (`targo → <stray> — managed <build> — SHADOWED
#      by <stray> — type alab-targo for the managed one`), so a SHADOWED answer
#      is re-asked as `alab-targo`; an alias that is not laid falls through.
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
	pin="$(gate_pinned_channel "$root")"
	if [[ "$pin" != trust ]]; then
		MANAGED_DECLINE="rust-toolchain.toml pins '${pin:-nothing}', not 'trust', so the aterm-managed Trust store is not this checkout's toolchain"
		return 1
	fi
	if aterm="$(command -v aterm 2>/dev/null)" && [[ -n "$aterm" ]]; then
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
		IFS=: read -ra dirs <<<"${PATH:-}"
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
# trust` where an `aterm` is in reach to run it (on PATH, or install.sh's
# $BIN_DIR), else the one path left on a machine with no `aterm` — rustup plus
# the hand-linked Trust tarballs, since rustup alone cannot download the pin.
# Trust is published for macOS only (atpkg's trust package and the toolchain
# tarballs alike), so elsewhere the remedy is rustup and a Trust built from
# source (trust_link_remedy).
gate_trust_remedy() {
	if [[ "$(uname -s)" != Darwin ]]; then
		printf '%s\n' "install rustup (https://rustup.rs), then $(trust_link_remedy)"
	elif command -v aterm >/dev/null 2>&1 || [[ -n "${BIN_DIR:-}" && -x "${BIN_DIR:-}/aterm" ]]; then
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
# which the store cannot serve, rustup's remedy alone.
find_cargo_and_rustup() { # [root]
	local root="${1:-}" home="${CARGO_HOME:-$HOME/.cargo}" beside="" seen managed=1 declined=""
	CARGO_BIN=""
	RUSTUP_BIN=""
	CARGO_LANE=""
	CARGO_RESOLVE_REASON=""
	MANAGED_DECLINE=""
	MANAGED_SEEN=""
	if [[ -n "$root" ]]; then
		if find_managed_targo "$root"; then
			CARGO_LANE=store
			CARGO_BIN="$MANAGED_TARGO"
			return 0
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
	elif ! RUSTUP_BIN="$(command -v rustup 2>/dev/null)" || [[ -z "$RUSTUP_BIN" ]]; then
		RUSTUP_BIN=""
	fi
	if [[ -n "$RUSTUP_BIN" ]]; then
		beside="$(dirname "$RUSTUP_BIN")/cargo"
	fi
	if [[ -n "$beside" && -x "$beside" ]]; then
		CARGO_BIN="$beside"
	elif [[ -x "$home/bin/cargo" ]]; then
		CARGO_BIN="$home/bin/cargo"
	elif ! CARGO_BIN="$(command -v cargo 2>/dev/null)" || [[ -z "$CARGO_BIN" ]]; then
		CARGO_BIN=""
		if [[ -n "$RUSTUP_BIN" && "$managed" -eq 1 ]]; then
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
# .cargo/config.toml carries no off-switch (docs/ISSUE-trust-unlaned-offswitch-2026-10-07.md).
# The store's bin dir goes first on PATH, never rustup's, so a build script
# that runs a bare `rustc` finds the store's sibling. An ambient RUSTC or
# CARGO_BUILD_RUSTC would steer the compiler, so both are scrubbed: the
# compiler the gate proved ($CARGO_RUSTC, trustc beside targo) is the one that
# runs.
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
# CARGO_UNVERIFIED_LANE with them) from host units.
CARGO_UNVERIFIED_LANE="target.'cfg(trust_verify)'.rustflags=[\"-Ztrust-verify=off\"]"
cargo_build_pinned() { # <root>
	if [[ "${CARGO_LANE:-}" == store ]]; then
		(cd "$1" && env -u CARGO_BUILD_TARGET -u RUSTUP_TOOLCHAIN -u RUSTC -u CARGO_BUILD_RUSTC PATH="$(dirname "$CARGO_BIN"):$PATH" "$CARGO_BIN" --unverified build --release -p aterm --target-dir "$1/target")
	else
		(cd "$1" && env -u CARGO_BUILD_TARGET -u RUSTUP_TOOLCHAIN PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$(dirname "$CARGO_BIN"):$PATH" "$CARGO_BIN" build --release -p aterm --target-dir "$1/target" --config "$CARGO_UNVERIFIED_LANE")
	fi
}
# The rustup lane in the block above serves a tree whose pin names a stock channel,
# or a `trust` link rustup resolves with no aterm-managed store in reach. The public
# snapshot pins `trust` like the dev tree (2026-09-29), so on a machine with aterm
# both take the store lane. A dev build of the tree is `targo --unverified ship
# dev-app`, on the Trust toolchain the release cutter resolves.

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
# cannot run here (a non-zero exit or no answer in 30 s; what it printed on stderr,
# a missing glibc symbol say, reaches the person); LINUX_CANDIDATE_SAID is the line
# it gave. stdin is /dev/null: under `curl … | bash` the inherited stdin
# is the rest of this script. Called directly, never in $(…).
LINUX_CANDIDATE_SAID=""
linux_candidate_answers_tag() { # <exe> <tag>
	local nl=$'\n' said
	LINUX_CANDIDATE_SAID=""
	said="$(timeout 30 "$1" --version </dev/null)" || return 2
	LINUX_CANDIDATE_SAID="${said%%"$nl"*}"
	case "$LINUX_CANDIDATE_SAID" in
	"aterm ${2#v}" | "aterm ${2#v}+"* | "aterm ${2#v} "*) return 0 ;;
	*) return 1 ;;
	esac
}

# Linux has no Apple bootstrap chain. Verify the SAME paper-master -> live
# machine -> appcast chain as update-core before interpreting an artifact claim.
# Kept inside the one-file installer so the curl|bash path never fetches an
# unverified executable helper, and this heredoc is the verifier's only copy.
# Python parses TOML; OpenSSL performs Ed25519; ctypes reads the two facts the
# standard library lacks (statfs, getpwent_r).
# crates/aterm-update/tests/install_sh runs this interpreter over Rust-signed
# synthetic releases (swapping only the pin, never through a production
# override) and holds it to the updater's chain and to the committed root pin.
linux_release_verify() {
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
            stderr=subprocess.DEVNULL, timeout=10, check=False,
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

# --- OWNERSHIP: one set of tests for BOTH directions ---------------------------
#
# The uninstaller always refused to delete what it did not place; the installer
# used to move aside whatever sat at $DEST/aterm.app and `ln -sfn` over whatever
# sat at $BIN_DIR/aterm without looking. These are the uninstaller's own tests,
# hoisted so the install path asks the same questions and prints the same
# SKIPPED line (docs/AUDIT-distribution-channel-2026-08-31.md, "ownership
# asymmetry"). Defined above the library guard so
# crates/aterm-update/tests/install_sh pins them.

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
	elif [[ -f "$p" && -x "$p" ]] && first_line_bounded "$p" --version | grep -q '^aterm '; then
		echo pre-store
	else
		echo "foreign not a symlink — a hand-built binary this installer did not place"
	fi
}

# first_line_bounded <exe> [args…] -> the first line a run that finished printed,
# or nothing: what a binary says it is (`--version`), asked of one this installer
# placed or one it did not. It runs through bounded: on /dev/null (under
# `curl … | bash` the inherited stdin is the rest of this script, which a binary
# that reads it would swallow), in its own process group, and cut off at
# VERIFY_WAIT_SECS. A run cut off answered nothing, whatever it printed first.
first_line_bounded() {
	local out s=0
	out="$(mktemp "${TMPDIR:-/tmp}/aterm-probe.XXXXXX")" || return 0
	bounded "$VERIFY_WAIT_SECS" "$@" >"$out" 2>/dev/null || s=$?
	[[ "$s" -eq 124 ]] || head -n 1 "$out"
	rm -f "$out"
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
	for c in ${DEST:+"$DEST"} ${ATERM_INSTALL_DIR:+"$ATERM_INSTALL_DIR"} /Applications "$HOME/Applications"; do
		m="$c/aterm.app/Contents/MacOS"
		if [[ -x "$m/aterm" ]] && first_line_bounded "$m/aterm" --version | grep -q '^aterm '; then
			echo "$m/aterm"
			return 0
		fi
		ok=1
		for b in aterm-cli aterm-ctl atpkg aterm-fleet aterm-drive; do
			[[ -x "$m/$b" ]] || { ok=0; break; }
		done
		if [[ "$ok" -eq 1 ]] && bounded "$VERIFY_WAIT_SECS" "$m/aterm-cli" --version >/dev/null 2>&1; then
			echo "$m/aterm-cli"
			return 0
		fi
	done
	return 0
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
		bounded "$VERIFY_WAIT_SECS" "$c" pkg help "$verb" >/dev/null 2>&1 || continue
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
# does not install the set anyway (docs/DESIGN-cli-toolchain-seed-2026-08-31.md).
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

# Persist the exclusion into <cfg> per no_toolchain_plan. Writes through a
# symlinked config (a dotfiles repo keeps its link) and keeps the file's mode.
persist_no_toolchain() { # <cfg>
	local cfg="$1" plan tmp
	plan="$(no_toolchain_plan "$cfg")"
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
		tmp="$(mktemp "${TMPDIR:-/tmp}/aterm-config.XXXXXX")" || return 1
		awk '
			{ print }
			!done {
				line = $0; sub(/[ \t]*#.*$/, "", line); gsub(/^[ \t]+|[ \t]+$/, "", line)
				if (line ~ /^\[[ \t]*packages[ \t]*\]$/) { print "auto_install = false"; done = 1 }
			}' "$cfg" >"$tmp" && cat "$tmp" >"$cfg"
		local status=$?
		rm -f "$tmp"
		[[ $status -eq 0 ]] || return 1
		;;
	keep*)
		echo "install.sh: --no-toolchain: $cfg already says \`${plan#keep }\` — left as it is"
		return 0
		;;
	esac
	echo "install.sh: --no-toolchain: wrote \`[packages] auto_install = false\` into $cfg — $NO_TOOLCHAIN_EFFECT"
}

# The test seam: a suite sources this file with ATERM_INSTALL_LIBRARY_ONLY=1 to
# drive the functions above without parsing arguments or touching the host.
if [[ "${ATERM_INSTALL_LIBRARY_ONLY:-0}" == 1 ]]; then
	return 0 2>/dev/null || exit 0
fi

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
		elect_container "$DO_TOOLCHAIN" "$VERSION" \
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

	if [[ "$DO_CLI" -eq 0 ]]; then
		echo "install.sh: cli: excluded (--no-cli)"
	elif [[ -n "$CLI_SKIP" ]]; then
		echo "install.sh: cli: would be SKIPPED: $CLI_SKIP"
	else
		bin_aterm_plan "$BIN_DIR/aterm" "$STORE_DIR"
		# The skip first: every refusal found after a cargo was (no rustup
		# behind it, the pinned toolchain absent, a cargo that ignores the
		# pin, a rustc beside it that is not the proxy, an unwritable store)
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
		plan="$(no_toolchain_plan "$cfg")"
		case "$plan" in
		create) echo "  config:   creates $cfg with \`[packages] auto_install = false\` — the exclusion persists: $NO_TOOLCHAIN_EFFECT" ;;
		append | insert) echo "  config:   adds \`auto_install = false\` to [packages] in $cfg — the exclusion persists: $NO_TOOLCHAIN_EFFECT" ;;
		keep*) echo "  config:   $cfg already says \`${plan#keep }\` — left as it is" ;;
		esac
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
# Everything from here down executes ONLY through the `main "$@"` on this
# file's LITERAL LAST LINE. The documented invocation is `curl … | bash`, and
# bash runs a pipe as it arrives: without this wrapper, a transfer truncated
# after the argument loop still executed a PREFIX of the run — empirically,
# `head -n <pre-dispatch> install.sh | bash` created ~/.local/bin, spent
# anonymous GitHub API budget, and exited 0. (Round 10 refuted a truncation
# hazard on the grounds that every DESTRUCTIVE operation lives in a function
# with the dispatch as the tail — true for the app swap, but the TOP-LEVEL
# pre-flight between the two already carried side effects, so the refutation
# did not cover the file. docs/INSTALL-UPDATE-AUDITS-2026-08.md records the
# original verdict; this guard supersedes it — do not remove it on that
# document's authority.)
# Wrapped, truncation has exactly two outcomes, both inert: a cut inside the
# body is a parse error (bash executes none of an unterminated compound), and
# a cut that keeps the body but loses the last line defines everything and
# runs nothing. No side effect can happen unless the final byte arrived — the
# same tail-call guard rustup and uv ship. Tested at every top-level cut point
# by crates/aterm-update/tests/install_sh.
# The body below keeps its pre-wrap indentation ON PURPOSE: re-indenting
# ~1,600 lines would bury the guard's real diff in mechanical churn and risk
# pushing literal tabs into multi-line strings.
main() {
TAG=""
TAG_INPUT=""
TAG_EXPLICIT=0
DO_APP=1
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
# ATERM_BIN_DIR, the store's `aterm` and the PATH link were one file:
# claim_bin_aterm removed the binary just placed as a "pre-store" leftover and
# linked `aterm` to itself, and the run still said "ONE command on PATH"
# (2026-10-02 audit). A store inside the PATH directory, or around it, mixes
# the store's entries with PATH's the same way. Refused up front, before any
# network or write, whenever a half that may fill the store is on: the cli half
# (its source-build lane) or Linux's app half. The PATH directories are the ones
# the uninstall sweep walks — ATERM_BIN_DIR, and ~/.local/bin, where the app
# plants its own links whatever ATERM_BIN_DIR says — so no install lays down a
# store the uninstall must leave behind as part of a PATH directory.
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
			# So are the verifier's tools.
			if ! command -v openssl >/dev/null 2>&1 || ! command -v timeout >/dev/null 2>&1 ||
				! python3 -I -c 'import tomllib, hashlib, os; assert hasattr(os, "memfd_create"); assert hasattr(hashlib, "file_digest")' >/dev/null 2>&1; then
				APP_SKIP="signed Linux bootstrap needs Python 3.11+ and OpenSSL with Ed25519 support — install them and re-run"
			elif ! ensure_dirs_writable "$STORE_DIR" "$BIN_DIR"; then
				APP_SKIP="cannot create/write $STORE_DIR or $BIN_DIR (set ATERM_STORE_DIR / ATERM_BIN_DIR to writable dirs)"
			elif ! linux_store_preflight "$STORE_DIR"; then
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
				if [[ "$SKIP_STATUS" -eq 0 ]]; then
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
		CLI_SKIP="cannot create/write $BIN_DIR (set ATERM_BIN_DIR to a writable dir)"
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
			CLI_CARGO_SKIP="cannot create/write $STORE_DIR (set ATERM_STORE_DIR to a writable dir)"
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

	TMP="$(mktemp -d "${TMPDIR:-/tmp}/aterm-install.XXXXXX")"
	MNT="$TMP/mnt"
	STAGE=""
	OLD=""
	cleanup() {
		# Best-effort only, and never let a cleanup failure rewrite the exit status.
		set +e
		# Only a DMG is mounted; the lean zip is expanded into this same path, and
		# `rm -rf "$TMP"` below is all it needs.
		if [[ "${CONTAINER_KIND:-dmg}" == dmg && -d "$MNT" ]]; then
			hdiutil detach "$MNT" -quiet >/dev/null 2>&1 ||
				{ sleep 2; hdiutil detach "$MNT" -force -quiet >/dev/null 2>&1; }
		fi
		[[ -n "$STAGE" ]] && rm -rf "$STAGE"
		# If the swap was interrupted after the old bundle was set aside, restore it.
		if [[ -n "$OLD" && -d "$OLD" && ! -e "$DEST/aterm.app" ]]; then
			mv "$OLD" "$DEST/aterm.app"
		fi
		[[ -n "$OLD" ]] && rm -rf "$OLD"
		rm -rf "$TMP"
	}
	trap cleanup EXIT

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
	# named its cause.
	elect_container "$DO_TOOLCHAIN" "$VERSION" \
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
		if [[ "$LEAN_REASON" == default ]]; then
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
	if [[ "$CONTAINER_KIND" == zip ]]; then
		mkdir -p "$MNT"
		ditto -x -k "$TMP/$ASSET_NAME" "$MNT" || {
			echo "install.sh: could not expand $ASSET_NAME" >&2
			exit 1
		}
	else
		hdiutil attach "$TMP/$ASSET_NAME" -nobrowse -readonly -mountpoint "$MNT" -quiet
	fi
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

	# Stage next to the destination, swap by rename, delete the old copy last — either
	# the old or the new bundle exists at $DEST/aterm.app at every instant, and an
	# interrupt mid-swap restores the old one (see cleanup).
	# The staging name must be visible (no leading dot): Spotlight never indexes
	# content under dot-paths, and the final rename does not backfill the index.
	# And unpredictable: /Applications is shared, admin-writable space, so a
	# guessable staging name could be pre-created by another local user to ambush
	# the swap. Same rule for the set-aside path.
	STAGE="$DEST/aterm.app.installing.$(random_suffix)"
	ditto "$MNT/aterm.app" "$STAGE"
	if [[ -e "$DEST/aterm.app" ]]; then
		OLD="$DEST/.aterm.app.old.$(random_suffix)"
		mv "$DEST/aterm.app" "$OLD"
	fi
	mv "$STAGE" "$DEST/aterm.app"
	STAGE=""
	if [[ -n "$OLD" ]]; then
		rm -rf "$OLD" 2>/dev/null ||
			echo "install.sh: NOTE: could not fully remove the previous copy (left at $OLD; remove with: sudo rm -rf '$OLD')" >&2
		OLD=""
	fi

	# Nudge Spotlight (the staged folder was indexed as a plain directory; the
	# rename just made it an .app bundle) and LaunchServices, so the app is
	# findable the moment the installer exits.
	mdimport "$DEST/aterm.app" >/dev/null 2>&1 || true
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
		-f "$DEST/aterm.app" >/dev/null 2>&1 || true

	if pgrep -f '/aterm\.app/Contents/MacOS/aterm' >/dev/null 2>&1; then
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
	echo "  launch:  open '$DEST/aterm.app'"
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
	TMP="$(mktemp -d "${TMPDIR:-/tmp}/aterm-install.XXXXXX")"
	cleanup() {
		set +e
		rm -rf "$TMP"
	}
	trap cleanup EXIT

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
	local target=""
	[[ "$LINUX_APP_INSTALLED" -eq 1 ]] || target="$(find_bundle_cli)"
	if [[ "$LINUX_APP_INSTALLED" -eq 1 ]]; then
		# The app half already landed the released Linux binary in the store and
		# exposed the ONE `aterm` symlink (place_store_binary). Building from
		# source here would silently replace that digest-verified release with
		# an unverified dev build — only the trimmings below are left to do.
		PLACED_ATERM="$STORE_DIR/aterm"
	elif [[ -n "$target" ]]; then
		PLACED_ATERM="$target"
		# ONE name on PATH (the [workspace.metadata.atpkg] expose declaration):
		# `aterm` alone — a symlink at the bundle's one binary (or, against a
		# previous-generation bundle, its aterm-cli front door).
		claim_bin_aterm || return 0
		ln -sfn "$target" "$BIN_DIR/aterm"
		BIN_ATERM_EXPOSED=1
		echo "install.sh: linked $("$BIN_DIR/aterm" --version | head -n 1) -> $BIN_DIR/aterm (follows the app's updates)"
	elif [[ -n "$CLI_CARGO_SKIP" ]]; then
		# Neither source is available here: no installed bundle ships the
		# toolset (older release, or no app), and the build fallback is
		# impossible for the pre-flighted reason.
		if [[ "$(uname -s)" == Darwin ]]; then
			echo "install.sh: SKIPPED the CLI (aterm): no installed aterm.app ships the toolset (newer releases do — update or reinstall the app first), and $CLI_CARGO_SKIP" >&2
		else
			# No bundle exists off macOS: the released binary is the app half's,
			# and this run did not install it (its own line says why).
			echo "install.sh: SKIPPED the CLI (aterm): this run installed no released aterm (see the app line), and $CLI_CARGO_SKIP" >&2
		fi
		return 0
	else
		install_cli_from_source
	fi
	retire_exposed_siblings
	setup_completions_and_man
	# The hand-edit PATH hint is DEFERRED to the end of the run: printed here
	# it told the user to edit the very profile wire_shell_path was about to
	# edit for them (with a block that now carries $BIN_DIR). The final
	# dispatch prints it only when no managed block exists.
	CLI_PATH_HINT_WANTED=1
	INSTALLED_ANY=1
}

install_cli_from_source() {
	echo "install.sh: building aterm from $ROOT at $(git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null || echo '(not a git checkout)')"
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
		# Under `set -e` the failed build used to end the run with cargo's own
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
	echo "install.sh: installed $("$STORE_DIR/aterm" --version | head -n 1) -> $STORE_DIR"
	say_store_command ""
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
	local staged
	staged="$(mktemp "$STORE_DIR/.aterm-install.XXXXXX")" || return 1
	if ! install -m 755 "$1" "$staged" || ! mv -f "$staged" "$STORE_DIR/aterm"; then
		rm -f "$staged"
		return 1
	fi
	expose_store_binary
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
	case ":$PATH:" in
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
		echo "  export PATH=\"$BIN_DIR:\$PATH\"" >&2
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
	aterm_bin="$BIN_DIR/aterm"
	if [[ ! -x "$aterm_bin" ]]; then
		aterm_bin="$(find_bundle_cli)"
	fi
	if [[ -z "$aterm_bin" || ! -x "$aterm_bin" ]]; then
		echo "install.sh: SKIPPED the ALab toolset: no installed \`aterm\` to drive it" >&2
		return 0
	fi

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
# install/update pass appends a block sourcing it to an EXISTING ~/.zshrc,
# ~/.bashrc, ~/.bash_profile or ~/.config/fish/config.fish. The installed aterm's
# `pkg shell-setup --path` reaches what that leaves out (~/.bash_login and
# ~/.profile on a macOS home without ~/.bash_profile, fish under $XDG_CONFIG_HOME,
# a profile that does not exist yet) with ONE marker-fenced block — the hook,
# and $BIN_DIR when PATH lacks it — which `self-uninstall` undoes. PATH_BLOCK_WROTE
# says a block stands; without a runnable aterm nothing is written, and the
# hand-edit hint below says what to add.
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
		# TOOLCHAIN_DEFERRED is set only by a fresh lean-default app install;
		# every repair lane (--no-app, an already-current app, a pinned
		# pre-lean DMG, Linux) still runs install_toolchain here.
		echo "install.sh: toolset: installs on aterm's first launch (from a terminal instead: aterm pkg install --default-set)"
	else
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
# is the file's last line on purpose: nothing runs unless every byte before it
# arrived, so a truncated `curl … | bash` is inert.
main "$@"
