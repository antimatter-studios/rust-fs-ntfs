import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/run-fixture-process.py"


class FixtureProcess(unittest.TestCase):
    def setUp(self):
        self.sandbox = tempfile.TemporaryDirectory(prefix="fixture process ")
        self.addCleanup(self.sandbox.cleanup)
        self.work = pathlib.Path(self.sandbox.name)
        self.logs = self.work / "logs"
        self.lifecycle = self.work / "lifecycle.log"

    def run_child(self, source, timeout=10, executable=None, watch_phases=False):
        executable = executable or sys.executable
        suffix = ".ps1" if pathlib.Path(executable).stem == "pwsh" else ".py"
        script = self.work / ("child fixture" + suffix)
        script.write_text(source, encoding="utf-8")
        arguments = [executable, str(script)]
        if suffix == ".ps1":
            arguments = [executable, "-NoProfile", "-File", str(script)]
        return subprocess.run(
            [sys.executable, str(RUNNER), "--timeout-seconds", str(timeout),
             "--log-dir", str(self.logs), "--lifecycle", str(self.lifecycle),
             *(["--watch-phases"] if watch_phases else []), "--", *arguments],
            capture_output=True, text=True, timeout=25,
        )

    def report(self):
        return json.loads((self.logs / "result.json").read_text(encoding="utf-8"))

    def test_success_retains_both_streams_and_paths_with_spaces(self):
        result = self.run_child("import sys\nprint('fixture stdout')\nprint('fixture stderr', file=sys.stderr)\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.logs / "stdout.log").read_text().strip(), "fixture stdout")
        self.assertEqual((self.logs / "stderr.log").read_text().strip(), "fixture stderr")
        self.assertEqual(self.report()["exit_code"], 0)
        self.assertFalse(self.report()["timed_out"])

    def test_native_failure_preserves_status_and_diagnostics(self):
        result = self.run_child("import sys\nprint('native failed', file=sys.stderr)\nsys.exit(7)\n")
        self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
        self.assertEqual(self.report()["exit_code"], 7)
        self.assertIn("native failed", (self.logs / "stderr.log").read_text())

    def test_phase_deadline_ends_the_producer_before_its_overall_deadline(self):
        source = (
            "import json,os,pathlib,time\n"
            "print('before the stall', flush=True)\n"
            "phase = {'id': 'phase-7', 'name': 'win-read', 'seconds': 1}\n"
            "state = pathlib.Path(os.environ['NTFS_FIXTURE_PHASE_STATE'])\n"
            "state.write_text(json.dumps(phase))\n"
            "time.sleep(0.5)\n"
            "state.write_text('{')\n"
            "time.sleep(30)\n"
        )
        result = self.run_child(source, timeout=20, watch_phases=True)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertLess(self.report()["elapsed_seconds"], 15)
        self.assertEqual(self.report()["timed_out_phase"], "win-read")
        self.assertIn("win-read", result.stderr)
        self.assertIn("before the stall", result.stdout, "the job log lost the producer's output")

    def test_blocked_dotnet_phase_and_its_descendant_end_at_the_phase_deadline(self):
        pwsh = shutil.which("pwsh")
        self.assertIsNotNone(pwsh, "install PowerShell with chore setup:tools")

        def quoted(value):
            return "'" + str(value).replace("'", "''") + "'"

        marker = self.work / "descendant-survived"
        ready = self.work / "descendant-started"
        leaf = self.work / "leaf.py"
        leaf.write_text(
            "import pathlib,sys,time\n"
            "pathlib.Path(sys.argv[1]).write_text('ready')\n"
            "time.sleep(12)\n"
            "pathlib.Path(sys.argv[2]).write_text('alive')\n",
            encoding="utf-8",
        )
        source = (
            "$ErrorActionPreference = 'Stop'\n"
            f". {quoted(ROOT / 'test-disks/windows-fixture.ps1')}\n"
            "Write-FixturePhase test-fixture attach\n"
            "Invoke-FixturePhase -PhaseName attach -PhaseTimeoutSeconds 3 -PhaseAction {\n"
            "    $start = [Diagnostics.ProcessStartInfo]::new()\n"
            f"    $start.FileName = {quoted(sys.executable)}\n"
            "    $start.UseShellExecute = $false\n"
            f"    foreach ($a in {quoted(leaf)}, {quoted(ready)}, {quoted(marker)}) {{ $start.ArgumentList.Add($a) }}\n"
            "    $null = [Diagnostics.Process]::Start($start)\n"
            "    [Threading.Thread]::Sleep(60000)\n"
            "}\n"
        )
        result = self.run_child(source, timeout=22, executable=pwsh, watch_phases=True)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertEqual(self.report()["timed_out_phase"], "attach")
        self.assertLess(self.report()["elapsed_seconds"], 20)
        self.assertIn("test-fixture attach", self.lifecycle.read_text())
        self.assertTrue(ready.exists(), "the descendant never started, so nothing was proven")
        time.sleep(13)
        self.assertFalse(marker.exists(), "the blocked phase's descendant outlived the watchdog")

    def test_finished_phases_leave_a_normal_run_and_its_artifact(self):
        pwsh = shutil.which("pwsh")
        self.assertIsNotNone(pwsh, "install PowerShell with chore setup:tools")
        helper = str(ROOT / "test-disks/windows-fixture.ps1").replace("'", "''")
        artifact = self.work / "artifact"
        target = str(artifact).replace("'", "''")
        source = (
            "$ErrorActionPreference = 'Stop'\n"
            f". '{helper}'\n"
            "foreach ($n in 1..20) {\n"
            "    Invoke-FixturePhase -PhaseName \"quick-$n\" -PhaseTimeoutSeconds 30 -PhaseAction { }\n"
            "}\n"
            f"[IO.File]::WriteAllText('{target}', 'built')\n"
        )
        result = self.run_child(source, timeout=20, executable=pwsh, watch_phases=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(artifact.read_text(), "built")
        self.assertEqual((self.logs / "phase.json").read_text().strip(), "null")

    def test_uninterruptible_phase_is_killed_and_retains_its_last_phase(self):
        pwsh = shutil.which("pwsh")
        self.assertIsNotNone(pwsh, "install PowerShell with chore setup:tools")
        helper = str(ROOT / "test-disks/windows-fixture.ps1").replace("'", "''")
        lifecycle = str(self.lifecycle).replace("'", "''")
        source = (
            "$ErrorActionPreference = 'Stop'\n"
            f". '{helper}'\n"
            f"[IO.File]::WriteAllText('{lifecycle}', 'phase=blocking-storage state=started')\n"
            "Invoke-FixturePhase -PhaseName blocking-storage -PhaseTimeoutSeconds 1 -PhaseAction {\n"
            "    [Threading.Thread]::Sleep(30000)\n"
            "}\n"
        )
        started = time.monotonic()
        result = self.run_child(source, timeout=20, executable=pwsh)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertLess(time.monotonic() - started, 60)
        self.assertTrue(self.report()["timed_out"])
        self.assertIsNotNone(self.report()["exit_code"])
        self.assertIn("phase=blocking-storage state=started", self.lifecycle.read_text())
        self.assertIn("phase=blocking-storage state=started", result.stderr)

    def test_timeout_kills_descendants_not_just_the_parent(self):
        marker = self.work / "descendant survived"
        ready = self.work / "descendant started"
        descendant = f"import pathlib,time; time.sleep(3); pathlib.Path({str(marker)!r}).write_text('alive')"
        source = (
            "import pathlib,subprocess,sys,time\n"
            f"subprocess.Popen([sys.executable, '-c', {descendant!r}])\n"
            f"pathlib.Path({str(ready)!r}).write_text('ready')\n"
            "time.sleep(30)\n"
        )
        result = self.run_child(source, timeout=1)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertTrue(ready.exists())
        time.sleep(3.5)
        self.assertFalse(marker.exists(), "the owned descendant outlived the watchdog")

    def test_timeout_kills_orphans_started_up_to_the_deadline(self):
        # Until the deadline the fixture starts intermediate processes in a
        # tight loop. Each starts a leaf and exits at once, so every leaf
        # but the last is an orphan before the watchdog fires. A kill that
        # walks parent links from the fixture process (taskkill /T) cannot
        # reach an orphan; only ownership taken at creation (a Windows job
        # object, a POSIX process group) can.
        leaves = self.work / "leaves"
        leaves.mkdir()
        orphaned = self.work / "first orphan started"
        leaf = "import pathlib,sys,time; time.sleep(3); pathlib.Path(sys.argv[1]).write_text('alive')"
        middle = f"import subprocess,sys; subprocess.Popen([sys.executable, '-c', {leaf!r}, sys.argv[1]])"
        source = (
            "import pathlib,subprocess,sys\n"
            f"leaves = pathlib.Path({str(leaves)!r})\n"
            "started = 0\n"
            "while True:\n"
            f"    subprocess.run([sys.executable, '-c', {middle!r}, str(leaves / f'leaf {{started}}')])\n"
            "    started += 1\n"
            "    if started == 1:\n"
            f"        pathlib.Path({str(orphaned)!r}).write_text('orphaned')\n"
        )
        result = self.run_child(source, timeout=1)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertTrue(orphaned.exists(), "no orphan was started before the deadline")
        time.sleep(5)
        survivors = sorted(path.name for path in leaves.iterdir())
        self.assertEqual(survivors, [], "orphaned descendants outlived the watchdog")

    def test_invalid_timeout_is_refused(self):
        result = self.run_child("raise SystemExit('must not run')", timeout=0)
        self.assertEqual(result.returncode, 2)
        self.assertIn("timeout", result.stderr)

    def test_failed_launch_has_a_retained_verdict(self):
        result = self.run_child("", executable=str(self.work / "missing executable"))
        self.assertEqual(result.returncode, 127, result.stdout + result.stderr)
        self.assertIsNone(self.report()["exit_code"])
        self.assertIn("missing executable", self.report()["error"])


if __name__ == "__main__":
    unittest.main()
