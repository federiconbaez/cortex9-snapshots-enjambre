//! Volcado de la memoria escribible de un proceso.
//!
//! Lee `/proc/pid/maps`, filtra regiones con permiso de escritura, lee el
//! contenido con `process_vm_readv` (y si falla, con `/proc/pid/mem`) y lo
//! guarda en un directorio de snapshot:
//!
//! ```text
//! snap/
//!   manifiesto.json
//!   regions/
//!     0000.bin   # sha256 en el manifiesto
//!     0001.bin
//! ```

use crate::sha256_hex;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub inicio: u64,
    pub fin: u64,
    pub perms: String,
    pub offset: u64,
    pub dispositivo: String,
    pub inodo: u64,
    pub ruta: String,
    pub archivo: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifiesto {
    pub pid: i32,
    pub pagina: usize,
    pub regiones: Vec<Region>,
    pub bytes_totales: u64,
    pub sha256_todo: String,
}

/// Una línea de `/proc/pid/maps`.
#[derive(Debug, Clone)]
pub struct MapEntry {
    pub inicio: u64,
    pub fin: u64,
    pub perms: String,
    pub offset: u64,
    pub dispositivo: String,
    pub inodo: u64,
    pub ruta: String,
}

pub fn parsear_maps(texto: &str) -> Vec<MapEntry> {
    let mut out = Vec::new();
    for linea in texto.lines() {
        let mut it = linea.split_whitespace();
        let rango = match it.next() {
            Some(r) => r,
            None => continue,
        };
        let mut rf = rango.split('-');
        let (Some(a), Some(b)) = (rf.next(), rf.next()) else { continue };
        let Ok(inicio) = u64::from_str_radix(a, 16) else { continue };
        let Ok(fin) = u64::from_str_radix(b, 16) else { continue };
        let perms = it.next().unwrap_or("").to_string();
        let offset = u64::from_str_radix(it.next().unwrap_or("0"), 16).unwrap_or(0);
        let dispositivo = it.next().unwrap_or("00:00").to_string();
        let inodo = it.next().unwrap_or("0").parse().unwrap_or(0);
        let ruta = it.collect::<Vec<_>>().join(" ");
        out.push(MapEntry { inicio, fin, perms, offset, dispositivo, inodo, ruta });
    }
    out
}

pub fn maps_de(pid: i32) -> io::Result<Vec<MapEntry>> {
    let t = fs::read_to_string(format!("/proc/{pid}/maps"))?;
    Ok(parsear_maps(&t))
}

/// Lee `len` bytes en la dirección virtual `addr` del proceso `pid`.
pub fn leer_remoto(pid: i32, addr: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    // process_vm_readv
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut _,
        iov_len: len,
    };
    let remoto = libc::iovec {
        iov_base: addr as *mut _,
        iov_len: len,
    };
    // SAFETY: iovecs apuntan a buffers válidos; el kernel valida el remoto.
    let n = unsafe {
        libc::process_vm_readv(
            pid,
            &local as *const libc::iovec,
            1,
            &remoto as *const libc::iovec,
            1,
            0,
        )
    };
    if n == len as isize {
        return Ok(buf);
    }
    // fallback: /proc/pid/mem
    let mut f = File::open(format!("/proc/{pid}/mem"))?;
    f.seek(SeekFrom::Start(addr))?;
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Volca todas las regiones escribibles de `pid` a `dir`.
///
/// Si `solo_anonimas` es true, solo se volcán regiones sin archivo de respaldo
/// (el caso típico del heap del agente de prueba).
pub fn volcar(pid: i32, dir: &Path, solo_anonimas: bool) -> io::Result<Manifiesto> {
    fs::create_dir_all(dir.join("regions"))?;
    let maps = maps_de(pid)?;
    let mut regiones = Vec::new();
    let mut hasher_todo = sha2::Sha256::new();
    let mut bytes_totales = 0u64;
    let mut idx = 0usize;

    for m in maps {
        if !m.perms.contains('w') {
            continue;
        }
        // saltar vsyscall / vvar
        if m.ruta.starts_with('[') && m.ruta != "[heap]" && m.ruta != "[stack]" && !m.ruta.is_empty()
        {
            // [anon:...] y similares sí nos interesan; [vvar]/[vdso]/[vsyscall] no
            if m.ruta.contains("vdso") || m.ruta.contains("vvar") || m.ruta.contains("vsyscall") {
                continue;
            }
        }
        if solo_anonimas {
            // mapa anónimo: ruta vacía, [heap], [stack], o [anon:...]
            let anon = m.ruta.is_empty()
                || m.ruta == "[heap]"
                || m.ruta == "[stack]"
                || m.ruta.starts_with("[anon");
            if !anon {
                continue;
            }
        }
        let len = (m.fin - m.inicio) as usize;
        if len == 0 {
            continue;
        }
        let datos = match leer_remoto(pid, m.inicio, len) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "aviso: no se pudo leer {:x}-{:x} ({}) de pid {pid}: {e}",
                    m.inicio, m.fin, m.ruta
                );
                continue;
            }
        };
        let sha = sha256_hex(&datos);
        hasher_todo.update(&datos);
        let archivo = format!("{idx:04}.bin");
        let mut f = File::create(dir.join("regions").join(&archivo))?;
        f.write_all(&datos)?;
        bytes_totales += datos.len() as u64;
        regiones.push(Region {
            inicio: m.inicio,
            fin: m.fin,
            perms: m.perms,
            offset: m.offset,
            dispositivo: m.dispositivo,
            inodo: m.inodo,
            ruta: m.ruta,
            archivo,
            sha256: sha,
            bytes: datos.len() as u64,
        });
        idx += 1;
    }

    let man = Manifiesto {
        pid,
        pagina: crate::pagina(),
        regiones,
        bytes_totales,
        sha256_todo: hex::encode(hasher_todo.finalize()),
    };
    let j = serde_json::to_string_pretty(&man).map_err(io::Error::other)?;
    fs::write(dir.join("manifiesto.json"), j)?;
    Ok(man)
}

/// Carga un manifiesto desde un snapshot.
pub fn cargar_manifiesto(dir: &Path) -> io::Result<Manifiesto> {
    let j = fs::read_to_string(dir.join("manifiesto.json"))?;
    serde_json::from_str(&j).map_err(io::Error::other)
}

/// Busca en el manifiesto una región que empiece en `inicio`.
pub fn region_en(man: &Manifiesto, inicio: u64) -> Option<&Region> {
    man.regiones.iter().find(|r| r.inicio == inicio)
}

/// Ruta al archivo binario de una región.
pub fn archivo_region(dir: &Path, r: &Region) -> PathBuf {
    dir.join("regions").join(&r.archivo)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_maps_basico() {
        let t = "555555554000-555555555000 r-xp 00000000 08:01 123 /bin/x\n\
                 7ffff7ff6000-7ffff7ff8000 rw-p 00000000 00:00 0\n\
                 7ffff7ff8000-7ffff7ffa000 r--p 00000000 00:00 0 [vvar]\n";
        let m = parsear_maps(t);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1].inicio, 0x7ffff7ff6000);
        assert!(m[1].perms.contains('w'));
        assert_eq!(m[2].ruta, "[vvar]");
    }
}
