"""Hermes-source tests: render messages-table rows and scan a fixture
state.db. No tmux, no real Hermes."""

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

from hermon import HermesSource, render_hermes_row

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def plain(lines):
    return [ANSI.sub("", ln) for ln in lines]


FIXTURE_SCHEMA = """
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    model TEXT,
    title TEXT,
    started_at REAL NOT NULL,
    ended_at REAL,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0,
    cache_write_tokens INTEGER DEFAULT 0,
    estimated_cost_usd REAL,
    actual_cost_usd REAL
);
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT,
    tool_calls TEXT,
    tool_name TEXT,
    timestamp REAL NOT NULL
);
"""


class TestRenderHermesRow(unittest.TestCase):
    def test_assistant_text(self):
        out = plain(render_hermes_row("assistant", "hello world", None, None))
        self.assertEqual(out, ["hello world"])

    def test_assistant_tool_calls_openai_shape(self):
        calls = json.dumps([{
            "id": "toolu_x", "type": "function",
            "function": {"name": "terminal",
                         "arguments": json.dumps({"command": "ls " + "x" * 300})},
        }])
        out = plain(render_hermes_row("assistant", None, calls, None))
        self.assertEqual(len(out), 1)
        self.assertIn("▶ terminal", out[0])
        self.assertIn("…", out[0])  # args clipped
        self.assertLess(len(out[0]), 140)

    def test_tool_result_json_output_extracted(self):
        content = json.dumps({"output": "OK-SMOKE", "exit_code": 0, "error": None})
        out = plain(render_hermes_row("tool", content, None, "terminal"))
        self.assertEqual(len(out), 1)
        self.assertIn("◀ terminal", out[0])
        self.assertIn("OK-SMOKE", out[0])
        self.assertNotIn("exit_code", out[0])

    def test_tool_result_error_via_exit_code(self):
        content = json.dumps({"output": "boom", "exit_code": 1, "error": None})
        out = plain(render_hermes_row("tool", content, None, "terminal"))
        self.assertIn("◀ ERROR", out[0])

    def test_tool_result_error_via_error_field(self):
        content = json.dumps({"output": None, "exit_code": 0, "error": "timeout"})
        out = plain(render_hermes_row("tool", content, None, "web"))
        self.assertIn("◀ ERROR", out[0])
        self.assertIn("timeout", out[0])

    def test_tool_result_plain_text_content(self):
        out = plain(render_hermes_row("tool", "not json at all", None, "search"))
        self.assertIn("◀ search", out[0])
        self.assertIn("not json at all", out[0])

    def test_user_prompt(self):
        out = plain(render_hermes_row("user", "run the tests", None, None))
        self.assertIn("» user:", out[0])
        self.assertIn("run the tests", out[0])

    def test_unknown_role_dim_marker(self):
        out = plain(render_hermes_row("developer", "x", None, None))
        self.assertEqual(out, ["· developer"])

    def test_never_raises_on_hostile_rows(self):
        hostile = [
            ("assistant", None, "{broken json", None),
            ("assistant", None, json.dumps([None, 42, {"function": "str"}]), None),
            ("tool", None, None, None),
            (None, None, None, None),
            ("user", None, None, None),
            ("tool", json.dumps({"weird": True}), None, None),
        ]
        for role, content, tool_calls, tool_name in hostile:
            render_hermes_row(role, content, tool_calls, tool_name)  # must not raise


class TestHermesSource(unittest.TestCase):
    def _fixture_db(self, now):
        d = tempfile.mkdtemp()
        path = os.path.join(d, "state.db")
        conn = sqlite3.connect(path)
        conn.executescript(FIXTURE_SCHEMA)
        conn.execute(
            "INSERT INTO sessions (id, source, model, title, started_at, ended_at,"
            " input_tokens, output_tokens, cache_read_tokens, estimated_cost_usd)"
            " VALUES ('20260708_100000_live01', 'tui', 'claude-sonnet-5',"
            " 'Live One', ?, NULL, 100, 20, 50, 0.05)", (now - 120,))
        conn.execute(
            "INSERT INTO sessions (id, source, model, title, started_at, ended_at)"
            " VALUES ('20260708_090000_done02', 'cli', 'gpt-5-mini', 'Done', ?, ?)",
            (now - 7200, now - 7000))
        for i in range(3):
            conn.execute(
                "INSERT INTO messages (session_id, role, content, tool_name, timestamp)"
                " VALUES ('20260708_100000_live01', 'tool', '{\"output\":\"x\"}',"
                " ?, ?)", (f"tool{i}", now - 30 + i))
        conn.commit()
        conn.close()
        self.addCleanup(lambda: os.unlink(path))
        return path

    def test_snapshot_states_and_labels(self):
        now = time.time()
        src = HermesSource(self._fixture_db(now))
        snaps = src.snapshot(now, idle_timeout=60)
        live = snaps["hermes:20260708_100000_live01"]
        self.assertEqual(live.state, "live")       # last message 28s ago
        self.assertEqual(live.label, "H:live01")
        self.assertEqual(live.argv[0:2], ["render", "--hermes"])
        # ended_at set -> done regardless of anything else
        done = snaps.get("hermes:20260708_090000_done02")
        if done is not None:  # outside idle window it may be filtered entirely
            self.assertEqual(done.state, "done")

    def test_session_stats_row(self):
        now = time.time()
        src = HermesSource(self._fixture_db(now))
        rows = src.sessions(now - 3600)
        live = next(r for r in rows if r["id"].endswith("live01"))
        self.assertEqual(live["in_tok"], 150)  # input + cache_read
        self.assertEqual(live["out_tok"], 20)
        self.assertAlmostEqual(live["cost"], 0.05)
        self.assertEqual(live["title"], "Live One")
        self.assertGreater(live["last_ts"], live["started_at"])

    def test_last_tool(self):
        now = time.time()
        src = HermesSource(self._fixture_db(now))
        self.assertEqual(src.last_tool("20260708_100000_live01"), "tool2")
        self.assertEqual(src.last_tool("nope"), "-")

    def test_missing_db_returns_empty(self):
        src = HermesSource("/nonexistent/dir/state.db")
        self.assertEqual(src.sessions(0), [])
        self.assertEqual(src.snapshot(time.time(), 60), {})


if __name__ == "__main__":
    unittest.main()
