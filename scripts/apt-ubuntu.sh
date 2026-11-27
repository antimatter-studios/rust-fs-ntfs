#!/usr/bin/env bash
# apt-ubuntu.sh PACKAGE...
#
# `apt-get update` and `apt-get install` from Ubuntu's own source list and no
# other, on hosted and self-hosted runners. A runner image carries third-party
# lists too, and one of them refusing a request fails a plain update before any
# project code runs: packages.microsoft.com answered 403 (#1427).
#
# Sourced, it only defines ubuntu_apt_options, for a caller that runs apt-get
# its own way (scripts/install-user-tools.sh, which has no root).
#
# APT_UBUNTU_SOURCES names another source list, for the guard test.

# ubuntu_apt_options DIR: set UBUNTU_APT_OPTIONS to the apt-get options that
# read Ubuntu's list alone, through a copy written into DIR. Fails, naming the
# file, where there is no such list.
ubuntu_apt_options() {
    local source_list="${APT_UBUNTU_SOURCES:-/etc/apt/sources.list.d/ubuntu.sources}"
    # apt takes a source list's format from its suffix.
    local copy="$1/ubuntu.sources"
    if [ ! -f "$source_list" ]; then
        echo "apt-ubuntu: $source_list is missing; there is no Ubuntu source list to read" >&2
        return 1
    fi
    # The self-hosted runners reach Ubuntu's mirrors over https.
    if [ "${RUNNER_ENVIRONMENT:-}" = self-hosted ]; then
        sed -E 's@http://(archive|security|ports)[.]ubuntu[.]com/@https://\1.ubuntu.com/@g' \
            "$source_list" > "$copy" || return 1
    else
        cp "$source_list" "$copy" || return 1
    fi
    UBUNTU_APT_OPTIONS=(
        -o "Dir::Etc::sourcelist=$copy"
        -o Dir::Etc::sourceparts=-
        -o Acquire::Retries=3
        -o APT::Update::Error-Mode=any
    )
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    set -euo pipefail
    if [ "$#" -eq 0 ]; then
        echo "usage: apt-ubuntu.sh PACKAGE..." >&2
        exit 2
    fi
    scratch=$(mktemp -d)
    trap 'rm -rf "$scratch"' EXIT
    ubuntu_apt_options "$scratch"
    sudo apt-get "${UBUNTU_APT_OPTIONS[@]}" update
    sudo apt-get "${UBUNTU_APT_OPTIONS[@]}" install -y "$@"
fi
