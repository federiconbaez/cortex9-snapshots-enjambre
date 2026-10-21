#!/usr/bin/env bash
# Experimento de valor del sistema 3: snapshot vs arranque en frío.
# 3 corridas independientes (procesos separados) → build/valor/corrida-{1,2,3}.json,
# luego scripts/valor_resumen.py junta las muestras y escribe resultados/valor-*.json.
set -euo pipefail
cd "$(dirname "$0")/.."
CORRIDAS=${CORRIDAS:-3}
REPS=${REPS:-5}
cargo build --release --quiet
mkdir -p build/valor
uptime > build/valor/carga-antes.txt
for c in $(seq 1 "$CORRIDAS"); do
  ./target/release/bench_valor --tamanos 4M,64M --agentes 1,4,16 \
    --calentamiento 30,300,3000 --reps "$REPS" \
    --out build/valor/tmp --json "build/valor/corrida-$c.json" > "build/valor/corrida-$c.txt"
  uptime >> build/valor/carga-antes.txt
done
python3 scripts/valor_resumen.py build/valor/corrida-*.json
