# Remoterm Spec (v0.1)

Date: 2026-03-06

## 1) Product goal

Build a Rust-native alternative to `ttyd` that supports persistent, long-running terminal sessions across devices, with strong mobile usability (Termux-like extra keyboard) and multi-session management from a sidebar.

Primary workload: interactive coding with long-lived agent processes (`claude`, `codex`, shells, editors, compilers).

## 2) Non-goals (v0)

- Full collaborative CRDT terminal editing.
- Full SSH bastion/reverse proxy replacement.
- Multi-tenant enterprise RBAC beyond basic user/session ACL.
- Guaranteed replay of every byte forever (bounded history in v0).

## 3) User stories

- As a developer, I can create named sessions (`backend`, `frontend`, `infra`) and switch quickly.
- If my phone/laptop disconnects, session processes keep running and I can reattach later.
- On mobile, I can use Ctrl/Alt/Tab/Esc/arrows/F-keys reliably.
- I can open several sessions in a sidebar and see status at a glance.
- I can reconnect from a second device and continue exactly where I left off.

## 4) Functional requirements

- Session CRUD (create/list/rename/archive/delete).
- Persistent PTY-backed process per session (independent from active clients).
- Multiple concurrent websocket clients per session.
- Session list with metadata:
  - name
  - cwd
  - command
  - status (running/exited)
  - last activity
  - attached clients count
- Terminal attach protocol:
  - handshake
  - output stream
  - input events
  - resize
  - heartbeat
  - resumable cursor/sequence for output replay.
- Mobile keyboard overlay:
  - sticky Ctrl/Alt/Shift
  - Esc, Tab
  - arrows, Home, End, PgUp, PgDn
  - function row toggle
  - configurable key rows.
- Session persistence policy:
  - survive client disconnect
  - configurable idle/TTL cleanup.

## 5) Non-functional requirements

- Low-latency input/output path (<50ms LAN p95 target).
- Efficient fan-out to multiple clients.
- Graceful recovery on server restart.
- Strong auditability: structured logs + session lifecycle events.
- Security defaults:
  - loopback bind by default
  - authentication required in non-local mode
  - CSRF/origin controls for websocket and API.

## 6) `libghostty` integration strategy

## Reality check

- `libghostty` crate (`https://crates.io/crates/libghostty`) is yanked.
- Current viable crates are `ghostty-sys` and `ghostty`.
- `ghostty-sys` requires external dynamic `libghostty` availability via `GHOSTTY_LOCATION`.
- API and embedding surface are evolving.

## Decision

Use a feature-gated terminal engine abstraction:

- `engine_ghostty` (feature: `ghostty`)
  - uses `ghostty-sys`/`ghostty` where platform/runtime constraints are met.
  - ideal for native app surfaces and future richer rendering paths.
- `engine_vt` (default)
  - stable Linux server path using PTY + VT parser model for replay/history.
  - production fallback when ghostty runtime is unavailable.

This keeps the project buildable today while still aligning to your `libghostty` direction.

## 7) High-level architecture

`remoterm-server` (single binary in v0)

- API layer (HTTP + WS): `axum`
- Session manager:
  - owns session registry
  - spawn/stop/reap
  - attach/detach clients
- PTY runtime:
  - per-session PTY process + reader/writer tasks
  - resize/input channel
- Output ring:
  - bounded byte/event buffer for resumable reattach
- Storage:
  - SQLite metadata + event checkpoints
- Auth:
  - local token auth in v0
  - room for OIDC/reverse-proxy auth later.

## 8) Session lifecycle model

1. `POST /api/sessions` creates session metadata.
2. Server spawns command (default shell) with PTY.
3. Session enters `running` state.
4. Clients attach via websocket.
5. If all clients detach, PTY continues running.
6. On process exit, session state records exit code/time.
7. Session can be restarted from UI/API.

## 9) API spec (v0)

## REST

- `GET /healthz`
- `GET /api/sessions`
- `POST /api/sessions`
  - body: `{name, cwd, shell, args?}`
- `GET /api/sessions/:id`
- `PATCH /api/sessions/:id`
- `DELETE /api/sessions/:id`
- `POST /api/sessions/:id/restart`
- `POST /api/sessions/:id/archive`

## WebSocket

Endpoint: `/ws/:session_id`

Framing: JSON text frames for control + base64 or binary for raw IO (v0 uses JSON; v1 can move hot path to binary).

Client frames:

- `hello {resume_from_seq?, cols, rows, capabilities}`
- `input {data_b64}`
- `resize {cols, rows}`
- `keyboard {action, payload}` (mobile soft-keys, modifiers)
- `ping {ts_ms}`

Server frames:

- `hello_ack {session, protocol_version, next_seq}`
- `snapshot {history_from_seq, chunks[]}`
- `output {seq, data_b64}`
- `status {running, pid, attached_clients}`
- `session_updated {...}`
- `pong {ts_ms}`
- `error {code, message}`

## 10) Persistence model

SQLite tables:

- `sessions`
  - `id TEXT PRIMARY KEY`
  - `name TEXT`
  - `cwd TEXT`
  - `shell TEXT`
  - `args_json TEXT`
  - `status TEXT`
  - `pid INTEGER NULL`
  - `created_at TEXT`
  - `updated_at TEXT`
  - `last_activity_at TEXT`
  - `exit_code INTEGER NULL`
- `session_events`
  - `id INTEGER PRIMARY KEY AUTOINCREMENT`
  - `session_id TEXT`
  - `seq INTEGER`
  - `kind TEXT`
  - `payload BLOB`
  - `created_at TEXT`

In-memory:

- active session map
- output ring buffer per session (size configurable, default 8MB/session).

## 11) Mobile keyboard spec (Termux-like)

## Layout

Default extra row:

- Row 1: `Ctrl` `Alt` `Esc` `Tab` `|` `/` `-` `_` `:` `;` `"` `'`
- Row 2: `↑` `↓` `←` `→` `Home` `End` `PgUp` `PgDn` `Ins` `Del`
- Row 3 (toggle): `F1..F12`

## Interaction

- Modifier keys support:
  - tap for one-shot modifier
  - double-tap to lock
  - visible lock state badge.
- Long press on arrow keys repeats at configurable rate.
- Haptic feedback optional (mobile).
- Keyboard survives reconnect and orientation changes.

## Transport

- Soft-key presses translate to terminal input bytes/events server-side.
- Keep mapping table explicit and testable (`key -> bytes`).

## 12) Sidebar sessions UX

- Left sidebar with:
  - running indicator
  - unread output badge while unfocused
  - active session highlight
  - quick create button.
- Middle pane terminal for active session.
- Mobile:
  - collapsible drawer
  - swipe-to-switch optional (v1).

## 13) Security model (v0)

- Auth modes:
  - single-user token
  - reverse-proxy forwarded user header (trusted network only)
- Websocket origin check enabled by default.
- Optional readonly attachment mode.
- Rate limits on session creation and websocket attach attempts.

## 14) Observability

- Structured logs (`tracing`) with `session_id`, `client_id`.
- Metrics (Prometheus):
  - active sessions
  - connected clients
  - bytes in/out
  - reconnect count
  - process restarts
  - pty write latency.

## 15) Delivery roadmap

Phase 0 (1 week): Foundations

- workspace scaffold
- protocol crate
- in-memory session manager
- health + session CRUD APIs

Phase 1 (2-3 weeks): Persistent PTY sessions

- PTY spawn + IO fan-out
- attach/detach semantics
- output ring replay
- reconnect/resume handling

Phase 2 (2 weeks): Web client + sidebar + mobile keyboard

- session sidebar UI
- terminal pane + reconnect UX
- Termux-like keyboard row with modifiers

Phase 3 (1-2 weeks): Hardening

- SQLite durability
- auth hardening
- metrics/log polish
- soak tests for long-running sessions

Phase 4 (parallel R&D): `ghostty` engine integration

- optional `ghostty` feature
- runtime detection and fallback
- compatibility matrix + benchmarks.

## 16) Risks and mitigations

- Risk: `ghostty` API/runtime instability.
  - Mitigation: feature-gated engine trait + default stable engine.
- Risk: memory growth for long sessions.
  - Mitigation: bounded ring buffers + periodic checkpoints.
- Risk: mobile browser keyboard inconsistencies.
  - Mitigation: explicit soft-key UI + one-shot/lock modifiers.
- Risk: process orphaning on crashes.
  - Mitigation: supervisor state restore and kill/reclaim sweep.

## 17) Acceptance criteria for v0.1

- Can run server, create at least 3 named sessions, and switch among them.
- Sessions survive websocket disconnect/reconnect from a different device.
- Mobile keyboard can send Ctrl+C, Ctrl+R, arrows, Tab, Esc reliably.
- Long-running command (`sleep`, `npm test --watch`, AI CLI REPL) remains alive while client disconnects.

