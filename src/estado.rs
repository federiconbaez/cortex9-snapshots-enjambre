//! Heap de un agente de prueba que coopera.
//!
//! Todo el estado del agente vive en una única región anónima mapeada en una
//! dirección fija ([`BASE`]). Así, tras restaurar la región en la misma
//! dirección de un proceso nuevo, los punteros internos siguen siendo válidos.
//!
//! Layout: la primera página es una [`Cabecera`]; el resto son datos
//! pseudoaleatorios deterministas (xorshift64 a partir de una semilla) que el
//! agente va mutando: en cada paso incrementa el contador y escribe su valor en
//! una página que depende del contador.

/// Dirección fija del heap del agente (igual en el original y en el restaurado).
pub const BASE: usize = 0x5f00_0000_0000;
/// Firma de la cabecera ("FNBRMYCE").
pub const MAGIA: u64 = 0x4543_594d_5242_4e46;

/// Cabecera al principio del heap.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cabecera {
    pub magia: u64,
    pub tamano: u64,
    pub contador: u64,
    pub semilla: u64,
    pub pid_origen: u64,
}

fn xorshift(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

/// Inicializa el heap con datos deterministas y la cabecera.
///
/// # Safety
/// `ptr` debe apuntar a `tamano` bytes escribibles, alineados a 8, con `tamano >= 2 páginas`.
pub unsafe fn inicializar(ptr: *mut u8, tamano: usize, semilla: u64) {
    let pag = crate::pagina();
    assert!(tamano >= 2 * pag && tamano % 8 == 0);
    let mut x = semilla | 1;
    // SAFETY: rango válido garantizado por el llamador.
    let palabras = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u64, tamano / 8) };
    for p in palabras.iter_mut() {
        *p = xorshift(&mut x);
    }
    let cab = Cabecera {
        magia: MAGIA,
        tamano: tamano as u64,
        contador: 0,
        semilla,
        pid_origen: std::process::id() as u64,
    };
    // SAFETY: la cabecera entra en la primera página.
    unsafe { std::ptr::write_volatile(ptr as *mut Cabecera, cab) };
}

/// Lee la cabecera.
///
/// # Safety
/// `ptr` debe apuntar a un heap de agente válido.
pub unsafe fn cabecera(ptr: *const u8) -> Cabecera {
    // SAFETY: garantizado por el llamador.
    unsafe { std::ptr::read_volatile(ptr as *const Cabecera) }
}

/// Lee la cabecera desde bytes (por ejemplo, un archivo de región volcado).
pub fn cabecera_de_bytes(b: &[u8]) -> Option<Cabecera> {
    if b.len() < std::mem::size_of::<Cabecera>() {
        return None;
    }
    let u = |i: usize| u64::from_ne_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    let c = Cabecera { magia: u(0), tamano: u(1), contador: u(2), semilla: u(3), pid_origen: u(4) };
    (c.magia == MAGIA).then_some(c)
}

/// Un paso de trabajo del agente: incrementa el contador y lo escribe en una
/// página de datos elegida a partir del contador. Devuelve el contador nuevo.
///
/// # Safety
/// `ptr` debe apuntar a un heap de agente válido de `tamano` bytes.
pub unsafe fn paso(ptr: *mut u8, tamano: usize) -> u64 {
    let pag = crate::pagina();
    let paginas_datos = (tamano / pag) - 1;
    // SAFETY: heap válido.
    unsafe {
        let cab = ptr as *mut Cabecera;
        let c = std::ptr::read_volatile(&(*cab).contador) + 1;
        std::ptr::write_volatile(&mut (*cab).contador, c);
        let idx = 1 + (c.wrapping_mul(7919) as usize % paginas_datos);
        let destino = ptr.add(idx * pag + 8) as *mut u64;
        std::ptr::write_volatile(destino, c);
        c
    }
}

/// Réplica independiente: reconstruye desde cero el heap que debería tener un
/// agente con `semilla` tras `contador` pasos (el estado es determinista).
/// `pid_origen` se copia para comparar bit a bit con un volcado.
pub fn reconstruir(tamano: usize, semilla: u64, contador: u64, pid_origen: u64) -> Vec<u8> {
    let mut palabras = vec![0u64; tamano / 8];
    let ptr = palabras.as_mut_ptr() as *mut u8;
    // SAFETY: buffer propio alineado a 8 de `tamano` bytes.
    unsafe {
        inicializar(ptr, tamano, semilla);
        (*(ptr as *mut Cabecera)).pid_origen = pid_origen;
        for _ in 0..contador {
            paso(ptr, tamano);
        }
    }
    palabras.iter().flat_map(|w| w.to_ne_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn determinista_y_paso() {
        let pag = crate::pagina();
        let t = 8 * pag;
        let mut a = vec![0u64; t / 8];
        let mut b = vec![0u64; t / 8];
        unsafe {
            inicializar(a.as_mut_ptr() as *mut u8, t, 42);
            inicializar(b.as_mut_ptr() as *mut u8, t, 42);
        }
        // la cabecera incluye pid igual (mismo proceso), así que todo es igual
        assert_eq!(a, b);
        let c = unsafe { paso(a.as_mut_ptr() as *mut u8, t) };
        assert_eq!(c, 1);
        assert_ne!(a, b);
        let bytes: Vec<u8> = a.iter().flat_map(|w| w.to_ne_bytes()).collect();
        assert_eq!(cabecera_de_bytes(&bytes).unwrap().contador, 1);
    }
}
