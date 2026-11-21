"""Python private-span redaction must agree with the Rust Redactor.

Both implementations read tests/fixtures/redaction_cases.json. The Rust side is
`redaction::tests::shared_fixture_cases_match` in src/engine/redaction.rs.
"""
import json
import unittest
from pathlib import Path

try:
    from .test_visibility import PLUGIN
except ImportError:
    from test_visibility import PLUGIN

FIXTURE = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "redaction_cases.json"
CASES = json.loads(FIXTURE.read_text(encoding="utf-8"))["cases"]


class RedactionParityTests(unittest.TestCase):
    def test_fixture_is_not_empty(self):
        self.assertGreaterEqual(len(CASES), 10)

    def test_shared_fixture_cases(self):
        for case in CASES:
            with self.subTest(case["name"]):
                redactor = PLUGIN._PrivateRedactor()
                redactor.depth = case["depth_in"]
                self.assertEqual(redactor.text(case["input"]), case["expected"])
                self.assertEqual(redactor.depth, case["depth_out"])

    def test_output_is_stable_when_redacted_again(self):
        for case in CASES:
            if case["depth_in"] or case["depth_out"]:
                continue
            with self.subTest(case["name"]):
                self.assertEqual(
                    PLUGIN._PrivateRedactor().text(case["expected"]), case["expected"]
                )


if __name__ == "__main__":
    unittest.main()
