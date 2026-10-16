"""The aggregate ci-ok check requires every Python oracle job.

A job outside ci-ok's needs can fail without blocking a merge, so the Windows
fsutil comparison would be advisory in practice. This reads the workflow as
text and fails when ci-ok's needs omit a required job, or when a required job
is not defined at all.
"""

import os
import re
import unittest

WORKFLOW = os.path.join(
    os.path.dirname(__file__), "..", "..", ".github", "workflows", "ci.yml"
)
REQUIRED = ["python-abi", "python-volume-info-windows"]


def jobs_and_aggregate_needs(text):
    jobs = set(re.findall(r"^  ([A-Za-z0-9_-]+):\s*$", text, re.MULTILINE))
    match = re.search(
        r"^  ci-ok:\n(?:    .*\n)*?    needs: \[([^\]]*)\]", text, re.MULTILINE
    )
    needs = [n.strip() for n in match.group(1).split(",")] if match else []
    return jobs, needs


def missing_from_gate(text, required):
    jobs, needs = jobs_and_aggregate_needs(text)
    return [j for j in required if j not in jobs or j not in needs]


class AggregateRequiresPythonOracles(unittest.TestCase):
    def setUp(self):
        with open(WORKFLOW, encoding="utf-8") as f:
            self.text = f.read()

    def test_ci_ok_needs_every_python_oracle_job(self):
        self.assertEqual(missing_from_gate(self.text, REQUIRED), [])

    def test_a_dropped_dependency_is_detected(self):
        dropped = self.text.replace(", python-volume-info-windows]", "]")
        dropped = dropped.replace(", python-abi]", "]")
        self.assertIn("python-abi", missing_from_gate(dropped, REQUIRED))

    def test_an_undefined_job_is_detected(self):
        self.assertIn("no-such-job", missing_from_gate(self.text, ["no-such-job"]))


if __name__ == "__main__":
    unittest.main()
