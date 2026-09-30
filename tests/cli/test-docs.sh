# Every installed tool has its man page and its zsh, bash and fish
# completions where the install prefix keeps them -- share/ beside the bin/
# that PATH found the tool in, the layout of the release tarball and of a
# Homebrew prefix alike -- `man -w` finds that page from PATH, each page
# names every subcommand the tool's --help lists, and each --help carries
# an example for every subcommand.
source "$(dirname "$0")/lib.sh"

command -v man >/dev/null 2>&1 || {
    fail "man is not on PATH; the pages are checked through it (apt-get install man-db)"
    finish
}

# canonical FILE: FILE with its directory's symlinks resolved.
canonical() { echo "$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")"; }

for name in $(rust-fs-ntfs generate names) rust-fs-ntfs; do
    path="$(command -v "$name" 2>/dev/null || true)"
    if [ -z "$path" ]; then
        fail "$name is not on PATH"
        continue
    fi
    share="$(cd "$(dirname "$path")/.." && pwd -P)/share"
    case "$name" in mkfs.* | fsck.*) section=8 ;; *) section=1 ;; esac
    page="$share/man/man$section/$name.$section"
    check "$name has a man page at $page" test -s "$page"
    found="$(man -w "$section" "$name" 2>/dev/null || true)"
    check "man -w $section $name finds that page from PATH (got '$found')" \
        test -n "$found" -a "$(canonical "${found:-/nonexistent}")" = "$page"
    verbs="$("$name" --help | awk '/^Commands:/ { on = 1; next } on && /^  [a-z]/ { print $1 } on && !/^  / { on = 0 }')"
    for verb in $verbs; do
        [ "$verb" = help ] && continue
        check "$name's man page mentions $verb" grep -q -- "$verb" "$page"
        # The repository entry point's tool verbs are the tools, whose own
        # --help is checked under their dotted names.
        if [ "$name" = rust-fs-ntfs ]; then continue; fi
        check "$name $verb --help carries an example" grep -q '^Examples:' <<<"$("$name" x "$verb" --help 2>/dev/null)"
    done
    check "$name has a zsh completion" test -s "$share/zsh/site-functions/_$name"
    check "$name has a bash completion" test -s "$share/bash-completion/completions/$name"
    check "$name has a fish completion" test -s "$share/fish/vendor_completions.d/$name.fish"
done
check "rust-fs-ntfs doctor --help carries an example" grep -q '^Examples:' <<<"$(rust-fs-ntfs doctor --help 2>/dev/null)"
check "rust-fs-ntfs doctor has a page of its own" test -s "${share:-/nonexistent}/man/man1/rust-fs-ntfs-doctor.1"

finish
