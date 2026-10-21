//! Experimento de VALOR del sistema 3: ¿restaurar el enjambre desde el
//! snapshot le gana al arranque en frío (relanzar y reconstruir el estado)?
//!
//!   bench_valor --tamanos 4M,64M --agentes 1,4,16 --calentamiento 30,300,3000 \
//!               --reps 5 --json build/valor/corrida-1.json
//!
//! Por cada (tamaño, N, calentamiento, repetición):
//! 1. lanza N agentes, los deja trabajar `calentamiento` ms, corta con la
//!    barrera, congela, vuelca y los mata (eso es el snapshot);
//! 2. levanta el enjambre otra vez con cada variante, N procesos nuevos con
//!    largada común:
//!    - `snap_uffd`, `snap_uffd16`, `snap_completo`, `snap_mmap`: `restaurador` desde el volcado;
//!    - `frio_reconstruir` (LÍNEA BASE): `arranque_frio` rehace init + los mismos pasos;
//!    - `frio_blanco`: `arranque_frio` sin pasos (arranca pero PIERDE el estado).
//! 3. exige sha256 por agente == volcado en todas las variantes salvo `frio_blanco`.
//!
//! Métricas por enjambre (max sobre los N, un solo CLOCK_MONOTONIC):
//! - `listo_ms`: largada → el último agente puede ejecutar;
//! - `carga_ms`: largada → el último agente tiene todo su estado en RAM;
//! - `e2e_ms`: primer spawn → último `estado` recibido (incluye exec, apertura
//!   de la imagen y, en uffd, el `sudo -n`).

use enjambre_snap::consenso::Largada;
use enjambre_snap::enjambre::{Enjambre, Volcado};
use enjambre_snap::monotonic_ns;
use serde::Serialize;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

fn arg(args: &[String], n: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == n).map(|w| w[1].clone())
}
fn lista(args: &[String], n: &str, def: &str) -> Vec<String> {
    arg(args, n).unwrap_or_else(|| def.into()).split(',').map(|s| s.trim().to_string()).collect()
}

struct Proc {
    hijo: Child,
    _entrada: ChildStdin,
    salida: BufReader<ChildStdout>,
}
impl Proc {
    fn linea(&mut self, prefijo: &str) -> io::Result<serde_json::Value> {
        let mut s = String::new();
        loop {
            s.clear();
            if self.salida.read_line(&mut s)? == 0 {
                return Err(io::Error::other(format!("hijo terminó esperando '{prefijo}'")));
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

#[derive(Serialize, Clone)]
struct Muestra {
    tamano: String,
    agentes: usize,
    calentamiento_ms: u64,
    rep: usize,
    variante: String,
    listo_ms: f64,
    carga_ms: f64,
    e2e_ms: f64,
    estado_equivalente: bool,
    pasos_reconstruidos_total: u64,
}

fn levantar(bin: &Path, variante: &str, tam: &str, vol: &Volcado, sudo: &[String], tmp: &Path) -> io::Result<(f64, f64, f64, bool)> {
    let n = vol.dirs.len();
    let ruta_largada = tmp.with_extension(format!("{variante}.largada"));
    let largada = Largada::crear(&ruta_largada)?;
    let t_spawn = monotonic_ns();
    let mut ps = Vec::with_capacity(n);
    for i in 0..n {
        let mut cmd;
        if let Some(modo) = variante.strip_prefix("snap_") {
            let pre: &[String] = if modo.starts_with("uffd") { sudo } else { &[] };
            cmd = if pre.is_empty() {
                Command::new(bin.join("restaurador"))
            } else {
                let mut c = Command::new(&pre[0]);
                c.args(&pre[1..]).arg(bin.join("restaurador"));
                c
            };
            cmd.args(["--in", vol.dirs[i].to_str().unwrap(), "--modo", modo]);
        } else {
            let contador = if variante == "frio_blanco" { 0 } else { vol.contadores[i] };
            cmd = Command::new(bin.join("arranque_frio"));
            cmd.args(["--tamano", tam])
                .args(["--semilla", &vol.semillas[i].to_string()])
                .args(["--contador", &contador.to_string()])
                .args(["--pid-origen", &vol.pids_origen[i].to_string()]);
        }
        let mut hijo = cmd
            .args(["--indice", &i.to_string(), "--largada", ruta_largada.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let entrada = hijo.stdin.take().unwrap();
        let salida = BufReader::new(hijo.stdout.take().unwrap());
        ps.push(Proc { hijo, _entrada: entrada, salida });
    }
    for p in &mut ps {
        p.linea("preparado")?;
    }
    let t_largada = monotonic_ns();
    largada.dar();
    let mut estados = Vec::with_capacity(n);
    for p in &mut ps {
        estados.push(p.linea("estado")?);
    }
    let t_fin = monotonic_ns();
    for p in &mut ps {
        let _ = p.linea("final");
        let _ = p.hijo.wait();
    }
    drop(largada);
    let _ = fs::remove_file(&ruta_largada);
    let g = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap_or(0);
    let ms = |t: u64| t.saturating_sub(t_largada) as f64 / 1e6;
    let listo = estados.iter().map(|e| ms(g(e, "t_listo_ns"))).fold(0.0, f64::max);
    let carga = estados.iter().map(|e| ms(g(e, "t_carga_ns"))).fold(0.0, f64::max);
    let e2e = (t_fin - t_spawn) as f64 / 1e6;
    let equiv = estados
        .iter()
        .enumerate()
        .all(|(i, e)| e["sha256"].as_str() == Some(vol.shas[i].as_str()) && g(e, "contador") == vol.contadores[i]);
    Ok((listo, carga, e2e, equiv))
}

fn pct(v: &[f64], q: f64) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return f64::NAN;
    }
    // percentil por rango más cercano
    let r = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len());
    v[r - 1]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tamanos = lista(&args, "--tamanos", "4M,64M");
    let ns: Vec<usize> = lista(&args, "--agentes", "1,4,16").iter().map(|s| s.parse().expect("N")).collect();
    let cals: Vec<u64> = lista(&args, "--calentamiento", "30,300,3000").iter().map(|s| s.parse().expect("ms")).collect();
    let variantes = lista(&args, "--variantes", "snap_uffd,snap_uffd16,snap_completo,snap_mmap,frio_reconstruir,frio_blanco");
    let reps: usize = arg(&args, "--reps").and_then(|s| s.parse().ok()).unwrap_or(5);
    let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "build/valor/tmp".into()));
    let json = PathBuf::from(arg(&args, "--json").unwrap_or_else(|| "build/valor/corrida.json".into()));
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap().to_path_buf();
    let sudo: Vec<String> = if unsafe { libc::geteuid() } == 0 { vec![] } else { vec!["sudo".into(), "-n".into()] };

    let mut muestras: Vec<Muestra> = Vec::new();
    let mut fallas: Vec<String> = Vec::new();
    for tam in &tamanos {
        for &n in &ns {
            for &cal in &cals {
                for r in 0..reps {
                    let dir = out.join(format!("{tam}-n{n}-c{cal}-r{r}"));
                    let _ = fs::remove_dir_all(&dir);
                    let seg = PathBuf::from(format!("/dev/shm/valor-{}-{n}-{cal}-{r}", std::process::id()));
                    let mut e = Enjambre::lanzar(&bin.join("agente"), tam, n, &dir, &seg).expect("lanzar");
                    std::thread::sleep(Duration::from_millis(cal));
                    e.barrera().expect("barrera");
                    e.congelar_todos().expect("congelar");
                    let (vol, _) = e.volcar_todos(&dir).expect("volcar");
                    e.matar();
                    let pasos: u64 = vol.contadores.iter().sum();
                    // rotamos el orden para que ninguna variante corra siempre primera
                    let k = variantes.len();
                    for j in 0..k {
                        let v = &variantes[(j + r) % k];
                        match levantar(&bin, v, tam, &vol, &sudo, &seg) {
                            Ok((listo, carga, e2e, eq)) => {
                                if !eq && v != "frio_blanco" {
                                    fallas.push(format!("{tam} n={n} c={cal} r={r} {v}: estado distinto del volcado"));
                                }
                                muestras.push(Muestra {
                                    tamano: tam.clone(),
                                    agentes: n,
                                    calentamiento_ms: cal,
                                    rep: r,
                                    variante: v.clone(),
                                    listo_ms: listo,
                                    carga_ms: carga,
                                    e2e_ms: e2e,
                                    estado_equivalente: eq,
                                    pasos_reconstruidos_total: if v == "frio_reconstruir" { pasos } else { 0 },
                                });
                            }
                            Err(err) => fallas.push(format!("{tam} n={n} c={cal} r={r} {v}: {err}")),
                        }
                    }
                    let _ = fs::remove_dir_all(&dir);
                    eprintln!("{tam} n={n} c={cal}ms r={r}: pasos={pasos}");
                }
                // resumen de la celda
                for v in &variantes {
                    let sel: Vec<&Muestra> = muestras
                        .iter()
                        .filter(|m| &m.tamano == tam && m.agentes == n && m.calentamiento_ms == cal && &m.variante == v)
                        .collect();
                    let c: Vec<f64> = sel.iter().map(|m| m.carga_ms).collect();
                    let l: Vec<f64> = sel.iter().map(|m| m.listo_ms).collect();
                    let x: Vec<f64> = sel.iter().map(|m| m.e2e_ms).collect();
                    println!(
                        "{tam} N={n} cal={cal}ms {v:17} listo p50={:.3} carga p50={:.3} p95={:.3} e2e p50={:.3} p95={:.3}",
                        pct(&l, 0.5), pct(&c, 0.5), pct(&c, 0.95), pct(&x, 0.5), pct(&x, 0.95)
                    );
                }
            }
        }
    }
    let doc = serde_json::json!({
        "sistema": "03-snapshots-enjambre",
        "experimento": "valor: restaurar desde snapshot vs arranque en frío",
        "parametros": { "tamanos": tamanos, "agentes": ns, "calentamiento_ms": cals, "reps": reps, "variantes": variantes },
        "fallas": fallas,
        "muestras": muestras,
    });
    if let Some(p) = json.parent() {
        let _ = fs::create_dir_all(p);
    }
    fs::write(&json, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    println!("muestras={} fallas={} json={}", muestras.len(), fallas.len(), json.display());
    for f in &fallas {
        println!("FALLA: {f}");
    }
    if !fallas.is_empty() {
        std::process::exit(1);
    }
}
