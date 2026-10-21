//! Congelado de un enjambre de procesos.
//!
//! - `Sigstop`: envía `SIGSTOP` a cada pid y espera a que `/proc/pid/stat`
//!   muestre el estado `T` (detenido). No requiere root para procesos propios.
//! - `Cgroup`: crea un cgroup v2 hijo del cgroup actual, mueve los pids y
//!   escribe `1` en `cgroup.freeze`; espera `frozen 1` en `cgroup.events`.
//!   Requiere permiso de escritura en el árbol de cgroups (en esta máquina: root).

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metodo {
    Sigstop,
    Cgroup,
}

impl std::str::FromStr for Metodo {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "sigstop" => Ok(Metodo::Sigstop),
            "cgroup" => Ok(Metodo::Cgroup),
            o => Err(format!("método desconocido: {o}")),
        }
    }
}

/// Estado del proceso según `/proc/pid/stat` (tercer campo).
pub fn estado_proceso(pid: i32) -> io::Result<char> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // el nombre va entre paréntesis y puede tener espacios
    let cierre = s.rfind(')').ok_or_else(|| io::Error::other("stat sin ')'"))?;
    s[cierre + 1..]
        .trim_start()
        .chars()
        .next()
        .ok_or_else(|| io::Error::other("stat vacío"))
}

/// Cgroup v2 actual de este proceso, como ruta en /sys/fs/cgroup.
pub fn cgroup_propio() -> io::Result<PathBuf> {
    let s = fs::read_to_string("/proc/self/cgroup")?;
    let linea = s
        .lines()
        .find(|l| l.starts_with("0::"))
        .ok_or_else(|| io::Error::other("no hay cgroup v2"))?;
    Ok(PathBuf::from("/sys/fs/cgroup").join(linea[3..].trim_start_matches('/')))
}

pub struct Congelador {
    pub metodo: Metodo,
    pub pids: Vec<i32>,
    cgroup: Option<PathBuf>,
    padre: Option<PathBuf>,
}

impl Congelador {
    /// Prepara el congelado. Con `Cgroup` crea el cgroup y mueve los pids (no cuenta como tiempo de congelado).
    pub fn nuevo(metodo: Metodo, pids: &[i32], nombre: &str) -> io::Result<Self> {
        let mut c = Congelador { metodo, pids: pids.to_vec(), cgroup: None, padre: None };
        if metodo == Metodo::Cgroup {
            let padre = cgroup_propio()?;
            let dir = padre.join(nombre);
            fs::create_dir_all(&dir)?;
            c.cgroup = Some(dir.clone());
            c.padre = Some(padre);
            for p in pids {
                fs::write(dir.join("cgroup.procs"), p.to_string())?;
            }
        }
        Ok(c)
    }

    /// Congela todos los procesos y espera la confirmación. Devuelve el tiempo total.
    pub fn congelar(&self) -> io::Result<Duration> {
        let t0 = Instant::now();
        match self.metodo {
            Metodo::Sigstop => {
                for &p in &self.pids {
                    // SAFETY: kill sin precondiciones de memoria.
                    if unsafe { libc::kill(p, libc::SIGSTOP) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                for &p in &self.pids {
                    // 'T' = stopped, 't' = tracing stop
                    esperar(|| {
                        let e = estado_proceso(p)?;
                        Ok(e == 'T' || e == 't')
                    })?;
                }
            }
            Metodo::Cgroup => {
                let dir = self.cgroup.as_ref().unwrap();
                fs::write(dir.join("cgroup.freeze"), "1")?;
                esperar(|| Ok(fs::read_to_string(dir.join("cgroup.events"))?.contains("frozen 1")))?;
            }
        }
        Ok(t0.elapsed())
    }

    pub fn descongelar(&self) -> io::Result<Duration> {
        let t0 = Instant::now();
        match self.metodo {
            Metodo::Sigstop => {
                for &p in &self.pids {
                    // SAFETY: idem.
                    unsafe { libc::kill(p, libc::SIGCONT) };
                }
            }
            Metodo::Cgroup => {
                let dir = self.cgroup.as_ref().unwrap();
                fs::write(dir.join("cgroup.freeze"), "0")?;
                esperar(|| Ok(fs::read_to_string(dir.join("cgroup.events"))?.contains("frozen 0")))?;
            }
        }
        Ok(t0.elapsed())
    }
}

impl Drop for Congelador {
    fn drop(&mut self) {
        if let (Some(dir), Some(padre)) = (&self.cgroup, &self.padre) {
            let _ = fs::write(dir.join("cgroup.freeze"), "0");
            for p in &self.pids {
                let _ = fs::write(padre.join("cgroup.procs"), p.to_string());
            }
            // si quedaran procesos (p.ej. hijos), rmdir falla y lo dejamos
            let _ = fs::remove_dir(dir);
        }
    }
}

fn esperar(mut f: impl FnMut() -> io::Result<bool>) -> io::Result<()> {
    let limite = Instant::now() + Duration::from_secs(10);
    loop {
        if f()? {
            return Ok(());
        }
        if Instant::now() > limite {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timeout esperando congelado"));
        }
        std::hint::spin_loop();
        std::thread::yield_now();
    }
}
