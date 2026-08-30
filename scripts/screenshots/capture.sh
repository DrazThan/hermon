#!/usr/bin/env bash
# Drive `hermon watch` inside tmux, capture list/grid/zoom/help views, and
# rasterise each ANSI dump to a PNG.  Writes assets into the repo's assets/.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
DEMO="$REPO/scripts/screenshots/.demo"
OUT="$REPO/assets"
PY="$REPO/.venv-screenshots/bin/python"
BIN="$REPO/target/release/hermon"

SOCK="hermon-shot"
WIDTH=150
HEIGHT=42

mkdir -p "$OUT"

# -- build the binary if missing --
if [[ ! -x "$BIN" ]]; then
  (cd "$REPO" && cargo build --release)
fi

# -- regenerate fixtures so timestamps are fresh right now --
"$PY" "$REPO/scripts/screenshots/fixtures.py" >/dev/null

# -- fresh tmux server, sized window, app running inside --
tmux -L "$SOCK" kill-server 2>/dev/null || true
tmux -L "$SOCK" new-session -d -s shot -x "$WIDTH" -y "$HEIGHT"
tmux -L "$SOCK" set-option -g default-terminal "xterm-256color" 2>/dev/null || true

CMD="cd $REPO && COLORTERM=truecolor TERM=xterm-256color exec $BIN watch \
  --claude-dir '$DEMO/claude/projects' \
  --hermes-db '$DEMO/hermes/state.db' \
  --opencode-db '$DEMO/opencode/opencode.db' \
  --hermes-log '$DEMO/hermes/agent.log' \
  --linger 120"
tmux -L "$SOCK" send-keys -t shot "$CMD" Enter

# let the engine scan sources and paint the first frame
sleep 4

shot() { # $1 = filename stem
  tmux -L "$SOCK" capture-pane -p -e -t shot > "$OUT/$1.ansi"
  "$PY" "$REPO/scripts/screenshots/ansi2png.py" "$OUT/$1.ansi" "$OUT/$1.png" --size 22
  rm -f "$OUT/$1.ansi"
  echo "  $1.png"
}

echo "capturing…"
shot screenshot-list          # roster + preview
tmux -L "$SOCK" send-keys -t shot l; sleep 0.6
shot screenshot-grid          # tiled panes
# zoom into the Hermes live session (4th row) for a fuller transcript
tmux -L "$SOCK" send-keys -t shot j; sleep 0.2
tmux -L "$SOCK" send-keys -t shot j; sleep 0.2
tmux -L "$SOCK" send-keys -t shot j; sleep 0.2
tmux -L "$SOCK" send-keys -t shot Enter; sleep 0.6
shot screenshot-zoom          # zoomed transcript
tmux -L "$SOCK" send-keys -t shot Escape; sleep 0.6
tmux -L "$SOCK" send-keys -t shot l; sleep 0.6
tmux -L "$SOCK" send-keys -t shot '?'; sleep 0.6
shot screenshot-help          # keybindings overlay

tmux -L "$SOCK" kill-server 2>/dev/null || true
echo "done → $OUT"
