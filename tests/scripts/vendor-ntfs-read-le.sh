#!/usr/bin/env bash
# The vendored ntfs reader calls binrw's endian readers by their trait path,
# never with method syntax (rust-fs-ntfs#1300).
#
# `std::io::Read` is gaining `read_le` on nightly. A call such as
# `r.read_le::<T>()` on a reader is then ambiguous between that method and
# `binrw::BinReaderExt::read_le`, and the compiler warns that it may resolve
# to the standard library's once that is stable. Those warnings pushed the
# ASan tier past its output budget. `BinReaderExt::read_le::<T>(r)` names
# the method it means and stays correct whatever the standard library adds.
#
# The guard is checked against sandboxed sources first, so it is known to
# fail on the pattern it exists to catch before it is trusted to pass the
# real tree.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
fails=0

# Method-syntax calls of binrw's BinReaderExt endian readers, one per line,
# as path:line:text. A trait-path call has `::` before the name, not `.`.
method_calls() {
    grep -rnE --include='*.rs' '\.[[:space:]]*read_(le|be|ne)(_args)?[[:space:]]*(::|\()' "$1"
}

check() {
    local name="$1" want="$2" text="$3"
    rm -rf "$scratch/src"
    mkdir -p "$scratch/src"
    printf '%s\n' "$text" > "$scratch/src/example.rs"
    if method_calls "$scratch/src" > /dev/null; then got=found; else got=clean; fi
    if [ "$got" != "$want" ]; then
        printf 'FAIL %s: expected %s, got %s\n' "$name" "$want" "$got"
        fails=$((fails + 1))
    fi
}

check method-turbofish found '        let header = r.read_le::<FileNameHeader>()?;'
check method-plain found '        let header: FileNameHeader = r.read_le()?;'
check method-big-endian found '        let x = r.read_be::<u32>()?;'
check method-native-args found '        let x = r.read_ne_args::<T>(args)?;'
check method-next-line found '        let x = r
            .read_le::<u32>()?;'
check trait-path clean '        let header = BinReaderExt::read_le::<FileNameHeader>(r)?;'
check full-trait-path clean '        let x = binrw::BinReaderExt::read_le::<u32>(r)?;'
check unrelated-name clean '        let x = r.read_length()?;'

target="$ROOT/vendor/ntfs/src"
if [ ! -d "$target" ]; then
    printf 'FAIL vendored reader: %s is missing\n' "$target"
    fails=$((fails + 1))
elif found="$(method_calls "$target")"; then
    printf 'FAIL vendored reader calls binrw endian readers with method syntax:\n%s\n' \
        "${found//"$ROOT/"/}"
    fails=$((fails + 1))
fi

[ "$fails" -eq 0 ] || exit 1
echo "vendor-ntfs-read-le.sh: all checks passed"
