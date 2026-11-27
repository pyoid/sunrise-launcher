import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SPEC = importlib.util.spec_from_file_location("release_notes", Path(__file__).resolve().parents[1] / "release_notes.py")
notes = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notes)


def pull(number, title, *labels):
    return {"number": number, "title": title, "labels": [{"name": label} for label in labels]}


class ReleaseNotes(unittest.TestCase):
    def test_features_come_first_and_skipped_pull_requests_are_left_out(self):
        title, body = notes.notes("v1.2.0", [
            pull(7, "Fix the update check offline"),
            pull(8, "Instances keep their own build", "feature"),
            pull(9, "CI: daily ignored tests", "skip-release"),
        ])
        self.assertEqual(title, "Sunrise Launcher 1.2.0")
        self.assertLess(body.index("Instances keep their own build (#8)"), body.index("Fix the update check offline (#7)"))
        self.assertNotIn("#9", body)

    def test_a_release_with_nothing_to_tell_says_so(self):
        self.assertIn("Maintenance release.", notes.notes("v1.2.1", [pull(1, "CI", "skip-release")])[1])

    def test_the_tag_is_cargo_tomls_version_and_never_released_twice(self):
        head = "a" * 40
        with mock.patch.object(notes, "version", return_value="1.3.0"), mock.patch.object(notes, "tags", return_value=["v1.2.0"]):
            self.assertEqual(notes.decide(head), {"version": "1.3.0", "tag": "v1.3.0", "head": head})
        with mock.patch.object(notes, "version", return_value="1.2.0"), mock.patch.object(notes, "tags", return_value=["v1.2.0"]):
            with self.assertRaises(SystemExit):
                notes.decide(head)

    def test_only_a_stable_version_newer_than_the_game_repositorys_last_launcher_is_released(self):
        head = "a" * 40
        # launcher@1.0.0 and launcher@1.0.1 went out from the game repository: installed copies would never take either again.
        for refused in ["1.0.0", "1.0.1", "0.9.9", "1.1.0-rc1", "1.1.0+build", "01.1.0", "1.1", "1.1.0.0", "1.1.0\n"]:
            with mock.patch.object(notes, "version", return_value=refused), mock.patch.object(notes, "tags", return_value=[]):
                with self.assertRaises(SystemExit, msg=refused):
                    notes.decide(head)
        with mock.patch.object(notes, "version", return_value="1.0.2"), mock.patch.object(notes, "tags", return_value=[]):
            self.assertEqual(notes.decide(head)["tag"], "v1.0.2")
            with self.assertRaises(SystemExit):
                notes.decide("main")

    def test_unpublished_tags_are_skipped_and_noted(self):
        previous, skipped = notes.previous_release(
            ["v1.3.9", "v1.3.8", "v1.3.7", "v1.3.6"],
            "v1.3.9",
            {"v1.3.7", "v1.3.5"},
        )
        self.assertEqual(previous, "v1.3.7")
        self.assertEqual(skipped, ["v1.3.8"])
        title, body = notes.notes("v1.3.9", [pull(647, "Remove automatic cache cleanup")], skipped)
        self.assertEqual(title, "Sunrise Launcher 1.3.9")
        self.assertTrue(body.startswith("1.3.8 was not released.\n\n### Fixed and changed\n"))
        self.assertIn("Remove automatic cache cleanup (#647)", body)

    def test_tags_are_sorted_and_filter_non_release_names(self):
        completed = subprocess.CompletedProcess([], 0, stdout="v1.4.2 v1.4.1-rc.1 v1.4 not-a-release")
        with mock.patch.object(notes.subprocess, "run", return_value=completed) as run:
            self.assertEqual(notes.tags(), ["v1.4.2", "v1.4.1-rc.1"])
        run.assert_called_once_with(
            ["git", "-C", str(notes.ROOT), "tag", "--list", "v*", "--sort=-v:refname"],
            capture_output=True, text=True, check=True,
        )

    def test_tags_can_be_limited_to_the_release_heads_ancestors(self):
        completed = subprocess.CompletedProcess([], 0, stdout="v1.3.9\n")
        with mock.patch.object(notes.subprocess, "run", return_value=completed) as run:
            self.assertEqual(notes.tags("a" * 40), ["v1.3.9"])
        self.assertIn(f"--merged={'a' * 40}", run.call_args.args[0])

    def test_published_tags_use_all_pages_and_exclude_drafts(self):
        completed = subprocess.CompletedProcess([], 0, stdout="v1.3.9\n\n v1.3.7 \nv1.3.9\n")
        with mock.patch.object(notes.subprocess, "run", return_value=completed) as run:
            self.assertEqual(notes.published_tags(), {"v1.3.9", "v1.3.7"})
        cmd = run.call_args.args[0]
        self.assertIn("--paginate", cmd)
        self.assertIn(f"repos/{notes.REPOSITORY}/releases", cmd)
        self.assertIn(".[] | select(.draft | not) | .tag_name", cmd)
        self.assertTrue(run.call_args.kwargs["check"])

    def test_a_failed_publication_lookup_is_not_an_empty_release_list(self):
        with mock.patch.object(notes.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "gh")):
            with self.assertRaises(subprocess.CalledProcessError):
                notes.published_tags()

    def test_previous_release_never_compares_the_tag_to_itself(self):
        self.assertEqual(
            notes.previous_release(["v1.3.9", "v1.3.7"], "v1.3.9", {"v1.3.9", "v1.3.7"}),
            ("v1.3.7", []),
        )

    def test_no_published_ancestor_is_the_first_release(self):
        self.assertEqual(
            notes.previous_release(["v1.3.9", "v1.3.8"], "v1.3.9", {"v1.3.7"}),
            (None, []),
        )

    def test_multiple_unreleased_tags_are_listed_oldest_first(self):
        _, body = notes.notes("v1.3.9", [], ["v1.3.8", "v1.3.7"])
        self.assertTrue(body.startswith("1.3.7 was not released.\n1.3.8 was not released.\n\nMaintenance release."))

    def test_merged_pull_requests_are_filtered_by_the_revision_range(self):
        included = dict(pull(2, "Between releases"), mergeCommit={"oid": "included"})
        excluded = dict(pull(1, "Already released"), mergeCommit={"oid": "excluded"})
        replies = ["2026-11-25T15:00:00Z\n", json.dumps([included, excluded, pull(3, "No merge")]), "included\n"]
        with mock.patch.object(
            notes.subprocess, "run", side_effect=[subprocess.CompletedProcess([], 0, stdout=r) for r in replies],
        ) as run:
            self.assertEqual(notes.merged_since("v1.3.7", "head", "release/launcher-1.3.6"), [included])
        self.assertIn("is:merged base:release/launcher-1.3.6 merged:>2026-11-25T15:00:00Z", run.call_args_list[1].args[0])
        self.assertEqual(run.call_args.args[0][-2:], ["rev-list", "v1.3.7..head"])

    def test_notes_command_uses_the_latest_published_ancestor(self):
        head = "a" * 40
        with (
            tempfile.TemporaryDirectory() as folder,
            mock.patch.object(notes, "tags", return_value=["v1.3.9", "v1.3.8", "v1.3.7"]) as tags,
            mock.patch.object(notes, "published_tags", return_value={"v1.3.7"}),
            mock.patch.object(notes, "merged_since", return_value=[pull(650, "Skip unpublished tags")]) as merged,
            mock.patch("builtins.print"),
        ):
            self.assertEqual(notes.main(["notes", "--tag", "v1.3.9", "--head", head, "--out", folder]), 0)
            tags.assert_called_once_with(head)
            merged.assert_called_once_with("v1.3.7", head, "main")
            self.assertEqual((Path(folder) / "title.txt").read_text(), "Sunrise Launcher 1.3.9\n")
            self.assertIn("1.3.8 was not released.", (Path(folder) / "body.md").read_text())


if __name__ == "__main__":
    unittest.main()
