use anyhow::Context;
use fs2::FileExt;
use ssh_key::{Algorithm, LineEnding, PrivateKey};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .context("HOME is required to locate SSH configuration")
}
pub fn directory() -> anyhow::Result<PathBuf> {
    let path = match std::env::var_os("DRUM_CONFIG_DIR") {
        Some(path) => PathBuf::from(path),
        None => match std::env::var_os("XDG_CONFIG_HOME") {
            Some(path) => PathBuf::from(path).join("drum"),
            None => home()?.join(".config/drum"),
        },
    };
    anyhow::ensure!(
        path.is_absolute(),
        "Drum config directory must be an absolute path"
    );
    Ok(path)
}

pub fn private_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let metadata = fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
            "configuration directory must be a real directory owned by you"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    anyhow::bail!("private key storage currently supports Unix only");
    Ok(())
}

fn open(path: &Path, create: bool) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "configuration file must be a regular file"
    );
    Ok(file)
}

pub fn lock(path: &Path) -> anyhow::Result<File> {
    let file = open(path, true)?;
    file.lock_exclusive()?;
    Ok(file)
}

/// Optional work should skip a busy lock rather than delaying another command.
pub fn try_lock(path: &Path) -> anyhow::Result<Option<File>> {
    let file = open(path, true)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn read(path: &Path) -> anyhow::Result<Option<String>> {
    let file = match open(path, false) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut data = String::new();
    file.take(1_048_577).read_to_string(&mut data)?;
    anyhow::ensure!(data.len() <= 1_048_576, "configuration file is too large");
    Ok(Some(data))
}

pub fn write(path: &Path, value: &str) -> anyhow::Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        open(path, false)?;
    }
    let parent = path.parent().context("configuration path has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(value.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn load_or_create(directory: &Path) -> anyhow::Result<PrivateKey> {
    let path = directory.join("id_ed25519");
    let key = match read(&path)? {
        Some(encoded) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                let metadata = fs::symlink_metadata(&path)?;
                anyhow::ensure!(
                    metadata.permissions().mode() & 0o077 == 0
                        && metadata.uid() == unsafe { libc::geteuid() },
                    "Drum private key must be owned by you with permissions 0600"
                );
            }
            PrivateKey::from_openssh(zeroize::Zeroizing::new(encoded).as_str())
                .context("could not read Drum identity; existing keys are never replaced")?
        }
        None => {
            let key = PrivateKey::random(&mut rand::rngs::OsRng, Algorithm::Ed25519)?;
            write(&path, key.to_openssh(LineEnding::LF)?.as_str())?;
            key
        }
    };
    anyhow::ensure!(
        key.algorithm() == Algorithm::Ed25519 && !key.is_encrypted(),
        "Drum identity must be an unencrypted Ed25519 key"
    );
    write(
        &directory.join("id_ed25519.pub"),
        &format!("{}\n", key.public_key().to_openssh()?),
    )?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_stable_and_private() {
        let dir = tempfile::tempdir().unwrap();
        private_dir(dir.path()).unwrap();
        let first = load_or_create(dir.path()).unwrap();
        assert_eq!(
            first.public_key(),
            load_or_create(dir.path()).unwrap().public_key()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("id_ed25519"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    #[cfg(unix)]
    fn refuses_symlinked_keys() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("outside");
        fs::write(&target, "do not replace").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("id_ed25519")).unwrap();
        assert!(load_or_create(dir.path()).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "do not replace");
    }
}
