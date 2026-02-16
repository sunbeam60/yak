use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};

use stream_fs::{EntryType, OpenMode, SfsDefault as Sfs, StreamHandle};

// ---------------------------------------------------------------------------
// Opaque handle types
// ---------------------------------------------------------------------------

/// Opaque handle to an open SFS file.
/// C consumers see `SfsFile*` — the internal layout is never exposed.
pub struct SfsFile(Sfs);

/// Opaque directory listing returned by `sfs_list`.
pub struct SfsList {
    entries: Vec<(CString, c_int)>,
}

/// Opaque verification result returned by `sfs_verify`.
pub struct SfsVerifyResult {
    issues: Vec<CString>,
}

// ---------------------------------------------------------------------------
// Thread-local error handling
// ---------------------------------------------------------------------------

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: &str) {
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = Some(
            CString::new(msg)
                .unwrap_or_else(|_| CString::new("error message contained null byte").unwrap()),
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
/// `s` must have been returned by `sfs_hello` and not yet freed.
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

/// # Safety
/// `path` must be a valid, null-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn sfs_create(
    path: *const c_char,
    block_index_width: u8,
    block_size_shift: u8,
) -> *mut SfsFile {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match Sfs::create(path, block_index_width, block_size_shift) {
        Ok(sfs) => Box::into_raw(Box::new(SfsFile(sfs))),
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Open an existing SFS file. mode: 0=READ, 1=WRITE.
///
/// # Safety
/// `path` must be a valid, null-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn sfs_open(path: *const c_char, mode: c_int) -> *mut SfsFile {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let mode = match mode {
        0 => OpenMode::Read,
        1 => OpenMode::Write,
        _ => {
            set_last_error("invalid open mode");
            return std::ptr::null_mut();
        }
    };
    match Sfs::open(path, mode) {
        Ok(sfs) => Box::into_raw(Box::new(SfsFile(sfs))),
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Close the SFS file, flushing any open stream handles.
/// Returns 0 on success, -1 on error. The handle is always consumed
/// regardless of the return value — callers must not reuse it.
///
/// # Safety
/// `handle` must be a valid pointer returned by `sfs_create` or `sfs_open`,
/// and must not have been closed already.
#[no_mangle]
pub unsafe extern "C" fn sfs_close(handle: *mut SfsFile) -> c_int {
    if handle.is_null() {
        return 0;
    }
    let sfs_file = unsafe { Box::from_raw(handle) };
    match sfs_file.0.close() {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Directory operations
// ---------------------------------------------------------------------------

/// # Safety
/// `handle` must be a live `SfsFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn sfs_mkdir(handle: *mut SfsFile, path: *const c_char) -> c_int {
    let sfs = unsafe { &(*handle).0 };
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

/// # Safety
/// `handle` must be a live `SfsFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn sfs_rmdir(handle: *mut SfsFile, path: *const c_char) -> c_int {
    let sfs = unsafe { &(*handle).0 };
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

/// # Safety
/// `handle` must be a live `SfsFile` pointer. Both path arguments must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn sfs_rename_dir(
    handle: *mut SfsFile,
    old_path: *const c_char,
    new_path: *const c_char,
) -> c_int {
    let sfs = unsafe { &(*handle).0 };
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

/// # Safety
/// `handle` must be a live `SfsFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn sfs_list(handle: *mut SfsFile, path: *const c_char) -> *mut SfsList {
    let sfs = unsafe { &(*handle).0 };
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
            }))
        }
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `list` must be a valid pointer returned by `sfs_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_list_count(list: *mut SfsList) -> c_int {
    let list = unsafe { &*list };
    list.entries.len() as c_int
}

/// # Safety
/// `list` must be a valid pointer returned by `sfs_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_list_entry_name(list: *mut SfsList, index: c_int) -> *const c_char {
    let list = unsafe { &*list };
    if index < 0 || (index as usize) >= list.entries.len() {
        return std::ptr::null();
    }
    list.entries[index as usize].0.as_ptr()
}

/// # Safety
/// `list` must be a valid pointer returned by `sfs_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_list_entry_type(list: *mut SfsList, index: c_int) -> c_int {
    let list = unsafe { &*list };
    if index < 0 || (index as usize) >= list.entries.len() {
        return -1;
    }
    list.entries[index as usize].1
}

/// # Safety
/// `list` must be a valid pointer returned by `sfs_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_list_free(list: *mut SfsList) {
    if !list.is_null() {
        unsafe {
            drop(Box::from_raw(list));
        }
    }
}

// ---------------------------------------------------------------------------
// Verify
// ---------------------------------------------------------------------------

/// Run integrity verification across all layers. Returns a result handle (NULL on error).
/// Use sfs_verify_count/sfs_verify_issue to read issues, then sfs_verify_free to release.
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer.
#[no_mangle]
pub unsafe extern "C" fn sfs_verify(handle: *mut SfsFile) -> *mut SfsVerifyResult {
    let sfs = unsafe { &(*handle).0 };
    match sfs.verify() {
        Ok(issues) => {
            let c_issues: Vec<CString> = issues
                .into_iter()
                .map(|s| CString::new(s).unwrap_or_else(|_| CString::new("?").unwrap()))
                .collect();
            Box::into_raw(Box::new(SfsVerifyResult { issues: c_issues }))
        }
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `result` must be a valid pointer returned by `sfs_verify` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_verify_count(result: *mut SfsVerifyResult) -> c_int {
    let result = unsafe { &*result };
    result.issues.len() as c_int
}

/// # Safety
/// `result` must be a valid pointer returned by `sfs_verify` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_verify_issue(
    result: *mut SfsVerifyResult,
    index: c_int,
) -> *const c_char {
    let result = unsafe { &*result };
    if index < 0 || (index as usize) >= result.issues.len() {
        return std::ptr::null();
    }
    result.issues[index as usize].as_ptr()
}

/// # Safety
/// `result` must be a valid pointer returned by `sfs_verify` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn sfs_verify_free(result: *mut SfsVerifyResult) {
    if !result.is_null() {
        unsafe {
            drop(Box::from_raw(result));
        }
    }
}

// ---------------------------------------------------------------------------
// Stream lifecycle
// ---------------------------------------------------------------------------

/// Create a new stream and open it for writing. Returns handle ID or -1.
/// `compressed`: non-zero to create a compressed stream (requires vbss > 0).
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn sfs_create_stream(
    handle: *mut SfsFile,
    path: *const c_char,
    compressed: i32,
) -> i64 {
    let sfs = unsafe { &(*handle).0 };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match sfs.create_stream(path, compressed != 0) {
        Ok(sh) => sh.id() as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Open an existing stream. mode: 0=READ, 1=WRITE. Returns handle ID or -1.
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn sfs_open_stream(
    handle: *mut SfsFile,
    path: *const c_char,
    mode: c_int,
) -> i64 {
    let sfs = unsafe { &(*handle).0 };
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
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn sfs_close_stream(handle: *mut SfsFile, stream: i64) -> c_int {
    let sfs = unsafe { &(*handle).0 };
    match sfs.close_stream(StreamHandle::from_id(stream as u64)) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Delete a stream by path. Must not be currently open.
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn sfs_delete_stream(handle: *mut SfsFile, path: *const c_char) -> c_int {
    let sfs = unsafe { &(*handle).0 };
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
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. Both path arguments must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn sfs_rename_stream(
    handle: *mut SfsFile,
    old_path: *const c_char,
    new_path: *const c_char,
) -> c_int {
    let sfs = unsafe { &(*handle).0 };
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
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
/// `buf` must point to at least `len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_read(
    handle: *mut SfsFile,
    stream: i64,
    buf: *mut u8,
    len: u64,
) -> i64 {
    let sfs = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    let buf = unsafe { std::slice::from_raw_parts_mut(buf, len as usize) };
    match sfs.read(&sh, buf) {
        Ok(n) => n as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Write `len` bytes. Returns bytes written or -1 on error.
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
/// `buf` must point to at least `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_write(
    handle: *mut SfsFile,
    stream: i64,
    buf: *const u8,
    len: u64,
) -> i64 {
    let sfs = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    let buf = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    match sfs.write(&sh, buf) {
        Ok(n) => n as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Set the head position. Returns 0 or -1.
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_seek(handle: *mut SfsFile, stream: i64, pos: u64) -> c_int {
    let sfs = unsafe { &(*handle).0 };
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
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_tell(handle: *mut SfsFile, stream: i64) -> i64 {
    let sfs = unsafe { &(*handle).0 };
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
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_length(handle: *mut SfsFile, stream: i64) -> i64 {
    let sfs = unsafe { &(*handle).0 };
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
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_truncate(
    handle: *mut SfsFile,
    stream: i64,
    new_len: u64,
) -> c_int {
    let sfs = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match sfs.truncate(&sh, new_len) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Pre-allocate storage for a stream. Returns 0 or -1.
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_reserve(
    handle: *mut SfsFile,
    stream: i64,
    n_bytes: u64,
) -> c_int {
    let sfs = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match sfs.reserve(&sh, n_bytes) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Get the reserved capacity of a stream. Returns capacity or -1 on error.
///
/// # Safety
/// `handle` must be a live `SfsFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn sfs_stream_reserved(handle: *mut SfsFile, stream: i64) -> i64 {
    let sfs = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match sfs.stream_reserved(&sh) {
        Ok(r) => r as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}
