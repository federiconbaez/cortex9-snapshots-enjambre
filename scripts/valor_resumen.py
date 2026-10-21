#!/usr/bin/env python3
"""Junta las corridas de bench_valor y resume p50/p95 por celda.

Uso: valor_resumen.py build/valor/corrida-*.json
Escribe resultados/valor-<fecha>-ART.json e imprime tablas markdown.
"""
import json, math, sys, time, collections

def pct(v, q):
    v = sorted(v)
    if not v:
        return float("nan")
    r = min(max(math.ceil(q * len(v)), 1), len(v))
    return v[r - 1]

muestras, fallas = [], []
for i, p in enumerate(sys.argv[1:], 1):
    d = json.load(open(p))
    fallas += d["fallas"]
    for m in d["muestras"]:
        m["corrida"] = i
        muestras.append(m)

celdas = collections.defaultdict(list)
for m in muestras:
    celdas[(m["tamano"], m["agentes"], m["calentamiento_ms"], m["variante"])].append(m)

def tam_key(t):
    return int(t[:-1]) * (1024 if t[-1] in "Gg" else 1)

resumen = []
for (t, n, c, v), ms in sorted(celdas.items(), key=lambda kv: (tam_key(kv[0][0]), kv[0][1], kv[0][2], kv[0][3])):
    fila = {"tamano": t, "agentes": n, "calentamiento_ms": c, "variante": v, "muestras": len(ms),
            "corridas": len({m["corrida"] for m in ms}),
            "estado_equivalente": all(m["estado_equivalente"] for m in ms)}
    for k in ("listo_ms", "carga_ms", "e2e_ms"):
        xs = [m[k] for m in ms]
        fila[k] = {"p50": pct(xs, .5), "p95": pct(xs, .95), "min": min(xs), "max": max(xs)}
    fila["carga_p50_por_corrida"] = [pct([m["carga_ms"] for m in ms if m["corrida"] == r], .5)
                                     for r in sorted({m["corrida"] for m in ms})]
    resumen.append(fila)

idx = {(f["tamano"], f["agentes"], f["calentamiento_ms"], f["variante"]): f for f in resumen}
comparacion = []
for f in resumen:
    if f["variante"] != "frio_reconstruir":
        continue
    t, n, c = f["tamano"], f["agentes"], f["calentamiento_ms"]
    for v in ("snap_uffd", "snap_uffd16", "snap_completo", "snap_mmap"):
        s = idx.get((t, n, c, v))
        if not s:
            continue
        comparacion.append({
            "tamano": t, "agentes": n, "calentamiento_ms": c, "variante": v,
            "x_carga_p50": f["carga_ms"]["p50"] / s["carga_ms"]["p50"],
            "x_e2e_p50": f["e2e_ms"]["p50"] / s["e2e_ms"]["p50"],
            "x_listo_p50": f["listo_ms"]["p50"] / max(s["listo_ms"]["p50"], 1e-6),
            # gana sin ambigüedad: la cola del snapshot por debajo de la mediana de la línea base
            "gana_carga_p95_vs_p50": s["carga_ms"]["p95"] < f["carga_ms"]["p50"],
            "gana_e2e_p95_vs_p50": s["e2e_ms"]["p95"] < f["e2e_ms"]["p50"],
        })

art = time.gmtime(time.time() - 3 * 3600)
fecha = time.strftime("%Y-%m-%d %H:%M ART (UTC-03:00)", art)
corto = time.strftime("%Y-%m-%dT%H%M", art)
doc = {"sistema": "03-snapshots-enjambre", "experimento": "valor: snapshot vs arranque en frío",
       "fecha_ART": fecha, "archivos": sys.argv[1:], "muestras_totales": len(muestras),
       "fallas": fallas, "resumen": resumen, "comparacion_vs_frio_reconstruir": comparacion}
salida = f"resultados/valor-{corto}-ART.json"
json.dump(doc, open(salida, "w"), indent=1, ensure_ascii=False)

print(f"# {fecha} · muestras={len(muestras)} · fallas={len(fallas)} · {salida}\n")
print("| heap | N | calent. | variante | listo p50 | carga p50 | carga p95 | e2e p50 | e2e p95 | p50 por corrida (carga) | equiv |")
print("|---|---|---|---|---|---|---|---|---|---|---|")
for f in resumen:
    print(f"| {f['tamano']} | {f['agentes']} | {f['calentamiento_ms']} | {f['variante']} | "
          f"{f['listo_ms']['p50']:.3f} | {f['carga_ms']['p50']:.3f} | {f['carga_ms']['p95']:.3f} | "
          f"{f['e2e_ms']['p50']:.2f} | {f['e2e_ms']['p95']:.2f} | "
          f"{' / '.join(f'{x:.2f}' for x in f['carga_p50_por_corrida'])} | {'sí' if f['estado_equivalente'] else 'NO'} |")
print("\n| heap | N | calent. | variante | × carga p50 | × e2e p50 | carga p95<base p50 | e2e p95<base p50 |")
print("|---|---|---|---|---|---|---|---|")
for c in comparacion:
    print(f"| {c['tamano']} | {c['agentes']} | {c['calentamiento_ms']} | {c['variante']} | {c['x_carga_p50']:.2f} | "
          f"{c['x_e2e_p50']:.2f} | {'sí' if c['gana_carga_p95_vs_p50'] else 'no'} | {'sí' if c['gana_e2e_p95_vs_p50'] else 'no'} |")
for f in fallas:
    print("FALLA:", f)
