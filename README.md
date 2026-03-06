# Remoterm

Persistent, multi-session remote terminal server. A `ttyd` alternative built in Rust for long-running coding agent workflows.

## Why

`ttyd` is great for single shared sessions. Remoterm is designed for a different use case:

- **Multiple named sessions** in a sidebar — switch between `backend`, `frontend`, `infra`
- **Sessions survive disconnects** — close your laptop, reattach from your phone, output picks up where you left off
- **Sessions survive server restarts** — running sessions are respawned automatically
- **Mobile keyboard** — Ctrl, Alt, Esc, Tab, arrows, Home/End, PgUp/PgDn, F1–F12
- **Unread badges** on background sessions
- **Single binary**, SQLite for state

## Quick start

```bash
cargo run -p remoterm-server -- --listen 127.0.0.1:8787
```

Then open http://127.0.0.1:8787/

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

# Or use make
make smoke           # interactive smoke test
make test-restart    # restart recovery integration test
```

## Security

No built-in auth yet. Bind to localhost or put behind Tailscale / VPN / reverse proxy with auth. Do not expose to the public internet without authentication.

## License

MIT
