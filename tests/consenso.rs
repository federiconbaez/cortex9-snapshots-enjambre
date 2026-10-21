//! Modo consenso: N agentes → barrera → congelar → volcar en paralelo →
//! matar originales → restaurar en N procesos nuevos → consenso + reanudación.

use enjambre_snap::consenso;
use enjambre_snap::enjambre::{restaurar_todos, Enjambre, Volcado};
use enjambre_snap::estado;
use enjambre_snap::sha256_hex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

static LOCK: Mutex<()> = Mutex::new(());

fn bin(n: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agente")).parent().unwrap().join(n)
}

fn tomar_volcado(nombre: &str, n: usize) -> (Volcado, PathBuf) {
    let dir = PathBuf::from(format!("/tmp/test-consenso-{nombre}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let seg = PathBuf::from(format!("/dev/shm/test-consenso-{nombre}-{}", std::process::id()));
    let mut e = Enjambre::lanzar(&bin("agente"), "4M", n, &dir, &seg).expect("lanzar");
    std::thread::sleep(Duration::from_millis(40));
    e.barrera().expect("barrera");
    e.congelar_todos().expect("congelar");
    let (vol, _) = e.volcar_todos(&dir).expect("volcar");
    e.matar();
    // el corte del volcado ya cumple el invariante
    let vista = consenso::leer_segmento(&vol.segmento).unwrap();
    consenso::verificar_invariante(&vista, &vol.contadores).expect("invariante en el volcado");
    assert!(vol.contadores.iter().all(|&c| c > 0), "los agentes debieron avanzar: {:?}", vol.contadores);
    (vol, dir)
}

fn seg_rest(nombre: &str) -> PathBuf {
    PathBuf::from(format!("/dev/shm/test-consenso-rest-{nombre}-{}", std::process::id()))
}

fn assert_ok(r: &enjambre_snap::enjambre::ResultadoEnjambre) {
    assert!(r.sha_ok, "sha por agente: {:?}", r.detalle_error);
    assert!(r.consenso_ok, "consenso: {:?}", r.detalle_error);
    assert!(r.reanudacion_ok, "reanudación: {:?}", r.detalle_error);
}

#[test]
fn enjambre_4_consenso_completo_y_mmap() {
    let _g = LOCK.lock().unwrap();
    let (vol, dir) = tomar_volcado("cm", 4);
    for modo in ["completo", "mmap"] {
        let r = restaurar_todos(&bin("restaurador"), &[], &vol, modo, &seg_rest(modo), 500).unwrap();
        assert_ok(&r);
        assert_eq!(r.hash_conjunto, vol.hash_conjunto);
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn enjambre_8_consenso_uffd() {
    let _g = LOCK.lock().unwrap();
    let root = unsafe { libc::geteuid() } == 0;
    let sudo = !root && Command::new("sudo").args(["-n", "true"]).status().map(|s| s.success()).unwrap_or(false);
    if !root && !sudo {
        eprintln!("SKIP uffd: ni root ni sudo -n (vm.unprivileged_userfaultfd=0)");
        return;
    }
    let pre: Vec<String> = if root { vec![] } else { vec!["sudo".into(), "-n".into()] };
    let (vol, dir) = tomar_volcado("uffd", 8);
    for modo in ["uffd", "uffd16"] {
        let r = restaurar_todos(&bin("restaurador"), &pre, &vol, modo, &seg_rest(modo), 500).unwrap();
        assert_ok(&r);
        assert!(r.fallos_uffd > 0, "uffd debió servir fallos reales");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn replica_determinista_coincide_bit_a_bit() {
    let _g = LOCK.lock().unwrap();
    let (vol, dir) = tomar_volcado("rep", 2);
    for i in 0..2 {
        let b = estado::reconstruir(4 << 20, vol.semillas[i], vol.contadores[i], vol.pids_origen[i]);
        assert_eq!(sha256_hex(&b), vol.shas[i], "agente {i}: la réplica independiente no coincide");
    }
    let _ = fs::remove_dir_all(dir);
}

fn region(vol: &Volcado, i: usize) -> PathBuf {
    let d: &Path = &vol.dirs[i];
    let man = enjambre_snap::volcado::cargar_manifiesto(d).unwrap();
    let r = enjambre_snap::volcado::region_en(&man, estado::BASE as u64).unwrap().clone();
    enjambre_snap::volcado::archivo_region(d, &r)
}

#[test]
fn detecta_heap_alterado() {
    let _g = LOCK.lock().unwrap();
    let (vol, dir) = tomar_volcado("mal", 3);
    let p = region(&vol, 1);
    let mut b = fs::read(&p).unwrap();
    b[4096 * 3 + 100] ^= 0xff; // un byte de datos, no la cabecera
    fs::write(&p, b).unwrap();
    let r = restaurar_todos(&bin("restaurador"), &[], &vol, "completo", &seg_rest("mal"), 10).unwrap();
    assert!(!r.sha_ok, "debió detectar el heap alterado");
    assert!(!r.consenso_ok);
    assert_ne!(r.hash_conjunto, vol.hash_conjunto);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn detecta_contador_compartido_alterado() {
    let _g = LOCK.lock().unwrap();
    let (mut vol, dir) = tomar_volcado("tot", 3);
    // total está en el offset 24 del segmento
    let t = u64::from_ne_bytes(vol.segmento[24..32].try_into().unwrap()) + 1;
    vol.segmento[24..32].copy_from_slice(&t.to_ne_bytes());
    let r = restaurar_todos(&bin("restaurador"), &[], &vol, "completo", &seg_rest("tot"), 10).unwrap();
    assert!(r.sha_ok, "los heaps están intactos");
    assert!(!r.consenso_ok, "debió detectar Σ contador != total");
    let e = r.detalle_error.unwrap();
    assert!(e.contains("total compartido"), "{e}");
    let _ = fs::remove_dir_all(dir);
}
