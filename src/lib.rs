#![feature(convert)]
#![cfg_attr(windows, feature(fs_ext))]
//! Securely create and manage temporary files. Temporary files created by this create are
//! automatically deleted.
//!
//! This crate provides two temporary file variants: `TempFile` and `NamedTempFile`. When choosing
//! between the variants, prefer `TempFile` unless you either need to know the file's path or to be
//! able to persist it.
//!
//! # Differences
//!
//! ## Resource Leaking
//!
//! `TempFile` will (almost) never fail to cleanup temporary files but `NamedTempFile` will if its
//! destructor doesn't run. This is because `TempFile` relies on the OS to cleanup the underlying
//! file so the file while `NamedTempFile` relies on its destructor to do so.
//!
//! ## Security
//!
//! In the presence of pathological temporary file cleaner, relying on file paths is unsafe because
//! a temporary file cleaner could delete the temporary file which an attacker could then replace.
//!
//! `TempFile` doesn't rely on file paths so this isn't an issue. However, `NamedTempFile` does
//! rely on file paths.
//!
extern crate libc;
extern crate rand;

use std::io::{self, Read, Write, Seek, SeekFrom};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::error;
use std::fmt;
use std::env;

mod imp;
mod util;

/// An unnamed temporary file.
///
/// This variant is secure/reliable in the presence of a pathological temporary file cleaner.
///
/// Deletion:
///
/// Linux >= 3.11: The temporary file is never linked into the filesystem so it can't be leaked.
///
/// Other *nix: The temporary file is immediately unlinked on create. The OS will delete it when
/// the last open copy of it is closed (the last TempFile reference to it is dropped).
///
/// Windows: The temporary file is marked DeleteOnClose and, again, will be deleted when the last
/// open copy of it is closed. Unlike *nix operating systems, the file is not immediately unlinked
/// from the filesystem.
pub struct TempFile(File);

impl TempFile {
    /// Create a new temporary file.
    #[inline]
    pub fn new() -> io::Result<TempFile> {
        Self::new_in(&env::temp_dir())
    }

    /// Create a new temporary file in the specified directory.
    #[inline]
    pub fn new_in<P: AsRef<Path>>(dir: P) -> io::Result<TempFile> {
        imp::create(dir.as_ref()).map(|f| TempFile(f))
    }

    /// Create a new temporary file and open it `count` times returning `count` independent
    /// references to the same file.
    ///
    /// This can be useful if you want multiple seek positions in the same temporary file.
    /// Additionally, this function guarantees that all of the returned temporary file objects
    /// refer to the same underlying temporary file even in the presence of a pathological
    /// temporary file cleaner.
    #[inline]
    pub fn shared(count: usize) -> io::Result<Vec<TempFile>> {
        Self::shared_in(&env::temp_dir(), count)
    }

    /// Same as `shared` but creates the file in the specified directory.
    #[inline]
    pub fn shared_in<P: AsRef<Path>>(dir: P, count: usize) -> io::Result<Vec<TempFile>> {
        imp::create_shared(dir.as_ref(), count).map(|files| {
            files.into_iter().map(|f|TempFile(f)).collect()
        })
    }


    /// Number of bytes in the file.
    #[inline]
    pub fn len(&self) -> io::Result<u64> {
        self.0.metadata().map(|m| m.len())
    }

    /// Truncate the file to `size` bytes.
    #[inline(always)]
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        self.0.set_len(size)
    }

    /// Re-open the temporary file. The returned TempFile will refer to the same underlying
    /// temporary file but will have an independent offset.
    ///
    /// This method is only available on windows and Linux, not FreeBSD/MacOS. Unfortunately, it is
    /// impossible to reliably implement this method on those operating systems.
    ///
    /// If you need your code to be cross-platform, please use `shared`/`shared_in` defined above.
    ///
    /// **Unstable**: This is platform specific and may go away in the future.
    #[cfg(any(windows, target_os = "linux"))]
    #[inline]
    pub fn reopen(&self) -> io::Result<TempFile> {
        imp::reopen(&self.0).map(|f|TempFile(f))
    }
}

impl Read for TempFile {
    #[inline(always)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for TempFile {
    #[inline(always)]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    #[inline(always)]
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Seek for TempFile {
    #[inline(always)]
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for TempFile {
    #[inline(always)]
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawHandle for TempFile {
    #[inline(always)]
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0.as_raw_handle()
    }
}

/// A named temporary file.
///
/// This variant is *NOT* secure/reliable in the presence of a pathological temporary file cleaner.
///
/// NamedTempFiles are deleted on drop. As rust doesn't guarantee that a struct will ever be
/// dropped, these temporary files will not be deleted on abort, resource leak, early exit, etc.
///
/// Please use TempFile unless you absolutely need a named file.
///
pub struct NamedTempFile(Option<NamedTempFileInner>);

struct NamedTempFileInner {
    file: File,
    path: PathBuf,
}

impl fmt::Debug for NamedTempFile {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "NamedTempFile({:?})", self.0.as_ref().unwrap().path)
    }
}

/// Error returned when persisting a temporary file fails
#[derive(Debug)]
pub struct PersistError {
    /// The underlying IO error.
    pub error: io::Error,
    /// The temporary file that couldn't be persisted.
    pub file: NamedTempFile,
}

impl From<PersistError> for io::Error {
    #[inline]
    fn from(error: PersistError) -> io::Error {
        error.error
    }
}

impl fmt::Display for PersistError {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "failed to persist temporary file: {}", self.error)
    }
}

impl error::Error for PersistError {
    #[inline]
    fn description(&self) -> &str {
        "failed to persist temporary file"
    }
    #[inline]
    fn cause(&self) -> Option<&error::Error> {
        Some(&self.error)
    }
}

impl NamedTempFile {
    #[inline]
    fn inner(&self) -> &NamedTempFileInner {
        self.0.as_ref().unwrap()
    }

    #[inline]
    fn inner_mut(&mut self) -> &mut NamedTempFileInner {
        self.0.as_mut().unwrap()
    }

    /// Create a new temporary file.
    #[inline]
    pub fn new() -> io::Result<NamedTempFile> {
        Self::new_in(&env::temp_dir())
    }

    /// Create a new temporary file in the specified directory.
    #[inline]
    pub fn new_in<P: AsRef<Path>>(dir: P) -> io::Result<NamedTempFile> {
        loop {
            let path = dir.as_ref().join(&util::tmpname());
            return match imp::create_named(&path) {
                Ok(file) => Ok(NamedTempFile(Some(NamedTempFileInner { path: path, file: file, }))),
                Err(ref e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => Err(e),
            }
        }
    }

    /// Queries metadata about the underlying file.
    #[inline]
    pub fn metadata(&self) -> io::Result<fs::Metadata> {
        self.inner().file.metadata()
    }

    /// Truncate the file to `size` bytes.
    #[inline]
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        self.inner().file.set_len(size)
    }

    /// Get the temporary file's path.
    #[inline]
    pub fn path(&self) -> &Path {
        &self.inner().path
    }

    /// Close and remove the temporary file.
    ///
    /// Use this if you want to detect errors in deleting the file.
    #[inline]
    pub fn close(mut self) -> io::Result<()> {
        let NamedTempFileInner { path, file } = self.0.take().unwrap();
        drop(file);
        fs::remove_file(path)
    }

    /// Extract the path to the temporary file. Calling this will prevent the temporary file from
    /// being automatically deleted.
    #[inline]
    pub fn into_path(mut self) -> PathBuf {
        let NamedTempFileInner { path, .. } = self.0.take().unwrap();
        path
    }

    /// Persist the temporary file at the target path.
    ///
    /// If a file exists at the target path, persist will atomically replace it. If this method
    /// fails, it will return `self` in the resulting PersistError.
    ///
    /// Note: Temporary files cannot be persisted across filesystems.
    #[inline]
    pub fn persist<P: AsRef<Path>>(mut self, new_path: P) -> Result<File, PersistError> {
        match fs::rename(&self.inner().path, new_path) {
            Ok(_) => Ok(self.0.take().unwrap().file),
            Err(e) => Err(PersistError { file: self, error: e }),
        }
    }
}

impl Drop for NamedTempFile {
    #[inline]
    fn drop(&mut self) {
        if let Some(NamedTempFileInner { file, path }) = self.0.take() {
            drop(file);
            let _ = fs::remove_file(path);
        }
    }
}

impl Read for NamedTempFile {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner_mut().file.read(buf)
    }
}

impl Write for NamedTempFile {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner_mut().file.write(buf)
    }
    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.inner_mut().file.flush()
    }
}

impl Seek for NamedTempFile {
    #[inline]
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner_mut().file.seek(pos)
    }
}

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for NamedTempFile {
    #[inline]
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.inner().file.as_raw_fd()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawHandle for NamedTempFile {
    #[inline]
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.inner().file.as_raw_handle()
    }
}
