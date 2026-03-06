# ttyd Architecture Mapping (Reference)

Repository analyzed: `/tmp/ttyd` at commit `e2819f2`

## Core split

- `src/server.c`
  - CLI option parsing
  - libwebsockets context setup
  - lifecycle/signals
- `src/http.c`
  - serves index and token endpoint
  - auth checks
- `src/protocol.c`
  - websocket protocol
  - client message parsing
  - PTY spawn/write/resize/pause/resume
- `src/pty.c`
  - PTY process abstraction on Unix/Windows
  - async read/write via libuv

## ttyd protocol shape (important for compatibility ideas)

- Client commands (first byte):
  - `'0'`: input
  - `'1'`: resize
  - `'2'`: pause
  - `'3'`: resume
  - `'{'`: initial JSON blob (auth token, columns, rows)
- Server commands (first byte):
  - `'0'`: output bytes
  - `'1'`: set window title
  - `'2'`: set preferences

## Behavior worth keeping

- Flow control via pause/resume when render backlog grows.
- Terminal resize messages as JSON `{columns,rows}`.
- Simple startup handshake with preferences delivery.
- Optional readonly mode and authentication hooks.

## Behavior to improve for Remoterm

- Per-connection process model: new process spawned per websocket attach.
  - Remoterm needs persistent session model independent of websocket lifecycle.
- No first-class multi-session UX.
- No mobile keyboard UX model.
- Auth model is basic and mostly header/basic token driven.
- State durability beyond process runtime is limited.

