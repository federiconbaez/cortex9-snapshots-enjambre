//! Benchmark de ENJAMBRE con consenso.
//!
//!   bench_enjambre --tamanos 4M,64M --agentes 1,4,8,16 --reps 5 \
//!                  --modos uffd,completo --reanudar 1000 --json resultados/X.json
//!
//! Por cada (tamaño, N, repetición): lanza N agentes en modo consenso, corta
//! con la barrera, congela, vuelca en paralelo, mata a los originales y
//! restaura el enjambre en N procesos nuevos con largada común. Cada
//! restauración verifica sha por agente, hash conjunto, invariante
//! `total == Σ contador` y la reanudación coordinada de K pasos.
//! uffd necesita root (`vm.unprivileged_userfaultfd=0`): si no somos root, los
//! restauradores se lanzan con `sudo -n`.

use enjambre_snap::enjambre::{restaurar_todos, Enjambre};
use enjambre_snap::{humano, parsear_tamano};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

fn arg(args: &[String], n: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == n).map(|w| w[1].clone())
}

fn mediana(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
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

#[derive(Serialize)]
struct Fila {
    tamano: String,
    tamano_bytes: usize,
    agentes: usize,
    fase: String,
    modo: String,
    ms_mediana: f64,
    ms_min: f64,
    ms_max: f64,
    reps: usize,
    meta_5ms: bool,
}

/// Hora de Argentina (America/Buenos_Aires = UTC−3 fijo, sin horario de verano
/// desde 2009). La caja corre en UTC y no trae tzdata, así que se convierte a mano.
fn ahora_local() -> (String, String) {
    const ART: i64 = -3 * 3600;
    // SAFETY: time/gmtime_r con punteros válidos.
    unsafe {
        let t = libc::time(std::ptr::null_mut()) + ART as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&t, &mut tm);
        let largo = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} ART (UTC-03:00)",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        );
        let corto = format!(
            "{:04}-{:02}-{:02}T{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        );
        (largo, corto)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tamanos: Vec<String> = arg(&args, "--tamanos").unwrap_or_else(|| "4M,64M".into()).split(',').map(String::from).collect();
    let ns: Vec<usize> = arg(&args, "--agentes")
        .unwrap_or_else(|| "1,4,8,16".into())
        .split(',')
        .map(|s| s.trim().parse().expect("N"))
        .collect();
    let reps: usize = arg(&args, "--reps").and_then(|s| s.parse().ok()).unwrap_or(5);
    let modos: Vec<String> = arg(&args, "--modos").unwrap_or_else(|| "uffd,completo".into()).split(',').map(String::from).collect();
    let k: u64 = arg(&args, "--reanudar").and_then(|s| s.parse().ok()).unwrap_or(1000);
    let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "/tmp/bench-enjambre".into()));
    let (fecha, fecha_corta) = ahora_local();
    let json = PathBuf::from(
        arg(&args, "--json").unwrap_or_else(|| format!("resultados/enjambre-{fecha_corta}-ART.json")),
    );

    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap();
    let soy_root = unsafe { libc::geteuid() } == 0;
    let prefijo_uffd: Vec<String> = if soy_root { vec![] } else { vec!["sudo".into(), "-n".into()] };

    let mut filas = Vec::new();
    let mut fallas = Vec::new();
    let mut verificaciones = 0usize;
    println!("| tamaño | N | fase | modo | mediana_ms | min | max | <5 ms |");
    println!("|---|---|---|---|---|---|---|---|");

    for tam in &tamanos {
        let tam_b = parsear_tamano(tam).expect("tamaño");
        for &n in &ns {
            let mut m: std::collections::BTreeMap<(String, String), Vec<f64>> = Default::default();
            let mut push = |f: &str, mo: &str, v: f64| m.entry((f.into(), mo.into())).or_default().push(v);
            for r in 0..reps {
                let dir = out.join(format!("{tam}-n{n}-r{r}"));
                let _ = fs::remove_dir_all(&dir);
                let seg = PathBuf::from(format!("/dev/shm/enjambre-bench-{}-{n}-{r}", std::process::id()));
                let mut e = Enjambre::lanzar(&bin.join("agente"), tam, n, &dir, &seg).expect("lanzar");
                std::thread::sleep(Duration::from_millis(30));
                let tb = e.barrera().expect("barrera");
                let tc = e.congelar_todos().expect("congelar");
                let (vol, tv) = e.volcar_todos(&dir).expect("volcar");
                e.matar();
                push("barrera_consenso", "cooperativa", tb.as_secs_f64() * 1e3);
                push("congelar_todos", "sigstop", tc.as_secs_f64() * 1e3);
                push("volcar_todos", "paralelo", tv.as_secs_f64() * 1e3);

                for modo in &modos {
                    let pre: &[String] = if modo.starts_with("uffd") { &prefijo_uffd } else { &[] };
                    let seg_r = PathBuf::from(format!("/dev/shm/enjambre-rest-{}-{n}-{r}-{modo}", std::process::id()));
                    match restaurar_todos(&bin.join("restaurador"), pre, &vol, modo, &seg_r, k) {
                        Ok(res) => {
                            verificaciones += 1;
                            if !(res.sha_ok && res.consenso_ok && res.reanudacion_ok) {
                                fallas.push(format!("{tam} n={n} r={r} {modo}: {:?}", res.detalle_error));
                            }
                            push("restaurar_listo", modo, res.listo_ms);
                            push("restaurar_primer_acceso", modo, res.primer_acceso_ms);
                            push("restaurar_carga_completa", modo, res.carga_completa_ms);
                            push("restaurar_listo_por_agente", modo, res.listo_por_agente_mediana_ms);
                        }
                        Err(err) => fallas.push(format!("{tam} n={n} r={r} {modo}: error {err}")),
                    }
                }
                let _ = fs::remove_dir_all(&dir);
            }
            let orden = [
                "barrera_consenso",
                "congelar_todos",
                "volcar_todos",
                "restaurar_listo_por_agente",
                "restaurar_listo",
                "restaurar_primer_acceso",
                "restaurar_carga_completa",
            ];
            let mut claves: Vec<_> = m.keys().cloned().collect();
            claves.sort_by_key(|(f, mo)| (orden.iter().position(|o| o == f).unwrap_or(99), mo.clone()));
            for (f, mo) in claves {
                let v = &m[&(f.clone(), mo.clone())];
                let med = mediana(v);
                let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
                let max = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                println!(
                    "| {} | {n} | {f} | {mo} | {med:.3} | {min:.3} | {max:.3} | {} |",
                    humano(tam_b),
                    if med < 5.0 { "SÍ" } else { "no" }
                );
                filas.push(Fila {
                    tamano: tam.clone(),
                    tamano_bytes: tam_b,
                    agentes: n,
                    fase: f,
                    modo: mo,
                    ms_mediana: med,
                    ms_min: min,
                    ms_max: max,
                    reps: v.len(),
                    meta_5ms: med < 5.0,
                });
            }
        }
    }

    let doc = serde_json::json!({
        "sistema": "03-snapshots-enjambre",
        "fecha_ART": fecha,
        "zona": "America/Buenos_Aires",
        "parametros": { "tamanos": tamanos, "agentes": ns, "reps": reps, "modos": modos, "reanudar_pasos": k },
        "definiciones": {
            "barrera_consenso": "pausa cooperativa hasta ack de los N (corte consistente)",
            "congelar_todos": "SIGSTOP a los N + espera de estado T",
            "volcar_todos": "N volcados en paralelo (process_vm_readv + sha256 + escritura) + segmento",
            "restaurar_listo": "max_i(t_listo_i) - largada común, N procesos nuevos",
            "restaurar_primer_acceso": "max_i(cabecera leída) - largada (en uffd: fallo de página real)",
            "restaurar_carga_completa": "max_i(todas las páginas en RAM) - largada",
            "restaurar_listo_por_agente": "mediana de (t_listo_i - t_inicio_i), sin retraso de despertar"
        },
        "verificaciones_de_consenso": verificaciones,
        "fallas": fallas,
        "filas": filas,
    });
    if let Some(p) = json.parent() {
        let _ = fs::create_dir_all(p);
    }
    fs::write(&json, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    println!("\nverificaciones de consenso: {verificaciones}, fallas: {}", fallas.len());
    for f in &fallas {
        println!("FALLA: {f}");
    }
    println!("JSON: {}", json.display());
    if !fallas.is_empty() {
        std::process::exit(1);
    }
}
