#!/usr/bin/env python3
"""Supervise libFuzzer and retain replay evidence, including on hard timeouts."""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def run(seconds, seed, corpus, output):
    if not 1 <= seconds <= 3600 or not 1 <= seed <= 4294967295:
        raise RuntimeError("seconds must be 1..3600; seed must be 1..4294967295")
    seeds = sorted(corpus.glob("*.json"))
    if not seeds:
        raise RuntimeError(
            "missing operation corpus: restore fuzz/corpus/operation_sequence"
        )
    for item in seeds:
        replay = json.loads(item.read_text())
        if replay.get("version") != 1 or not isinstance(replay.get("operations"), list):
            raise RuntimeError(f"malformed or non-v1 corpus input: {item}")
    output.mkdir(parents=True, exist_ok=True)
    (output / "run.json").write_text(
        json.dumps(
            {
                "seed": seed,
                "seconds": seconds,
                "input_seconds": 90,
                "deadline_seconds": seconds + 95,
                "rss_mb": 1024,
                "malloc_mb": 768,
                "corpus": [str(item) for item in seeds],
            },
            indent=2,
        )
        + "\n"
    )
    with tempfile.TemporaryDirectory(prefix="ntfs-opseq-") as scratch:
        command = [
            "timeout",
            "--kill-after=5",
            str(seconds + 90),
            "cargo",
            "+nightly",
            "fuzz",
            "run",
            "operation_sequence",
            scratch,
            str(corpus),
            "--",
            f"-seed={seed}",
            f"-max_total_time={seconds}",
            "-timeout=90",
            "-rss_limit_mb=1024",
            "-malloc_limit_mb=768",
            "-max_len=16384",
            "-print_final_stats=1",
            f"-artifact_prefix={output}/",
        ]
        print(
            f"operation_sequence: seed={seed}, budget={seconds}s, watchdog={seconds + 95}s",
            flush=True,
        )
        try:
            with (output / "libfuzzer.log").open("wb") as log:
                try:
                    result = subprocess.run(
                        command,
                        stdout=log,
                        stderr=subprocess.STDOUT,
                        timeout=seconds + 95,
                        env={
                            **os.environ,
                            "OPSEQ_REFERENCE_TOOLS": "1",
                            # Every finding names the seed that found it.
                            "OPSEQ_FUZZ_SEED": str(seed),
                        },
                        check=False,
                    )
                except subprocess.TimeoutExpired as error:
                    raise RuntimeError(
                        "operation explorer deadline exceeded"
                    ) from error
            if result.returncode:
                raise RuntimeError(
                    f"operation explorer failed: exit {result.returncode} (timeout is a failure)"
                )
            counts = re.findall(
                r"stat::number_of_executed_units:\s*(\d+)",
                (output / "libfuzzer.log").read_text(),
            )
            if not counts or int(counts[-1]) < len(seeds):
                raise RuntimeError(
                    "missing executed-input floor: not every corpus input ran"
                )
        except BaseException:
            active = output / "active-image.txt"
            if active.is_file() and Path(active.read_text()).is_file():
                shutil.copyfile(active.read_text(), output / "failure.img")
            raise
        finally:
            # The tier wrapper budgets the entire raw log, including failures.
            print((output / "libfuzzer.log").read_text(errors="replace"), end="")


if __name__ == "__main__":
    try:
        run(
            int(sys.argv[1]),
            int(os.environ.get("OPSEQ_FUZZ_SEED", "683")),
            Path("fuzz/corpus/operation_sequence"),
            Path("tmp/op-sequence-model/explorer"),
        )
    except (RuntimeError, OSError, ValueError, IndexError) as error:
        sys.exit(f"operation explorer: {error}")
