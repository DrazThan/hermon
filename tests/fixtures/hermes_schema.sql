-- Hermes state.db schema fixture.
--
-- Captured live, read-only, schema only (no rows) via:
--   sqlite3 "file:~/.hermes/state.db?mode=ro" .schema
--
-- Trimmed to the tables hermon.py reads (HermesSource, hermon.py:530):
-- `sessions` and `messages`, plus their indexes. The live database has
-- several other tables (schema_version, state_meta, gateway_routing,
-- compression_locks, messages_fts*, async_delegations,
-- session_model_usage) that hermon never queries; they are omitted here
-- for a smaller, faithful fixture.
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    user_id TEXT,
    session_key TEXT,
    chat_id TEXT,
    chat_type TEXT,
    thread_id TEXT,
    display_name TEXT,
    origin_json TEXT,
    expiry_finalized INTEGER DEFAULT 0,
    model TEXT,
    model_config TEXT,
    system_prompt TEXT,
    parent_session_id TEXT,
    started_at REAL NOT NULL,
    ended_at REAL,
    end_reason TEXT,
    message_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0,
    cache_write_tokens INTEGER DEFAULT 0,
    reasoning_tokens INTEGER DEFAULT 0,
    cwd TEXT,
    git_branch TEXT,
    git_repo_root TEXT,
    billing_provider TEXT,
    billing_base_url TEXT,
    billing_mode TEXT,
    estimated_cost_usd REAL,
    actual_cost_usd REAL,
    cost_status TEXT,
    cost_source TEXT,
    pricing_version TEXT,
    title TEXT,
    api_call_count INTEGER DEFAULT 0,
    handoff_state TEXT,
    handoff_platform TEXT,
    handoff_error TEXT,
    compression_failure_cooldown_until REAL,
    compression_failure_error TEXT,
    rewind_count INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0, "compression_fallback_streak" INTEGER NOT NULL DEFAULT 0, "profile_name" TEXT, "compression_ineffective_count" INTEGER NOT NULL DEFAULT 0, "pinned" INTEGER NOT NULL DEFAULT 0, "last_activity_at" REAL, "last_activity_description" TEXT, "last_activity_provenance" TEXT,
    FOREIGN KEY (parent_session_id) REFERENCES sessions(id)
);
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role TEXT NOT NULL,
    content TEXT,
    tool_call_id TEXT,
    tool_calls TEXT,
    tool_name TEXT,
    timestamp REAL NOT NULL,
    token_count INTEGER,
    finish_reason TEXT,
    reasoning TEXT,
    reasoning_content TEXT,
    reasoning_details TEXT,
    codex_reasoning_items TEXT,
    codex_message_items TEXT,
    platform_message_id TEXT,
    observed INTEGER DEFAULT 0,
    active INTEGER NOT NULL DEFAULT 1,
    compacted INTEGER NOT NULL DEFAULT 0
, "effect_disposition" TEXT, "api_content" TEXT, "display_kind" TEXT, "display_metadata" TEXT);
CREATE INDEX idx_sessions_source ON sessions(source);
CREATE INDEX idx_sessions_source_id ON sessions(source, id);
CREATE INDEX idx_sessions_parent ON sessions(parent_session_id);
CREATE INDEX idx_sessions_started ON sessions(started_at DESC);
CREATE INDEX idx_messages_session ON messages(session_id, timestamp);
CREATE INDEX idx_messages_platform_msg_id ON messages(session_id, platform_message_id) WHERE platform_message_id IS NOT NULL;
CREATE INDEX idx_messages_session_active
    ON messages(session_id, active, timestamp);
CREATE INDEX idx_sessions_session_key
    ON sessions(session_key, started_at DESC);
CREATE INDEX idx_sessions_gateway_peer
    ON sessions(source, user_id, chat_id, chat_type, thread_id, started_at DESC);
CREATE INDEX idx_sessions_handoff_state
    ON sessions(handoff_state, started_at);
CREATE UNIQUE INDEX idx_sessions_title_unique ON sessions(title) WHERE title IS NOT NULL;
CREATE INDEX idx_messages_active_null
    ON messages(active) WHERE active IS NULL;
CREATE INDEX idx_messages_session_id ON messages(session_id, id);
