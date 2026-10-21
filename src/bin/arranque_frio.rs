//! Línea base del experimento de valor del sistema 3: **arranque en frío**.
//!
//! Un proceso nuevo que NO usa el snapshot: mapea su heap en `BASE`, lo
//! inicializa desde la semilla y **rehace** los `--contador` pasos hasta llegar
//! al mismo estado que tenía el agente original. Es la forma más barata y
//! honesta de reconstruir el estado de este agente (que es determinista).
//! Con `--contador 0` es el «relanzar en blanco»: arranca, pero pierde el estado.
//!
//!   arranque_frio --tamano 64M --semilla 1000 --contador 123456 \
//!                 --pid-origen 4242 --largada /dev/shm/X --indice 0
//!
//! Mismo protocolo que `restaurador` (para medir con el mismo reloj):
//! `preparado` → espera la largada → reconstruye → `estado {json}` → `final {json}`.

use enjambre_snap::consenso::Largada;
use enjambre_snap::estado::{self, BASE};
use enjambre_snap::{monotonic_ns, parsear_tamano, sha256_hex};
use std::io::{Read, Write};

fn arg(args: &[String], n: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == n).map(|w| w[1].clone())
}

fn morir(msg: String) -> ! {
    eprintln!("arranque_frio: {msg}");
    println!("error {}", serde_json::json!({ "error": msg }));
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let num = |n: &str| -> u64 {
        arg(&args, n).and_then(|s| s.parse().ok()).unwrap_or_else(|| morir(format!("falta {n}")))
    };
    let tamano = arg(&args, "--tamano")
        .and_then(|s| parsear_tamano(&s))
        .unwrap_or_else(|| morir("falta --tamano".into()));
    let semilla = num("--semilla");
    let contador = num("--contador");
    let pid_origen = num("--pid-origen");
    let indice: u64 = arg(&args, "--indice").and_then(|s| s.parse().ok()).unwrap_or(0);
    let largada = arg(&args, "--largada")
        .map(|r| Largada::abrir(std::path::Path::new(&r)).unwrap_or_else(|e| morir(format!("largada: {e}"))));

    let mut out = std::io::stdout().lock();
    writeln!(out, "preparado").unwrap();
    out.flush().unwrap();
    match &largada {
        Some(l) => l.esperar(),
        None => {
            let mut b = [0u8; 1];
            if std::io::stdin().read_exact(&mut b).is_err() {
                morir("sin largada".into());
            }
        }
    }

    // --- arranque en frío medido ---
    let t_inicio = monotonic_ns();
    // SAFETY: mmap anónimo en dirección fija, igual que el agente original.
    let p = unsafe {
        libc::mmap(
            BASE as *mut libc::c_void,
            tamano,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        morir(format!("mmap BASE: {}", std::io::Error::last_os_error()));
    }
    let ptr = p as *mut u8;
    // SAFETY: región válida recién mapeada de `tamano` bytes.
    unsafe {
        estado::inicializar(ptr, tamano, semilla);
        (*(ptr as *mut estado::Cabecera)).pid_origen = pid_origen;
    }
    let t_init = monotonic_ns();
    for _ in 0..contador {
        // SAFETY: heap válido.
        unsafe { estado::paso(ptr, tamano) };
    }
    let t_listo = monotonic_ns();
    // la línea base no tiene carga diferida: listo == carga completa
    // SAFETY: región válida.
    let sha = sha256_hex(unsafe { std::slice::from_raw_parts(ptr, tamano) });
    let cab = unsafe { estado::cabecera(ptr) };
    writeln!(
        out,
        "estado {}",
        serde_json::json!({
            "indice": indice,
            "pid": std::process::id(),
            "contador": cab.contador,
            "semilla": cab.semilla,
            "sha256": sha,
            "t_inicio_ns": t_inicio,
            "t_init_ns": t_init,
            "t_listo_ns": t_listo,
            "t_carga_ns": t_listo,
        })
    )
    .unwrap();
    writeln!(out, "final {}", serde_json::json!({ "indice": indice, "contador": cab.contador })).unwrap();
    out.flush().unwrap();
}
