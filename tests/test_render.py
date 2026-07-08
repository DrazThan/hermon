"""Renderer tests: feed synthetic transcript lines through render_line and
the roster accumulator. No tmux, no real Claude sessions."""

import json
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import hermon
from hermon import ClaudeStats as SessionStats, render_claude_line as render_line

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def plain(lines):
    return [ANSI.sub("", ln) for ln in lines]


def assistant(blocks, usage=None, model=None, **extra):
    msg = {"role": "assistant", "content": blocks}
    if usage:
        msg["usage"] = usage
    if model:
        msg["model"] = model
    return json.dumps({"type": "assistant", "message": msg, **extra})


def user(content, **extra):
    return json.dumps(
        {"type": "user", "message": {"role": "user", "content": content}, **extra}
    )


class TestRenderLine(unittest.TestCase):
    def test_assistant_text_is_rendered_and_wrapped(self):
        line = assistant([{"type": "text", "text": "hello " * 40}])
        out = plain(render_line(line, width=40))
        self.assertTrue(out, "expected output lines")
        self.assertTrue(all(len(ln) <= 40 for ln in out))
        self.assertIn("hello", out[0])

    def test_tool_use_shows_name_and_clipped_input(self):
        line = assistant(
            [{"type": "tool_use", "name": "Bash", "input": {"command": "x" * 500}}]
        )
        out = plain(render_line(line))
        self.assertEqual(len(out), 1)
        self.assertIn("▶ Bash", out[0])
        # input excerpt clipped to ~120 chars
        self.assertLess(len(out[0]), 140)
        self.assertIn("…", out[0])

    def test_tool_result_truncated(self):
        line = user(
            [{"type": "tool_result", "content": "y" * 500}]
        )
        out = plain(render_line(line))
        self.assertEqual(len(out), 1)
        self.assertTrue(out[0].startswith("◀ result"))
        self.assertLess(len(out[0]), 220)

    def test_tool_result_error_marker(self):
        line = user(
            [{"type": "tool_result", "content": "boom", "is_error": True}]
        )
        out = plain(render_line(line))
        self.assertIn("◀ ERROR", out[0])
        self.assertIn("boom", out[0])

    def test_tool_result_block_list_content(self):
        line = user(
            [{"type": "tool_result",
              "content": [{"type": "text", "text": "block text"}]}]
        )
        out = plain(render_line(line))
        self.assertIn("block text", out[0])

    def test_usage_sigma_line(self):
        line = assistant(
            [{"type": "text", "text": "ok"}],
            usage={"input_tokens": 1200, "output_tokens": 34,
                   "cache_read_input_tokens": 800},
        )
        out = plain(render_line(line))
        self.assertIn("Σ in:2,000 out:34", out[-1])

    def test_result_event_with_cost(self):
        line = json.dumps({
            "type": "result",
            "total_cost_usd": 0.1234,
            "usage": {"input_tokens": 10, "output_tokens": 5},
        })
        out = plain(render_line(line))
        self.assertEqual(len(out), 1)
        self.assertIn("Σ in:10 out:5 $0.1234", out[0])

    def test_user_string_prompt(self):
        line = user("do the thing")
        out = plain(render_line(line))
        self.assertIn("» user:", out[0])
        self.assertIn("do the thing", out[0])

    def test_unknown_event_type_single_dim_line(self):
        out = plain(render_line(json.dumps({"type": "wormhole", "junk": [1, 2]})))
        self.assertEqual(out, ["· wormhole"])

    def test_unknown_block_type_single_dim_line(self):
        line = assistant([{"type": "thinking", "thinking": "hmm"}])
        out = plain(render_line(line))
        self.assertEqual(out, ["· thinking"])

    def test_malformed_json_is_skipped_not_raised(self):
        out = plain(render_line("{not json"))
        self.assertEqual(out, ["· parse-skip"])

    def test_non_object_json_is_skipped(self):
        out = plain(render_line("[1, 2, 3]"))
        self.assertEqual(out, ["· parse-skip"])

    def test_blank_line_yields_nothing(self):
        self.assertEqual(render_line("   \n"), [])

    def test_survives_empty_object_lines(self):
        # §7 version-tolerance requirement: a transcript of `{}` lines.
        for _ in range(50):
            out = render_line("{}")
            self.assertEqual(plain(out), ["· unknown"])

    def test_never_raises_on_hostile_shapes(self):
        hostile = [
            json.dumps({"type": "assistant", "message": "not a dict"}),
            json.dumps({"type": "assistant", "message": {"content": 42}}),
            json.dumps({"type": "assistant",
                        "message": {"role": "assistant", "content": [None, 7, "x"]}}),
            json.dumps({"type": "user",
                        "message": {"role": "user", "content": {"weird": True}}}),
            json.dumps({"type": "result", "usage": "nope", "total_cost_usd": "free"}),
            "null", "true", '"string"', "12",
        ]
        for line in hostile:
            render_line(line)  # must not raise


class TestSessionStats(unittest.TestCase):
    def _write(self, lines):
        tmp = tempfile.NamedTemporaryFile(
            mode="w", suffix=".jsonl", delete=False
        )
        tmp.write("\n".join(lines) + "\n")
        tmp.close()
        self.addCleanup(os.unlink, tmp.name)
        return tmp.name

    def test_accumulates_tokens_cost_model_and_tool(self):
        path = self._write([
            user("hi", timestamp="2026-07-08T10:00:00Z"),
            assistant(
                [{"type": "tool_use", "name": "Read", "input": {}}],
                usage={"input_tokens": 100, "output_tokens": 10},
                model="claude-fable-5",
                timestamp="2026-07-08T10:00:05Z",
            ),
            assistant(
                [{"type": "text", "text": "done"}],
                usage={"input_tokens": 200, "output_tokens": 20},
                model="claude-fable-5",
                timestamp="2026-07-08T10:01:00Z",
            ),
            json.dumps({"type": "result", "total_cost_usd": 0.5,
                        "timestamp": "2026-07-08T10:01:01Z"}),
            "{malformed",
            "{}",
        ])
        s = SessionStats(path)
        s.update()
        self.assertEqual(s.model, "claude-fable-5")
        self.assertEqual(s.in_tok, 300)
        self.assertEqual(s.out_tok, 30)
        self.assertEqual(s.cost, 0.5)
        self.assertEqual(s.last_tool, "Read")
        self.assertAlmostEqual(s.elapsed, 61.0)

    def test_update_is_incremental(self):
        path = self._write([assistant(
            [{"type": "text", "text": "a"}],
            usage={"input_tokens": 1, "output_tokens": 1},
        )])
        s = SessionStats(path)
        s.update()
        s.update()  # second pass over same bytes must not double-count
        self.assertEqual(s.in_tok, 1)
        with open(path, "a") as f:
            f.write(assistant(
                [{"type": "text", "text": "b"}],
                usage={"input_tokens": 2, "output_tokens": 2},
            ) + "\n")
        s.update()
        self.assertEqual(s.in_tok, 3)
        self.assertEqual(s.out_tok, 3)

    def test_truncation_resets_cleanly(self):
        path = self._write([assistant(
            [], usage={"input_tokens": 5, "output_tokens": 5})])
        s = SessionStats(path)
        s.update()
        self.assertEqual(s.in_tok, 5)
        with open(path, "w") as f:
            f.write("{}\n")  # file replaced with something shorter
        s.update()
        self.assertEqual(s.in_tok, 0)
        with open(path, "a") as f:
            f.write(assistant(
                [], usage={"input_tokens": 1, "output_tokens": 1}) + "\n")
        s.update()
        self.assertEqual(s.in_tok, 1)


if __name__ == "__main__":
    unittest.main()
