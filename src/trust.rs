//! User approvals live outside repositories and bind the repository URL and API.
use crate::{identity, protocol::api_origin};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    repository: String,
    remote: String,
    api: String,
}

impl Source {
    pub fn new(remote: &str, api: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !remote.chars().any(char::is_control),
            "repository URL cannot contain control characters"
        );
        // Init uses SCP syntax on the standard SSH port. Normalize it so the
        // equivalent explicit SSH URL identifies the same repository.
        let remote = if let Some(address) = remote.strip_prefix("git@") {
            let (host, path) = address.split_once(':').context("invalid Git remote")?;
            format!("ssh://git@{host}/{}", path.trim_start_matches('/'))
        } else {
            remote.to_string()
        };
        let mut url = url::Url::parse(&remote).context("invalid repository URL")?;
        anyhow::ensure!(
            matches!(url.scheme(), "https" | "http" | "ssh")
                && url.host_str().is_some()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && if url.scheme() == "ssh" {
                    url.username() == "git"
                } else {
                    url.username().is_empty()
                },
            "expected a Drum HTTP(S) or SSH repository URL without credentials, query or fragment"
        );
        let short = url.path().trim_start_matches('/').trim_end_matches(".git");
        anyhow::ensure!(
            crate::protocol::git::valid_short_id(short) && url.path() == format!("/{short}.git"),
            "expected a Drum repository URL ending in xxxx-xxxx.git"
        );
        if url.scheme() == "ssh" && url.port() == Some(22) {
            url.set_port(None)
                .map_err(|_| anyhow::anyhow!("invalid SSH port"))?;
        }
        Ok(Self {
            repository: url.to_string(),
            remote: url.to_string(),
            api: api_origin(api).map_err(anyhow::Error::msg)?,
        })
    }

    pub fn with_effective_remote(
        repository: &str,
        effective: &str,
        api: &str,
    ) -> anyhow::Result<Self> {
        let mut source = Self::new(repository, api)?;
        source.remote = Self::new(effective, api)?.remote;
        Ok(source)
    }

    fn prompt(&self) -> String {
        // Debug escaping also makes invisible/bidirectional characters visible.
        format!(
            "Trust this repository?\n\nRepository: {:?}\nGit remote: {:?}\nAPI server: {:?}\n\nOnly continue if you trust this source.\nDownloaded code may be unsafe to run.\n\nContinue? [y/N] ",
            self.repository, self.remote, self.api
        )
    }
}

pub fn approve(source: &Source, explicit: bool) -> anyhow::Result<()> {
    approve_in(
        &identity::directory()?,
        source,
        explicit,
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        || {
            eprint!("{}", source.prompt());
            io::stderr().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            Ok(matches!(
                answer.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            ))
        },
    )
}

fn approve_in(
    directory: &Path,
    source: &Source,
    explicit: bool,
    interactive: bool,
    confirm: impl FnOnce() -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    identity::private_dir(directory)?;
    let records = directory.join("trust-v1");
    identity::private_dir(&records)?;
    let encoded = serde_json::to_string(source)?;
    let id: String = Sha256::digest(encoded.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = records.join(format!("{id}.json"));
    if let Some(saved) = identity::read(&path)? {
        let saved: Source =
            serde_json::from_str(&saved).context("invalid repository trust record")?;
        anyhow::ensure!(
            saved == *source,
            "repository trust record does not match source"
        );
        return Ok(());
    }
    if !explicit {
        anyhow::ensure!(
            interactive,
            "repository is not trusted; run interactively to review it, or pass --trust to approve and remember it"
        );
        anyhow::ensure!(confirm()?, "repository trust declined; operation cancelled");
    }
    // Do not hold a lock while the user is deciding. Writes are atomic and
    // different records are independent; concurrent identical approvals agree.
    identity::write(&path, &encoded).context("saving repository trust approval")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(remote: &str, api: &str) -> Source {
        Source::new(remote, api).unwrap()
    }

    #[test]
    fn approval_is_required_and_only_remembers_the_approved_source() {
        let dir = tempfile::tempdir().unwrap();
        let original = source("https://git.example/abcd-1234.git", "https://api.example");
        assert!(approve_in(dir.path(), &original, false, false, || panic!()).is_err());
        assert!(approve_in(dir.path(), &original, false, true, || Ok(false)).is_err());
        approve_in(dir.path(), &original, false, true, || Ok(true)).unwrap();
        approve_in(dir.path(), &original, false, false, || panic!()).unwrap();
        for changed in [
            source("https://other.example/abcd-1234.git", "https://api.example"),
            source("https://git.example/abcd-5678.git", "https://api.example"),
            source("https://git.example/abcd-1234.git", "https://other.example"),
            source(
                "ssh://git@git.example:2222/abcd-1234.git",
                "https://api.example",
            ),
        ] {
            assert!(approve_in(dir.path(), &changed, false, false, || panic!()).is_err());
        }
    }

    #[test]
    fn explicit_approval_works_without_a_terminal_and_creates_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        let source = source("https://git.example/abcd-1234.git", "https://api.example");
        approve_in(dir.path(), &source, true, false, || panic!()).unwrap();
        approve_in(dir.path(), &source, false, false, || panic!()).unwrap();
        assert!(!dir.path().join("id_ed25519").exists());
    }

    #[test]
    fn equivalent_urls_agree_and_secrets_or_terminal_controls_are_rejected() {
        assert_eq!(
            source("git@git.example:abcd-1234.git", "https://api.example/"),
            source(
                "ssh://git@git.example:22/abcd-1234.git",
                "https://api.example"
            )
        );
        for remote in [
            "https://git.example/abcd-1234.git#token=secret",
            "https://user:secret@git.example/abcd-1234.git",
            "https://git.example/abcd-1234.git?token=secret",
            "https://git.example/abcd-1234.git\x1b[31m",
            "ext::some-command",
        ] {
            assert!(Source::new(remote, "https://api.example").is_err());
        }
    }
}
