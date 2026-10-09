//! Filesystem helpers for private outputs that reject symlinks in every path
//! component, not only at the final filename.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

fn cstring(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn open_child_dir(parent: &File, name: &[u8], create: bool) -> io::Result<File> {
    let name = cstring(name)?;
    if create {
        let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if rc != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn open_dir(path: &Path, create: bool) -> io::Result<File> {
    let normalized = normalize_system_alias(path);
    let path = normalized.as_path();
    let mut dir = if path.is_absolute() { File::open("/")? } else { File::open(".")? };
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => dir = open_child_dir(&dir, b"..", false)?,
            Component::Normal(name) => dir = open_child_dir(&dir, name.as_bytes(), create)?,
            Component::Prefix(_) => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsupported path prefix"));
            }
        }
    }
    Ok(dir)
}

fn normalize_system_alias(path: &Path) -> PathBuf {
    // These are fixed macOS filesystem aliases, not user-controlled path
    // components. Resolving only the alias keeps later components protected.
    #[cfg(target_os = "macos")]
    {
        if let Ok(rest) = path.strip_prefix("/var") {
            return Path::new("/private/var").join(rest);
        }
        if let Ok(rest) = path.strip_prefix("/tmp") {
            return Path::new("/private/tmp").join(rest);
        }
    }
    path.to_path_buf()
}

fn final_name(path: &Path) -> io::Result<&[u8]> {
    path.file_name()
        .map(|name| name.as_bytes())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no final component"))
}

/// Create a directory tree with mode 0700 for new components. Every existing
/// component must be a real directory, never a symlink.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    open_dir(path, true).map(drop)
}

/// Atomically create the final directory. Parent directories are created
/// privately and symlinks in the path are rejected.
pub fn create_dir_exclusive(path: &Path) -> io::Result<()> {
    let parent = open_dir(path.parent().unwrap_or_else(|| Path::new(".")), true)?;
    let name = cstring(final_name(path)?)?;
    let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Create or replace an owner-only file without following a symlink in any
/// component of its path.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = open_dir(path.parent().unwrap_or_else(|| Path::new(".")), true)?;
    let name = cstring(final_name(path)?)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(io::Error::last_os_error());
    }
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn private_writes_reject_final_and_intermediate_symlinks() {
        let root = tempfile::tempdir().unwrap();
        // macOS exposes /var as a symlink to /private/var, so canonicalize the
        // trusted temporary root before testing our own path components.
        let root_path = root.path().canonicalize().unwrap();
        let safe = root_path.join("safe/nested/report.json");
        write_private(&safe, b"ok").unwrap();
        assert_eq!(std::fs::read(&safe).unwrap(), b"ok");
        assert_eq!(std::fs::metadata(&safe).unwrap().permissions().mode() & 0o777, 0o600);

        let outside = root_path.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let intermediate = root_path.join("redirect");
        symlink(&outside, &intermediate).unwrap();
        assert!(write_private(&intermediate.join("escaped.json"), b"no").is_err());
        assert!(!outside.join("escaped.json").exists());

        let target = root_path.join("target");
        std::fs::write(&target, b"private").unwrap();
        let final_link = root_path.join("final-link");
        symlink(&target, &final_link).unwrap();
        assert!(write_private(&final_link, b"changed").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"private");
    }

    #[test]
    fn exclusive_directory_creation_is_atomic() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("a/b");
        create_dir_exclusive(&path).unwrap();
        assert!(path.is_dir());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(create_dir_exclusive(&path).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    }
}
