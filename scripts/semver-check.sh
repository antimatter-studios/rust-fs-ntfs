#!/usr/bin/env bash
# semver-check.sh — refuse a public-API break the version does not declare.
#
# Compares this crate's public API against the newest version PUBLISHED TO
# CRATES.IO — not a tag, not origin/main — with cargo-semver-checks, and fails
# when the change needs a bigger bump than Cargo.toml's version makes. For a
# 0.x crate a break needs the minor to move (0.6.0 -> 0.7.0); an addition
# needs at least the patch.
#
# WHY THIS EXISTS (#399). `fsck`'s signature and the Rust API's error type
# both changed after 0.6.0 published, and Cargo.toml stayed on 0.6.0 with the
# breaks written under [Unreleased]: a release cut from main would have
# published them as a patch, and every `^0.6` consumer's build would have
# broken on `cargo update`. rust-fs-ext4 met the same shape first and added
# this gate (christhomas/rust-fs-ext4#120); a step a person has to remember is
# the step that goes missing, so this one runs on every pull request.
#
# THE BASELINE IS THE REGISTRY because that is what a consumer has. A tag can
# exist for a version that never published, and a version can publish from a
# commit no tag names; the registry is the one list a downstream `cargo update`
# actually reads.
#
# WHAT IT CANNOT SEE. A change of behaviour behind an unchanged signature, and
# anything in the C ABI: cargo-semver-checks reads Rust items, not a C
# struct's layout or an exported function's arity as a C caller sees it. Those
# still need a changelog line written by a person.
#
# cargo-semver-checks is MIT/Apache-2.0. CARGO_SEMVER_CHECKS_VERSION pins the
# version CI installs; a different local one is reported, not refused.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PINNED="${CARGO_SEMVER_CHECKS_VERSION:-0.50.0}"

if ! cargo semver-checks --version >/dev/null 2>&1; then
  echo "semver-check: cargo-semver-checks is not installed." >&2
  echo "              cargo install cargo-semver-checks --locked --version $PINNED" >&2
  exit 1
fi
have="$(cargo semver-checks --version | awk '{print $2}')"
if [ "$have" != "$PINNED" ]; then
  echo "semver-check: note: cargo-semver-checks $have here, CI pins $PINNED" >&2
fi

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
echo "semver-check: am-fs-ntfs $version against the newest crates.io release"

# --release-type is NOT passed: the bump is read from Cargo.toml, so the
# version a release would publish is the version being checked.
exec cargo semver-checks check-release --package am-fs-ntfs
