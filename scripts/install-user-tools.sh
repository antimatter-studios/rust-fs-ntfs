#!/usr/bin/env bash
# install-user-tools.sh TOOL...
#
# Puts the tools a CI job needs on PATH without root, so one workflow runs on
# GitHub's hosted runners and on the antimatter-vps ARC runners, whose pods
# cannot sudo (allowPrivilegeEscalation: false). A tool already on PATH is left
# alone: hosted runners, which preinstall most of these, skip straight past.
#
# Tools:
#   chore    the release binary
#   pwsh     PowerShell's release tarball
#   ntfs-3g  Ubuntu's packages, resolved by apt and unpacked with dpkg-deb
#   pyyaml   the python3 yaml module rust-fs-core's ci-gate imports, the same
#            way (python3-yaml, reached through PYTHONPATH)
#   cc       zig cc, through cargo-zigbuild's wrapper, which rewrites the
#            linker arguments rustc passes that zig does not take
#
# Everything lands in USER_TOOLS_DIR (default ~/.local/user-tools), which
# .github/actions/user-tools caches. Run again over a restored directory, the
# script downloads nothing and only puts what is there back on PATH. Later
# steps get PATH and LD_LIBRARY_PATH through $GITHUB_PATH and $GITHUB_ENV.
#
# Every download is pinned to a version and checked against its SHA-256.
set -euo pipefail

CHORE_VERSION=0.11.0
PWSH_VERSION=7.6.9
ZIG_VERSION=0.17.0
CARGO_ZIGBUILD_VERSION=0.24.0

root="${USER_TOOLS_DIR:-$HOME/.local/user-tools}"
bin="$root/bin"
apt_tree="$root/apt/tree"
mkdir -p "$bin"

os=$(uname -s)
case "$(uname -m)" in
    x86_64)        arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) echo "install-user-tools: unsupported machine $(uname -m)" >&2; exit 1 ;;
esac

sha256_of() {
    case "$os-$arch-$1" in
        Linux-x86_64-chore)          echo 0616bf7f7e21f1b3b951161c35c56da17eb59fcb7e59e90f8aa8ef32f1f2ff2c ;;
        Linux-aarch64-chore)         echo 77c47ed2171ddc7542eb0f0c0dbf550ab41dfe370cebc3eb6900b18cb6552d3d ;;
        Darwin-x86_64-chore)         echo bd78231e780209566a24794cb131de5e9d4c2d23a256bce6e6f1ba53d67e7bea ;;
        Darwin-aarch64-chore)        echo f15057391789b0e1946da9b6f08ffd1b2a98eda9ec753e4a11bac564f55ef2e4 ;;
        Linux-x86_64-pwsh)           echo 1da6d6d6035d651f20e70ddee7c4c15ac6ab8b9fb40ee92240d91263f28a884b ;;
        Linux-aarch64-pwsh)          echo 02eb9991af447c7e7fe881acde195961afd397c40b8eb1c2b3856b84ec2d85f2 ;;
        Linux-x86_64-zig)            echo 1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026 ;;
        Linux-aarch64-zig)           echo 9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8 ;;
        Linux-x86_64-cargo-zigbuild) echo 3ca25ee4348f9abd0932ed4debe826d246e20fb616d0201de5bd05e7f965e9d9 ;;
        Linux-aarch64-cargo-zigbuild) echo de42626985f428033078dfc12ac8a59ee713f8e5627235ec883b1c899bee895c ;;
        *) echo "install-user-tools: no $1 build for $os $arch" >&2; return 1 ;;
    esac
}

added_paths=()
added_library_paths=()
added_python_paths=()

add_path() {
    export PATH="$1:$PATH"
    added_paths+=("$1")
    if [ -n "${GITHUB_PATH:-}" ]; then echo "$1" >> "$GITHUB_PATH"; fi
}

add_library_path() {
    export LD_LIBRARY_PATH="$1${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    added_library_paths+=("$1")
    if [ -n "${GITHUB_ENV:-}" ]; then echo "LD_LIBRARY_PATH=$LD_LIBRARY_PATH" >> "$GITHUB_ENV"; fi
}

add_python_path() {
    export PYTHONPATH="$1${PYTHONPATH:+:$PYTHONPATH}"
    added_python_paths+=("$1")
    if [ -n "${GITHUB_ENV:-}" ]; then echo "PYTHONPATH=$PYTHONPATH" >> "$GITHUB_ENV"; fi
}

have() { PATH="$PATH:/usr/sbin:/sbin" command -v "$1" >/dev/null 2>&1; }

# fetch URL NAME DEST: download to DEST and check it against NAME's pinned hash.
fetch() {
    local want got
    want=$(sha256_of "$2")
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 -o "$3" "$1"
    if have sha256sum; then got=$(sha256sum "$3"); else got=$(shasum -a 256 "$3"); fi
    got=${got%% *}
    if [ "$got" != "$want" ]; then
        echo "install-user-tools: $1 has sha256 $got, expected $want" >&2
        rm -f "$3"
        exit 1
    fi
}

# untar_xz TARBALL DIR: unpack, dropping the top directory. The stock runner
# image has no xz; Python's tarfile reads it.
untar_xz() {
    mkdir -p "$2"
    if have xz; then
        tar xJf "$1" -C "$2" --strip-components=1
        return
    fi
    python3 - "$1" "$2" <<'PY_EOF'
import sys, tarfile
src, dest = sys.argv[1], sys.argv[2]
with tarfile.open(src, "r:xz") as tar:
    members = []
    for m in tar.getmembers():
        parts = m.name.split("/", 1)
        if len(parts) < 2 or not parts[1]:
            continue
        m.name = parts[1]
        if m.islnk():
            m.linkname = m.linkname.split("/", 1)[-1]
        members.append(m)
    tar.extractall(dest, members=members, filter="data")
PY_EOF
}

# What a restored cache holds goes back on PATH before anything is judged missing.
activate_apt_tree() {
    local dir
    if [ -d "$apt_tree" ]; then
        for dir in usr/bin usr/sbin bin sbin; do
            if [ -d "$apt_tree/$dir" ]; then add_path "$apt_tree/$dir"; fi
        done
        for dir in "usr/lib/$arch-linux-gnu" "lib/$arch-linux-gnu" usr/lib lib; do
            if [ -d "$apt_tree/$dir" ]; then add_library_path "$apt_tree/$dir"; fi
        done
        if [ -d "$apt_tree/usr/lib/python3/dist-packages" ]; then
            add_python_path "$apt_tree/usr/lib/python3/dist-packages"
        fi
    fi
}

activate() {
    add_path "$bin"
    activate_apt_tree
}

install_chore() {
    have chore && return
    local target
    case "$os-$arch" in
        Linux-x86_64)   target=linux-x86_64 ;;
        Linux-aarch64)  target=linux-arm64 ;;
        Darwin-x86_64)  target=darwin-x86_64 ;;
        Darwin-aarch64) target=darwin-arm64 ;;
        *) echo "install-user-tools: chore has no build for $os" >&2; exit 1 ;;
    esac
    local tarball="$root/chore.tar.gz"
    fetch "https://github.com/antimatter-studios/chore/releases/download/v$CHORE_VERSION/chore-$CHORE_VERSION-$target.tar.gz" chore "$tarball"
    tar xzf "$tarball" -C "$bin" chore
    rm -f "$tarball"
}

install_pwsh() {
    have pwsh && return
    local target dir="$root/opt/powershell-$PWSH_VERSION" tarball="$root/pwsh.tar.gz"
    case "$arch" in x86_64) target=x64 ;; aarch64) target=arm64 ;; esac
    fetch "https://github.com/PowerShell/PowerShell/releases/download/v$PWSH_VERSION/powershell-$PWSH_VERSION-linux-$target.tar.gz" pwsh "$tarball"
    mkdir -p "$dir"
    tar xzf "$tarball" -C "$dir"
    rm -f "$tarball"
    chmod +x "$dir/pwsh"
    ln -sf "$dir/pwsh" "$bin/pwsh"
}

# apt resolves and downloads as any user once its lists and caches are its own;
# dpkg-deb unpacks without touching dpkg's database. Only what apt reports as
# missing is unpacked, so nothing already installed is shadowed.
user_apt() {
    local state="$root/apt/state"
    mkdir -p "$state/lists/partial" "$state/cache/archives/partial" "$apt_tree"
    local opts=(
        -q=2
        -o "Dir::State::Lists=$state/lists"
        -o "Dir::Cache=$state/cache"
        -o "Debug::NoLocking=1"
        -o "APT::Sandbox::User=$(id -un)"
    )
    apt-get "${opts[@]}" update
    local pkgs
    mapfile -t pkgs < <(apt-get "${opts[@]}" -s install --no-install-recommends "$@" | awk '$1 == "Inst" { print $2 }')
    if [ "${#pkgs[@]}" -gt 0 ]; then
        local debs="$state/debs"
        mkdir -p "$debs"
        (cd "$debs" && apt-get "${opts[@]}" download "${pkgs[@]}")
        local deb
        for deb in "$debs"/*.deb; do dpkg-deb -x "$deb" "$apt_tree"; done
    fi
    # The tree is all a later run needs; keep the cache small.
    rm -rf "$state"
    activate_apt_tree
}

install_ntfs_3g() {
    have ntfsfix && have ntfsls && have ntfscat && return
    if [ "$os" != Linux ]; then echo "install-user-tools: ntfs-3g is installed this way on Linux only" >&2; exit 1; fi
    user_apt ntfs-3g
}

install_pyyaml() {
    python3 -c 'import yaml' >/dev/null 2>&1 && return
    if [ "$os" != Linux ]; then
        echo "install-user-tools: python3 has no yaml module; install PyYAML into $(command -v python3)'s environment" >&2
        exit 1
    fi
    user_apt python3-yaml
}

install_cc() {
    have cc && return
    if [ "$os" != Linux ]; then echo "install-user-tools: no cc and no zig route on $os" >&2; exit 1; fi
    local zig_dir="$root/opt/zig-$ZIG_VERSION" czb_dir="$root/opt/cargo-zigbuild-$CARGO_ZIGBUILD_VERSION"
    local tarball="$root/download.tar.xz"
    fetch "https://ziglang.org/download/$ZIG_VERSION/zig-$arch-linux-$ZIG_VERSION.tar.xz" zig "$tarball"
    untar_xz "$tarball" "$zig_dir"
    fetch "https://github.com/rust-cross/cargo-zigbuild/releases/download/v$CARGO_ZIGBUILD_VERSION/cargo-zigbuild-$arch-unknown-linux-gnu.tar.xz" cargo-zigbuild "$tarball"
    untar_xz "$tarball" "$czb_dir"
    rm -f "$tarball"
    # The zig cache (glibc stubs, compiler-rt) lives in the cached directory too,
    # so only the first link after a version bump builds them.
    cat > "$bin/cc" <<EOF
#!/bin/sh
export CARGO_ZIGBUILD_ZIG_COMMAND="$zig_dir/zig"
export CARGO_ZIGBUILD_ZIG_VERSION=$ZIG_VERSION
export ZIG_GLOBAL_CACHE_DIR="$root/zig-cache"
# cargo-zigbuild asks rustc which of rustc's linker arguments to rewrite. Called
# without rustc around, there are none of those, and any version will do.
command -v rustc >/dev/null 2>&1 || export CARGO_ZIGBUILD_RUSTC_VERSION=1.95.0
exec "$czb_dir/cargo-zigbuild" zig cc -- -target $arch-linux-gnu "\$@"
EOF
    chmod +x "$bin/cc"
}

activate
for tool in "$@"; do
    case "$tool" in
        chore)   install_chore ;;
        pwsh)    install_pwsh ;;
        ntfs-3g) install_ntfs_3g ;;
        pyyaml)  install_pyyaml ;;
        cc)      install_cc ;;
        *) echo "install-user-tools: unknown tool '$tool'" >&2; exit 1 ;;
    esac
done

# Outside CI nothing carries PATH on to the next command, so say what to add.
if [ -z "${GITHUB_PATH:-}" ]; then
    joined() { local IFS=:; echo "$*"; }
    echo "install-user-tools: add to your shell profile:" >&2
    echo "  export PATH=\"$(joined "${added_paths[@]}"):\$PATH\"" >&2
    if [ "${#added_library_paths[@]}" -gt 0 ]; then
        echo "  export LD_LIBRARY_PATH=\"$(joined "${added_library_paths[@]}")\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}\"" >&2
    fi
    if [ "${#added_python_paths[@]}" -gt 0 ]; then
        echo "  export PYTHONPATH=\"$(joined "${added_python_paths[@]}")\${PYTHONPATH:+:\$PYTHONPATH}\"" >&2
    fi
fi
