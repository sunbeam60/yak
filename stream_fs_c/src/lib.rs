use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};

use stream_fs::{EntryType, OpenMode, SfsDefault as Sfs, StreamHandle};

// ---------------------------------------------------------------------------
// Thread-local error handling
// ---------------------------------------------------------------------------

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: &str) {
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = Some(
            CString::new(msg).unwrap_or_else(|_| {
                CString::new("error message contained null byte").unwrap()
            }),
        );
    });
}

#[no_mangle]
pub extern "C" fn sfs_last_error_message() -> *const c_char {
    LAST_ERROR.with(|e| match &*e.borrow() {
        Some(msg) => msg.as_ptr(),
        None => std::ptr::null(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a `&str` from a C string pointer, setting last error on failure.
fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        set_last_error("null pointer passed as string");
        return None;
    }
    match unsafe { CStr::from_ptr(ptr) }.to_str() {
        Ok(s) => Some(s),
        Err(e) => {
            set_last_error(&format!("invalid UTF-8: {}", e));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy hello
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn sfs_hello() -> *mut c_char {
    let greeting = stream_fs::hello();
    CString::new(greeting)
        .expect("greeting contained a null byte")
        .into_raw()
}

/// # Safety
/// The pointer must have been returned by `sfs_hello` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

// ---------------------------------------------------------------------------
// SFS file lifecycle
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn sfs_create(
    path: *const c_char,
    block_index_width: u8,
    block_size_shift: u8,
) -> *mut c_void {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match Sfs::create(path, block_index_width, block_size_shift) {
        Ok(sfs) => Box::into_raw(Box::new(sfs)) as *mut c_void,
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn sfs_open(path: *const c_char) -> *mut c_void {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match Sfs::open(path) {
        Ok(sfs) => Box::into_raw(Box::new(sfs)) as *mut c_void,
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `handle` must be a valid pointer returned by `sfs_create` or `sfs_open`,
/// and must not have been closed already.
#[no_mangle]
pub extern "C" fn sfs_close(handle: *mut c_void) {
    if !handle.is_null() {
        let sfs = unsafe { Box::from_raw(handle as *mut Sfs) };
        sfs.close();
    }
}

// ---------------------------------------------------------------------------
// Directory operations
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn sfs_mkdir(handle: *mut c_void, path: *const c_char) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match sfs.mkdir(path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn sfs_rmdir(handle: *mut c_void, path: *const c_char) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match sfs.rmdir(path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn sfs_rename_dir(
    handle: *mut c_void,
    old_path: *const c_char,
    new_path: *const c_char,
) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let old_path = match cstr_to_str(old_path) {
        Some(s) => s,
        None => return -1,
    };
    let new_path = match cstr_to_str(new_path) {
        Some(s) => s,
        None => return -1,
    };
    match sfs.rename_dir(old_path, new_path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

struct SfsList {
    entries: Vec<(CString, c_int)>,
}

#[no_mangle]
pub extern "C" fn sfs_list(handle: *mut c_void, path: *const c_char) -> *mut c_void {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match sfs.list(path) {
        Ok(entries) => {
            let list_entries: Vec<(CString, c_int)> = entries
                .into_iter()
                .map(|e| {
                    let type_val: c_int = match e.entry_type {
                        EntryType::Stream => 0,
                        EntryType::Directory => 1,
                    };
                    (
                        CString::new(e.name).unwrap_or_else(|_| CString::new("?").unwrap()),
                        type_val,
                    )
                })
                .collect();
            Box::into_raw(Box::new(SfsList {
                entries: list_entries,
            })) as *mut c_void
        }
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn sfs_list_count(list: *mut c_void) -> c_int {
    let list = unsafe { &*(list as *mut SfsList) };
    list.entries.len() as c_int
}

#[no_mangle]
pub extern "C" fn sfs_list_entry_name(list: *mut c_void, index: c_int) -> *const c_char {
    let list = unsafe { &*(list as *mut SfsList) };
    if index < 0 || (index as usize) >= list.entries.len() {
        return std::ptr::null();
    }
    list.entries[index as usize].0.as_ptr()
}

#[no_mangle]
pub extern "C" fn sfs_list_entry_type(list: *mut c_void, index: c_int) -> c_int {
    let list = unsafe { &*(list as *mut SfsList) };
    if index < 0 || (index as usize) >= list.entries.len() {
        return -1;
    }
    list.entries[index as usize].1
}

/// # Safety
/// `list` must be a valid pointer returned by `sfs_list` and not yet freed.
#[no_mangle]
pub extern "C" fn sfs_list_free(list: *mut c_void) {
    if !list.is_null() {
        unsafe {
            drop(Box::from_raw(list as *mut SfsList));
        }
    }
}

// ---------------------------------------------------------------------------
// Stream lifecycle
// ---------------------------------------------------------------------------

/// Create a new stream and open it for writing. Returns handle ID or -1.
#[no_mangle]
pub extern "C" fn sfs_create_stream(handle: *mut c_void, path: *const c_char) -> i64 {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match sfs.create_stream(path) {
        Ok(sh) => sh.id() as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Open an existing stream. mode: 0=READ, 1=WRITE. Returns handle ID or -1.
#[no_mangle]
pub extern "C" fn sfs_open_stream(
    handle: *mut c_void,
    path: *const c_char,
    mode: c_int,
) -> i64 {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    let mode = match mode {
        0 => OpenMode::Read,
        1 => OpenMode::Write,
        _ => {
            set_last_error("invalid open mode");
            return -1;
        }
    };
    match sfs.open_stream(path, mode) {
        Ok(sh) => sh.id() as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Close a stream handle.
#[no_mangle]
pub extern "C" fn sfs_close_stream(handle: *mut c_void, stream: i64) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    match sfs.close_stream(StreamHandle::from_id(stream as u64)) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Delete a stream by path. Must not be currently open.
#[no_mangle]
pub extern "C" fn sfs_delete_stream(handle: *mut c_void, path: *const c_char) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match sfs.delete_stream(path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Rename/move a stream by path. Must not be currently open.
#[no_mangle]
pub extern "C" fn sfs_rename_stream(
    handle: *mut c_void,
    old_path: *const c_char,
    new_path: *const c_char,
) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let old_path = match cstr_to_str(old_path) {
        Some(s) => s,
        None => return -1,
    };
    let new_path = match cstr_to_str(new_path) {
        Some(s) => s,
        None => return -1,
    };
    match sfs.rename_stream(old_path, new_path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Stream I/O
// ---------------------------------------------------------------------------

/// Read up to `len` bytes. Returns bytes read or -1 on error.
#[no_mangle]
pub extern "C" fn sfs_read(
    handle: *mut c_void,
    stream: i64,
    buf: *mut c_void,
    len: u64,
) -> i64 {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let sh = StreamHandle::from_id(stream as u64);
    let buf = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, len as usize) };
    match sfs.read(&sh, buf) {
        Ok(n) => n as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Write `len` bytes. Returns bytes written or -1 on error.
#[no_mangle]
pub extern "C" fn sfs_write(
    handle: *mut c_void,
    stream: i64,
    buf: *const c_void,
    len: u64,
) -> i64 {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let sh = StreamHandle::from_id(stream as u64);
    let buf = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) };
    match sfs.write(&sh, buf) {
        Ok(n) => n as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Set the head position. Returns 0 or -1.
#[no_mangle]
pub extern "C" fn sfs_seek(handle: *mut c_void, stream: i64, pos: u64) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let sh = StreamHandle::from_id(stream as u64);
    match sfs.seek(&sh, pos) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Get the current head position. Returns position or -1 on error.
#[no_mangle]
pub extern "C" fn sfs_tell(handle: *mut c_void, stream: i64) -> i64 {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let sh = StreamHandle::from_id(stream as u64);
    match sfs.tell(&sh) {
        Ok(pos) => pos as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Get the stream length. Returns length or -1 on error.
#[no_mangle]
pub extern "C" fn sfs_stream_length(handle: *mut c_void, stream: i64) -> i64 {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let sh = StreamHandle::from_id(stream as u64);
    match sfs.stream_length(&sh) {
        Ok(len) => len as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Truncate a stream. Returns 0 or -1.
#[no_mangle]
pub extern "C" fn sfs_truncate(handle: *mut c_void, stream: i64, new_len: u64) -> c_int {
    let sfs = unsafe { &*(handle as *const Sfs) };
    let sh = StreamHandle::from_id(stream as u64);
    match sfs.truncate(&sh, new_len) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}
