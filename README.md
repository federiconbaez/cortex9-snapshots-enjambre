# 03 — Snapshots de enjambre (`enjambre_snap`)

Hipervisor de snapshots de memoria para congelar y restaurar el estado de un
enjambre de procesos-agente locales. Parte de la prueba de Federico (sistema #3).

**Dueña:** Federica Memory Mycelium  
**Ruta:** `/workspace/sistemas/03-snapshots-enjambre`  
**Lenguaje:** Rust (edition 2021), `cargo`/`rustc` 1.85.1

## Qué hace

1. **Congela** N procesos con `SIGSTOP` (camino verificado) o intenta el freezer
   de cgroup v2 (en esta máquina: no verificado, ver abajo).
2. **Volca** la memoria escribible vía `/proc/pid/maps` + `process_vm_readv`
   (fallback `/proc/pid/mem`) a un directorio con `manifiesto.json` y un
   archivo por región, cada uno con **sha256**.
3. **Restaura** la región del agente de prueba (dirección fija `0x5f0000000000`)
   en tres modos:
   - `completo`: `mmap` anónimo + `memcpy` de toda la imagen.
   - `mmap`: `mmap` privado del archivo de región (el kernel pagina desde disco).
   - `uffd`: `mmap` anónimo + `userfaultfd` MISSING; un hilo entrega páginas con
     `UFFDIO_COPY` bajo demanda. Se mide por separado el tiempo hasta **listo
     para ejecutar** (uffd registrado) y el de **carga completa** (todas las
     páginas materializadas).

4. **Enjambre con consenso** (`enjambre`, `consenso`, binarios `restaurador` y
   `bench_enjambre`): N agentes comparten un segmento en `/dev/shm`. Una
   **barrera cooperativa** deja al enjambre en un **corte consistente**
   (ningún agente a mitad de paso); recién ahí se congela con `SIGSTOP` y se
   vuelcan los N **en paralelo**. Se matan los originales y se restaura el
   enjambre en **N procesos nuevos** que arrancan a la vez (largada por un
   atómico compartido, un solo `CLOCK_MONOTONIC`). Tras restaurar se exige:
   - sha256 de cada heap restaurado == sha del volcado;
   - **hash conjunto** `sha256(sha_0 ‖ … ‖ sha_{N-1} ‖ segmento)` idéntico;
   - invariante `total_compartido == Σ contador_i` y `espejo[i] == contador_i`;
   - **reanudación coordinada**: se levanta la pausa, cada agente da K pasos y
     se exige `total' == total + N·K` y `contador_i' == contador_i + K`.
   - Además, una **réplica independiente** reconstruye cada heap desde
     `(semilla, contador)` y coincide bit a bit (el estado es determinista).
5. `uffd16`: variante de userfaultfd con **ventana de prefetch** de 16 páginas
   (64 KiB por `UFFDIO_COPY`, con mapa de bits de páginas servidas).
   La imagen se mapea en solo lectura como fuente: no hay copia previa.

El agente de prueba (`agente`) guarda **todo** su estado en un único `mmap`
anónimo en esa dirección fija. Así, tras restaurar la región, los punteros
internos siguen siendo válidos. Es el camino cooperativo verificable.

## Qué NO hace

- **No restaura procesos ajenos completos** (registros, fds, hilos, sockets) como
  haría CRIU. Eso requeriría un checkpoint completo del kernel.
- **No alcanza la meta de Federico de &lt;5 ms para cargar toda la memoria** de un
  enjambre de decenas/cientos de MiB. Esa cifra **sí** aparece en el tiempo hasta
  “listo” con restauración diferida (`uffd` / `mmap`), no en la carga completa.
- **No usa CRIU**: en este kernel CRIU no puede ni volcar un proceso propio.
  `criu dump` muere en `kcmp-ids.c:108: kcmp failed ... Function not implemented`
  → `Can't make VM id` → `Dumping FAILED`. La syscall `kcmp(2)` devuelve
  `ENOSYS` (kernel sin `CONFIG_KCMP`/`CHECKPOINT_RESTORE`) y no existe
  `/proc/sys/kernel/ns_last_pid`. No hay workaround real: `--unprivileged`
  solo cambia el requisito de capacidades, no la falta de `kcmp`.
  Evidencia: `scripts/criu-intento.sh` → `resultados/criu-intento-2026-10-20T2311-ART.txt`.
- **No congela con cgroup freezer** aquí: mover pids a un cgroup hijo bajo
  `/sys/fs/cgroup/agent` devuelve `ENOTSUP`.

## Cómo reproducir

```bash
cd /workspace/sistemas/03-snapshots-enjambre
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test -- --test-threads=1

# tests que ejercitan userfaultfd (hace falta root; vm.unprivileged_userfaultfd=0)
sudo -n cargo test --test integracion -- --test-threads=1 --nocapture

# demo punta a punta
./target/release/demo_roundtrip --tamano 4M --modo completo
./target/release/demo_roundtrip --tamano 4M --modo mmap
sudo -n ./target/release/demo_roundtrip --tamano 4M --modo uffd

# benchmarks
sudo -n ./target/release/bench_snap \
  --tamanos 4M,64M,256M --agentes 1,2 --reps 5 --out /tmp/bench-snap

# benchmark de ENJAMBRE con consenso (uffd usa `sudo -n` solo en los restauradores)
./target/release/bench_enjambre --tamanos 4M,64M --agentes 1,4,8,16 --reps 5 \
  --modos uffd,uffd16,completo,mmap --reanudar 1000
# → resultados/enjambre-<fecha>T<hhmm>-ART.json

# sonda e intento real de CRIU (guarda el log en resultados/)
./target/release/snap criu-probe
./scripts/criu-intento.sh
```

Binarios: `agente`, `snap`, `restaurador`, `bench_enjambre`, `bench_snap`, `demo_roundtrip`, `uffd_debug` (diagnóstico).

## Tests (15)

`cargo test -- --test-threads=1`:

- Unitarios (6): tamaños, sha conocido, parseo de `/proc/pid/maps`, heap
  determinista, **barrera entre hilos** (durante la pausa nadie avanza y
  `Σ espejo == total`), hash conjunto sensible al orden.
- `tests/consenso.rs` (5, entre **procesos** reales):
  - `enjambre_4_consenso_completo_y_mmap`: 4 agentes → volcado → 4 restauradores nuevos, consenso + 500 pasos.
  - `enjambre_8_consenso_uffd`: 8 agentes, modos `uffd` y `uffd16` (restauradores con `sudo -n`; se salta solo si no hay root ni sudo) y exige fallos uffd reales.
  - `replica_determinista_coincide_bit_a_bit`: reconstrucción independiente == volcado.
  - `detecta_heap_alterado`: 1 byte cambiado en un heap → sha, consenso y hash conjunto fallan.
  - `detecta_contador_compartido_alterado`: total +1 en el segmento → falla el invariante.
- `tests/integracion.rs` (4): roundtrip, SIGSTOP de 3, uffd y cgroup (estos dos
  últimos imprimen SKIP si el proceso de test no es root).

## Resultados medidos — un agente (ronda 1)

**Máquina:** Linux `6.12.94+`, 8 núcleos, 15 GiB RAM, sin GPU, Debian 13.  
**Fecha/hora:** 2026-10-20 22:55 ART (UTC-03:00). *Corrección:* la versión
anterior decía «2026-10-21 01:55 ART», pero esa hora era UTC (la caja corre en
UTC); el nombre del archivo conserva esa fecha UTC para no romper referencias.  
**JSON crudo:** `resultados/bench-2026-10-21-ART.json`  
**Método:** mediana de 5 repeticiones; volcado solo de regiones anónimas escribibles.

| tamaño | agentes | fase | modo | mediana_ms | &lt;5 ms |
|--------|---------|------|------|------------|--------|
| 4 MiB | 1 | congelar | sigstop | 0.032 | SÍ |
| 4 MiB | 1 | volcar | anonimas | 9.544 | no |
| 4 MiB | 1 | restaurar_listo | completo | 3.259 | SÍ |
| 4 MiB | 1 | restaurar_listo | mmap | 0.004 | SÍ |
| 4 MiB | 1 | restaurar_listo | uffd | 0.023 | SÍ |
| 4 MiB | 1 | restaurar_carga_completa | uffd | 6.677 | no |
| 4 MiB | 2 | congelar | sigstop | 0.042 | SÍ |
| 4 MiB | 2 | volcar | anonimas | 19.160 | no |
| 64 MiB | 1 | congelar | sigstop | 0.040 | SÍ |
| 64 MiB | 1 | volcar | anonimas | 173.918 | no |
| 64 MiB | 1 | restaurar_listo | completo | 80.049 | no |
| 64 MiB | 1 | restaurar_listo | mmap | 0.004 | SÍ |
| 64 MiB | 1 | restaurar_listo | uffd | 0.025 | SÍ |
| 64 MiB | 1 | restaurar_carga_completa | mmap | 2.208 | SÍ |
| 64 MiB | 1 | restaurar_carga_completa | uffd | 186.893 | no |
| 256 MiB | 1 | congelar | sigstop | 0.042 | SÍ |
| 256 MiB | 1 | volcar | anonimas | 759.821 | no |
| 256 MiB | 1 | restaurar_listo | completo | 344.497 | no |
| 256 MiB | 1 | restaurar_listo | mmap | 0.004 | SÍ |
| 256 MiB | 1 | restaurar_listo | uffd | 0.033 | SÍ |
| 256 MiB | 1 | restaurar_carga_completa | mmap | 14.208 | no |
| 256 MiB | 1 | restaurar_carga_completa | uffd | 773.633 | no |

Lectura honesta frente a la meta **&lt;5 ms para el enjambre**:

- **Congelar** con `SIGSTOP` está muy por debajo de 5 ms (decenas de µs).
- **Listo para ejecutar** con `uffd` o `mmap` también (&lt;0.1 ms), porque no se
  copia la memoria todavía.
- **Volcar** y **carga completa** escalan con los bytes y **no** cumplen &lt;5 ms
  salvo heaps chicos (≈4 MiB) en algunos modos.
- La objeción de Ben / Research Sentinel se confirma con números: &lt;5 ms solo es
  realista con poca memoria o con restauración diferida (y midiendo “listo”, no
  “todo en RAM”).

## Resultados medidos — ENJAMBRE con consenso (ronda 2)

**Fecha/hora:** 2026-10-20 23:11:50 ART (UTC-03:00) · **JSON:** `resultados/enjambre-2026-10-20T2311-ART.json`  
**Máquina:** la misma (8 núcleos, 15 GiB, compartida con otras agentes en carga).  
**Método:** mediana de 5 repeticiones por celda; 160 restauraciones de enjambre,
**160/160 con consenso verificado** (sha por agente, hash conjunto, invariante y
reanudación de 1000 pasos por agente). Tiempos de enjambre = `max_i(t_i) − largada`.
Imagen caliente en page cache (recién volcada). Negrita = mediana < 5 ms.

| heap | N | congelar_todos | volcar_todos | listo uffd | listo uffd16 | carga uffd | carga uffd16 | carga completo | carga mmap* | barrera |
|---|---|---|---|---|---|---|---|---|---|---|
| 4 MiB | 1 | **0.045** | 10.00 | **0.062** | **0.054** | 11.69 | **2.825** | **2.316** | **0.199** | **0.002** |
| 4 MiB | 4 | **0.073** | 10.18 | **0.082** | **0.090** | 16.76 | **3.252** | **1.920** | **0.230** | **0.030** |
| 4 MiB | 8 | **0.460** | 20.10 | **0.278** | **0.947** | 17.48 | 7.81 | 6.44 | **4.976** | **0.062** |
| 4 MiB | 16 | **1.410** | 33.42 | **3.439** | **2.771** | 25.37 | 8.74 | 17.58 | 12.00 | **2.104** |
| 64 MiB | 1 | **0.053** | 163.2 | **0.052** | **0.052** | 203.7 | 38.30 | 38.98 | **3.305** | **0.002** |
| 64 MiB | 4 | **0.085** | 178.4 | **0.061** | **0.065** | 219.9 | 39.69 | 38.37 | **2.919** | **0.003** |
| 64 MiB | 8 | **0.568** | 263.6 | **0.470** | **0.822** | 190.2 | 66.59 | 79.92 | 11.50 | **0.254** |
| 64 MiB | 16 | **1.108** | 739.2 | **3.267** | **3.784** | 420.6 | 120.7 | 154.7 | 19.98 | **3.996** |

(ms. `carga` = `restaurar_carga_completa`; también están en el JSON
`restaurar_primer_acceso` y `restaurar_listo_por_agente`.)
\* `mmap`: las páginas quedan **compartidas con el page cache** y la copia
privada ocurre recién al escribir (CoW); no es «todo copiado en RAM propia».

Lectura honesta frente a la meta **«congelar y restaurar el enjambre en < 5 ms»**:

- **Congelar el enjambre**: sí, hasta N=16 y 64 MiB (≤ 1,4 ms con `SIGSTOP`).
- **Enjambre listo para ejecutar con uffd**: sí, < 5 ms hasta N=16 en ambos
  tamaños (≈ 0,05–0,5 ms con N ≤ 8; ≈ 3,3–3,8 ms con N=16, porque 16
  restauradores compiten por 8 núcleos). El primer acceso real (fallo de página
  servido) también queda < 5 ms.
- **Volcar**: **no**. 10 ms (4 MiB) a 739 ms (16 × 64 MiB). El costo es
  `process_vm_readv` + sha256 + escritura, y escala con los bytes.
- **Carga completa**: **no**, salvo heaps chicos. `uffd16` es 4–5× más rápido
  que `uffd` página a página, pero 64 MiB sigue en ≈ 38–120 ms.
- Conclusión: el ciclo completo «congelar + volcar + restaurar todo en RAM» en
  < 5 ms queda **refutado** para un enjambre real; lo que sí se cumple es
  *congelar* y *dejar listo para ejecutar* con restauración diferida.

## No verificado

| Ítem | Motivo |
|------|--------|
| Restauración completa estilo CRIU de procesos arbitrarios (registros, fds, hilos, sockets) | `criu` 4.1.1 instalado. `criu check`: *Does not look good* (diag de sockets ausente, `ns_last_pid` inaccesible). `criu dump --leave-running` de un agente propio: `kcmp failed ... Function not implemented` → `Dumping FAILED`. `kcmp(2)` = `ENOSYS` en este kernel. Sin workaround real. |
| Consenso entre agentes **no cooperativos** | El corte consistente depende de que cada agente pase por `punto_seguro` entre pasos. Un proceso ajeno no tiene barrera: `SIGSTOP` solo no garantiza el invariante. |
| Imagen fría (NVMe, no page cache) | Todas las cifras de restauración son con la imagen recién escrita, caliente en page cache. No se midió desde disco frío. |
| N > núcleos sin ruido | La máquina es compartida con otras agentes; con N=16 en 8 núcleos hay colas de scheduler (se ve en `max_ms` del JSON). |
| `userfaultfd` sin root | `vm.unprivileged_userfaultfd=0`; syscall y `/dev/userfaultfd` requieren root. Los benches/demos `uffd` se corren con `sudo -n`. |
| Congelado por cgroup v2 freezer | Crear/mover pids a un hijo de `/sys/fs/cgroup/agent` → `ENOTSUP`. El código está, la prueba lo marca como no verificado. |
| Meta &lt;5 ms carga completa del enjambre | **Refutada** por las mediciones arriba para ≥64 MiB, y para 4 MiB con N ≥ 8. |
| Meta &lt;5 ms volcado del enjambre | **Refutada**: ≥ 10 ms aun con 1 agente de 4 MiB. |
| GPU / NVML / sched_ext | Fuera de alcance de este repo; esta máquina no los tiene. |

## Criterio de “verificado” (Memory)

Un cambio en este repo solo se anota como verificado si:

1. `cargo build --release` OK  
2. `cargo clippy --all-targets -- -D warnings` limpio  
3. `cargo test -- --test-threads=1` en verde (y con `sudo -n` el camino uffd)  
4. Hay cifra medida (no prometida) para la métrica que se reivindica  
5. Lo que no corre en este hardware queda en **No verificado**, no se maquilla

## Layout del snapshot

```text
snap/pid-1234/
  manifiesto.json
  regions/
    0000.bin
    ...
```
