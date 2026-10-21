//! Demostración de punta a punta: 2 agentes → congelar → volcar → restaurar → verificar.
//!
//!   demo_roundtrip --tamano 4M --modo uffd

use enjambre_snap::congelar::{Congelador, Metodo};
use enjambre_snap::estado::{self, BASE};
use enjambre_snap::parsear_tamano;
use enjambre_snap::restaurar::{self, Modo};
use enjambre_snap::sha256_hex;
use enjambre_snap::volcado;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn arg(args: &[String], n: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == n).map(|w| w[1].clone())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tam = arg(&args, "--tamano").unwrap_or_else(|| "4M".into());
    let tam_b = parsear_tamano(&tam).expect("tamano");
    let modo: Modo = arg(&args, "--modo")
        .unwrap_or_else(|| "completo".into())
        .parse()
        .expect("modo");
    let out = PathBuf::from("/tmp/demo-enjambre");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();

    let bin = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("agente");
    let mut children = Vec::new();
    let mut pids = Vec::new();
    let mut contadores = Vec::new();

    for i in 0..2 {
        let listo = out.join(format!("a{i}.ready"));
        let _ = fs::remove_file(&listo);
        let c = Command::new(&bin)
            .args([
                "--tamano",
                &tam,
                "--pasos",
                "0",
                "--semilla",
                &format!("{}", 7 + i),
                "--senal-listo",
                listo.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // esperar ready
        for _ in 0..5000 {
            if listo.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        let pid: i32 = fs::read_to_string(&listo).unwrap().trim().parse().unwrap();
        children.push(c);
        pids.push(pid);
    }
    // dejarlos trabajar
    thread::sleep(Duration::from_millis(200));

    let cong = Congelador::nuevo(Metodo::Sigstop, &pids, "demo").unwrap();
    let tf = cong.congelar().unwrap();
    println!("congelados {:?} en {tf:?}", pids);

    for &pid in &pids {
        let dir = out.join(format!("pid-{pid}"));
        let man = volcado::volcar(pid, &dir, true).unwrap();
        let reg = volcado::region_en(&man, BASE as u64).expect("región agente");
        let bytes = fs::read(volcado::archivo_region(&dir, reg)).unwrap();
        let cab = estado::cabecera_de_bytes(&bytes).unwrap();
        contadores.push(cab.contador);
        println!(
            "volcado pid={pid} bytes={} contador={} sha={}",
            man.bytes_totales,
            cab.contador,
            &reg.sha256[..16]
        );
        assert_eq!(sha256_hex(&bytes), reg.sha256);
    }
    cong.descongelar().unwrap();
    for c in &mut children {
        let _ = c.kill();
        let _ = c.wait();
    }

    // restaurar el primero en este proceso
    let dir = out.join(format!("pid-{}", pids[0]));
    let t0 = Instant::now();
    let r = restaurar::restaurar_agente(&dir, modo).expect("restaurar");
    println!(
        "restaurado modo={modo:?} listo={:?} carga_completa={:?} fallos={} (wall {:?})",
        r.listo,
        r.carga_completa,
        r.fallos,
        t0.elapsed()
    );
    unsafe { restaurar::verificar_contador(r.ptr, contadores[0]).unwrap() };
    let slice = unsafe { std::slice::from_raw_parts(r.ptr, r.tamano) };
    let man = volcado::cargar_manifiesto(&dir).unwrap();
    let reg = volcado::region_en(&man, BASE as u64).unwrap();
    assert_eq!(sha256_hex(slice), reg.sha256);
    println!(
        "OK integridad: contador={} sha256 coincide, tamano={}",
        contadores[0], tam_b
    );
}
