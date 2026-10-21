//! CLI del hipervisor: congelar, volcar, restaurar (demo) un enjambre.
//!
//!   snap freeze --pids 1,2,3 --metodo sigstop|cgroup
//!   snap dump --pid N --out DIR [--solo-anonimas]
//!   snap dump-enjambre --pids 1,2,3 --out DIR --metodo sigstop [--solo-anonimas]
//!   snap restore --in DIR --modo completo|mmap|uffd
//!   snap criu-probe   # documenta si criu check pasa

use enjambre_snap::congelar::{self, Congelador, Metodo};
use enjambre_snap::restaurar::{self, Modo};
use enjambre_snap::volcado;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

fn uso() -> ! {
    eprintln!(
        "uso:\n  snap freeze --pids a,b --metodo sigstop|cgroup\n  \
         snap dump --pid N --out DIR [--solo-anonimas]\n  \
         snap dump-enjambre --pids a,b --out DIR --metodo sigstop|cgroup [--solo-anonimas]\n  \
         snap restore --in DIR --modo completo|mmap|uffd\n  \
         snap criu-probe"
    );
    std::process::exit(2);
}

fn arg(args: &[String], n: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == n).map(|w| w[1].clone())
}
fn flag(args: &[String], n: &str) -> bool {
    args.iter().any(|a| a == n)
}
fn pids(s: &str) -> Vec<i32> {
    s.split(',').filter_map(|x| x.trim().parse().ok()).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    match cmd {
        "freeze" => {
            let ps = pids(&arg(&args, "--pids").unwrap_or_else(|| uso()));
            let m: Metodo = arg(&args, "--metodo")
                .unwrap_or_else(|| "sigstop".into())
                .parse()
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    uso();
                });
            let c = Congelador::nuevo(m, &ps, "enjambre-freeze").expect("congelador");
            let d = c.congelar().expect("congelar");
            println!("congelado {ps:?} en {d:?}");
            let d2 = c.descongelar().expect("descongelar");
            println!("descongelado en {d2:?}");
        }
        "dump" => {
            let pid: i32 = arg(&args, "--pid")
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| uso());
            let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| uso()));
            let solo = flag(&args, "--solo-anonimas");
            let t0 = Instant::now();
            let man = volcado::volcar(pid, &out, solo).expect("volcar");
            println!(
                "volcado pid={pid} regiones={} bytes={} en {:?} sha={}",
                man.regiones.len(),
                man.bytes_totales,
                t0.elapsed(),
                &man.sha256_todo[..16]
            );
        }
        "dump-enjambre" => {
            let ps = pids(&arg(&args, "--pids").unwrap_or_else(|| uso()));
            let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| uso()));
            let m: Metodo = arg(&args, "--metodo")
                .unwrap_or_else(|| "sigstop".into())
                .parse()
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    uso();
                });
            let solo = flag(&args, "--solo-anonimas");
            let c = Congelador::nuevo(m, &ps, "enjambre-dump").expect("congelador");
            let tf = c.congelar().expect("congelar");
            println!("congelado en {tf:?}");
            for &pid in &ps {
                let dir = out.join(format!("pid-{pid}"));
                let t0 = Instant::now();
                let man = volcado::volcar(pid, &dir, solo).expect("volcar");
                println!(
                    "  pid={pid} regiones={} bytes={} en {:?}",
                    man.regiones.len(),
                    man.bytes_totales,
                    t0.elapsed()
                );
            }
            let td = c.descongelar().expect("descongelar");
            println!("descongelado en {td:?}");
        }
        "restore" => {
            let dir = PathBuf::from(arg(&args, "--in").unwrap_or_else(|| uso()));
            let modo: Modo = arg(&args, "--modo")
                .unwrap_or_else(|| "completo".into())
                .parse()
                .unwrap_or_else(|e| {
                    eprintln!("{e}");
                    uso();
                });
            let r = restaurar::restaurar_agente(&dir, modo).expect("restaurar");
            unsafe { restaurar::verificar_contador(r.ptr, {
                // leer contador del manifiesto/bytes
                let man = volcado::cargar_manifiesto(&dir).unwrap();
                let reg = volcado::region_en(&man, enjambre_snap::estado::BASE as u64).unwrap();
                let b = std::fs::read(volcado::archivo_region(&dir, reg)).unwrap();
                enjambre_snap::estado::cabecera_de_bytes(&b)
                    .unwrap()
                    .contador
            })
            .expect("verificar") };
            println!(
                "restaurado modo={modo:?} listo={:?} carga_completa={:?} fallos={}",
                r.listo, r.carga_completa, r.fallos
            );
        }
        "criu-probe" => {
            println!("=== criu --version ===");
            let _ = Command::new("/usr/sbin/criu").arg("--version").status();
            println!("=== criu check ===");
            let st = Command::new("sudo")
                .args(["-n", "/usr/sbin/criu", "check"])
                .status();
            println!("status={st:?}");
            println!(
                "nota: en esta máquina criu 4.1.1 está instalado pero `criu check` \
                 reporta 'Does not look good' (faltan módulos diag de sockets y \
                 sys/kernel/ns_last_pid). No se usa CRIU para el camino verificado; \
                 el camino verificado es el agente cooperativo + volcado + uffd/mmap."
            );
            let _ = congelar::cgroup_propio();
        }
        _ => uso(),
    }
}
