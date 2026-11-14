#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bash "$ROOT/scripts/require-tomllib.sh" "$(basename "$0")"
command -v ruby >/dev/null || { echo "linux-harness-profile: FAIL ruby is required to run the harness run-profile resolver: install ruby on the runner" >&2; exit 1; }
python3 - "$ROOT" <<'PY'
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import yaml

root = Path(sys.argv[1])
harness = root.parent / "fs-linux-test-harness"
config = tomllib.loads((root / "fs-linux-test-harness.toml").read_text())
assert config["vm"] == {"profile": "ntfs3"}, "only the kernel oracle uses this VM config"
profile_dir = root / "run-profiles"
profile = tomllib.loads((profile_dir / "ntfs3.toml").read_text())
assert profile["profile"]["guest"] == "debian-12", "macOS needs Debian 12"
assert profile["profile"]["modules"] == "ntfs3"
prepare = profile_dir / profile["profile"]["setup"]
assert hashlib.sha256(prepare.read_bytes()).hexdigest() == profile["profile"]["setup_sha256"]
for arch, suffix in (("x86_64", "amd64"), ("aarch64", "arm64")):
    assert profile[arch]["release"] == f"6.12.95+deb12-{suffix}"
    resolved = subprocess.run(["ruby", harness / "scripts/run-profile.rb", "resolve", profile_dir / "ntfs3.toml", arch], capture_output=True, text=True)
    assert resolved.returncode == 0 and f"release\t{profile[arch]['release']}" in resolved.stdout, resolved
unsupported_arch = subprocess.run(["ruby", harness / "scripts/run-profile.rb", "resolve", profile_dir / "ntfs3.toml", "ppc64"], capture_output=True, text=True)
assert unsupported_arch.returncode != 0 and "unsupported architecture" in unsupported_arch.stderr, unsupported_arch
assert '"linux-image-$FLTH_KERNEL_RELEASE=6.12.95-1~bpo12+1"' in prepare.read_text()
assert "apt-get" not in (root / "scripts/vm-setup.sh").read_text()
chores = yaml.safe_load((root / "chores.yml").read_text())
assert chores["vars"]["LINUX_HARNESS_REF"] == "v0.7.0"
jobs = yaml.safe_load((root / ".github/workflows/ci.yml").read_text())["jobs"]
for job in ("kernel", "box-cache"):
    assert any(step.get("run") == "../fs-linux-test-harness/scripts/ci-setup-linux.sh"
               for step in jobs[job]["steps"]), f"{job} needs virtiofsd from harness host setup"
cleanup = [step for step in jobs["kernel"]["steps"] if step.get("run") == "chore linux:down"]
assert len(cleanup) == 1 and "always()" in cleanup[0]["if"]
assert not any("! chore linux:status" in step.get("run", "") for step in jobs["kernel"]["steps"])

with tempfile.TemporaryDirectory() as temporary:
    bin_dir = Path(temporary)
    stubs = {
        "uname": 'printf "%s\\n" "$BOOT_RELEASE"',
        "modinfo": '[ "$1" = -k ] && [ "$2" = "$BOOT_RELEASE" ] && [ "$3" = ntfs3 ] && [ "$BOOT_RELEASE" = "$INSTALLED_RELEASE" ]',
        "modprobe": '[ "$1" = ntfs3 ] && [ "${LOAD_FAIL:-0}" = 0 ]',
    }
    for name, body in stubs.items():
        target = bin_dir / name
        target.write_text("#!/bin/sh\n" + body + "\n")
        target.chmod(0o755)
    environment = {key: value for key, value in os.environ.items() if not key.startswith("FLTH_")}
    override = subprocess.run([harness / "scripts/vm.sh", "config"], cwd=bin_dir,
                              env=dict(environment, FLTH_CONFIG=str(root / "fs-linux-test-harness.toml"), FLTH_PROFILE="ntfs3", FLTH_PROFILE_DIR=str(profile_dir)), capture_output=True, text=True)
    assert override.returncode == 0 and f"root={root}\n" in override.stdout and "vm.profile=ntfs3\n" in override.stdout, override
    missing = subprocess.run([harness / "scripts/vm.sh", "config"], cwd=root,
                             env=dict(environment, FLTH_PROFILE="missing-profile"), capture_output=True, text=True)
    assert missing.returncode != 0 and "missing-profile.toml" in missing.stderr, missing
    environment["PATH"] = f"{bin_dir}:{os.environ['PATH']}"
    for arch in ("x86_64", "aarch64"):
        release = profile[arch]["release"]
        environment.update(BOOT_RELEASE=release, INSTALLED_RELEASE=release, LOAD_FAIL="0")
        good = subprocess.run(["bash", root / "scripts/vm-setup.sh"], env=environment, capture_output=True, text=True)
        assert good.returncode == 0 and f"running kernel {release} has ntfs3" in good.stdout, good
        environment["BOOT_RELEASE"] = "6.1.0-42-cloud-" + ("arm64" if arch == "aarch64" else "amd64")
        wrong = subprocess.run(["bash", root / "scripts/vm-setup.sh"], env=environment, capture_output=True, text=True)
        assert wrong.returncode != 0 and environment["BOOT_RELEASE"] in wrong.stderr and "ntfs3 run profile" in wrong.stderr, wrong
        verifier = 'source "$1/scripts/lib/profile.sh"; FLTH_PROFILE_ID=test; FLTH_PROFILE_RELEASE="$INSTALLED_RELEASE"; FLTH_PROFILE_MODULES=ntfs3; engine_run() { bash -c "$1"; }; profile_verify_guest'
        refused = subprocess.run(["bash", "-c", verifier, "profile-guard", str(harness)], env=environment, capture_output=True, text=True)
        assert refused.returncode != 0 and f"profile expected kernel {release}, guest runs {environment['BOOT_RELEASE']}" in refused.stderr, refused
        environment.update(BOOT_RELEASE=release, LOAD_FAIL="1")
        unloadable = subprocess.run(["bash", root / "scripts/vm-setup.sh"], env=environment, capture_output=True, text=True)
        assert unloadable.returncode != 0 and release in unloadable.stderr, unloadable
    unsupported = subprocess.run(["bash", prepare], env=dict(environment, FLTH_KERNEL_ARCH="aarch64", FLTH_KERNEL_RELEASE=profile["x86_64"]["release"]), capture_output=True, text=True)
    assert unsupported.returncode != 0 and "unsupported architecture/release" in unsupported.stderr, unsupported
print("linux-harness-profile: pins, host setup, cleanup and booted-kernel guards passed (both architectures)")
PY
