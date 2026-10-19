# fs.ntfs import publishes a file only when complete, and ntfs-3g's own
# tools (libntfs-3g, run at arm's length) read back what it published: the
# bytes through ntfscat, the directory through ntfsls, so no staging name is
# left behind. Windows' chkdsk stays the authority (the Windows job's `cli`
# scenarios); this is the independent reader on the Linux runner.
#
# Missing, the file FAILS naming the package: `apt-get install ntfs-3g`.
source "$(dirname "$0")/../cli/lib.sh"

for tool in ntfscat ntfsls mkfs.ntfs; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        fail "$tool is not on PATH: install ntfs-3g (\`apt-get install ntfs-3g\`; CI's cli job installs it)"
        finish
    fi
done
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}

img="$SANDBOX/import.img"
mkfs.ntfs -q --size 256M --label IMPORT "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"
fs.ntfs "$img" mkdir /d >/dev/null 2>&1
check "mkdir /d exits 0" test $? -eq 0

printf x >"$SANDBOX/one"
head -c 67108864 /dev/urandom >"$SANDBOX/big"
for item in 'one /one' 'big /d/big'; do
    read -r src dest <<<"$item"
    fs.ntfs "$img" import "$SANDBOX/$src" "$dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    status=$?
    check "import $dest exits 0 ($(cat "$SANDBOX/err"))" test "$status" -eq 0
    hash="$(sha256 <"$SANDBOX/$src")"
    jq_check "import $dest reports the source's SHA-256" ".sha256 == \"$hash\" and .replaced == false" "$SANDBOX/out"
    check "ntfscat $dest has the source's SHA-256" test "$(ntfscat "$img" "$dest" 2>/dev/null | sha256)" = "$hash"
done
check "ntfsls / lists d and one, and no staging name" \
    test "$(ntfsls "$img" 2>/dev/null | sort | tr '\n' ' ')" = "d one "
check "ntfsls -p /d lists big alone" test "$(ntfsls -p /d "$img" 2>/dev/null | tr '\n' ' ')" = "big "

printf 'from stdin' | fs.ntfs "$img" import - /stdin.txt --verify >"$SANDBOX/out" 2>"$SANDBOX/err"
status=$?
check "stdin import with --verify exits 0 ($(cat "$SANDBOX/err"))" test "$status" -eq 0
jq_check "--verify is reported" '.verified == true and .bytes == 10' "$SANDBOX/out"
check "ntfscat /stdin.txt has the piped bytes" test "$(ntfscat "$img" /stdin.txt 2>/dev/null)" = "from stdin"

before="$(sha256 <"$img")"
fs.ntfs "$img" import "$SANDBOX/one" /one >"$SANDBOX/out" 2>"$SANDBOX/err"
check "an existing destination is refused" test $? -eq 1
fs.ntfs "$img" import "$SANDBOX/one" /one --replace >"$SANDBOX/out" 2>"$SANDBOX/err"
check "--replace is not implemented: rename cannot replace in one step" test $? -eq 3
fs.ntfs "$img" import "$SANDBOX" /dir >"$SANDBOX/out" 2>"$SANDBOX/err"
check "a directory source is refused" test $? -eq 1
fs.ntfs "$img" import "$img" /self >"$SANDBOX/out" 2>"$SANDBOX/err"
check "the backing image as source is refused" test $? -eq 1
check "refusals leave the image byte-identical" test "$(sha256 <"$img")" = "$before"

small="$SANDBOX/small.img"
mkfs.ntfs -q --size 8M --label FULL "$small" >/dev/null 2>&1
printf keep | fs.ntfs "$small" write /keep >/dev/null 2>&1
fs.ntfs "$small" import "$SANDBOX/big" /big >"$SANDBOX/out" 2>"$SANDBOX/err"
check "an import that runs out of space fails" test $? -eq 1
check "ntfscat /keep is untouched after the failed import" test "$(ntfscat "$small" /keep 2>/dev/null)" = keep
check "ntfsls / lists keep alone: the staging file was removed" \
    test "$(ntfsls "$small" 2>/dev/null | tr '\n' ' ')" = "keep "

finish
