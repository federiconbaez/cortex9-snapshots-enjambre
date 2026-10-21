//! Restaurador de UN agente del enjambre, pensado para correr N en paralelo.
//!
//!   restaurador --in DUMP/pid-X --modo uffd|completo|mmap \
//!               [--segmento /dev/shm/SEG --indice I --reanudar K]
//!
//! Protocolo con el orquestador (stdin/stdout, una línea por evento):
//! 1. abre la imagen y el segmento, imprime `preparado`;
//! 2. espera la largada común (`--largada /dev/shm/X`, un atómico compartido;
//!    o un byte por stdin) (la largada común, para medir al enjambre con un
//!    solo reloj `CLOCK_MONOTONIC`);
//! 3. restaura, lee la cabecera, materializa, calcula sha → `estado {json}`;
//! 4. espera la reanudación coordinada (época par), da K pasos → `final {json}`.

use enjambre_snap::consenso::{Largada, Mapeo};
use enjambre_snap::estado::{self, BASE};
use enjambre_snap::restaurar::{self, Modo, Restauracion};
use enjambre_snap::{monotonic_ns, volcado};
use std::io::{Read, Write};
use std::path::PathBuf;

fn arg(args: &[String], n: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == n).map(|w| w[1].clone())
}

fn morir(msg: String) -> ! {
    eprintln!("restaurador: {msg}");
    println!("error {}", serde_json::json!({ "error": msg }));
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(arg(&args, "--in").unwrap_or_else(|| morir("falta --in".into())));
    let modo: Modo = arg(&args, "--modo")
        .unwrap_or_else(|| "uffd".into())
        .parse()
        .unwrap_or_else(|e: String| morir(e));
    let indice: usize = arg(&args, "--indice").and_then(|s| s.parse().ok()).unwrap_or(0);
    let k: u64 = arg(&args, "--reanudar").and_then(|s| s.parse().ok()).unwrap_or(0);

    // --- preparación (fuera del tiempo medido) ---
    let man = volcado::cargar_manifiesto(&dir).unwrap_or_else(|e| morir(format!("manifiesto: {e}")));
    let reg = volcado::region_en(&man, BASE as u64)
        .unwrap_or_else(|| morir("no hay región BASE".into()))
        .clone();
    let (archivo, len) = restaurar::preparar(&volcado::archivo_region(&dir, &reg))
        .unwrap_or_else(|e| morir(format!("imagen: {e}")));
    if len != reg.bytes as usize {
        morir(format!("imagen de {len} bytes, manifiesto dice {}", reg.bytes));
    }
    let seg = arg(&args, "--segmento")
        .map(|r| Mapeo::abrir(std::path::Path::new(&r)).unwrap_or_else(|e| morir(format!("segmento: {e}"))));

    let largada = arg(&args, "--largada")
        .map(|r| Largada::abrir(std::path::Path::new(&r)).unwrap_or_else(|e| morir(format!("largada: {e}"))));

    let mut out = std::io::stdout().lock();
    writeln!(out, "preparado").unwrap();
    out.flush().unwrap();

    match &largada {
        // largada por memoria compartida: sin latencia de pipe ni de sudo
        Some(l) => l.esperar(),
        None => {
            let mut b = [0u8; 1];
            if std::io::stdin().read_exact(&mut b).is_err() {
                morir("sin largada".into());
            }
        }
    }

    // --- restauración medida ---
    let t_inicio = monotonic_ns();
    let mut r = Restauracion::iniciar(&archivo, len, modo).unwrap_or_else(|e| morir(format!("iniciar: {e}")));
    let t_listo = monotonic_ns();
    let cab = r.primer_acceso();
    let t_primer = monotonic_ns();
    r.materializar();
    let t_carga = monotonic_ns();
    let fallos = r.cerrar_manejador();
    if cab.magia != estado::MAGIA {
        morir("magia inválida tras restaurar".into());
    }
    let sha = r.sha256();
    let sha_ok = sha == reg.sha256;

    writeln!(
        out,
        "estado {}",
        serde_json::json!({
            "indice": indice,
            "pid": std::process::id(),
            "pid_origen": cab.pid_origen,
            "contador": cab.contador,
            "semilla": cab.semilla,
            "sha256": sha,
            "sha_ok": sha_ok,
            "t_inicio_ns": t_inicio,
            "t_listo_ns": t_listo,
            "t_primer_acceso_ns": t_primer,
            "t_carga_ns": t_carga,
            "fallos_uffd": fallos,
        })
    )
    .unwrap();
    out.flush().unwrap();

    // --- reanudación coordinada ---
    let mut contador = cab.contador;
    if let Some(m) = &seg {
        // el segmento restaurado viene con la pausa puesta: esperamos la época par
        m.punto_seguro(indice, || false);
        for _ in 0..k {
            // SAFETY: región restaurada válida del agente.
            contador = unsafe { estado::paso(r.ptr, r.tamano) };
            m.registrar_paso(indice, contador);
        }
    }
    writeln!(
        out,
        "final {}",
        serde_json::json!({ "indice": indice, "contador": contador, "sha256": r.sha256() })
    )
    .unwrap();
    out.flush().unwrap();
}
