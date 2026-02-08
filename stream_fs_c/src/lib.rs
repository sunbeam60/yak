use std::ffi::CString;
use std::os::raw::c_char;

/// Returns a hello world greeting from the Stream File System library.
///
/// The caller is responsible for freeing the returned string by passing it
/// to `sfs_free_string`.
#[no_mangle]
pub extern "C" fn sfs_hello() -> *mut c_char {
    let greeting = stream_fs::hello();
    CString::new(greeting)
        .expect("greeting contained a null byte")
        .into_raw()
}

/// Frees a string previously returned by an `sfs_*` function.
///
/// # Safety
///
/// The pointer must have been returned by a function in this library
/// (e.g. `sfs_hello`) and must not have been freed already.
#[no_mangle]
pub unsafe extern "C" fn sfs_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}
