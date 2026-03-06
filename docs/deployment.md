# Deployment

Remoterm can be deployed two ways: **native on the host** (recommended for full host access) or **in a Docker container** (sandboxed sessions).

## Native host deployment (recommended)

Sessions run directly on the host with full access to all tools, files, and networking.

### Prerequisites

- Docker (for building the binary — no Rust toolchain needed on host)
- systemd with user session support (`loginctl enable-linger <user>`)

### Build

Cross-compile inside Docker:

```bash
just build-linux
```

This runs `cargo build --release` inside a `rust:1.86-bookworm` container with `--platform linux/amd64`, producing `remoterm-server-linux-amd64`.

### Deploy to remote host

```bash
export REMOTERM_HOST=myserver   # or pass host= to each recipe

just deploy           # builds + uploads + restarts
just install-service  # creates systemd user service (first time)
```

The `install-service` recipe creates `~/.config/systemd/user/remoterm.service`:

```ini
[Unit]
Description=Remoterm terminal server
After=network.target

[Service]
Type=simple
ExecStart=%h/.local/bin/remoterm-server --listen 0.0.0.0:8787 --db-path %h/remoterm-data/remoterm.sqlite3
Environment=RUST_LOG=remoterm_server=info
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
```

### Service management

```bash
just status   # systemctl --user status remoterm
just logs     # journalctl --user -u remoterm -f
just stop     # systemctl --user stop remoterm
```

### Reverse proxy

Put Remoterm behind a reverse proxy (Caddy, nginx, etc.) with websocket support:

```
# Caddy example
terminal.example.com {
    reverse_proxy localhost:8787
    @websockets {
        header Connection *Upgrade*
        header Upgrade websocket
    }
    reverse_proxy @websockets localhost:8787
}
```

### Session paths

Since sessions run on the host, use real host paths:

- `cwd`: `/home/youruser` or `/home/youruser/Code/myproject`
- `shell`: `/bin/bash`

---

## Docker container deployment

Sessions run inside the container. Useful for sandboxed environments.

### Quick start

```bash
docker compose up -d --build
```

### Important

Session `cwd` and `shell` must exist inside the container. If your code lives on the host, bind-mount it in `docker-compose.yml`:

```yaml
volumes:
  - remoterm-data:/data
  - /home/youruser:/workspace
```

Then use container paths for sessions:

- `cwd`: `/workspace/myproject`
- `shell`: `/bin/bash`

The default runtime image includes common shell tooling plus `htop` and `btop`, so those TUIs work in container-backed sessions without extending the image first.

### Extending the image

```dockerfile
FROM remoterm:latest

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ripgrep \
        tmux \
    && rm -rf /var/lib/apt/lists/*
```

---

## Persistence

The SQLite database at the configured `--db-path` stores:

- session metadata (name, cwd, shell, status)
- bounded output replay history
- restart recovery state

Sessions marked `running` or `starting` are respawned on server restart.

### Backup

Back up the single SQLite file:

- Native: `~/remoterm-data/remoterm.sqlite3`
- Docker: the `remoterm-data` named volume (or bind-mount it for easier backup)

## Security

- No built-in auth yet — do not expose to the public internet
- Bind to localhost or put behind Tailscale / VPN / reverse proxy with auth
- The default listen address is `127.0.0.1:8787`

## Updating

```bash
# Native
just deploy

# Docker
docker compose up -d --build
```

Schema migrations are applied automatically on startup.
