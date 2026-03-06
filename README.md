# Remoterm

Persistent, multi-session remote terminal server. A `ttyd` alternative built in Rust for long-running coding agent workflows.

![Remoterm UI — desktop with btop](static/screenshot.png)

<img src="static/screenshot-mobile.png" alt="Remoterm UI — mobile with soft keyboard" width="280">

## Features

- **Multiple named sessions** in a sidebar — switch between `backend`, `monitoring`, `frontend`
- **Sessions survive disconnects** — close your laptop, reattach from your phone, output picks up where you left off
- **Sessions survive server restarts** — running sessions are respawned automatically
- **Focus mode** — collapse sidebar and chrome for a distraction-free terminal (Esc to exit)
- **Mobile soft keyboard** — Ctrl, Alt, Esc, Tab, arrows, Home/End, PgUp/PgDn, F1–F12 with one-shot and lock modifiers
- **Ctrl+Arrow and modified navigation** — soft keys emit proper CSI sequences for Ctrl+Arrow, Ctrl+Home/End, etc.
- **Live terminal size indicator** — cols x rows displayed in the toolbar, updates on every resize
- **Unread badges** on background sessions with auto-refreshing session list
- **Session management** — rename, stop, restart, archive/restore, delete
- **Single binary** with embedded web UI, SQLite for state
- **Error toasts** — API errors surface in the UI instead of silently failing

## Install

Download a prebuilt binary from the [latest release](https://github.com/mr-karan/remoterm/releases/latest), or use Docker:

```bash
docker pull ghcr.io/mr-karan/remoterm:latest
```

Or build from source:

```bash
cargo build --release -p remoterm-server
```

## Quick start

```bash
remoterm-server --listen 127.0.0.1:8787
```

Then open http://127.0.0.1:8787/

Or with Docker Compose:

```bash
docker compose up -d
```

## API

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/healthz` | Health check |
| `GET` | `/api/sessions` | List all sessions |
| `POST` | `/api/sessions` | Create session (`{"name","cwd","shell","args"}`) |
| `GET` | `/api/sessions/:id` | Get session |
| `PATCH` | `/api/sessions/:id` | Rename session |
| `DELETE` | `/api/sessions/:id` | Delete session |
| `POST` | `/api/sessions/:id/restart` | Restart session |
| `POST` | `/api/sessions/:id/stop` | Stop session |
| `POST` | `/api/sessions/:id/archive` | Archive session |
| `POST` | `/api/sessions/:id/restore` | Restore archived session |
| `GET` | `/ws/:id` | WebSocket attach (PTY + replay) |

## Deployment

See [docs/deployment.md](docs/deployment.md) for native host, Docker, systemd, and reverse proxy setup.

## Architecture

- **`remoterm-server`** — Axum HTTP/WS server, PTY management, SQLite storage
- **`remoterm-proto`** — Shared protocol types (frames, session models)
- **`static/index.html`** — Built-in web UI (embedded via `include_str!`)

## Protocol

WebSocket at `/ws/:session_id` with JSON framing:

- Client sends `hello` with `resume_from_seq` for reconnect replay
- Server replies with `hello_ack` + `snapshot` (buffered output) + `status`
- Live `output` frames with monotonic `seq`
- Client sends `input`, `resize`, `keyboard` actions

## Development

```bash
# Dev server with debug logging
just dev

# Run all tests (unit + restart recovery integration)
just test

# Interactive smoke test
just smoke
```

## Security

No built-in auth yet. Bind to localhost or put behind Tailscale / VPN / reverse proxy with auth. Do not expose to the public internet without authentication.

## License

MIT
