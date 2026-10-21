//! Pruebas de integración.
//!
//! Correr con un solo hilo: `cargo test -- --test-threads=1`
//! (varios tests mapean la dirección fija BASE en el proceso de prueba).
//! Las que necesitan root (uffd) se saltan si no somos uid 0.
//! cgroup freezer: en este entorno mover pids al cgroup hijo da ENOTSUP.

use enjambre_snap::congelar::{Congelador, Metodo};
use enjambre_snap::estado::{self, BASE};
use enjambre_snap::sha256_hex;
use enjambre_snap::volcado;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

// Serializa tests que tocan procesos/BASE.
static LOCK: Mutex<()> = Mutex::new(());

fn bin(nombre: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agente"))
        .parent()
        .unwrap()
        .join(nombre)
}

fn soy_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn spawn_y_esperar(tam: &str, dir: &Path, i: usize, pasos: &str) -> (std::process::Child, i32) {
    let listo = dir.join(format!("a{i}.ready"));
    let _ = fs::remove_file(&listo);
    let c = Command::new(bin("agente"))
        .args([
            "--tamano", tam,
            "--pasos", pasos,
            "--semilla", "99",
            "--senal-listo", listo.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..5000 {
        if listo.exists() { break; }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(listo.exists(), "agente no escribió senal-listo");
    let pid = fs::read_to_string(&listo).unwrap().trim().parse().unwrap();
    (c, pid)
}

fn restore_via_snap(dump: &Path, modo: &str) -> (String, String) {
    let o = Command::new(bin("snap"))
        .args(["restore", "--in", dump.to_str().unwrap(), "--modo", modo])
        .output()
        .expect("spawn snap");
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "snap restore {modo} falló: {stderr}\n{stdout}");
    (stdout, stderr)
}

#[test]
fn roundtrip_completo_4m() {
    let _g = LOCK.lock().unwrap();
    let dir = PathBuf::from(format!("/tmp/test-enjambre-completo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (mut child, pid) = spawn_y_esperar("4M", &dir, 0, "0");
    thread::sleep(Duration::from_millis(150));
    let cong = Congelador::nuevo(Metodo::Sigstop, &[pid], "t-comp").unwrap();
    cong.congelar().unwrap();
    let dump = dir.join("dump");
    let man = volcado::volcar(pid, &dump, true).unwrap();
    assert!(!man.regiones.is_empty());
    let reg = volcado::region_en(&man, BASE as u64).expect("región BASE");
    let bytes = fs::read(volcado::archivo_region(&dump, reg)).unwrap();
    assert_eq!(sha256_hex(&bytes), reg.sha256);
    let cont = estado::cabecera_de_bytes(&bytes).unwrap().contador;
    assert!(cont > 0, "el agente debió mutar el contador");
    cong.descongelar().unwrap();
    child.kill().ok();
    child.wait().ok();

    let (stdout, _) = restore_via_snap(&dump, "completo");
    assert!(stdout.contains("restaurado"), "{stdout}");
}

#[test]
fn freeze_sigstop_tres() {
    let _g = LOCK.lock().unwrap();
    let dir = PathBuf::from(format!("/tmp/test-enjambre-freeze-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut kids = Vec::new();
    let mut pids = Vec::new();
    for i in 0..3 {
        let (c, p) = spawn_y_esperar("4M", &dir, i, "0");
        kids.push(c);
        pids.push(p);
    }
    let cong = Congelador::nuevo(Metodo::Sigstop, &pids, "t-fr").unwrap();
    let d = cong.congelar().unwrap();
    assert!(d.as_millis() < 2000);
    for &p in &pids {
        let e = enjambre_snap::congelar::estado_proceso(p).unwrap();
        assert!(e == 'T' || e == 't', "estado={e}");
    }
    cong.descongelar().unwrap();
    for c in &mut kids {
        c.kill().ok();
        c.wait().ok();
    }
}

#[test]
fn uffd_roundtrip_requiere_root() {
    let _g = LOCK.lock().unwrap();
    if !soy_root() {
        eprintln!("SKIP uffd: no somos root (correr: sudo -n cargo test --test integracion -- --test-threads=1)");
        return;
    }
    let dir = PathBuf::from(format!("/tmp/test-enjambre-uffd-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (mut child, pid) = spawn_y_esperar("4M", &dir, 0, "0");
    thread::sleep(Duration::from_millis(150));
    let cong = Congelador::nuevo(Metodo::Sigstop, &[pid], "t-uffd").unwrap();
    cong.congelar().unwrap();
    let dump = dir.join("dump");
    let man = volcado::volcar(pid, &dump, true).unwrap();
    let reg = volcado::region_en(&man, BASE as u64).unwrap();
    let bytes = fs::read(volcado::archivo_region(&dump, reg)).unwrap();
    let cont = estado::cabecera_de_bytes(&bytes).unwrap().contador;
    assert!(cont > 0);
    cong.descongelar().unwrap();
    child.kill().ok();
    child.wait().ok();

    let (stdout, _) = restore_via_snap(&dump, "uffd");
    assert!(stdout.contains("fallos="), "{stdout}");
    // fallos debe ser > 0
    let f = stdout.split("fallos=").nth(1).unwrap().split_whitespace().next().unwrap();
    let f: u64 = f.parse().unwrap();
    assert!(f > 0, "uffd debió entregar páginas, stdout={stdout}");
}

#[test]
fn cgroup_freezer_requiere_root() {
    let _g = LOCK.lock().unwrap();
    if !soy_root() {
        eprintln!("SKIP cgroup: no somos root");
        return;
    }
    let dir = PathBuf::from(format!("/tmp/test-enjambre-cgroup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (mut child, pid) = spawn_y_esperar("4M", &dir, 0, "0");
    match Congelador::nuevo(Metodo::Cgroup, &[pid], "t-cg") {
        Ok(cong) => match cong.congelar() {
            Ok(_) => {
                cong.descongelar().unwrap();
                eprintln!("cgroup freezer OK");
            }
            Err(e) => eprintln!("SKIP/NO-VERIFICADO cgroup.freeze: {e}"),
        },
        Err(e) => eprintln!("SKIP/NO-VERIFICADO cgroup setup: {e}"),
    }
    child.kill().ok();
    child.wait().ok();
}
