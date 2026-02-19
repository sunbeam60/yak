use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};

use yak::{CreateOptions, EntryType, OpenMode, StreamHandle, YakDefault as YakInner};

// ---------------------------------------------------------------------------
// Opaque handle types
// ---------------------------------------------------------------------------

/// Opaque handle to an open Yak file.
/// C consumers see `YakFile*` — the internal layout is never exposed.
pub struct YakFile(YakInner);

/// Opaque directory listing returned by `yak_list`.
pub struct YakList {
    entries: Vec<(CString, c_int)>,
}

/// Opaque verification result returned by `yak_verify`.
pub struct YakVerifyResult {
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
pub extern "C" fn yak_last_error_message() -> *const c_char {
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
// Yak file lifecycle
// ---------------------------------------------------------------------------

/// # Safety
/// `path` must be a valid, null-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn yak_create(
    path: *const c_char,
    block_index_width: u8,
    block_size_shift: u8,
) -> *mut YakFile {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match YakInner::create(
        path,
        CreateOptions {
            block_index_width,
            block_size_shift,
            ..Default::default()
        },
    ) {
        Ok(inner) => Box::into_raw(Box::new(YakFile(inner))),
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Create a new Yak file with a custom compressed block size shift for compression.
/// `compressed_block_size_shift` must be >= `block_size_shift`.
///
/// # Safety
/// `path` must be a valid, null-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn yak_create_with_cbss(
    path: *const c_char,
    block_index_width: u8,
    block_size_shift: u8,
    compressed_block_size_shift: u8,
) -> *mut YakFile {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match YakInner::create(
        path,
        CreateOptions {
            block_index_width,
            block_size_shift,
            compressed_block_size_shift,
            ..Default::default()
        },
    ) {
        Ok(inner) => Box::into_raw(Box::new(YakFile(inner))),
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Open an existing Yak file. mode: 0=READ, 1=WRITE.
///
/// # Safety
/// `path` must be a valid, null-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn yak_open(path: *const c_char, mode: c_int) -> *mut YakFile {
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
    match YakInner::open(path, mode) {
        Ok(inner) => Box::into_raw(Box::new(YakFile(inner))),
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Create a new encrypted Yak file. `password` is a null-terminated UTF-8 string.
/// Returns NULL on error.
///
/// # Safety
/// `path` and `password` must be valid, null-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn yak_create_encrypted(
    path: *const c_char,
    block_index_width: u8,
    block_size_shift: u8,
    password: *const c_char,
) -> *mut YakFile {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let password = match cstr_to_str(password) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match YakInner::create(
        path,
        CreateOptions {
            block_index_width,
            block_size_shift,
            password: Some(password.as_bytes()),
            ..Default::default()
        },
    ) {
        Ok(inner) => Box::into_raw(Box::new(YakFile(inner))),
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Open an existing encrypted Yak file. mode: 0=READ, 1=WRITE.
/// `password` is a null-terminated UTF-8 string.
/// Returns NULL on error (`yak_last_error_message` will indicate wrong password
/// or missing password).
///
/// # Safety
/// `path` and `password` must be valid, null-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn yak_open_encrypted(
    path: *const c_char,
    mode: c_int,
    password: *const c_char,
) -> *mut YakFile {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let password = match cstr_to_str(password) {
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
    match YakInner::open_encrypted(path, mode, password.as_bytes()) {
        Ok(inner) => Box::into_raw(Box::new(YakFile(inner))),
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Check whether the Yak file has encryption enabled.
/// Returns 1 if encrypted, 0 if not.
///
/// # Safety
/// `handle` must be a live `YakFile` pointer.
#[no_mangle]
pub unsafe extern "C" fn yak_is_encrypted(handle: *mut YakFile) -> c_int {
    let inner = unsafe { &(*handle).0 };
    if inner.is_encrypted() {
        1
    } else {
        0
    }
}

/// Close the Yak file, flushing any open stream handles.
/// Returns 0 on success, -1 on error. The handle is always consumed
/// regardless of the return value — callers must not reuse it.
///
/// # Safety
/// `handle` must be a valid pointer returned by `yak_create` or `yak_open`,
/// and must not have been closed already.
#[no_mangle]
pub unsafe extern "C" fn yak_close(handle: *mut YakFile) -> c_int {
    if handle.is_null() {
        return 0;
    }
    let yak_file = unsafe { Box::from_raw(handle) };
    match yak_file.0.close() {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Yak file properties
// ---------------------------------------------------------------------------

/// Get the compressed block size shift. Returns 0 if compression is not configured.
///
/// # Safety
/// `handle` must be a live `YakFile` pointer.
#[no_mangle]
pub unsafe extern "C" fn yak_compressed_block_size_shift(handle: *mut YakFile) -> u8 {
    let inner = unsafe { &(*handle).0 };
    inner.compressed_block_size_shift()
}

// ---------------------------------------------------------------------------
// Directory operations
// ---------------------------------------------------------------------------

/// # Safety
/// `handle` must be a live `YakFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn yak_mkdir(handle: *mut YakFile, path: *const c_char) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match inner.mkdir(path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// # Safety
/// `handle` must be a live `YakFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn yak_rmdir(handle: *mut YakFile, path: *const c_char) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match inner.rmdir(path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// # Safety
/// `handle` must be a live `YakFile` pointer. Both path arguments must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn yak_rename_dir(
    handle: *mut YakFile,
    old_path: *const c_char,
    new_path: *const c_char,
) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let old_path = match cstr_to_str(old_path) {
        Some(s) => s,
        None => return -1,
    };
    let new_path = match cstr_to_str(new_path) {
        Some(s) => s,
        None => return -1,
    };
    match inner.rename_dir(old_path, new_path) {
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
/// `handle` must be a live `YakFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn yak_list(handle: *mut YakFile, path: *const c_char) -> *mut YakList {
    let inner = unsafe { &(*handle).0 };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match inner.list(path) {
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
            Box::into_raw(Box::new(YakList {
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
/// `list` must be a valid pointer returned by `yak_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn yak_list_count(list: *mut YakList) -> c_int {
    let list = unsafe { &*list };
    list.entries.len() as c_int
}

/// # Safety
/// `list` must be a valid pointer returned by `yak_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn yak_list_entry_name(list: *mut YakList, index: c_int) -> *const c_char {
    let list = unsafe { &*list };
    if index < 0 || (index as usize) >= list.entries.len() {
        return std::ptr::null();
    }
    list.entries[index as usize].0.as_ptr()
}

/// # Safety
/// `list` must be a valid pointer returned by `yak_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn yak_list_entry_type(list: *mut YakList, index: c_int) -> c_int {
    let list = unsafe { &*list };
    if index < 0 || (index as usize) >= list.entries.len() {
        return -1;
    }
    list.entries[index as usize].1
}

/// # Safety
/// `list` must be a valid pointer returned by `yak_list` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn yak_list_free(list: *mut YakList) {
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
/// Use yak_verify_count/yak_verify_issue to read issues, then yak_verify_free to release.
///
/// # Safety
/// `handle` must be a live `YakFile` pointer.
#[no_mangle]
pub unsafe extern "C" fn yak_verify(handle: *mut YakFile) -> *mut YakVerifyResult {
    let inner = unsafe { &(*handle).0 };
    match inner.verify() {
        Ok(issues) => {
            let c_issues: Vec<CString> = issues
                .into_iter()
                .map(|s| CString::new(s).unwrap_or_else(|_| CString::new("?").unwrap()))
                .collect();
            Box::into_raw(Box::new(YakVerifyResult { issues: c_issues }))
        }
        Err(e) => {
            set_last_error(&e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `result` must be a valid pointer returned by `yak_verify` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn yak_verify_count(result: *mut YakVerifyResult) -> c_int {
    let result = unsafe { &*result };
    result.issues.len() as c_int
}

/// # Safety
/// `result` must be a valid pointer returned by `yak_verify` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn yak_verify_issue(
    result: *mut YakVerifyResult,
    index: c_int,
) -> *const c_char {
    let result = unsafe { &*result };
    if index < 0 || (index as usize) >= result.issues.len() {
        return std::ptr::null();
    }
    result.issues[index as usize].as_ptr()
}

/// # Safety
/// `result` must be a valid pointer returned by `yak_verify` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn yak_verify_free(result: *mut YakVerifyResult) {
    if !result.is_null() {
        unsafe {
            drop(Box::from_raw(result));
        }
    }
}

// ---------------------------------------------------------------------------
// Compaction
// ---------------------------------------------------------------------------

/// Optimize a Yak file by rewriting it without free blocks.
/// Removes free blocks and rewrites streams contiguously for maximum locality.
/// Returns the number of bytes saved, or -1 on error.
/// For unencrypted files, pass NULL for `password`.
/// For encrypted files, `password` must be a valid null-terminated UTF-8 string.
///
/// # Safety
/// `path` must be a valid, null-terminated UTF-8 string.
/// `password`, if non-null, must be a valid, null-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn yak_optimize(path: *const c_char, password: *const c_char) -> i64 {
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    let pw = if password.is_null() {
        None
    } else {
        match cstr_to_str(password) {
            Some(s) => Some(s.as_bytes()),
            None => return -1,
        }
    };
    match YakInner::optimize(path, pw) {
        Ok(saved) => saved as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Stream lifecycle
// ---------------------------------------------------------------------------

/// Create a new stream and open it for writing. Returns handle ID or -1.
/// `compressed`: non-zero to create a compressed stream (requires cbss > 0).
///
/// # Safety
/// `handle` must be a live `YakFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn yak_create_stream(
    handle: *mut YakFile,
    path: *const c_char,
    compressed: i32,
) -> i64 {
    let inner = unsafe { &(*handle).0 };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match inner.create_stream(path, compressed != 0) {
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
/// `handle` must be a live `YakFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn yak_open_stream(
    handle: *mut YakFile,
    path: *const c_char,
    mode: c_int,
) -> i64 {
    let inner = unsafe { &(*handle).0 };
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
    match inner.open_stream(path, mode) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_close_stream(handle: *mut YakFile, stream: i64) -> c_int {
    let inner = unsafe { &(*handle).0 };
    match inner.close_stream(StreamHandle::from_id(stream as u64)) {
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
/// `handle` must be a live `YakFile` pointer. `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn yak_delete_stream(handle: *mut YakFile, path: *const c_char) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let path = match cstr_to_str(path) {
        Some(s) => s,
        None => return -1,
    };
    match inner.delete_stream(path) {
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
/// `handle` must be a live `YakFile` pointer. Both path arguments must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn yak_rename_stream(
    handle: *mut YakFile,
    old_path: *const c_char,
    new_path: *const c_char,
) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let old_path = match cstr_to_str(old_path) {
        Some(s) => s,
        None => return -1,
    };
    let new_path = match cstr_to_str(new_path) {
        Some(s) => s,
        None => return -1,
    };
    match inner.rename_stream(old_path, new_path) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
/// `buf` must point to at least `len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_read(
    handle: *mut YakFile,
    stream: i64,
    buf: *mut u8,
    len: u64,
) -> i64 {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    let buf = unsafe { std::slice::from_raw_parts_mut(buf, len as usize) };
    match inner.read(&sh, buf) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
/// `buf` must point to at least `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_write(
    handle: *mut YakFile,
    stream: i64,
    buf: *const u8,
    len: u64,
) -> i64 {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    let buf = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    match inner.write(&sh, buf) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_seek(handle: *mut YakFile, stream: i64, pos: u64) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match inner.seek(&sh, pos) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_tell(handle: *mut YakFile, stream: i64) -> i64 {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match inner.tell(&sh) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_length(handle: *mut YakFile, stream: i64) -> i64 {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match inner.stream_length(&sh) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_truncate(
    handle: *mut YakFile,
    stream: i64,
    new_len: u64,
) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match inner.truncate(&sh, new_len) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_reserve(
    handle: *mut YakFile,
    stream: i64,
    n_bytes: u64,
) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match inner.reserve(&sh, n_bytes) {
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
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_reserved(handle: *mut YakFile, stream: i64) -> i64 {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match inner.stream_reserved(&sh) {
        Ok(r) => r as i64,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}

/// Check whether a stream is compressed. Returns 1 if compressed, 0 if not, -1 on error.
///
/// # Safety
/// `handle` must be a live `YakFile` pointer. `stream` must be an open stream handle ID.
#[no_mangle]
pub unsafe extern "C" fn yak_stream_is_compressed(handle: *mut YakFile, stream: i64) -> c_int {
    let inner = unsafe { &(*handle).0 };
    let sh = StreamHandle::from_id(stream as u64);
    match inner.is_stream_compressed(&sh) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(e) => {
            set_last_error(&e.to_string());
            -1
        }
    }
}
