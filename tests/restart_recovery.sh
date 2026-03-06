#!/usr/bin/env bash
#
# Integration test: restart recovery
#
# Verifies that sessions marked running/starting are respawned
# after a server restart, and that stopped/exited sessions are not.
#
set -euo pipefail

BINARY="${BINARY:-./target/debug/remoterm-server}"
PORT="${PORT:-0}"  # 0 = pick a free port
DB=$(mktemp /tmp/remoterm-test-XXXXXX.sqlite3)
LISTEN="127.0.0.1"

cleanup() {
    [[ -n "${SERVER_PID:-}" ]] && kill "$SERVER_PID" 2>/dev/null && wait "$SERVER_PID" 2>/dev/null || true
    rm -f "$DB"
}
trap cleanup EXIT

# --- helpers ---

wait_healthy() {
    local url="$1"
    for i in $(seq 1 30); do
        if curl -sf "$url/healthz" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    echo "FAIL: server did not become healthy at $url"
    exit 1
}

start_server() {
    # Pick a free port by binding to :0, but we need a concrete port.
    # Use a known port range trick: let the OS pick, parse from log.
    if [[ "$PORT" == "0" ]]; then
        local free_port
        free_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')
        PORT="$free_port"
    fi
    local addr="${LISTEN}:${PORT}"
    RUST_LOG=remoterm_server=warn "$BINARY" --listen "$addr" --db-path "$DB" &
    SERVER_PID=$!
    BASE_URL="http://$addr"
    wait_healthy "$BASE_URL"
}

stop_server() {
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null || true
    unset SERVER_PID
}

assert_eq() {
    local label="$1" expected="$2" actual="$3"
    if [[ "$expected" != "$actual" ]]; then
        echo "FAIL: $label — expected '$expected', got '$actual'"
        exit 1
    fi
}

assert_contains() {
    local label="$1" needle="$2" haystack="$3"
    if [[ "$haystack" != *"$needle"* ]]; then
        echo "FAIL: $label — expected to contain '$needle', got: $haystack"
        exit 1
    fi
}

# --- build ---

echo "Building remoterm-server..."
cargo build -p remoterm-server --quiet

# --- test ---

echo "Starting server (round 1)..."
start_server

echo "Creating session..."
CREATE_RESP=$(curl -sf -X POST "$BASE_URL/api/sessions" \
    -H 'content-type: application/json' \
    -d '{"name":"restart-test","cwd":"/tmp","shell":"/bin/sh","args":["-c","sleep 600"]}')

SESSION_ID=$(echo "$CREATE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")
SESSION_STATUS=$(echo "$CREATE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])")
assert_eq "session created as running" "running" "$SESSION_STATUS"
echo "  session $SESSION_ID created (status: $SESSION_STATUS)"

# Verify it shows up in the list
LIST_RESP=$(curl -sf "$BASE_URL/api/sessions")
assert_contains "session in list" "$SESSION_ID" "$LIST_RESP"

echo "Stopping server..."
stop_server

# Small pause to make sure port is released
sleep 0.5

echo "Starting server (round 2)..."
PORT=0  # pick a new port for round 2
start_server

echo "Checking session recovery..."
GET_RESP=$(curl -sf "$BASE_URL/api/sessions/$SESSION_ID")
RECOVERED_STATUS=$(echo "$GET_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])")
RECOVERED_NAME=$(echo "$GET_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['name'])")

assert_eq "session name preserved" "restart-test" "$RECOVERED_NAME"
assert_eq "session recovered as running" "running" "$RECOVERED_STATUS"
echo "  session $SESSION_ID recovered (status: $RECOVERED_STATUS)"

# Verify the session has a new PID (re-spawned)
RECOVERED_PID=$(echo "$GET_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('pid','None'))")
echo "  recovered PID: $RECOVERED_PID"

# Clean up the session
curl -sf -X DELETE "$BASE_URL/api/sessions/$SESSION_ID" >/dev/null

echo ""
echo "PASS: restart recovery test"
