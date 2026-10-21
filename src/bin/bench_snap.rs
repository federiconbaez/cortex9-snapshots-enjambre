//! Benchmark reproducible: congela N agentes, volca, restaura en 3 modos.
//!
//!   bench_snap --tamanos 4M,64M,256M --agentes 1,4 --reps 5 --out /tmp/bench-snap
//!
//! Imprime una tabla markdown y un JSON con los tiempos. La meta de Federico
//! (<5 ms para el enjambre) se reporta como cifra medida, sin afirmarla.

use enjambre_snap::congelar::{Congelador, Metodo};
use enjambre_snap::humano;
use enjambre_snap::parsear_tamano;
use enjambre_snap::restaurar::Modo;
use enjambre_snap::volcado;
use enjambre_snap::estado::BASE;
use serde::Serialize;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Serialize, Clone)]
struct Fila {
    tamano: String,
    tamano_bytes: usize,
    agentes: usize,
    fase: String,
    modo: String,
    ms_mediana: f64,
    ms_min: f64,
    ms_max: f64,
    bytes_totales: u64,
    fallos_uffd: u64,
    meta_5ms: bool,
}

fn arg(args: &[String], n: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == n).map(|w| w[1].clone())
}

fn mediana(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn esperar_archivo(p: &Path, to: Duration) -> std::io::Result<String> {
    let t0 = Instant::now();
    loop {
        if p.exists() {
            return fs::read_to_string(p);
        }
        if t0.elapsed() > to {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timeout esperando {}", p.display()),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn spawn_agente(bin: &Path, tamano: &str, dir: &Path, i: usize) -> (Child, PathBuf, i32) {
    let listo = dir.join(format!("a{i}.ready"));
    let fin = dir.join(format!("a{i}.done"));
    let _ = fs::remove_file(&listo);
    let child = Command::new(bin)
        .args([
            "--tamano",
            tamano,
            "--pasos",
            "0",
            "--semilla",
            &format!("{}", 1000 + i),
            "--senal-listo",
            listo.to_str().unwrap(),
            "--senal-fin",
            fin.to_str().unwrap(),
            "--pausar-al-listo",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn agente");
    let pid_s = esperar_archivo(&listo, Duration::from_secs(30)).expect("ready");
    let pid: i32 = pid_s.trim().parse().expect("pid");
    // el agente se auto-SIGSTOP con --pausar-al-listo; esperamos estado T
    for _ in 0..1000 {
        if enjambre_snap::congelar::estado_proceso(pid).ok() == Some('T') {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    (child, fin, pid)
}

fn matar(c: &mut Child) {
    let _ = c.kill();
    let _ = c.wait();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tamanos_s = arg(&args, "--tamanos").unwrap_or_else(|| "4M,64M".into());
    let agentes_s = arg(&args, "--agentes").unwrap_or_else(|| "1".into());
    let reps: usize = arg(&args, "--reps")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "/tmp/bench-snap".into()));
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();

    let bin_agente = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("agente");
    if !bin_agente.exists() {
        eprintln!("no encuentro {bin_agente:?}; compilá con cargo build --release");
        std::process::exit(1);
    }

    let tamanos: Vec<(String, usize)> = tamanos_s
        .split(',')
        .map(|s| {
            let s = s.trim().to_string();
            let b = parsear_tamano(&s).expect("tamano");
            (s, b)
        })
        .collect();
    let n_agentes: Vec<usize> = agentes_s
        .split(',')
        .map(|s| s.trim().parse().expect("n"))
        .collect();

    let mut filas = Vec::new();
    println!(
        "| tamano | agentes | fase | modo | mediana_ms | min_ms | max_ms | bytes | <5ms |"
    );
    println!(
        "|--------|---------|------|------|------------|--------|--------|-------|------|"
    );

    for (tam_s, tam_b) in &tamanos {
        for &n in &n_agentes {
            // --- freeze ---
            let mut muestras_freeze = Vec::new();
            let mut muestras_dump = Vec::new();
            let mut bytes_tot = 0u64;
            for r in 0..reps {
                let rundir = out.join(format!("t{tam_s}-n{n}-r{r}"));
                fs::create_dir_all(&rundir).unwrap();
                let mut children = Vec::new();
                let mut pids = Vec::new();
                for i in 0..n {
                    let (c, _, pid) = spawn_agente(&bin_agente, tam_s, &rundir, i);
                    children.push(c);
                    pids.push(pid);
                }
                // ya están en T por --pausar-al-listo; medimos SIGSTOP de un
                // segundo enjambre "vivo" aparte: CONT + re-STOP
                for &p in &pids {
                    unsafe { libc::kill(p, libc::SIGCONT) };
                }
                thread::sleep(Duration::from_millis(20));
                let cong =
                    Congelador::nuevo(Metodo::Sigstop, &pids, &format!("bench-{r}")).unwrap();
                let tf = cong.congelar().unwrap();
                muestras_freeze.push(tf.as_secs_f64() * 1e3);

                let t0 = Instant::now();
                let mut bt = 0u64;
                for &pid in &pids {
                    let dir = rundir.join(format!("pid-{pid}"));
                    let man = volcado::volcar(pid, &dir, true).unwrap();
                    bt += man.bytes_totales;
                }
                muestras_dump.push(t0.elapsed().as_secs_f64() * 1e3);
                bytes_tot = bt;
                let _ = cong.descongelar();
                for c in &mut children {
                    matar(c);
                }
            }
            push_fila(
                &mut filas,
                tam_s,
                *tam_b,
                n,
                "congelar",
                "sigstop",
                &mut muestras_freeze,
                bytes_tot,
                0,
            );
            push_fila(
                &mut filas,
                tam_s,
                *tam_b,
                n,
                "volcar",
                "anonimas",
                &mut muestras_dump,
                bytes_tot,
                0,
            );

            // --- restore (usa el último dump del primer agente) ---
            // rehacer un dump limpio
            let rundir = out.join(format!("t{tam_s}-n{n}-restore"));
            let _ = fs::remove_dir_all(&rundir);
            fs::create_dir_all(&rundir).unwrap();
            let (mut child, _, pid) = spawn_agente(&bin_agente, tam_s, &rundir, 0);
            // avanzar un poco el estado
            unsafe { libc::kill(pid, libc::SIGCONT) };
            thread::sleep(Duration::from_millis(50));
            let cong = Congelador::nuevo(Metodo::Sigstop, &[pid], "bench-rest").unwrap();
            cong.congelar().unwrap();
            // hacer algunos pasos antes de congelar ya pasaron; volcamos
            let dump = rundir.join(format!("pid-{pid}"));
            volcado::volcar(pid, &dump, true).unwrap();
            cong.descongelar().unwrap();
            matar(&mut child);

            for (_modo, nombre) in [
                (Modo::Completo, "completo"),
                (Modo::MmapArchivo, "mmap"),
                (Modo::Uffd, "uffd"),
            ] {
                let mut m_listo = Vec::new();
                let mut m_carga = Vec::new();
                let mut fallos = 0u64;
                let mut ok_modo = true;
                for _ in 0..reps {
                    // cada restore necesita proceso limpio (BASE fija): lo hacemos
                    // en un hijo para no chocar direcciones
                    let status = Command::new(std::env::current_exe().unwrap().parent().unwrap().join("snap"))
                        .args([
                            "restore",
                            "--in",
                            dump.to_str().unwrap(),
                            "--modo",
                            nombre,
                        ])
                        .output();
                    match status {
                        Ok(o) if o.status.success() => {
                            let stdout = String::from_utf8_lossy(&o.stdout);
                            // "restaurado modo=... listo=X carga_completa=Y fallos=Z"
                            let listo = parse_dur_ms(&stdout, "listo=");
                            let carga = parse_dur_ms(&stdout, "carga_completa=");
                            let f = parse_u64(&stdout, "fallos=");
                            m_listo.push(listo);
                            m_carga.push(carga);
                            fallos = f;
                        }
                        Ok(o) => {
                            eprintln!(
                                "restore {nombre} falló: {}",
                                String::from_utf8_lossy(&o.stderr)
                            );
                            ok_modo = false;
                            break;
                        }
                        Err(e) => {
                            eprintln!("restore {nombre} error: {e}");
                            ok_modo = false;
                            break;
                        }
                    }
                }
                if !ok_modo || m_listo.is_empty() {
                    println!(
                        "| {} | {n} | restaurar_listo | {nombre} | ERR | — | — | — | — |",
                        humano(*tam_b)
                    );
                    continue;
                }
                push_fila(
                    &mut filas,
                    tam_s,
                    *tam_b,
                    n,
                    "restaurar_listo",
                    nombre,
                    &mut m_listo,
                    *tam_b as u64 * n as u64,
                    fallos,
                );
                push_fila(
                    &mut filas,
                    tam_s,
                    *tam_b,
                    n,
                    "restaurar_carga_completa",
                    nombre,
                    &mut m_carga,
                    *tam_b as u64 * n as u64,
                    fallos,
                );
                let _ = BASE; // silence
            }
        }
    }

    let json = out.join("resultados.json");
    let mut f = File::create(&json).unwrap();
    writeln!(
        f,
        "{}",
        serde_json::to_string_pretty(&filas).unwrap()
    )
    .unwrap();
    println!("\nJSON: {}", json.display());
}

#[allow(clippy::too_many_arguments)]
fn push_fila(
    filas: &mut Vec<Fila>,
    tam_s: &str,
    tam_b: usize,
    n: usize,
    fase: &str,
    modo: &str,
    muestras: &mut [f64],
    bytes: u64,
    fallos: u64,
) {
    let med = mediana(muestras);
    let min = muestras.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = muestras.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let meta = med < 5.0;
    println!(
        "| {} | {n} | {fase} | {modo} | {med:.3} | {min:.3} | {max:.3} | {bytes} | {} |",
        humano(tam_b),
        if meta { "SÍ" } else { "no" }
    );
    filas.push(Fila {
        tamano: tam_s.into(),
        tamano_bytes: tam_b,
        agentes: n,
        fase: fase.into(),
        modo: modo.into(),
        ms_mediana: med,
        ms_min: min,
        ms_max: max,
        bytes_totales: bytes,
        fallos_uffd: fallos,
        meta_5ms: meta,
    });
}

fn parse_dur_ms(s: &str, key: &str) -> f64 {
    // Duration Debug: "123.4µs" / "1.23ms" / "1.2s"
    let i = s.find(key).expect(key);
    let rest = &s[i + key.len()..];
    let token = rest.split_whitespace().next().unwrap_or("");
    parse_duration_token(token)
}

fn parse_duration_token(token: &str) -> f64 {
    if let Some(v) = token.strip_suffix("ns") {
        return v.parse::<f64>().unwrap_or(0.0) / 1e6;
    }
    if let Some(v) = token.strip_suffix("µs").or_else(|| token.strip_suffix("us")) {
        return v.parse::<f64>().unwrap_or(0.0) / 1e3;
    }
    if let Some(v) = token.strip_suffix("ms") {
        return v.parse::<f64>().unwrap_or(0.0);
    }
    if let Some(v) = token.strip_suffix('s') {
        return v.parse::<f64>().unwrap_or(0.0) * 1e3;
    }
    0.0
}

fn parse_u64(s: &str, key: &str) -> u64 {
    let i = s.find(key).unwrap_or(0);
    let rest = &s[i + key.len()..];
    let token = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("0");
    token.parse().unwrap_or(0)
}
