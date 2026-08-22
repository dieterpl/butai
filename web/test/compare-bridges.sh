#!/usr/bin/env bash
# Run `server.py` and the Bun bridge against ONE isolated daemon and diff every
# answer. The port is only correct if a client cannot tell them apart, so this
# compares replies rather than asserting on them one at a time — the assertions
# would encode what I *think* the Python does, which is exactly the thing under
# test.
#
# Isolation, because a daemon on the default paths restores the user's real
# session and spawns their agents:
#   * a throwaway HOME, kept short — the socket path has to fit SUN_LEN
#   * BUTAI_SOCKET set explicitly; it is inherited from any butai pane this runs
#     inside, and without it an "isolated" daemon aims at the real one
#   * killed by socket at the end. Never `pkill -f butai`: that matches the
#     user's own daemon and has killed it before.
set -uo pipefail

# `BUTAI_BIN`, not `BUTAI`: a butai pane exports `BUTAI` itself, set to the
# *socket* path, so `${BUTAI:-<a binary>}` silently resolves to a socket and the
# script tries to execute it. The whole family is polluted inside a pane —
# `BUTAI`, `BUTAI_SOCKET`, `BUTAI_PANE`, `BUTAI_WORKSPACE` — which is the same
# trap that makes an "isolated" daemon aim at the real one.
BUTAI_BIN=${BUTAI_BIN:-/var/tmp/butai-probe/butai}
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)   # the repo root
WEB="$ROOT/web"
RUN=/var/tmp/bt-cmp
SOCK=$RUN/d.sock
PY_PORT=8091
TS_PORT=8092
FAIL=0

cleanup() {
  [ -n "${PY_PID:-}" ] && kill "$PY_PID" 2>/dev/null
  [ -n "${TS_PID:-}" ] && kill "$TS_PID" 2>/dev/null
  [ -S "$SOCK" ] && HOME=$RUN "$BUTAI_BIN" --socket "$SOCK" kill-server >/dev/null 2>&1
  sleep 0.3
}
trap cleanup EXIT

rm -rf "$RUN"; mkdir -p "$RUN"
export HOME=$RUN BUTAI_SOCKET=$SOCK
unset BUTAI BUTAI_WORKSPACE BUTAI_PANE BUTAI_SOCKETS BUTAI_SOCKET_DIRS

"$BUTAI_BIN" daemon >"$RUN/daemon.log" 2>&1 &
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.2; done
[ -S "$SOCK" ] || { echo "daemon never came up:"; cat "$RUN/daemon.log"; exit 1; }

# Something for the snapshot to have in it: a workspace exercises
# `qualify_workspace`, which is where the wrong-machine bugs would live.
mkdir -p "$RUN/proj" && git -C "$RUN/proj" init -q 2>/dev/null
"$BUTAI_BIN" --socket "$SOCK" workspace new "$RUN/proj" >/dev/null 2>&1 \
  || "$BUTAI_BIN" --socket "$SOCK" new -d -c "$RUN/proj" >/dev/null 2>&1

PORT=$PY_PORT BUTAI_SOCKET=$SOCK python3 "$WEB/server.py" >"$RUN/py.log" 2>&1 &
PY_PID=$!
PORT=$TS_PORT BUTAI_SOCKET=$SOCK bun "$WEB/app/server/index.ts" >"$RUN/ts.log" 2>&1 &
TS_PID=$!

up() { for _ in $(seq 1 50); do curl -fsS "http://127.0.0.1:$1/api/daemons" >/dev/null 2>&1 && return 0; sleep 0.2; done; return 1; }
up $PY_PORT || { echo "server.py never came up:"; cat "$RUN/py.log"; exit 1; }
up $TS_PORT || { echo "bun bridge never came up:"; cat "$RUN/ts.log"; exit 1; }

# `socket` and `system` differ legitimately (an absolute temp path, and live
# telemetry sampled a moment apart), so they are blanked before comparing.
norm() { python3 -c '
import json,sys,re
def scrub(o):
    if isinstance(o,dict):
        return {k:("<v>" if k in ("system","socket","sampled_ms","started_ms","working_since_ms",
                                  "at_ms","seq","attached_clients","uptime_ms") else scrub(v))
                for k,v in sorted(o.items())}
    if isinstance(o,list): return [scrub(x) for x in o]
    return o
try: print(json.dumps(scrub(json.load(sys.stdin)),indent=1,sort_keys=True))
except Exception as e: print("NOT JSON:",e)
'; }

cmp_route() {
  local what=$1 method=${2:-GET} path=$3 body=${4:-}
  local a b
  if [ -n "$body" ]; then
    a=$(curl -sS -X "$method" -H 'Content-Type: application/json' -d "$body" "http://127.0.0.1:$PY_PORT$path" | norm)
    b=$(curl -sS -X "$method" -H 'Content-Type: application/json' -d "$body" "http://127.0.0.1:$TS_PORT$path" | norm)
  else
    a=$(curl -sS -X "$method" "http://127.0.0.1:$PY_PORT$path" | norm)
    b=$(curl -sS -X "$method" "http://127.0.0.1:$TS_PORT$path" | norm)
  fi
  if [ "$a" = "$b" ]; then
    echo "  ok    $what"
  else
    echo "  FAIL  $what"
    diff <(echo "$a") <(echo "$b") | head -20 | sed 's/^/          /'
    FAIL=1
  fi
}

cmp_status() {
  local what=$1 method=$2 path=$3
  local a b
  a=$(curl -sS -o /dev/null -w '%{http_code}' -X "$method" "http://127.0.0.1:$PY_PORT$path")
  b=$(curl -sS -o /dev/null -w '%{http_code}' -X "$method" "http://127.0.0.1:$TS_PORT$path")
  if [ "$a" = "$b" ]; then echo "  ok    $what ($a)"; else echo "  FAIL  $what: py=$a bun=$b"; FAIL=1; fi
}

echo "== the bridge's own routes =="
cmp_route "GET /api/daemons"       GET /api/daemons
cmp_route "GET /api/state"         GET /api/state

echo "== proxied to the daemon =="
cmp_route "GET /api/workspaces"    GET /api/workspaces
cmp_route "GET /api/system"        GET /api/system
cmp_route "GET /api/agents"        GET /api/agents
cmp_route "GET /api/notifications" GET /api/notifications
cmp_route "GET /api/usage"         GET /api/usage
cmp_route "GET /api/workspaces/1"  GET /api/workspaces/1
cmp_route "GET .../1/tree"         GET /api/workspaces/1/tree
cmp_route "GET .../1/changes"      GET /api/workspaces/1/changes

echo "== the refusals (the security boundary) =="
cmp_route "unknown daemon key"     GET "/api/workspaces/nope:1"
cmp_route "?daemon= unknown"       GET "/api/agents?daemon=nope"
cmp_route "add: no socket"         POST /api/daemons '{}'
cmp_route "add: host is refused"   POST /api/daemons '{"host":"gpu-box"}'
cmp_route "add: relative path"     POST /api/daemons '{"socket":"relative/x.sock"}'
cmp_route "add: outside allowlist" POST /api/daemons '{"socket":"/etc/passwd"}'
cmp_route "add: not a socket"      POST /api/daemons "{\"socket\":\"$RUN/daemon.log\"}"
cmp_route "add: missing socket"    POST /api/daemons "{\"socket\":\"$RUN/nope.sock\"}"
cmp_route "remove env daemon"      DELETE /api/daemons/local
cmp_route "remove unknown"         DELETE /api/daemons/nope

echo "== status codes and static =="
cmp_status "GET /"                 GET /
cmp_status "GET /api.js"           GET /api.js
cmp_status "GET /ui/"              GET /ui/
cmp_status "GET /ui/kit.js"        GET /ui/kit.js
cmp_status "GET /favicon.svg"      GET /favicon.svg
cmp_status "GET /nope"             GET /nope
cmp_status "traversal /ui/../server.py" GET "/ui/../server.py"
cmp_status "GET /vendor/react.js"  GET /vendor/react.js

echo "== the event stream =="
for p in $PY_PORT $TS_PORT; do
  curl -sS --max-time 3 -N "http://127.0.0.1:$p/api/events" 2>/dev/null \
    | grep -oE '^(event: [a-z]+|retry: [0-9]+)' | head -3 > "$RUN/sse.$p"
done
if diff -q "$RUN/sse.$PY_PORT" "$RUN/sse.$TS_PORT" >/dev/null; then
  echo "  ok    SSE prologue: $(tr '\n' ' ' < "$RUN/sse.$PY_PORT")"
else
  echo "  FAIL  SSE prologue differs"; diff "$RUN/sse.$PY_PORT" "$RUN/sse.$TS_PORT" | sed 's/^/          /'; FAIL=1
fi

echo
[ $FAIL -eq 0 ] && echo "ALL MATCH" || echo "DIFFERENCES FOUND"
exit $FAIL
