#!/usr/bin/env python3
"""hermon: live monitor deck for Hermes and Claude Code agent sessions.

(Hermes + monitor, and the mountain.) One tmux window — one iTerm2 window
under `tmux -CC` — that splits into a pane per live session and unsplits
when sessions end. Read-only; never sends input to sessions.

Sources:
  * Claude Code transcripts   ~/.claude/projects/<slug>/<uuid>.jsonl
  * Hermes session store      ~/.hermes/state.db  (sessions + messages)
  * Hermes small/aux calls    ~/.hermes/logs/agent.log  (roster ticker)

Python 3.9+ stdlib only. External binaries: tmux (required for `watch`),
lsof (optional, improves Claude-transcript liveness).

Usage:
  hermon watch  [--session NAME] [--interval SEC] [--fresh-window SEC]
                [--idle-timeout SEC] [--linger SEC] [--max-panes N]
                [--claude-root DIR] [--hermes-db PATH] [--hermes-log PATH]
                [--no-claude] [--no-hermes]
  hermon render FILE [--replay-bytes N]
  hermon render --hermes SESSION_ID [--hermes-db PATH] [--replay-msgs N]
  hermon render --summary [source flags] [--interval SEC]
  hermon ls     [source flags] [--fresh-window SEC]
"""

import argparse
import json
import os
import re
import shlex
import shutil
import sqlite3
import subprocess
import sys
import textwrap
import time
from datetime import datetime
from pathlib import Path

DEFAULT_CLAUDE_ROOT = str(Path.home() / ".claude" / "projects")
DEFAULT_HERMES_DB = str(Path.home() / ".hermes" / "state.db")
DEFAULT_HERMES_LOG = str(Path.home() / ".hermes" / "logs" / "agent.log")
DEFAULT_SESSION = "hermon"
WINDOW = "deck"
ROSTER_TITLE = "hermon-roster"

# ---------------------------------------------------------------- colors

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
CYAN = "\033[36m"

USE_COLOR = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None


def c(code, s):
    return f"{code}{s}{RESET}" if USE_COLOR else str(s)


# ---------------------------------------------------------------- helpers


def short_id(ident):
    """Pane/roster label core: last 6 chars of a uuid stem or hermes id."""
    return str(ident)[-6:] or "??????"


def clip(s, n):
    s = " ".join(str(s).split())
    return s if len(s) <= n else s[: n - 1] + "…"


def term_width(default=100):
    try:
        return max(shutil.get_terminal_size((default, 24)).columns - 2, 20)
    except Exception:
        return default


def parse_ts(val):
    """ISO timestamp -> epoch seconds, or None."""
    if not isinstance(val, str):
        return None
    try:
        return datetime.fromisoformat(val.replace("Z", "+00:00")).timestamp()
    except Exception:
        return None


def fmt_elapsed(sec):
    if sec is None or sec < 0:
        return "-"
    sec = int(sec)
    if sec < 60:
        return f"{sec}s"
    if sec < 3600:
        return f"{sec // 60}m{sec % 60:02d}s"
    return f"{sec // 3600}h{(sec % 3600) // 60:02d}m"


def wrap_prefixed(prefix_colored, text, width):
    lines = textwrap.wrap(text.strip(), max(width - 8, 20)) or [""]
    out = [prefix_colored + " " + lines[0]]
    out.extend("        " + ln for ln in lines[1:])
    return out


# ---------------------------------------------------------------- claude transcript liveness

LSOF = shutil.which("lsof")


def has_open_handle(path):
    """True if some process holds the file open for WRITING, else False;
    None when lsof is unavailable/failed.

    Write access only: hermon's own renderer panes hold the transcript
    open read-only, and counting those would keep every session live forever.
    """
    if not LSOF:
        return None
    try:
        r = subprocess.run(
            [LSOF, "-F", "a", "--", str(path)],
            capture_output=True, text=True, timeout=5,
        )
        # -F a emits one `a<mode>` line per open descriptor: r, w, or u.
        return any(
            line.startswith("a") and ("w" in line or "u" in line)
            for line in r.stdout.splitlines()
        )
    except Exception:
        return None


# ---------------------------------------------------------------- claude transcript renderer


def _result_text(content):
    """tool_result content may be a plain string or a list of blocks."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for b in content:
            if isinstance(b, dict) and b.get("type") == "text":
                parts.append(str(b.get("text", "")))
        return " ".join(parts)
    return str(content)


def _usage_in(usage):
    total = 0
    for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"):
        v = usage.get(k)
        if isinstance(v, (int, float)):
            total += int(v)
    return total


def _event_cost(ev):
    for k in ("total_cost_usd", "cost_usd", "costUSD"):
        v = ev.get(k)
        if isinstance(v, (int, float)):
            return float(v)
    return None


def render_claude_line(raw, width=None):
    """Render one Claude transcript line to a list of printable strings.

    Defensive by contract: never raises, never dumps raw JSON. Unknown
    event/block types collapse to a dim `· <type>` marker.
    """
    width = width or term_width()
    raw = raw.strip()
    if not raw:
        return []
    try:
        ev = json.loads(raw)
    except Exception:
        return [c(DIM, "· parse-skip")]
    if not isinstance(ev, dict):
        return [c(DIM, "· parse-skip")]

    etype = ev.get("type")
    msg = ev.get("message") if isinstance(ev.get("message"), dict) else {}
    role = msg.get("role")
    content = msg.get("content")
    out = []

    if etype == "assistant" or role == "assistant":
        if isinstance(content, list):
            for block in content:
                if not isinstance(block, dict):
                    continue
                btype = block.get("type")
                if btype == "text":
                    text = str(block.get("text", "")).strip()
                    for ln in textwrap.wrap(text, width):
                        out.append(ln)
                elif btype == "tool_use":
                    name = block.get("name", "?")
                    try:
                        arg = json.dumps(block.get("input", {}), ensure_ascii=False)
                    except Exception:
                        arg = ""
                    out.append(c(BOLD, f"▶ {name}") + " " + c(DIM, clip(arg, 120)))
                else:
                    out.append(c(DIM, f"· {btype}"))
        usage = msg.get("usage") if isinstance(msg.get("usage"), dict) else None
        if usage:
            line = f"Σ in:{_usage_in(usage):,} out:{int(usage.get('output_tokens') or 0):,}"
            cost = _event_cost(ev)
            if cost is not None:
                line += f" ${cost:.4f}"
            out.append(c(CYAN, line))

    elif etype == "user" or role == "user":
        if isinstance(content, str):
            out.extend(wrap_prefixed(c(YELLOW, "» user:"), content, width))
        elif isinstance(content, list):
            for block in content:
                if not isinstance(block, dict):
                    continue
                btype = block.get("type")
                if btype == "tool_result":
                    text = clip(_result_text(block.get("content", "")), 200)
                    if block.get("is_error"):
                        out.append(c(RED, "◀ ERROR") + " " + c(DIM, text))
                    else:
                        out.append(c(DIM, f"◀ result {text}"))
                elif btype == "text":
                    text = str(block.get("text", "")).strip()
                    if text:
                        out.extend(wrap_prefixed(c(YELLOW, "» user:"), text, width))
                else:
                    out.append(c(DIM, f"· {btype}"))

    elif etype == "result":
        usage = ev.get("usage") if isinstance(ev.get("usage"), dict) else {}
        line = f"Σ in:{_usage_in(usage):,} out:{int(usage.get('output_tokens') or 0):,}"
        cost = _event_cost(ev)
        if cost is not None:
            line += f" ${cost:.4f}"
        out.append(c(GREEN, line))

    else:
        out.append(c(DIM, f"· {etype if etype is not None else 'unknown'}"))

    return out


def cmd_render_claude(path, replay_bytes):
    path = Path(path).expanduser()
    print(c(DIM, f"hermon · tailing {path}"), flush=True)
    f = None
    buf = ""
    first_open = True
    warned_missing = False
    while True:
        if f is None:
            try:
                f = open(path, "r", errors="replace")
            except OSError:
                if not warned_missing:
                    print(c(DIM, "· transcript not found — waiting"), flush=True)
                    warned_missing = True
                time.sleep(1)
                continue
            warned_missing = False
            if first_open:
                first_open = False
                try:
                    size = os.fstat(f.fileno()).st_size
                except OSError:
                    size = 0
                if replay_bytes >= 0 and size > replay_bytes:
                    f.seek(size - replay_bytes)
                    f.readline()  # discard partial line at the seek point

        chunk = f.readline()
        if chunk:
            buf += chunk
            if buf.endswith("\n"):
                for ln in render_claude_line(buf):
                    print(ln, flush=True)
                buf = ""
            continue

        # EOF: watch for deletion or truncation, then keep polling.
        try:
            st = os.stat(path)
        except OSError:
            print(c(DIM, "· transcript removed — waiting for it to return"), flush=True)
            f.close()
            f = None
            buf = ""
            time.sleep(1)
            continue
        if st.st_size < f.tell():
            print(c(DIM, "· transcript truncated — reloading"), flush=True)
            f.close()
            f = None
            buf = ""
            continue
        time.sleep(0.25)


# ---------------------------------------------------------------- claude transcript stats


class ClaudeStats:
    """Incremental per-transcript accumulator for the roster."""

    def __init__(self, path):
        self.path = Path(path)
        self.offset = 0
        self.model = "?"
        self.in_tok = 0
        self.out_tok = 0
        self.cost_sum = 0.0        # summed per-message costs (older transcripts)
        self.cost_reported = None  # running total from `result` events (authoritative)
        self.last_tool = "-"
        self.first_ts = None
        self.last_ts = None
        self.state = "live"

    @property
    def cost(self):
        return self.cost_reported if self.cost_reported is not None else self.cost_sum

    @property
    def elapsed(self):
        if self.first_ts is None or self.last_ts is None:
            return None
        return self.last_ts - self.first_ts

    def _reset(self):
        self.__init__(self.path)

    def update(self):
        try:
            size = os.path.getsize(self.path)
        except OSError:
            return
        if size < self.offset:  # truncated/replaced: reparse from scratch
            self._reset()
        if size == self.offset:
            return
        try:
            with open(self.path, "r", errors="replace") as f:
                f.seek(self.offset)
                for raw in f:
                    if not raw.endswith("\n"):
                        break  # partial trailing line; re-read next tick
                    self.offset += len(raw.encode("utf-8", "replace"))
                    self._ingest(raw)
        except OSError:
            return

    def _ingest(self, raw):
        try:
            ev = json.loads(raw)
        except Exception:
            return
        if not isinstance(ev, dict):
            return
        ts = parse_ts(ev.get("timestamp"))
        if ts is not None:
            if self.first_ts is None:
                self.first_ts = ts
            self.last_ts = ts
        msg = ev.get("message") if isinstance(ev.get("message"), dict) else {}
        if isinstance(msg.get("model"), str):
            self.model = msg["model"]
        usage = msg.get("usage") if isinstance(msg.get("usage"), dict) else None
        if usage:
            self.in_tok += _usage_in(usage)
            out = usage.get("output_tokens")
            if isinstance(out, (int, float)):
                self.out_tok += int(out)
        content = msg.get("content")
        if isinstance(content, list):
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    self.last_tool = str(block.get("name", "?"))
        cost = _event_cost(ev)
        if cost is not None:
            if ev.get("type") == "result":
                self.cost_reported = cost
            else:
                self.cost_sum += cost


# ---------------------------------------------------------------- claude source


class Snap:
    """One session as seen by a source at one scan."""

    def __init__(self, key, label, argv, state, last_ts):
        self.key = key
        self.label = label
        self.argv = argv          # `hermon render …` argv for this session's pane
        self.state = state        # "live" | "done"
        self.last_ts = last_ts


def scan_claude_root(root):
    try:
        return sorted(Path(root).glob("*/*.jsonl"))
    except OSError:
        return []


class ClaudeSource:
    def __init__(self, root):
        self.root = Path(root).expanduser()

    def snapshot(self, now, idle_timeout, tracked_keys=()):
        snaps = {}
        for f in scan_claude_root(self.root):
            key = f"claude:{f}"
            try:
                mtime = os.path.getmtime(f)
            except OSError:
                continue
            if now - mtime <= idle_timeout:
                state = "live"
            elif key in tracked_keys or now - mtime <= idle_timeout * 5:
                # only pay for an lsof call where the answer could matter
                state = "live" if has_open_handle(f) else "done"
            else:
                state = "done"
            snaps[key] = Snap(
                key, f"C:{short_id(f.stem)}",
                ["render", str(f)], state, mtime,
            )
        return snaps


# ---------------------------------------------------------------- hermes source


def hermes_connect(db_path):
    return sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=2)


class HermesSource:
    def __init__(self, db_path):
        self.db_path = str(Path(db_path).expanduser())
        self._warned = False

    def sessions(self, since):
        """Recent/unfinished session rows as dicts; [] on any error."""
        try:
            conn = hermes_connect(self.db_path)
        except sqlite3.Error:
            self._warn()
            return []
        try:
            rows = conn.execute(
                """
                SELECT s.id, s.started_at, s.ended_at, s.model, s.title,
                       s.input_tokens, s.output_tokens,
                       s.cache_read_tokens, s.cache_write_tokens,
                       COALESCE(s.actual_cost_usd, s.estimated_cost_usd),
                       COALESCE((SELECT MAX(m.timestamp) FROM messages m
                                 WHERE m.session_id = s.id), s.started_at)
                FROM sessions s
                WHERE s.started_at >= ? OR s.ended_at IS NULL
                """,
                (since,),
            ).fetchall()
        except sqlite3.Error:
            self._warn()
            return []
        finally:
            conn.close()
        out = []
        for r in rows:
            out.append({
                "id": r[0], "started_at": r[1] or 0, "ended_at": r[2],
                "model": r[3] or "?", "title": r[4] or "",
                "in_tok": int((r[5] or 0) + (r[7] or 0) + (r[8] or 0)),
                "out_tok": int(r[6] or 0),
                "cost": r[9],
                "last_ts": r[10] or r[1] or 0,
            })
        return out

    def last_tool(self, session_id):
        try:
            conn = hermes_connect(self.db_path)
            row = conn.execute(
                "SELECT tool_name FROM messages WHERE session_id = ?"
                " AND tool_name IS NOT NULL ORDER BY id DESC LIMIT 1",
                (session_id,),
            ).fetchone()
            conn.close()
            return row[0] if row else "-"
        except sqlite3.Error:
            return "-"

    def snapshot(self, now, idle_timeout, tracked_keys=()):
        snaps = {}
        for s in self.sessions(now - idle_timeout * 5):
            key = f"hermes:{s['id']}"
            if s["ended_at"]:
                state = "done"
            else:
                state = "live" if now - s["last_ts"] <= idle_timeout else "done"
            snaps[key] = Snap(
                key, f"H:{short_id(s['id'])}",
                ["render", "--hermes", s["id"], "--hermes-db", self.db_path],
                state, s["last_ts"],
            )
        return snaps

    def _warn(self):
        if not self._warned:
            self._warned = True
            print(c(DIM, f"· hermes db unavailable: {self.db_path}"), flush=True)


# ---------------------------------------------------------------- hermes message renderer


def render_hermes_row(role, content, tool_calls, tool_name, width=None):
    """Render one Hermes messages-table row to printable lines. Never raises."""
    width = width or term_width()
    out = []
    role = role or "?"

    if role == "assistant":
        if content:
            for ln in textwrap.wrap(str(content).strip(), width):
                out.append(ln)
        if tool_calls:
            try:
                calls = json.loads(tool_calls) if isinstance(tool_calls, str) else tool_calls
            except Exception:
                calls = []
            if isinstance(calls, list):
                for call in calls:
                    if not isinstance(call, dict):
                        continue
                    fn = call.get("function") if isinstance(call.get("function"), dict) else {}
                    name = fn.get("name") or call.get("name") or "?"
                    args = fn.get("arguments") or ""
                    out.append(c(BOLD, f"▶ {name}") + " " + c(DIM, clip(args, 120)))

    elif role == "tool":
        text = str(content or "")
        is_error = False
        try:  # hermes tool results are often JSON: {"output":…,"exit_code":…,"error":…}
            parsed = json.loads(text)
            if isinstance(parsed, dict):
                err = parsed.get("error")
                code = parsed.get("exit_code")
                is_error = bool(err) or (code not in (0, None))
                text = str(err) if err else str(parsed.get("output", text))
        except Exception:
            pass
        label = f"◀ {tool_name or 'result'}"
        if is_error:
            out.append(c(RED, "◀ ERROR") + " " + c(DIM, clip(text, 200)))
        else:
            out.append(c(DIM, f"{label} {clip(text, 200)}"))

    elif role == "user":
        if content:
            out.extend(wrap_prefixed(c(YELLOW, "» user:"), str(content), width))

    elif role == "system":
        out.append(c(DIM, "· system"))

    else:
        out.append(c(DIM, f"· {role}"))

    return out


def cmd_render_hermes(session_id, db_path, replay_msgs):
    print(c(DIM, f"hermon · hermes session {session_id} ({db_path})"), flush=True)
    last_id = None
    last_stats = None
    warned = False
    tick = 0
    while True:
        try:
            conn = hermes_connect(db_path)
        except sqlite3.Error:
            if not warned:
                print(c(DIM, "· hermes db unavailable — waiting"), flush=True)
                warned = True
            time.sleep(1)
            continue
        warned = False
        try:
            if last_id is None:
                row = conn.execute(
                    "SELECT MIN(id) FROM (SELECT id FROM messages WHERE session_id = ?"
                    " ORDER BY id DESC LIMIT ?)",
                    (session_id, max(replay_msgs, 1)),
                ).fetchone()
                last_id = (row[0] - 1) if row and row[0] is not None else 0

            rows = conn.execute(
                "SELECT id, role, content, tool_calls, tool_name FROM messages"
                " WHERE session_id = ? AND id > ? ORDER BY id LIMIT 500",
                (session_id, last_id),
            ).fetchall()
            for mid, role, content, tool_calls, tool_name in rows:
                last_id = mid
                for ln in render_hermes_row(role, content, tool_calls, tool_name):
                    print(ln, flush=True)

            tick += 1
            if tick % 4 == 1:  # stats every ~2s
                srow = conn.execute(
                    "SELECT model, input_tokens, output_tokens, cache_read_tokens,"
                    " cache_write_tokens, COALESCE(actual_cost_usd, estimated_cost_usd),"
                    " ended_at FROM sessions WHERE id = ?",
                    (session_id,),
                ).fetchone()
                if srow:
                    stats = tuple(srow[:6])
                    if stats != last_stats and any(v for v in srow[1:6]):
                        last_stats = stats
                        in_tok = int((srow[1] or 0) + (srow[3] or 0) + (srow[4] or 0))
                        line = f"Σ in:{in_tok:,} out:{int(srow[2] or 0):,}"
                        if srow[5] is not None:
                            line += f" ${srow[5]:.4f}"
                        line += f"  [{srow[0] or '?'}]"
                        print(c(CYAN, line), flush=True)
                    if srow[6]:  # ended_at set: session closed cleanly
                        print(c(GREEN, f"Σ session ended"), flush=True)
                        conn.close()
                        return
        except sqlite3.Error:
            print(c(DIM, "· hermes db read error — retrying"), flush=True)
            time.sleep(1)
        finally:
            conn.close()
        time.sleep(0.5)


# ---------------------------------------------------------------- roster / summary

API_CALL_RE = re.compile(
    r"(\d\d:\d\d:\d\d),\d+ \S+ \[\S*?(\w{6})\] agent\.conversation_loop: "
    r"API call #(\d+): model=(\S+) provider=(\S+) in=(\d+) out=(\d+)"
    r".*?latency=([\d.]+s)"
)


def api_call_ticker(log_path, limit=4):
    """Last few Hermes API calls (covers small/auxiliary traffic), from agent.log."""
    try:
        with open(log_path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - 65536))
            tail = f.read().decode("utf-8", "replace")
    except OSError:
        return []
    hits = API_CALL_RE.findall(tail)[-limit:]
    lines = []
    for hh, sid, n, model, provider, i, o, lat in hits:
        lines.append(c(DIM,
            f"  {hh} {sid} #{n:>3} {model}@{provider} in={int(i):,} out={int(o):,} {lat}"))
    return lines


class RosterRow:
    def __init__(self, label, state, model, last_tool, in_tok, out_tok, cost,
                 elapsed, last_ts, title=""):
        self.label = label
        self.state = state
        self.model = model
        self.last_tool = last_tool
        self.in_tok = in_tok
        self.out_tok = out_tok
        self.cost = cost
        self.elapsed = elapsed
        self.last_ts = last_ts
        self.title = title


def roster_lines(rows, ticker):
    rows = sorted(rows, key=lambda r: r.last_ts or 0, reverse=True)
    lines = [
        c(BOLD, f"hermon · {len(rows)} session(s) · "
                f"{datetime.now().strftime('%H:%M:%S')}"),
        c(DIM, f"{'':2}{'id':<10}{'model':<24}{'last tool':<16}"
               f"{'in':>12}{'out':>9}{'cost':>9}{'elapsed':>9}  title"),
    ]
    for r in rows:
        icon = c(GREEN, "●") if r.state == "live" else c(DIM, "✓")
        cost = f"{r.cost:.4f}" if isinstance(r.cost, (int, float)) else "-"
        lines.append(
            f"{icon} {r.label:<10}"
            f"{clip(r.model, 23):<24}"
            f"{clip(r.last_tool, 15):<16}"
            f"{r.in_tok:>12,}{r.out_tok:>9,}"
            f"{cost:>9}"
            f"{fmt_elapsed(r.elapsed):>9}"
            f"  {c(DIM, clip(r.title, 40))}"
        )
    if not rows:
        lines.append(c(DIM, "  (no sessions in window — waiting)"))
    if ticker:
        lines.append("")
        lines.append(c(DIM, "  recent hermes API calls:"))
        lines.extend(ticker)
    return lines


def build_roster(sources, claude_stats, now, fresh_window, idle_timeout):
    rows = []
    for src in sources:
        if isinstance(src, ClaudeSource):
            snaps = src.snapshot(now, idle_timeout)
            for key, snap in snaps.items():
                if now - snap.last_ts > fresh_window and snap.state != "live":
                    claude_stats.pop(key, None)
                    continue
                st = claude_stats.get(key)
                if st is None:
                    st = claude_stats[key] = ClaudeStats(key.split(":", 1)[1])
                st.update()
                rows.append(RosterRow(
                    snap.label, snap.state, st.model, st.last_tool,
                    st.in_tok, st.out_tok,
                    st.cost if (st.cost_reported is not None or st.cost_sum) else None,
                    st.elapsed, snap.last_ts,
                ))
        elif isinstance(src, HermesSource):
            for s in src.sessions(now - fresh_window):
                if s["ended_at"]:
                    state = "done"
                else:
                    state = "live" if now - s["last_ts"] <= idle_timeout else "done"
                if state != "live" and now - s["last_ts"] > fresh_window:
                    continue
                rows.append(RosterRow(
                    f"H:{short_id(s['id'])}", state, s["model"],
                    src.last_tool(s["id"]),
                    s["in_tok"], s["out_tok"], s["cost"],
                    s["last_ts"] - s["started_at"] if s["started_at"] else None,
                    s["last_ts"], s["title"],
                ))
    return rows


def cmd_summary(sources, hermes_log, interval, fresh_window, idle_timeout, once=False):
    claude_stats = {}
    while True:
        now = time.time()
        rows = build_roster(sources, claude_stats, now, fresh_window, idle_timeout)
        ticker = api_call_ticker(hermes_log) if hermes_log else []
        body = "\n".join(roster_lines(rows, ticker))
        if once:
            print(body)
            return 0
        sys.stdout.write("\033[H\033[2J" + body + "\n")
        sys.stdout.flush()
        time.sleep(interval)


# ---------------------------------------------------------------- watch daemon (tmux panes)


def tmux(*args):
    return subprocess.run(["tmux", *args], capture_output=True, text=True, check=False)


def self_cmd(*args):
    py = sys.executable or "python3"
    return shlex.join([py, str(Path(__file__).resolve()), *args])


def bare_title(title):
    """Strip the done-marker prefix; a non-UTF-8 tmux stores ✓ as _."""
    return title.lstrip("✓_ ")


class Tracked:
    def __init__(self, key, label, argv, pane_id, state="live"):
        self.key = key
        self.label = label
        self.argv = argv
        self.pane_id = pane_id
        self.state = state
        self.finished_at = None
        self.killed = False


class Deck:
    """Owns the tmux session: one window, roster pane + a pane per session."""

    def __init__(self, args, source_flags):
        self.session = args.session
        self.target = f"{args.session}:{WINDOW}"
        self.max_panes = args.max_panes
        self.roster_cmd = self_cmd(
            "render", "--summary",
            "--interval", str(args.interval),
            "--fresh-window", str(args.fresh_window),
            "--idle-timeout", str(args.idle_timeout),
            *source_flags,
        )

    def ensure_session(self):
        """Create tmux session + roster pane if absent. True if created."""
        if tmux("has-session", "-t", self.session).returncode == 0:
            return False
        r = tmux("new-session", "-d", "-s", self.session, "-n", WINDOW,
                 self.roster_cmd)
        if r.returncode != 0:
            sys.exit(f"hermon: failed to create tmux session: {r.stderr.strip()}")
        tmux("select-pane", "-t", f"{self.target}.0", "-T", ROSTER_TITLE)
        tmux("set-option", "-w", "-t", self.target, "pane-border-status", "top")
        tmux("set-option", "-w", "-t", self.target,
             "pane-border-format", " #{pane_title} ")
        return True

    def panes(self):
        """Map bare pane title -> (pane_id, is_done)."""
        r = tmux("list-panes", "-t", self.target,
                 "-F", "#{pane_id}\t#{pane_title}")
        out = {}
        if r.returncode != 0:
            return out
        for line in r.stdout.splitlines():
            if "\t" not in line:
                continue
            pid, title = line.split("\t", 1)
            bare = bare_title(title)
            out[bare] = (pid, title != bare)
        return out

    def ensure_roster(self, panes):
        if ROSTER_TITLE in panes:
            return
        r = tmux("split-window", "-d", "-t", self.target,
                 "-P", "-F", "#{pane_id}", self.roster_cmd)
        pid = r.stdout.strip()
        if pid:
            tmux("select-pane", "-t", pid, "-T", ROSTER_TITLE)
            self.retile()

    def split(self, label, argv):
        r = tmux("split-window", "-d", "-t", self.target,
                 "-P", "-F", "#{pane_id}", self_cmd(*argv))
        pid = r.stdout.strip()
        if r.returncode != 0 or not pid:
            # e.g. "no space for new pane" — retile and let next tick retry
            self.retile()
            return None
        tmux("select-pane", "-t", pid, "-T", label)
        self.retile()
        return pid

    def kill(self, pane_id):
        tmux("kill-pane", "-t", pane_id)
        self.retile()

    def rename(self, pane_id, title):
        tmux("select-pane", "-t", pane_id, "-T", title)

    def retile(self):
        tmux("select-layout", "-t", self.target, "tiled")

    def session_pane_count(self):
        return max(len(self.panes()) - 1, 0)  # minus roster


def build_sources(args):
    sources = []
    if not args.no_claude:
        root = Path(args.claude_root).expanduser()
        if root.is_dir():
            sources.append(ClaudeSource(root))
        else:
            print(c(DIM, f"· claude root missing, skipping: {root}"), flush=True)
    if not args.no_hermes:
        db = Path(args.hermes_db).expanduser()
        if db.exists():
            sources.append(HermesSource(db))
        else:
            print(c(DIM, f"· hermes db missing, skipping: {db}"), flush=True)
    if not sources:
        sys.exit("hermon: no sources available (claude root and hermes db both missing)")
    return sources


def source_flags(args):
    flags = ["--claude-root", str(args.claude_root),
             "--hermes-db", str(args.hermes_db),
             "--hermes-log", str(args.hermes_log)]
    if args.no_claude:
        flags.append("--no-claude")
    if args.no_hermes:
        flags.append("--no-hermes")
    return flags


def cmd_watch(args):
    if not shutil.which("tmux"):
        sys.exit("hermon: tmux not found on PATH — install tmux first")
    sources = build_sources(args)
    deck = Deck(args, source_flags(args))
    tracked = {}  # key -> Tracked
    log = lambda s: print(s, flush=True)

    created = deck.ensure_session()
    log(f"hermon: watching {len(sources)} source(s) → tmux session "
        f"'{args.session}'{' (created)' if created else ' (adopted)'}")
    log(f"hermon: attach with:  tmux -CC attach -t {shlex.quote(args.session)}"
        f"   (iTerm2)  or  tmux attach -t {shlex.quote(args.session)}")
    if not LSOF:
        log("hermon: lsof not found — mtime-only liveness for claude transcripts")

    while True:
        if deck.ensure_session():
            log("hermon: tmux session recreated")
            for t in tracked.values():
                t.pane_id = None
                t.killed = False

        panes = deck.panes()
        deck.ensure_roster(panes)
        now = time.time()

        snaps = {}
        tracked_keys = set(tracked)
        for src in sources:
            snaps.update(src.snapshot(now, args.idle_timeout, tracked_keys))

        # -- discovery: pane for every live session (adopt survivors first)
        for key, snap in snaps.items():
            if key in tracked:
                continue
            adopted = panes.get(snap.label)
            if adopted:
                pid, done = adopted
                tracked[key] = Tracked(key, snap.label, snap.argv, pid,
                                       state="done" if done else "live")
                if done:
                    tracked[key].finished_at = now
                log(f"hermon: ~ adopted pane {snap.label}")
                continue
            if snap.state != "live":
                continue  # never open panes for already-finished history
            if deck.session_pane_count() >= args.max_panes:
                if not self_evict(deck, tracked):
                    continue  # full of live panes; roster still shows it
            pid = deck.split(snap.label, snap.argv)
            if pid:
                tracked[key] = Tracked(key, snap.label, snap.argv, pid)
                log(f"hermon: + pane {snap.label}")

        # -- lifecycle
        for key, t in list(tracked.items()):
            snap = snaps.get(key)
            state = snap.state if snap else "done"
            if t.state == "live" and state == "done":
                t.state = "done"
                t.finished_at = now
                if t.pane_id:
                    deck.rename(t.pane_id, f"✓{t.label}")
                log(f"hermon: ✓ {t.label} finished")
            elif t.state == "done" and state == "live":
                t.state = "live"
                t.finished_at = None
                t.killed = False
                if t.pane_id and t.label in {bare_title(k) for k in panes}:
                    deck.rename(t.pane_id, t.label)
                else:
                    t.pane_id = deck.split(t.label, t.argv)
                log(f"hermon: ● {t.label} resumed")
            if (t.state == "done" and not t.killed and args.linger > 0
                    and t.finished_at is not None
                    and now - t.finished_at >= args.linger):
                if t.pane_id:
                    deck.kill(t.pane_id)
                t.killed = True
                t.pane_id = None
                log(f"hermon: × {t.label} unsplit after linger")
            if t.killed and snap is None:
                del tracked[key]  # gone from sources too; forget it

        time.sleep(args.interval)


def self_evict(deck, tracked):
    """Kill the oldest finished pane to make room. True if space was made."""
    done = [t for t in tracked.values() if t.state == "done" and t.pane_id]
    if not done:
        return False
    victim = min(done, key=lambda t: t.finished_at or 0)
    deck.kill(victim.pane_id)
    victim.pane_id = None
    victim.killed = True
    return True


# ---------------------------------------------------------------- CLI


def add_source_flags(p):
    p.add_argument("--claude-root", default=DEFAULT_CLAUDE_ROOT,
                   help="Claude Code transcript root")
    p.add_argument("--hermes-db", default=DEFAULT_HERMES_DB,
                   help="Hermes state.db path")
    p.add_argument("--hermes-log", default=DEFAULT_HERMES_LOG,
                   help="Hermes agent.log (roster API-call ticker)")
    p.add_argument("--no-claude", action="store_true",
                   help="disable the Claude transcript source")
    p.add_argument("--no-hermes", action="store_true",
                   help="disable the Hermes db source")


def build_parser():
    p = argparse.ArgumentParser(
        prog="hermon",
        description="Live tmux monitor deck for Hermes and Claude Code sessions.",
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    w = sub.add_parser("watch", help="run the watcher daemon (owns the tmux session)")
    add_source_flags(w)
    w.add_argument("--session", default=DEFAULT_SESSION, help="tmux session name")
    w.add_argument("--interval", type=float, default=1.0, help="scan interval (s)")
    w.add_argument("--fresh-window", type=float, default=300.0,
                   help="roster lookback for recently-finished sessions (s)")
    w.add_argument("--idle-timeout", type=float, default=60.0,
                   help="no activity for this long = session finished")
    w.add_argument("--linger", type=float, default=60.0,
                   help="keep finished panes this long before unsplitting; 0 = forever")
    w.add_argument("--max-panes", type=int, default=8,
                   help="max session panes (finished panes evicted first)")

    r = sub.add_parser("render", help="tail one session (or --summary roster)")
    add_source_flags(r)
    r.add_argument("file", nargs="?", help="claude transcript .jsonl to tail")
    r.add_argument("--hermes", metavar="SESSION_ID",
                   help="hermes session id to tail from state.db")
    r.add_argument("--replay-bytes", type=int, default=20480,
                   help="claude transcript history shown on open")
    r.add_argument("--replay-msgs", type=int, default=30,
                   help="hermes message history shown on open")
    r.add_argument("--summary", action="store_true",
                   help="roster mode: live table of all sessions")
    r.add_argument("--interval", type=float, default=1.0)
    r.add_argument("--fresh-window", type=float, default=300.0)
    r.add_argument("--idle-timeout", type=float, default=60.0)

    l = sub.add_parser("ls", help="print the roster once to stdout (no tmux)")
    add_source_flags(l)
    l.add_argument("--fresh-window", type=float, default=3600.0,
                   help="include sessions active within this many seconds")
    l.add_argument("--idle-timeout", type=float, default=60.0)

    return p


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        if args.cmd == "watch":
            cmd_watch(args)
        elif args.cmd == "render":
            if args.summary:
                return cmd_summary(
                    build_sources(args),
                    None if args.no_hermes else args.hermes_log,
                    args.interval, args.fresh_window, args.idle_timeout,
                )
            if args.hermes:
                cmd_render_hermes(args.hermes, args.hermes_db, args.replay_msgs)
            elif args.file:
                cmd_render_claude(args.file, args.replay_bytes)
            else:
                sys.exit("hermon render: FILE, --hermes ID, or --summary required")
        elif args.cmd == "ls":
            return cmd_summary(
                build_sources(args),
                None if args.no_hermes else args.hermes_log,
                0, args.fresh_window, args.idle_timeout, once=True,
            )
    except KeyboardInterrupt:
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
