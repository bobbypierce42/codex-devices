use crate::{Error, Result};
use std::fs::File;
use std::path::Path;

#[derive(Clone)]
pub(crate) struct Directory {
    #[cfg(unix)]
    file: std::sync::Arc<File>,
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::MAX_BYTES;
    use rustix::fs::{self, Dir, Mode, OFlags};
    use rustix::io::Errno;
    use std::ffi::OsStr;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;
    use std::sync::Arc;

    fn system<T>(result: rustix::io::Result<T>) -> Result<T> {
        result.map_err(|error| match error {
            Errno::LOOP | Errno::NOTDIR => {
                Error::refused("Symlinks and non-directory path components are not supported.")
            }
            _ => std::io::Error::from(error).into(),
        })
    }

    fn member(name: &OsStr) -> Result<()> {
        let mut parts = Path::new(name).components();
        if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
            return Err(Error::refused(
                "Artifact member must be one local filename.",
            ));
        }
        Ok(())
    }

    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    fn private(file: &File, directory: bool) -> Result<()> {
        let metadata = file.metadata()?;
        if metadata.is_dir() != directory
            || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(Error::refused(
                "Artifacts require owned private directories and private regular files without hard links.",
            ));
        }
        Ok(())
    }

    impl Directory {
        fn walk(path: &Path) -> Result<Self> {
            if path.as_os_str().is_empty() {
                return Err(Error::refused("Use an explicit local directory."));
            }
            super::super::local_path(path)?;
            let start = if path.is_absolute() { "/" } else { "." };
            let mut file = File::from(system(fs::open(start, directory_flags(), Mode::empty()))?);
            for component in path.components() {
                if let Component::Normal(name) = component {
                    file = File::from(system(fs::openat(
                        &file,
                        name,
                        directory_flags(),
                        Mode::empty(),
                    ))?);
                }
            }
            Ok(Self {
                file: Arc::new(file),
            })
        }

        pub(crate) fn open(path: &Path) -> Result<Self> {
            let directory = Self::walk(path)?;
            private(&directory.file, true)?;
            Ok(directory)
        }

        pub(crate) fn create(path: &Path) -> Result<Self> {
            super::super::local_path(path)?;
            let name = path
                .file_name()
                .ok_or_else(|| Error::refused("Use a new artifact directory."))?;
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Self::walk(parent)?.create_child(name)
        }

        pub(crate) fn create_child(&self, name: impl AsRef<OsStr>) -> Result<Self> {
            let name = name.as_ref();
            member(name)?;
            let metadata = self.file.metadata()?;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0
            {
                return Err(Error::refused(
                    "A new artifact needs an owned parent without group/world write access.",
                ));
            }
            match fs::mkdirat(self.file.as_ref(), name, Mode::RWXU) {
                Err(Errno::EXIST) => {
                    return Err(Error::refused(
                        "Artifact destination already exists. It will not be overwritten.",
                    ));
                }
                result => system(result)?,
            }
            let directory = self.open_child(name)?;
            self.sync()?;
            Ok(directory)
        }

        pub(crate) fn open_child(&self, name: impl AsRef<OsStr>) -> Result<Self> {
            let name = name.as_ref();
            member(name)?;
            let file = File::from(system(fs::openat(
                self.file.as_ref(),
                name,
                directory_flags(),
                Mode::empty(),
            ))?);
            private(&file, true)?;
            Ok(Self {
                file: Arc::new(file),
            })
        }

        fn open_file(&self, name: &OsStr, flags: OFlags) -> Result<File> {
            member(name)?;
            private(&self.file, true)?;
            let file = File::from(system(fs::openat(
                self.file.as_ref(),
                name,
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            ))?);
            private(&file, false)?;
            Ok(file)
        }

        pub(crate) fn read(&self, name: impl AsRef<OsStr>) -> Result<Vec<u8>> {
            read_private_file(&mut self.open_file(name.as_ref(), OFlags::RDONLY)?)
        }

        pub(crate) fn check_file(&self, name: impl AsRef<OsStr>) -> Result<()> {
            self.open_file(name.as_ref(), OFlags::RDONLY)?;
            Ok(())
        }

        pub(crate) fn open_read_write(&self, name: impl AsRef<OsStr>) -> Result<File> {
            self.open_file(name.as_ref(), OFlags::RDWR)
        }

        pub(crate) fn open_append(&self, name: impl AsRef<OsStr>) -> Result<File> {
            self.open_file(name.as_ref(), OFlags::RDWR | OFlags::APPEND)
        }

        pub(crate) fn write_new(&self, name: impl AsRef<OsStr>, bytes: &[u8]) -> Result<()> {
            if bytes.len() as u64 > MAX_BYTES {
                return Err(Error::refused("Artifact exceeds the 4 MiB limit."));
            }
            let mut file = self.open_file(
                name.as_ref(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            )?;
            file.write_all(bytes)?;
            file.sync_all()?;
            private(&file, false)
        }

        pub(crate) fn replace(&self, from: &str, to: &str) -> Result<()> {
            member(OsStr::new(from))?;
            member(OsStr::new(to))?;
            private(&self.file, true)?;
            system(fs::renameat(
                self.file.as_ref(),
                from,
                self.file.as_ref(),
                to,
            ))?;
            self.sync()
        }

        pub(crate) fn check_members(
            &self,
            allowed: &[&str],
            pending_prefix: Option<&str>,
        ) -> Result<()> {
            let entries = system(Dir::read_from(self.file.as_ref()))?;
            for entry in entries {
                let entry = system(entry)?;
                let bytes = entry.file_name().to_bytes();
                if bytes == b"."
                    || bytes == b".."
                    || allowed.iter().any(|name| bytes == name.as_bytes())
                {
                    continue;
                }
                if pending_prefix.is_some_and(|prefix| bytes.starts_with(prefix.as_bytes())) {
                    self.open_file(OsStr::from_bytes(bytes), OFlags::RDONLY)?;
                } else {
                    return Err(Error::integrity("Unexpected artifact directory member."));
                }
            }
            Ok(())
        }

        pub(crate) fn sync(&self) -> Result<()> {
            self.file.sync_all()?;
            Ok(())
        }
    }

    pub(crate) fn read_private_file(file: &mut File) -> Result<Vec<u8>> {
        private(file, false)?;
        if file.metadata()?.len() > MAX_BYTES {
            return Err(Error::refused("Artifact exceeds the 4 MiB limit."));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::refused("Artifact grew beyond the 4 MiB limit."));
        }
        private(file, false)?;
        Ok(bytes)
    }
}

#[cfg(unix)]
pub(crate) use unix::read_private_file;

#[cfg(not(unix))]
fn unsupported<T>() -> Result<T> {
    Err(Error::refused(
        "Private artifact operations are unavailable until native ACL handling is implemented.",
    ))
}

#[cfg(not(unix))]
impl Directory {
    pub(crate) fn open(_: &Path) -> Result<Self> {
        unsupported()
    }
    pub(crate) fn create(_: &Path) -> Result<Self> {
        unsupported()
    }
    pub(crate) fn create_child(&self, _: impl AsRef<std::ffi::OsStr>) -> Result<Self> {
        unsupported()
    }
    pub(crate) fn open_child(&self, _: impl AsRef<std::ffi::OsStr>) -> Result<Self> {
        unsupported()
    }
    pub(crate) fn read(&self, _: impl AsRef<std::ffi::OsStr>) -> Result<Vec<u8>> {
        unsupported()
    }
    pub(crate) fn check_file(&self, _: impl AsRef<std::ffi::OsStr>) -> Result<()> {
        unsupported()
    }
    pub(crate) fn open_read_write(&self, _: impl AsRef<std::ffi::OsStr>) -> Result<File> {
        unsupported()
    }
    pub(crate) fn open_append(&self, _: impl AsRef<std::ffi::OsStr>) -> Result<File> {
        unsupported()
    }
    pub(crate) fn write_new(&self, _: impl AsRef<std::ffi::OsStr>, _: &[u8]) -> Result<()> {
        unsupported()
    }
    pub(crate) fn replace(&self, _: &str, _: &str) -> Result<()> {
        unsupported()
    }
    pub(crate) fn check_members(&self, _: &[&str], _: Option<&str>) -> Result<()> {
        unsupported()
    }
    pub(crate) fn sync(&self) -> Result<()> {
        unsupported()
    }
}

#[cfg(not(unix))]
pub(crate) fn read_private_file(_: &mut File) -> Result<Vec<u8>> {
    unsupported()
}
