//! Snapshots de memoria para un enjambre de procesos-agente locales.
//!
//! Módulos:
//! - [`estado`]: formato del heap de un agente de prueba que coopera (vive en una región `mmap` fija).
//! - [`congelar`]: congelado de N procesos con `SIGSTOP` o con el freezer de cgroup v2.
//! - [`volcado`]: volcado de la memoria escribible (`/proc/pid/maps` + `process_vm_readv`) con manifiesto y sha256.
//! - [`uffd`]: envoltorio mínimo de `userfaultfd(2)`.
//! - [`consenso`]: segmento compartido en `/dev/shm`, barrera de corte consistente e invariante del enjambre.
//! - [`enjambre`]: orquestación de N agentes: congelar_todos, volcar_todos (en paralelo), restaurar_todos con reloj común.
//! - [`restaurar`]: restauración de una región: diferida con userfaultfd, `mmap` privado del archivo, o carga completa.

pub mod congelar;
pub mod consenso;
pub mod enjambre;
pub mod estado;
pub mod restaurar;
pub mod uffd;
pub mod volcado;

use sha2::{Digest, Sha256};

/// sha256 en hexadecimal.
pub fn sha256_hex(datos: &[u8]) -> String {
    hex::encode(Sha256::digest(datos))
}

/// `CLOCK_MONOTONIC` en nanosegundos (comparable entre procesos de la misma máquina).
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: puntero válido.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Tamaño de página del sistema.
pub fn pagina() -> usize {
    // SAFETY: sysconf no tiene precondiciones.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p <= 0 { 4096 } else { p as usize }
}

/// Parsea tamaños tipo `4M`, `64M`, `1G`, `512K` o bytes.
pub fn parsear_tamano(s: &str) -> Option<usize> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'K' | 'k' => (&s[..s.len() - 1], 1usize << 10),
        'M' | 'm' => (&s[..s.len() - 1], 1usize << 20),
        'G' | 'g' => (&s[..s.len() - 1], 1usize << 30),
        _ => (s, 1),
    };
    num.parse::<usize>().ok().map(|n| n * mult)
}

/// Formatea bytes en forma humana (MiB/GiB).
pub fn humano(b: usize) -> String {
    if b >= 1 << 30 && b % (1 << 30) == 0 {
        format!("{} GiB", b >> 30)
    } else if b >= 1 << 20 {
        format!("{} MiB", b >> 20)
    } else {
        format!("{} KiB", b >> 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tamanos() {
        assert_eq!(parsear_tamano("4M"), Some(4 << 20));
        assert_eq!(parsear_tamano("1G"), Some(1 << 30));
        assert_eq!(parsear_tamano("100"), Some(100));
        assert_eq!(parsear_tamano("x"), None);
    }
    #[test]
    fn sha_conocido() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
