#!/usr/bin/env bash
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# install.sh — the aterm install: the released aterm.app AND the `aterm`
# command (ONE name on PATH; it fronts every verb — aterm help / ctl / pkg /
# fleet / drive), in one command. The DEFAULT is the LEAN container: a ~27 MB
# download, aterm opens immediately, and the ALab toolchain installs itself on
# first launch with live progress. Flags only EXCLUDE — except --token and
# --batteries, the two opt-ins (--batteries selects the batteries-included DMG
# pair: first launch installs the whole toolset with no network, the
# offline / air-gapped lane).
#
# The DEFAULT download source is the PUBLIC release repo (alabsystems/aterm),
# fetched anonymously — no GitHub credential required. An authenticated `gh`
# is preferred when present (it serves any slug), and is REQUIRED for the
# private staging repo: ATERM_REPO_SLUG=alabsystems/aterm, or a run from the
# private checkout, whose Cargo.toml derives that slug. No credential is
# copied anywhere by default — the `token` half below is opt-in, because the
# compiled-in public update channel reads none. The copy matters only for a
# machine later REPOINTED at a private update source ($ATERM_UPDATE_OWNER/
# _REPO), which then keeps updating without `gh` on PATH — which a
# Finder-launched .app does not have.
#
# The three halves, and when each can run:
#   app — the released aterm.app from the GitHub Release (the bundle is macOS-
#         only; linux-x86_64 takes this half as the released ONE binary — see
#         the LINUX paragraph below; anonymous curl against a public repo,
#         authenticated gh otherwise).
#         Verified with a deliberately weaker bootstrap tier than the installed
#         updater (see docs/RELEASING.md):
#           1. paginate the complete release catalog and select the unique
#              numeric maximum of its current-scheme vMAJOR.MINOR.PATCH tags,
#              independent of GitHub REST row order (NOT the "latest" pointer,
#              which non-app releases can hold). Retired two-component tags are
#              archive history: they are skipped, never elected, exactly as the
#              in-app updater skips them. An explicit --version pin bypasses
#              selection and may still name an archived release
#           2. require exactly one manifest and canonical container asset —
#              the lean aterm-<v>-mac.zip by default, the batteries DMG pair
#              on --batteries (elect_container) — carrying each exact API
#              asset ID and byte size into its download
#           3. bind tag == manifest version == container filename, then verify
#              the container's SHA-256 against that manifest
#           4. verify the bundle's code signature; on the official public repo
#              the Developer-ID team is pinned IN THIS SCRIPT (A66A9P66Z7 —
#              see OFFICIAL_TEAM_ID) and a manifest that omits or contradicts
#              it is REFUSED; elsewhere the pin comes from manifest team_id or
#              $ATERM_TEAM_ID. A pinned team requires the full Developer-ID
#              requirement chain for that team + notarization
#           5. swap aterm.app into /Applications — or ~/Applications when
#              /Applications isn't user-writable — replacing any existing copy
#         BOOTSTRAP TRUST BOUNDARY: releases publish Developer-ID-signed,
#         notarized builds plus Ed25519 .sig assets, but this script cannot
#         verify Ed25519 (macOS's stock LibreSSL has no support), so the
#         bootstrap root of trust is the APPLE CODE-SIGNING CHAIN via the
#         pinned Team ID, plus the transport's repo metadata — a
#         gh-authenticated API session, or TLS to api.github.com on the
#         anonymous lane — and the manifest digest. Full Ed25519 verification
#         lives in the installed updater.
#         LINUX (x86_64 only): no bundle exists, so this half instead installs
#         the elected release's aterm-<version>-linux-x86_64.tar.gz — the ONE
#         binary — into the cli store below, exposed as the one `aterm`
#         symlink. Same authoritative-tag selection as macOS; integrity anchor
#         is the companion .sha256 digest asset over the same transport (the
#         signed appcast carries no linux keys yet — planned for the next
#         cut). A release published before the first Linux cut has no such
#         asset: loud skip, and the cli half's source build is the remedy.
#   cli — the `aterm` command (transparent PTY passthrough + the front door
#         for every verb). ONE name lands on PATH — the [workspace.metadata.
#         atpkg] expose declaration; the verb siblings (aterm-ctl, atpkg,
#         aterm-fleet, aterm-drive) ride CO-LOCATED and `aterm ctl/pkg/fleet/
#         drive` resolve them via current_exe, never PATH. Preferred source:
#         the installed app bundle — newer releases ship the whole toolset
#         inside Contents/MacOS (the CLI terminal under the name `aterm-cli`;
#         the bundle's `aterm` is the GUI) — so this half is ONE SYMLINK into
#         the bundle: no cargo, works from a piped script, and the link tracks
#         in-place app updates. Fallback: the toolset is built from THIS
#         checkout's source (run `git pull --ff-only` first for the latest
#         main; needs a checkout + cargo + the pinned CUSTOM `trust` toolchain
#         — rust-toolchain.toml; rustup cannot download it, so its absence is
#         a loud pre-flight skip naming the acquisition path, never a
#         mid-build abort) into a private store
#         (~/.local/lib/aterm/bin, override ATERM_STORE_DIR) with the one
#         `aterm` symlink in ~/.local/bin — non-macOS, older bundles, or no
#         installed app. A PATH hint prints if ~/.local/bin isn't on PATH.
#   token — OPT-IN: a per-machine GitHub token for the IN-APP UPDATER, written
#         to "~/Library/Application Support/aterm/update-token" (0600, in a
#         0700 dir). Sourced from $ATERM_UPDATE_TOKEN if set, else `gh auth
#         token`. NOT needed for the default channel: the compiled-in update
#         source is the PUBLIC repo, which the updater reads anonymously, and
#         for it the token chain consults only an explicit $ATERM_UPDATE_TOKEN
#         — never the keychain and never this file
#         (crates/aterm-update-core/src/token.rs, `needs_ambient_credential`).
#         The file matters ONLY when a machine repoints the updater with
#         $ATERM_UPDATE_OWNER/_REPO — so by DEFAULT this half is SKIPPED: a
#         broad `gh auth token` credential must not land in a plaintext file
#         nothing reads. It runs only when something will read the file or the
#         operator asks: --token, a repointing $ATERM_UPDATE_OWNER/_REPO, or an
#         explicit $ATERM_UPDATE_TOKEN. A one-line intent notice prints before
#         the write; --no-token is a hard off over all of those.
#         On the public channel a token only buys the faster check cadence
#         (~75s vs ~30min), and ONLY via an exported $ATERM_UPDATE_TOKEN — this
#         file cannot supply it there.
#         macOS only (the updater is macOS-only); idempotent; the token is
#         never printed. Re-running refreshes a rotated token, and no failure
#         here is fatal: the app is installed and, on the public channel,
#         updating regardless.
#
# FAILSAFE POLICY: each half is pre-flighted BEFORE any install work; a half
# that is impossible in this environment (piped script with no checkout,
# an OS/arch with no released artifact, a repo needing credentials this run
# lacks, missing cargo or the
# pinned trust toolchain, no release yet, unwritable destination) is
# SKIPPED with a loud reason and the rest still installs. Exit 1 when nothing
# was installed. A real mid-flight failure (download, SHA-256 / signature
# verification, build) always aborts non-zero — those are never skipped.
# --version selects the app release only — the symlinked `aterm` follows
# whatever app is installed, and the cargo fallback always builds the checkout.
#
# Usage:
#   tools/install.sh                                  # the recommended install: ~27 MB
#                                                     # download. aterm opens immediately;
#                                                     # the ALab toolchain installs itself
#                                                     # on first launch with live progress
#   tools/install.sh --batteries                      # the batteries-included DMG pair —
#                                                     # offline / air-gapped: first launch
#                                                     # installs the toolset, no network
#   tools/install.sh --no-cli                         # exclude the `aterm` command
#   tools/install.sh --no-app                         # exclude the app
#   tools/install.sh --token                          # DO provision the update token
#                                                     # (default: skipped — see `token`)
#   tools/install.sh --no-token                       # hard off for the token half
#   tools/install.sh --no-toolchain                   # lean zip, packages disabled — no
#                                                     # toolset half at all (`aterm pkg
#                                                     # install --default-set` later)
#   tools/install.sh --no-path                        # don't touch the shell profile
#   tools/install.sh --token --no-app --no-cli --no-toolchain --no-path
#                                                     # ONLY provision the token — for a
#                                                     # machine that points the updater at
#                                                     # a private repo (see `token` above)
#   tools/install.sh --version 0.5.0                  # pin the app release
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
# app when its bundle ships the tools; against older releases the cli half is
# skipped with a note, since building from source needs a checkout):
#   curl -fsSL https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh | bash
# Operators installing from the PRIVATE staging repo (needs authenticated gh):
#   gh api -H "Accept: application/vnd.github.raw" \
#     repos/alabsystems/aterm/contents/tools/install.sh |
#     ATERM_REPO_SLUG=alabsystems/aterm bash
set -euo pipefail

# Is this file really on disk (vs piped/`bash -s`, where BASH_SOURCE is unusable —
# and on bash >= 5 points at the bash BINARY itself, so -r alone is not enough)?
self_on_disk() {
	[[ -r "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == *install.sh ]]
}

usage() {
	if self_on_disk; then
		# Print the header comment: from line 5 to the first non-comment line, drop it.
		sed -n '5,/^[^#]/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
	else
		echo "usage: install.sh [--batteries] [--no-cli] [--no-app] [--token] [--no-token] [--no-toolchain] [--no-path] [--version X.Y.Z] [--uninstall [--dry-run]]   (env: ATERM_REPO_SLUG, ATERM_INSTALL_DIR, ATERM_BIN_DIR, ATERM_STORE_DIR, ATERM_MAN_DIR, ATERM_TEAM_ID, ATERM_UPDATE_TOKEN, ATERM_NO_TOOLCHAIN, ATERM_NO_PATH)"
	fi
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
#   candidate — the current scheme: canonical vMAJOR.MINOR.PATCH, i.e. the
#               workspace MAJOR.MINOR.DEV version with DEV reset to 0
#               (VERSIONING.md). Only these are ever installed by default.
#   legacy    — a retired pre-cut-over two-component vMAJOR.MINOR. Those
#               releases stay published as archive history and are skipped, not
#               errors, so the archive can coexist with the current channel.
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
	# `read -a` discards a final empty field on Bash 3.2, so validate the whole
	# grammar first; this rejects both v0.55. and v0..55 before component parsing.
	[[ "$body" =~ ^[0-9]+(\.[0-9]+)+$ ]] || return 1
	IFS='.' read -r -a parts <<<"$body"
	for part in "${parts[@]}"; do
		# Reject leading-zero spellings so a tag has exactly ONE canonical form
		# and two distinct tags can never share a numeric order. The u64 bound is
		# checked BEFORE the arity split, exactly as the updater does: an
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

# A canonical numeric dotted tag of either arity. This is the shape gate for
# operator input and for the manifest identity bind — an explicit --version may
# still name a retired two-component archive release — and for the min_os
# advisory, whose operands are macOS product versions, not aterm tags.
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
	local lhs="$1" rhs="$2" body i common l r
	local LC_ALL=C
	local -a left right
	parse_release_tag "$lhs" && parse_release_tag "$rhs" || return 2
	body="${lhs#v}"
	IFS='.' read -r -a left <<<"$body"
	body="${rhs#v}"
	IFS='.' read -r -a right <<<"$body"
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

# Input rows are TAG<TAB>DRAFT<TAB>EXACT_MANIFEST_COUNT, emitted for every page
# by gh's embedded jq. Output is the unique numeric maximum of the CURRENT-scheme
# vMAJOR.MINOR.PATCH candidates; retired two-component releases are skipped even
# when their numbers are larger (v0.61 does not outrank v0.5.0 — it is not in the
# running at all). Return 1 for no app candidate and 2 for malformed/ambiguous/
# noncanonical authority.
#
# Mirrors select_authoritative_release (crates/aterm-update/src/github.rs) on
# BOTH of its order-independence rules, not just tag arbitration:
#   - a duplicate-manifest release is a candidate with POISONED metadata: it
#     still competes under its unambiguous tag, and the poison is fatal only
#     when that release WINS (the winner-only gate after the loop). Erroring
#     before arbitration let a duplicate asset on an old, strictly-lower
#     release wedge the whole install even though it could never be elected —
#     a losing release simply loses, and failing closed over it defends
#     nothing.
#   - a REPEATED candidate tag fails closed wherever it sits in the catalog,
#     the same as the updater's seen_tags set. Detecting ties only against the
#     running maximum made the verdict depend on REST row order: the duplicate
#     pair was invisible whenever a higher tag arrived first.
select_authoritative_tag() {
	local rows="$1" tag draft manifest_count extra selected=""
	local seen_candidates=$'\n' poisoned=$'\n'
	while IFS=$'\t' read -r tag draft manifest_count extra ||
		[[ -n "${tag}${draft}${manifest_count}${extra}" ]]; do
		[[ -n "${tag}${draft}${manifest_count}${extra}" ]] || continue
		if [[ -n "$extra" || ( "$draft" != true && "$draft" != false ) ||
			! "$manifest_count" =~ ^[0-9]+$ ]]; then
			echo "install.sh: malformed release metadata row for ${tag:-<missing-tag>}" >&2
			return 2
		fi
		[[ "$draft" == true ]] && continue
		[[ "$manifest_count" == 0 ]] && continue
		if ! parse_release_tag "$tag"; then
			echo "install.sh: app release tag $tag is not numeric dotted vN.N.N" >&2
			return 2
		fi
		# Retired-scheme releases stay published but are never installed. Skipping
		# (rather than erroring) is what lets the pre-cut-over archive coexist with
		# the current channel — the same `continue` the updater's selector takes,
		# which also discards any duplicate-manifest poison a legacy row carries.
		[[ "$TAG_KIND_RESULT" == candidate ]] || continue
		if [[ "$seen_candidates" == *$'\n'"$tag"$'\n'* ]]; then
			echo "install.sh: published app releases use the numeric order of $tag more than once" >&2
			return 2
		fi
		seen_candidates="$seen_candidates$tag"$'\n'
		[[ "$manifest_count" == 1 ]] || poisoned="$poisoned$tag"$'\n'
		if [[ -z "$selected" ]]; then
			selected="$tag"
		else
			compare_numeric_tags "$tag" "$selected" || return 2
			case "$TAG_COMPARE_RESULT" in
			1) selected="$tag" ;;
			0)
				# Unreachable for two DISTINCT canonical tags (leading-zero
				# rejection gives each numeric order exactly one spelling, and
				# literal repeats fail on the set above) — kept as a fail-closed
				# backstop, never as the primary duplicate detector.
				echo "install.sh: published app releases $selected and $tag have the same numeric order" >&2
				return 2
				;;
			esac
		fi
	done <<<"$rows"
	[[ -n "$selected" ]] || return 1
	# THE WINNER-ONLY GATE (github.rs): a poisoned maximum fails the whole
	# check closed — never elect a runner-up behind a broken winner.
	if [[ "$poisoned" == *$'\n'"$selected"$'\n'* ]]; then
		echo "install.sh: release $selected has duplicate aterm-appcast.toml assets" >&2
		return 2
	fi
	if ! canonical_authority_tag "$selected"; then
		echo "install.sh: authoritative app release $selected is not canonical vMAJOR.MINOR.PATCH" >&2
		return 2
	fi
	printf '%s\n' "$selected"
}

# Require one exact API asset ID. This is separately tested because using an
# order-dependent first match would make duplicate assets an availability bug.
require_unique_asset_id() {
	local ids="$1" label="$2" id count=0 selected=""
	while IFS= read -r id || [[ -n "$id" ]]; do
		[[ -n "$id" ]] || continue
		if [[ ! "$id" =~ ^[1-9][0-9]*$ ]]; then
			echo "install.sh: malformed asset ID for $label" >&2
			return 2
		fi
		count=$((count + 1))
		selected="$id"
	done <<<"$ids"
	if [[ "$count" -ne 1 ]]; then
		echo "install.sh: release has $count assets named $label; expected exactly one" >&2
		return 2
	fi
	printf '%s\n' "$selected"
}

# Bind tag, manifest version, local path shape, and digest before any DMG lookup,
# join, or download. The elected authority is always vMAJOR.MINOR.PATCH; an
# explicit --version may also name a retired two-component archive release, and
# either way the tag, the manifest spelling, and aterm-<version>.dmg must agree
# exactly — the same identity bind the updater enforces.
validate_manifest_identity() {
	local tag="$1" version="$2" dmg_name="$3" sha256="$4"
	if ! canonical_numeric_tag "$tag" || [[ "$tag" != "v$version" ]]; then
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

# --- anonymous-lane extractors: the gh-embedded jq's job, without gh ----------
#
# The anonymous lane reads api.github.com directly, which pretty-prints: in the
# release LIST response, release-object fields sit at exactly 4 spaces and asset
# fields at exactly 8; in the single-release (tags/<tag>) response, asset objects
# open at exactly 4 and their fields sit at exactly 6. JSON strings cannot carry
# a raw newline or raw tab, so exact-indent anchoring cannot be forged from a
# release name or body, and the emitted TSV cannot be split by a value. This is
# NOT a permissive JSON parser: any release/asset whose expected fields arrive
# in an unexpected shape emits a deliberately malformed row, which the row
# validators downstream (select_authoritative_tag / require_unique_asset_record)
# refuse LOUDLY — a parser surprise stops the install, never narrows the set.

# stdin: one page of /repos/<slug>/releases JSON.
# stdout: TAG<TAB>DRAFT<TAB>EXACT_MANIFEST_COUNT per release — the same rows the
# gh lane's embedded jq emits for select_authoritative_tag.
anon_release_rows() {
	awk '
		/^  \{$/ { open = 1; bad = 0; tag = ""; draft = ""; count = 0; next }
		open != 1 { next }
		/^    "tag_name":/ {
			if ($0 ~ /^    "tag_name": "[^"]*",?$/) {
				tag = $0
				sub(/^    "tag_name": "/, "", tag)
				sub(/",?$/, "", tag)
			} else bad = 1
		}
		/^    "draft":/ {
			if ($0 ~ /^    "draft": (true|false),?$/) {
				draft = $0
				sub(/^    "draft": /, "", draft)
				sub(/,$/, "", draft)
			} else bad = 1
		}
		/^        "name": "aterm-appcast\.toml",?$/ { count += 1 }
		/^  \},?$/ {
			if (bad || tag == "" || draft == "") print "MALFORMED\tMALFORMED\tMALFORMED\tMALFORMED"
			else print tag "\t" draft "\t" count
			open = 0
		}
	'
}

# stdin: one /repos/<slug>/releases/tags/<tag> JSON document.
# stdout: ID<TAB>SIZE for every asset whose name is exactly $1 — the same rows
# the gh lane's embedded jq feeds require_unique_asset_record.
anon_asset_records() {
	awk -v wanted="$1" '
		/^    \{$/ { open = 1; bad = 0; n = ""; id = ""; size = ""; next }
		open != 1 { next }
		/^      "name":/ {
			if ($0 ~ /^      "name": "[^"]*",?$/) {
				n = $0
				sub(/^      "name": "/, "", n)
				sub(/",?$/, "", n)
			} else bad = 1
		}
		/^      "id":/ {
			if ($0 ~ /^      "id": [0-9]+,?$/) {
				id = $0
				sub(/^      "id": /, "", id)
				sub(/,$/, "", id)
			} else bad = 1
		}
		/^      "size":/ {
			if ($0 ~ /^      "size": [0-9]+,?$/) {
				size = $0
				sub(/^      "size": /, "", size)
				sub(/,$/, "", size)
			} else bad = 1
		}
		/^    \},?$/ {
			if (n == wanted) {
				if (bad || id == "" || size == "") print "MALFORMED\tMALFORMED\tMALFORMED"
				else print id "\t" size
			}
			open = 0
		}
	'
}

# Return every exact ID/size record for a validated tag/name pair. Callers must
# apply require_unique_asset_record before using the result; keeping immutable
# API identity metadata makes the subsequent octet download order-independent.
# Both lanes carry the same records, so every uniqueness/bounds gate downstream
# is lane-independent. APP_LANE defaults to gh so the library-only test seam
# (which mocks `gh`) keeps its transport semantics.
release_asset_records() {
	local tag="$1" name="$2"
	canonical_numeric_tag "$tag" || {
		echo "install.sh: refusing asset lookup for invalid release tag $tag" >&2
		return 2
	}
	case "$name" in
	aterm-appcast.toml | aterm-appcast.toml.sig) ;;
	*)
		# Three canonical container names, all anchored and version-shaped. The
		# zip is admitted because lean installs come FROM it (Intel on releases
		# without an Intel DMG, and any --no-toolchain install); without that arm
		# the whole lean lane was dead on arrival — every Intel install aborted
		# here with "noncanonical name" before downloading a byte. The
		# `-x86_64.dmg` row is the Intel batteries-included DMG (per-arch pair,
		# 2026-08): releases whose seed covers x86_64-apple-darwin name it in
		# the manifest (`dmg_x86_64`), and an Intel + toolchain install elects
		# it — the same signed universal app with that architecture's seed.
		#
		# Kept as an explicit allowlist rather than a loosened pattern: the point of
		# this gate is that a manifest cannot name an arbitrary asset in the
		# release, and each suffix is exactly as constrained as `.dmg`.
		#
		# The Linux rows are the same shape: the released ONE binary's tarball
		# and its sha256 sidecar — that sidecar is the Linux lane's integrity
		# anchor while the signed appcast carries no linux keys.
		[[ "$name" =~ ^aterm-[0-9]+(\.[0-9]+)+\.dmg$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-x86_64\.dmg$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-mac\.zip$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-linux-x86_64\.tar\.gz(\.sha256)?$ ]] || {
			echo "install.sh: refusing asset lookup for noncanonical name $name" >&2
			return 2
		}
		;;
	esac
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		gh api "repos/$REPO_SLUG/releases/tags/$tag" \
			--jq ".assets[] | select(.name == \"$name\") | [(.id | tostring), (.size | tostring)] | @tsv"
	elif [[ "${RELEASE_DOC_TAG:-}" == "$tag" && -r "${RELEASE_DOC_FILE:-}" ]]; then
		# ONE fetch of the release document serves every lookup for the elected
		# tag: prime_release_document filled this file, so the manifest, .sig,
		# and container lookups no longer spend three of the 60 anonymous
		# requests/hour re-reading the same immutable document.
		anon_asset_records "$name" <"$RELEASE_DOC_FILE"
	else
		curl -fsS --connect-timeout 10 --retry 2 -H "Accept: application/vnd.github+json" \
			"https://api.github.com/repos/$REPO_SLUG/releases/tags/$tag" |
			anon_asset_records "$name"
	fi
}

# Prime the one anonymous fetch of the elected release's document (see the
# cache arm in release_asset_records). Best-effort: on failure the cache stays
# unset and every lookup falls back to its own fetch with its own error path.
RELEASE_DOC_FILE=""
RELEASE_DOC_TAG=""
prime_release_document() { # <tag> <destination-file>
	[[ "${APP_LANE:-gh}" == anon ]] || return 0
	curl -fsS --connect-timeout 10 --retry 2 -H "Accept: application/vnd.github+json" \
		"https://api.github.com/repos/$REPO_SLUG/releases/tags/$1" >"$2" 2>/dev/null || return 0
	RELEASE_DOC_FILE="$2"
	RELEASE_DOC_TAG="$1"
}

# A rate-limited anonymous user deserves the real diagnosis, not a mute
# transport error. /rate_limit is documented as NOT counting against the
# budget, so this costs nothing even when the budget is the problem.
# Best-effort: any parse surprise stays silent and the caller's own error
# message stands.
explain_anon_rate_limit() {
	[[ "${APP_LANE:-gh}" == anon ]] || return 0
	local doc remaining reset when=""
	doc="$(curl -fsS --connect-timeout 10 --retry 2 -H "Accept: application/vnd.github+json" \
		"https://api.github.com/rate_limit" 2>/dev/null)" || return 0
	remaining="$(awk '/"core":/ { f = 1 } f && /"remaining":/ { gsub(/[^0-9]/, ""); print; exit }' <<<"$doc")"
	reset="$(awk '/"core":/ { f = 1 } f && /"reset":/ { gsub(/[^0-9]/, ""); print; exit }' <<<"$doc")"
	[[ "$remaining" == 0 ]] || return 0
	if [[ "$reset" =~ ^[0-9]+$ ]]; then
		# macOS date takes -r <epoch>; GNU date takes -d @<epoch>.
		when="$(date -r "$reset" 2>/dev/null || date -d "@$reset" 2>/dev/null || true)"
	fi
	echo "install.sh: GitHub anonymous rate limit exhausted (60 requests/hour/IP) — retry after ${when:-it resets (within the hour)}, or authenticate: brew install gh && gh auth login" >&2
}

require_unique_asset_record() {
	local records="$1" label="$2" minimum="$3" maximum="$4"
	local id size extra count=0 selected_id="" selected_size=""
	while IFS=$'\t' read -r id size extra || [[ -n "${id}${size}${extra}" ]]; do
		[[ -n "${id}${size}${extra}" ]] || continue
		if [[ -n "$extra" || ! "$id" =~ ^[1-9][0-9]*$ ]] ||
			! decimal_in_closed_range "$size" "$minimum" "$maximum"; then
			echo "install.sh: malformed or out-of-bounds asset metadata for $label" >&2
			return 2
		fi
		count=$((count + 1))
		selected_id="$id"
		selected_size="$size"
	done <<<"$records"
	if [[ "$count" -ne 1 ]]; then
		echo "install.sh: release has $count assets named $label; expected exactly one" >&2
		return 2
	fi
	printf '%s\t%s\n' "$selected_id" "$selected_size"
}

release_unique_asset_record() {
	local tag="$1" name="$2" minimum="$3" maximum="$4" records
	if ! records="$(release_asset_records "$tag" "$name")"; then
		echo "install.sh: could not resolve $name in release $tag" >&2
		return 2
	fi
	require_unique_asset_record "$records" "$name" "$minimum" "$maximum"
}

# The one producer of asset octets, per lane. Both stream the exact immutable
# asset ID (never a name or "latest" pointer), so the identity carried from
# release_asset_records is what gets downloaded on either transport.
fetch_asset_octets() {
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		gh api -H "Accept: application/octet-stream" "repos/$REPO_SLUG/releases/assets/$1"
	else
		# -L: the API answers an octet-stream request with a redirect to the CDN.
		curl -fsSL -H "Accept: application/octet-stream" \
			"https://api.github.com/repos/$REPO_SLUG/releases/assets/$1"
	fi
}

# --- download progress ---------------------------------------------------------
# The DMG went from ~51 MB to ~1.1 GB when the toolchain moved into it, on a
# transport chosen to be silent (`curl -fsS`, `gh api` with no meter). That is
# many wordless minutes in the middle of a `curl … | bash`, which is exactly
# when someone concludes it has hung and ^Cs a half-written install.
#
# The meter polls the DESTINATION FILE rather than the transport, so one
# implementation covers both lanes and neither one's flags have to change — and
# it leans on the only quantity the caller already proved trustworthy, the
# immutable API size that bounds the read below.
#
# Terminal-only. Under `curl … | bash` just stdin is the pipe, so stderr is
# still the tty and the meter shows; CI redirects stderr and gets clean logs
# instead of thousands of carriage returns.
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

download_release_asset_id() {
	local id="$1" expected_size="$2" destination="$3" actual_size rc=0
	if [[ ! "$id" =~ ^[1-9][0-9]*$ || -z "$destination" ]] ||
		! decimal_in_closed_range "$expected_size" 1 2147483648; then
		echo "install.sh: refusing malformed release asset download" >&2
		return 2
	fi
	# Read at most one byte beyond the immutable API size. pipefail makes a
	# producer that overruns the bound fail, while the exact byte-count check
	# below catches both short and one-byte-overlong responses. This caps disk
	# exposure even if transport metadata and body disagree.
	#
	# The meter is started around the transfer and stopped on EVERY path — the
	# `|| rc=$?` keeps `set -e` from leaving a poller orphaned on a failed
	# download, which would otherwise redraw over the error message explaining
	# what went wrong.
	start_download_meter "$destination" "$expected_size"
	fetch_asset_octets "$id" |
		head -c "$((expected_size + 1))" >"$destination" || rc=$?
	stop_download_meter
	if [[ "$rc" -ne 0 ]]; then
		echo "install.sh: exact asset $id download failed" >&2
		# On the anonymous lane the likeliest silent killer is the shared
		# 60/hour budget — one uncounted /rate_limit call says so by name.
		explain_anon_rate_limit
		return 1
	fi
	actual_size="$(wc -c <"$destination" | tr -d '[:space:]')"
	if [[ "$actual_size" != "$expected_size" ]]; then
		echo "install.sh: exact asset $id size mismatch (API $expected_size, downloaded $actual_size)" >&2
		return 2
	fi
}

# Parse one emitted TOML string field. Required fields must occur exactly once;
# optional fields may occur zero or one time. This intentionally accepts only
# the release cutter's simple one-line string form rather than implementing a
# permissive shell TOML parser around security-sensitive identity fields.
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

# --- publisher identity: the official channel's Developer-ID pin ---------------
#
# OFFICIAL_TEAM_ID is the Apple Developer team that signs official
# alabsystems/aterm releases: A66A9P66Z7, "ANDREW DONALD YATES" — the identity
# in the notarized bundle's Developer-ID certificate chain. Pinned HERE, in
# the script, and not only in the manifest, because the manifest rides this
# bootstrap lane UNSIGNED: a manifest free to omit (or swap) team_id would
# downgrade verification to accepting any ad-hoc signature while the script
# still printed "verified".
OFFICIAL_REPO_SLUG="alabsystems/aterm"
OFFICIAL_TEAM_ID="A66A9P66Z7"

# The REQUIRED signing team for one install, or a refusal. stdout: the Team ID
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

# Whether the token half RUNS. Pure, so the deterministic suite pins the
# consent contract: a broad `gh auth token` credential is copied to disk only
# when something will actually read the file (the ambient chain runs only for
# a repointed updater — ATERM_UPDATE_OWNER/_REPO), when the operator supplies
# the value (ATERM_UPDATE_TOKEN), or when asked outright (--token).
# --no-token is a hard off over all of those.
token_provisioning_wanted() { # <do_token> <token_flag> <env_token> <env_owner> <env_repo>
	[[ "$1" -eq 1 ]] || return 1
	[[ "$2" -eq 1 || -n "$3" || -n "$4" || -n "$5" ]]
}

# The idempotence gate for the app half: an UNPINNED run that elected exactly
# the installed version has nothing to download. Pure — the caller reads the
# bundle's Info.plist and passes the fields. An explicit --version pin always
# reinstalls, and a foreign bundle at the path never short-circuits (it gets
# replaced, exactly as before).
app_already_current() { # <elected_tag> <tag_explicit> <bundle_id> <installed_version>
	[[ "$2" -eq 0 ]] || return 1
	[[ "$3" == "com.aterm.aterm" ]] || return 1
	[[ -n "$4" && "v$4" == "$1" ]] || return 1
}

# The gate may only vouch for a bundle the fresh install would have accepted:
# same codesign seal, same Team-ID designated requirement, same Gatekeeper ask
# when a team is pinned. The pin here is the PRE-FLIGHT one (ATERM_TEAM_ID, or
# the compiled-in official team for the official slug) — the manifest is not
# fetched yet, and for the official slug the manifest may not name any other
# team anyway (required_team_for). Matching version strings on an unverifiable
# bundle are not idempotence: a declined gate falls through to a full verified
# reinstall, which is the remedy, so this never errors — it only declines.
installed_bundle_verified() { # <app-path> <team-or-empty>
	local app="$1" team="$2"
	codesign --verify --deep --strict "$app" >/dev/null 2>&1 || return 1
	[[ -n "$team" ]] || return 0
	[[ "$team" =~ ^[A-Za-z0-9]+$ ]] || return 1
	codesign --verify --deep --strict \
		-R "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$team\"" \
		"$app" >/dev/null 2>&1 || return 1
	spctl -a -t exec "$app" >/dev/null 2>&1
}

# Free-space pre-flight: df's answer is advisory (a network mount can lie), so
# an unparseable answer never blocks — only a KNOWN shortfall does, and it
# does so BEFORE the first byte, not a gigabyte in.
require_free_space() { # <dir> <bytes-needed> <what-for>
	local dir="$1" need="$2" what="$3" avail_kb need_kb
	avail_kb="$(df -Pk "$dir" 2>/dev/null | awk 'NR == 2 { print $4 }')"
	[[ "$avail_kb" =~ ^[0-9]+$ ]] || return 0
	need_kb=$((need / 1024))
	if [[ "$avail_kb" -lt "$need_kb" ]]; then
		echo "install.sh: not enough free space on the volume holding $dir for $what — need ~$((need / 1000000)) MB (2.5x the download: the container itself, the staged bundle copy, and expansion slack), have $((avail_kb / 1000)) MB" >&2
		return 1
	fi
}

# The desktop-identity lane's ownership receipt. ~/.local/share/applications
# and the hicolor icon tree are shared, user-owned space, and the NAME `aterm`
# has meant other software for decades (the X11 aterm), so a basename can never
# authorise a removal there. The entry this installer writes carries this exact
# comment line, and the uninstall sweep removes ONLY an entry that still does —
# the same marker-pair discipline as the PATH block. ONE definition, shared by
# the writer and the sweep, so the two can never drift.
ATERM_DESKTOP_MARKER="# Managed by aterm install.sh — its uninstall removes only an entry carrying this line."

# Whether a binary path may ride the desktop entry's Exec= line UNQUOTED. The
# desktop-entry spec's quoting/escaping rules are subtle enough (double-escaped
# backslashes, %-field codes) that carrying an escaper here would be its own
# risk surface — so the lane instead REFUSES any path outside a strict
# character allowlist and writes plain text. Pure, so the deterministic suite
# pins both sides: /-rooted and only [A-Za-z0-9/._+-] — no whitespace, no
# quotes, no %, nothing the Exec parser could interpret.
desktop_exec_path_ok() {
	local p="$1"
	local LC_ALL=C
	[[ "$p" == /* ]] || return 1
	[[ "$p" =~ ^[A-Za-z0-9/._+-]+$ ]]
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
# The DEFAULT is the LEAN container (`aterm-<v>-mac.zip`, ~27 MB) on EVERY CPU:
# aterm opens immediately, and the ALab toolchain installs itself on first
# launch — per program, resumable, with live progress
# (docs/DESIGN-streaming-batteries-2026-08-23.md §7). The batteries-included
# DMG pair stays a first-class objective for offline / air-gapped installs and
# is elected by the explicit --batteries flag: the canonical `aterm-<v>.dmg`
# seals the arm64 toolchain, the additive `aterm-<v>-x86_64.dmg` the Intel one.
# A release that names no zip in its manifest predates the lean container, so
# the default falls back to the DMG election that release was cut for — every
# older --version pin keeps installing exactly as before.
#
# Pure decision logic, extracted from install_app so the whole election matrix
# is pinned by tools/test-install-channel.sh without a network or one Mac of
# each CPU. Inputs are positional; outputs are globals:
#   CONTAINER_KIND      dmg | zip
#   ASSET_NAME/ASSET_SHA  the elected asset and its manifest digest
#   LEAN_REASON         "" | default | no-toolchain (which lean lane, if any)
#   TOOLCHAIN_DEFERRED  1 = the toolset half must NOT run synchronously here:
#                       first launch provisions it, and the GUI's own seed
#                       pass records adoption there — the audited consent
#                       seam (atpkg cmd_seed). This script never writes
#                       consent headlessly.
# Status 2 = the manifest cannot honestly serve the requested lane (malformed
# identity fields, or --batteries on a CPU whose seed this release never
# shipped). Every refusal names its next act; the caller aborts.
#
# Every identity bind below is deliberate: the canonical-name bind stops a
# manifest naming some other asset in the release, and the 64-hex bind stops
# an empty or malformed field from turning the later digest comparison into a
# no-op. A pair travels together or not at all — a name without a digest is a
# malformed release this cutter never produces, and proceeding on half a pair
# would be the one download whose digest check quietly degraded. The binds run
# whenever the fields are PRESENT, not merely when their lane is elected: a
# malformed manifest is a loud abort on every CPU, matching the doctrine that
# only ABSENCE is silent (the fields parse under required=0 with the identity
# fields, so duplicates and malformed values already aborted upstream).
elect_container() { # <batteries01> <toolchain01> <apple_silicon01> <version> <dmg> <dmg_sha> <zip> <zip_sha> <dmg_x86> <dmg_x86_sha>
	local batteries="$1" toolchain="$2" silicon="$3" version="$4"
	local dmg_name="$5" dmg_sha="$6" zip_name="$7" zip_sha="$8"
	local x86_name="$9" x86_sha="${10}"
	CONTAINER_KIND=dmg
	ASSET_NAME="$dmg_name"
	ASSET_SHA="$dmg_sha"
	LEAN_REASON=""
	TOOLCHAIN_DEFERRED=0

	if [[ -n "$x86_name" || -n "$x86_sha" ]]; then
		if [[ -z "$x86_name" || -z "$x86_sha" ]]; then
			echo "install.sh: manifest carries half an Intel DMG pair (dmg_x86_64/dmg_x86_64_sha256) — refusing" >&2
			return 2
		fi
		if [[ "$x86_name" != "aterm-$version-x86_64.dmg" ]]; then
			echo "install.sh: manifest dmg_x86_64 $x86_name is not canonical aterm-$version-x86_64.dmg" >&2
			return 2
		fi
		if [[ ! "$x86_sha" =~ ^[0-9a-fA-F]{64}$ ]]; then
			echo "install.sh: manifest dmg_x86_64_sha256 is not exactly 64 hexadecimal digits" >&2
			return 2
		fi
	fi
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

	if [[ "$batteries" -eq 1 ]]; then
		# The explicit offline / air-gapped ask: the sealed toolchain, or a
		# loud refusal — never a silent downgrade to a container that needs
		# the network this lane exists to avoid. (--batteries + --no-toolchain
		# was refused at the argument gate, so toolchain=1 here.)
		if [[ "$silicon" -eq 1 ]]; then
			return 0 # the canonical DMG, elected above
		fi
		if [[ -z "$x86_name" ]]; then
			echo "install.sh: --batteries on an Intel Mac, but this release names no Intel batteries DMG" >&2
			echo "  (the aterm-<version>-x86_64.dmg pair ships since 2026-08). Next act: rerun without" >&2
			echo "  --batteries — the lean install; the toolchain installs itself on first launch —" >&2
			echo "  or pin a release that ships the pair: --version <X.Y.Z>." >&2
			return 2
		fi
		ASSET_NAME="$x86_name"
		ASSET_SHA="$x86_sha"
		return 0
	fi

	# The DEFAULT lane: the lean zip on EVERY CPU when the release ships one.
	if [[ -n "$zip_name" ]]; then
		CONTAINER_KIND=zip
		ASSET_NAME="$zip_name"
		ASSET_SHA="$zip_sha"
		if [[ "$toolchain" -eq 0 ]]; then
			# --no-toolchain keeps its meaning: lean zip, packages disabled —
			# this run defers nothing, because the user excluded the toolset.
			LEAN_REASON=no-toolchain
		else
			LEAN_REASON=default
			TOOLCHAIN_DEFERRED=1
		fi
		return 0
	fi

	# No zip in the manifest: a pre-lean-container release (reachable via an
	# explicit --version pin). Take the DMG election that release was built
	# around — Intel its own DMG when the pair exists, arm64 the canonical
	# one; the universal app installs either way, exactly as before the flip.
	if [[ "$silicon" -eq 0 && "$toolchain" -eq 1 && -n "$x86_name" ]]; then
		ASSET_NAME="$x86_name"
		ASSET_SHA="$x86_sha"
	fi
	return 0
}

# Internal test seam: source this file to exercise the pure functions without
# parsing CLI arguments or touching the host. Never part of the public surface.
if [[ "${ATERM_INSTALL_LIBRARY_ONLY:-0}" == 1 ]]; then
	return 0 2>/dev/null || exit 0
fi

# --- uninstall: reverse exactly what this installer places ------------------
#
# Removes the six things install.sh creates — app bundle, the ONE `aterm`
# symlink, the source-built store, man pages, shell completions, the Linux
# desktop entry + icons — plus the update token and its keychain item.
# Nothing else.
#
# EVERY removal is OWNERSHIP-CHECKED first. The installer writes into shared,
# user-owned locations (/Applications, ~/.local/bin, the XDG man and completion
# trees) that are full of things it did not put there, so "the path we would
# have written" is NOT sufficient reason to delete. A bundle must identify as
# com.aterm.aterm; the `aterm` on PATH must be a symlink resolving into a bundle
# or our store (a user's own hand-built binary at that path is left alone); a
# man page must be one this repo actually ships. Anything failing its check is
# reported as SKIPPED with the reason, never removed.
#
# --dry-run prints the same decisions and deletes nothing.
uninstall_everything() {
	local removed=0 skipped=0 act="remove"
	local TOOLCHAIN_SWEPT=0
	[[ "$DRY_RUN" -eq 1 ]] && act="would remove"

	_rm() { # _rm <path> <label>
		if [[ "$DRY_RUN" -eq 1 ]]; then
			echo "install.sh: $act $2: $1"
		elif rm -rf "$1" 2>/dev/null; then
			echo "install.sh: removed $2: $1"
		else
			echo "install.sh: SKIPPED $2 (cannot remove, try sudo): $1" >&2
			skipped=$((skipped + 1))
			# Status 0 ON PURPOSE: every call site is errexit-live, so a nonzero
			# return here killed the whole uninstall at the FIRST unremovable
			# item — the later steps never ran and no summary printed, the exact
			# inversion of the skip-and-continue contract above. The aggregate
			# failure still reaches the caller: skipped > 0 returns 1 at the end.
			return 0
		fi
		removed=$((removed + 1))
	}
	_skip() {
		echo "install.sh: SKIPPED $1 — $2" >&2
		skipped=$((skipped + 1))
	}

	# 0. THE TOOLCHAIN STORE, BEFORE THE BUNDLE THAT OWNS IT. `atpkg` exists on
	#    disk in exactly one place — an argv0 symlink inside the app bundle — and
	#    the store it fills is multiple GB under Application Support. Deleting the
	#    bundle first (which this did) removed the only binary that knows how to
	#    remove the store, so the documented `atpkg uninstall --all` remedy became
	#    unrunnable and multiple GB were stranded with no supported way out
	#    (2026-08-20 round-8 audit). Best-effort and never fatal: an uninstall must
	#    still remove the app if the toolchain sweep cannot run.
	local dir app plist id
	local atpkg_bin=""
	# SAME OVERRIDE RULE AS THE BUNDLE SWEEP BELOW. This used to scan
	# /Applications first unconditionally, so an uninstall aimed at a scratch
	# ATERM_INSTALL_DIR reached the REAL /Applications bundle, ran ITS `atpkg
	# uninstall --all`, and deleted the machine's multi-GB toolchain store plus
	# wrote a durable `declined` marker — from a command the operator scoped to
	# a throwaway directory. Step 1 was hardened against exactly this; step 0
	# was not, and it is the more destructive of the two.
	local atpkg_dirs=(/Applications "$HOME/Applications")
	[[ -n "${ATERM_INSTALL_DIR:-}" ]] && atpkg_dirs=("$ATERM_INSTALL_DIR")
	for dir in "${atpkg_dirs[@]}"; do
		[[ -n "$dir" && -x "$dir/aterm.app/Contents/MacOS/atpkg" ]] || continue
		atpkg_bin="$dir/aterm.app/Contents/MacOS/atpkg"
		break
	done
	if [[ -n "$atpkg_bin" ]]; then
		# DRY RUN MEANS DRY. `atpkg uninstall --all` deletes multiple GB and writes a
		# durable "declined" marker; running it here unconditionally made
		# `--uninstall --dry-run` — a command whose entire promise is that it changes
		# nothing — the most destructive path in this script (2026-08-20 round-9 audit).
		if [[ "$DRY_RUN" -eq 1 ]]; then
			# Counted and flagged exactly like the real sweep (the summary
			# prints the would-be variant), so a dry run and its real run show
			# the same decisions AND the same arithmetic.
			echo "install.sh: $act the ALab toolchain store: $atpkg_bin uninstall --all"
			removed=$((removed + 1))
			TOOLCHAIN_SWEPT=1
		elif "$atpkg_bin" uninstall --all >/dev/null 2>&1; then
			echo "install.sh: removed the ALab toolchain store"
			TOOLCHAIN_SWEPT=1
			removed=$((removed + 1))
		else
			_skip "the ALab toolchain store" \
				"\`$atpkg_bin uninstall --all\` failed; run it by hand before deleting the app"
		fi
	fi

	# 1. app bundle — every candidate dir, but only genuine aterm bundles.
	# An explicit ATERM_INSTALL_DIR names THE install location — scan only it.
	# Scanning the defaults as well would let an override aimed at a scratch dir
	# reach out and delete the real /Applications/aterm.app.
	local app_dirs=(/Applications "$HOME/Applications")
	[[ -n "${ATERM_INSTALL_DIR:-}" ]] && app_dirs=("$ATERM_INSTALL_DIR")
	for dir in "${app_dirs[@]}"; do
		app="$dir/aterm.app"
		[[ -d "$app" ]] || continue
		plist="$app/Contents/Info.plist"
		id=""
		[[ -f "$plist" ]] && id="$(defaults read "$plist" CFBundleIdentifier 2>/dev/null || true)"
		if [[ "$id" == "com.aterm.aterm" ]]; then
			_rm "$app" "app bundle"
		else
			_skip "$app" "not an aterm bundle (CFBundleIdentifier=${id:-unreadable})"
		fi
	done

	# 2. the `aterm` symlink — only when it still resolves into a bundle or our
	#    store. A real file there is someone's own build, not ours to delete.
	# The `atpkg` companion link, planted by the app itself on every run from a
	# bundle (crates/atpkg/src/hooks.rs). Nothing else removes it, so leaving it
	# behind guaranteed a dangling command after the app is gone
	# (2026-08-20 round-9 audit).
	# BOTH bin dirs when they differ: ensure_command_links (hooks.rs) hardcodes
	# ~/.local/bin for the links the app plants — it never sees ATERM_BIN_DIR, a
	# shell-only variable — so an override-scoped sweep that walked only
	# $ATERM_BIN_DIR searched a path the app never wrote and left the app's own
	# links dangling. The ownership checks below make the extra dir safe.
	local store="${ATERM_STORE_DIR:-$HOME/.local/lib/aterm/bin}"
	local bin_dirs=("${ATERM_BIN_DIR:-$HOME/.local/bin}")
	[[ "${bin_dirs[0]}" != "$HOME/.local/bin" ]] && bin_dirs+=("$HOME/.local/bin")
	local bd atpkg_link bin target
	for bd in "${bin_dirs[@]}"; do
		atpkg_link="$bd/atpkg"
		if [[ -L "$atpkg_link" ]]; then
			case "$(readlink "$atpkg_link" 2>/dev/null || true)" in
			*/aterm.app/Contents/MacOS/*) _rm "$atpkg_link" "atpkg command" ;;
			*) _skip "$atpkg_link" "symlink points outside an aterm bundle" ;;
			esac
		fi
		bin="$bd/aterm"
		if [[ -L "$bin" ]]; then
			target="$(readlink "$bin" 2>/dev/null || true)"
			case "$target" in
			*/aterm.app/Contents/MacOS/* | "$store"/*) _rm "$bin" "aterm command" ;;
			*) _skip "$bin" "symlink points outside an aterm bundle or store ($target)" ;;
			esac
		elif [[ -e "$bin" ]]; then
			_skip "$bin" "not a symlink — a hand-built binary this installer did not place"
		fi
	done

	# 3. the toolset store — filled by the cargo-fallback lane, or on Linux by
	#    the released-binary lane; either way it is entirely ours to remove.
	[[ -d "$store" ]] && _rm "$store" "source-built toolset"

	# 4. man pages — only names this repo ships, so a foreign aterm*.1 is safe.
	#    A piped run has no checkout to consult, so it falls back to the FIXED
	#    allowlist of every basename install_cli_manpages has ever shipped: the
	#    glob alone is NOT authorisation (aterm was an X terminal for decades;
	#    aterm*-prefixed pages that are not ours exist in the wild, and the
	#    checkout-only guard silently vanished for the `curl | bash` audience).
	local man_root="${ATERM_MAN_DIR:-$HOME/.local/share/man}" page base repo_man
	repo_man=""
	if self_on_disk; then
		repo_man="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd || true)/man"
	fi
	for page in "$man_root"/man[1-9]/aterm*.[1-9]; do
		[[ -e "$page" ]] || continue
		base="${page##*/}"
		# OWNERSHIP CHECK: only a page this repo ships may be removed. With a
		# checkout, its man/ listing is the authority; a piped run has none,
		# so only the exact pages this installer itself writes are swept —
		# anything else is UNKNOWN and skipped, never deleted. (The piped
		# path used to take the OPPOSITE branch and delete any aterm*.N
		# page it found.)
		if [[ -n "$repo_man" && -d "$repo_man" ]]; then
			if [[ ! -e "$repo_man/$base" ]]; then
				_skip "$page" "not a man page this repo ships"
				continue
			fi
		else
			case "$base" in
			aterm.1 | aterm-ctl.1 | aterm-gui.1) ;;
			*)
				_skip "$page" "not a man page this installer ships (run from a checkout to sweep newer pages)"
				continue
				;;
			esac
		fi
		_rm "$page" "man page"
	done

	# 5. shell completions — the exact per-shell paths install_cli_completions
	#    writes: the aterm-ctl verb sibling and the `aterm` front door.
	local xdg_data="${XDG_DATA_HOME:-$HOME/.local/share}"
	local xdg_config="${XDG_CONFIG_HOME:-$HOME/.config}"
	local comp
	for comp in \
		"$xdg_data/bash-completion/completions/aterm" \
		"$xdg_data/zsh/site-functions/_aterm" \
		"$xdg_config/fish/completions/aterm.fish" \
		"$xdg_data/bash-completion/completions/aterm-ctl" \
		"$xdg_data/zsh/site-functions/_aterm-ctl" \
		"$xdg_config/fish/completions/aterm-ctl.fish"; do
		[[ -e "$comp" ]] && _rm "$comp" "shell completion"
	done

	# 6. the Linux desktop identity: the launcher entry + the hicolor icons.
	#    The basename authorises NOTHING here — `aterm` named an unrelated X11
	#    terminal for decades, and both trees are shared user space — so the
	#    only authorisation is the exact ATERM_DESKTOP_MARKER line the writer
	#    put in the entry. The icons were only ever installed WITH that entry
	#    (install_linux_desktop_entry writes both or neither tree), so the
	#    marker-owned entry — found here, or just removed by this sweep — is
	#    what vouches for them too; without it every aterm.png is UNKNOWN and
	#    skipped, never deleted. A no-op on macOS: neither path exists there.
	local desktop_entry="$xdg_data/applications/aterm.desktop" desktop_owned=0 icon
	if [[ -f "$desktop_entry" || -L "$desktop_entry" ]]; then
		if [[ ! -L "$desktop_entry" ]] &&
			grep -qxF "$ATERM_DESKTOP_MARKER" "$desktop_entry" 2>/dev/null; then
			desktop_owned=1
			_rm "$desktop_entry" "desktop entry"
			# Best-effort, real runs only (--dry-run changes nothing): tell the
			# desktop database the entry is gone so launchers drop it now.
			if [[ "$DRY_RUN" -eq 0 ]] && command -v update-desktop-database >/dev/null 2>&1; then
				update-desktop-database "$xdg_data/applications" >/dev/null 2>&1 || true
			fi
		else
			_skip "$desktop_entry" "not a desktop entry this installer wrote (missing its marker line)"
		fi
	fi
	# Only the five sizes THIS installer ships — a foreign aterm.png at any
	# other size (a user's own 48x48, a distro package's) is not ours to sweep,
	# marker-vouched or not.
	for icon in "$xdg_data"/icons/hicolor/{32x32,64x64,128x128,256x256,512x512}/apps/aterm.png; do
		[[ -e "$icon" ]] || continue
		if [[ "$desktop_owned" -eq 1 ]]; then
			_rm "$icon" "app icon"
		else
			_skip "$icon" "no marker-owned aterm.desktop vouches for it"
		fi
	done

	# 7. the update token, its keychain twin, and the support dir when EMPTY.
	#    The support dir also holds settings and staged updates, so it is only
	#    rmdir'd (never rm -rf'd) — a non-empty one is left exactly as it is.
	#    A dry run PROBES both (read-only) and reports the same decisions the
	#    real run takes: '--dry-run prints the same decisions' is the contract
	#    above, and the keychain item and the rmdir were silently exempt from it.
	local support="$HOME/Library/Application Support/aterm" leftover
	[[ -f "$support/update-token" ]] && _rm "$support/update-token" "update token"
	if command -v security >/dev/null 2>&1 &&
		security find-generic-password -s aterm-update-token >/dev/null 2>&1; then
		if [[ "$DRY_RUN" -eq 1 ]]; then
			echo "install.sh: $act keychain item: aterm-update-token"
			removed=$((removed + 1))
		elif security delete-generic-password -s aterm-update-token >/dev/null 2>&1; then
			echo "install.sh: removed keychain item: aterm-update-token"
			removed=$((removed + 1))
		fi
	fi
	if [[ -d "$support" ]]; then
		if [[ "$DRY_RUN" -eq 1 ]]; then
			# Judge emptiness as the real run will see it — after the
			# update-token removal announced above has actually happened.
			leftover="$(ls -A "$support" 2>/dev/null | grep -v '^update-token$' || true)"
			if [[ -z "$leftover" ]]; then
				echo "install.sh: $act empty support dir: $support"
				removed=$((removed + 1))
			fi
		elif rmdir "$support" 2>/dev/null; then
			echo "install.sh: removed empty support dir: $support"
			removed=$((removed + 1))
		fi
	fi

	# 8. the PATH block `wire_shell_path` appended. Same ownership rule as every
	#    removal above: a login file is full of lines this installer did not
	#    write, so the ONLY thing that authorises an edit is our exact marker
	#    pair being present. The rewrite goes through the original inode so the
	#    profile keeps its permissions.
	# The candidate list covers every file either writer has ever targeted:
	# .zshrc; .bashrc plus the login files the Darwin bash arm picks
	# (.bash_profile / .bash_login / .profile); fish's XDG-resolved rc AND the
	# literal ~/.config spelling older versions wrote before honoring
	# XDG_CONFIG_HOME. Visiting a file twice is harmless — the marker gate
	# makes the second pass a no-op.
	local rc_file rc_tmp start_marker end_marker
	start_marker="# >>> aterm ALab toolset (managed by install.sh) >>>"
	end_marker="# <<< aterm ALab toolset <<<"
	for rc_file in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" \
		"$HOME/.bash_login" "$HOME/.profile" "$xdg_config/fish/config.fish" \
		"$HOME/.config/fish/config.fish"; do
		[[ -f "$rc_file" ]] || continue
		grep -qF "$start_marker" "$rc_file" 2>/dev/null || continue
		# The documented authorisation is the exact marker PAIR; a lone start
		# marker means someone edited the block, and the awk below would
		# otherwise drop every line from the marker to end-of-file.
		if ! grep -qF "$end_marker" "$rc_file" 2>/dev/null; then
			_skip "$rc_file" "start marker without its end marker — delete the aterm block by hand"
			continue
		fi
		if [[ "$DRY_RUN" -eq 1 ]]; then
			echo "install.sh: $act PATH block: $rc_file"
			removed=$((removed + 1))
			continue
		fi
		rc_tmp="$rc_file.aterm-uninstall.$$"
		if awk -v s="$start_marker" -v e="$end_marker" '
			$0 == s { drop = 1; next }
			$0 == e { drop = 0; next }
			!drop
			END { if (drop) exit 1 }
		' "$rc_file" >"$rc_tmp" 2>/dev/null && cat "$rc_tmp" >"$rc_file" 2>/dev/null; then
			rm -f "$rc_tmp"
			echo "install.sh: removed PATH block: $rc_file"
			removed=$((removed + 1))
		else
			rm -f "$rc_tmp"
			_skip "$rc_file" "could not rewrite the profile — delete the aterm block by hand"
		fi
	done

	if [[ "$removed" -eq 0 && "$skipped" -eq 0 ]]; then
		echo "install.sh: nothing installed by install.sh was found — nothing to do"
		return 0
	fi
	local verb="uninstall"
	[[ "$DRY_RUN" -eq 1 ]] && verb="dry run"
	local tail=""
	[[ "$skipped" -gt 0 ]] && tail=", $skipped skipped"
	echo "install.sh: $verb complete — $removed item(s)$tail"
	# User data is deliberately NOT touched: settings, themes and Trail Packs
	# outlive an uninstall on purpose. The toolchain store is the exception —
	# step 0 sweeps it when it can reach `atpkg`, so this line reports which of
	# the two actually happened rather than always claiming the toolchain stayed
	# (it says "left in place" only when it really was).
	if [[ "$TOOLCHAIN_SWEPT" -eq 1 ]]; then
		if [[ "$DRY_RUN" -eq 1 ]]; then
			echo "install.sh: left in place: your settings/themes under $support (the ALab toolchain store would be removed)"
		else
			echo "install.sh: left in place: your settings/themes under $support (the ALab toolchain store was removed)"
		fi
	else
		echo "install.sh: left in place: your settings/themes under $support, and any atpkg toolchain"
	fi
	[[ "$skipped" -gt 0 ]] && return 1
	return 0
}

TAG=""
TAG_INPUT=""
TAG_EXPLICIT=0
DO_APP=1
DO_CLI=1
DO_TOKEN=1
TOKEN_EXPLICIT=0
# The toolset is the product, not an add-on (docs/GOLDEN-INSTALL-PATH.md §1.3:
# "Installing aterm installs all those packages"). Until this existed the seed
# fired ONLY from the GUI (crates/aterm-gui/src/lib.rs `spawn_pkg_update_check`),
# so `curl … | bash` on a headless box — or by anyone who then uses `aterm` as a
# CLI and never opens the app — installed the ~1 GB payload and left all ten
# programs unpacked FOREVER, with nothing printed to say so. Default on; the
# opt-out exists for CI, which wants the app without the 4.2 GB expansion.
DO_TOOLCHAIN="${ATERM_NO_TOOLCHAIN:+0}"
DO_TOOLCHAIN="${DO_TOOLCHAIN:-1}"
# The container election (2026-08-23 funnel flip — DESIGN-streaming-batteries
# §7): default = the LEAN zip on every CPU (aterm opens immediately; the
# toolchain installs itself on first launch with live progress). --batteries
# is the second opt-in flag (after --token): the sealed DMG pair, for
# offline / air-gapped installs where first launch must need no network.
DO_BATTERIES=0
# PATH wiring for the user's OWN shell. `shell.d` is generated correctly but is
# auto-sourced only by an aterm session, so every other terminal (iTerm, VS
# Code, ssh) saw none of the toolset. Opt out with ATERM_NO_PATH=1.
DO_PATH="${ATERM_NO_PATH:+0}"
DO_PATH="${DO_PATH:-1}"
DO_UNINSTALL=0
DRY_RUN=0
while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help)
		usage
		exit 0
		;;
	-v | --version)
		# PINS a release; it is NOT a version query. Both failure modes say so:
		# `-v` reads like "print the version" everywhere else, so the error has
		# to teach what the flag actually does, not just demand a value.
		if [[ -z "${2:-}" ]]; then
			echo "install.sh: --version PINS the release to install and needs a value (e.g. --version 0.44.0) — it is not a version query; \`aterm --version\` asks the installed binary" >&2
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
	--token)
		TOKEN_EXPLICIT=1
		shift
		;;
	--no-token)
		DO_TOKEN=0
		shift
		;;
	--no-toolchain)
		DO_TOOLCHAIN=0
		shift
		;;
	--batteries)
		DO_BATTERIES=1
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
if [[ "$DRY_RUN" -eq 1 ]]; then
	echo "install.sh: --dry-run is only meaningful with --uninstall" >&2
	exit 2
fi
# The two flags answer the same question in opposite directions, so honoring
# one would silently discard the other — refuse at the argument gate, before
# any network or host mutation, like every other input contradiction.
if [[ "$DO_BATTERIES" -eq 1 && "$DO_TOOLCHAIN" -eq 0 ]]; then
	echo "install.sh: --batteries downloads the sealed toolchain and --no-toolchain excludes it — pick one" >&2
	exit 2
fi
# The token half's arbitration, decided ONCE and consulted by the excludes
# gate here and the run phase below (see token_provisioning_wanted).
TOKEN_WANTED=0
token_provisioning_wanted "$DO_TOKEN" "$TOKEN_EXPLICIT" "${ATERM_UPDATE_TOKEN:-}" \
	"${ATERM_UPDATE_OWNER:-}" "${ATERM_UPDATE_REPO:-}" && TOKEN_WANTED=1
# Refuse only when every half that COULD run is out: app, cli, the default-on
# toolchain and PATH halves, and a token half nothing opted into (the token
# half is opt-in — --token, or a repointed updater via ATERM_UPDATE_OWNER/
# _REPO or ATERM_UPDATE_TOKEN). Checking fewer halves made the toolchain-only
# repair (--no-app --no-cli against an already-installed app) unreachable.
if [[ "$DO_APP" -eq 0 && "$DO_CLI" -eq 0 && "$TOKEN_WANTED" -eq 0 &&
	"$DO_TOOLCHAIN" -eq 0 && "$DO_PATH" -eq 0 ]]; then
	echo "install.sh: every half is excluded or unwanted (--no-app --no-cli --no-toolchain --no-path, and no --token) — nothing to do" >&2
	exit 2
fi
if [[ "$TAG_EXPLICIT" -eq 1 ]] && ! canonical_numeric_tag "$TAG"; then
	echo "install.sh: --version pins a RELEASE to install, and '$TAG_INPUT' is not a release version — expected X.Y.Z, e.g. --version 0.44.0 (a retired two-component X.Y archive release is also accepted). It is not a version query." >&2
	exit 2
fi

# Repo slug: env override first; else (when run from a checkout) the single source
# of truth, [workspace.package] repository in Cargo.toml — same derivation as the
# binary's compiled-in default, so the private staging checkout targets itself and
# the public export (whose transform rewrites the owner) targets the mirror; else
# the canonical PUBLIC release repo, which is what a piped run installs from.
repo_slug_from_cargo() {
	local root
	# When piped (`… | bash`) there is no script path — skip straight to the default.
	self_on_disk || return 0
	root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || return 0
	[[ -f "$root/Cargo.toml" ]] || return 0
	awk -F'"' '/^\[workspace\.package\]/{f=1} f&&/^repository[[:space:]]*=/{print $2; exit}' "$root/Cargo.toml" |
		sed -E 's#^[a-z]+://github\.com/##; s#^git@github\.com:##; s#\.git$##; s#/$##'
}
REPO_SLUG="${ATERM_REPO_SLUG:-$(repo_slug_from_cargo)}"
: "${REPO_SLUG:=alabsystems/aterm}"

# --- pre-flight: decide what each half CAN do here (fail fast, skip loud) ------
BIN_DIR="${ATERM_BIN_DIR:-$HOME/.local/bin}"
# The private store for toolset binaries — filled by the cli source build OR by
# the Linux release lane: ONE name (`aterm`) is exposed on PATH as a symlink
# into here; the verb siblings ride alongside, resolved via current_exe — the
# same expose/bundle split as [workspace.metadata.atpkg] and the app bundle.
# Defined before the app pre-flight because on Linux the store IS the app
# half's destination.
STORE_DIR="${ATERM_STORE_DIR:-$HOME/.local/lib/aterm/bin}"
APP_SKIP=""
APP_FATAL=""
APP_ALREADY=""
APP_LANE=""
DEST=""
# The app half's Linux shape: there is no bundle, so the released artifact is
# the ONE binary as aterm-<version>-linux-x86_64.tar.gz, landed in the SAME
# store layout the source-build fallback fills. Only x86_64 is published;
# every other OS/arch keeps the loud skip, with the source build as remedy.
LINUX_RELEASE=0
[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] && LINUX_RELEASE=1
# --batteries names a macOS container; no sealed DMG exists off macOS. Loud
# note, not an abort — the failsafe policy: the rest still installs, and the
# toolset half below fetches from the network index exactly as before.
if [[ "$DO_BATTERIES" -eq 1 && "$(uname -s)" != Darwin ]]; then
	echo "install.sh: NOTE: --batteries selects the macOS batteries-included DMG — no sealed container exists for $(uname -s); the ALab toolset installs from the network index instead" >&2
fi
LINUX_TAR=""
LINUX_TAR_RECORDS=""
if [[ "$DO_APP" -eq 1 ]]; then
	if [[ "$(uname -s)" != "Darwin" && "$LINUX_RELEASE" -eq 0 ]]; then
		APP_SKIP="the released aterm.app is macOS-only, and the released Linux binary is x86_64-only — no artifact exists for $(uname -s)/$(uname -m)"
	elif command -v gh >/dev/null 2>&1 && gh auth token >/dev/null 2>&1; then
		# An authenticated gh serves ANY slug, and is the only way into the
		# private staging repo. Decided ONCE here: every release/asset call
		# below rides the same lane, so metadata and octets never split
		# between credentials.
		APP_LANE=gh
	elif ! command -v curl >/dev/null 2>&1; then
		APP_SKIP="needs curl (anonymous public download) or an authenticated gh (brew install gh, then gh auth login)"
	else
		# The default lane: the PUBLIC release repo, fetched anonymously — on a
		# budget of 60 API requests/hour/IP. No separate reachability probe is
		# spent when the catalog walk below runs: the walk doubles as the
		# probe. An explicit --version pin skips the walk, so only that path
		# still probes — reachability decides SKIP vs install (failsafe
		# policy), and without it a private repo would abort mid-install
		# instead of skipping up front.
		APP_LANE=anon
		if [[ -n "$TAG" ]] && ! curl -fsS --connect-timeout 10 --retry 2 -o /dev/null \
			"https://api.github.com/repos/$REPO_SLUG"; then
			APP_LANE=""
			APP_SKIP="cannot reach $REPO_SLUG anonymously — a private repo (the staging tree) needs an authenticated gh (brew install gh, then gh auth login); otherwise the network or the anonymous API rate limit is the problem"
		fi
	fi
	if [[ -n "$APP_LANE" ]]; then
		LIST_ERR=0
		if [[ -z "$TAG" ]]; then
			# The repo also publishes non-app releases (for example the atpkg
			# index), so enumerate every page and arbitrate the complete exact-name
			# appcast catalog. GitHub does not promise REST release row order.
			RELEASE_ROWS=""
			if [[ "$APP_LANE" == gh ]]; then
				if ! RELEASE_ROWS="$(gh api --paginate "repos/$REPO_SLUG/releases?per_page=100" \
					--jq '.[] | [.tag_name, (.draft | tostring), ([((.assets // [])[] | select(.name == "aterm-appcast.toml"))] | length | tostring)] | @tsv' \
					2>/dev/null)"; then
					LIST_ERR=1
				fi
			else
				# Anonymous pagination: same complete-catalog contract as gh's
				# --paginate. A short page ends the walk; the page cap keeps a
				# pathological catalog from spinning against the 60-requests/hour
				# anonymous limit (30 pages ⇒ 3000 releases, not a real state).
				RELEASE_PAGE=1
				while :; do
					if ! RELEASE_PAGE_JSON="$(curl -fsS --connect-timeout 10 --retry 2 -H "Accept: application/vnd.github+json" \
						"https://api.github.com/repos/$REPO_SLUG/releases?per_page=100&page=$RELEASE_PAGE")"; then
						LIST_ERR=1
						break
					fi
					RELEASE_PAGE_ROWS="$(anon_release_rows <<<"$RELEASE_PAGE_JSON")"
					[[ -n "$RELEASE_PAGE_ROWS" ]] || break
					RELEASE_ROWS="${RELEASE_ROWS:+$RELEASE_ROWS$'\n'}$RELEASE_PAGE_ROWS"
					[[ "$(printf '%s\n' "$RELEASE_PAGE_ROWS" | wc -l | tr -d '[:space:]')" -eq 100 ]] || break
					RELEASE_PAGE=$((RELEASE_PAGE + 1))
					[[ "$RELEASE_PAGE" -le 30 ]] || { LIST_ERR=1; break; }
				done
			fi
			if [[ "$LIST_ERR" -eq 0 ]]; then
				SELECT_STATUS=0
				TAG="$(select_authoritative_tag "$RELEASE_ROWS")" || SELECT_STATUS=$?
				if [[ "$SELECT_STATUS" -eq 2 ]]; then
					if [[ "$APP_LANE" == anon ]]; then
						# Two very different causes produce one parse surprise
						# on the anonymous lane — name both, and the way out.
						APP_FATAL="release catalog is malformed or ambiguous; refusing order-dependent fallback. On the anonymous lane a GitHub response-format change and rate limiting both look like this — install/authenticate gh (brew install gh && gh auth login), or retry later"
					else
						APP_FATAL="release catalog is malformed or ambiguous; refusing order-dependent fallback"
					fi
				fi
			fi
		fi
		if [[ -n "$APP_FATAL" ]]; then
			:
		elif [[ -z "$TAG" && "$LIST_ERR" -eq 1 ]]; then
			if [[ "$APP_LANE" == gh ]]; then
				APP_SKIP="could not list releases in $REPO_SLUG (bad/expired token, no repo access, or rate limit — try: gh auth status)"
			else
				APP_SKIP="could not list releases in $REPO_SLUG anonymously — a private repo (the staging tree) needs an authenticated gh (brew install gh, then gh auth login); otherwise the network or the GitHub API rate limit is the problem (retry later, or authenticate)"
			fi
		elif [[ -z "$TAG" ]]; then
			APP_SKIP="no current-scheme app release (a vMAJOR.MINOR.PATCH tag carrying aterm-appcast.toml) found in $REPO_SLUG — retired two-component releases are archive history and are never elected; name one with --version to install it anyway"
		elif [[ "$LINUX_RELEASE" -eq 1 ]]; then
			# Destination on Linux is the cli store itself, and the artifact may
			# simply not exist yet (every release before the first Linux cut is
			# macOS-only). Both are environment facts, so both are pre-flighted
			# HERE — an unwritable store or an artifact-less release skips the
			# half before any download, per the failsafe policy. The resolved
			# records are CARRIED into the install so the probed identity and
			# the downloaded bytes cannot drift between two listings.
			LINUX_TAR="aterm-${TAG#v}-linux-x86_64.tar.gz"
			# FAILSAFE POLICY: the verify/extract tools are environment facts,
			# so probe them HERE — their absence used to surface as a
			# mid-flight abort AFTER the full tarball download (sha256sum at
			# the digest check, tar at extraction), the one failure shape the
			# policy promises never to produce for a predictable impossibility.
			if ! command -v sha256sum >/dev/null 2>&1 ||
				! command -v tar >/dev/null 2>&1 ||
				! command -v gzip >/dev/null 2>&1; then
				APP_SKIP="needs sha256sum, tar, and gzip to verify and extract the released Linux binary — install them (coreutils/tar/gzip, in every distro repo) and re-run"
			elif ! mkdir -p "$STORE_DIR" "$BIN_DIR" 2>/dev/null || [[ ! -w "$STORE_DIR" || ! -w "$BIN_DIR" ]]; then
				APP_SKIP="cannot create/write $STORE_DIR or $BIN_DIR (set ATERM_STORE_DIR / ATERM_BIN_DIR to writable dirs)"
			elif ! LINUX_TAR_RECORDS="$(release_asset_records "$TAG" "$LINUX_TAR")"; then
				APP_SKIP="could not inspect release $TAG for $LINUX_TAR (network, or the GitHub API rate limit)"
			elif [[ -z "$LINUX_TAR_RECORDS" ]]; then
				APP_SKIP="release $TAG publishes no $LINUX_TAR — releases before the first Linux cut ship macOS artifacts only. Remedy: the cli half builds the same toolset from source (it runs next), or pin a release that ships the Linux artifact with --version"
			fi
		else
			# Destination: explicit env wins; else /Applications, else ~/Applications.
			# (The in-app updater never relocates — it defers when its location isn't
			# writable, docs/RELEASING.md — so choosing a user-writable dir here is what
			# keeps self-update working for non-admin users.) Checked HERE so a bad
			# destination skips the half up front, not after the whole DMG download.
			DEST="${ATERM_INSTALL_DIR:-}"
			if [[ -z "$DEST" ]]; then
				if [[ -w /Applications ]]; then DEST=/Applications; else DEST="$HOME/Applications"; fi
			fi
			if ! mkdir -p "$DEST" 2>/dev/null || [[ ! -w "$DEST" ]]; then
				APP_SKIP="cannot create/write $DEST (set ATERM_INSTALL_DIR to a writable dir)"
			elif [[ -f "$DEST/aterm.app/Contents/Info.plist" ]] && app_already_current "$TAG" "$TAG_EXPLICIT" \
				"$(defaults read "$DEST/aterm.app/Contents/Info.plist" CFBundleIdentifier 2>/dev/null || true)" \
				"$(defaults read "$DEST/aterm.app/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || true)"; then
				# IDEMPOTENT RE-RUN: the elected release IS the installed bundle,
				# so the app half has nothing to download — the cli/token/toolset
				# halves still run. An explicit --version pin always reinstalls.
				# Version strings alone vouch for nothing: the bundle must still
				# pass the same signature gate a fresh install enforces, else the
				# re-run a user reaches for AS the remedy would bless a tampered
				# copy. The pre-flight pin: env override, or the official team
				# on the official slug (a fork without a pin verifies the seal only).
				SKIP_TEAM="${ATERM_TEAM_ID:-}"
				if [[ -z "$SKIP_TEAM" && "$REPO_SLUG" == "$OFFICIAL_REPO_SLUG" ]]; then
					SKIP_TEAM="$OFFICIAL_TEAM_ID"
				fi
				if installed_bundle_verified "$DEST/aterm.app" "$SKIP_TEAM"; then
					APP_ALREADY="${TAG#v}"
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
# BIN_DIR and STORE_DIR are defined above the app pre-flight: on Linux the
# store doubles as the app half's destination.
if [[ "$DO_CLI" -eq 1 ]]; then
	# The destination gates BOTH sources (symlink + build) — check it first,
	# BEFORE any install work, so a bad destination skips the half up front.
	if ! mkdir -p "$BIN_DIR" 2>/dev/null || [[ ! -w "$BIN_DIR" ]]; then
		CLI_SKIP="cannot create/write $BIN_DIR (set ATERM_BIN_DIR to a writable dir)"
	else
		# Feasibility of the cargo FALLBACK only. The PREFERRED source — an
		# installed bundle shipping the tools — is probed at install time
		# (install_cli), because the app half hasn't run yet here and may be
		# about to install exactly the bundle the symlinks want.
		if self_on_disk; then
			ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || ROOT=""
		fi
		if [[ -z "$ROOT" || ! -f "$ROOT/Cargo.toml" ]]; then
			CLI_CARGO_SKIP="building from source needs a checkout (a piped script has none) — git clone https://github.com/$REPO_SLUG && cd ${REPO_SLUG##*/} && tools/install.sh"
		elif ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
			CLI_CARGO_SKIP="building from source needs cargo (and rustc) — install via https://rustup.rs (or: brew install rust)"
		elif PINNED_CHANNEL="$(toml_single_str "$ROOT/rust-toolchain.toml" channel 0 2>/dev/null)" &&
			[[ -n "$PINNED_CHANNEL" ]] &&
			! rustup toolchain list 2>/dev/null | awk '{print $1}' | grep -Eq "^${PINNED_CHANNEL}(-|$)"; then
			# The checkout pins a CUSTOM toolchain (rust-toolchain.toml: the
			# linked Trust stage2 — rustup cannot download it) and
			# .cargo/config.toml injects -Ztrust-verify=off, which no stock
			# rustc parses — so a build without the pinned toolchain is doomed
			# on every machine but the operator's. FAILSAFE POLICY: detected
			# HERE as a loud skip naming the acquisition path, never as a
			# mid-flight abort of a build that cannot succeed.
			CLI_CARGO_SKIP="building from source needs the pinned '$PINNED_CHANNEL' rustup toolchain, which rustup cannot download — unpack the rustc/cargo/rust-std dist tarballs from https://github.com/alabsystems/trust/releases into one prefix, then: rustup toolchain link $PINNED_CHANNEL <prefix>"
		elif ! mkdir -p "$STORE_DIR" 2>/dev/null || [[ ! -w "$STORE_DIR" ]]; then
			# Pre-flighted HERE (failsafe policy: an unwritable destination
			# skips the half up front) — never discovered after the
			# multi-minute toolset build. Note `mkdir -p` succeeds on an
			# EXISTING unwritable dir, hence the explicit -w check.
			CLI_CARGO_SKIP="cannot create/write $STORE_DIR (set ATERM_STORE_DIR to a writable dir)"
		fi
	fi
fi

INSTALLED_ANY=0
# Set by install_linux_app: on Linux the app half fills the SAME store the cli
# half's source build would, so the cli half must know it already happened.
LINUX_APP_INSTALLED=0
# Coordination for the PATH story and the repair invocations: install_cli
# REQUESTS the hand-edit hint, wire_shell_path records whether the managed
# block (which carries $BIN_DIR) exists, and the final dispatch prints the
# hint only when no block does — two messages telling the user to edit the
# same file, one of them by hand, is how a fresh install ended with `aterm`
# off PATH. install_toolchain records a completed seed so a toolchain-only
# repair run can exit 0 without touching INSTALLED_ANY's meaning.
CLI_PATH_HINT_WANTED=0
PATH_BLOCK_WROTE=0
TOOLCHAIN_RAN=0

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
		# Only a DMG is MOUNTED. The lean zip is expanded into this same path, and
		# calling hdiutil on a plain directory costs a pointless `sleep 2` plus a
		# second failed force-detach on every lean install — `rm -rf "$TMP"` below
		# is all that one needs.
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

	# ONE anonymous fetch of the release document serves every asset lookup
	# below (manifest, .sig, container) — see release_asset_records.
	prime_release_document "$TAG" "$TMP/release-document.json"

	# Resolve and carry one exact manifest asset identity. Filename-pattern
	# downloads can silently pick an order-dependent duplicate and are forbidden.
	MANIFEST_RECORD="$(release_unique_asset_record "$TAG" 'aterm-appcast.toml' 1 5000000)" ||
		{ explain_anon_rate_limit; exit 1; }
	IFS=$'\t' read -r MANIFEST_ID MANIFEST_SIZE <<<"$MANIFEST_RECORD"
	download_release_asset_id "$MANIFEST_ID" "$MANIFEST_SIZE" "$TMP/aterm-appcast.toml"
	if ! VERSION="$(toml_single_str "$TMP/aterm-appcast.toml" version 1)" ||
		! DMG_NAME="$(toml_single_str "$TMP/aterm-appcast.toml" dmg 1)" ||
		! SHA_WANT="$(toml_single_str "$TMP/aterm-appcast.toml" sha256 1)" ||
		! TEAM_MANIFEST="$(toml_single_str "$TMP/aterm-appcast.toml" team_id 0)" ||
		! MIN_OS="$(toml_single_str "$TMP/aterm-appcast.toml" min_os 0)" ||
		! ZIP_NAME="$(toml_single_str "$TMP/aterm-appcast.toml" zip 0)" ||
		! ZIP_SHA="$(toml_single_str "$TMP/aterm-appcast.toml" zip_sha256 0)" ||
		! DMG_X86_NAME="$(toml_single_str "$TMP/aterm-appcast.toml" dmg_x86_64 0)" ||
		! DMG_X86_SHA="$(toml_single_str "$TMP/aterm-appcast.toml" dmg_x86_64_sha256 0)"; then
		echo "install.sh: release $TAG has a malformed or duplicate manifest identity field" >&2
		exit 1
	fi
	validate_manifest_identity "$TAG" "$VERSION" "$DMG_NAME" "$SHA_WANT" || exit 1
	# Publisher-identity arbitration is fail-closed on the official channel: a
	# manifest that omits (or contradicts) the compiled-in OFFICIAL_TEAM_ID is
	# refused outright — riding UNSIGNED, the manifest must never be able to
	# downgrade verification to ad-hoc. Forks keep manifest/ATERM_TEAM_ID
	# semantics; see required_team_for.
	TEAM_WANT="$(required_team_for "$REPO_SLUG" "${ATERM_TEAM_ID:-}" "$TEAM_MANIFEST")" || exit 1

	# --- pick the container: the LEAN zip by default, DMG pair on --batteries --
	# HARDWARE, not the reporting process. `uname -m` answers for the running
	# process: `#!/usr/bin/env bash` takes whatever bash is first on PATH, so an
	# Intel-Homebrew /usr/local/bin/bash — or `arch -x86_64 bash`, or any
	# Rosetta-translated shell — reports x86_64 on an M-series Mac. Deciding
	# the container from that would hand a --batteries Apple Silicon machine
	# the Intel seed, and the user would never know why. `hw.optional.arm64`
	# answers for the CPU.
	IS_APPLE_SILICON=0
	[[ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" == 1 ]] && IS_APPLE_SILICON=1
	# The decision itself lives in elect_container (with the election-order
	# rationale and every manifest-identity bind), extracted so
	# tools/test-install-channel.sh pins the whole matrix without a network or
	# one Mac of each CPU. It sets CONTAINER_KIND/ASSET_NAME/ASSET_SHA/
	# LEAN_REASON/TOOLCHAIN_DEFERRED; a refusal already named its next act.
	elect_container "$DO_BATTERIES" "$DO_TOOLCHAIN" "$IS_APPLE_SILICON" "$VERSION" \
		"$DMG_NAME" "$SHA_WANT" "$ZIP_NAME" "$ZIP_SHA" "$DMG_X86_NAME" "$DMG_X86_SHA" || exit 1
	# Narrate the election before any bytes move. Each arm says what was
	# decided and why, in the voice of the decision's own cause: the default
	# says what first launch will do, the flags echo the flag, the fallbacks
	# name the release property that forced them.
	case "$CONTAINER_KIND:$LEAN_REASON" in
	zip:default)
		echo "install.sh: using the lean container ($ASSET_NAME) — the recommended install."
		;;
	zip:no-toolchain)
		echo "install.sh: --no-toolchain — using the lean container ($ASSET_NAME)."
		echo "install.sh:   Identical signed app, without the ~1 GB sealed toolchain payload it"
		echo "install.sh:   would only delete unopened. Install the ALab toolset any time later"
		echo "install.sh:   with \`aterm pkg seed\` or \`aterm pkg install --default-set\`."
		;;
	dmg:*)
		if [[ "$DO_BATTERIES" -eq 1 ]]; then
			if [[ "$IS_APPLE_SILICON" == 1 ]]; then
				echo "install.sh: --batteries — using the batteries-included DMG ($ASSET_NAME)."
			else
				echo "install.sh: --batteries — using the Intel batteries-included DMG ($ASSET_NAME)."
				echo "install.sh:   Same signed, notarized universal app; the sealed toolchain carries"
				echo "install.sh:   x86_64 builds of every ALab program."
			fi
			echo "install.sh:   First launch installs the whole ALab toolset with no network —"
			echo "install.sh:   the offline / air-gapped lane."
		else
			echo "install.sh: this release predates the lean container (no zip in its manifest) —"
			echo "install.sh:   using the batteries-included DMG ($ASSET_NAME), exactly as its own"
			echo "install.sh:   installer did."
		fi
		;;
	esac

	# Official releases DO publish an Ed25519 aterm-appcast.toml.sig, but this
	# bootstrap lane cannot verify it (macOS's stock LibreSSL has no Ed25519) —
	# so its presence is inventory-checked only: absence is tolerated (dev/fork
	# builds), and a present signature must still be exactly one 64-byte asset,
	# never a duplicate resolved by order.
	if ! SIGNATURE_RECORDS="$(release_asset_records "$TAG" 'aterm-appcast.toml.sig')"; then
		echo "install.sh: could not inspect manifest signatures for release $TAG" >&2
		explain_anon_rate_limit
		exit 1
	elif [[ -n "$SIGNATURE_RECORDS" ]]; then
		require_unique_asset_record "$SIGNATURE_RECORDS" 'aterm-appcast.toml.sig' 64 64 >/dev/null || exit 1
	fi
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		TRANSPORT_DESC="gh-authenticated repo metadata"
	else
		TRANSPORT_DESC="TLS to api.github.com (anonymous)"
	fi
	if [[ -n "$TEAM_WANT" ]]; then
		echo "install.sh: BOOTSTRAP TRUST BOUNDARY: the release's Ed25519 manifest signature is not" >&2
		echo "  verified here (macOS's stock LibreSSL cannot), so the root of trust is the Apple" >&2
		echo "  code-signing chain via pinned Team ID $TEAM_WANT, plus $TRANSPORT_DESC" >&2
		echo "  and the manifest digest. Full Ed25519 verification lives in the installed updater." >&2
	else
		echo "install.sh: BOOTSTRAP TRUST BOUNDARY: the release's Ed25519 manifest signature is not" >&2
		echo "  verified here (macOS's stock LibreSSL cannot), and no Team ID is pinned for this repo" >&2
		echo "  — this install trusts $TRANSPORT_DESC and the manifest digest ONLY" >&2
		echo "  (Tier REPO). Expect the UNVERIFIED publisher note below." >&2
	fi

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
	# joined to TMP. Resolve exactly one matching API asset and download that ID.
	ASSET_RECORD="$(release_unique_asset_record "$TAG" "$ASSET_NAME" 1 2147483648)" ||
		{ explain_anon_rate_limit; exit 1; }
	IFS=$'\t' read -r ASSET_ID ASSET_SIZE <<<"$ASSET_RECORD"
	# DISK PRE-FLIGHT, before the first byte: ~2.5x the container covers the
	# download itself, the staged bundle copy, and mount/expansion slack —
	# checked on BOTH volumes, since TMPDIR and the destination may not share
	# one. Running out 1.1 GB in would abort mid-swap with nothing to show.
	SPACE_NEED=$((ASSET_SIZE * 5 / 2))
	require_free_space "$TMP" "$SPACE_NEED" "downloading and staging $ASSET_NAME" || exit 1
	require_free_space "$DEST" "$SPACE_NEED" "installing aterm.app" || exit 1
	# SAY WHAT IS ABOUT TO HAPPEN. This download went from ~51 MB to ~650 MB when the
	# toolchain moved into the DMG, and the script's output did not change by one
	# character: between "installing <slug> <tag>" and "sha256 verified" it prints
	# nothing, on a transport that is deliberately quiet (`curl -fsS`, `gh api` with
	# no meter). On a slow line that is many minutes of a `curl | bash` pipeline that
	# looks hung — the classic reason someone ^Cs an install half-written.
	# SAY THE WHOLE PLAN, BEFORE ANY OF IT HAPPENS. Two numbers decide whether
	# someone waits or reaches for ^C: what is about to come down the wire, and
	# what it becomes on disk. Printing the first alone (which is all this did)
	# still ambushes them with a multi-GB expansion afterwards.
	if [[ "$CONTAINER_KIND" == dmg ]]; then
		echo "install.sh: downloading $ASSET_NAME — $((ASSET_SIZE / 1000000)) MB"
		echo "  the ALab toolset rides inside the app, so nothing else is downloaded to install it."
		if [[ "$DO_TOOLCHAIN" -eq 1 ]]; then
			echo "  then: aterm.app -> $DEST, and the toolset unpacks to ~4.2 GB under your home directory."
			echo "  the app reclaims its ~1 GB copy of the payload as soon as the toolset is in place."
		else
			echo "  then: aterm.app -> $DEST. Toolset skipped (--no-toolchain); \`aterm pkg seed\` installs it later."
		fi
	else
		echo "install.sh: downloading $ASSET_NAME — $((ASSET_SIZE / 1000000)) MB"
		if [[ "$LEAN_REASON" == default ]]; then
			# The recommended plan, in the owner's words: the small download is
			# the whole wait — the toolset arrives per program, visibly, AFTER
			# the window is already open. And the road not taken is named, so
			# an air-gapped operator learns about --batteries here, not after
			# a first launch that cannot reach the index.
			echo "  then: aterm.app -> $DEST. aterm opens immediately; the ALab toolchain installs"
			echo "  itself on first launch with live progress — programs download individually,"
			echo "  resumably, and only this machine's builds. Offline / air-gapped install"
			echo "  instead: rerun with --batteries."
		else
			echo "  then: aterm.app -> $DEST. Toolset excluded (--no-toolchain); \`aterm pkg seed\` or \`aterm pkg install --default-set\` installs it later."
		fi
	fi
	download_release_asset_id "$ASSET_ID" "$ASSET_SIZE" "$TMP/$ASSET_NAME"
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
	# is identical for either. A stripped bundle verifies exactly like the fat one
	# (the payload sits in a `.lproj` directory sealed `optional = true`), which is
	# the property the whole lean lane rests on.
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

	codesign --verify --deep --strict "$MNT/aterm.app" || {
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
		codesign --verify --deep --strict -R "$REQ" "$MNT/aterm.app" || {
			echo "install.sh: bundle is not Developer-ID signed by team $TEAM_WANT — refusing to install" >&2
			exit 1
		}
		spctl -a -t exec "$MNT/aterm.app" || {
			echo "install.sh: Gatekeeper assessment (notarization) FAILED — refusing to install" >&2
			exit 1
		}
		echo "install.sh: signature verified (Developer ID, Team ID $TEAM_WANT, notarized)"
	else
		# Never say "verified" here: an ad-hoc signature proves code integrity
		# since signing, not WHO published — with no pinned team, anyone's
		# bundle passes this check.
		echo "install.sh: UNVERIFIED publisher — ad-hoc signature accepted; no pinned Team ID."
		echo "install.sh:   Code integrity was checked, but nothing binds this bundle to a publisher."
	fi

	# Stage next to the destination, swap by rename, delete the old copy last — either
	# the old or the new bundle exists at $DEST/aterm.app at every instant, and an
	# interrupt mid-swap restores the old one (see cleanup).
	# The staging name must be VISIBLE (no leading dot): Spotlight never indexes
	# content under dot-paths, and the final rename does not backfill the index —
	# a dot-staged install lands in /Applications permanently invisible to
	# Spotlight (mdimport after the fact does not recover it).
	# And UNPREDICTABLE: /Applications is shared, admin-writable space, so a
	# fixed (or pid-derived, hence guessable) staging name could be pre-created
	# by another local user to ambush the swap. Same rule for the set-aside path.
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

	# The container and its mount are dead weight the moment the bundle is
	# swapped in: release the ~1 GB temp file and the mounted volume NOW rather
	# than at script EXIT, which the multi-minute toolset seed downstream would
	# otherwise hold open. The EXIT trap keeps the same cleanup as the
	# failure-path net; every step here is idempotent under that second run.
	if [[ "${CONTAINER_KIND:-dmg}" == dmg && -d "$MNT" ]]; then
		hdiutil detach "$MNT" -quiet >/dev/null 2>&1 ||
			{ sleep 2; hdiutil detach "$MNT" -force -quiet >/dev/null 2>&1; } || true
	fi
	rm -rf "$TMP" 2>/dev/null || true

	echo "install.sh: installed aterm $VERSION -> $DEST/aterm.app"
	echo "  launch:  open '$DEST/aterm.app'"
	# ONE line-set for both lanes, because the truth is lane-independent: the
	# compiled-in update channel is the PUBLIC repo and the updater reads it
	# anonymously (token.rs consults only an explicit $ATERM_UPDATE_TOKEN
	# there). The old lane-split text promised updates "once the token half
	# provisions a credential" — false, and contradicted minutes later by the
	# token half's own no-credential message. Round-11 honesty: checks and
	# staging run in the app AND in `aterm` terminal sessions; the APPLY (a
	# re-exec) rides the window entry, so the window is named as the apply path
	# rather than promising a silence that terminal-only machines cannot cash.
	echo "  updates: automatic (silent, verified) — public channel, no credential needed; checks run"
	echo "           in the app and in \`aterm\` sessions, and a staged update applies when the aterm"
	echo "           window opens. Health: aterm update status — opt out with ATERM_NO_AUTO_UPDATE=1"
	INSTALLED_ANY=1
}

# --- the app half on Linux: the released ONE binary, digest-verified -----------
# No bundle, no DMG: the artifact is a tarball carrying the ONE `aterm` binary
# at its root, landed in the SAME store layout the source build fills
# (place_store_binary) — so `aterm` on PATH is identical either way, and
# --uninstall's ownership checks match both producers. The pre-flight already
# proved the tarball exists and carried its exact records in LINUX_TAR_RECORDS.
install_linux_app() {
	echo "install.sh: installing $REPO_SLUG $TAG (linux-x86_64)"

	TMP="$(mktemp -d "${TMPDIR:-/tmp}/aterm-install.XXXXXX")"
	cleanup() {
		# Best-effort only, and never let a cleanup failure rewrite the exit status.
		set +e
		rm -rf "$TMP"
	}
	trap cleanup EXIT

	local tar_name="$LINUX_TAR"
	local sum_name="$tar_name.sha256"
	local tar_record tar_id tar_size sum_record sum_id sum_size

	# Pin the tarball to exactly one immutable API asset, same as the DMG lane —
	# a filename-pattern download could pick an order-dependent duplicate.
	tar_record="$(require_unique_asset_record "$LINUX_TAR_RECORDS" "$tar_name" 1 2147483648)" || exit 1
	IFS=$'\t' read -r tar_id tar_size <<<"$tar_record"

	# INTEGRITY ANCHOR: the companion digest asset. The signed appcast carries
	# no linux keys yet (planned for the next cut), so this lane's anchor is the
	# release's own sha256 sidecar over the same transport. Its absence is a
	# refusal, not a skip: a tarball published without its digest is a malformed
	# release, never something to install unverified.
	sum_record="$(release_unique_asset_record "$TAG" "$sum_name" 64 1024)" ||
		{ explain_anon_rate_limit; exit 1; }
	IFS=$'\t' read -r sum_id sum_size <<<"$sum_record"
	download_release_asset_id "$sum_id" "$sum_size" "$TMP/$sum_name"

	# sha256sum-style sidecar: "<hex>  <filename>", exactly one line, bound to
	# the canonical tarball name — a digest naming any OTHER file must never
	# gate this download. Lowercase-only hex because sha256sum emits exactly
	# that; accepting a second spelling would give one digest two forms.
	local sha_want="" sum_file="" sum_extra=""
	read -r sha_want sum_file sum_extra <"$TMP/$sum_name" || true
	if [[ -n "$sum_extra" || "$sum_file" != "$tar_name" ]] ||
		[[ ! "$sha_want" =~ ^[0-9a-f]{64}$ ]] ||
		[[ "$(wc -l <"$TMP/$sum_name" | tr -d '[:space:]')" -gt 1 ]]; then
		echo "install.sh: $sum_name is not one sha256sum line naming $tar_name — refusing to install" >&2
		exit 1
	fi

	echo "install.sh: BOOTSTRAP TRUST BOUNDARY: the Linux lane's integrity anchor is the release's" >&2
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		echo "  own $sum_name digest asset, over gh-authenticated repo metadata. The signed appcast" >&2
	else
		echo "  own $sum_name digest asset, over TLS to api.github.com (anonymous). The signed appcast" >&2
	fi
	echo "  carries no linux keys yet (planned for the next cut), so no signature binds this artifact." >&2

	echo "install.sh: downloading $tar_name ($((tar_size / 1000000)) MB)"
	download_release_asset_id "$tar_id" "$tar_size" "$TMP/$tar_name"
	SHA_GOT="$(sha256sum "$TMP/$tar_name" | awk '{print $1}')"
	if [[ "$SHA_GOT" != "$sha_want" ]]; then
		echo "install.sh: SHA-256 MISMATCH for $tar_name — refusing to install" >&2
		echo "  digest asset: $sha_want" >&2
		echo "  download:     $SHA_GOT" >&2
		exit 1
	fi
	echo "install.sh: sha256 verified (${SHA_GOT:0:12}…)"

	mkdir -p "$TMP/extract"
	tar -xzf "$TMP/$tar_name" -C "$TMP/extract" || {
		echo "install.sh: could not extract $tar_name" >&2
		exit 1
	}
	# The ONE binary, a regular file at the archive root — the shape the
	# publisher cuts. Anything else fails closed rather than guessing.
	if [[ ! -f "$TMP/extract/aterm" || -L "$TMP/extract/aterm" ]]; then
		echo "install.sh: no aterm binary at the root of $tar_name — refusing to install" >&2
		exit 1
	fi
	# Smoke-run BEFORE placement: a binary this machine cannot execute (newer
	# glibc, foreign userland) must refuse here, while the store still holds
	# whatever last worked. `^aterm ` is the same self-identification
	# find_bundle_cli demands of a bundle's one binary.
	if ! "$TMP/extract/aterm" --version 2>/dev/null | grep -q '^aterm '; then
		echo "install.sh: the extracted binary does not run on this machine (or does not identify as aterm) — refusing to install" >&2
		exit 1
	fi

	# STORE_DIR and BIN_DIR were pre-flighted (created + writability-checked)
	# before any download.
	place_store_binary "$TMP/extract/aterm"
	echo "install.sh: installed the released ONE binary -> $STORE_DIR ($("$BIN_DIR/aterm" --version))"
	echo "  ONE command on PATH: $BIN_DIR/aterm — the terminal, the window (--window), and every verb"
	echo "  updates: re-run this installer — the in-app self-updater is macOS-only today"
	INSTALLED_ANY=1
	LINUX_APP_INSTALLED=1
}

# --- the cli half: the one `aterm` command, preferring the installed bundle ----
# Newer releases ship the CLI terminal inside the bundle as `aterm-cli` (its
# cargo binary is `aterm`; that name belongs to the GUI in Contents/MacOS)
# next to the verb siblings — so the preferred install is ONE symlink into the
# bundle: no cargo, piped-script friendly, and the link tracks in-place app
# updates (the updater swaps aterm.app at the same path).

# The first installed bundle whose toolset can back the `aterm` symlink,
# echoed as the exact TARGET to link. Two generations qualify:
#   one-binary (current): Contents/MacOS/aterm IS everything — it identifies
#     itself as "aterm <version>" (the old GUI-only executable in that slot said
#     "aterm-gui <version>", which must NOT be linked: it had no session mode).
#     The name is the discriminator; the version spelling is not parsed here.
#   toolset (previous):   aterm-cli + the four verb siblings all present.
# Candidates: the app half's fresh install first, then the env override, then
# the standard app dirs. The --version probes filter truncated/foreign copies.
find_bundle_cli() {
	[[ "$(uname -s)" == "Darwin" ]] || return 0
	local c m b ok
	for c in ${DEST:+"$DEST"} ${ATERM_INSTALL_DIR:+"$ATERM_INSTALL_DIR"} /Applications "$HOME/Applications"; do
		m="$c/aterm.app/Contents/MacOS"
		if [[ -x "$m/aterm" ]] && "$m/aterm" --version 2>/dev/null | grep -q '^aterm '; then
			echo "$m/aterm"
			return 0
		fi
		ok=1
		for b in aterm-cli aterm-ctl atpkg aterm-fleet aterm-drive; do
			[[ -x "$m/$b" ]] || { ok=0; break; }
		done
		if [[ "$ok" -eq 1 ]] && "$m/aterm-cli" --version >/dev/null 2>&1; then
			echo "$m/aterm-cli"
			return 0
		fi
	done
	return 0
}

install_cli() {
	local target=""
	[[ "$LINUX_APP_INSTALLED" -eq 1 ]] || target="$(find_bundle_cli)"
	if [[ "$LINUX_APP_INSTALLED" -eq 1 ]]; then
		# The app half already landed the released Linux binary in the store and
		# exposed the ONE `aterm` symlink (place_store_binary). Building from
		# source here would silently replace that digest-verified release with
		# an unverified dev build — only the trimmings below are left to do.
		:
	elif [[ -n "$target" ]]; then
		# ONE name on PATH (the [workspace.metadata.atpkg] expose declaration):
		# `aterm` alone — a symlink at the bundle's one binary (or, against a
		# previous-generation bundle, its aterm-cli front door).
		ln -sfn "$target" "$BIN_DIR/aterm"
		echo "install.sh: linked $("$BIN_DIR/aterm" --version) -> $BIN_DIR/aterm"
		echo "  a symlink into ${target%/Contents/MacOS/*} — it follows the app's silent auto-updates."
		if [[ "$target" == */MacOS/aterm ]]; then
			echo "  ONE command: \`aterm\` is the terminal, the window (--window), and every verb (aterm help / ctl / pkg / fleet / drive)"
		else
			echo "  ONE command: \`aterm\` fronts every verb (aterm help / ctl / pkg / fleet / drive); this release predates --window"
		fi
	elif [[ -n "$CLI_CARGO_SKIP" ]]; then
		# Neither source is available here: no installed bundle ships the
		# toolset (older release, or no app), and the build fallback is
		# impossible for the pre-flighted reason.
		echo "install.sh: SKIPPED the CLI (aterm): no installed aterm.app ships the toolset (newer releases do — update or reinstall the app first), and $CLI_CARGO_SKIP" >&2
		return 0
	else
		install_cli_from_source
	fi
	retire_exposed_siblings
	install_cli_manpages
	install_cli_completions
	install_linux_desktop_entry
	# The hand-edit PATH hint is DEFERRED to the end of the run: printed here
	# it told the user to edit the very profile wire_shell_path was about to
	# edit for them (with a block that now carries $BIN_DIR). The final
	# dispatch prints it only when no managed block exists.
	CLI_PATH_HINT_WANTED=1
	INSTALLED_ANY=1
}

install_cli_from_source() {
	echo "install.sh: building the aterm toolset at $(git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null || echo '(not a git checkout)')" \
		"— run \`git pull --ff-only\` first for the latest main"
	# Pin the target dir (a global CARGO_TARGET_DIR / build.target-dir redirect
	# would strand the fresh binaries elsewhere) but NEVER a --target triple:
	# cargo withholds [target.*] rustflags from HOST units (build scripts, proc
	# macros, and their dependencies) whenever an explicit target is set — the
	# flag, CARGO_BUILD_TARGET, or [build] target — and this repo compiles with
	# Trust, whose one verification opt-out lives in exactly those rustflags
	# (.cargo/config.toml). Under a pinned triple the host units verify
	# strictly and the build fails closed (first casualty: shlex, under cc). A
	# plain host build applies the opt-out to every unit and lands the binary
	# at the ONE deterministic path target/release/. CARGO_BUILD_TARGET is
	# unset for the same reason; a [build] target redirect in user cargo
	# config cannot be neutralized from here, so the freshness proof below
	# turns that into a loud abort instead of a stale (or missing) install.
	local rel="$ROOT/target/release"
	# Existence-after-build IS the freshness proof: cargo always re-links a
	# deleted output, so a redirected build can never leave a previous build's
	# binary here to be silently installed.
	rm -f "$rel/aterm"
	# ONE binary carries everything: the session, the window, and every verb
	# in-process (`-p aterm` pulls the whole library graph).
	(cd "$ROOT" && env -u CARGO_BUILD_TARGET cargo build --release -p aterm --target-dir "$ROOT/target")
	if [[ ! -x "$rel/aterm" ]]; then
		echo "install.sh: the build finished but produced no $rel/aterm — a [build] target in your cargo config redirected it; remove that setting (or install the released app instead: tools/install.sh --no-cli)" >&2
		exit 1
	fi
	# STORE_DIR was pre-flighted (created + writability-checked) before the build.
	place_store_binary "$rel/aterm"
	echo "install.sh: installed the ONE binary -> $STORE_DIR ($("$BIN_DIR/aterm" --version))"
	echo "  ONE command on PATH: $BIN_DIR/aterm — the terminal, the window (--window), and every verb"
}

# Land ONE binary in the private store and expose it: the argv0 verb siblings
# beside it, the single `aterm` symlink on PATH. Shared by BOTH producers — the
# source build and the released Linux binary — so the two lanes cannot drift
# into different layouts, and --uninstall's ownership checks match either.
# Callers pre-flight STORE_DIR and BIN_DIR before any build or download work.
place_store_binary() {
	install -m 755 "$1" "$STORE_DIR/aterm"
	# argv0 compat aliases beside it (matching the bundle's symlinks), so
	# in-session \`aterm-ctl …\` scripts and \$ATERM_CTL keep resolving.
	local alias
	for alias in aterm-cli aterm-ctl atpkg aterm-fleet aterm-drive aterm-gui; do
		ln -sfn aterm "$STORE_DIR/$alias"
	done
	# rm first: a leftover REGULAR FILE from a pre-store install must not
	# survive as a stale copy shadowing the store.
	rm -f "$BIN_DIR/aterm"
	ln -sfn "$STORE_DIR/aterm" "$BIN_DIR/aterm"
}

# Earlier installs (and the pre-one-command layout) exposed `aterm-ctl` on
# PATH next to `aterm`. The front door supersedes it (`aterm ctl …`), so
# retire OUR previous copy — and only ours: a symlink into an aterm.app
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
	elif ! "$p" --version 2>/dev/null | grep -q '^aterm-ctl '; then
		return 0
	fi
	rm -f "$p"
	echo "install.sh: retired $p — \`aterm ctl\` is the front door now (the sibling still ships, co-located)"
}

install_cli_manpages() {
	# Man pages (best-effort, NON-FATAL): copy man/*.N into the XDG man tree so
	# `man aterm-ctl` / `man aterm` / `man aterm-gui` work. Never fails the install —
	# the binaries are already in place; a missing man/ dir or an unwritable dest is
	# silently skipped, as is the checkout-less symlink path (ROOT empty — man
	# pages live in the repo, not the bundle). Section = the trailing digit of
	# each filename (all .1 today).
	MAN_ROOT="${ATERM_MAN_DIR:-$HOME/.local/share/man}"
	if [[ -n "$ROOT" && -d "$ROOT/man" ]]; then
		man_n=0
		for page in "$ROOT"/man/*.[1-9]; do
			[[ -e "$page" ]] || continue # unmatched glob: nothing to install
			dest="$MAN_ROOT/man${page##*.}"
			if mkdir -p "$dest" 2>/dev/null && install -m 644 "$page" "$dest/" 2>/dev/null; then
				man_n=$((man_n + 1))
			fi
		done
		if [[ "$man_n" -gt 0 ]]; then
			echo "install.sh: installed $man_n man page(s) -> $MAN_ROOT (e.g. man aterm-ctl)"
			# ~/.local/share/man is a standard XDG man location most setups already
			# search; only nudge when MANPATH is explicitly set and omits it.
			case ":${MANPATH:-}:" in
			:: | *":$MAN_ROOT:"*) ;;
			*) echo "install.sh: NOTE: $MAN_ROOT is not in \$MANPATH — add it to run \`man aterm-ctl\`" >&2 ;;
			esac
		fi
	fi
}

install_cli_completions() {
	# Shell completions (best-effort, NON-FATAL): each is GENERATED from the
	# just-installed toolset and dropped in the conventional per-user dir. A dir
	# that can't be created/written skips just that shell — the binary is
	# already in place, so this never fails the install.
	#
	# HONESTY RULE: the final message claims ONLY a shell whose completion was
	# BOTH generated AND has a plausible loader for the target dir. A file
	# nowhere loads is not an install, and claiming it teaches people the
	# feature is broken.
	local ac="$BIN_DIR/aterm"
	[[ -x "$ac" ]] || return 0
	local xdg_data="${XDG_DATA_HOME:-$HOME/.local/share}"
	local xdg_config="${XDG_CONFIG_HOME:-$HOME/.config}"
	local zsh_dir="$xdg_data/zsh/site-functions"

	# Loader detection, per shell:
	#   bash — a bash-completion v2 entry point on this machine (that package
	#          is what auto-loads $XDG_DATA_HOME/bash-completion/completions;
	#          stock bash alone loads nothing from there).
	#   zsh  — the target dir referenced from ~/.zshrc ($fpath is invisible
	#          from here, so a mention — expanded or ~-spelled — is the
	#          plausible signal; the hint below prints the exact addition
	#          otherwise).
	#   fish — fish itself installed ($XDG_CONFIG_HOME/fish/completions is one
	#          of its built-in load paths).
	local bash_loader=0 zsh_loader=0 fish_loader=0 f
	for f in /opt/homebrew/etc/profile.d/bash_completion.sh \
		/usr/local/etc/profile.d/bash_completion.sh \
		/etc/profile.d/bash_completion.sh \
		/usr/share/bash-completion/bash_completion; do
		[[ -r "$f" ]] && { bash_loader=1; break; }
	done
	if [[ -f "$HOME/.zshrc" ]] &&
		{ grep -qF "$zsh_dir" "$HOME/.zshrc" ||
			grep -qF "${zsh_dir/#$HOME/~}" "$HOME/.zshrc"; } 2>/dev/null; then
		zsh_loader=1
	fi
	command -v fish >/dev/null 2>&1 && fish_loader=1

	# CONTRACT with the bundle CLI front door: `aterm --completions
	# <bash|zsh|fish>` prints a completion script for `aterm` itself, and the
	# zsh output's FIRST line carries '#compdef aterm'. Probed, never assumed —
	# older bundles lack the flag, and then only the aterm-ctl sibling's
	# completions below are written. stdin from /dev/null so an older binary
	# that misreads the flag can never sit waiting on a pipe.
	local front_door=0 probe first_line
	probe="$("$ac" --completions zsh </dev/null 2>/dev/null || true)"
	first_line="$(printf '%s\n' "$probe" | sed -n 1p)"
	case "$first_line" in
	*"#compdef aterm"*) front_door=1 ;;
	esac

	# shell:kind:destination triples — the aterm-ctl verb sibling (always), and
	# the `aterm` front door itself (only when the bundle can generate it).
	local entry shell rest kind dest dir
	local wrote_bash=0 wrote_zsh=0 wrote_fish=0
	for entry in \
		"bash:ctl:$xdg_data/bash-completion/completions/aterm-ctl" \
		"zsh:ctl:$zsh_dir/_aterm-ctl" \
		"fish:ctl:$xdg_config/fish/completions/aterm-ctl.fish" \
		"bash:front:$xdg_data/bash-completion/completions/aterm" \
		"zsh:front:$zsh_dir/_aterm" \
		"fish:front:$xdg_config/fish/completions/aterm.fish"; do
		shell="${entry%%:*}"
		rest="${entry#*:}"
		kind="${rest%%:*}"
		dest="${rest#*:}"
		[[ "$kind" == front && "$front_door" -eq 0 ]] && continue
		dir="$(dirname "$dest")"
		# mkdir -p succeeds on an existing unwritable dir, hence the explicit -w.
		mkdir -p "$dir" 2>/dev/null || continue
		[[ -w "$dir" ]] || continue
		# Write to a temp then rename, so a mid-write failure never leaves a
		# half-written completion file behind.
		if [[ "$kind" == front ]]; then
			"$ac" --completions "$shell" </dev/null >"$dest.tmp.$$" 2>/dev/null || true
		else
			"$ac" ctl --completions "$shell" </dev/null >"$dest.tmp.$$" 2>/dev/null || true
		fi
		if [[ -s "$dest.tmp.$$" ]] && mv "$dest.tmp.$$" "$dest" 2>/dev/null; then
			case "$shell" in
			bash) wrote_bash=1 ;;
			zsh) wrote_zsh=1 ;;
			fish) wrote_fish=1 ;;
			esac
		else
			rm -f "$dest.tmp.$$" 2>/dev/null
		fi
	done

	local claimed=""
	[[ "$wrote_bash" -eq 1 && "$bash_loader" -eq 1 ]] && claimed="bash"
	[[ "$wrote_zsh" -eq 1 && "$zsh_loader" -eq 1 ]] && claimed="${claimed:+$claimed, }zsh"
	[[ "$wrote_fish" -eq 1 && "$fish_loader" -eq 1 ]] && claimed="${claimed:+$claimed, }fish"
	[[ -n "$claimed" ]] &&
		echo "install.sh: installed shell completions ($claimed) — restart your shell to load them"
	if [[ "$wrote_zsh" -eq 1 && "$zsh_loader" -eq 0 ]]; then
		# Same shape as cli_path_hint: name what landed where, then print the
		# exact addition that makes it load.
		echo "install.sh: NOTE: zsh completions landed in $zsh_dir, which ~/.zshrc does not put on \$fpath — add:" >&2
		echo "  fpath=(\"$zsh_dir\" \$fpath)" >&2
		echo "  autoload -Uz compinit && compinit" >&2
	fi
	return 0
}

install_linux_desktop_entry() {
	# Desktop identity (best-effort, NON-FATAL, Linux only): GNOME/KDE identify
	# an aterm window by the Wayland app_id / X11 WM_CLASS the GUI sets
	# ("aterm", crates/aterm-gui/src/app_window.rs) and resolve its icon,
	# launcher entry, and dock pinning through a desktop file of the SAME
	# basename — without aterm.desktop the compositor shows a generic gear and
	# no launcher entry exists. So: aterm.desktop into the XDG applications
	# dir, and the repo's shipped hicolor PNGs (assets/linux/icons/hicolor,
	# cut from the same brand art as the mac/windows icons) beside it. Same
	# contract as man pages/completions: the binaries are already in place, so
	# an unwritable tree or a missing source skips just this trimming, loudly,
	# never the install. Every skip names its remedy.
	[[ "$(uname -s)" == Linux ]] || return 0
	local aterm_bin="$BIN_DIR/aterm"
	# The entry launches the INSTALLED command (the store-backed symlink), so a
	# run that never landed one (pure trimmings repair) writes nothing.
	[[ -x "$aterm_bin" ]] || return 0
	if ! desktop_exec_path_ok "$aterm_bin"; then
		# Exec= is written unquoted by design (desktop_exec_path_ok), so a path
		# outside the allowlist is refused outright rather than escaped.
		echo "install.sh: SKIPPED the desktop entry: $aterm_bin contains characters an unquoted desktop Exec= line cannot carry safely (set ATERM_BIN_DIR to a plain path)" >&2
		return 0
	fi
	local xdg_data="${XDG_DATA_HOME:-$HOME/.local/share}"
	local app_dir="$xdg_data/applications"
	# mkdir -p succeeds on an existing unwritable dir, hence the explicit -w.
	if ! mkdir -p "$app_dir" 2>/dev/null || [[ ! -w "$app_dir" ]]; then
		echo "install.sh: SKIPPED the desktop entry: cannot create/write $app_dir" >&2
		return 0
	fi
	local entry="$app_dir/aterm.desktop"
	# A pre-existing entry WITHOUT our marker is someone else's file. Replacing
	# it is what an installer does, but silently is not — say so, because a
	# later uninstall removes OUR replacement and their original stays gone.
	if [[ -f "$entry" ]] && ! grep -qF "$ATERM_DESKTOP_MARKER" "$entry" 2>/dev/null; then
		echo "install.sh: NOTE: replacing a pre-existing $entry this installer did not write (no backup is kept)"
	fi
	# Write to a temp then rename, so a mid-write failure never leaves a
	# half-written entry behind. The marker line is the uninstall sweep's
	# ownership receipt (ATERM_DESKTOP_MARKER) — keep it first.
	if ! printf '%s\n' \
		"$ATERM_DESKTOP_MARKER" \
		"[Desktop Entry]" \
		"Type=Application" \
		"Name=aterm" \
		"GenericName=Terminal" \
		"Comment=The batteries-included terminal for AI" \
		"TryExec=$aterm_bin" \
		"Exec=$aterm_bin --window" \
		"Icon=aterm" \
		"Terminal=false" \
		"Categories=System;TerminalEmulator;" \
		"Keywords=terminal;shell;console;command line;" \
		"StartupNotify=true" \
		"StartupWMClass=aterm" >"$entry.tmp.$$" 2>/dev/null ||
		! mv "$entry.tmp.$$" "$entry" 2>/dev/null; then
		rm -f "$entry.tmp.$$" 2>/dev/null
		echo "install.sh: SKIPPED the desktop entry: could not write $entry" >&2
		return 0
	fi

	# The icon, at every size the repo ships. Source is the CHECKOUT (icons
	# live in the repo, not the released tarball — the tarball follow-up is
	# tracked); a piped run has none, and the entry above still buys window
	# grouping + the launcher row, so that is a NOTE, not a rollback.
	local icon_n=0 src size dest
	if [[ -n "$ROOT" && -d "$ROOT/assets/linux/icons/hicolor" ]]; then
		for src in "$ROOT"/assets/linux/icons/hicolor/*/apps/aterm.png; do
			[[ -e "$src" ]] || continue
			size="${src#"$ROOT/assets/linux/icons/hicolor/"}"
			size="${size%%/*}"
			dest="$xdg_data/icons/hicolor/$size/apps"
			if mkdir -p "$dest" 2>/dev/null && install -m 644 "$src" "$dest/aterm.png" 2>/dev/null; then
				icon_n=$((icon_n + 1))
			fi
		done
	fi

	# Best-effort: tell launchers the entry exists NOW rather than at their next
	# rescan. Its absence (or failure) is fine — desktops rescan on their own.
	if command -v update-desktop-database >/dev/null 2>&1; then
		update-desktop-database "$app_dir" >/dev/null 2>&1 || true
	fi

	if [[ "$icon_n" -gt 0 ]]; then
		echo "install.sh: installed the desktop entry -> $entry (+ the aterm icon at $icon_n size(s) under $xdg_data/icons/hicolor)"
	else
		echo "install.sh: installed the desktop entry -> $entry"
		echo "install.sh: NOTE: no checkout to source the aterm icon from — the launcher entry works, with a generic icon; run tools/install.sh from a clone to add it" >&2
	fi
	return 0
}

# --- update token: the credential for a REPOINTED update source ---------------
#
# History: the update channel used to be a private repo, so the updater needed a
# token to read it at all, and an install on a `gh`-authenticated developer Mac
# "just worked" while every other machine silently never updated. That is no
# longer the shape. The compiled-in channel is the PUBLIC repo and the updater
# reads it with no credential; a token only raises the anonymous API cadence.
#
# So this half is NOT what decides whether a Mac updates. Per
# crates/aterm-update-core/src/token.rs (`needs_ambient_credential` + `walk`):
# for the compiled-in channel the chain consults ONLY an explicit
# $ATERM_UPDATE_TOKEN and never touches the keychain or the file written below.
# The ambient chain — keychain, this 0600 file, $GITHUB_TOKEN, $GH_TOKEN, `gh
# auth token` — runs only when $ATERM_UPDATE_OWNER/_REPO repoint the source,
# which is the one way to reach a repo that can require authentication.
#
# That is why this half is OPT-IN (token_provisioning_wanted): copying a broad
# `gh auth token` credential into a plaintext file NOTHING reads is pure
# exposure, so by default nothing is copied and the run says so. It runs only
# for --token, a repointing ATERM_UPDATE_OWNER/_REPO, or an explicit
# ATERM_UPDATE_TOKEN — the cases where the file (or value) is actually read —
# and it keeps the repointed case working without `gh` on PATH (a
# Finder-launched .app has a minimal one).
#
# It is idempotent (a matching token is left alone), it NEVER prints the token,
# and --no-token is a hard off. No failure here is fatal.
UPDATE_TOKEN_DIR="$HOME/Library/Application Support/aterm"
UPDATE_TOKEN_FILE="$UPDATE_TOKEN_DIR/update-token"

# Whether $1 is a well-formed token by the SAME rule the app enforces
# (`valid_token` in token.rs: [A-Za-z0-9_-], 1..=512). Keeping the two in step
# matters — provisioning a value the app will refuse is worse than not
# provisioning at all, because it looks done.
well_formed_token() {
	local t="$1"
	[[ -n "$t" && ${#t} -le 512 && "$t" =~ ^[A-Za-z0-9_-]+$ ]]
}

provision_update_token() {
	# macOS-only: the updater itself is macOS-only (aterm_update::enabled), so a
	# token elsewhere would be a file nothing reads.
	[[ "$(uname -s)" == "Darwin" ]] || return 0

	local tok="" tok_source=""
	if [[ -n "${ATERM_UPDATE_TOKEN:-}" ]]; then
		tok="$ATERM_UPDATE_TOKEN"
		tok_source="\$ATERM_UPDATE_TOKEN"
	elif command -v gh >/dev/null 2>&1; then
		tok="$(gh auth token 2>/dev/null || true)"
		tok_source="gh auth token"
	fi
	# Trim only LEADING/TRAILING whitespace — exactly what the app's validation
	# chokepoint does. Deleting whitespace everywhere would be worse than useless:
	# it would turn a garbage value with an interior space into something that
	# passes `well_formed_token` and gets provisioned as a real credential.
	tok="${tok#"${tok%%[![:space:]]*}"}"
	tok="${tok%"${tok##*[![:space:]]}"}"

	if [[ -z "$tok" ]]; then
		# Only shout if there is genuinely no other way in. A machine with the
		# keychain item or an existing 0600 file is already provisioned and does
		# not need a lecture.
		if [[ -r "$UPDATE_TOKEN_FILE" ]] ||
			security find-generic-password -s aterm-update-token -w >/dev/null 2>&1; then
			INSTALLED_ANY=1
			echo "install.sh: update token already provisioned (existing file or keychain item) — left alone"
			return 0
		fi
		# NOT a warning: the compiled-in channel is public and updates
		# anonymously. Only a machine that repoints the updater at a private
		# repo needs this file, and it can add it later.
		# NOT a warning: the compiled-in channel is public, so this Mac updates
		# without any credential — just on the slower anonymous interval.
		echo "install.sh: no GitHub token available — the update channel is public, so this Mac"
		echo "install.sh:   still auto-updates. Unauthenticated checks share ~60 GitHub requests per"
		echo "install.sh:   hour per IP, so it checks about every 30 minutes instead of every 75s."
		echo "install.sh:   To get the faster cadence, export ATERM_UPDATE_TOKEN for the app — on the"
		echo "install.sh:   public channel that is the ONLY token source the updater consults."
		# NOT "$0": piped as `… | bash` that is literally "bash". Name the
		# invocation for how this run ACTUALLY happened: tools/install.sh
		# exists only for a checkout, and the audience that reaches this
		# message is dominated by piped anon-lane users (no gh means no gh
		# lane) — they need the one-liner, not a path they do not have.
		echo "install.sh:   The file this step writes is read only when ATERM_UPDATE_OWNER/_REPO point"
		echo "install.sh:   the updater at another repo; add it later with:"
		if self_on_disk; then
			echo "install.sh:     gh auth login && tools/install.sh --token --no-app --no-cli --no-toolchain --no-path"
		else
			echo "install.sh:     gh auth login && curl -fsSL https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh |"
			echo "install.sh:       bash -s -- --token --no-app --no-cli --no-toolchain --no-path"
		fi
		echo "install.sh:   Check update health any time with:  aterm update status"
		return 0
	fi

	if ! well_formed_token "$tok"; then
		echo "install.sh: WARNING: the available GitHub token is malformed, so it was NOT provisioned" >&2
		echo "install.sh:   (expected [A-Za-z0-9_-]; got ${#tok} characters). Updates from the public" >&2
		echo "install.sh:   channel are unaffected; re-authenticate with \`gh auth login\` if you need a" >&2
		echo "install.sh:   token for a repointed, private update source." >&2
		return 0
	fi

	# Already provisioned with this exact token → nothing to change. Still counts
	# as a satisfied half, so `--no-app --no-cli` (the repair invocation) reports
	# success rather than "nothing was installed".
	if [[ -r "$UPDATE_TOKEN_FILE" ]] && [[ "$(cat "$UPDATE_TOKEN_FILE" 2>/dev/null)" == "$tok" ]]; then
		INSTALLED_ANY=1
		echo "install.sh: update token already provisioned -> $UPDATE_TOKEN_FILE (unchanged)"
		return 0
	fi

	# CONSENT BEFORE THE COPY: a broad GitHub credential is about to land in a
	# plaintext (0600) file, so intent prints BEFORE the write — what, where,
	# why, and the off switch. (The half only runs at all when something will
	# read the file or the operator asked; see token_provisioning_wanted.)
	echo "install.sh: provisioning: copying the GitHub credential from $tok_source to $UPDATE_TOKEN_FILE (0600) so a repointed updater (ATERM_UPDATE_OWNER/_REPO) can authenticate without gh on PATH — --no-token skips this"

	if ! mkdir -p "$UPDATE_TOKEN_DIR" 2>/dev/null; then
		echo "install.sh: WARNING: could not create $UPDATE_TOKEN_DIR — the update token was NOT provisioned (public-channel updates are unaffected)" >&2
		return 0
	fi
	chmod 700 "$UPDATE_TOKEN_DIR" 2>/dev/null || true
	# Write to a per-pid temp created under umask 077 (so the bytes are never on
	# disk world-readable, not even briefly), then rename over the destination:
	# the live file is atomically the old token or the new one, never a partial.
	local tmp="$UPDATE_TOKEN_FILE.$$.tmp"
	if ! (
		umask 077
		printf '%s' "$tok" >"$tmp"
	) 2>/dev/null; then
		rm -f "$tmp" 2>/dev/null
		echo "install.sh: WARNING: could not write $UPDATE_TOKEN_FILE — the update token was NOT provisioned (public-channel updates are unaffected)" >&2
		return 0
	fi
	chmod 600 "$tmp" 2>/dev/null || true
	if ! mv -f "$tmp" "$UPDATE_TOKEN_FILE" 2>/dev/null; then
		rm -f "$tmp" 2>/dev/null
		echo "install.sh: WARNING: could not install $UPDATE_TOKEN_FILE — the update token was NOT provisioned (public-channel updates are unaffected)" >&2
		return 0
	fi
	INSTALLED_ANY=1
	echo "install.sh: provisioned the update token (0600) -> $UPDATE_TOKEN_FILE (used only if you repoint the updater at a private repo)"
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
			# macOS terminals spawn LOGIN shells, and login bash reads
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

# --- the toolset half: what "installing aterm" is actually FOR --------------------
# docs/GOLDEN-INSTALL-PATH.md §1: "Installing aterm installs all those packages."
# That was true only through the GUI. The payload rode down inside the DMG and
# then sat there unopened for anyone who installed from a terminal and stayed in
# one — no error, no notice, just ten programs that never existed.
#
# `atpkg seed` is the right thing to call rather than reimplementing any of
# its decisions here: it is local-only (a DirFetcher over the sealed payload —
# no network), and it already answers every "cannot seed" case as SUCCESS with
# a stable spoken marker (`seed-unusable:` for a machine the registry
# publishes nothing for, or a lapsed horizon, `seed-pending:` for
# [packages].seed_install = false, and a plain note when a previous
# `uninstall --all` declined the set). So a non-zero exit here is a REAL
# failure and nothing else, and the config knobs keep their meaning instead of
# being second-guessed in shell.
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

	if [[ "$(uname -s)" == Darwin && "${CONTAINER_KIND:-dmg}" != zip ]]; then
		echo "install.sh: installing the ALab toolset from the payload inside the app (no download)"
	else
		# No sealed payload can exist here — the Linux store layout has no
		# bundle, and the Intel Mac lean container ships without the seal — so
		# `pkg seed` resolves the signed NETWORK index and installs whatever is
		# published for this machine. Claiming "no download" on these paths
		# was simply false.
		echo "install.sh: checking the ALab toolset (aterm pkg seed — installs from the network index when builds exist for this machine)"
	fi
	if ! "$aterm_bin" pkg seed; then
		# NON-FATAL, and say why that is the right call: the terminal is
		# installed and working, and `aterm pkg seed` is re-runnable at any
		# time. Failing the whole install here would throw away a good app
		# over a recoverable toolset problem.
		echo "install.sh: NOTE: the ALab toolset did not install — aterm itself is fine." >&2
		echo "  retry with: $aterm_bin pkg seed     (diagnose with: $aterm_bin pkg doctor)" >&2
		return 0
	fi
	TOOLCHAIN_RAN=1

	# The seal is a SNAPSHOT taken when the release was staged, so a program
	# published between staging and now is still behind on a machine that has
	# only ever seen the payload. The GUI closes that on its 6h loop
	# (crates/aterm-gui/src/lib.rs), which is no help to someone who installed
	# from a terminal and never opens the app — the same hole this whole
	# function exists to close, one layer up. Cheap: only what actually drifted
	# is fetched, and an offline machine simply keeps the sealed builds.
	echo "install.sh: bringing the toolset up to the latest published builds"
	echo "  only out-of-date programs are downloaded; the rest are already current."
	if ! "$aterm_bin" pkg update; then
		echo "install.sh: NOTE: could not reach the index to check for newer builds." >&2
		# "the sealed builds are installed and usable" was only true on the
		# macOS DMG path — a lean/zip/Linux install has no seal, and a seed
		# that installed nothing leaves nothing "usable" (audit-2 item 6).
		echo "  anything already installed keeps working; retry later with: $aterm_bin pkg update" >&2
	fi
	# Deliberately does NOT set INSTALLED_ANY. This half runs on top of an app
	# the other halves (or a previous run) placed, and on an already-seeded
	# machine it correctly does nothing — claiming an install here would turn
	# the "nothing was installed" failure into a false success.
}

# --- PATH, for shells that are not aterm ------------------------------------------
# atpkg writes a correct per-shell hook into ~/.aterm/shell.d on every install,
# but ONLY an aterm session auto-sources it. So the toolset was installed,
# verified, kept current — and invisible in iTerm, VS Code's terminal, and over
# ssh, where `trustc` simply did not exist. `atpkg doctor` reported this as a
# warning with the exact fix, which is one step better than silence and several
# steps short of working.
#
# Sourcing the generated hook (rather than writing a PATH line) keeps ONE
# definition of the bin directory: atpkg rewrites the hook whenever the prefix
# moves, and this line picks that up without the rc file ever being touched again.
wire_shell_path() {
	local shell_name hook rc line path_line marker f
	shell_name="${SHELL##*/}"
	case "$shell_name" in
	zsh) hook="$HOME/.aterm/shell.d/00-atpkg.zsh"; rc="$HOME/.zshrc" ;;
	bash)
		hook="$HOME/.aterm/shell.d/00-atpkg.bash"
		rc="$HOME/.bashrc"
		# macOS terminals spawn LOGIN shells, and login bash reads
		# ~/.bash_profile (then .bash_login, then .profile) — never .bashrc.
		# Target the first file login bash will actually read; create
		# .bash_profile only when none exists (nothing is shadowed then).
		# On Linux, interactive terminals are non-login: .bashrc is right.
		if [[ "$(uname -s)" == Darwin ]]; then
			rc="$HOME/.bash_profile"
			for f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
				[[ -f "$f" ]] && { rc="$f"; break; }
			done
		fi
		;;
	# fish itself resolves its config through XDG_CONFIG_HOME; hardcoding
	# ~/.config wrote the block to a file fish never reads whenever that
	# variable is set — and to a file the uninstaller (which honors XDG)
	# could never find.
	fish) hook="$HOME/.aterm/shell.d/00-atpkg.fish"; rc="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish" ;;
	*)
		# An unrecognised shell gets told, not guessed at: a wrong line in a
		# login file is worse than no line.
		[[ -f "$HOME/.aterm/shell.d/00-atpkg.bash" ]] || return 0
		echo "install.sh: NOTE: source ~/.aterm/shell.d/00-atpkg.* from your ${shell_name:-shell} profile to get the ALab toolset on PATH" >&2
		return 0
		;;
	esac
	# No hook means no toolset was laid down (--no-toolchain, a machine the
	# index publishes nothing for, or a declined set). Nothing to point at, so
	# say nothing — UNLESS the toolset is deferred to first launch: that lane
	# lays the hook down minutes from now, and this script never runs again,
	# so the guarded block goes in ahead of it. Safe because the line written
	# below guards on the hook's existence itself — a no-op until first launch
	# creates it, active from then on.
	if [[ ! -f "$hook" ]]; then
		[[ "${TOOLCHAIN_DEFERRED:-0}" -eq 1 ]] || return 0
	fi

	marker="# >>> aterm ALab toolset (managed by install.sh) >>>"
	if [[ -f "$rc" ]] && grep -qF "$marker" "$rc" 2>/dev/null; then
		echo "install.sh: ALab toolset already on PATH via $rc"
		PATH_BLOCK_WROTE=1
		return 0
	fi
	# `if` rather than `[ -f … ] && .` on purpose: as the LAST statement in a
	# profile the && form leaves the rc file's exit status at 1 whenever the
	# hook is absent (an uninstalled toolset, a moved prefix), which is a
	# confusing thing to hand to anything that checks it. The `if` form is
	# always status 0 and reads better besides.
	if [[ "$shell_name" == fish ]]; then
		line="if test -f \"$hook\"; source \"$hook\"; end"
	else
		line="if [ -f \"$hook\" ]; then . \"$hook\"; fi"
	fi
	# The one `aterm` symlink needs $BIN_DIR on PATH just as much as the
	# toolset needs the hook — a default macOS install used to end with the
	# hand-edit hint scrolled away and `aterm` unreachable while this very
	# function edited the same profile for the toolset only. Carrying both in
	# the ONE managed block keeps uninstall's marker sweep the single undo for
	# everything install wrote into a profile. Guarded and APPENDED (matching
	# the hook's never-shadow-system-tools placement), so it never stacks
	# duplicates in nested shells.
	path_line=""
	case ":$PATH:" in
	*":$BIN_DIR:"*) ;;
	*)
		if [[ "$shell_name" == fish ]]; then
			path_line="if not contains -- \"$BIN_DIR\" \$PATH; set -gx PATH \$PATH \"$BIN_DIR\"; end"
		else
			path_line="case \":\$PATH:\" in *\":$BIN_DIR:\"*) ;; *) export PATH=\"\$PATH:$BIN_DIR\" ;; esac"
		fi
		;;
	esac
	mkdir -p "$(dirname "$rc")" 2>/dev/null || true
	if ! {
		if [[ -n "$path_line" ]]; then
			printf '\n%s\n%s\n%s\n%s\n' \
				"$marker" "$line" "$path_line" "# <<< aterm ALab toolset <<<"
		else
			printf '\n%s\n%s\n%s\n' \
				"$marker" "$line" "# <<< aterm ALab toolset <<<"
		fi
	} >>"$rc" 2>/dev/null; then
		echo "install.sh: NOTE: could not write $rc — add this line yourself for the ALab toolset on PATH:" >&2
		echo "  $line" >&2
		return 0
	fi
	PATH_BLOCK_WROTE=1
	# Tell the truth about WHEN the line does something: sourcing a hook that
	# does not exist yet (the deferred lane) would be advice that fails.
	local activate="open a new shell, or: . '$hook'"
	[[ -f "$hook" ]] || activate="the guarded line activates once first launch installs the toolset"
	if [[ -n "$path_line" ]]; then
		echo "install.sh: put the ALab toolset and $BIN_DIR on PATH in $rc — $activate"
	else
		echo "install.sh: put the ALab toolset on PATH in $rc — $activate"
	fi
	echo "  skip this next time with ATERM_NO_PATH=1"
}

# --- run what's possible, skip the rest loudly, fail only if nothing ran -------
if [[ "$DO_APP" -eq 1 ]]; then
	if [[ -n "$APP_SKIP" ]]; then
		echo "install.sh: SKIPPED the app: $APP_SKIP" >&2
	elif [[ -n "$APP_ALREADY" ]]; then
		# SATISFIED, not skipped: the elected release is already at the
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
if [[ "$TOKEN_WANTED" -eq 1 ]]; then
	provision_update_token
elif [[ "$DO_TOKEN" -eq 1 ]]; then
	# The honest default: the compiled-in public channel reads NO token file
	# (crates/aterm-update-core/src/token.rs), so no credential is copied
	# unless something will read it or the operator asks. --no-token stays
	# silent — an explicit exclusion needs no status line.
	echo "install.sh: token: skipped (public channel reads no token file; --token to provision for a repointed updater)"
fi
# After the CLI half: the seed lane is driven THROUGH the installed `aterm`, so
# it needs the symlink (or the bundle) to already exist. Before the PATH half,
# which points at a hook that only exists once atpkg has laid the toolset down.
#
# Deliberately NOT gated on the app half having run. `--no-app` is the normal
# way to say "the app is already installed"; refusing to seed there would leave
# the one command that repairs a missing toolset unable to reach the app
# sitting right in front of it. `install_toolchain` proves an `aterm` exists
# and skips with a reason when it does not, which is the check that matters.
if [[ "$DO_TOOLCHAIN" -eq 1 ]]; then
	if [[ "${TOOLCHAIN_DEFERRED:-0}" -eq 1 ]]; then
		# The lean default (docs/DESIGN-streaming-batteries-2026-08-23.md §7):
		# first launch provisions the toolset per program with live progress,
		# and the GUI's own seed pass records adoption there — the audited
		# consent seam (atpkg cmd_seed). Running `pkg seed` headlessly here
		# would block this install on the full download AND move the consent
		# write out of that seam, so the deferral IS the design, not a skip.
		# TOOLCHAIN_DEFERRED is set only by a fresh lean-DEFAULT app install;
		# every repair lane (--no-app, an already-current app, --batteries,
		# Linux) still runs install_toolchain synchronously right here.
		echo "install.sh: toolset: installs on first launch — open aterm and the ALab toolchain"
		echo "install.sh:   downloads itself with live progress, program by program. Terminal-first"
		echo "install.sh:   instead: aterm pkg install --default-set"
	else
		install_toolchain
	fi
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
	# A toolchain/PATH-only invocation (--no-app --no-cli --no-token) installs
	# nothing by INSTALLED_ANY's deliberately narrow definition even when it
	# did exactly what was asked; a completed seed or a standing managed PATH
	# block IS that run's success, so it must not exit "nothing was installed".
	if [[ "$DO_APP" -eq 0 && "$DO_CLI" -eq 0 && "$DO_TOKEN" -eq 0 ]] &&
		[[ "$TOOLCHAIN_RAN" -eq 1 || "$PATH_BLOCK_WROTE" -eq 1 ]]; then
		exit 0
	fi
	echo "install.sh: nothing was installed" >&2
	exit 1
fi
