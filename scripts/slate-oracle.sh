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
#   slate-oracle.sh build SRC [BIN]
#       Build BIN (`ntfs-chkdsk` unless named; `ntfs-write-lab` is the other)
#       from SRC's tools crate (src/tools, MIT), --locked and --release, at
#       this repository's pinned toolchain, and print the binary's path. Only
#       the MIT crates are built: ntfs_utils (MIT AND GPL-2.0-or-later),
#       kernel/ and kernel/patches/ are never compiled, and nothing here
#       links Slate into this crate.
#
#   slate-oracle.sh write WRITES IMAGES WRITER OUT FLOOR
#       Slate's copy-only writer on copies (#863). WRITES is a header and then
#       one `case source root_file offset expected_hex replacement_hex` row
#       per write (tab-separated). WRITER runs as ntfs-write-lab does,
#       `WRITER SOURCE NEW ROOT_FILE OFFSET EXPECTED REPLACEMENT`, but SOURCE
#       is a read-only copy of IMAGES/<source>, so the image itself is never
#       handed to it. The copy must keep the source's SHA-256, the writer must
#       exit 0, and NEW must differ from the source. OUT/images/<case>.img
#       keeps each written image, OUT/<case>.write.txt the writer's output and
#       OUT/slate-writes.tsv one row per case. At least FLOOR cases must be
#       written.
#
#   slate-oracle.sh replay CAPTURES REPLAYER OUT FLOOR
#       Slate's offline journal recovery on every Windows interrupted-write
#       capture (#871). A capture is each CAPTURES/windows-interrupted-<case>
#       .img.gz other than Windows' own .recovered.img.gz, and it must have
#       Windows' .recovered.manifest beside it. REPLAYER runs as ntfs-chkdsk
#       does, `REPLAYER --replay-to SOURCE NEW`, with SOURCE a read-only
#       decompressed copy, so the capture itself is never handed to it. Exit 0
#       with NEW written is `replayed`; any other exit is `refused`, recorded
#       as such and never counted as a replay. A changed source, or exit 0 with
#       no NEW, fails by name. OUT/images/<case>.img keeps each replayed image,
#       OUT/<case>.replay.txt the replayer's output and OUT/slate-replays.tsv
#       one row per capture. At least FLOOR captures must be replayed.
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
#       checker's report. At least FLOOR cases must agree with Windows or
#       match a graded ledger row.
#
#       Every disagreement is looked up in the ledger, $SLATE_LEDGER or
#       tests/slate-disagreements.tsv (#693). A disagreement with a graded
#       row is `recorded`: reported with its row and not a failure. One with
#       no row is new and fails. A row Windows has not graded fails, and so
#       does a row whose case did not disagree in this run: it no longer
#       reproduces and must be removed, so the ledger cannot go stale.
#
#   slate-oracle.sh ledger [LEDGER]
#       Check the ledger alone (default tests/slate-disagreements.tsv): a
#       header and then one `case regenerate windows_chkdsk windows_reading
#       assigned issue` row per disagreement (tab-separated). regenerate is
#       the command that rebuilds the minimized image, and its first word is
#       a generator in this repository, never a committed disk image.
#       windows_chkdsk is Windows' verdict on that image, `clean` or
#       `not-clean`, or `ungraded`. A graded row names Windows' reading of
#       the image, the implementation it is assigned to (`ours` or `slate`)
#       and the issue; an ungraded row is assigned to nobody (`-`), because
#       no row is assigned before Windows has graded it.
#
# Exit 0 when every case ran and agreed with Windows and the floor was met;
# 1 when any case disagreed, could not be graded, or the floor was missed;
# 2 for a usage error. Every case is graded before the verdict, so one run
# names all of its problems.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    echo "usage: slate-oracle.sh fetch DEST | build SRC [BIN] | compare CASES IMAGES CHECKER OUT FLOOR | ledger [LEDGER] | write WRITES IMAGES WRITER OUT FLOOR | replay CAPTURES REPLAYER OUT FLOOR" >&2
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
    [ "$#" -eq 1 ] || [ "$#" -eq 2 ] || usage
    local src=$1 bin=${2:-ntfs-chkdsk}
    case "$bin" in
        ntfs-chkdsk | ntfs-write-lab) ;;
        *) die "build makes ntfs-chkdsk or ntfs-write-lab, not '$bin'" ;;
    esac
    [ -f "$src/src/tools/Cargo.toml" ] || die "$src has no src/tools/Cargo.toml"
    # Run from this repository, so rustup picks the toolchain its
    # rust-toolchain.toml pins rather than whatever the runner has.
    (cd "$ROOT" && CARGO_TARGET_DIR="$src/target" cargo build --quiet --locked --release \
        --manifest-path "$src/src/tools/Cargo.toml" --bin "$bin") >&2 ||
        die "cannot build $bin from $src"
    [ -x "$src/target/release/$bin" ] || die "the build left no $src/target/release/$bin"
    printf '%s\n' "$src/target/release/$bin"
}

write() {
    [ "$#" -eq 5 ] || usage
    local writes=$1 images=$2 writer=$3 out=$4 floor=$5
    [[ $floor =~ ^[0-9]+$ ]] || usage
    [ -f "$writes" ] || die "no write table at $writes"
    [ -x "$writer" ] || die "no writer at $writer"
    mkdir -p "$out/work" "$out/images" || die "cannot create $out"
    printf 'case\tsource\tsource_sha256\twritten_sha256\twriter_exit\tresult\n' >"$out/slate-writes.tsv"

    local rows=0 written=0 problems=0
    local name source root_file offset expected replacement
    while IFS=$'\t' read -r name source root_file offset expected replacement; do
        replacement="${replacement%$'\r'}"
        [ "$name" = case ] && continue
        [ -n "$name" ] || continue
        rows=$((rows + 1))
        local original="$images/$source" staged="$out/work/$name.source.img"
        local new="$out/images/$name.img" report="$out/$name.write.txt"
        local digest=- after=- status=- result
        rm -f "$new"

        if [ ! -f "$original" ]; then
            result=missing
            echo "FAIL $name: no source image at $original"
        else
            digest="$(sha "$original")"
            if ! cp "$original" "$staged" || ! chmod 0444 "$staged"; then
                die "cannot make a read-only copy of $original"
            fi
            "$writer" "$staged" "$new" "$root_file" "$offset" "$expected" "$replacement" >"$report" 2>&1
            status=$?
            [ -f "$new" ] && after="$(sha "$new")"
            if [ "$(sha "$original")" != "$digest" ] || [ "$(sha "$staged")" != "$digest" ]; then
                result='source-changed'
                echo "FAIL $name: the writer changed the source it was given, not only its copy"
            elif [ "$status" -ne 0 ]; then
                result='writer-error'
                echo "FAIL $name: the writer ended with exit $status; see $report"
            elif [ "$after" = - ]; then
                result='no-copy'
                echo "FAIL $name: the writer exited 0 and left no image at $new"
            elif [ "$after" = "$digest" ]; then
                result=unchanged
                echo "FAIL $name: the written copy is byte-identical to its source"
            else
                result=written
                written=$((written + 1))
            fi
        fi
        # The source is in IMAGES and the written copy in OUT/images; the
        # staged copy is not kept.
        chmod u+w "$staged" 2>/dev/null
        rm -f "$staged"
        [ "$result" = written ] || problems=$((problems + 1))
        printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$source" "$digest" "$after" "$status" "$result" \
            >>"$out/slate-writes.tsv"
    done <"$writes"

    if [ "$written" -lt "$floor" ]; then
        echo "FAIL $written of $rows cases were written, below the floor of $floor"
        problems=$((problems + 1))
    fi
    if [ "$problems" -gt 0 ]; then
        echo "slate-oracle: $problems problem(s) in $rows writes; table in $out/slate-writes.tsv"
        return 1
    fi
    echo "slate-oracle: $written of $rows cases written on copies (floor $floor); table in $out/slate-writes.tsv"
}

replay() {
    [ "$#" -eq 4 ] || usage
    local captures=$1 replayer=$2 out=$3 floor=$4
    [[ $floor =~ ^[0-9]+$ ]] || usage
    [ -d "$captures" ] || die "no capture directory at $captures"
    [ -x "$replayer" ] || die "no replayer at $replayer"
    mkdir -p "$out/work" "$out/images" || die "cannot create $out"
    printf 'case\tsource_sha256\treplayed_sha256\treplayer_exit\toutcome\n' >"$out/slate-replays.tsv"

    local rows=0 replayed=0 problems=0 capture
    for capture in "$captures"/windows-interrupted-*.img.gz; do
        [ -f "$capture" ] || continue
        case "$capture" in *.recovered.img.gz) continue ;; esac
        rows=$((rows + 1))
        local name
        name="$(basename "$capture" .img.gz)"
        name="${name#windows-interrupted-}"
        local packed staged="$out/work/$name.source.img" new="$out/images/$name.img"
        local report="$out/$name.replay.txt" digest=- after=- status=- outcome
        packed="$(sha "$capture")"
        rm -f "$new"

        if [ ! -f "$captures/windows-interrupted-$name.recovered.manifest" ]; then
            outcome='no-windows-manifest'
            echo "FAIL $name: no Windows recovered manifest beside $capture"
        else
            if ! gzip -dc "$capture" >"$staged" || ! chmod 0444 "$staged"; then
                die "cannot make a read-only copy of $capture"
            fi
            digest="$(sha "$staged")"
            "$replayer" --replay-to "$staged" "$new" >"$report" 2>&1
            status=$?
            [ -f "$new" ] && after="$(sha "$new")"
            if [ "$(sha "$capture")" != "$packed" ] || [ "$(sha "$staged")" != "$digest" ]; then
                outcome='source-changed'
                echo "FAIL $name: the replayer changed the source it was given, not only its new image"
            elif [ "$status" -ne 0 ]; then
                # A refusal is Slate's answer, not a failure of the run; it
                # is never counted as a replay.
                outcome=refused
                after=-
            elif [ "$after" = - ]; then
                outcome='no-image'
                echo "FAIL $name: the replayer exited 0 and left no image at $new"
            else
                outcome=replayed
                replayed=$((replayed + 1))
            fi
        fi
        # The capture stays in CAPTURES and the replayed image in OUT/images;
        # the staged copy is not kept.
        chmod u+w "$staged" 2>/dev/null
        rm -f "$staged"
        case "$outcome" in replayed | refused) ;; *) problems=$((problems + 1)) ;; esac
        printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$digest" "$after" "$status" "$outcome" \
            >>"$out/slate-replays.tsv"
    done

    if [ "$rows" -eq 0 ]; then
        echo "FAIL no windows-interrupted-*.img.gz capture in $captures"
        problems=$((problems + 1))
    fi
    if [ "$replayed" -lt "$floor" ]; then
        echo "FAIL $replayed of $rows captures were replayed, below the floor of $floor"
        problems=$((problems + 1))
    fi
    if [ "$problems" -gt 0 ]; then
        echo "slate-oracle: $problems problem(s) in $rows captures; table in $out/slate-replays.tsv"
        return 1
    fi
    echo "slate-oracle: $replayed of $rows captures replayed into new images (floor $floor); table in $out/slate-replays.tsv"
}

# ledger_problems LEDGER: one `FAIL` line per malformed or ungraded row.
ledger_problems() {
    local ledger=$1 name regenerate windows reading assigned issue generator
    [ -f "$ledger" ] || { echo "FAIL no disagreement ledger at $ledger"; return; }
    while IFS=$'\t' read -r name regenerate windows reading assigned issue; do
        issue="${issue%$'\r'}"
        [ "$name" = case ] && continue
        [ -n "$name" ] || continue
        generator="${regenerate%% *}"
        if [ -z "$generator" ] || [ ! -f "$ROOT/$generator" ]; then
            echo "FAIL ledger $name: regenerate '$regenerate' does not start with a generator in this repository"
        fi
        case "$generator" in
            *.img | *.img.gz | *.vhd | *.vhdx | *.raw | *.gz | *.zst | *.xz)
                echo "FAIL ledger $name: regenerate names a disk image, not a generator: '$regenerate'" ;;
        esac
        case "$windows" in
            clean | not-clean)
                [ -n "$reading" ] && [ "$reading" != - ] ||
                    echo "FAIL ledger $name: no Windows reading of the minimized image"
                case "$assigned" in
                    ours | slate) ;;
                    *) echo "FAIL ledger $name: assigned '$assigned' is neither ours nor slate" ;;
                esac
                [ -n "$issue" ] && [ "$issue" != - ] || echo "FAIL ledger $name: no issue for the assigned defect"
                ;;
            ungraded)
                echo "FAIL ledger $name: Windows has not graded the minimized image; the row is not graded"
                if [ "$assigned" != - ] || [ "$issue" != - ]; then
                    echo "FAIL ledger $name: assigned to '$assigned' ($issue) without a Windows verdict"
                fi
                ;;
            *) echo "FAIL ledger $name: windows_chkdsk '$windows' is neither clean, not-clean nor ungraded" ;;
        esac
    done <"$ledger"
}

# ledger_row LEDGER CASE: the case's row, without its case field, or nothing.
ledger_row() {
    [ -f "$1" ] || return 0
    awk -F'\t' -v c="$2" 'NR > 1 && $1 == c { sub(/\r$/, ""); sub(/^[^\t]*\t/, ""); print; exit }' "$1"
}

ledger() {
    [ "$#" -le 1 ] || usage
    local file="${1:-${SLATE_LEDGER:-$ROOT/tests/slate-disagreements.tsv}}" found
    found="$(ledger_problems "$file")"
    if [ -n "$found" ]; then
        printf '%s\n' "$found"
        echo "slate-oracle: $(printf '%s\n' "$found" | wc -l | tr -d ' ') problem(s) in $file"
        return 1
    fi
    echo "slate-oracle: $file is a well-formed ledger"
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

    local ledger="${SLATE_LEDGER:-$ROOT/tests/slate-disagreements.tsv}" found row
    local rows=0 agreed=0 recorded=0 problems=0 reproduced=' '
    found="$(ledger_problems "$ledger")"
    if [ -n "$found" ]; then
        printf '%s\n' "$found"
        problems=$((problems + $(printf '%s\n' "$found" | wc -l)))
    fi
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
                row="$(ledger_row "$ledger" "$name")"
                if [ -n "$row" ]; then
                    result=recorded
                    recorded=$((recorded + 1))
                    reproduced="$reproduced$name "
                    echo "RECORDED $name: Windows chkdsk accepts this $origin image and Slate reports findings; ledger row: ${row//$'\t'/ | }"
                else
                    result=disagree
                    echo "FAIL $name: Windows chkdsk accepts this $origin image and Slate reports findings, and the case is not in the ledger $ledger; see $report"
                fi
            fi
        fi
        # The image Windows graded is already in the artifact; its copy is not kept.
        rm -f "$copy"
        case "$result" in agree | recorded) ;; *) problems=$((problems + 1)) ;; esac
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$origin" "$digest" "$windows" \
            "$windows_exit" "$slate" "$status" "$result" >>"$out/verdicts.tsv"
    done <"$cases"

    # A row whose case did not disagree here no longer reproduces.
    if [ -f "$ledger" ]; then
        local listed
        while IFS=$'\t' read -r listed _; do
            [ "$listed" = case ] && continue
            [ -n "$listed" ] || continue
            case "$reproduced" in *" $listed "*) continue ;; esac
            echo "FAIL ledger $listed: the disagreement no longer reproduces in this run; remove its row"
            problems=$((problems + 1))
        done <"$ledger"
    fi
    if [ $((agreed + recorded)) -lt "$floor" ]; then
        echo "FAIL $agreed of $rows cases agreed with Windows chkdsk and $recorded matched the ledger, below the floor of $floor"
        problems=$((problems + 1))
    fi
    if [ "$problems" -gt 0 ]; then
        echo "slate-oracle: $problems problem(s) in $rows cases; table in $out/verdicts.tsv"
        return 1
    fi
    echo "slate-oracle: $agreed of $rows cases agree with Windows chkdsk and $recorded are recorded disagreements (floor $floor); table in $out/verdicts.tsv"
}

[ "$#" -ge 1 ] || usage
command=$1
shift
case "$command" in
    fetch) fetch "$@" ;;
    build) build "$@" ;;
    compare) compare "$@" ;;
    ledger) ledger "$@" ;;
    write) write "$@" ;;
    replay) replay "$@" ;;
    *) usage ;;
esac
