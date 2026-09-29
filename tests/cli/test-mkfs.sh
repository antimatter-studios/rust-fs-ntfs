# mkfs.ntfs: formats an image it creates, reports it as JSON read back from
# the volume, and fails with a structured error on stderr and nothing on
# stdout.
source "$(dirname "$0")/lib.sh"

img="$SANDBOX/cli.img"
mkfs.ntfs --size 64M --label CLITEST "$img" >"$SANDBOX/mkfs.json" 2>"$SANDBOX/mkfs.err"
check "mkfs.ntfs --size 64M --label CLITEST exits 0 ($(cat "$SANDBOX/mkfs.err"))" test $? -eq 0
jq_check "the report is ntfs and formatted" '.fs == "ntfs" and .formatted == true and .dry_run == false' "$SANDBOX/mkfs.json"
jq_check "the report carries the label" '.label == "CLITEST"' "$SANDBOX/mkfs.json"
jq_check "the report's sizes are numbers" \
    '[.block_size, .total_bytes, .total_clusters, .mft_record_size, .device_bytes] | all(type == "number")' \
    "$SANDBOX/mkfs.json"
# NTFS keeps the last sector for the backup boot sector, so the volume is
# one sector short of the device.
jq_check "the image is the size asked for, the volume one sector less" \
    '.device_bytes == 67108864 and .total_bytes == 67108352' "$SANDBOX/mkfs.json"
jq_check "the defaults are 4 KiB clusters and 4 KiB records" '.block_size == 4096 and .mft_record_size == 4096' "$SANDBOX/mkfs.json"
jq_check "the serial is 16 hex digits" '.serial_number | test("^[0-9a-f]{16}$")' "$SANDBOX/mkfs.json"
check "the image exists at 64 MiB" test "$(wc -c <"$img" | tr -d ' ')" = 67108864

# The flags the formatter always took reach the volume.
mkfs.ntfs -q -c 1024 --serial deadbeefcafe1234 -L 'Backup Volume' "$img" >"$SANDBOX/flags.json" 2>/dev/null
check "mkfs.ntfs with -c, --serial and -L exits 0" test $? -eq 0
jq_check "-c, --serial and a label with a space reach the volume" \
    '.block_size == 1024 and .serial_number == "deadbeefcafe1234" and .label == "Backup Volume"' "$SANDBOX/flags.json"

# The repository-named form is the same program.
rust-fs-ntfs mkfs -n "$img" >"$SANDBOX/repo.json" 2>/dev/null
check "rust-fs-ntfs mkfs -n exits 0" test $? -eq 0
jq_check "rust-fs-ntfs mkfs -n reports a dry run" '.dry_run == true and .formatted == false' "$SANDBOX/repo.json"

# A dry run with --size creates nothing.
mkfs.ntfs --dry-run --size 64M "$SANDBOX/never.img" >"$SANDBOX/dry.json" 2>/dev/null
check "a dry run with --size exits 0" test $? -eq 0
check "a dry run with --size leaves the target uncreated" test ! -e "$SANDBOX/never.img"
jq_check "a dry run reports the size it would have made" '.device_bytes == 67108864 and .formatted == false' "$SANDBOX/dry.json"

# --text: nothing on stdout, as the tool always printed.
mkfs.ntfs --text -q "$img" >"$SANDBOX/text.out" 2>"$SANDBOX/text.err"
check "mkfs.ntfs --text -q exits 0" test $? -eq 0
check "mkfs.ntfs --text -q prints nothing on stdout" test ! -s "$SANDBOX/text.out"
check "mkfs.ntfs --text -q prints nothing on stderr" test ! -s "$SANDBOX/text.err"

# A wrong command line: status 2, a JSON error on stderr, empty stdout.
mkfs.ntfs --no-such-flag "$img" >"$SANDBOX/usage.out" 2>"$SANDBOX/usage.err"
check "an unknown flag exits 2" test $? -eq 2
check "an unknown flag prints nothing on stdout" test ! -s "$SANDBOX/usage.out"
jq_check "an unknown flag is a structured error" '.code == 2 and (.error | test("no-such-flag"))' "$SANDBOX/usage.err"
mkfs.ntfs -L "$(printf 'x%.0s' $(seq 33))" "$img" >"$SANDBOX/label.out" 2>"$SANDBOX/label.err"
check "a 33-unit label exits 2" test $? -eq 2
jq_check "a 33-unit label names the 32-unit limit" '.code == 2 and (.error | test("limit is 32"))' "$SANDBOX/label.err"
mkfs.ntfs --mft-record-size 1024 --size 64M "$SANDBOX/small.img" >/dev/null 2>"$SANDBOX/rec.err"
check "a 1024-byte MFT record exits 2" test $? -eq 2
check "a refused record size creates nothing" test ! -e "$SANDBOX/small.img"

# A failed run: status 1, the same shape.
mkfs.ntfs "$SANDBOX/absent.img" >"$SANDBOX/fail.out" 2>"$SANDBOX/fail.err"
check "a missing target exits 1" test $? -eq 1
check "a missing target prints nothing on stdout" test ! -s "$SANDBOX/fail.out"
jq_check "a missing target is a structured error" '.code == 1 and (.error | type) == "string"' "$SANDBOX/fail.err"

finish
