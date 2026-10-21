
use enjambre_snap::uffd::*;
use enjambre_snap::estado::BASE;
fn main() {
    println!("UffdioApi={}", std::mem::size_of::<UffdioApi>());
    println!("UffdioRegister={}", std::mem::size_of::<UffdioRegister>());
    println!("UffdioCopy={}", std::mem::size_of::<UffdioCopy>());
    println!("UffdMsg={}", std::mem::size_of::<UffdMsg>());
    println!("ioctl_api={:x}", ioctl_api());
    println!("ioctl_register={:x}", ioctl_register());
    println!("ioctl_copy={:x}", ioctl_copy());
    match UserfaultFd::crear() {
        Ok(u) => {
            println!("crear OK fd={}", u.as_raw());
            let tam: usize = 4<<20;
            let p = unsafe {
                libc::mmap(BASE as *mut _, tam, libc::PROT_READ|libc::PROT_WRITE,
                    libc::MAP_PRIVATE|libc::MAP_ANONYMOUS|libc::MAP_FIXED_NOREPLACE, -1, 0)
            };
            println!("mmap {:p} err={}", p, std::io::Error::last_os_error());
            match u.registrar_missing(BASE as u64, tam as u64) {
                Ok(()) => println!("register OK"),
                Err(e) => println!("register ERR {e}"),
            }
            let page = vec![0xabu8; 4096];
            match u.copiar(BASE as u64, page.as_ptr(), 4096) {
                Ok(()) => println!("copy OK"),
                Err(e) => println!("copy ERR {e}"),
            }
        }
        Err(e) => println!("crear ERR {e}"),
    }
}
