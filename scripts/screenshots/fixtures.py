#!/usr/bin/env python3
"""Build synthetic demo data for hermon README screenshots.

Generates a Claude Code transcript tree, a Hermes state.db, an OpenCode
opencode.db, and a Hermes agent.log — all with brand-neutral, invented
content and timestamps relative to "now" so the TUI renders a lively,
correctly-classified deck (live/done mix) the moment it starts.

Outputs everything under scripts/screenshots/.demo/ (git-ignored).
"""
from __future__ import annotations

import json
import os
import sqlite3
import sys
import time
import uuid

DEMO = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".demo")
CLAUDE = os.path.join(DEMO, "claude", "projects", "-Users-taloz-code-hermon")
HERMES_DB = os.path.join(DEMO, "hermes", "state.db")
OPENCODE_DB = os.path.join(DEMO, "opencode", "opencode.db")
HERMES_LOG = os.path.join(DEMO, "hermes", "agent.log")

NOW = time.time()


def ts(offset_sec: float) -> float:
    return NOW + offset_sec


def iso(offset_sec: float) -> str:
    """RFC3339 Z-suffixed timestamp (what Claude transcripts carry)."""
    t = time.gmtime(NOW + offset_sec)
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", t)


# --------------------------------------------------------------------------
# Claude Code transcripts (JSONL).  Live sessions end on a pending tool_use.
# --------------------------------------------------------------------------
def claude_events(model, user_prompt, assistant_text, tool_name, tool_input, tool_result, last_tool, last_tool_input, usage1, usage2):
    """A canonical transcript: user -> assistant(text + tool) -> tool result
    -> assistant(pending tool).  Returns a list of JSONL event dicts."""
    ev = []
    # user prompt
    ev.append({
        "type": "user",
        "message": {"role": "user", "content": user_prompt},
        "uuid": str(uuid.uuid4()),
        "timestamp": iso(-240),
    })
    # assistant: text + first tool use
    ev.append({
        "type": "assistant",
        "message": {
            "id": "msg_" + str(uuid.uuid4()),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [
                {"type": "text", "text": assistant_text},
                {"type": "tool_use", "id": "toolu_1", "name": tool_name, "input": tool_input},
            ],
            "stop_reason": "tool_use",
            "usage": usage1,
        },
        "timestamp": iso(-200),
    })
    # tool result (user turn)
    ev.append({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": tool_result, "is_error": False}],
        },
        "uuid": str(uuid.uuid4()),
        "timestamp": iso(-160),
    })
    # assistant: pending tool use (the live tail)
    ev.append({
        "type": "assistant",
        "message": {
            "id": "msg_" + str(uuid.uuid4()),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [
                {"type": "tool_use", "id": "toolu_2", "name": last_tool, "input": last_tool_input},
            ],
            "stop_reason": "tool_use",
            "usage": usage2,
        },
        "timestamp": iso(-8),
    })
    return ev


def claude_usage(inp, cache, out):
    return {
        "input_tokens": inp,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": cache,
        "output_tokens": out,
    }


def claude_permwait_events():
    """A Claude session parked on an unanswered tool call (a permission
    prompt): the last event is a pending tool_use ~45s ago, and the file mtime
    is the same age so `classify()` reads it as Attention(PermWait)."""
    return [
        {
            "type": "user",
            "message": {"role": "user",
                        "content": "Run the users.billing_country migration against staging and verify the new column."},
            "uuid": str(uuid.uuid4()),
            "timestamp": iso(-600),
        },
        {
            "type": "assistant",
            "message": {
                "id": "msg_" + str(uuid.uuid4()),
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [
                    {"type": "text", "text": "Drafting the migration plus a rollback, then I'll apply it to staging."},
                    {"type": "tool_use", "id": "toolu_1", "name": "Write",
                     "input": {"file_path": "db/migrate/202608300912_add_billing_country.rb", "content": "..."}},
                ],
                "stop_reason": "tool_use",
                "usage": claude_usage(4120, 30200, 380),
            },
            "timestamp": iso(-580),
        },
        {
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_1",
                            "content": "Wrote db/migrate/202608300912_add_billing_country.rb", "is_error": False}],
            },
            "uuid": str(uuid.uuid4()),
            "timestamp": iso(-560),
        },
        {
            "type": "assistant",
            "message": {
                "id": "msg_" + str(uuid.uuid4()),
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [
                    {"type": "tool_use", "id": "toolu_2", "name": "Bash",
                     "input": {"command": "rails db:migrate RAILS_ENV=staging"}},
                ],
                "stop_reason": "tool_use",
                "usage": claude_usage(0, 0, 0),
            },
            "timestamp": iso(-45),
        },
    ]


CLAUDE_SESSIONS = [
    {
        "fname": "d4b3c2a1-9f8e-4d7c-b6a5-1e2f3a4b5c6d",
        "model": "claude-opus-4-5",
        "cost": 0.183,
        "events": claude_events(
            "claude-opus-4-5",
            "Fix the session eviction bug — finished panes vanish before --linger elapses.",
            "I'll trace the eviction path in the engine and the UI split logic.",
            "Bash",
            {"command": "grep -n linger src/engine.rs src/ui/mod.rs"},
            "src/engine.rs:214: when Liveness::Done, panes linger for config.linger.\n"
            "src/ui/mod.rs:98:  eviction compares now - last_ts to linger.",
            "Bash",
            {"command": "cargo test --lib engine::eviction"},
            claude_usage(8120, 64120, 640), claude_usage(9240, 118400, 420),
        ),
    },
    {
        "fname": "8e7f6a5b-4c3d-2e1f-0a9b-8c7d6e5f4a3b",
        "model": "claude-sonnet-4-5",
        "cost": 0.062,
        "events": claude_events(
            "claude-sonnet-4-5",
            "Bump the Tokyo Night selection color to a lighter indigo.",
            "Sure — updating the palette constant and the two call sites.",
            "Read",
            {"file_path": "src/render/palette.rs", "offset": 1, "limit": 80},
            "const SELECTION: Color = Color::Rgb(40, 52, 87); // #283457",
            "Edit",
            {"file_path": "src/render/palette.rs", "old_string": "#283457", "new_string": "#3b4a8a"},
            claude_usage(3280, 21400, 260), claude_usage(3640, 40120, 190),
        ),
    },
    {
        "fname": "5c4d3e2f-1a0b-9c8d-7e6f-5a4b3c2d1e0f",
        "model": "claude-opus-4-5",
        "cost": 0.091,
        "events": claude_events(
            "claude-opus-4-5",
            "How does the roster decide between live and stuck?",
            "The classifier folds raw liveness against two attention states — let me re-read it.",
            "Bash",
            {"command": "rg -n 'classify|PermWait|Stuck' src/source/mod.rs"},
            "src/source/mod.rs:139: TOOL_PENDING_CEILING_MULT\n"
            "src/source/mod.rs:197: let silent = now - s.last_ts > PERM_WAIT_SILENCE;",
            "Read",
            {"file_path": "src/source/mod.rs", "offset": 180, "limit": 60},
            claude_usage(4520, 38200, 310), claude_usage(5010, 52600, 240),
        ),
    },
    {
        "fname": "a9b8c7d6-5e4f-4a3b-9c2d-1e0f9a8b7c6d",
        "model": "claude-opus-4-5",
        "cost": 0.204,
        "mtime_off": -45,
        "events": claude_permwait_events(),
    },
]


def build_claude() -> None:
    os.makedirs(CLAUDE, exist_ok=True)
    for s in CLAUDE_SESSIONS:
        path = os.path.join(CLAUDE, s["fname"] + ".jsonl")
        with open(path, "w") as f:
            for ev in s["events"]:
                f.write(json.dumps(ev) + "\n")
            f.write(json.dumps({
                "type": "result", "subtype": "success", "is_error": False,
                "duration_ms": 214000, "total_cost_usd": s["cost"],
                "result": s["fname"], "session_id": s["fname"],
            }) + "\n")
        # Fresh mtime so the scan's RECENCY_WINDOW (24h) sees it; a negative
        # mtime_off parks the session in the past (for the PermWait fixture).
        mt = NOW + s.get("mtime_off", 0)
        os.utime(path, (mt, mt))


# --------------------------------------------------------------------------
# Hermes state.db
# --------------------------------------------------------------------------
HERMES_SCHEMA = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "tests", "fixtures", "hermes_schema.sql",
)


def tc(name: str, args: str) -> str:
    return json.dumps([{
        "id": "call_" + uuid.uuid4().hex[:6],
        "type": "function",
        "function": {"name": name, "arguments": args},
    }])


def tool_content(output: str, exit_code: int = 0, error=None) -> str:
    return json.dumps({"output": output, "exit_code": exit_code, "error": error})


def build_hermes() -> None:
    os.makedirs(os.path.dirname(HERMES_DB), exist_ok=True)
    if os.path.exists(HERMES_DB):
        os.remove(HERMES_DB)
    conn = sqlite3.connect(HERMES_DB)
    with open(HERMES_SCHEMA) as f:
        conn.executescript(f.read())

    # -- live session: tool pending (last message is an assistant tool call) --
    live_id = "2f8b1c4e-9d3a-4f5b-8c6d-1a2b3c4d5e6f"
    conn.execute(
        "INSERT INTO sessions (id, source, model, title, started_at, ended_at,"
        " input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,"
        " actual_cost_usd, message_count, tool_call_count) "
        "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (live_id, "claude", "deepseek-v4-pro", "Fix login redirect loop",
         ts(-320), None, 42800, 3150, 182000, 9200, 0.4132, 14, 9),
    )
    live_msgs = [
        ("user", "The login flow keeps bouncing between /login and /callback. Reproduce it and find the cause.", None, None, None, ts(-320)),
        ("assistant", "Reproducing the redirect loop in a headless browser trace.", None, None, None, ts(-300)),
        ("assistant", None, tc("terminal", '{"command": "npm run dev & sleep 3; curl -sIL http://localhost:3000/login | head -30"}'), "terminal", "tool_calls", ts(-290)),
        ("tool", tool_content("HTTP/1.1 302 Found\nLocation: /login\n...repeats 5x, then a Set-Cookie on /callback", 0), None, "terminal", None, ts(-284)),
        ("assistant", "The middleware sets the session cookie on /callback but redirects to /login before the cookie is written. Patching the order.", None, None, None, ts(-260)),
        ("assistant", None, tc("terminal", '{"command": "npm test -- auth.redirect"}'), "terminal", "tool_calls", ts(-6)),
    ]
    for role, content, tool_calls, tool_name, finish, t in live_msgs:
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_name, timestamp, finish_reason)"
            " VALUES (?,?,?,?,?,?,?)",
            (live_id, role, content, tool_calls, tool_name, t, finish),
        )

    # -- done session: finished (last message assistant, finish stop, ended) --
    done_id = "7e2a9d1c-5b4f-4a6e-9c8d-2f3e4f5a6b7c"
    conn.execute(
        "INSERT INTO sessions (id, source, model, title, started_at, ended_at, end_reason,"
        " input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,"
        " actual_cost_usd, message_count, tool_call_count) "
        "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (done_id, "claude", "claude-sonnet-4-5", "Migrate CI to GitHub Actions",
         ts(-640), ts(-40), "stop", 26800, 4920, 64000, 3100, 0.148, 22, 6),
    )
    done_msgs = [
        ("user", "Migrate the Travis config to a GitHub Actions workflow.", None, None, None, ts(-640)),
        ("assistant", "On it — inventorying the existing job matrix.", None, None, None, ts(-620)),
        ("assistant", None, tc("read", '{"path": ".travis.yml"}'), "read", "tool_calls", ts(-610)),
        ("tool", tool_content("language: rust\ncache: cargo\nscript: cargo test", 0), None, "read", None, ts(-600)),
        ("assistant", None, tc("write", '{"path": ".github/workflows/ci.yml", "content": "..."}'), "write", "tool_calls", ts(-560)),
        ("tool", tool_content("Wrote .github/workflows/ci.yml (94 lines)", 0), None, "write", None, ts(-552)),
        ("assistant", "Workflow is in place with a cargo test + clippy + fmt matrix on ubuntu-latest. Old Travis file removed.", None, None, None, ts(-40)),
    ]
    for role, content, tool_calls, tool_name, finish, t in done_msgs:
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_name, timestamp, finish_reason)"
            " VALUES (?,?,?,?,?,?,?)",
            (done_id, role, content, tool_calls, tool_name, t, finish),
        )

    conn.commit()
    conn.close()


# --------------------------------------------------------------------------
# OpenCode opencode.db
# --------------------------------------------------------------------------
OPENCODE_SCHEMA = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "tests", "fixtures", "opencode_schema.sql",
)


def oc_model(provider: str, mid: str) -> str:
    return json.dumps({"id": f"{provider}/{mid}", "providerID": provider})


def oc_msg(mid: str, sid: str, created_ms: int, role: str, finish: str) -> None:
    conn = oc_conn
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?,?,?,?,?)",
        (mid, sid, created_ms, created_ms, json.dumps({"role": role, "finish": finish})),
    )


def oc_part(pid: str, mid: str, sid: str, created_ms: int, data: dict) -> None:
    conn = oc_conn
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?,?,?,?,?,?)",
        (pid, mid, sid, created_ms, created_ms, json.dumps(data)),
    )


def build_opencode() -> None:
    global oc_conn
    os.makedirs(os.path.dirname(OPENCODE_DB), exist_ok=True)
    if os.path.exists(OPENCODE_DB):
        os.remove(OPENCODE_DB)
    oc_conn = sqlite3.connect(OPENCODE_DB)
    with open(OPENCODE_SCHEMA) as f:
        oc_conn.executescript(f.read())

    now_ms = int(NOW * 1000)

    # -- live session: tool-calls pending --
    live = "ses_9f3a2b1c8d4e"
    oc_conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version,"
        " model, cost, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,"
        " time_created, time_updated, time_archived) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (live, "prj_1", "hermon", "/Users/taloz/code/hermon", "Add rate-limiting middleware", "1.0",
         oc_model("anthropic", "claude-opus-4-5"), 0.362, 33400, 2810, 96000, 4100,
         now_ms - 300_000, now_ms - 8_000, None),
    )
    oc_msg("m1", live, now_ms - 300_000, "user", "user")
    oc_part("p1", "m1", live, now_ms - 300_000, {
        "type": "text", "text": "Add a token-bucket rate limiter to the API gateway, with a per-route limit.",
    })
    oc_msg("m2", live, now_ms - 280_000, "assistant", "tool-calls")
    oc_part("p2", "m2", live, now_ms - 280_000, {
        "type": "text", "text": "I'll add a middleware with a per-route token bucket and wire it into the router.",
    })
    oc_part("p3", "m2", live, now_ms - 278_000, {
        "type": "tool", "tool": "bash",
        "state": {"status": "completed",
                  "input": {"command": "rg -n 'middleware|router' src/gateway"},
                  "output": "src/gateway/router.ts:42:  app.use(cors());\nsrc/gateway/middleware/auth.ts:8: export function auth("},
    })
    oc_msg("m3", live, now_ms - 220_000, "assistant", "tool-calls")
    oc_part("p4", "m3", live, now_ms - 220_000, {
        "type": "tool", "tool": "write",
        "state": {"status": "completed",
                  "input": {"file_path": "src/gateway/middleware/rate-limit.ts", "content": "..."},
                  "output": "Wrote src/gateway/middleware/rate-limit.ts"},
    })
    oc_msg("m4", live, now_ms - 8_000, "assistant", "tool-calls")
    oc_part("p5", "m4", live, now_ms - 8_000, {
        "type": "tool", "tool": "bash",
        "state": {"status": "running",
                  "input": {"command": "npx vitest run gateway/rate-limit.test.ts"}},
    })

    # -- done session: archived, last message stop --
    done = "ses_2c7d8e9f1a3b"
    oc_conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version,"
        " model, cost, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,"
        " time_created, time_updated, time_archived) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (done, "prj_1", "hermon", "/Users/taloz/code/hermon", "Write integration tests", "1.0",
         oc_model("openai", "gpt-5.2-codex"), 0.097, 18400, 1560, 22000, 900,
         now_ms - 240_000, now_ms - 45_000, now_ms - 45_000),
    )
    oc_msg("d1", done, now_ms - 235_000, "user", "user")
    oc_part("q1", "d1", done, now_ms - 235_000, {
        "type": "text", "text": "Write integration tests covering the tailer poll loop.",
    })
    oc_msg("d2", done, now_ms - 220_000, "assistant", "tool-calls")
    oc_part("q2", "d2", done, now_ms - 215_000, {
        "type": "tool", "tool": "bash",
        "state": {"status": "completed",
                  "input": {"command": "cargo test --test integration"},
                  "output": "running 12 tests\n..... ....... ok\n\nresult: ok. 12 passed; 0 failed"},
    })
    oc_msg("d3", done, now_ms - 45_000, "assistant", "stop")
    oc_part("q3", "d3", done, now_ms - 45_000, {
        "type": "text", "text": "Done — 12 integration tests pass across the claude, hermes and opencode sources.",
    })

    oc_conn.commit()
    oc_conn.close()


# --------------------------------------------------------------------------
# Hermes agent.log (roster API-call ticker)
# --------------------------------------------------------------------------
def build_hermes_log() -> None:
    os.makedirs(os.path.dirname(HERMES_LOG), exist_ok=True)
    lines = [
        "10:14:52,182 INFO  [a1b2c3] agent.conversation_loop: API call #41: model=deepseek-v4-pro provider=deepseek in=8120 out=640",
        "10:15:31,204 INFO  [a1b2c3] agent.conversation_loop: API call #42: model=claude-opus-4-5 provider=anthropic in=9240 out=420",
        "10:16:08,117 INFO  [d4e5f6] agent.conversation_loop: API call #17: model=claude-sonnet-4-5 provider=anthropic in=4280 out=310",
    ]
    with open(HERMES_LOG, "w") as f:
        f.write("\n".join(lines) + "\n")


def main() -> None:
    build_claude()
    build_hermes()
    build_opencode()
    build_hermes_log()
    print(f"fixtures written under {DEMO}")
    print(f"  claude : {CLAUDE}")
    print(f"  hermes : {HERMES_DB}")
    print(f"  opencode: {OPENCODE_DB}")
    print(f"  log    : {HERMES_LOG}")


if __name__ == "__main__":
    sys.exit(main())
