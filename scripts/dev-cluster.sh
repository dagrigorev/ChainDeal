#!/usr/bin/env bash
# Connects locally run (and debugged) services to the Kubernetes cluster.
#
#   scripts/dev-cluster.sh up       start port-forwards, write target/dev/*.env
#   scripts/dev-cluster.sh status   show forwards
#   scripts/dev-cluster.sh down     stop forwards
#
# `up` is idempotent and quick when everything is already connected, so IDE run
# configurations call it as a "before launch" step. Each forward runs detached
# in a reconnect loop, so it survives pod restarts and outlives the IDE task.
#
#   127.0.0.1:3301   tarantool-rw     chain DB master
#   127.0.0.1:3311   tarantool-read   chain DB read replica
#   127.0.0.1:3302   auth-db          identity DB
#   127.0.0.1:18081  auth             in-cluster auth service (JWKS for a local backend)
#
# Local services bind 127.0.0.1:8080 (backend) and 127.0.0.1:8081 (auth).
# The env files hold secrets: they live under target/ (gitignored), mode 600.
set -euo pipefail
# IDEs launched from the Dock may not have the shell PATH (kubectl).
export PATH="/usr/local/bin:/opt/homebrew/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEV="$ROOT/target/dev"
SECRETS="$ROOT/deploy/k8s/.secrets"
CTX="${CTX:-docker-desktop}"
NS=chaindeal
FORWARDS=(
  "tarantool-rw 3301 3301"
  "tarantool-read 3311 3301"
  "auth-db 3302 3301"
  "auth 18081 8080"
)
mkdir -p "$DEV/forwards"

open_port() { nc -z 127.0.0.1 "$1" >/dev/null 2>&1; }

start_forward() {
  local svc=$1 port=$2 target=$3 pidf="$DEV/forwards/$1.pid"
  if open_port "$port"; then return; fi
  if [[ -f $pidf ]] && kill -0 "$(cat "$pidf")" 2>/dev/null; then return; fi
  # setsid: its own process group, detached from the caller (IDE, terminal).
  perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' bash -c "
    while :; do
      kubectl --context '$CTX' -n $NS port-forward --address 127.0.0.1 svc/$svc $port:$target
      sleep 1
    done" >"$DEV/forwards/$svc.log" 2>&1 </dev/null &
  echo $! >"$pidf"
  echo "  forwarding $svc -> 127.0.0.1:$port"
}

wait_ports() {
  local port
  for f in "${FORWARDS[@]}"; do
    port=$(cut -d' ' -f2 <<<"$f")
    for _ in $(seq 1 100); do open_port "$port" && break; sleep 0.2; done
    open_port "$port" || { echo "127.0.0.1:$port did not come up; see $DEV/forwards/" >&2; exit 1; }
  done
}

secret() { grep -E "^$2=" "$SECRETS/$1" | cut -d= -f2-; }

write_env() {
  [[ -d $SECRETS ]] || { echo "no $SECRETS: run 'make k8s-secrets' (or 'make k8s-up') first" >&2; exit 1; }
  umask 077
  cat >"$DEV/backend.env" <<EOF
# Written by scripts/dev-cluster.sh: a local chain node joined to the cluster.
TARANTOOL_ADDR=127.0.0.1:3301
TARANTOOL_READ_ADDR=127.0.0.1:3311
TARANTOOL_PASSWORD=$(secret db.env CHAINDEAL_DB_PASSWORD)
AUTH_JWKS_URL=http://127.0.0.1:18081/oauth/jwks
AUTH_ISSUER=https://chaindeal.localhost
CHAINDEAL_ATTESTATION_KEY=$(secret attest.env CHAINDEAL_ATTESTATION_KEY)
CHAINDEAL_NODE_ID=local-debug
CHAINDEAL_ROLE=follower
BIND_ADDR=127.0.0.1:8080
GRPC_ADDR=127.0.0.1:9090
RUST_LOG=info,chaindeal_backend=debug,tower_http=info
EOF
  { echo "# Written by scripts/dev-cluster.sh: a local auth service on the cluster's identity DB."
    grep -E '^AUTH_' "$SECRETS/auth.env"
    cat <<EOF
AUTH_DB_ADDR=127.0.0.1:3302
AUTH_ISSUER=https://chaindeal.localhost
AUTH_WEB_REDIRECTS=https://chaindeal.localhost/callback,https://chaindeal.test/callback,http://localhost:5173/callback
BIND_ADDR=127.0.0.1:8081
RUST_LOG=info,chaindeal_auth=debug,tower_http=info
EOF
  } >"$DEV/auth.env"
}

case "${1:-up}" in
  up)
    kubectl --context "$CTX" -n "$NS" get svc tarantool-rw >/dev/null \
      || { echo "cluster not reachable (context $CTX); is Docker Desktop Kubernetes running and 'make k8s-up' done?" >&2; exit 1; }
    # Several run configurations may start at once (compound): serialise.
    until mkdir "$DEV/.lock" 2>/dev/null; do sleep 0.2; done
    trap 'rmdir "$DEV/.lock"' EXIT
    for f in "${FORWARDS[@]}"; do start_forward $f; done
    wait_ports
    write_env
    echo "cluster connected; env: target/dev/backend.env, target/dev/auth.env"
    ;;
  status)
    for f in "${FORWARDS[@]}"; do
      read -r svc port _ <<<"$f"
      if open_port "$port"; then echo "  up    $svc  127.0.0.1:$port"; else echo "  down  $svc  127.0.0.1:$port"; fi
    done
    ;;
  down)
    for pidf in "$DEV"/forwards/*.pid; do
      [[ -f $pidf ]] || continue
      kill -- "-$(cat "$pidf")" 2>/dev/null || true
      rm -f "$pidf"
    done
    echo "port-forwards stopped"
    ;;
  *) echo "usage: $0 [up|status|down]" >&2; exit 2 ;;
esac
