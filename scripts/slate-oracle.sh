#!/usr/bin/env bash
# slate-oracle.sh -- Slate's offline checker beside Windows chkdsk (#687).
#
# Slate (github.com/NullMagic2/slate-ntfs) is a third NTFS implementation. It
# is a supplementary oracle: it can show where two implementations read the
# same bytes differently, and it cannot by itself say which one Windows
# agrees with. So every image it grades has already been graded by Windows
# chkdsk on the same bytes, and Windows' verdict is the reference.
#
#   slate-oracle.sh fetch DEST
#       Clone Slate into DEST at the pinned commit and check it: the commit is
#       the full SHA the pin names, LICENSE has the pinned SHA-256, and the
#       core and tools crates both declare `license = "MIT"`. The pins are
#       SLATE_URL, SLATE_REF and SLATE_LICENSE_SHA256 in chores.yml, or in the
#       file $SLATE_PINS names.
#
#   slate-oracle.sh build SRC
#       Build `ntfs-chkdsk` from SRC's tools crate (src/tools, MIT), --locked
#       and --release, at this repository's pinned toolchain, and print the
#       binary's path. Only the MIT crates are built: ntfs_utils (MIT AND
#       GPL-2.0-or-later), kernel/ and kernel/patches/ are never compiled, and
#       nothing here links Slate into this crate.
#
#   slate-oracle.sh compare CASES IMAGES CHECKER OUT FLOOR
#       CASES is the Windows job's table, a header and then one
#       `case origin image sha256 windows_verdict windows_exit` row per image
#       (tab-separated; windows_verdict is `clean` or `not-clean`). Each image
#       in IMAGES must have the SHA-256 Windows graded. CHECKER runs as
#       `CHECKER --check COPY` on a read-only copy, so it can neither change
#       the image nor answer about different bytes. Slate's statuses are 0
#       for clean and 4 for findings; anything else is a failure of the run.
#       OUT/verdicts.tsv gets one row per case, OUT/<case>.slate.txt the
#       checker's report. At least FLOOR cases must agree with Windows.
#
# Exit 0 when every case ran and agreed with Windows and the floor was met;
# 1 when any case disagreed, could not be graded, or the floor was missed;
# 2 for a usage error. Every case is graded before the verdict, so one run
# names all of its problems.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    echo "usage: slate-oracle.sh fetch DEST | build SRC | compare CASES IMAGES CHECKER OUT FLOOR" >&2
    exit 2
}
die() { echo "slate-oracle: $*" >&2; exit 1; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

# pin NAME: one value from the vars block of the pins file.
pin() {
    local pins="${SLATE_PINS:-$ROOT/chores.yml}" value
    value="$(sed -n "s/^  $1: *//p" "$pins" | tr -d '\r' | head -n 1)"
    [ -n "$value" ] || die "$1 is not declared in $pins"
    printf '%s\n' "$value"
}

# declares_mit MANIFEST: the [package] section's licence is MIT and nothing else.
declares_mit() {
    awk '
        /^\[/ { in_package = ($0 == "[package]") }
        in_package && /^license[[:space:]]*=/ { found = $0 }
        END { exit !(found ~ /^license[[:space:]]*=[[:space:]]*"MIT"[[:space:]]*$/) }
    ' "$1"
}

fetch() {
    [ "$#" -eq 1 ] || usage
    local dest=$1 url ref licence
    url="$(pin SLATE_URL)" || exit 1
    ref="$(pin SLATE_REF)" || exit 1
    licence="$(pin SLATE_LICENSE_SHA256)" || exit 1
    [[ $ref =~ ^[0-9a-f]{40}$ ]] || die "SLATE_REF must be a full 40-digit commit SHA, not '$ref'"
    [ ! -e "$dest" ] || die "$dest already exists"

    git clone --quiet --no-checkout "$url" "$dest" || die "cannot clone $url"
    git -C "$dest" checkout --quiet --detach "$ref" || die "$url holds no commit $ref"
    [ "$(git -C "$dest" rev-parse HEAD)" = "$ref" ] || die "the checkout is not at $ref"

    [ -f "$dest/LICENSE" ] || die "the checkout at $ref has no LICENSE"
    local actual
    actual="$(sha "$dest/LICENSE")"
    [ "$actual" = "$licence" ] || die "LICENSE at $ref has SHA-256 $actual, not the pinned $licence"
    declares_mit "$dest/Cargo.toml" || die "the core crate at $ref does not declare license = \"MIT\""
    declares_mit "$dest/src/tools/Cargo.toml" || die "the tools crate at $ref does not declare license = \"MIT\""
    [ -f "$dest/src/tools/Cargo.lock" ] || die "the tools crate at $ref has no Cargo.lock to build --locked from"
    echo "slate-oracle: $url at $ref, LICENSE $licence, core and tools MIT"
}

build() {
    [ "$#" -eq 1 ] || usage
    local src=$1
    [ -f "$src/src/tools/Cargo.toml" ] || die "$src has no src/tools/Cargo.toml"
    # Run from this repository, so rustup picks the toolchain its
    # rust-toolchain.toml pins rather than whatever the runner has.
    (cd "$ROOT" && CARGO_TARGET_DIR="$src/target" cargo build --quiet --locked --release \
        --manifest-path "$src/src/tools/Cargo.toml" --bin ntfs-chkdsk) >&2 ||
        die "cannot build ntfs-chkdsk from $src"
    [ -x "$src/target/release/ntfs-chkdsk" ] || die "the build left no $src/target/release/ntfs-chkdsk"
    printf '%s\n' "$src/target/release/ntfs-chkdsk"
}

compare() {
    [ "$#" -eq 5 ] || usage
    local cases=$1 images=$2 checker=$3 out=$4 floor=$5
    [[ $floor =~ ^[0-9]+$ ]] || usage
    [ -f "$cases" ] || die "no cases table at $cases"
    [ -x "$checker" ] || die "no checker at $checker"
    mkdir -p "$out/work" || die "cannot create $out"
    printf 'case\torigin\tsha256\twindows_verdict\twindows_exit\tslate_verdict\tslate_exit\tresult\n' \
        >"$out/verdicts.tsv"

    local rows=0 agreed=0 problems=0
    local name origin image digest windows windows_exit
    while IFS=$'\t' read -r name origin image digest windows windows_exit; do
        windows_exit="${windows_exit%$'\r'}"
        [ "$name" = case ] && continue
        [ -n "$name" ] || continue
        rows=$((rows + 1))
        local source="$images/$image" copy="$out/work/$name.img" report="$out/$name.slate.txt"
        local slate=- status=- result

        if [ "$windows" != clean ] && [ "$windows" != not-clean ]; then
            result=malformed
            echo "FAIL $name: windows_verdict '$windows' is neither clean nor not-clean"
        elif [ ! -f "$source" ]; then
            result=missing
            echo "FAIL $name: no image at $source"
        elif [ "$(sha "$source")" != "$digest" ]; then
            result='hash-mismatch'
            echo "FAIL $name: $source is not the image Windows graded (SHA-256 $digest)"
        else
            if ! cp "$source" "$copy" || ! chmod 0444 "$copy"; then
                die "cannot make a read-only copy of $source"
            fi
            "$checker" --check "$copy" >"$report" 2>&1
            status=$?
            case "$status" in
                0) slate=clean ;;
                4) slate=findings ;;
                *) slate=error ;;
            esac
            if [ "$(sha "$copy")" != "$digest" ]; then
                result=changed
                echo "FAIL $name: the checker changed the image it was given"
            elif [ "$slate" = error ]; then
                result='checker-error'
                echo "FAIL $name: the checker ended with exit $status, neither clean (0) nor findings (4); see $report"
            elif [ "$windows" != clean ]; then
                # This tier compares cases Windows accepts. A case it rejects
                # is a broken control or a defect of ours, never an agreement.
                result='windows-rejected'
                echo "FAIL $name: Windows chkdsk did not accept this $origin image (exit $windows_exit)"
            elif [ "$slate" = clean ]; then
                result=agree
                agreed=$((agreed + 1))
            else
                result=disagree
                echo "FAIL $name: Windows chkdsk accepts this $origin image and Slate reports findings; see $report"
            fi
        fi
        # The image Windows graded is already in the artifact; its copy is not kept.
        rm -f "$copy"
        [ "$result" = agree ] || problems=$((problems + 1))
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$origin" "$digest" "$windows" \
            "$windows_exit" "$slate" "$status" "$result" >>"$out/verdicts.tsv"
    done <"$cases"

    if [ "$agreed" -lt "$floor" ]; then
        echo "FAIL $agreed of $rows cases agreed with Windows chkdsk, below the floor of $floor"
        problems=$((problems + 1))
    fi
    if [ "$problems" -gt 0 ]; then
        echo "slate-oracle: $problems problem(s) in $rows cases; table in $out/verdicts.tsv"
        return 1
    fi
    echo "slate-oracle: $agreed of $rows cases agree with Windows chkdsk (floor $floor); table in $out/verdicts.tsv"
}

[ "$#" -ge 1 ] || usage
command=$1
shift
case "$command" in
    fetch) fetch "$@" ;;
    build) build "$@" ;;
    compare) compare "$@" ;;
    *) usage ;;
esac
