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

El agente de prueba (`agente`) guarda **todo** su estado en un único `mmap`
anónimo en esa dirección fija. Así, tras restaurar la región, los punteros
internos siguen siendo válidos. Es el camino cooperativo verificable.

## Qué NO hace

- **No restaura procesos ajenos completos** (registros, fds, hilos, sockets) como
  haría CRIU. Eso requeriría un checkpoint completo del kernel.
- **No alcanza la meta de Federico de &lt;5 ms para cargar toda la memoria** de un
  enjambre de decenas/cientos de MiB. Esa cifra **sí** aparece en el tiempo hasta
  “listo” con restauración diferida (`uffd` / `mmap`), no en la carga completa.
- **No usa CRIU** en el camino verificado: `criu check` falla en este entorno
  (faltan módulos diag de sockets y `ns_last_pid`).
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

# sonda CRIU
./target/release/snap criu-probe
```

Binarios: `agente`, `snap`, `bench_snap`, `demo_roundtrip`, `uffd_debug` (diagnóstico).

## Resultados medidos

**Máquina:** Linux `6.12.94+`, 8 núcleos, 15 GiB RAM, sin GPU, Debian 13.  
**Fecha/hora (ART, America/Buenos_Aires):** 2026-10-21 01:55 ART (America/Buenos_Aires, UTC-3).  
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

## No verificado

| Ítem | Motivo |
|------|--------|
| Restauración completa estilo CRIU de procesos arbitrarios | `criu` 4.1.1 instalado (`/usr/sbin/criu`) pero `criu check` reporta *Does not look good* (sockets diag missing, `ns_last_pid` inaccesible). No se afirma checkpoint/restore de procesos ajenos. |
| `userfaultfd` sin root | `vm.unprivileged_userfaultfd=0`; syscall y `/dev/userfaultfd` requieren root. Los benches/demos `uffd` se corren con `sudo -n`. |
| Congelado por cgroup v2 freezer | Crear/mover pids a un hijo de `/sys/fs/cgroup/agent` → `ENOTSUP`. El código está, la prueba lo marca como no verificado. |
| Meta &lt;5 ms carga completa del enjambre | **Refutada** por las mediciones arriba para ≥64 MiB. |
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
