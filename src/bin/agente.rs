//! Proceso-agente de prueba: vive todo su estado en un mmap fijo en BASE.
//!
//! Uso:
//!   agente --tamano 64M --pasos 1000 --semilla 42 --senal-listo /tmp/a.ready --senal-fin /tmp/a.done
//!
//! Tras mapear e inicializar escribe su pid en `--senal-listo`. Corre `--pasos`
//! iteraciones (o infinito si pasos=0) y al recibir SIGTERM escribe el contador
//! final en `--senal-fin` y sale.

use enjambre_snap::estado::{self, BASE};
use enjambre_snap::parsear_tamano;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn uso() -> ! {
    eprintln!(
        "uso: agente --tamano <4M|64M|1G> [--pasos N] [--semilla U64] \
         [--senal-listo PATH] [--senal-fin PATH] [--pausar-al-listo] \
         [--consenso /dev/shm/SEG --indice I]"
    );
    std::process::exit(2);
}

fn arg(args: &[String], nombre: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == nombre)
        .map(|w| w[1].clone())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tamano = arg(&args, "--tamano")
        .and_then(|s| parsear_tamano(&s))
        .unwrap_or_else(|| uso());
    let pasos: u64 = arg(&args, "--pasos")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let semilla: u64 = arg(&args, "--semilla")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0xC0FFEE);
    let senal_listo = arg(&args, "--senal-listo").map(PathBuf::from);
    let senal_fin = arg(&args, "--senal-fin").map(PathBuf::from);
    let pausar = args.iter().any(|a| a == "--pausar-al-listo");
    // Modo consenso: segmento compartido + barrera de corte consistente.
    let consenso = arg(&args, "--consenso").map(|r| {
        let i: usize = arg(&args, "--indice").and_then(|s| s.parse().ok()).unwrap_or_else(|| uso());
        let m = enjambre_snap::consenso::Mapeo::abrir(std::path::Path::new(&r)).unwrap_or_else(|e| {
            eprintln!("no pude abrir segmento {r}: {e}");
            std::process::exit(1);
        });
        if i >= m.n() {
            eprintln!("indice {i} >= n {}", m.n());
            std::process::exit(1);
        }
        (m, i)
    });

    let pag = enjambre_snap::pagina();
    if tamano < 2 * pag || tamano % pag != 0 {
        eprintln!("tamano debe ser múltiplo de la página y >= 2 páginas");
        std::process::exit(1);
    }

    // SAFETY: mmap anónimo en dirección fija.
    let ptr = unsafe {
        libc::mmap(
            BASE as *mut libc::c_void,
            tamano,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        eprintln!("mmap BASE falló: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }
    let ptr = ptr as *mut u8;
    // SAFETY: región válida recién mapeada.
    unsafe { estado::inicializar(ptr, tamano, semilla) };

    if let Some(p) = &senal_listo {
        if let Some(padre) = p.parent() {
            let _ = fs::create_dir_all(padre);
        }
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(p)
            .expect("senal-listo");
        writeln!(f, "{}", std::process::id()).unwrap();
        f.flush().unwrap();
    }

    if pausar {
        // Espera SIGCONT del hipervisor (tras el volcado).
        // SAFETY: raise SIGSTOP
        unsafe { libc::raise(libc::SIGSTOP) };
    }

    let salir = Arc::new(AtomicBool::new(false));
    // manejador simple vía flag; usamos signal-hook libre = libc::signal
    // SAFETY: handler mínimo async-signal-safe (solo store).
    extern "C" fn on_term(_: libc::c_int) {
        // no podemos tocar Arc desde acá; usamos variable estática
        SALIR.store(true, Ordering::Release);
    }
    static SALIR: AtomicBool = AtomicBool::new(false);
    unsafe {
        libc::signal(libc::SIGTERM, on_term as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as libc::sighandler_t);
    }

    let mut hechos = 0u64;
    loop {
        if SALIR.load(Ordering::Acquire) || salir.load(Ordering::Acquire) {
            break;
        }
        if pasos > 0 && hechos >= pasos {
            break;
        }
        if let Some((m, i)) = &consenso {
            // solo entre pasos: la barrera nunca corta un paso a la mitad
            if !m.punto_seguro(*i, || SALIR.load(Ordering::Acquire)) {
                break;
            }
        }
        // SAFETY: heap válido
        let c = unsafe { estado::paso(ptr, tamano) };
        if let Some((m, i)) = &consenso {
            m.registrar_paso(*i, c);
        }
        hechos += 1;
        if hechos % 10_000 == 0 {
            std::thread::sleep(Duration::from_micros(1));
        }
    }

    let cab = unsafe { estado::cabecera(ptr) };
    if let Some(p) = &senal_fin {
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(p)
            .expect("senal-fin");
        writeln!(f, "contador={}", cab.contador).unwrap();
        writeln!(f, "semilla={}", cab.semilla).unwrap();
        writeln!(f, "tamano={}", cab.tamano).unwrap();
        writeln!(f, "pasos_locales={hechos}").unwrap();
    }
    eprintln!(
        "agente pid={} contador={} tamano={}",
        std::process::id(),
        cab.contador,
        cab.tamano
    );
}
