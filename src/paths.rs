use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Locations {
    pub state: PathBuf,
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}
impl Locations {
    pub fn resolve(override_dir: Option<&Path>) -> Result<Self> {
        if let Some(p) = override_dir {
            let p = crate::config::expand(p)?;
            return Ok(Self {
                state: p.join("state"),
                data: p.join("data"),
                config: p.join("config.yaml"),
                cache: p.join("cache"),
            });
        }
        let dirs = directories::ProjectDirs::from("", "", "rulescan")
            .context("user directories unavailable; use --state-dir")?;
        Ok(Self {
            state: dirs
                .state_dir()
                .unwrap_or(dirs.data_local_dir())
                .join("state"),
            data: dirs.data_local_dir().join("data"),
            config: dirs.config_dir().join("config.yaml"),
            cache: dirs.cache_dir().to_owned(),
        })
    }
    pub fn init(&mut self) -> Result<()> {
        for p in [
            &self.state,
            &self.data,
            &self.cache,
            &self.data.join("quarantine"),
        ] {
            private_dir(p)?;
        }
        self.state = fs::canonicalize(&self.state)?;
        self.data = fs::canonicalize(&self.data)?;
        self.cache = fs::canonicalize(&self.cache)?;
        Ok(())
    }
    pub fn db(&self) -> PathBuf {
        self.state.join("rulescan.sqlite3")
    }
    pub fn excluded(&self, path: &Path) -> bool {
        path.starts_with(&self.state)
            || path.starts_with(&self.data)
            || path.starts_with(&self.cache)
    }
    pub fn lock(&self) -> Result<File> {
        lock(&self.state.join("coordinator.lock"))
    }
}
pub fn private_dir(p: &Path) -> Result<()> {
    if !p.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(p)?;
    }
    let m = fs::symlink_metadata(p)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "private directory cannot be a link"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Ownership is checked before tightening permissions; never chmod another user's directory.
        ensure!(
            m.uid() == unsafe { libc::geteuid() },
            "private directory has a different owner"
        );
        ensure!(
            m.mode() & 0o077 == 0,
            "private directory is accessible to other users; restrict its permissions"
        );
    }
    Ok(())
}
pub fn private_file(path: &Path, create_new: bool) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true);
    if create_new {
        o.create_new(true);
    } else {
        o.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let f = o.open(path)?;
    let m = f.metadata()?;
    ensure!(m.is_file(), "state entry must be a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0 && m.nlink() == 1,
            "state file has unsafe ownership, permissions, or links"
        );
    }
    Ok(f)
}
pub fn lock(path: &Path) -> Result<File> {
    let f = private_file(path, false)?;
    f.try_lock()
        .context("another RuleScan operation is active; retry after it finishes")?;
    Ok(f)
}

/// Open a directory without resolving any symlink component. Directory handles
/// anchor later operations even when an ancestor is concurrently renamed.
#[cfg(unix)]
pub fn open_dir(path: &Path) -> Result<File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        },
        path::Component,
    };
    ensure!(path.is_absolute(), "directory must be absolute");
    let mut dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => {
                let name = CString::new(name.as_bytes())?;
                let fd = unsafe {
                    libc::openat(
                        dir.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                dir = unsafe { File::from_raw_fd(fd) };
            }
            _ => anyhow::bail!("non-normal directory component"),
        }
    }
    Ok(dir)
}
pub fn open_regular(path: &Path) -> Result<File> {
    #[cfg(unix)]
    {
        use std::{
            ffi::CString,
            os::{
                fd::{AsRawFd, FromRawFd},
                unix::ffi::OsStrExt,
            },
        };
        let parent = open_dir(path.parent().context("missing parent")?)?;
        let name = CString::new(path.file_name().context("missing filename")?.as_bytes())?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        ensure!(file.metadata()?.is_file(), "not a regular file");
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(0x00200000)
            .open(path)?;
        ensure!(
            file.metadata()?.is_file() && !is_link(&file.metadata()?),
            "reparse points are read-only"
        );
        // Ancestor reparse-point resolution is outside the native validation gate.
        let mut current = path.parent();
        while let Some(p) = current {
            ensure!(
                !is_link(&fs::symlink_metadata(p)?),
                "reparse ancestor is unsupported"
            );
            current = p.parent();
        }
        Ok(file)
    }
}
pub fn is_link(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0
    }
    #[cfg(unix)]
    {
        m.file_type().is_symlink()
    }
}
pub fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        open_dir(path)?.sync_all()?;
    }
    #[cfg(windows)]
    {
        let _ = path;
    }
    Ok(())
}
