#!/usr/bin/env bash
# The release tarball holds exactly `mkfs.ntfs` and the licences, and the
# binary in it runs.
#
# Cargo refuses a dot in a target name, so the formatter builds as
# `mkfs_ntfs`. scripts/package-cli.sh renames it to `mkfs.ntfs` before
# packaging; the underscore is a build-system constraint and must not reach
# a public artifact. `rust-ntfs`, the test driver, is not shipped at all.
#
# This runs the real packaging script against stand-in binaries in a
# sandbox: one that behaves, and one for each way a build can be wrong
# (missing, --help failing, reporting a version other than the tag's). The
# release workflow runs the same script against the real binary on every
# platform it publishes, so the checks here are the checks a release makes.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PACKAGE="$ROOT/scripts/package-cli.sh"
pass=0
fail=0

ok()  { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$*"; }

sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT

crate="$(sed -n 's/^name = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
[ "$crate" = "am-fs-ntfs" ] && ok || bad "crate name read from Cargo.toml: '$crate'"

# A stand-in for the built formatter. $1 is the version it reports, $2 the
# exit status of --help.
stub() {
    local path="$sandbox/$3"
    mkdir -p "$(dirname "$path")"
    cat > "$path" <<STUB
#!/usr/bin/env bash
case "\$1" in
    --help)    echo "Usage: mkfs.ntfs [options] <device>"; exit $2 ;;
    --version) echo "mkfs.ntfs ($crate) $1" ;;
    *)         exit 2 ;;
esac
STUB
    chmod +x "$path"
    printf '%s\n' "$path"
}

# Runs the packaging script in a fresh output directory; prints its stdout.
package() {
    local out="$sandbox/out-$RANDOM$RANDOM"
    mkdir -p "$out"
    (cd "$out" && bash "$PACKAGE" "$@" 2>"$sandbox/stderr")
}

[ -f "$PACKAGE" ] && ok || bad "scripts/package-cli.sh exists"

# --- A good build: the tarball, its name, and exactly its contents. -------
good="$(stub 9.9.9 0 good/mkfs_ntfs)"
if tarball="$(package 9.9.9 darwin-arm64 "$good")"; then
    ok
else
    bad "a good build packages: $(cat "$sandbox/stderr")"
    tarball=""
fi

case "$(basename "$tarball")" in
    "$crate-9.9.9-darwin-arm64.tar.gz") ok ;;
    *) bad "tarball is named <crate>-<version>-<label>.tar.gz, got '$tarball'" ;;
esac

if [ -f "$tarball" ]; then
    listing="$(tar -tzf "$tarball" | sort | tr '\n' ' ')"
    [ "$listing" = "LICENSE-APACHE LICENSE-MIT mkfs.ntfs " ] && ok \
        || bad "tarball holds exactly mkfs.ntfs and the licences, got: $listing"

    case "$listing" in
        *mkfs_ntfs*) bad "the cargo target name reached the tarball: $listing" ;;
        *) ok ;;
    esac
    case "$listing" in
        *rust-ntfs*) bad "the test driver was shipped: $listing" ;;
        *) ok ;;
    esac

    unpacked="$sandbox/unpacked"
    mkdir -p "$unpacked"
    tar -xzf "$tarball" -C "$unpacked"
    [ -x "$unpacked/mkfs.ntfs" ] && ok || bad "mkfs.ntfs is executable in the tarball"
    cmp -s "$unpacked/LICENSE-MIT" "$ROOT/LICENSE-MIT" && ok || bad "LICENSE-MIT is the repository's"
    cmp -s "$unpacked/LICENSE-APACHE" "$ROOT/LICENSE-APACHE" && ok || bad "LICENSE-APACHE is the repository's"
    cmp -s "$unpacked/mkfs.ntfs" "$good" && ok || bad "mkfs.ntfs is the built binary, renamed"
fi

# --- Each way a build can be wrong is refused, with no tarball left. ------
refused() {
    local why="$1"; shift
    local out
    if out="$(package "$@")"; then
        bad "$why is refused, but packaging succeeded: $out"
    else
        ok
        [ -z "$out" ] && ok || bad "$why leaves no tarball named on stdout: $out"
    fi
}

refused "a missing binary" 9.9.9 darwin-arm64 "$sandbox/nowhere/mkfs_ntfs"
refused "a binary whose --help fails" 9.9.9 darwin-arm64 "$(stub 9.9.9 1 helpfails/mkfs_ntfs)"
refused "a binary reporting a version other than the tag's" 9.9.9 darwin-arm64 "$(stub 1.0.0 0 wrongver/mkfs_ntfs)"
refused "a missing label" 9.9.9 "" "$good"
refused "a missing version" "" darwin-arm64 "$good"

# --- The release workflow packages through this script. ------------------
release="$ROOT/.github/workflows/release.yml"
grep -q 'scripts/package-cli.sh' "$release" && ok \
    || bad "release.yml packages through scripts/package-cli.sh"
grep -q 'cargo build --release --locked --bin mkfs_ntfs' "$release" && ok \
    || bad "release.yml builds the mkfs_ntfs target"
printf 'package-cli: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
