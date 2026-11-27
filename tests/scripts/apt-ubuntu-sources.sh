#!/usr/bin/env bash
# An `apt-get update` reads Ubuntu's own source list and no other (#1427).
#
# A hosted runner image carries third-party lists. When packages.microsoft.com
# answered 403, a plain `apt-get update` exited 100 and failed jobs that install
# only Ubuntu packages. So scripts/apt-ubuntu.sh is the one way a job updates,
# and scripts/install-user-tools.sh takes the same options from it.
#
# A step that really needs a third-party source says which one and why, in the
# comment block directly above its update:
#   # apt-third-party-source: <source> -- <why>
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
failed=0
fail() { echo "FAIL: $*" >&2; failed=1; }

# 1. No file a workflow runs updates apt by any other way.
if ! python3 - "$ROOT" <<'PY'
import pathlib
import re
import sys

# The two files that may run an update; sections 2 and 3 run both and read the
# options apt-get was given.
VERIFIED = {"scripts/apt-ubuntu.sh", "scripts/install-user-tools.sh"}
APT = re.compile(r"(?<![\w.-])apt(?:-get)?\s(?:.*\s)?update(?![\w-])")
MARKER = re.compile(r"#\s*apt-third-party-source:\s*\S.*?\s--\s+\S")


def unlisted(text):
    """Line numbers of updates that carry no third-party marker."""
    found, comments, pending, start = [], [], "", 0
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not pending and line.startswith("#"):
            comments.append(line)
            continue
        start = start or number
        if line.endswith("\\"):
            pending += line[:-1] + " "
            continue
        command = re.sub(r"(^|\s)#.*$", "", pending + line)
        if any(APT.search(part) for part in re.split(r"&&|\|\||;|\|", command)):
            if not any(MARKER.search(c) for c in comments):
                found.append(start)
        # A YAML `run: |` line sits between a step's comment and its command.
        if not line.endswith(("run: |", "run: >")):
            comments = []
        pending, start = "", 0
    return found


# The guard guards itself: these must be seen, and these must not.
for bad in (
    "run: sudo apt-get update && sudo apt-get install -y build-essential",
    "run: |\n  sudo apt-get update\n  sudo apt-get install -y ntfs-3g",
    "run: command -v ruby || { sudo apt-get update -qq && sudo apt-get install -y ruby; }",
    "sudo apt update",
    "sudo apt-get -o Acquire::Retries=3 \\\n  update",
    "# apt-third-party-source: example.org\nsudo apt-get update",
    "# apt-third-party-source: example.org -- why\ntrue\nsudo apt-get update",
):
    assert unlisted(bad), f"the guard misses: {bad!r}"
for good in (
    "run: bash scripts/apt-ubuntu.sh ntfs-3g",
    "# apt-get update would read every list",
    "echo 'install it: apt-get install jq'",
    "# apt-third-party-source: example.org -- the only build of the tool\nsudo apt-get update",
    "# apt-third-party-source: example.org -- the only build\nrun: |\n  sudo apt-get update",
):
    assert not unlisted(good), f"the guard wrongly refuses: {good!r}"

root = pathlib.Path(sys.argv[1])
files = [root / "chores.yml"]
for top, patterns in ((".github", ("*.yml", "*.yaml", "*.sh")), ("scripts", ("*.sh",)), ("test-disks", ("*.sh",))):
    for pattern in patterns:
        files += (root / top).rglob(pattern)
assert len(files) > 10, f"only {len(files)} files examined"
bad = 0
for path in sorted(files):
    name = path.relative_to(root).as_posix()
    if name in VERIFIED:
        continue
    for number in unlisted(path.read_text(errors="replace")):
        print(f"FAIL: {name}:{number}: apt update reads every source list; run `bash scripts/apt-ubuntu.sh PACKAGE...`", file=sys.stderr)
        bad += 1
sys.exit(1 if bad else 0)
PY
then
    failed=1
fi

# Stand-ins: apt-get records its arguments and the source list it was given.
mkdir -p "$WORK/stubs"
cat > "$WORK/stubs/apt-get" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$APT_LOG"
for arg in "$@"; do
    case "$arg" in Dir::Etc::sourcelist=*) cat "${arg#*=}" > "$APT_LOG.sources" ;; esac
done
STUB
cat > "$WORK/stubs/sudo" <<'STUB'
#!/usr/bin/env bash
exec "$@"
STUB
# install-user-tools.sh reaches apt on Linux, for a python3 with no yaml.
cat > "$WORK/stubs/uname" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in -m) echo x86_64 ;; *) echo Linux ;; esac
STUB
printf '#!/usr/bin/env bash\nexit 1\n' > "$WORK/stubs/python3"
chmod +x "$WORK/stubs/"*
printf 'Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: noble\n' > "$WORK/ubuntu.sources"

# restricted LOG: every apt-get call in LOG reads one source list and no parts.
restricted() {
    local line
    grep -qE '(^| )update$' "$1" || { fail "$2: no apt-get update ran"; return; }
    while IFS= read -r line; do
        case "$line" in
            *"Dir::Etc::sourcelist="*"Dir::Etc::sourceparts=- "*) ;;
            *) fail "$2: apt-get reads every source list: $line" ;;
        esac
    done < "$1"
}

# 2. The shared script, on a hosted and on a self-hosted runner.
for environment in github-hosted self-hosted; do
    log="$WORK/$environment.log"
    : > "$log"
    if ! APT_LOG="$log" APT_UBUNTU_SOURCES="$WORK/ubuntu.sources" RUNNER_ENVIRONMENT="$environment" \
        PATH="$WORK/stubs:$PATH" bash "$ROOT/scripts/apt-ubuntu.sh" ntfs-3g attr; then
        fail "$environment: scripts/apt-ubuntu.sh failed"
        continue
    fi
    restricted "$log" "$environment"
    grep -q 'APT::Update::Error-Mode=any.* update$' "$log" || fail "$environment: a failed fetch does not fail the update"
    grep -q ' install -y ntfs-3g attr$' "$log" || fail "$environment: the packages were not installed"
    scheme=http
    [ "$environment" = self-hosted ] && scheme=https
    grep -qx "URIs: $scheme://archive.ubuntu.com/ubuntu/" "$log.sources" ||
        fail "$environment: the source list does not use $scheme"
done

# A runner with no Ubuntu source list fails by name and runs no apt-get.
log="$WORK/missing.log"
: > "$log"
if APT_LOG="$log" APT_UBUNTU_SOURCES="$WORK/absent.sources" PATH="$WORK/stubs:$PATH" \
    bash "$ROOT/scripts/apt-ubuntu.sh" ntfs-3g 2> "$WORK/missing.err"; then
    fail "a missing Ubuntu source list was accepted"
fi
grep -q 'absent.sources is missing' "$WORK/missing.err" || fail "the missing source list is not named"
[ ! -s "$log" ] || fail "apt-get ran with no Ubuntu source list"

# 3. The user-tools route, which updates without root.
log="$WORK/user-tools.log"
: > "$log"
status=0
APT_LOG="$log" APT_UBUNTU_SOURCES="$WORK/ubuntu.sources" USER_TOOLS_DIR="$WORK/user-tools" \
    GITHUB_PATH="$WORK/github-path" GITHUB_ENV="$WORK/github-env" PATH="$WORK/stubs:$PATH" \
    bash "$ROOT/scripts/install-user-tools.sh" pyyaml > "$WORK/user-tools.out" 2>&1 || status=$?
restricted "$log" user-tools
# user_apt reads apt's answer with mapfile, which bash 3.2 (macOS) lacks.
if bash -c 'type mapfile' > /dev/null 2>&1 && [ "$status" -ne 0 ]; then
    fail "install-user-tools.sh exited $status: $(tail -n 3 "$WORK/user-tools.out")"
fi

[ "$failed" -eq 0 ] || exit 1
echo "PASS: every apt update reads only Ubuntu's source list"
