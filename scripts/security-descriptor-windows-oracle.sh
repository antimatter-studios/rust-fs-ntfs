#!/usr/bin/env bash
# One executable contract for dedicated and required full Windows CI.
# The native test writes its evidence before asserting Windows' verdicts.
# Callers always upload the bounded tier log and descriptor captures.
set -euo pipefail

: "${NTFS_SD_HEAD:?set NTFS_SD_HEAD to the expected PR head or full-run SHA}"
export CARGO_BUILD_JOBS=2
export NTFS_SD_CAPTURE=security-descriptor-windows.tsv
export NTFS_SD_EVIDENCE=security-descriptor-zero-flag

bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored unit -- cargo test --locked --test security_descriptor_windows_oracle
grep -Eq '^test result: ok\. 4 passed; 0 failed; 0 ignored;' tmp/logs/unit.log
for name in windows_authored_descriptors_match_security_apis_and_are_captured \
  windows_authored_object_aces_match_windows_guids_and_sids \
  windows_accepts_zero_flag_object_ace_through_add_ace \
  windows_built_acl_presence_states_match_both_roles; do
  grep -Eq "^test $name \.\.\. ok$" tmp/logs/unit.log || { echo "not observed: $name"; exit 1; }
done

# Independently bind Windows' native hash, bytes, provenance and rights verdicts.
ev=security-descriptor-zero-flag.tsv
bin=security-descriptor-zero-flag.bin
field() { awk -F'\t' -v k="$1" '$1 == k { print $2 }' "$ev"; }
hash="$(field descriptor_sha256)"
[[ "$hash" =~ ^[0-9a-f]{64}$ ]] || { echo "no SHA-256 recorded"; exit 1; }
[ "$(sha256sum "$bin" | cut -d' ' -f1)" = "$hash" ] || { echo "Windows SHA-256 disagrees with coreutils"; exit 1; }
[ "$(od -An -v -tx1 "$bin" | tr -d ' \n')" = "$(field descriptor_hex)" ] || { echo "descriptor_hex is not the retained bytes"; exit 1; }
[ "$(stat -c %s "$bin")" = "$(field descriptor_len)" ] || { echo "descriptor_len disagrees"; exit 1; }
case "$(field descriptor_hex)" in *"$(field stored_ace_hex)"*) ;; *) echo "stored ACE absent from descriptor"; exit 1 ;; esac
[ "$(field head)" = "$NTFS_SD_HEAD" ] || { echo "head is not $NTFS_SD_HEAD"; exit 1; }
[ "$(field checkout)" = "$(git rev-parse HEAD)" ] || { echo "checkout is not the built tree"; exit 1; }
[ "$(field access_0x100)" = "granted=0x100 status=true" ] || { echo "allow verdict: $(field access_0x100)"; exit 1; }
[ "$(field access_0x1)" = "granted=0x0 status=false" ] || { echo "deny verdict: $(field access_0x1)"; exit 1; }
[ "$(field access_0x101)" = "granted=0x0 status=false" ] || { echo "deny verdict: $(field access_0x101)"; exit 1; }
# Negative controls use the same hash checker and must both be refused.
echo "$hash  $bin" | sha256sum --quiet -c -
if printf '%064d  %s\n' 0 "$bin" | sha256sum --quiet -c - 2>/dev/null; then echo "wrong hash accepted"; exit 1; fi
cp "$bin" security-descriptor-zero-flag.tampered.bin
last=$(( $(stat -c %s security-descriptor-zero-flag.tampered.bin) - 1 ))
printf '\xff' | dd of=security-descriptor-zero-flag.tampered.bin bs=1 seek="$last" conv=notrunc status=none
cmp -s "$bin" security-descriptor-zero-flag.tampered.bin && { echo "tamper did not change bytes"; exit 1; }
if echo "$hash  security-descriptor-zero-flag.tampered.bin" | sha256sum --quiet -c - 2>/dev/null; then echo "tampered bytes accepted"; exit 1; fi
echo "descriptor evidence verified: $hash"
