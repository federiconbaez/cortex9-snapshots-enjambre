# AUDIT-valor — sistema 3 (snapshots-enjambre)

**Auditora:** Federica Research Sentinel · **Fecha:** 2026-10-21 00:20 ART (UTC-03:00)
**Objeto:** commit `07f8ee2` (sobre `ac3b1f8`), `BENCH-valor.md`, `resultados/valor-2026-10-21T0010-ART.json`.

## Umbral #3 (literal)

Restaurar N agentes listos para consenso con **mediana < 5 ms**, éxito ≥ 0,99, **y** ese tiempo ≤ 0,10× el arranque
en frío (`frio_reconstruir`). Métrica principal de la auditoría: **e2e** (primer spawn → último `estado`), que es
«N agentes vivos con su estado». `carga` (sin exec ni apertura) se informa como lectura laxa.

## Verificaciones

- HEAD `07f8ee2`, árbol limpio.
- `cargo clippy --all-targets -- -D warnings`: limpio. `cargo test --release`: 15/15 (6 + 5 + 4).
- JSON commiteado: 1620 muestras, 0 fallas; las 1350 no-`frio_blanco` con sha256 por agente == sha del volcado y contador igual.
  Estado idéntico se verifica con **sha256 de la región completa** + contador (`bench_valor.rs::levantar`).
- Todos los números citados por Memory coinciden con el JSON: 64M/1/3 s 1104,4 / 79,9 / 45,7 ms; 64M/16/3 s 373,9 / 272,2 / 177,1 ms;
  mmap gana 18/18; copia pierde en 3 (0,89–0,92×). En 4M/1, **3,7 y 5,8 ms son medianas de dos variantes** (mmap / copia), no un rango.

## Rerun de auditoría (una corrida, 5 reps, clon aparte)

`bench_valor` de `07f8ee2`, 18 celdas × {snap_completo, snap_mmap, frio_reconstruir, frio_blanco}, **sin uffd** (no entra en el reclamo y
quita contención). 00:13–00:16 ART, load 2,8→12. 360 muestras, 0 fallas, 0 estados distintos. Crudo:
`resultados/auditoria/rerun-valor-2026-10-21T0016-ART.json`; análisis: `python3 scripts/auditoria_valor.py <json>`.
Reproduce el original dentro de ±10 % en todas las celdas relevantes y da **los mismos conteos de celdas que pasan**.

## Celdas que cumplen (e2e, mediana; original n=15 / rerun n=5)

| celda | frío p50 | mmap p50 (p90) | ratio | copia p50 (p90) | ratio |
|---|---|---|---|---|---|
| 4M/1/30 | 14,2 | **3,7** (12,0) | 0,26 ✗ | 5,7 (14,9) | 0,40 ✗ |
| 4M/1/300 | 95,5 | **3,6** (9,2) | **0,038 ✓✓** | 5,5 (17,3) | 0,057 (≥5 ms ✗) |
| 4M/1/3000 | 853,9 | **3,7** (11,1) | **0,004 ✓✓** | 5,8 (14,6) | 0,007 (≥5 ms ✗) |
| 4M/4/3000 | 142,0 | **4,3** (5,0) | **0,030 ✓✓** | 6,2 (7,6) | 0,043 (≥5 ms ✗) |
| 64M/1/3000 | 1104,4 | 45,7 | 0,041 (≥5 ms ✗) | 79,9 | 0,072 (≥5 ms ✗) |
| resto (13 celdas) | — | — | ✗ | — | ✗ |

Conteos (e2e), iguales en original y rerun:

| variante | mediana < 5 ms | ratio ≤ 0,10× | **ambos** |
|---|---|---|---|
| snap_mmap | 6/18 | 4/18 | **3/18** (4M/1/300, 4M/1/3000, 4M/4/3000) |
| snap_completo | 0/18 | 4/18 | **0/18** |

Lectura laxa (`carga`, sin exec): mmap 12/18 ambos; copia 3/18 (4M/1/300, 4M/1/3000, 4M/4/3000). Ninguna celda N=16 pasa en ninguna métrica.

## El ratio depende del warm-up

`frio_reconstruir` rehace el mismo bucle `inicializar` + `paso` del agente, **sin sleep** (el agente original duerme 1 µs cada
10 000 pasos; la reconstrucción no). La base no está inflada: rehace 3000 ms de trabajo en 0,1–1,1 s. Pero su costo es lineal en el
warm-up (ajuste sobre p50 e2e: 4M/1 ≈ 5,7 + 0,283·cal ms; 64M/1 ≈ 82,5 + 0,341·cal; 4M/4 ≈ 7,1 + 0,045·cal; 64M/16 ≈ 258 + 0,039·cal),
así que ≤ 0,10× se cumple con cualquier restore si el warm-up es lo bastante largo. Warm-up a partir del cual ratio ≤ 0,10× es automático:

| celda | mmap | copia |
|---|---|---|
| 4M/1 | ≈ 110 ms | ≈ 180 ms |
| 4M/4 | ≈ 780 ms | ≈ 1,2 s |
| 64M/1 | ≈ 1,1 s | ≈ 2,0 s |
| 4M/16 | ≈ 6,5 s | ≈ 8,7 s |
| 64M/4 | ≈ 6,0 s | ≈ 11 s |
| 64M/16 | ≈ 41 s | ≈ 66 s |

El ratio no mide el mecanismo: mide cuánto trabajo sintético se eligió. La condición discriminante es la mediana < 5 ms, y esa
la cumple solo mmap con 4 MiB y N ≤ 4.

## Veredicto

- **FALLA en general.** Pasa solo con **snap_mmap** en 3/18 celdas (4M/1/300, 4M/1/3000, 4M/4/3000). Copia completa: **0/18** en e2e.
- Las 3 celdas que pasan lo hacen porque el warm-up supera el umbral de trivialidad (tabla anterior); no prueban valor general.
- Éxito 1,00 (1620/1620 + 360/360), sha256 idéntico: cumple.
- «B+ condicional; el umbral no se cumple en general» es **correcto en el fondo**. Hay que precisar dos cosas: «cumple con 4 MB/1 agente
  3,7–5,8 ms» vale **solo para mmap** (la copia a 5,8 ms no pasa) y mmap además pasa 4M/4/3000.

## Caveats

- `snap_mmap` no copia a RAM propia. `materializar` hace lecturas que mapean páginas del page cache (CoW recién al escribir), y la imagen
  estaba **caliente** (recién volcada). Sin root no se puede vaciar la caché (`drop_caches`), así que desde disco frío queda sin medir y
  probablemente sería peor.
- El e2e incluye el sha256 de verificación de toda la región en ambas ramas (en 64 MiB son decenas de ms).
- Agente sintético y determinista; sin CRIU (`kcmp` ENOSYS); 8 núcleos compartidos (N=16 satura); el rerun es de una sola corrida.
