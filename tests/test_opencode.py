"""OpenCode-source tests: render part rows and scan a fixture opencode.db.
No tmux, no real OpenCode. Times in the fixture are epoch milliseconds,
matching the real schema."""

import json
import os
import re
import sqlite3
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hermon import OpenCodeSource, render_opencode_part, turn_liveness

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def plain(lines):
    return [ANSI.sub("", ln) for ln in lines]


FIXTURE_SCHEMA = """
CREATE TABLE session (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    model TEXT,
    cost REAL DEFAULT 0 NOT NULL,
    tokens_input INTEGER DEFAULT 0 NOT NULL,
    tokens_output INTEGER DEFAULT 0 NOT NULL,
    tokens_cache_read INTEGER DEFAULT 0 NOT NULL,
    tokens_cache_write INTEGER DEFAULT 0 NOT NULL,
    time_created INTEGER NOT NULL,
    time_updated INTEGER NOT NULL,
    time_archived INTEGER
);
CREATE TABLE message (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    time_updated INTEGER NOT NULL,
    data TEXT NOT NULL
);
CREATE TABLE part (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    time_updated INTEGER NOT NULL,
    data TEXT NOT NULL
);
"""


def msg(role, finish=None):
    d = {"role": role}
    if finish is not None:
        d["finish"] = finish
    return json.dumps(d)


class TestRenderOpencodePart(unittest.TestCase):
    def test_text_part_user_role(self):
        data = json.dumps({"type": "text", "text": "implement the feature"})
        lines, status = render_opencode_part("user", data, None)
        out = plain(lines)
        self.assertIn("» user:", out[0])
        self.assertIn("implement the feature", out[0])
        self.assertEqual(status, "shown")

    def test_text_part_assistant_role(self):
        data = json.dumps({"type": "text", "text": "Here's the plan."})
        lines, status = render_opencode_part("assistant", data, None)
        self.assertEqual(plain(lines), ["Here's the plan."])

    def test_non_tool_part_rendered_once(self):
        data = json.dumps({"type": "text", "text": "hello"})
        lines1, status1 = render_opencode_part("assistant", data, None)
        self.assertTrue(lines1)
        lines2, status2 = render_opencode_part("assistant", data, status1)
        self.assertEqual(lines2, [])  # already shown, no re-render

    def test_tool_call_first_seen_pending(self):
        data = json.dumps({
            "type": "tool", "tool": "bash",
            "state": {"status": "running", "input": {"command": "ls " + "x" * 300}},
        })
        lines, status = render_opencode_part("assistant", data, None)
        out = plain(lines)
        self.assertEqual(len(out), 1)
        self.assertIn("▶ bash", out[0])
        self.assertIn("…", out[0])
        self.assertEqual(status, "running")

    def test_tool_call_transitions_to_completed_emits_result(self):
        pending = json.dumps({
            "type": "tool", "tool": "bash",
            "state": {"status": "running", "input": {"command": "ls"}},
        })
        _, status = render_opencode_part("assistant", pending, None)
        completed = json.dumps({
            "type": "tool", "tool": "bash",
            "state": {"status": "completed", "input": {"command": "ls"},
                      "output": "file1\nfile2"},
        })
        lines, status2 = render_opencode_part("assistant", completed, status)
        out = plain(lines)
        self.assertEqual(len(out), 1)
        self.assertIn("◀ bash", out[0])
        self.assertIn("file1", out[0])
        self.assertEqual(status2, "completed")

    def test_tool_call_already_completed_on_first_sight_shows_both_lines(self):
        # a fast tool call can complete between two polls
        data = json.dumps({
            "type": "tool", "tool": "read",
            "state": {"status": "completed", "input": {"filePath": "x.py"},
                      "output": "print('hi')"},
        })
        lines, status = render_opencode_part("assistant", data, None)
        out = plain(lines)
        self.assertEqual(len(out), 2)
        self.assertIn("▶ read", out[0])
        self.assertIn("◀ read", out[1])
        self.assertEqual(status, "completed")

    def test_tool_call_error(self):
        data = json.dumps({
            "type": "tool", "tool": "edit",
            "state": {"status": "error", "input": {},
                      "error": "user rejected permission"},
        })
        lines, status = render_opencode_part("assistant", data, None)
        out = plain(lines)
        self.assertEqual(len(out), 2)
        self.assertIn("◀ ERROR", out[1])
        self.assertIn("user rejected permission", out[1])

    def test_no_op_update_does_not_reprint(self):
        completed = json.dumps({
            "type": "tool", "tool": "bash",
            "state": {"status": "completed", "output": "ok"},
        })
        lines1, status1 = render_opencode_part("assistant", completed, None)
        self.assertTrue(lines1)
        # same status seen again (e.g. a cosmetic re-touch) -> nothing new
        lines2, status2 = render_opencode_part("assistant", completed, status1)
        self.assertEqual(lines2, [])

    def test_reasoning_and_step_parts_are_dim_markers(self):
        for ptype in ("reasoning", "step-start", "step-finish", "file", "patch"):
            data = json.dumps({"type": ptype})
            lines, _ = render_opencode_part("assistant", data, None)
            self.assertEqual(plain(lines), [f"· {ptype}"])

    def test_malformed_json_is_skipped_not_raised(self):
        lines, status = render_opencode_part("assistant", "{not json", None)
        self.assertEqual(plain(lines), ["· parse-skip"])
        self.assertIsNone(status)

    def test_non_object_json_is_skipped(self):
        lines, _ = render_opencode_part("assistant", "[1,2,3]", None)
        self.assertEqual(plain(lines), ["· parse-skip"])

    def test_never_raises_on_hostile_shapes(self):
        hostile = [
            json.dumps({"type": "tool", "tool": "x", "state": "not a dict"}),
            json.dumps({"type": "tool", "state": {"status": "completed"}}),
            json.dumps({"type": None}),
            "null", "true", '"string"', "12",
        ]
        for data in hostile:
            render_opencode_part("assistant", data, None)  # must not raise


class TestOpenCodeSource(unittest.TestCase):
    def _fixture_db(self, now_ms):
        d = tempfile.mkdtemp()
        path = os.path.join(d, "opencode.db")
        conn = sqlite3.connect(path)
        conn.executescript(FIXTURE_SCHEMA)
        model = json.dumps({"id": "claude-sonnet-5", "providerID": "github-copilot"})
        conn.execute(
            "INSERT INTO session (id, title, model, cost, tokens_input,"
            " tokens_output, tokens_cache_read, time_created, time_updated,"
            " time_archived) VALUES ('ses_live01', 'Live One', ?, 0.05, 100,"
            " 20, 50, ?, ?, NULL)",
            (model, now_ms - 120_000, now_ms - 5_000))
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)"
            " VALUES ('msg_1', 'ses_live01', ?, ?, ?)",
            (now_ms - 5_000, now_ms - 5_000, msg("assistant", "tool-calls")))
        conn.commit()
        conn.close()
        self.addCleanup(lambda: os.unlink(path))
        return path

    def test_snapshot_states_and_labels(self):
        now_ms = time.time() * 1000
        src = OpenCodeSource(self._fixture_db(now_ms))
        snaps = src.snapshot(now_ms / 1000, idle_timeout=180)
        live = snaps["opencode:ses_live01"]
        self.assertEqual(live.label, "O:live01")
        self.assertEqual(live.argv[0:2], ["render", "--opencode"])
        # tool-calls pending -> live even past the base idle_timeout window
        self.assertEqual(live.state, "live")

    def test_session_stats_row(self):
        now_ms = time.time() * 1000
        src = OpenCodeSource(self._fixture_db(now_ms))
        rows = src.sessions(now_ms / 1000 - 3600)
        live = next(r for r in rows if r["id"] == "ses_live01")
        self.assertEqual(live["model"], "claude-sonnet-5")
        self.assertEqual(live["in_tok"], 150)  # input + cache_read
        self.assertEqual(live["out_tok"], 20)
        self.assertAlmostEqual(live["cost"], 0.05)
        self.assertEqual(live["title"], "Live One")
        self.assertTrue(live["tool_pending"])
        self.assertFalse(live["turn_done"])

    def test_turn_done_on_clean_stop(self):
        now_ms = time.time() * 1000
        path = self._fixture_db(now_ms)
        conn = sqlite3.connect(path)
        conn.execute(
            "UPDATE message SET data = ? WHERE id = 'msg_1'",
            (msg("assistant", "stop"),))
        conn.commit()
        conn.close()
        src = OpenCodeSource(path)
        snaps = src.snapshot(now_ms / 1000, idle_timeout=180)
        self.assertEqual(snaps["opencode:ses_live01"].state, "done")

    def test_last_tool_scans_recent_parts(self):
        now_ms = time.time() * 1000
        path = self._fixture_db(now_ms)
        conn = sqlite3.connect(path)
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created,"
            " time_updated, data) VALUES ('prt_1', 'msg_1', 'ses_live01', ?, ?, ?)",
            (now_ms - 4000, now_ms - 4000,
             json.dumps({"type": "text", "text": "thinking"})))
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created,"
            " time_updated, data) VALUES ('prt_2', 'msg_1', 'ses_live01', ?, ?, ?)",
            (now_ms - 3000, now_ms - 3000,
             json.dumps({"type": "tool", "tool": "bash",
                         "state": {"status": "completed"}})))
        conn.commit()
        conn.close()
        src = OpenCodeSource(path)
        self.assertEqual(src.last_tool("ses_live01"), "bash")
        self.assertEqual(src.last_tool("nope"), "-")

    def test_missing_db_returns_empty(self):
        src = OpenCodeSource("/nonexistent/dir/opencode.db")
        self.assertEqual(src.sessions(0), [])
        self.assertEqual(src.snapshot(time.time(), 180), {})

    def test_shares_turn_liveness_with_hermes(self):
        # OpenCodeSource rows must fit the exact dict shape turn_liveness
        # expects, since both sources feed the same classifier.
        now_ms = time.time() * 1000
        src = OpenCodeSource(self._fixture_db(now_ms))
        s = src.sessions(now_ms / 1000 - 3600)[0]
        state = turn_liveness(s, now_ms / 1000, 180)
        self.assertEqual(state, "live")


if __name__ == "__main__":
    unittest.main()
