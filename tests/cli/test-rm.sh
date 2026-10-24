# fs.ntfs rm and rmdir, as installed: a directory filled past one index
# level is emptied in shuffled order, so entries of interior index nodes go
# as well as leaf ones, and then removed; the refusals each verb owes; and
# a clean fsck.ntfs afterwards.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/lib-ntfs.sh"

img="$SANDBOX/rm.img"
mkfs.ntfs -q --text --size 64M --label CLIRM "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"
fs.ntfs "$img" mkdir /d >/dev/null 2>&1
check "mkdir /d exits 0" test $? -eq 0

# 300 names of varying length: past one index level in 4 KiB blocks.
names="$SANDBOX/names"
: >"$names"
for i in $(seq 1 300); do
    printf 'name-%03d-%s\n' "$i" "$(printf '%*s' $((i % 23)) '' | tr ' ' x)" >>"$names"
done
while read -r n; do
    printf '%s' "$n" | fs.ntfs "$img" write "/d/$n" >/dev/null 2>"$SANDBOX/w.err" ||
        check "write /d/$n exits 0 ($(cat "$SANDBOX/w.err"))" false
done <"$names"
fs.ntfs "$img" ls /d >"$SANDBOX/full.json" 2>/dev/null
jq_check "ls /d lists the 300 files written" 'length == 300' "$SANDBOX/full.json"

# The refusals.
fs.ntfs "$img" rm /d >"$SANDBOX/x.out" 2>"$SANDBOX/x.err"
check "rm of a directory exits 1" test $? -eq 1
check "rm of a directory prints nothing on stdout" test ! -s "$SANDBOX/x.out"
jq_check "rm of a directory points at rmdir" '.code == 1 and (.error | test("rmdir"))' "$SANDBOX/x.err"
# A refusal the volume's state gives is exit 4, `refused`, in the CLI
# contract (EXIT_REFUSED): a directory that is not empty, a missing name.
cp "$img" "$SANDBOX/before.img"
fs.ntfs "$img" rmdir /d >"$SANDBOX/x.out" 2>"$SANDBOX/x.err"
check "rmdir of a directory with files in it exits 4" test $? -eq 4
check "rmdir of a directory with files in it prints nothing on stdout" test ! -s "$SANDBOX/x.out"
jq_check "rmdir of a directory with files in it says so" '.code == 4 and (.error | test("not empty"))' "$SANDBOX/x.err"
check "rmdir of a directory with files in it left the image byte-identical" cmp -s "$img" "$SANDBOX/before.img"
fs.ntfs "$img" rm /d/missing >/dev/null 2>"$SANDBOX/x.err"
check "rm of a missing path exits 4" test $? -eq 4
jq_check "rm of a missing path says so" '.code == 4 and (.error | test("not found"))' "$SANDBOX/x.err"

# Every file, in an order fixed by a seed and by nothing about the index.
awk 'BEGIN { srand(495) } { print rand() "\t" $0 }' "$names" | sort | cut -f2 >"$SANDBOX/order"
removed=0
while read -r n; do
    fs.ntfs "$img" rm "/d/$n" >"$SANDBOX/rm.json" 2>"$SANDBOX/rm.err"
    status=$?
    if [ "$status" -ne 0 ]; then
        check "rm /d/$n exits 0 ($(cat "$SANDBOX/rm.err"))" false
        break
    fi
    removed=$((removed + 1))
done <"$SANDBOX/order"
check "rm removed all 300 files (removed $removed)" test "$removed" -eq 300
jq_check "rm reports the path it removed" '.path | startswith("/d/")' "$SANDBOX/rm.json"
fs.ntfs "$img" ls /d >"$SANDBOX/empty.json" 2>/dev/null
jq_check "ls /d is empty" '. == []' "$SANDBOX/empty.json"

fs.ntfs "$img" rmdir /d >"$SANDBOX/rmdir.json" 2>"$SANDBOX/rmdir.err"
status=$?
check "rmdir of the emptied /d exits 0 ($(cat "$SANDBOX/rmdir.err"))" test "$status" -eq 0
jq_check "rmdir reports the path" '.path == "/d"' "$SANDBOX/rmdir.json"
fs.ntfs "$img" ls / >"$SANDBOX/root.json" 2>/dev/null
jq_check "ls / no longer lists d" 'all(.[]; .name != "d")' "$SANDBOX/root.json"

fsck.ntfs "$img" >"$SANDBOX/fsck.json" 2>/dev/null
check "fsck.ntfs finds the volume clean afterwards" test $? -eq 0

finish
