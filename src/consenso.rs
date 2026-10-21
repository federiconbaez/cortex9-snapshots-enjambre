//! Modo **consenso**: un segmento compartido en `/dev/shm` y una barrera
//! cooperativa que deja al enjambre en un **corte consistente** antes del
//! congelado.
//!
//! Cada agente `i`, en cada paso, hace (y solo entre pasos mira la barrera):
//!
//! ```text
//! c = paso(heap)              // contador local en la cabecera del heap
//! total.fetch_add(1)          // contador compartido del enjambre
//! contador[i].store(c)        // espejo del contador local
//! ```
//!
//! El orquestador pide la pausa escribiendo una `epoca` impar y espera que
//! cada agente responda `ack[i] = epoca`. Desde ese momento ningún agente está
//! a mitad de un paso, así que vale el invariante
//!
//! ```text
//! total == Σ_i heap_i.contador   y   contador[i] == heap_i.contador
//! ```
//!
//! Recién entonces se congela con `SIGSTOP` y se vuelca. Tras restaurar los N
//! heaps en N procesos nuevos y copiar el segmento, el invariante y el
//! **hash conjunto** (sha256 de los sha de cada heap + el segmento) tienen que
//! coincidir bit a bit con los del volcado; y tras la reanudación coordinada
//! (`epoca` par) con K pasos por agente, `total' == total + N·K`.

use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Máximo de agentes por segmento.
pub const MAX_AGENTES: usize = 64;
/// Firma del segmento ("FNBRCONS").
pub const MAGIA_SEG: u64 = 0x534e_4f43_5242_4e46;
/// Bytes del segmento en disco/`/dev/shm` (una página).
pub const BYTES_SEG: usize = 4096;

/// Layout del segmento compartido.
#[repr(C)]
pub struct Segmento {
    pub magia: u64,
    pub n: u64,
    /// Impar = pausa pedida; par = correr.
    pub epoca: AtomicU64,
    /// Pasos totales del enjambre.
    pub total: AtomicU64,
    pub reservado: u64,
    pub ack: [AtomicU64; MAX_AGENTES],
    pub contador: [AtomicU64; MAX_AGENTES],
}

const _: () = assert!(std::mem::size_of::<Segmento>() <= BYTES_SEG);

/// Un mapeo `MAP_SHARED` de un archivo-segmento.
pub struct Mapeo {
    ptr: *mut Segmento,
}

// SAFETY: el segmento solo se accede con atómicos (salvo magia/n, inmutables tras crear).
unsafe impl Send for Mapeo {}
unsafe impl Sync for Mapeo {}

impl Mapeo {
    /// Crea (o trunca) el archivo, lo mapea e inicializa para `n` agentes.
    pub fn crear(ruta: &Path, n: usize) -> io::Result<Self> {
        if n == 0 || n > MAX_AGENTES {
            return Err(io::Error::other(format!("n={n} fuera de 1..={MAX_AGENTES}")));
        }
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(ruta)?;
        f.set_len(BYTES_SEG as u64)?;
        let m = Self::mapear(&f)?;
        // SAFETY: recién mapeado, nadie más lo usa todavía; archivo en cero.
        unsafe {
            (*m.ptr).magia = MAGIA_SEG;
            (*m.ptr).n = n as u64;
        }
        Ok(m)
    }

    /// Abre un segmento existente (verifica la firma).
    pub fn abrir(ruta: &Path) -> io::Result<Self> {
        let f = OpenOptions::new().read(true).write(true).open(ruta)?;
        let m = Self::mapear(&f)?;
        if m.seg().magia != MAGIA_SEG {
            return Err(io::Error::other("segmento sin firma FNBRCONS"));
        }
        Ok(m)
    }

    fn mapear(f: &std::fs::File) -> io::Result<Self> {
        // SAFETY: mmap compartido de un archivo de BYTES_SEG bytes.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                BYTES_SEG,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                f.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr: p as *mut Segmento })
    }

    pub fn seg(&self) -> &Segmento {
        // SAFETY: mapeo válido mientras viva `self`.
        unsafe { &*self.ptr }
    }

    pub fn n(&self) -> usize {
        self.seg().n as usize
    }

    /// Pide la pausa y espera el `ack` de todos. Devuelve (época, tiempo).
    pub fn pausar(&self, limite: Duration) -> io::Result<(u64, Duration)> {
        let t0 = Instant::now();
        let s = self.seg();
        let actual = s.epoca.load(Ordering::Acquire);
        let e = if actual & 1 == 0 { actual + 1 } else { actual + 2 };
        s.epoca.store(e, Ordering::Release);
        let n = self.n();
        loop {
            if (0..n).all(|i| s.ack[i].load(Ordering::Acquire) == e) {
                return Ok((e, t0.elapsed()));
            }
            if t0.elapsed() > limite {
                let faltan: Vec<usize> =
                    (0..n).filter(|&i| s.ack[i].load(Ordering::Acquire) != e).collect();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("barrera: sin ack de {faltan:?}"),
                ));
            }
            std::hint::spin_loop();
            std::thread::yield_now();
        }
    }

    /// Reanuda: época par siguiente.
    pub fn reanudar(&self) {
        let s = self.seg();
        let e = s.epoca.load(Ordering::Acquire);
        if e & 1 == 1 {
            s.epoca.store(e + 1, Ordering::Release);
        }
    }

    /// Lado agente: si hay pausa, confirma y espera; devuelve cuando puede dar un paso.
    /// Devuelve `false` si `salir()` se volvió verdadero mientras esperaba.
    pub fn punto_seguro(&self, i: usize, salir: impl Fn() -> bool) -> bool {
        let s = self.seg();
        loop {
            let e = s.epoca.load(Ordering::Acquire);
            if e & 1 == 0 {
                return true;
            }
            if s.ack[i].load(Ordering::Relaxed) != e {
                s.ack[i].store(e, Ordering::Release);
            }
            if salir() {
                return false;
            }
            std::hint::spin_loop();
            std::thread::yield_now();
        }
    }

    /// Lado agente: registra un paso ya aplicado al heap.
    pub fn registrar_paso(&self, i: usize, contador_local: u64) {
        let s = self.seg();
        s.total.fetch_add(1, Ordering::AcqRel);
        s.contador[i].store(contador_local, Ordering::Release);
    }

    /// Copia cruda del segmento (para el volcado).
    pub fn bytes(&self) -> Vec<u8> {
        // SAFETY: BYTES_SEG bytes mapeados.
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, BYTES_SEG).to_vec() }
    }
}

impl Drop for Mapeo {
    fn drop(&mut self) {
        // SAFETY: mapeo propio.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, BYTES_SEG) };
    }
}

/// Señal de largada común para N procesos: un `u64` atómico en un archivo de
/// `/dev/shm`. Los restauradores giran sobre él; el orquestador escribe 1.
pub struct Largada {
    ptr: *mut AtomicU64,
}
// SAFETY: solo acceso atómico.
unsafe impl Send for Largada {}
unsafe impl Sync for Largada {}

impl Largada {
    pub fn crear(ruta: &Path) -> io::Result<Self> {
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(ruta)?;
        f.set_len(8)?;
        Self::mapear(&f)
    }
    pub fn abrir(ruta: &Path) -> io::Result<Self> {
        let f = OpenOptions::new().read(true).write(true).open(ruta)?;
        Self::mapear(&f)
    }
    fn mapear(f: &std::fs::File) -> io::Result<Self> {
        // SAFETY: mmap compartido de 8 bytes (se redondea a una página).
        let p = unsafe {
            libc::mmap(std::ptr::null_mut(), 8, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, f.as_raw_fd(), 0)
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr: p as *mut AtomicU64 })
    }
    pub fn dar(&self) {
        // SAFETY: mapeo válido.
        unsafe { (*self.ptr).store(1, Ordering::Release) };
    }
    pub fn esperar(&self) {
        // SAFETY: mapeo válido.
        while unsafe { (*self.ptr).load(Ordering::Acquire) } == 0 {
            std::hint::spin_loop();
            std::thread::yield_now();
        }
    }
}
impl Drop for Largada {
    fn drop(&mut self) {
        // SAFETY: mapeo propio.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, 8) };
    }
}

/// Vista de solo lectura de un segmento a partir de bytes volcados.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VistaSegmento {
    pub n: usize,
    pub epoca: u64,
    pub total: u64,
    pub ack: Vec<u64>,
    pub contador: Vec<u64>,
}

pub fn leer_segmento(b: &[u8]) -> io::Result<VistaSegmento> {
    if b.len() < std::mem::size_of::<Segmento>() {
        return Err(io::Error::other("segmento corto"));
    }
    let u = |i: usize| u64::from_ne_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    if u(0) != MAGIA_SEG {
        return Err(io::Error::other("segmento sin firma"));
    }
    let n = u(1) as usize;
    if n == 0 || n > MAX_AGENTES {
        return Err(io::Error::other("n inválido"));
    }
    let base_ack = 5;
    let base_cont = base_ack + MAX_AGENTES;
    Ok(VistaSegmento {
        n,
        epoca: u(2),
        total: u(3),
        ack: (0..n).map(|i| u(base_ack + i)).collect(),
        contador: (0..n).map(|i| u(base_cont + i)).collect(),
    })
}

/// Hash conjunto del enjambre: sha256(sha_heap_0 ‖ … ‖ sha_heap_{n-1} ‖ segmento).
pub fn hash_conjunto(shas_heap: &[String], segmento: &[u8]) -> String {
    let mut h = Sha256::new();
    for (i, s) in shas_heap.iter().enumerate() {
        h.update((i as u64).to_le_bytes());
        h.update(s.as_bytes());
    }
    h.update(segmento);
    hex::encode(h.finalize())
}

/// Verifica el invariante del corte consistente.
pub fn verificar_invariante(seg: &VistaSegmento, contadores_heap: &[u64]) -> Result<(), String> {
    if contadores_heap.len() != seg.n {
        return Err(format!("heaps={} != n={}", contadores_heap.len(), seg.n));
    }
    let suma: u64 = contadores_heap.iter().sum();
    if suma != seg.total {
        return Err(format!("Σ contador heap = {suma} != total compartido = {}", seg.total));
    }
    for (i, (&h, &s)) in contadores_heap.iter().zip(&seg.contador).enumerate() {
        if h != s {
            return Err(format!("agente {i}: heap={h} != espejo={s}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn barrera_entre_hilos_da_corte_consistente() {
        let ruta = std::env::temp_dir().join(format!("seg-test-{}", std::process::id()));
        let m = Arc::new(Mapeo::crear(&ruta, 4).unwrap());
        let salir = Arc::new(AtomicBool::new(false));
        let mut hs = Vec::new();
        for i in 0..4 {
            let m = Arc::clone(&m);
            let salir = Arc::clone(&salir);
            hs.push(std::thread::spawn(move || {
                let mut local = 0u64;
                while m.punto_seguro(i, || salir.load(Ordering::Acquire)) {
                    if salir.load(Ordering::Acquire) {
                        break;
                    }
                    local += 1;
                    m.registrar_paso(i, local);
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(20));
        for _ in 0..5 {
            m.pausar(Duration::from_secs(5)).unwrap();
            let v = leer_segmento(&m.bytes()).unwrap();
            // los hilos no avanzan durante la pausa: el espejo suma el total
            assert_eq!(v.contador.iter().sum::<u64>(), v.total);
            std::thread::sleep(Duration::from_millis(2));
            assert_eq!(leer_segmento(&m.bytes()).unwrap(), v, "hubo pasos durante la pausa");
            m.reanudar();
            std::thread::sleep(Duration::from_millis(5));
        }
        salir.store(true, Ordering::Release);
        for h in hs {
            h.join().unwrap();
        }
        let _ = std::fs::remove_file(ruta);
    }

    #[test]
    fn hash_conjunto_sensible_al_orden() {
        let a = hash_conjunto(&["x".into(), "y".into()], b"s");
        let b = hash_conjunto(&["y".into(), "x".into()], b"s");
        assert_ne!(a, b);
        assert_eq!(a, hash_conjunto(&["x".into(), "y".into()], b"s"));
    }
}
