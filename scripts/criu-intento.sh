#!/usr/bin/env bash
# Intento REAL de CRIU sobre un agente propio: `criu check` y `criu dump --leave-running`.
# No finge éxito: guarda el log y sale con el código de CRIU.
set -u
cd "$(dirname "$0")/.."
BIN=./target/release/agente
[ -x "$BIN" ] || cargo build --release >/dev/null
TS=$(TZ=UTC date -d '-3 hours' +%Y-%m-%dT%H%M)
OUT=resultados/criu-intento-${TS}-ART.txt
D=$(mktemp -d /tmp/criu-dump-XXXX)
{
  echo "# criu-intento ${TS} ART (UTC-03:00) — kernel $(uname -r)"
  echo "## criu --version"; /usr/sbin/criu --version
  echo "## sudo -n criu check"; sudo -n /usr/sbin/criu check 2>&1; echo "rc=$?"
  echo "## criu check --unprivileged (sin root)"; /usr/sbin/criu check --unprivileged 2>&1; echo "rc=$?"
  setsid "$BIN" --tamano 4M --pasos 0 </dev/null >/dev/null 2>&1 &
  P=$!; sleep 0.3
  echo "## sudo -n criu dump -t $P --leave-running --shell-job"
  sudo -n /usr/sbin/criu dump -t "$P" -D "$D" --leave-running --shell-job -v4 -o dump.log 2>&1; RC=$?
  echo "rc=$RC"; sudo -n grep -E "Error|FAILED" "$D/dump.log"
  kill "$P" 2>/dev/null
  echo "## kcmp(2) directo"; ./target/release/snap criu-probe 2>/dev/null | sed -n '/kcmp/,$p'
} > "$OUT" 2>&1
sudo -n rm -rf "$D"
echo "$OUT"; cat "$OUT"
