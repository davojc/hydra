"""Tests for projects_gate.decide and projects_gate.approvers: python3 -m unittest."""

import unittest

from projects_gate import approvers, decide

HEAD = "h2"


class Decide(unittest.TestCase):
    def test_anyone_starts_a_project_they_lead(self):
        self.assertEqual(
            decide(["pg/project.toml", "pg/spec.md"], "aza", {}, {"pg": "aza"}, set()), []
        )

    def test_a_new_project_led_by_someone_else_is_refused(self):
        problems = decide(["pg/project.toml"], "aza", {}, {"pg": "bo"}, set())
        self.assertEqual(len(problems), 1)
        self.assertIn("lead", problems[0])

    def test_the_lead_changes_their_project_freely(self):
        self.assertEqual(decide(["pg/spec.md"], "Lead", {"pg": "lead"}, {}, set()), [])

    def test_anyone_else_needs_the_leads_approval_of_the_latest_commit(self):
        self.assertEqual(len(decide(["pg/spec.md"], "aza", {"pg": "lead"}, {}, set())), 1)
        self.assertEqual(decide(["pg/spec.md"], "aza", {"pg": "lead"}, {}, {"lead"}), [])

    def test_making_yourself_lead_in_your_own_change_counts_for_nothing(self):
        # The base branch's lead decides, not the one the change writes.
        problems = decide(["pg/project.toml"], "aza", {"pg": "lead"}, {"pg": "aza"}, set())
        self.assertEqual(len(problems), 1)

    def test_an_unreadable_lead_on_the_base_lets_nobody_in(self):
        self.assertEqual(len(decide(["pg/spec.md"], "aza", {"pg": ""}, {}, {"aza"})), 1)

    def test_every_project_a_change_touches_is_checked(self):
        problems = decide(
            ["pg/spec.md", "mx/spec.md"], "lead", {"pg": "lead", "mx": "other"}, {}, set()
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("mx", problems[0])

    def test_top_level_files_belong_to_the_admins(self):
        # README.md and .github/ pass here; CODEOWNERS makes them need an admin's review.
        self.assertEqual(decide(["README.md", ".github/CODEOWNERS"], "aza", {}, {}, set()), [])
        self.assertEqual(len(decide(["notes.md"], "aza", {}, {}, set())), 1)


class Approvers(unittest.TestCase):
    def test_only_a_latest_review_approving_the_head_counts(self):
        reviews = [
            {"user": {"login": "lead"}, "state": "APPROVED", "commit_id": "h1"},
            {"user": {"login": "bo"}, "state": "APPROVED", "commit_id": HEAD},
            {"user": {"login": "cy"}, "state": "APPROVED", "commit_id": HEAD},
            {"user": {"login": "cy"}, "state": "CHANGES_REQUESTED", "commit_id": HEAD},
            {"user": {"login": "di"}, "state": "APPROVED", "commit_id": HEAD},
            {"user": {"login": "di"}, "state": "COMMENTED", "commit_id": HEAD},
        ]
        # lead approved an older commit; cy changed their mind; a comment doesn't undo di's.
        self.assertEqual(approvers(reviews, HEAD), {"bo", "di"})


if __name__ == "__main__":
    unittest.main()
