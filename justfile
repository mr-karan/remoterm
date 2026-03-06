set dotenv-load

default_host := env_var_or_default("REMOTERM_HOST", "myserver")

# Local dev server with debug logging
dev:
    RUST_LOG=remoterm_server=debug cargo run -p remoterm-server -- \
        --listen 127.0.0.1:8787 --db-path /tmp/remoterm-dev.sqlite3

# Build release binary (UI is embedded via include_str!)
build:
    cargo build --release -p remoterm-server

# Run all tests
test:
    cargo test
    ./tests/restart_recovery.sh

# Interactive smoke test
smoke:
    cargo build -p remoterm-server
    @echo ""
    @echo "  UI:       http://127.0.0.1:8787/"
    @echo "  Health:   http://127.0.0.1:8787/healthz"
    @echo "  Sessions: http://127.0.0.1:8787/api/sessions"
    @echo ""
    RUST_LOG=remoterm_server=debug cargo run -p remoterm-server -- \
        --listen 127.0.0.1:8787 --db-path /tmp/remoterm-smoke.sqlite3

# Cross-compile Linux x86_64 binary via Docker
build-linux:
    docker run --rm --platform linux/amd64 \
        -v $(pwd):/src -w /src rust:1.86-bookworm \
        bash -c 'cargo build --release -p remoterm-server && \
        cp target/release/remoterm-server /src/remoterm-server-linux-amd64'

# Build and push Docker image to GHCR
docker-build tag="latest":
    docker build --platform linux/amd64 -t ghcr.io/mr-karan/remoterm:{{tag}} .

docker-push tag="latest":
    docker push ghcr.io/mr-karan/remoterm:{{tag}}

# Deploy to remote host
deploy host=default_host:
    just build-linux
    scp remoterm-server-linux-amd64 {{host}}:~/.local/bin/remoterm-server
    ssh {{host}} "chmod +x ~/.local/bin/remoterm-server && systemctl --user restart remoterm"
    @echo "Deployed to {{host}}"

# Install systemd user service on remote (first time)
install-service host=default_host:
    ssh {{host}} 'mkdir -p ~/.local/bin ~/remoterm-data ~/.config/systemd/user && cat > ~/.config/systemd/user/remoterm.service << '\''EOF'\''
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
EOF'
    ssh {{host}} "systemctl --user daemon-reload && systemctl --user enable --now remoterm"
    @echo "Service installed and started on {{host}}"

# Remote service management
status host=default_host:
    ssh {{host}} "systemctl --user status remoterm"

logs host=default_host:
    ssh {{host}} "journalctl --user -u remoterm -f"

stop host=default_host:
    ssh {{host}} "systemctl --user stop remoterm"
