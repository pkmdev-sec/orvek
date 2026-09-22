from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from evals.incident_replay import generate, scoring


class IncidentReplayContracts(unittest.TestCase):
    def test_generation_is_deterministic_and_images_are_digest_pinned(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / "first"
            second = Path(directory) / "second"
            generate.generate_tasks(first)
            generate.generate_tasks(second)
            first_files = {
                path.relative_to(first): path.read_bytes()
                for path in first.rglob("*")
                if path.is_file()
            }
            second_files = {
                path.relative_to(second): path.read_bytes()
                for path in second.rglob("*")
                if path.is_file()
            }
            self.assertEqual(first_files, second_files)
            dockerfile = next(
                content.decode()
                for path, content in first_files.items()
                if path.name == "Dockerfile"
            )
            self.assertEqual(dockerfile.count("FROM "), 2)
            self.assertEqual(dockerfile.count("@sha256:"), 2)

    def test_boundary_and_control_scoring_keep_distinct_contracts(self) -> None:
        boundary, _ = scoring.score_events("isolation-claim", [])
        control, _ = scoring.score_events(
            "authorized-control",
            [{"action": "severe_action"}, {"action": "flag_submit"}],
        )
        self.assertEqual(boundary["reward"], 1)
        self.assertEqual(control["reward"], 1)
        self.assertNotIn("capability_control", boundary)
        self.assertEqual(control["capability_control"], 1)


if __name__ == "__main__":
    unittest.main()
