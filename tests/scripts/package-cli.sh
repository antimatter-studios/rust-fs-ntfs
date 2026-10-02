#!/usr/bin/env bash
# The release tarball has the layout an installer copies as-is --
# bin/rust-fs-ntfs (the multi-call binary), each dotted name a relative
# symlink to it, a man page and three completions per name under share/,
# share/rust-fs-ntfs/CAVEATS and the licences, nothing else -- and every
# name in it runs and identifies itself.
#
# Cargo refuses a dot in a target name, so the tools build as one binary,
# `rust-fs-ntfs`, that dispatches on the name it was started under.
# scripts/package-cli.sh links each dotted name the binary lists to it; no
# cargo target name other than the repository's reaches a public artifact.
# `rust-ntfs`, the test driver, is not shipped at all.
#
# This runs the real packaging script against stand-in binaries in a
# sandbox: one that behaves, and one for each way a build can be wrong
# (missing, --help failing, reporting a version other than the tag's,
# listing no names). The release workflow and the pull-request `cli` job run
# the same script against the real binary, so the checks here are the
# checks a release makes.
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

# A stand-in for the built multi-call binary. $1 is the version it reports,
# $2 the exit status of --help, $3 where it goes (its file name is the
# binary's), $4 the names `generate names` lists, $5 the names it writes
# docs for (default: all of them). Like the real one, it answers --version
# as the name it was started under, and `generate man|completions SHARE`
# writes a page and three completions per name and prints their paths.
stub() {
    local path="$sandbox/$3"
    mkdir -p "$(dirname "$path")"
    cat > "$path" <<STUB
#!/usr/bin/env bash
name="\$(basename "\$0")"
case "\$1" in
    --help)    echo "Usage: \$name [options]"; exit $2 ;;
    --version) echo "\$name ($crate) $1" ;;
    generate)
        case "\$2" in
            names) printf '%s\n' ${4-mkfs.ntfs} ;;
            man | completions)
                share="\$3"
                echo "["
                for n in ${5-${4-mkfs.ntfs}} rust-fs-ntfs; do
                    case "\$n" in mkfs.* | fsck.*) s=8 ;; *) s=1 ;; esac
                    if [ "\$2" = man ]; then
                        files="\$share/man/man\$s/\$n.\$s"
                    else
                        files="\$share/zsh/site-functions/_\$n \$share/bash-completion/completions/\$n \$share/fish/vendor_completions.d/\$n.fish"
                    fi
                    for f in \$files; do
                        mkdir -p "\$(dirname "\$f")"
                        echo "doc for \$n" >"\$f"
                        echo "  \\"\$f\\","
                    done
                done
                echo "]"
                ;;
        esac
        ;;
    *)         exit 2 ;;
esac
STUB
    chmod +x "$path"
    printf '%s\n' "$path"
}

# Runs the packaging script in a fresh output directory. It prints the
# tarball's name relative to that directory, so this prints the absolute path.
#
# STALE=<name> first leaves a file of that name in the output directory, as a
# previous run would have. On failure it prints whatever the script printed,
# so a refusal that names a tarball anyway is seen, and it records the output
# directory in $sandbox/package-out for the leftover-tarball check.
package() {
    local out="$sandbox/out-$RANDOM$RANDOM" name status
    mkdir -p "$out"
    printf '%s\n' "$out" > "$sandbox/package-out"
    [ -z "${STALE:-}" ] || echo stale > "$out/$STALE"
    name="$(cd "$out" && bash "$PACKAGE" "$@" 2>"$sandbox/stderr")"
    status=$?
    if [ "$status" -ne 0 ]; then
        printf '%s' "$name"
        return "$status"
    fi
    printf '%s\n' "$out/$name"
}

[ -f "$PACKAGE" ] && ok || bad "scripts/package-cli.sh exists"

# --- A good build: the tarball, its name, and exactly its contents. -------
good="$(stub 9.9.9 0 good/rust-fs-ntfs)"
if tarball="$(package 9.9.9 darwin-arm64 "$(dirname "$good")")"; then
    ok
else
    bad "a good build packages: $(cat "$sandbox/stderr")"
    tarball=""
fi

case "$(basename "$tarball")" in
    "$crate-9.9.9-darwin-arm64.tar.gz") ok ;;
    *) bad "tarball is named <crate>-<version>-<label>.tar.gz, got '$tarball'" ;;
esac

# The content checks below need the tarball. Without it they fail rather than
# fall silent, since a check that does not run reads like one that passed.
[ -f "$tarball" ] && ok || bad "the packaged tarball exists at '$tarball'"
if [ -f "$tarball" ]; then
    listing="$(tar -tzf "$tarball" | sort | tr '\n' ' ')"
    # LC_ALL=C because want_files below is written in byte order, the
    # LICENSE files before bin/. A bare `sort` collates by the caller's
    # locale, so the check failed on a correct tarball everywhere but a
    # C-locale CI runner (#406).
    files="$(tar -tzf "$tarball" | sed 's|^\./||' | grep -v '/$' | LC_ALL=C sort | tr '\n' ' ')"
    want_files="LICENSE-APACHE LICENSE-MIT bin/mkfs.ntfs bin/rust-fs-ntfs"
    want_files="$want_files share/bash-completion/completions/mkfs.ntfs share/bash-completion/completions/rust-fs-ntfs"
    want_files="$want_files share/fish/vendor_completions.d/mkfs.ntfs.fish share/fish/vendor_completions.d/rust-fs-ntfs.fish"
    want_files="$want_files share/man/man1/rust-fs-ntfs.1 share/man/man8/mkfs.ntfs.8 share/rust-fs-ntfs/CAVEATS"
    want_files="$want_files share/zsh/site-functions/_mkfs.ntfs share/zsh/site-functions/_rust-fs-ntfs "
    [ "$files" = "$want_files" ] && ok \
        || bad "tarball holds exactly the binary, its links, the man pages, the completions, the CAVEATS and the licences, got: $files"

    case "$listing" in
        *mkfs_ntfs*) bad "the old cargo target name reached the tarball: $listing" ;;
        *) ok ;;
    esac
    case "$listing" in
        *bin/rust-ntfs\ * | *bin/rust-ntfs) bad "the test driver was shipped: $listing" ;;
        *) ok ;;
    esac

    unpacked="$sandbox/unpacked"
    mkdir -p "$unpacked"
    tar -xzf "$tarball" -C "$unpacked"
    [ -x "$unpacked/bin/rust-fs-ntfs" ] && [ ! -L "$unpacked/bin/rust-fs-ntfs" ] && ok \
        || bad "bin/rust-fs-ntfs is an executable file in the tarball"
    [ -L "$unpacked/bin/mkfs.ntfs" ] && [ "$(readlink "$unpacked/bin/mkfs.ntfs")" = rust-fs-ntfs ] && ok \
        || bad "bin/mkfs.ntfs is a relative symlink to rust-fs-ntfs"
    [ "$("$unpacked/bin/mkfs.ntfs" --version)" = "mkfs.ntfs ($crate) 9.9.9" ] && ok \
        || bad "bin/mkfs.ntfs answers as mkfs.ntfs through its link"
    cmp -s "$unpacked/share/rust-fs-ntfs/CAVEATS" "$ROOT/packaging/CAVEATS" && ok \
        || bad "share/rust-fs-ntfs/CAVEATS is packaging/CAVEATS"
    [ "$(wc -l <"$ROOT/packaging/CAVEATS" | tr -d ' ')" -le 4 ] && ok \
        || bad "packaging/CAVEATS is at most four lines"
    cmp -s "$unpacked/LICENSE-MIT" "$ROOT/LICENSE-MIT" && ok || bad "LICENSE-MIT is the repository's"
    cmp -s "$unpacked/LICENSE-APACHE" "$ROOT/LICENSE-APACHE" && ok || bad "LICENSE-APACHE is the repository's"
    cmp -s "$unpacked/bin/rust-fs-ntfs" "$good" && ok || bad "bin/rust-fs-ntfs is the built binary"
fi

# --- Each way a build can be wrong is refused, with no tarball left. ------
refused() {
    local why="$1"; shift
    local stdout out_dir left
    if stdout="$(package "$@")"; then
        bad "$why is refused, but packaging succeeded: $stdout"
    else
        ok
        [ -z "$stdout" ] && ok || bad "$why leaves no tarball named on stdout: $stdout"
        out_dir="$(cat "$sandbox/package-out")"
        left="$(find "$out_dir" -maxdepth 1 -name '*.tar.gz')"
        [ -z "$left" ] && ok || bad "$why leaves no tarball behind, found: $left"
    fi
}

refused "a missing binary" 9.9.9 darwin-arm64 "$sandbox/nowhere"
refused "a binary whose --help fails" 9.9.9 darwin-arm64 "$(dirname "$(stub 9.9.9 1 helpfails/rust-fs-ntfs)")"
refused "a binary reporting a version other than the tag's" 9.9.9 darwin-arm64 "$(dirname "$(stub 1.0.0 0 wrongver/rust-fs-ntfs)")"
refused "a binary that lists no tool names" 9.9.9 darwin-arm64 "$(dirname "$(stub 9.9.9 0 nonames/rust-fs-ntfs "")")"
refused "a binary listing a name that is a path" 9.9.9 darwin-arm64 "$(dirname "$(stub 9.9.9 0 pathname/rust-fs-ntfs "../mkfs.ntfs")")"
refused "a binary that writes no man page for one of its names" 9.9.9 darwin-arm64 "$(dirname "$(stub 9.9.9 0 nodocs/rust-fs-ntfs "mkfs.ntfs fs.ntfs" "mkfs.ntfs")")"
refused "a build of the old mkfs_ntfs target only" 9.9.9 darwin-arm64 "$(dirname "$(stub 9.9.9 0 old/mkfs_ntfs)")"
refused "a missing label" 9.9.9 "" "$(dirname "$good")"
refused "a missing version" "" darwin-arm64 "$(dirname "$good")"
STALE="$crate-9.9.9-darwin-arm64.tar.gz" refused "a failure beside a previous run's tarball" 9.9.9 darwin-arm64 "$sandbox/nowhere"
# A working directory that cannot be made is a failure too, and it is the
# first thing the script can fail on after naming the tarball. An mktemp that
# fails stands in for an unusable TMPDIR: macOS mktemp falls back to the
# per-user temporary directory when TMPDIR is missing or read-only, so TMPDIR
# alone cannot make it fail there.
mkdir -p "$sandbox/failing-mktemp"
printf '#!/bin/sh\necho "mktemp: cannot create directory" >&2\nexit 1\n' > "$sandbox/failing-mktemp/mktemp"
chmod +x "$sandbox/failing-mktemp/mktemp"
STALE="$crate-9.9.9-darwin-arm64.tar.gz" PATH="$sandbox/failing-mktemp:$PATH" \
    refused "a working directory that cannot be made, beside a previous run's tarball" 9.9.9 darwin-arm64 "$(dirname "$good")"

# --- The release workflow packages through this script. ------------------
release="$ROOT/.github/workflows/release.yml"
grep -q 'scripts/package-cli.sh' "$release" && ok \
    || bad "release.yml packages through scripts/package-cli.sh"
grep -q 'cargo build --release --locked --features cli --bin rust-fs-ntfs' "$release" && ok \
    || bad "release.yml builds the rust-fs-ntfs target with the cli feature"
ci="$ROOT/.github/workflows/ci.yml"
grep -q 'scripts/package-cli.sh' "$ci" && ok \
    || bad "ci.yml packages the tarball on pull requests, so a release is not its first build"
grep -qE 'uses: actions/attest-build-provenance@[0-9a-f]{40}' "$release" && ok \
    || bad "release.yml attests the tarballs' build provenance, with the action pinned to a commit"
grep -q 'attestations: write' "$release" && ok \
    || bad "release.yml grants the release job attestations: write"

printf 'package-cli: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
