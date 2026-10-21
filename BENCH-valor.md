# BENCH-valor — sistema 3: ¿el snapshot le gana al arranque en frío?

**Fecha:** 2026-10-21 00:10 ART (UTC-03:00), al cierre de las 3 corridas (00:00–00:10 ART).
**Máquina:** la caja compartida. Linux 6.12, 8 núcleos, 15 GiB, sin GPU, **sin CRIU** (`kcmp` = ENOSYS).
Load average de 4 a 16 durante la medición: hay otras agentes, y además los 16 agentes propios.
**JSON resumido:** `resultados/valor-2026-10-21T0010-ART.json`. Los crudos por corrida van a `build/valor/` (ignorado por git).
**Reproducir:** `./scripts/valor.sh` (build + 3 corridas + `scripts/valor_resumen.py`).

## Métrica de valor (del objetivo de valor, fila 3)

La métrica es el **arranque en frío del enjambre**: tiempo hasta que los N agentes vuelven a estar vivos **con el mismo estado** que tenían.
La línea base del objetivo es «matar y relanzar».

## Diseño

Para cada combinación de heap ∈ {4 MiB, 64 MiB}, N ∈ {1, 4, 16} y calentamiento ∈ {30, 300, 3000} ms:

1. Se lanzan N agentes, se los deja trabajar durante el calentamiento y se toma el snapshot: barrera de consenso, `SIGSTOP`, volcado y `SIGKILL`.
2. Se vuelve a levantar el enjambre en N procesos nuevos, una vez por variante:
   - **`frio_reconstruir` (LÍNEA BASE):** `arranque_frio` mapea el heap, lo inicializa desde la semilla y rehace los mismos pasos que había dado el original. Llega al **mismo estado**.
   - **`frio_blanco`:** igual pero sin pasos. Arranca, pero **pierde el estado**. Es el «matar y relanzar» literal y una cota inferior.
   - **`snap_completo`:** `restaurador` copia la imagen entera a RAM propia (pread).
   - **`snap_mmap`:** mmap privado del archivo. Las páginas quedan compartidas con el page cache y se copian al escribir (CoW).
   - **`snap_uffd`, `snap_uffd16`:** restauración diferida con userfaultfd, página a página o en ventanas de 64 KiB. Corren con `sudo -n`.
3. Se verifica que el sha256 de cada heap coincida con el volcado y que los contadores sean iguales. Se exige en todas las variantes salvo `frio_blanco`.

Métricas (máximo sobre los N agentes, un solo reloj `CLOCK_MONOTONIC`):

- **carga:** desde la largada común hasta que el último agente tiene todo su estado.
- **e2e:** desde el primer `spawn` hasta el último `estado`. Incluye exec, apertura de la imagen y, en uffd, el `sudo`.

Cada celda junta 3 corridas independientes × 5 repeticiones = **15 muestras**. Los percentiles se calculan por rango más cercano.
En total hay **1620 levantadas de enjambre**, con **0 fallas** y estado sha-idéntico en todas las variantes que lo requieren (1350 de 1350).
El orden de las variantes rota en cada repetición.

## Resultados — carga, p50 / p95 (ms)

| heap | N | calent. | frio_reconstruir | frio_blanco | snap_completo | snap_mmap | snap_uffd16 | snap_uffd |
|---|---|---|---|---|---|---|---|---|
| 4M | 1 | 30 ms | 11.0 / 16.0 | 2.4 / 6.9 | 2.0 / 5.3 | 0.2 / 4.0 | 2.2 / 14.2 | 12.9 / 14.2 |
| 4M | 1 | 300 ms | 91.8 / 170.9 | 2.7 / 5.4 | 2.0 / 5.5 | 0.2 / 4.4 | 3.0 / 10.2 | 12.5 / 32.4 |
| 4M | 1 | 3000 ms | 850.4 / 1463.6 | 2.7 / 3.9 | 2.3 / 7.4 | 0.2 / 4.5 | 2.6 / 5.3 | 12.8 / 16.1 |
| 4M | 4 | 30 ms | 4.5 / 6.4 | 2.5 / 2.8 | 2.3 / 2.7 | 0.2 / 0.3 | 2.7 / 6.6 | 14.8 / 18.2 |
| 4M | 4 | 300 ms | 16.8 / 27.9 | 2.6 / 6.7 | 2.2 / 3.0 | 0.2 / 0.3 | 3.0 / 9.7 | 15.8 / 19.4 |
| 4M | 4 | 3000 ms | 138.1 / 228.3 | 2.9 / 4.6 | 2.2 / 3.3 | 0.2 / 0.3 | 3.0 / 3.9 | 15.5 / 21.7 |
| 4M | 16 | 30 ms | 13.7 / 18.3 | 11.5 / 17.7 | 12.7 / 16.9 | 10.5 / 14.3 | 7.0 / 10.4 | 24.3 / 31.3 |
| 4M | 16 | 300 ms | 21.1 / 28.0 | 11.8 / 15.7 | 12.6 / 15.2 | 10.0 / 12.6 | 7.4 / 10.6 | 24.5 / 32.4 |
| 4M | 16 | 3000 ms | 92.2 / 112.4 | 12.6 / 17.2 | 13.7 / 16.4 | 9.5 / 14.9 | 6.8 / 11.1 | 22.7 / 32.8 |
| 64M | 1 | 30 ms | 50.0 / 59.2 | 39.9 / 77.7 | 32.2 / 45.2 | 2.7 / 4.5 | 37.0 / 59.9 | 180.4 / 200.4 |
| 64M | 1 | 300 ms | 141.7 / 175.5 | 38.6 / 41.1 | 33.1 / 39.2 | 2.9 / 3.6 | 36.0 / 77.8 | 186.3 / 251.2 |
| 64M | 1 | 3000 ms | 1060.3 / 1316.5 | 44.6 / 86.9 | 37.9 / 64.5 | 3.3 / 6.2 | 36.4 / 43.7 | 191.8 / 240.4 |
| 64M | 4 | 30 ms | 47.3 / 66.4 | 41.5 / 53.2 | 37.5 / 53.0 | 3.2 / 17.7 | 40.8 / 46.2 | 229.0 / 253.1 |
| 64M | 4 | 300 ms | 63.4 / 123.1 | 41.3 / 85.0 | 37.2 / 45.2 | 3.7 / 4.5 | 40.5 / 66.8 | 228.7 / 286.5 |
| 64M | 4 | 3000 ms | 242.5 / 349.9 | 41.7 / 46.7 | 37.4 / 43.0 | 3.5 / 8.8 | 41.7 / 55.9 | 219.2 / 265.1 |
| 64M | 16 | 30 ms | 180.0 / 212.5 | 170.9 / 219.3 | 163.3 / 213.8 | 19.7 / 62.4 | 95.8 / 163.0 | 322.3 / 489.4 |
| 64M | 16 | 300 ms | 185.2 / 226.3 | 163.7 / 202.9 | 165.2 / 220.4 | 21.2 / 32.8 | 96.3 / 105.3 | 327.6 / 411.2 |
| 64M | 16 | 3000 ms | 310.4 / 418.9 | 171.0 / 200.1 | 165.2 / 200.6 | 20.0 / 46.8 | 99.2 / 153.0 | 361.4 / 414.8 |

## Resultados — e2e, p50 / p95 (ms)

| heap | N | calent. | frio_reconstruir | frio_blanco | snap_completo | snap_mmap | snap_uffd16 | snap_uffd |
|---|---|---|---|---|---|---|---|---|
| 4M | 1 | 30 ms | 14.2 / 24.4 | 5.9 / 11.8 | 5.7 / 14.9 | 3.7 / 12.0 | 32.3 / 50.3 | 43.7 / 52.7 |
| 4M | 1 | 300 ms | 95.5 / 175.0 | 6.5 / 12.4 | 5.5 / 17.3 | 3.6 / 9.2 | 33.6 / 51.4 | 43.1 / 69.4 |
| 4M | 1 | 3000 ms | 853.9 / 1472.2 | 6.2 / 7.6 | 5.8 / 14.6 | 3.7 / 11.1 | 33.2 / 44.2 | 44.2 / 47.3 |
| 4M | 4 | 30 ms | 8.5 / 10.5 | 6.6 / 7.3 | 6.2 / 8.1 | 4.2 / 6.2 | 33.3 / 37.5 | 45.0 / 50.8 |
| 4M | 4 | 300 ms | 21.0 / 32.8 | 6.6 / 12.9 | 6.4 / 8.2 | 4.2 / 10.6 | 33.4 / 43.0 | 47.1 / 50.0 |
| 4M | 4 | 3000 ms | 142.0 / 232.8 | 6.9 / 8.3 | 6.2 / 7.6 | 4.3 / 5.0 | 33.1 / 40.4 | 46.6 / 51.5 |
| 4M | 16 | 30 ms | 23.4 / 28.9 | 20.5 / 26.4 | 25.3 / 28.9 | 19.2 / 23.8 | 67.0 / 78.1 | 76.2 / 87.3 |
| 4M | 16 | 300 ms | 29.5 / 36.5 | 21.6 / 25.4 | 23.6 / 25.6 | 19.5 / 21.7 | 66.2 / 73.9 | 74.7 / 84.9 |
| 4M | 16 | 3000 ms | 100.4 / 120.2 | 21.8 / 26.2 | 25.2 / 36.5 | 19.0 / 24.6 | 66.6 / 71.8 | 77.1 / 83.1 |
| 64M | 1 | 30 ms | 92.7 / 100.6 | 82.5 / 134.2 | 74.4 / 90.7 | 45.9 / 65.7 | 106.6 / 150.6 | 249.6 / 271.9 |
| 64M | 1 | 300 ms | 182.5 / 217.1 | 80.5 / 101.9 | 73.6 / 81.8 | 44.9 / 47.2 | 104.4 / 172.9 | 256.1 / 333.5 |
| 64M | 1 | 3000 ms | 1104.4 / 1362.1 | 86.0 / 182.9 | 79.9 / 151.8 | 45.7 / 54.7 | 107.3 / 147.9 | 265.4 / 311.7 |
| 64M | 4 | 30 ms | 90.8 / 119.9 | 84.3 / 101.3 | 82.6 / 95.8 | 47.3 / 62.0 | 112.0 / 133.4 | 306.3 / 330.6 |
| 64M | 4 | 300 ms | 106.7 / 208.3 | 84.3 / 175.4 | 80.4 / 99.5 | 47.2 / 56.5 | 109.4 / 152.3 | 297.8 / 379.8 |
| 64M | 4 | 3000 ms | 285.6 / 391.3 | 86.0 / 94.5 | 80.8 / 92.7 | 49.3 / 65.3 | 115.5 / 133.7 | 290.0 / 342.4 |
| 64M | 16 | 30 ms | 259.0 / 369.0 | 246.3 / 353.3 | 286.6 / 414.2 | 198.7 / 288.0 | 292.6 / 395.9 | 496.4 / 671.3 |
| 64M | 16 | 300 ms | 248.5 / 299.5 | 224.5 / 281.8 | 280.7 / 304.5 | 176.4 / 222.4 | 295.0 / 330.5 | 464.3 / 557.9 |
| 64M | 16 | 3000 ms | 373.9 / 583.1 | 228.2 / 410.8 | 272.2 / 373.5 | 177.1 / 220.6 | 281.6 / 440.8 | 470.9 / 624.8 |

## Veredicto contra la línea base (`frio_reconstruir`, e2e)

| variante | celdas con p50 mejor | celdas con p95 < p50 de la base | rango de mejora en p50 |
|---|---|---|---|
| snap_completo | 15/18 | 13/18 | 0.89× – 147.77× |
| snap_mmap | 18/18 | 16/18 | 1.22× – 230.54× |
| snap_uffd16 | 8/18 | 7/18 | 0.25× – 25.74× |
| snap_uffd | 5/18 | 5/18 | 0.19× – 19.30× |

En carga (sin exec ni sudo), `snap_completo` le gana a la base en p50 en las 18 celdas. Pero en tres de ellas la diferencia es apenas de 1,08 a 1,12×: 4M/N=16/30 ms, 64M/N=16/30 ms y 64M/N=16/300 ms.

## Lectura honesta

- **Nivel alcanzado: B+ condicional.** El mecanismo está medido y le gana a la línea base simple con margen y en la cola, pero **solo cuando reconstruir el estado cuesta más que copiar la imagen**.
  - Con 3 s de calentamiento, `snap_completo` gana de 1,4× a 148× en e2e, y `snap_mmap` de 2,1× a 230×.
  - Con 30 ms de calentamiento y N=16, la ventaja desaparece: `snap_completo` pierde en e2e (0,89–0,92×) y `snap_mmap` gana apenas 1,2–1,4×, sin margen en p95.
- **No es nivel C.** El agente es sintético: su estado se reconstruye de forma determinista con un xorshift y un contador. Un agente real (LLM con contexto, cachés, conexiones) tiene un estado más caro de reconstruir, o directamente imposible, y eso favorecería al snapshot. Pero **eso no está medido**.
- **La base está favorecida.** La reconstrucción rehace los pasos sin barrera ni pausas, así que tarda **menos** que el calentamiento real (3000 ms de trabajo original se rehacen en 0,09–1,1 s). Si se toma el tiempo real que costó llegar al estado, la ventaja del snapshot es mayor que la de las tablas.
- **Contra `frio_blanco`**, que pierde el estado, `snap_completo` empata: copiar 64 MiB cuesta lo mismo que llenarlos (≈ 32–41 ms con N=1). `snap_mmap` es más rápido porque no copia, pero ver la salvedad siguiente.
- **`snap_mmap` no deja todo en RAM propia.** Comparte páginas con el page cache y cada escritura dispara una copia CoW. Además, la imagen estaba **caliente** en el page cache (recién volcada). No se midió desde disco frío.
- **uffd pierde en casi todo.**
  - Página a página es lento: 180–360 ms para 64 MiB.
  - En e2e se suman unos 30–40 ms de `sudo -n`, porque `vm.unprivileged_userfaultfd=0` obliga a usar root.
  - Su valor es otro: queda **listo para ejecutar** en ≤ 3,6 ms aun con 16 × 64 MiB. Sirve si el agente no toca todo su heap enseguida, y eso este experimento no lo mide.
- **N=16 en 8 núcleos** satura la máquina, que además estaba compartida (load average hasta 16). La dispersión entre corridas se ve en la columna «p50 por corrida» del JSON. Las conclusiones de arriba se mantienen en las 3 corridas.
- **El alcance es el mismo que el del sistema 3:**
  - restaurar un heap cooperativo en una dirección fija, no procesos arbitrarios;
  - **sin CRIU**, porque `kcmp` da ENOSYS en esta caja;
  - restauración diferida solo en los modos uffd y mmap.
