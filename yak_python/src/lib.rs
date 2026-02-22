use ::yak::{
    CreateOptions, EntryType as RustEntryType, OpenMode as RustOpenMode, StreamHandle, YakDefault,
    YakError as RustYakError,
};
use pyo3::prelude::*;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

pyo3::create_exception!(libyak, YakError, pyo3::exceptions::PyException);

/// Convert a Rust `YakError` into a Python `libyak.YakError` exception.
fn to_py_err(e: RustYakError) -> PyErr {
    YakError::new_err(e.to_string())
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[pyclass(eq, eq_int, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenMode {
    /// Exposed to Python as `OpenMode.READ` (value 0).
    #[pyo3(name = "READ")]
    Read = 0,
    /// Exposed to Python as `OpenMode.WRITE` (value 1).
    #[pyo3(name = "WRITE")]
    Write = 1,
}

impl From<OpenMode> for RustOpenMode {
    fn from(m: OpenMode) -> Self {
        match m {
            OpenMode::Read => RustOpenMode::Read,
            OpenMode::Write => RustOpenMode::Write,
        }
    }
}

#[pyclass(eq, eq_int, from_py_object)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryType {
    /// Exposed to Python as `EntryType.STREAM` (value 0).
    #[pyo3(name = "STREAM")]
    Stream = 0,
    /// Exposed to Python as `EntryType.DIRECTORY` (value 1).
    #[pyo3(name = "DIRECTORY")]
    Directory = 1,
}

impl From<RustEntryType> for EntryType {
    fn from(e: RustEntryType) -> Self {
        match e {
            RustEntryType::Stream => EntryType::Stream,
            RustEntryType::Directory => EntryType::Directory,
        }
    }
}

// ---------------------------------------------------------------------------
// DirEntry
// ---------------------------------------------------------------------------

#[pyclass]
struct DirEntry {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    entry_type: EntryType,
}

#[pymethods]
impl DirEntry {
    fn __repr__(&self) -> String {
        let type_name = match self.entry_type {
            EntryType::Stream => "STREAM",
            EntryType::Directory => "DIRECTORY",
        };
        format!("DirEntry('{}', {})", self.name, type_name)
    }
}

// ---------------------------------------------------------------------------
// Yak
// ---------------------------------------------------------------------------

#[pyclass]
struct Yak {
    /// Arc-wrapped so `close()` can use `&self` (no exclusive PyCell borrow),
    /// avoiding "Already borrowed" when threads are still mid-call.
    inner: Mutex<Option<Arc<YakDefault>>>,
}

impl Yak {
    /// Clone the inner Arc, returning an error if already closed.
    /// The Mutex is held only for the duration of Arc::clone (nanoseconds).
    fn get(&self) -> PyResult<Arc<YakDefault>> {
        let guard = self.inner.lock().unwrap();
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| YakError::new_err("Yak file has been closed"))
    }
}

#[pymethods]
impl Yak {
    // -- Lifecycle -----------------------------------------------------------

    /// Create a new Yak file. Compression support is always enabled.
    /// If `compressed_block_size_shift` is None, uses the default (15 = 32 KB).
    /// If `password` is provided, the file is encrypted with AES-XTS.
    #[staticmethod]
    #[pyo3(signature = (path, block_index_width=4, block_size_shift=12, compressed_block_size_shift=None, password=None))]
    fn create(
        py: Python<'_>,
        path: &str,
        block_index_width: u8,
        block_size_shift: u8,
        compressed_block_size_shift: Option<u8>,
        password: Option<&[u8]>,
    ) -> PyResult<Self> {
        let path = path.to_owned();
        let pw: Option<Vec<u8>> = password.map(|p| p.to_vec());
        let defaults = CreateOptions::default();
        let cbss = compressed_block_size_shift.unwrap_or(defaults.compressed_block_size_shift);
        let inner = py
            .detach(move || {
                YakDefault::create(
                    &path,
                    CreateOptions {
                        block_index_width,
                        block_size_shift,
                        compressed_block_size_shift: cbss,
                        password: pw.as_deref(),
                    },
                )
            })
            .map_err(to_py_err)?;
        Ok(Yak {
            inner: Mutex::new(Some(Arc::new(inner))),
        })
    }

    /// Open an existing Yak file.
    /// If the file is encrypted, `password` must be provided.
    #[staticmethod]
    #[pyo3(signature = (path, mode, password=None))]
    fn open(py: Python<'_>, path: &str, mode: OpenMode, password: Option<&[u8]>) -> PyResult<Self> {
        let path = path.to_owned();
        let rust_mode = RustOpenMode::from(mode);
        let pw: Option<Vec<u8>> = password.map(|p| p.to_vec());
        let inner = py
            .detach(move || match pw.as_deref() {
                Some(pw) => YakDefault::open_encrypted(&path, rust_mode, pw),
                None => YakDefault::open(&path, rust_mode),
            })
            .map_err(to_py_err)?;
        Ok(Yak {
            inner: Mutex::new(Some(Arc::new(inner))),
        })
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let taken = self.inner.lock().unwrap().take();
        match taken {
            Some(arc) => py
                .detach(move || match Arc::try_unwrap(arc) {
                    Ok(yak) => yak.close(),
                    Err(_) => {
                        // Other threads still hold references — YakDefault
                        // will be cleaned up when the last Arc is dropped.
                        Ok(())
                    }
                })
                .map_err(to_py_err),
            None => Ok(()),
        }
    }

    // -- Directory operations ------------------------------------------------

    fn mkdir(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let inner = self.get()?;
        let path = path.to_owned();
        py.detach(move || inner.mkdir(&path)).map_err(to_py_err)
    }

    fn rmdir(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let inner = self.get()?;
        let path = path.to_owned();
        py.detach(move || inner.rmdir(&path)).map_err(to_py_err)
    }

    #[pyo3(signature = (path=""))]
    fn list(&self, py: Python<'_>, path: &str) -> PyResult<Vec<DirEntry>> {
        let inner = self.get()?;
        let path = path.to_owned();
        let entries = py.detach(move || inner.list(&path)).map_err(to_py_err)?;
        Ok(entries
            .into_iter()
            .map(|e| DirEntry {
                name: e.name,
                entry_type: e.entry_type.into(),
            })
            .collect())
    }

    fn rename_dir(&self, py: Python<'_>, old_path: &str, new_path: &str) -> PyResult<()> {
        let inner = self.get()?;
        let old = old_path.to_owned();
        let new = new_path.to_owned();
        py.detach(move || inner.rename_dir(&old, &new))
            .map_err(to_py_err)
    }

    // -- Stream lifecycle ----------------------------------------------------

    #[pyo3(signature = (path, compressed=false))]
    fn create_stream(&self, py: Python<'_>, path: &str, compressed: bool) -> PyResult<u64> {
        let inner = self.get()?;
        let path = path.to_owned();
        let handle = py
            .detach(move || inner.create_stream(&path, compressed))
            .map_err(to_py_err)?;
        Ok(handle.id())
    }

    fn open_stream(&self, py: Python<'_>, path: &str, mode: OpenMode) -> PyResult<u64> {
        let inner = self.get()?;
        let path = path.to_owned();
        let rust_mode = RustOpenMode::from(mode);
        let handle = py
            .detach(move || inner.open_stream(&path, rust_mode))
            .map_err(to_py_err)?;
        Ok(handle.id())
    }

    fn close_stream(&self, py: Python<'_>, handle: u64) -> PyResult<()> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(move || inner.close_stream(sh)).map_err(to_py_err)
    }

    fn delete_stream(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let inner = self.get()?;
        let path = path.to_owned();
        py.detach(move || inner.delete_stream(&path))
            .map_err(to_py_err)
    }

    fn rename_stream(&self, py: Python<'_>, old_path: &str, new_path: &str) -> PyResult<()> {
        let inner = self.get()?;
        let old = old_path.to_owned();
        let new = new_path.to_owned();
        py.detach(move || inner.rename_stream(&old, &new))
            .map_err(to_py_err)
    }

    // -- Stream I/O ----------------------------------------------------------

    fn read(&self, py: Python<'_>, handle: u64, length: usize) -> PyResult<Vec<u8>> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        let mut buf = vec![0u8; length];
        let n = py.detach(|| inner.read(&sh, &mut buf)).map_err(to_py_err)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn write(&self, py: Python<'_>, handle: u64, data: &[u8]) -> PyResult<usize> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.write(&sh, data)).map_err(to_py_err)
    }

    fn seek(&self, py: Python<'_>, handle: u64, pos: u64) -> PyResult<()> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.seek(&sh, pos)).map_err(to_py_err)
    }

    fn tell(&self, py: Python<'_>, handle: u64) -> PyResult<u64> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.tell(&sh)).map_err(to_py_err)
    }

    fn stream_length(&self, py: Python<'_>, handle: u64) -> PyResult<u64> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.stream_length(&sh)).map_err(to_py_err)
    }

    fn truncate(&self, py: Python<'_>, handle: u64, new_len: u64) -> PyResult<()> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.truncate(&sh, new_len))
            .map_err(to_py_err)
    }

    fn reserve(&self, py: Python<'_>, handle: u64, n_bytes: u64) -> PyResult<()> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.reserve(&sh, n_bytes)).map_err(to_py_err)
    }

    fn stream_reserved(&self, py: Python<'_>, handle: u64) -> PyResult<u64> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.stream_reserved(&sh)).map_err(to_py_err)
    }

    fn is_stream_compressed(&self, py: Python<'_>, handle: u64) -> PyResult<bool> {
        let inner = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| inner.is_stream_compressed(&sh))
            .map_err(to_py_err)
    }

    // -- Info ----------------------------------------------------------------

    fn is_encrypted(&self) -> PyResult<bool> {
        Ok(self.get()?.is_encrypted())
    }

    // -- Verification --------------------------------------------------------

    fn verify(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let inner = self.get()?;
        py.detach(|| inner.verify()).map_err(to_py_err)
    }

    // -- Compaction ----------------------------------------------------------

    /// Optimize a Yak file by rewriting it without free blocks.
    /// Removes free blocks and rewrites streams contiguously for maximum locality.
    /// Returns the number of bytes reclaimed.
    /// For encrypted files, the password must be provided.
    #[staticmethod]
    #[pyo3(signature = (path, password=None))]
    fn optimize(py: Python<'_>, path: &str, password: Option<&[u8]>) -> PyResult<u64> {
        let path = path.to_owned();
        let pw: Option<Vec<u8>> = password.map(|p| p.to_vec());
        py.detach(move || YakDefault::optimize(&path, pw.as_deref()))
            .map_err(to_py_err)
    }
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

#[pymodule]
fn libyak(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Yak>()?;
    m.add_class::<OpenMode>()?;
    m.add_class::<EntryType>()?;
    m.add_class::<DirEntry>()?;
    m.add("YakError", m.py().get_type::<YakError>())?;
    Ok(())
}
