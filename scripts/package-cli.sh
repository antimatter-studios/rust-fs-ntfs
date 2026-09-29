#!/usr/bin/env bash
# package-cli.sh <version> <label> [target-dir]
#
# Package the built command-line tools as a release tarball in the current
# directory, check it, and print its file name on stdout.
#
#   <version>     the release version, without the leading `v`
#   <label>       the platform, e.g. darwin-arm64 or linux-x86_64
#   [target-dir]  where cargo put the release build (default: target/release)
#
# THE TARBALL IS THE CONTRACT with whatever installs it. Its layout is a
# prefix, so an installer copies it as-is and needs to know nothing about
# which tools are in it:
#
#   bin/<repo>                  the multi-call binary, the one real file
#   bin/<tool>... -> <repo>     every dotted name, a RELATIVE symlink to it
#   share/<repo>/CAVEATS        at most four lines an installer shows
#   LICENSE-MIT, LICENSE-APACHE
#
# Man pages and shell completions, when there are any, go under share/ the
# same way. <repo> is the repository's name, from Cargo.toml's `repository`,
# and it is also the binary's cargo target name.
#
# THE DOTTED NAMES ARE MADE HERE. Cargo refuses a dot in a target name, so
# the binary builds as `<repo>` and dispatches on the name it was started
# under; the names to link come from the binary itself (`<repo> generate
# names`), so this script names no tool and cannot disagree with it.
# `rust-ntfs`, the CLI the test matrix drives, is not shipped.
#
# THEN IT CHECKS WHAT IT BUILT, because a tarball whose tools do not run is
# worse than no tarball: the failure would surface as a user's bug report
# rather than a red release. The member list must be exactly the intended
# one, every dotted name must be a relative symlink to the binary, and every
# name must answer --help and report `<name> (<crate>) <version>` from
# --version, which identifies it among same-named tools from other packages
# and catches a tag that disagrees with Cargo.toml. On any failure nothing
# is printed on stdout and the tarball is removed.
# tests/scripts/package-cli.sh holds this script to all of that.
set -euo pipefail

version="${1:-}"
label="${2:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${3:-$root/target/release}"

licences=(LICENSE-APACHE LICENSE-MIT)
# What an installer prints after install is kept short enough to be read.
max_caveat_lines=4

die() { echo "package-cli: $*" >&2; exit 1; }

[ -n "$version" ] || die "usage: package-cli.sh <version> <label> [target-dir]"
[ -n "$label" ] || die "usage: package-cli.sh <version> <label> [target-dir]"

crate="$(sed -n 's/^name = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
[ -n "$crate" ] || die "no package name in $root/Cargo.toml"
repo="$(sed -n 's/^repository = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
repo="${repo%/}"
repo="${repo##*/}"
[ -n "$repo" ] || die "no repository in $root/Cargo.toml"

tarball="$crate-$version-$label.tar.gz"
work=""

# ON ANY FAILURE, NO TARBALL: not a partial one, and not one a previous run
# left under the same name, which a caller could otherwise take for this
# run's output. The trap is installed before anything below can fail,
# mktemp included.
cleanup() {
    local status=$?
    [ -z "$work" ] || rm -rf "$work"
    [ "$status" -eq 0 ] || rm -f "$tarball"
    return "$status"
}
trap cleanup EXIT
work="$(mktemp -d)"

stage="$work/stage"
mkdir -p "$stage/bin" "$stage/share/$repo" "$work/unpacked"
built="$target_dir/$repo"
[ -x "$built" ] || die "no built $repo at $built (cargo build --release --locked --features cli --bin $repo)"
cp "$built" "$stage/bin/$repo"
chmod 755 "$stage/bin/$repo"
want=("bin/$repo")
names="$("$stage/bin/$repo" generate names)" || die "$repo generate names failed"
[ -n "$names" ] || die "$repo generate names listed no tools"
for name in $names; do
    case "$name" in
        */* | .* | "$repo") die "$repo generate names listed '$name', which is not a tool name" ;;
    esac
    ln -s "$repo" "$stage/bin/$name"
    want+=("bin/$name")
done
cp "$root/packaging/CAVEATS" "$stage/share/$repo/CAVEATS"
want+=("share/$repo/CAVEATS")
for f in "${licences[@]}"; do
    cp "$root/$f" "$stage/$f"
    want+=("$f")
done

# COPYFILE_DISABLE keeps macOS tar from adding ._ AppleDouble members.
COPYFILE_DISABLE=1 tar -czf "$tarball" -C "$stage" bin share "${licences[@]}"

# Files and links only: whether a tar lists the directories themselves
# varies by tar.
want_list="$(printf '%s\n' "${want[@]}" | sort)"
got_list="$(tar -tzf "$tarball" | sed 's|^\./||' | grep -v '/$' | sort)"
[ "$got_list" = "$want_list" ] \
    || die "$tarball holds [$(echo $got_list)], expected [$(echo $want_list)]"

tar -xzf "$tarball" -C "$work/unpacked"
caveats="$work/unpacked/share/$repo/CAVEATS"
[ -s "$caveats" ] || die "share/$repo/CAVEATS is empty"
lines="$(wc -l <"$caveats" | tr -d ' ')"
[ "$lines" -le "$max_caveat_lines" ] \
    || die "share/$repo/CAVEATS is $lines lines; an installer prints it, so it is at most $max_caveat_lines"
exe="$work/unpacked/bin/$repo"
[ -f "$exe" ] && [ ! -L "$exe" ] || die "bin/$repo is not a regular file in $tarball"
[ -x "$exe" ] || die "bin/$repo is not executable in $tarball"
for name in $names; do
    link="$work/unpacked/bin/$name"
    [ -L "$link" ] || die "bin/$name is not a symlink in $tarball"
    target="$(readlink "$link")"
    [ "$target" = "$repo" ] \
        || die "bin/$name points at '$target', expected the relative link '$repo'"
done
for name in "$repo" $names; do
    tool="$work/unpacked/bin/$name"
    "$tool" --help > /dev/null || die "$name --help failed"
    reported="$("$tool" --version)" || die "$name --version failed"
    [ "$reported" = "$name ($crate) $version" ] \
        || die "$name --version says '$reported', expected '$name ($crate) $version'"
done

printf '%s\n' "$tarball"
