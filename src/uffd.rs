//! Envoltorio mínimo de `userfaultfd(2)`.
//!
//! En esta máquina `/dev/userfaultfd` existe pero `vm.unprivileged_userfaultfd=0`,
//! así que crear el fd requiere root (o `CAP_SYS_PTRACE`). Documentado en el README.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub const UFFD_API: u64 = 0xAA;
pub const UFFDIO_API: u64 = 0xc018_aa3f; // _IOWR('?', 0x3F, struct uffdio_api) — se recalcula abajo
pub const UFFDIO_REGISTER: u64 = 0xc020_aa00;
pub const UFFDIO_COPY: u64 = 0xc028_aa03;
pub const UFFD_FEATURE_MISSING: u64 = 1 << 0;
pub const UFFDIO_REGISTER_MODE_MISSING: u64 = 1 << 0;
pub const UFFD_EVENT_PAGEFAULT: u8 = 0x12;

// Los ioctl números de userfaultfd son específicos del kernel; los construimos
// con las macros de Linux en tiempo de compilación vía constantes conocidas.
// Para x86_64 glibc/linux:
//   #define _IOWR(type,nr,size) ...
// Usamos valores comprobados en runtime con una prueba pequeña.

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UffdioApi {
    pub api: u64,
    pub features: u64,
    pub ioctls: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UffdioRange {
    pub start: u64,
    pub len: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UffdioRegister {
    pub range: UffdioRange,
    pub mode: u64,
    pub ioctls: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UffdioCopy {
    pub dst: u64,
    pub src: u64,
    pub len: u64,
    pub mode: u64,
    pub copy: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UffdMsg {
    pub event: u8,
    pub reserved1: u8,
    pub reserved2: u16,
    pub reserved3: u32,
    pub arg: UffdMsgArg,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union UffdMsgArg {
    pub pagefault: UffdPagefault,
    pub reserved: [u8; 24],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UffdPagefault {
    pub flags: u64,
    pub address: u64,
    pub feat: UffdPagefaultFeat,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union UffdPagefaultFeat {
    pub ptid: u32,
    pub reserved: u32,
}

impl Default for UffdMsg {
    fn default() -> Self {
        Self {
            event: 0,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            arg: UffdMsgArg { reserved: [0; 24] },
        }
    }
}

/// Construye el número de ioctl `_IOWR(type, nr, size)` de Linux.
const fn iowr(ty: u8, nr: u8, size: usize) -> u64 {
    let dir: u64 = 3; // _IOC_READ|_IOC_WRITE
    ((dir) << 30) | ((ty as u64) << 8) | (nr as u64) | ((size as u64) << 16)
}

/// Tipo de ioctl de userfaultfd (`#define UFFDIO 0xAA` en linux/userfaultfd.h).
const UFFDIO: u8 = 0xAA;

pub fn ioctl_api() -> u64 {
    iowr(UFFDIO, 0x3F, std::mem::size_of::<UffdioApi>())
}
pub fn ioctl_register() -> u64 {
    iowr(UFFDIO, 0x00, std::mem::size_of::<UffdioRegister>())
}
pub fn ioctl_copy() -> u64 {
    iowr(UFFDIO, 0x03, std::mem::size_of::<UffdioCopy>())
}

pub struct UserfaultFd {
    fd: OwnedFd,
}

impl UserfaultFd {
    /// Crea un userfaultfd. Si `unprivileged` es false, abre `/dev/userfaultfd`
    /// (necesita root cuando `vm.unprivileged_userfaultfd=0`).
    pub fn crear() -> io::Result<Self> {
        // intentar syscall primero (funciona si el sysctl está en 1)
        // SAFETY: syscall con flags válidos.
        let mut fd = unsafe { libc::syscall(libc::SYS_userfaultfd, libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if fd < 0 {
            // fallback: /dev/userfaultfd
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/userfaultfd")?;
            // ioctl USERFAULTFD_IOC_NEW = _IO('!', 0x0)
            let ioc_new: u64 = 0xaa00; // USERFAULTFD_IOC_NEW
            // SAFETY: ioctl USERFAULTFD_IOC_NEW
            fd = unsafe { libc::ioctl(f.as_raw_fd(), ioc_new as _, 0i32) as i64 };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // poner NONBLOCK + CLOEXEC
            // SAFETY: fcntl en fd recién creado
            unsafe {
                let fl = libc::fcntl(fd as RawFd, libc::F_GETFL);
                libc::fcntl(fd as RawFd, libc::F_SETFL, fl | libc::O_NONBLOCK);
                libc::fcntl(fd as RawFd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        // SAFETY: fd válido del kernel
        let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        let mut api = UffdioApi { api: UFFD_API, features: 0, ioctls: 0 };
        // SAFETY: ioctl UFFDIO_API
        let r = unsafe { libc::ioctl(fd.as_raw_fd(), ioctl_api() as _, &mut api) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    pub fn as_raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Registra un rango con modo MISSING (faltan las páginas; se entregan con COPY).
    pub fn registrar_missing(&self, start: u64, len: u64) -> io::Result<()> {
        let mut reg = UffdioRegister {
            range: UffdioRange { start, len },
            mode: UFFDIO_REGISTER_MODE_MISSING,
            ioctls: 0,
        };
        // SAFETY: ioctl UFFDIO_REGISTER
        let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_register() as _, &mut reg) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Copia `len` bytes de `src` a la dirección virtual `dst` registrada.
    pub fn copiar(&self, dst: u64, src: *const u8, len: u64) -> io::Result<()> {
        let mut c = UffdioCopy {
            dst,
            src: src as u64,
            len,
            mode: 0,
            copy: 0,
        };
        // SAFETY: ioctl UFFDIO_COPY; src debe ser válido durante la llamada.
        let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), ioctl_copy() as _, &mut c) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Lee un mensaje de pagefault (bloqueante con poll).
    pub fn leer_pagefault(&self) -> io::Result<u64> {
        loop {
            let mut msg = UffdMsg::default();
            // SAFETY: read en el fd de uffd.
            let n = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    &mut msg as *mut _ as *mut libc::c_void,
                    std::mem::size_of::<UffdMsg>(),
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock {
                    // poll
                    let mut pfd = libc::pollfd {
                        fd: self.fd.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: poll
                    let pr = unsafe { libc::poll(&mut pfd, 1, 20) };
                    if pr < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if pr == 0 {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "uffd poll timeout"));
                    }
                    continue;
                }
                return Err(e);
            }
            if msg.event != UFFD_EVENT_PAGEFAULT {
                continue;
            }
            // SAFETY: event es pagefault, el union es pagefault.
            let addr = unsafe { msg.arg.pagefault.address };
            return Ok(addr);
        }
    }
}
