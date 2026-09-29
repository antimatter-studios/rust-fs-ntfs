#!/usr/bin/env bash
# package-cli.sh <version> <label> [binary]
#
# Package the built formatter as a release tarball in the current directory,
# check it, and print its file name on stdout.
#
#   <version>  the release version, without the leading `v`
#   <label>    the platform, e.g. darwin-arm64 or linux-x86_64
#   [binary]   the built formatter (default: target/release/mkfs_ntfs)
#
# THE BINARY IS RENAMED HERE. Cargo refuses a dot in a target name, so the
# target is `mkfs_ntfs` and the published binary is `mkfs.ntfs`, the name
# the tooling convention uses and the one `mkfs -t ntfs` resolves to. The
# underscore is a build-system constraint and has no business in a public
# artifact, so the rename happens before packaging rather than in a package
# manager, where a direct downloader would never see it.
#
# The tarball is `<crate>-<version>-<label>.tar.gz` and holds exactly
# `mkfs.ntfs` and the two licence files, with no leading directory for a
# package manager to strip. `rust-ntfs`, the CLI the test matrix drives, is
# not shipped.
#
# THEN IT CHECKS WHAT IT BUILT, because a tarball whose binary does not run
# is worse than no tarball: the failure would surface as a user's bug report
# rather than a red release. The archive's member list must be exactly the
# intended one, and the unpacked binary must answer --help and report
# <version> from --version, which also catches a tag that disagrees with
# Cargo.toml. On any failure nothing is printed on stdout and the tarball is
# removed. tests/scripts/package-cli.sh holds this script to all of that.
set -euo pipefail

version="${1:-}"
label="${2:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${3:-$root/target/release/mkfs_ntfs}"

die() { echo "package-cli: $*" >&2; exit 1; }

[ -n "$version" ] || die "usage: package-cli.sh <version> <label> [binary]"
[ -n "$label" ] || die "usage: package-cli.sh <version> <label> [binary]"
[ -x "$binary" ] || die "no built formatter at $binary (cargo build --release --locked --bin mkfs_ntfs)"

crate="$(sed -n 's/^name = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
[ -n "$crate" ] || die "no package name in $root/Cargo.toml"

tool="mkfs.ntfs"
licences=(LICENSE-APACHE LICENSE-MIT)
tarball="$crate-$version-$label.tar.gz"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
trap 'rm -f "$tarball"' ERR

mkdir "$work/stage" "$work/unpacked"
cp "$binary" "$work/stage/$tool"
chmod 755 "$work/stage/$tool"
for f in "${licences[@]}"; do
    cp "$root/$f" "$work/stage/$f"
done
# COPYFILE_DISABLE keeps macOS tar from adding ._ AppleDouble members.
COPYFILE_DISABLE=1 tar -czf "$tarball" -C "$work/stage" "$tool" "${licences[@]}"

fail() { rm -f "$tarball"; die "$*"; }

want="$(printf '%s\n' "$tool" "${licences[@]}" | sort)"
got="$(tar -tzf "$tarball" | sort)"
[ "$got" = "$want" ] || fail "$tarball holds [$(echo $got)], expected [$(echo $want)]"

tar -xzf "$tarball" -C "$work/unpacked"
[ -x "$work/unpacked/$tool" ] || fail "$tool is not executable in $tarball"
"$work/unpacked/$tool" --help > /dev/null || fail "$tool --help failed"
reported="$("$work/unpacked/$tool" --version)" || fail "$tool --version failed"
[ "$reported" = "$tool ($crate) $version" ] \
    || fail "$tool --version says '$reported', expected '$tool ($crate) $version'"

printf '%s\n' "$tarball"
