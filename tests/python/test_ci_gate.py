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


def drop_aggregate_dependency(text, dependency):
    """Mutate the aggregate's actual list without relying on list position."""
    before, marker, aggregate = text.partition("  ci-ok:\n")
    if not marker:
        raise AssertionError("ci-ok is absent from the workflow")
    lines = aggregate.splitlines(keepends=True)
    prefix = "    needs: ["
    for index, line in enumerate(lines):
        if re.match(r"^  [A-Za-z0-9_-]+:", line):
            break
        if line.startswith(prefix):
            content, closing, suffix = line[len(prefix):].partition("]")
            if not closing:
                raise AssertionError("ci-ok needs is not an inline list")
            names = [name.strip() for name in content.split(",")]
            if dependency not in names:
                raise AssertionError(f"{dependency} is absent from ci-ok needs")
            names.remove(dependency)
            lines[index] = prefix + ", ".join(names) + closing + suffix
            return before + marker + "".join(lines)
    raise AssertionError("ci-ok needs is absent from the workflow")


class AggregateRequiresPythonOracles(unittest.TestCase):
    def setUp(self):
        with open(WORKFLOW, encoding="utf-8") as f:
            self.text = f.read()

    def test_ci_ok_needs_every_python_oracle_job(self):
        self.assertEqual(missing_from_gate(self.text, REQUIRED), [])

    def test_a_dropped_dependency_is_detected(self):
        dropped = drop_aggregate_dependency(self.text, "python-abi")
        self.assertIn("python-abi", missing_from_gate(dropped, REQUIRED))

    def test_an_undefined_job_is_detected(self):
        self.assertIn("no-such-job", missing_from_gate(self.text, ["no-such-job"]))

    def test_a_dropped_dependency_is_detected_in_each_list_position(self):
        for dependency in REQUIRED:
            others = [name for name in REQUIRED if name != dependency] + ["other-gate"]
            for position in range(len(others) + 1):
                with self.subTest(dependency=dependency, position=position):
                    names = others.copy()
                    names.insert(position, dependency)
                    text = "jobs:\n" + "".join(
                        f"  {name}:\n    runs-on: ubuntu-latest\n" for name in names
                    )
                    text += "  ci-ok:\n    needs: [" + ", ".join(names) + "]\n"
                    self.assertEqual(missing_from_gate(text, REQUIRED), [])
                    dropped = drop_aggregate_dependency(text, dependency)
                    jobs, needs = jobs_and_aggregate_needs(dropped)
                    self.assertIn(dependency, jobs)
                    self.assertEqual(needs, others)
                    self.assertEqual(missing_from_gate(dropped, REQUIRED), [dependency])

    def test_a_negative_control_refuses_an_absent_mutation_target(self):
        with self.assertRaisesRegex(AssertionError, "absent"):
            drop_aggregate_dependency(self.text, "no-such-job")
        with self.assertRaisesRegex(AssertionError, "absent"):
            drop_aggregate_dependency("jobs:\n", "python-abi")


if __name__ == "__main__":
    unittest.main()
