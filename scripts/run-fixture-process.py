"""Bound a fixture producer independently of its PowerShell pipeline."""

import argparse
import json
import os
import pathlib
import signal
import subprocess
import sys
import time


CREATE_SUSPENDED = 0x00000004


def deadline(value):
    seconds = int(value)
    if not 1 <= seconds <= 240:
        raise argparse.ArgumentTypeError("timeout must be between 1 and 240 seconds")
    return seconds


class WindowsJob:
    """A job object that owns the fixture process and every process it starts.

    taskkill /T walks parent links in a snapshot of the process table, so it
    misses a descendant whose parent has already exited and one started after
    the snapshot. A job holds a process from its creation, whatever happens
    to its parent. The fixture process starts suspended, joins the job, and
    only then runs, so nothing it starts escapes. KILL_ON_JOB_CLOSE ends the
    whole tree if this watchdog itself dies and the handle closes.
    """

    KILL_ON_JOB_CLOSE = 0x2000
    BASIC_ACCOUNTING_INFORMATION = 1
    EXTENDED_LIMIT_INFORMATION = 9
    PROCESS_TERMINATE = 0x0001
    PROCESS_SET_QUOTA = 0x0100
    SNAPSHOT_THREADS = 0x0004
    THREAD_SUSPEND_RESUME = 0x0002

    def __init__(self):
        import ctypes
        from ctypes import wintypes

        class BasicLimits(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", ctypes.c_int64), ("PerJobUserTimeLimit", ctypes.c_int64),
                ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class IoCounters(ctypes.Structure):
            _fields_ = [(name, ctypes.c_uint64) for name in (
                "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
                "ReadTransferCount", "WriteTransferCount", "OtherTransferCount")]

        class ExtendedLimits(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", BasicLimits), ("IoInfo", IoCounters),
                ("ProcessMemoryLimit", ctypes.c_size_t), ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t), ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        class Accounting(ctypes.Structure):
            _fields_ = [
                ("TotalUserTime", ctypes.c_int64), ("TotalKernelTime", ctypes.c_int64),
                ("ThisPeriodTotalUserTime", ctypes.c_int64), ("ThisPeriodTotalKernelTime", ctypes.c_int64),
                ("TotalPageFaultCount", wintypes.DWORD), ("TotalProcesses", wintypes.DWORD),
                ("ActiveProcesses", wintypes.DWORD), ("TotalTerminatedProcesses", wintypes.DWORD),
            ]

        class ThreadEntry(ctypes.Structure):
            _fields_ = [
                ("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
                ("th32ThreadID", wintypes.DWORD), ("th32OwnerProcessID", wintypes.DWORD),
                ("tpBasePri", wintypes.LONG), ("tpDeltaPri", wintypes.LONG), ("dwFlags", wintypes.DWORD),
            ]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        signatures = {
            "CreateJobObjectW": (wintypes.HANDLE, [wintypes.LPVOID, wintypes.LPCWSTR]),
            "SetInformationJobObject": (wintypes.BOOL, [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD]),
            "QueryInformationJobObject": (wintypes.BOOL, [
                wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]),
            "AssignProcessToJobObject": (wintypes.BOOL, [wintypes.HANDLE, wintypes.HANDLE]),
            "TerminateJobObject": (wintypes.BOOL, [wintypes.HANDLE, wintypes.UINT]),
            "OpenProcess": (wintypes.HANDLE, [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]),
            "OpenThread": (wintypes.HANDLE, [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]),
            "ResumeThread": (wintypes.DWORD, [wintypes.HANDLE]),
            "CreateToolhelp32Snapshot": (wintypes.HANDLE, [wintypes.DWORD, wintypes.DWORD]),
            "Thread32First": (wintypes.BOOL, [wintypes.HANDLE, ctypes.POINTER(ThreadEntry)]),
            "Thread32Next": (wintypes.BOOL, [wintypes.HANDLE, ctypes.POINTER(ThreadEntry)]),
            "CloseHandle": (wintypes.BOOL, [wintypes.HANDLE]),
        }
        for name, (restype, argtypes) in signatures.items():
            function = getattr(kernel32, name)
            function.restype = restype
            function.argtypes = argtypes
        self._ctypes = ctypes
        self._kernel32 = kernel32
        self._accounting = Accounting
        self._thread_entry = ThreadEntry
        self._handle = self._check(kernel32.CreateJobObjectW(None, None), "CreateJobObjectW")
        limits = ExtendedLimits()
        limits.BasicLimitInformation.LimitFlags = self.KILL_ON_JOB_CLOSE
        self._check(kernel32.SetInformationJobObject(
            self._handle, self.EXTENDED_LIMIT_INFORMATION, ctypes.byref(limits), ctypes.sizeof(limits),
        ), "SetInformationJobObject")

    def _check(self, value, call):
        if not value or value == self._ctypes.c_void_p(-1).value:
            error = self._ctypes.get_last_error()
            raise OSError(error, f"{call} failed: {self._ctypes.FormatError(error).strip()}")
        return value

    def adopt(self, pid):
        """Join a process created suspended to the job, then let it run."""
        kernel32 = self._kernel32
        process = self._check(kernel32.OpenProcess(
            self.PROCESS_SET_QUOTA | self.PROCESS_TERMINATE, False, pid), "OpenProcess")
        try:
            self._check(kernel32.AssignProcessToJobObject(self._handle, process), "AssignProcessToJobObject")
        finally:
            kernel32.CloseHandle(process)
        snapshot = self._check(kernel32.CreateToolhelp32Snapshot(self.SNAPSHOT_THREADS, 0), "CreateToolhelp32Snapshot")
        resumed = 0
        try:
            entry = self._thread_entry()
            entry.dwSize = self._ctypes.sizeof(entry)
            more = kernel32.Thread32First(snapshot, self._ctypes.byref(entry))
            while more:
                if entry.th32OwnerProcessID == pid:
                    thread = self._check(kernel32.OpenThread(self.THREAD_SUSPEND_RESUME, False, entry.th32ThreadID), "OpenThread")
                    try:
                        if kernel32.ResumeThread(thread) == 0xFFFFFFFF:
                            self._check(0, "ResumeThread")
                        resumed += 1
                    finally:
                        kernel32.CloseHandle(thread)
                more = kernel32.Thread32Next(snapshot, self._ctypes.byref(entry))
        finally:
            kernel32.CloseHandle(snapshot)
        if resumed == 0:
            raise OSError(f"the suspended fixture process {pid} has no thread to resume")

    def active_processes(self):
        accounting = self._accounting()
        self._check(self._kernel32.QueryInformationJobObject(
            self._handle, self.BASIC_ACCOUNTING_INFORMATION, self._ctypes.byref(accounting),
            self._ctypes.sizeof(accounting), None,
        ), "QueryInformationJobObject")
        return accounting.ActiveProcesses

    def terminate(self, grace_seconds=5):
        """End every process in the job and wait until none is left."""
        self._check(self._kernel32.TerminateJobObject(self._handle, 1), "TerminateJobObject")
        deadline = time.monotonic() + grace_seconds
        while (active := self.active_processes()) > 0:
            if time.monotonic() > deadline:
                raise OSError(f"{active} process(es) of the fixture job outlived TerminateJobObject")
            time.sleep(0.05)

    def close(self):
        self._kernel32.CloseHandle(self._handle)


def terminate_tree(process, job):
    errors = []
    try:
        if job is not None:
            job.terminate()
        else:
            os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    except OSError as error:
        errors.append(str(error))
        try:
            if process.poll() is None:
                process.kill()
        except OSError as kill_error:
            errors.append(str(kill_error))
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        errors.append("the owned fixture process did not exit after termination")
    return errors


def last_phase(path):
    try:
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            stream.seek(max(0, stream.tell() - 8192))
            lines = stream.read().decode("utf-8", errors="replace").splitlines()
        return lines[-1] if lines else "lifecycle log is empty"
    except OSError as error:
        return f"lifecycle log unavailable: {error}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout-seconds", type=deadline, required=True)
    parser.add_argument("--log-dir", type=pathlib.Path, required=True)
    parser.add_argument("--lifecycle", type=pathlib.Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    arguments = parser.parse_args()
    command = arguments.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser.error("a fixture command is required after --")

    arguments.log_dir.mkdir(parents=True, exist_ok=True)
    result = {"exit_code": None, "timed_out": False, "pid": None, "error": ""}
    started = time.monotonic()
    verdict = 127
    with (arguments.log_dir / "stdout.log").open("wb") as stdout:
        with (arguments.log_dir / "stderr.log").open("wb") as stderr:
            job = None
            if os.name == "nt":
                options = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED}
            else:
                options = {"start_new_session": True}
            try:
                if os.name == "nt":
                    job = WindowsJob()
                process = subprocess.Popen(command, stdout=stdout, stderr=stderr, **options)
            except OSError as error:
                result["error"] = f"cannot launch {command[0]}: {error}"
            else:
                result["pid"] = process.pid
                try:
                    if job is not None:
                        job.adopt(process.pid)
                except OSError as error:
                    process.kill()
                    process.wait()
                    result["error"] = f"cannot place {command[0]} in a job object: {error}"
                else:
                    try:
                        result["exit_code"] = process.wait(timeout=arguments.timeout_seconds)
                        verdict = result["exit_code"]
                        if verdict < 0:
                            verdict = 128 - verdict
                    except subprocess.TimeoutExpired:
                        result["timed_out"] = True
                        errors = terminate_tree(process, job)
                        result["exit_code"] = process.returncode
                        result["error"] = "; ".join(errors)
                        verdict = 124 if process.returncode is not None else 125
            finally:
                # On Windows this ends any descendant still in the job.
                if job is not None:
                    job.close()
    result["elapsed_seconds"] = round(time.monotonic() - started, 3)
    result["last_phase"] = last_phase(arguments.lifecycle)
    (arguments.log_dir / "result.json").write_text(
        json.dumps(result, indent=2) + "\n", encoding="utf-8",
    )
    if result["timed_out"]:
        print(f"fixture-process: timed out after {arguments.timeout_seconds}s; {result['last_phase']}", file=sys.stderr)
    elif not result["error"]:
        print(f"fixture-process: exited {verdict} after {result['elapsed_seconds']}s")
    if result["error"]:
        print(f"fixture-process: {result['error']}", file=sys.stderr)
    print(f"fixture-process: diagnostics retained in {arguments.log_dir}")
    return verdict


if __name__ == "__main__":
    raise SystemExit(main())
