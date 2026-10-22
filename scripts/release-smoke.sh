#!/usr/bin/env bash
# Test only a freshly built release in an isolated temporary directory.
set -euo pipefail
archive=$(realpath "$1")
stage_dir=$(mktemp -d)
mkdir -p "$stage_dir/app" "$stage_dir/data" "$stage_dir/restored"
tar -xzf "$archive" -C "$stage_dir/app"
test -f "$stage_dir/app/web/licenses/LICENSE"
test -f "$stage_dir/app/web/licenses/CC-BY-NC-4.0.txt"
export OVERSIGHT_DATA_DIR="$stage_dir/data"
export OVERSIGHT_WEB_DIR="$stage_dir/app/web"
export OVERSIGHT_HOST=127.0.0.1
export OVERSIGHT_PORT=18081
export OVERSIGHT_SETUP_TOKEN=smoke-setup-token
export OVERSIGHT_TEST_URL=http://127.0.0.1:18081
export OVERSIGHT_TEST_SETUP_TOKEN=smoke-setup-token
export OVERSIGHT_MOCK_ORIGIN=http://127.0.0.1:19091
pid=""
stop_server() {
  if [[ -n "$pid" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" || true
    pid=""
  fi
}
trap stop_server EXIT
start_server() {
  "$stage_dir/app/oversight" > "$stage_dir/server.log" 2>&1 &
  pid=$!
  for attempt in $(seq 1 30); do
    if curl --fail --silent "$OVERSIGHT_TEST_URL/healthz" >/dev/null; then return; fi
    sleep 1
  done
  cat "$stage_dir/server.log"
  return 1
}
start_server
node scripts/smoke.mjs
node scripts/verify-restored.mjs snapshot "$stage_dir/snapshot.json"
stop_server
# A stopped-volume backup must include SQLite and its encryption key.
tar -czf "$stage_dir/backup.tar.gz" -C "$stage_dir/data" .
(cd "$stage_dir/data" && find . -type f -print0 | sort -z | xargs -0 sha256sum) > "$stage_dir/data.sha256"
tar -xzf "$stage_dir/backup.tar.gz" -C "$stage_dir/restored"
(cd "$stage_dir/restored" && sha256sum --check "$stage_dir/data.sha256")
export OVERSIGHT_DATA_DIR="$stage_dir/restored"
start_server
node scripts/verify-restored.mjs verify "$stage_dir/snapshot.json"
echo "PASS packaged release and complete-volume backup restoration"
