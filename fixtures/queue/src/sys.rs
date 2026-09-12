use std::ffi::CString;

pub fn mkfifo(path: &str, mode: u16) -> i32 {
    let c_path = CString::new(path).unwrap();
    let p_path = c_path.as_ptr();
    unsafe {
        return raw::mkfifo(p_path, mode as libc::mode_t);
    }
}

pub fn unlink(path: &str) -> i32 {
    println!("unlink(\"{}\")", path);
    let c_path = CString::new(path).unwrap();
    let p_path = c_path.as_ptr();
    unsafe {
        return raw::unlink(p_path);
    }
}

mod raw {

    use libc::{c_char, c_int, mode_t};

    unsafe extern "system" {
        pub fn mkfifo(path: *const c_char, mode: mode_t) -> c_int;
        pub fn unlink(path: *const c_char) -> c_int;
    }
}
