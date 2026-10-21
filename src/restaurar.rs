//! Restauración de una región de agente.
//!
//! Tres modos:
//! - [`Modo::Completo`]: `mmap` anónimo + `memcpy` de todo el archivo (carga completa).
//! - [`Modo::MmapArchivo`]: `mmap` privado del archivo de región (el kernel pagina desde el disco).
//! - [`Modo::Uffd`]: `mmap` anónimo vacío + registro MISSING en userfaultfd; un hilo
//!   entrega páginas bajo demanda con `UFFDIO_COPY`. El tiempo hasta "listo" es el
//!   hasta que el mapeo está registrado; el de carga completa se mide tocando todas
//!   las páginas o dejando que el consumidor las pida.

use crate::estado::{self, BASE};
use crate::uffd::UserfaultFd;
use crate::volcado::{self, Manifiesto};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    Completo,
    MmapArchivo,
    /// userfaultfd, una página por fallo (diferido puro).
    Uffd,
    /// userfaultfd con ventana de prefetch: en cada fallo se copian hasta
    /// [`VENTANA_PREFETCH`] páginas contiguas aún no servidas con un solo `UFFDIO_COPY`.
    UffdPrefetch,
}

/// Páginas por `UFFDIO_COPY` en [`Modo::UffdPrefetch`] (16 × 4 KiB = 64 KiB).
pub const VENTANA_PREFETCH: usize = 16;

impl std::str::FromStr for Modo {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "completo" => Ok(Modo::Completo),
            "mmap" => Ok(Modo::MmapArchivo),
            "uffd" => Ok(Modo::Uffd),
            "uffd16" => Ok(Modo::UffdPrefetch),
            o => Err(format!("modo desconocido: {o}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResultadoRestaurar {
    /// Tiempo hasta que el mapeo está listo para empezar a ejecutar (uffd registrado
    /// o memcpy terminado o mmap del archivo hecho).
    pub listo: Duration,
    /// Tiempo hasta que todas las páginas están materializadas en RAM.
    pub carga_completa: Duration,
    /// Páginas entregadas por el manejador uffd (0 en otros modos).
    pub fallos: u64,
    /// Puntero a la base restaurada.
    pub ptr: *mut u8,
    pub tamano: usize,
}

/// Restaura la región del agente (dirección [`BASE`]) desde un snapshot.
///
/// # Safety
/// Devuelve un puntero a memoria mapeada; el llamador no debe dejar que se
/// desmapee mientras lo usa. En esta caja de pruebas el proceso termina sin
/// unmap explícito (el kernel limpia al salir).
pub fn restaurar_agente(dir: &Path, modo: Modo) -> io::Result<ResultadoRestaurar> {
    let man = volcado::cargar_manifiesto(dir)?;
    let reg = volcado::region_en(&man, BASE as u64).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no hay región del agente en 0x{BASE:x} en el manifiesto"),
        )
    })?;
    let path = volcado::archivo_region(dir, reg);
    let tamano = reg.bytes as usize;
    let sha_esp = reg.sha256.clone();

    match modo {
        Modo::Completo => restaurar_completo(&path, tamano, &sha_esp),
        Modo::MmapArchivo => restaurar_mmap_archivo(&path, tamano, &sha_esp),
        // la API histórica sirve página por página en ambos modos uffd
        Modo::Uffd | Modo::UffdPrefetch => restaurar_uffd(&path, tamano, &sha_esp),
    }
}

fn mapear_anon(tamano: usize) -> io::Result<*mut u8> {
    // SAFETY: mmap con dirección fija y tamaño válido.
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
        return Err(io::Error::last_os_error());
    }
    if p as usize != BASE {
        return Err(io::Error::other(format!(
            "mmap no respetó BASE: got {p:p}"
        )));
    }
    Ok(p as *mut u8)
}

fn restaurar_completo(path: &Path, tamano: usize, sha_esp: &str) -> io::Result<ResultadoRestaurar> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; tamano];
    let t0 = Instant::now();
    f.read_exact(&mut buf)?;
    let ptr = mapear_anon(tamano)?;
    // SAFETY: ptr apunta a `tamano` bytes escribibles.
    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, tamano) };
    let listo = t0.elapsed();
    let sha = crate::sha256_hex(unsafe { std::slice::from_raw_parts(ptr, tamano) });
    if sha != sha_esp {
        return Err(io::Error::other(format!("sha mismatch: {sha} != {sha_esp}")));
    }
    Ok(ResultadoRestaurar {
        listo,
        carga_completa: listo,
        fallos: 0,
        ptr,
        tamano,
    })
}

fn restaurar_mmap_archivo(path: &Path, tamano: usize, sha_esp: &str) -> io::Result<ResultadoRestaurar> {
    let f = File::open(path)?;
    let t0 = Instant::now();
    // SAFETY: mmap del archivo.
    let p = unsafe {
        libc::mmap(
            BASE as *mut libc::c_void,
            tamano,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_FIXED_NOREPLACE,
            f.as_raw_fd(),
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let listo = t0.elapsed();
    // materializar tocando
    let t1 = Instant::now();
    let pag = crate::pagina();
    let slice = unsafe { std::slice::from_raw_parts_mut(p as *mut u8, tamano) };
    let mut acc = 0u8;
    for i in (0..tamano).step_by(pag) {
        acc ^= slice[i];
    }
    std::hint::black_box(acc);
    let carga = t0.elapsed();
    let _ = t1;
    let sha = crate::sha256_hex(slice);
    if sha != sha_esp {
        return Err(io::Error::other(format!("sha mismatch: {sha} != {sha_esp}")));
    }
    Ok(ResultadoRestaurar {
        listo,
        carga_completa: carga,
        fallos: 0,
        ptr: p as *mut u8,
        tamano,
    })
}

fn restaurar_uffd(path: &Path, tamano: usize, sha_esp: &str) -> io::Result<ResultadoRestaurar> {
    // Cargar el archivo a un buffer de respaldo (fuente de UFFDIO_COPY).
    // Esto NO cuenta como "listo para ejecutar": es la imagen en disco/RAM del
    // hipervisor, no del proceso restaurado. Lo medimos aparte como preparación.
    let mut f = File::open(path)?;
    let mut backing = vec![0u8; tamano];
    f.read_exact(&mut backing)?;
    let backing = Arc::new(backing);

    let t0 = Instant::now();
    let ptr = mapear_anon(tamano)?;
    let uffd = UserfaultFd::crear()?;
    uffd.registrar_missing(BASE as u64, tamano as u64)?;
    let listo = t0.elapsed();

    let fallos = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let uffd_fd = uffd.as_raw();
    // mover ownership del OwnedFd al hilo vía raw + from_raw al final es frágil;
    // mejor: el hilo usa el mismo UserfaultFd a través de un canal de mensajes.
    // Simplificación: el hilo maneja fallos; el hilo principal toca páginas.
    let backing_h = Arc::clone(&backing);
    let fallos_h = Arc::clone(&fallos);
    let done_h = Arc::clone(&done);
    // SAFETY: transferimos el fd al hilo; el padre no lo usa más hasta join.
    let uffd_arc = Arc::new(uffd);
    let uffd_h = Arc::clone(&uffd_arc);
    let pag = crate::pagina() as u64;
    let handle = thread::spawn(move || {
        while !done_h.load(Ordering::Acquire) {
            match uffd_h.leer_pagefault() {
                Ok(addr) => {
                    let page = addr & !(pag - 1);
                    let off = (page as usize).saturating_sub(BASE);
                    if off >= backing_h.len() {
                        continue;
                    }
                    let src = unsafe { backing_h.as_ptr().add(off) };
                    if uffd_h.copiar(page, src, pag).is_ok() {
                        fallos_h.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
                Err(_) => break,
            }
        }
        // drenar pendientes brevemente
        for _ in 0..64 {
            match uffd_h.leer_pagefault() {
                Ok(addr) => {
                    let page = addr & !(pag - 1);
                    let off = (page as usize).saturating_sub(BASE);
                    if off < backing_h.len() {
                        let src = unsafe { backing_h.as_ptr().add(off) };
                        let _ = uffd_h.copiar(page, src, pag);
                        fallos_h.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(_) => break,
            }
        }
    });

    // materializar tocando todas las páginas (carga completa bajo demanda)
    let t1 = Instant::now();
    let slice = unsafe { std::slice::from_raw_parts_mut(ptr, tamano) };
    let mut acc = 0u8;
    let step = crate::pagina();
    for i in (0..tamano).step_by(step) {
        acc ^= slice[i];
    }
    std::hint::black_box(acc);
    let carga = t0.elapsed();
    let _ = t1;
    done.store(true, Ordering::Release);
    let _ = handle.join();

    let sha = crate::sha256_hex(slice);
    if sha != sha_esp {
        return Err(io::Error::other(format!("sha mismatch: {sha} != {sha_esp}")));
    }
    let _ = uffd_fd;
    Ok(ResultadoRestaurar {
        listo,
        carga_completa: carga,
        fallos: fallos.load(Ordering::Relaxed),
        ptr,
        tamano,
    })
}

/// Verifica que la cabecera del agente restaurado tiene el contador esperado.
///
/// # Safety
/// `ptr` debe apuntar a un heap de agente válido.
pub unsafe fn verificar_contador(ptr: *const u8, esperado: u64) -> io::Result<()> {
    // SAFETY: contrato del llamador.
    let c = unsafe { estado::cabecera(ptr) };
    if c.magia != estado::MAGIA {
        return Err(io::Error::other("magia inválida"));
    }
    if c.contador != esperado {
        return Err(io::Error::other(format!(
            "contador {} != esperado {esperado}",
            c.contador
        )));
    }
    Ok(())
}

/// Utilidad: lee el manifiesto y resume.
pub fn resumen(man: &Manifiesto) -> String {
    format!(
        "pid={} regiones={} bytes={} sha256_todo={}",
        man.pid,
        man.regiones.len(),
        man.bytes_totales,
        &man.sha256_todo[..16]
    )
}

// ---------------------------------------------------------------------------
// API por fases (usada por el restaurador de enjambre)
// ---------------------------------------------------------------------------

/// Manejador uffd vivo mientras el proceso restaurado ya ejecuta.
struct Manejador {
    hilo: thread::JoinHandle<()>,
    fin: Arc<AtomicBool>,
    fallos: Arc<AtomicU64>,
}

/// Imagen de la región mapeada en solo lectura (fuente de `UFFDIO_COPY`), sin copia previa.
struct ImagenMapeada {
    ptr: *const u8,
    len: usize,
}
// SAFETY: solo lectura, vive hasta que el manejador termina.
unsafe impl Send for ImagenMapeada {}
unsafe impl Sync for ImagenMapeada {}
impl Drop for ImagenMapeada {
    fn drop(&mut self) {
        // SAFETY: mapeo propio.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

/// Restauración en curso de la región del agente en [`BASE`].
///
/// Fases medidas por el llamador con [`crate::monotonic_ns`]:
/// `iniciar` → **listo** (se puede ejecutar), `primer_acceso` (cabecera leída,
/// con fallo de página real en uffd), `materializar` → **carga completa**.
pub struct Restauracion {
    pub modo: Modo,
    pub ptr: *mut u8,
    pub tamano: usize,
    manejador: Option<Manejador>,
    _imagen: Option<Arc<ImagenMapeada>>,
    _uffd: Option<Arc<UserfaultFd>>,
}

/// Prepara lo que no es restauración (abrir el archivo). Devuelve el archivo abierto.
pub fn preparar(path: &Path) -> io::Result<(File, usize)> {
    let f = File::open(path)?;
    let len = f.metadata()?.len() as usize;
    Ok((f, len))
}

impl Restauracion {
    /// Mapea la región y la deja **lista para ejecutar** según el modo.
    pub fn iniciar(archivo: &File, tamano: usize, modo: Modo) -> io::Result<Self> {
        match modo {
            Modo::Completo => {
                let ptr = mapear_anon(tamano)?;
                // leer directo al mapeo restaurado (una sola copia: page cache → región)
                // SAFETY: `tamano` bytes escribibles en ptr.
                let dst = unsafe { std::slice::from_raw_parts_mut(ptr, tamano) };
                let mut leidos = 0;
                while leidos < tamano {
                    // SAFETY: pread a un buffer válido.
                    let n = unsafe {
                        libc::pread(
                            archivo.as_raw_fd(),
                            dst[leidos..].as_mut_ptr() as *mut libc::c_void,
                            tamano - leidos,
                            leidos as libc::off_t,
                        )
                    };
                    if n <= 0 {
                        return Err(io::Error::last_os_error());
                    }
                    leidos += n as usize;
                }
                Ok(Self { modo, ptr, tamano, manejador: None, _imagen: None, _uffd: None })
            }
            Modo::MmapArchivo => {
                // SAFETY: mmap privado del archivo en BASE.
                let p = unsafe {
                    libc::mmap(
                        BASE as *mut libc::c_void,
                        tamano,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_PRIVATE | libc::MAP_FIXED_NOREPLACE,
                        archivo.as_raw_fd(),
                        0,
                    )
                };
                if p == libc::MAP_FAILED {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self { modo, ptr: p as *mut u8, tamano, manejador: None, _imagen: None, _uffd: None })
            }
            Modo::Uffd | Modo::UffdPrefetch => {
                let ventana = if modo == Modo::UffdPrefetch { VENTANA_PREFETCH } else { 1 };
                // SAFETY: mmap de solo lectura de la imagen (fuente de UFFDIO_COPY).
                let src = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        tamano,
                        libc::PROT_READ,
                        libc::MAP_PRIVATE,
                        archivo.as_raw_fd(),
                        0,
                    )
                };
                if src == libc::MAP_FAILED {
                    return Err(io::Error::last_os_error());
                }
                let imagen = Arc::new(ImagenMapeada { ptr: src as *const u8, len: tamano });
                let ptr = mapear_anon(tamano)?;
                let uffd = Arc::new(UserfaultFd::crear()?);
                uffd.registrar_missing(BASE as u64, tamano as u64)?;
                let fin = Arc::new(AtomicBool::new(false));
                let fallos = Arc::new(AtomicU64::new(0));
                let (u, im, f, fa) = (Arc::clone(&uffd), Arc::clone(&imagen), Arc::clone(&fin), Arc::clone(&fallos));
                let pag = crate::pagina();
                let hilo = thread::spawn(move || {
                    // Solo este hilo puebla la región (MISSING): el mapa de bits es exacto.
                    let total = im.len / pag;
                    let mut servida = vec![false; total];
                    loop {
                    match u.leer_pagefault() {
                        Ok(addr) => {
                            let idx = (addr as usize).wrapping_sub(BASE) / pag;
                            if idx >= total || servida[idx] {
                                continue;
                            }
                            let mut fin_v = idx + 1;
                            while fin_v < total && fin_v - idx < ventana && !servida[fin_v] {
                                fin_v += 1;
                            }
                            // SAFETY: rango dentro de la imagen mapeada.
                            let s = unsafe { im.ptr.add(idx * pag) };
                            let dst = (BASE + idx * pag) as u64;
                            if u.copiar(dst, s, ((fin_v - idx) * pag) as u64).is_ok() {
                                servida[idx..fin_v].iter_mut().for_each(|b| *b = true);
                                fa.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                            if f.load(Ordering::Acquire) {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                    }
                });
                Ok(Self {
                    modo,
                    ptr,
                    tamano,
                    manejador: Some(Manejador { hilo, fin, fallos }),
                    _imagen: Some(imagen),
                    _uffd: Some(uffd),
                })
            }
        }
    }

    /// Lee la cabecera (en uffd provoca un fallo de página real servido por el manejador).
    pub fn primer_acceso(&self) -> estado::Cabecera {
        // SAFETY: región mapeada (y servida por uffd si corresponde).
        unsafe { estado::cabecera(self.ptr) }
    }

    /// Toca todas las páginas: deja toda la región en RAM.
    pub fn materializar(&self) {
        let pag = crate::pagina();
        let mut acc = 0u8;
        for off in (0..self.tamano).step_by(pag) {
            // SAFETY: dentro de la región.
            acc ^= unsafe { std::ptr::read_volatile(self.ptr.add(off)) };
        }
        std::hint::black_box(acc);
    }

    /// sha256 de la región restaurada.
    pub fn sha256(&self) -> String {
        // SAFETY: región mapeada de `tamano` bytes.
        crate::sha256_hex(unsafe { std::slice::from_raw_parts(self.ptr, self.tamano) })
    }

    /// Detiene el manejador uffd (si lo hay) y devuelve las páginas servidas.
    /// Llamar después de `materializar`: ya no quedan páginas por pedir.
    pub fn cerrar_manejador(&mut self) -> u64 {
        match self.manejador.take() {
            Some(m) => {
                m.fin.store(true, Ordering::Release);
                let _ = m.hilo.join();
                m.fallos.load(Ordering::Relaxed)
            }
            None => 0,
        }
    }
}
