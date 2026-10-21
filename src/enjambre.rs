//! Orquestación de un enjambre de N agentes cooperativos.
//!
//! Ciclo completo, todo medido por fases:
//!
//! 1. [`Enjambre::lanzar`]: N procesos `agente` en modo consenso sobre un
//!    segmento de `/dev/shm`.
//! 2. [`Enjambre::barrera`]: corte consistente (ningún agente a mitad de paso).
//! 3. [`Enjambre::congelar_todos`]: `SIGSTOP` a los N y espera del estado `T`.
//! 4. [`Enjambre::volcar_todos`]: N volcados **en paralelo** (un hilo por
//!    agente, `process_vm_readv`) + copia del segmento compartido.
//! 5. Se matan los originales (`SIGKILL`): la restauración es en procesos nuevos.
//! 6. [`restaurar_todos`]: N `restaurador` preparados de antemano reciben la
//!    largada a la vez; el tiempo del enjambre es `max_i(t_i) − t_largada` con un
//!    único `CLOCK_MONOTONIC`. Luego se comprueba el consenso y se reanuda.

use crate::congelar::{Congelador, Metodo};
use crate::consenso::{self, Mapeo};
use crate::estado::{self, BASE};
use crate::volcado;
use serde::Serialize;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Un agente vivo del enjambre.
pub struct Agente {
    pub indice: usize,
    pub pid: i32,
    hijo: Child,
}

pub struct Enjambre {
    pub agentes: Vec<Agente>,
    pub segmento: Mapeo,
    pub ruta_segmento: PathBuf,
    congelador: Option<Congelador>,
}

/// Lo que queda en disco/memoria tras `volcar_todos`.
#[derive(Debug, Clone, Serialize)]
pub struct Volcado {
    pub dirs: Vec<PathBuf>,
    pub shas: Vec<String>,
    pub contadores: Vec<u64>,
    pub semillas: Vec<u64>,
    pub pids_origen: Vec<u64>,
    #[serde(skip)]
    pub segmento: Vec<u8>,
    pub hash_conjunto: String,
    pub bytes_totales: u64,
    pub total_compartido: u64,
}

fn esperar_archivo(p: &Path, limite: Duration) -> io::Result<String> {
    let t0 = Instant::now();
    loop {
        if let Ok(s) = fs::read_to_string(p) {
            if s.ends_with('\n') {
                return Ok(s);
            }
        }
        if t0.elapsed() > limite {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("timeout {}", p.display())));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

impl Enjambre {
    /// Lanza `n` agentes de `tamano` (p.ej. "4M") en modo consenso.
    pub fn lanzar(bin_agente: &Path, tamano: &str, n: usize, dir: &Path, ruta_segmento: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let segmento = Mapeo::crear(ruta_segmento, n)?;
        let mut agentes = Vec::with_capacity(n);
        for i in 0..n {
            let listo = dir.join(format!("a{i}.ready"));
            let _ = fs::remove_file(&listo);
            let hijo = Command::new(bin_agente)
                .args(["--tamano", tamano, "--pasos", "0"])
                .args(["--semilla", &(1000 + i as u64).to_string()])
                .args(["--senal-listo", listo.to_str().unwrap()])
                .args(["--consenso", ruta_segmento.to_str().unwrap()])
                .args(["--indice", &i.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            agentes.push(Agente { indice: i, pid: hijo.id() as i32, hijo });
        }
        for (i, a) in agentes.iter().enumerate() {
            let s = esperar_archivo(&dir.join(format!("a{i}.ready")), Duration::from_secs(60))?;
            let pid: i32 = s.trim().parse().map_err(io::Error::other)?;
            if pid != a.pid {
                return Err(io::Error::other(format!("pid {pid} != {}", a.pid)));
            }
        }
        Ok(Self { agentes, segmento, ruta_segmento: ruta_segmento.to_path_buf(), congelador: None })
    }

    pub fn pids(&self) -> Vec<i32> {
        self.agentes.iter().map(|a| a.pid).collect()
    }

    /// Corte consistente cooperativo.
    pub fn barrera(&self) -> io::Result<Duration> {
        self.segmento.pausar(Duration::from_secs(10)).map(|(_, d)| d)
    }

    /// `SIGSTOP` a todos y espera del estado `T`.
    pub fn congelar_todos(&mut self) -> io::Result<Duration> {
        let c = Congelador::nuevo(Metodo::Sigstop, &self.pids(), "enjambre")?;
        let d = c.congelar()?;
        self.congelador = Some(c);
        Ok(d)
    }

    /// Vuelca los N agentes en paralelo y copia el segmento. Devuelve (volcado, tiempo).
    pub fn volcar_todos(&self, dir: &Path) -> io::Result<(Volcado, Duration)> {
        let t0 = Instant::now();
        let dirs: Vec<PathBuf> = self.agentes.iter().map(|a| dir.join(format!("agente-{:02}", a.indice))).collect();
        let mans: Vec<io::Result<volcado::Manifiesto>> = std::thread::scope(|s| {
            let hs: Vec<_> = self
                .agentes
                .iter()
                .zip(&dirs)
                .map(|(a, d)| s.spawn(move || volcado::volcar(a.pid, d, true)))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let segmento = self.segmento.bytes();
        fs::write(dir.join("segmento.bin"), &segmento)?;
        let dt = t0.elapsed();

        let mut shas = Vec::new();
        let mut contadores = Vec::new();
        let mut semillas = Vec::new();
        let mut pids_origen = Vec::new();
        let mut bytes_totales = 0;
        for (m, d) in mans.into_iter().zip(&dirs) {
            let m = m?;
            bytes_totales += m.bytes_totales;
            let r = volcado::region_en(&m, BASE as u64).ok_or_else(|| io::Error::other("sin región BASE"))?;
            let b = fs::read(volcado::archivo_region(d, r))?;
            let c = estado::cabecera_de_bytes(&b).ok_or_else(|| io::Error::other("cabecera inválida"))?;
            shas.push(r.sha256.clone());
            contadores.push(c.contador);
            semillas.push(c.semilla);
            pids_origen.push(c.pid_origen);
        }
        let vista = consenso::leer_segmento(&segmento)?;
        let hash_conjunto = consenso::hash_conjunto(&shas, &segmento);
        Ok((
            Volcado {
                dirs,
                shas,
                contadores,
                semillas,
                pids_origen,
                segmento,
                hash_conjunto,
                bytes_totales,
                total_compartido: vista.total,
            },
            dt,
        ))
    }

    /// Mata a los originales (la restauración ocurre en procesos nuevos).
    pub fn matar(mut self) {
        for a in &mut self.agentes {
            let _ = a.hijo.kill();
            let _ = a.hijo.wait();
        }
        if let Some(c) = self.congelador.take() {
            drop(c);
        }
        let _ = fs::remove_file(&self.ruta_segmento);
    }
}

/// Resultado de restaurar el enjambre completo.
#[derive(Debug, Clone, Serialize)]
pub struct ResultadoEnjambre {
    pub modo: String,
    pub agentes: usize,
    /// max_i(t_listo_i) − largada.
    pub listo_ms: f64,
    /// max_i(t_primer_acceso_i) − largada.
    pub primer_acceso_ms: f64,
    /// max_i(t_carga_i) − largada.
    pub carga_completa_ms: f64,
    /// mediana por agente de (t_listo_i − t_inicio_i): sin el retraso de despertar.
    pub listo_por_agente_mediana_ms: f64,
    pub fallos_uffd: u64,
    pub sha_ok: bool,
    pub hash_conjunto: String,
    pub consenso_ok: bool,
    pub reanudacion_ok: bool,
    pub detalle_error: Option<String>,
}

struct Restaurador {
    hijo: Child,
    _entrada: ChildStdin,
    salida: BufReader<ChildStdout>,
}

impl Restaurador {
    fn linea(&mut self, prefijo: &str) -> io::Result<serde_json::Value> {
        let mut s = String::new();
        loop {
            s.clear();
            if self.salida.read_line(&mut s)? == 0 {
                return Err(io::Error::other(format!("restaurador terminó esperando '{prefijo}'")));
            }
            let t = s.trim_end();
            if t == prefijo {
                return Ok(serde_json::Value::Null);
            }
            if let Some(j) = t.strip_prefix(prefijo).and_then(|r| r.strip_prefix(' ')) {
                return serde_json::from_str(j).map_err(io::Error::other);
            }
            if let Some(j) = t.strip_prefix("error ") {
                return Err(io::Error::other(j.to_string()));
            }
        }
    }
}

fn mediana(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Restaura los N agentes de `vol` en N procesos `restaurador` y verifica el consenso.
///
/// `prefijo` se antepone al comando (p.ej. `["sudo","-n"]` para uffd sin ser root).
pub fn restaurar_todos(
    bin_restaurador: &Path,
    prefijo: &[String],
    vol: &Volcado,
    modo: &str,
    ruta_segmento: &Path,
    k: u64,
) -> io::Result<ResultadoEnjambre> {
    let n = vol.dirs.len();
    fs::write(ruta_segmento, &vol.segmento)?;
    let seg = Mapeo::abrir(ruta_segmento)?;
    let ruta_largada = ruta_segmento.with_extension("largada");
    let largada = consenso::Largada::crear(&ruta_largada)?;

    let mut rs = Vec::with_capacity(n);
    for (i, d) in vol.dirs.iter().enumerate() {
        let mut cmd = if prefijo.is_empty() {
            Command::new(bin_restaurador)
        } else {
            let mut c = Command::new(&prefijo[0]);
            c.args(&prefijo[1..]).arg(bin_restaurador);
            c
        };
        let mut hijo = cmd
            .args(["--in", d.to_str().unwrap(), "--modo", modo])
            .args(["--segmento", ruta_segmento.to_str().unwrap()])
            .args(["--indice", &i.to_string(), "--reanudar", &k.to_string()])
            .args(["--largada", ruta_largada.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let entrada = hijo.stdin.take().unwrap();
        let salida = BufReader::new(hijo.stdout.take().unwrap());
        rs.push(Restaurador { hijo, _entrada: entrada, salida });
    }
    for r in &mut rs {
        r.linea("preparado")?;
    }
    // largada común
    let t_largada = crate::monotonic_ns();
    largada.dar();
    let mut estados = Vec::with_capacity(n);
    for r in &mut rs {
        estados.push(r.linea("estado")?);
    }

    let g = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap_or(0);
    let ms = |t: u64| (t.saturating_sub(t_largada)) as f64 / 1e6;
    let listo_ms = estados.iter().map(|e| ms(g(e, "t_listo_ns"))).fold(0.0, f64::max);
    let primer_ms = estados.iter().map(|e| ms(g(e, "t_primer_acceso_ns"))).fold(0.0, f64::max);
    let carga_ms = estados.iter().map(|e| ms(g(e, "t_carga_ns"))).fold(0.0, f64::max);
    let mut por_agente: Vec<f64> = estados
        .iter()
        .map(|e| (g(e, "t_listo_ns") - g(e, "t_inicio_ns")) as f64 / 1e6)
        .collect();
    let fallos = estados.iter().map(|e| g(e, "fallos_uffd")).sum();
    let shas: Vec<String> = estados.iter().map(|e| e["sha256"].as_str().unwrap_or("").to_string()).collect();
    let sha_ok = estados.iter().all(|e| e["sha_ok"].as_bool() == Some(true)) && shas == vol.shas;
    let contadores: Vec<u64> = estados.iter().map(|e| g(e, "contador")).collect();

    // consenso: segmento restaurado + heaps restaurados == volcado
    let seg_bytes = seg.bytes();
    let hash = consenso::hash_conjunto(&shas, &seg_bytes);
    let vista = consenso::leer_segmento(&seg_bytes)?;
    let mut errores = Vec::new();
    if hash != vol.hash_conjunto {
        errores.push(format!("hash conjunto {hash} != volcado {}", vol.hash_conjunto));
    }
    if let Err(e) = consenso::verificar_invariante(&vista, &contadores) {
        errores.push(e);
    }
    if contadores != vol.contadores {
        errores.push(format!("contadores {contadores:?} != volcado {:?}", vol.contadores));
    }
    let consenso_ok = errores.is_empty();

    // reanudación coordinada
    seg.reanudar();
    let mut finales = Vec::with_capacity(n);
    for r in &mut rs {
        finales.push(r.linea("final")?);
    }
    for r in &mut rs {
        let _ = r.hijo.wait();
    }
    let v2 = consenso::leer_segmento(&seg.bytes())?;
    let finales_c: Vec<u64> = finales.iter().map(|f| g(f, "contador")).collect();
    let mut reanudacion_ok = v2.total == vol.total_compartido + n as u64 * k;
    if !reanudacion_ok {
        errores.push(format!("total tras reanudar {} != {} + {n}·{k}", v2.total, vol.total_compartido));
    }
    for (i, &c) in finales_c.iter().enumerate() {
        if c != vol.contadores[i] + k || v2.contador[i] != c {
            reanudacion_ok = false;
            errores.push(format!("agente {i}: final {c}, espejo {}, esperado {}", v2.contador[i], vol.contadores[i] + k));
        }
    }
    // el total arrancó en 0, así que el invariante debe seguir valiendo tras reanudar
    if let Err(e) = consenso::verificar_invariante(&v2, &finales_c) {
        reanudacion_ok = false;
        errores.push(format!("tras reanudar: {e}"));
    }
    drop(seg);
    drop(largada);
    let _ = fs::remove_file(ruta_segmento);
    let _ = fs::remove_file(&ruta_largada);

    Ok(ResultadoEnjambre {
        modo: modo.to_string(),
        agentes: n,
        listo_ms,
        primer_acceso_ms: primer_ms,
        carga_completa_ms: carga_ms,
        listo_por_agente_mediana_ms: mediana(&mut por_agente),
        fallos_uffd: fallos,
        sha_ok,
        hash_conjunto: hash,
        consenso_ok,
        reanudacion_ok,
        detalle_error: if errores.is_empty() { None } else { Some(errores.join("; ")) },
    })
}
