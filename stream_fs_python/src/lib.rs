use pyo3::prelude::*;
use stream_fs::{
    EntryType as RustEntryType, OpenMode as RustOpenMode, SfsDefault, SfsError as RustSfsError,
    StreamHandle,
};

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

pyo3::create_exception!(sfs, SfsError, pyo3::exceptions::PyException);

/// Convert a Rust `SfsError` into a Python `sfs.SfsError` exception.
fn to_py_err(e: RustSfsError) -> PyErr {
    SfsError::new_err(e.to_string())
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
// Sfs
// ---------------------------------------------------------------------------

#[pyclass]
struct Sfs {
    inner: Option<SfsDefault>,
}

impl Sfs {
    /// Borrow the inner SFS handle, returning an error if already closed.
    fn get(&self) -> PyResult<&SfsDefault> {
        self.inner
            .as_ref()
            .ok_or_else(|| SfsError::new_err("SFS file has been closed"))
    }
}

#[pymethods]
impl Sfs {
    // -- Lifecycle -----------------------------------------------------------

    #[staticmethod]
    #[pyo3(signature = (path, block_index_width=4, block_size_shift=12))]
    fn create(
        py: Python<'_>,
        path: &str,
        block_index_width: u8,
        block_size_shift: u8,
    ) -> PyResult<Self> {
        let path = path.to_owned();
        let sfs = py
            .detach(move || SfsDefault::create(&path, block_index_width, block_size_shift))
            .map_err(to_py_err)?;
        Ok(Sfs { inner: Some(sfs) })
    }

    #[staticmethod]
    fn open(py: Python<'_>, path: &str, mode: OpenMode) -> PyResult<Self> {
        let path = path.to_owned();
        let rust_mode = RustOpenMode::from(mode);
        let sfs = py
            .detach(move || SfsDefault::open(&path, rust_mode))
            .map_err(to_py_err)?;
        Ok(Sfs { inner: Some(sfs) })
    }

    fn close(&mut self, py: Python<'_>) -> PyResult<()> {
        match self.inner.take() {
            Some(sfs) => py.detach(move || sfs.close()).map_err(to_py_err),
            None => Ok(()),
        }
    }

    // -- Directory operations ------------------------------------------------

    fn mkdir(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let sfs = self.get()?;
        let path = path.to_owned();
        py.detach(move || sfs.mkdir(&path)).map_err(to_py_err)
    }

    fn rmdir(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let sfs = self.get()?;
        let path = path.to_owned();
        py.detach(move || sfs.rmdir(&path)).map_err(to_py_err)
    }

    #[pyo3(signature = (path=""))]
    fn list(&self, py: Python<'_>, path: &str) -> PyResult<Vec<DirEntry>> {
        let sfs = self.get()?;
        let path = path.to_owned();
        let entries = py.detach(move || sfs.list(&path)).map_err(to_py_err)?;
        Ok(entries
            .into_iter()
            .map(|e| DirEntry {
                name: e.name,
                entry_type: e.entry_type.into(),
            })
            .collect())
    }

    fn rename_dir(&self, py: Python<'_>, old_path: &str, new_path: &str) -> PyResult<()> {
        let sfs = self.get()?;
        let old = old_path.to_owned();
        let new = new_path.to_owned();
        py.detach(move || sfs.rename_dir(&old, &new))
            .map_err(to_py_err)
    }

    // -- Stream lifecycle ----------------------------------------------------

    fn create_stream(&self, py: Python<'_>, path: &str) -> PyResult<u64> {
        let sfs = self.get()?;
        let path = path.to_owned();
        let handle = py
            .detach(move || sfs.create_stream(&path))
            .map_err(to_py_err)?;
        Ok(handle.id())
    }

    fn open_stream(&self, py: Python<'_>, path: &str, mode: OpenMode) -> PyResult<u64> {
        let sfs = self.get()?;
        let path = path.to_owned();
        let rust_mode = RustOpenMode::from(mode);
        let handle = py
            .detach(move || sfs.open_stream(&path, rust_mode))
            .map_err(to_py_err)?;
        Ok(handle.id())
    }

    fn close_stream(&self, py: Python<'_>, handle: u64) -> PyResult<()> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(move || sfs.close_stream(sh)).map_err(to_py_err)
    }

    fn delete_stream(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let sfs = self.get()?;
        let path = path.to_owned();
        py.detach(move || sfs.delete_stream(&path))
            .map_err(to_py_err)
    }

    fn rename_stream(&self, py: Python<'_>, old_path: &str, new_path: &str) -> PyResult<()> {
        let sfs = self.get()?;
        let old = old_path.to_owned();
        let new = new_path.to_owned();
        py.detach(move || sfs.rename_stream(&old, &new))
            .map_err(to_py_err)
    }

    // -- Stream I/O ----------------------------------------------------------

    fn read(&self, py: Python<'_>, handle: u64, length: usize) -> PyResult<Vec<u8>> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        let mut buf = vec![0u8; length];
        let n = py.detach(|| sfs.read(&sh, &mut buf)).map_err(to_py_err)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn write(&self, py: Python<'_>, handle: u64, data: &[u8]) -> PyResult<usize> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| sfs.write(&sh, data)).map_err(to_py_err)
    }

    fn seek(&self, py: Python<'_>, handle: u64, pos: u64) -> PyResult<()> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| sfs.seek(&sh, pos)).map_err(to_py_err)
    }

    fn tell(&self, py: Python<'_>, handle: u64) -> PyResult<u64> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| sfs.tell(&sh)).map_err(to_py_err)
    }

    fn stream_length(&self, py: Python<'_>, handle: u64) -> PyResult<u64> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| sfs.stream_length(&sh)).map_err(to_py_err)
    }

    fn truncate(&self, py: Python<'_>, handle: u64, new_len: u64) -> PyResult<()> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| sfs.truncate(&sh, new_len)).map_err(to_py_err)
    }

    fn reserve(&self, py: Python<'_>, handle: u64, n_bytes: u64) -> PyResult<()> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| sfs.reserve(&sh, n_bytes)).map_err(to_py_err)
    }

    fn stream_reserved(&self, py: Python<'_>, handle: u64) -> PyResult<u64> {
        let sfs = self.get()?;
        let sh = StreamHandle::from_id(handle);
        py.detach(|| sfs.stream_reserved(&sh)).map_err(to_py_err)
    }

    // -- Verification --------------------------------------------------------

    fn verify(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let sfs = self.get()?;
        py.detach(|| sfs.verify()).map_err(to_py_err)
    }
}

// ---------------------------------------------------------------------------
// Module-level functions
// ---------------------------------------------------------------------------

/// Returns a greeting string from the SFS library.
#[pyfunction]
fn hello() -> String {
    stream_fs::hello()
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

#[pymodule]
fn sfs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Sfs>()?;
    m.add_class::<OpenMode>()?;
    m.add_class::<EntryType>()?;
    m.add_class::<DirEntry>()?;
    m.add("SfsError", m.py().get_type::<SfsError>())?;
    m.add_function(wrap_pyfunction!(hello, m)?)?;
    Ok(())
}
