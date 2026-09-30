# The second, weaker oracle: ntfs-3g's own tools (libntfs-3g, the code
# behind the ntfs-3g mount) read what fs.ntfs wrote. Every file comes back
# byte for byte through ntfscat, the directories list through ntfsls, the
# label reads through ntfslabel, and a volume fs.ntfs marked dirty is one
# ntfs-3g refuses to open until the flag is cleared. Windows' chkdsk is the
# authority (the Windows job's `cli` scenarios); this is an independent
# reader on the Linux runner.
#
# ntfs-3g is GPL and is used here only as a program run at arm's length.
# Missing, the file FAILS naming the package: `apt-get install ntfs-3g`.
source "$(dirname "$0")/../cli/lib.sh"

for tool in ntfscat ntfsls ntfslabel; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        fail "$tool is not on PATH: install ntfs-3g (\`apt-get install ntfs-3g\`; CI's cli job installs it)"
        finish
    fi
done

random() { if [ "$2" -gt 0 ]; then head -c "$2" /dev/urandom >"$1"; else : >"$1"; fi; }

img="$SANDBOX/3g.img"
mkfs.ntfs -q --size 64M --label CLI3G "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"
fs.ntfs "$img" mkdir /d >/dev/null 2>&1 && fs.ntfs "$img" mkdir /d/e >/dev/null 2>&1
check "mkdir /d and /d/e exit 0" test $? -eq 0

for size in 0 1 500 4096 4097 1048576; do
    random "$SANDBOX/src.$size" "$size"
    fs.ntfs "$img" write "/f$size" <"$SANDBOX/src.$size" >/dev/null 2>&1
    check "write /f$size exits 0" test $? -eq 0
done
random "$SANDBOX/deep" 5000
fs.ntfs "$img" write /d/e/deep <"$SANDBOX/deep" >/dev/null 2>&1
# A replacement by shorter content, across the non-resident line.
fs.ntfs "$img" write /f4097 <"$SANDBOX/src.1" >/dev/null 2>&1
cp "$SANDBOX/src.1" "$SANDBOX/src.4097"
fs.ntfs "$img" set label "Backup Volume" >/dev/null 2>&1
check "set label exits 0" test $? -eq 0

for size in 0 1 500 4096 4097 1048576; do
    check "ntfscat /f$size is what fs.ntfs wrote" cmp -s "$SANDBOX/src.$size" <(ntfscat "$img" "/f$size" 2>/dev/null)
done
check "ntfscat /d/e/deep is what fs.ntfs wrote" cmp -s "$SANDBOX/deep" <(ntfscat "$img" /d/e/deep 2>/dev/null)
check "ntfsls / lists d and every file" \
    test "$(ntfsls "$img" 2>/dev/null | sort | tr '\n' ' ')" = "d f0 f1 f1048576 f4096 f4097 f500 "
check "ntfsls -p /d lists e" grep -qx e <(ntfsls -p /d "$img" 2>/dev/null)
check "ntfslabel reads the label fs.ntfs set" test "$(ntfslabel "$img" 2>/dev/null)" = "Backup Volume"

# Dirty: ntfs-3g will not open a volume scheduled for check, and will again
# once fs.ntfs has cleared the flag.
fs.ntfs "$img" set dirty true >/dev/null 2>&1
ntfscat "$img" /f1 >/dev/null 2>"$SANDBOX/dirty.err"
check "ntfs-3g refuses the volume fs.ntfs marked dirty" test $? -ne 0
check "ntfs-3g says it is scheduled for check" grep -q 'scheduled for check' "$SANDBOX/dirty.err"
fs.ntfs "$img" set dirty false >/dev/null 2>&1
check "ntfs-3g opens it again once the flag is cleared" cmp -s "$SANDBOX/src.1" <(ntfscat "$img" /f1 2>/dev/null)

finish
