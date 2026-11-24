//! `getauxval` for the ARM build. The NDK links against glibc 2.12, which lacks it (added in 2.16), while
//! Rust's std calls it when it starts a thread. The TV's own libc has it; this reads the same table.
use std::ffi::c_ulong;

#[no_mangle]
pub extern "C" fn getauxval(key: c_ulong) -> c_ulong {
    let mut table = [0 as c_ulong; 128];
    unsafe {
        let fd = libc::open(c"/proc/self/auxv".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return 0;
        }
        let n = libc::read(fd, table.as_mut_ptr().cast(), std::mem::size_of_val(&table));
        libc::close(fd);
        let pairs = table[..n.max(0) as usize / std::mem::size_of::<c_ulong>()].chunks_exact(2);
        pairs
            .take_while(|p| p[0] != 0)
            .find(|p| p[0] == key)
            .map_or(0, |p| p[1])
    }
}
