use crate::identity;
use crate::protocol::git::Project;
use anyhow::Context;
use sha2::{Digest, Sha256};
use ssh_key::{Algorithm, PublicKey};
use std::path::Path;

fn quote(path: &Path) -> anyhow::Result<String> {
    let value = path
        .to_str()
        .context("SSH configuration paths must be UTF-8")?;
    anyhow::ensure!(
        !value.chars().any(char::is_control) && !value.contains('%'),
        "SSH configuration paths cannot contain control characters or %"
    );
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

struct Endpoint {
    host: String,
    port: u16,
    remote: String,
}
impl Endpoint {
    fn parse(project: &Project) -> anyhow::Result<Self> {
        let url = url::Url::parse(&project.ssh_url).context("API returned an invalid SSH URL")?;
        anyhow::ensure!(
            url.scheme() == "ssh"
                && url.username() == "git"
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && crate::protocol::git::valid_short_id(&project.short_id)
                && url.path() == format!("/{}.git", project.short_id),
            "API returned an invalid Git remote"
        );
        let host = url
            .host_str()
            .context("Git remote needs a host")?
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        // SSH Match patterns must be literal, never user-controlled wildcard/exec syntax.
        anyhow::ensure!(
            !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-:_".contains(&b)),
            "invalid Git hostname"
        );
        let port = url.port().unwrap_or(22);
        anyhow::ensure!(port != 0, "Git SSH port must be between 1 and 65535");
        let address = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.clone()
        };
        // SCP syntax is unambiguous for DNS/IPv4 on the standard port. All other
        // endpoints carry their actual port/address explicitly, including IPv6.
        let remote = if port == 22 && !host.contains(':') {
            format!("git@{host}:{}.git", project.short_id)
        } else {
            format!("ssh://git@{address}:{port}/{}.git", project.short_id)
        };
        Ok(Self { host, port, remote })
    }
    fn identifier(&self) -> String {
        Sha256::digest(format!("{}:{}", self.host, self.port).as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn known_host(&self) -> String {
        if self.port == 22 {
            self.host.clone()
        } else {
            format!("[{}]:{}", self.host, self.port)
        }
    }
    fn snippet(&self, directory: &Path, known_hosts: &Path) -> anyhow::Result<String> {
        // OpenSSH has no built-in remote-port Match criterion. %p expands to a
        // numeric port, so this fixed local test cannot execute API-supplied code.
        // Command-line -p (from the Git URL) takes precedence over Port below.
        Ok(format!(
            "Match originalhost {} user git exec \"/bin/test %p -eq {}\"\n    HostName {}\n    Port {}\n    IdentityFile {}\n    IdentitiesOnly yes\n    IdentityAgent none\n    UserKnownHostsFile {}\n    GlobalKnownHostsFile /dev/null\n    StrictHostKeyChecking yes\n    BatchMode yes\n",
            self.host,
            self.port,
            self.host,
            self.port,
            quote(&directory.join("id_ed25519"))?,
            quote(known_hosts)?
        ))
    }
}

pub fn configure(directory: &Path, project: &Project) -> anyhow::Result<String> {
    let endpoint = Endpoint::parse(project)?;
    let key = PublicKey::from_openssh(&project.host_public_key)?;
    anyhow::ensure!(
        key.algorithm() == Algorithm::Ed25519,
        "Git host must use an Ed25519 key"
    );
    let key = PublicKey::new(key.key_data().clone(), "").to_openssh()?;
    let hosts = directory.join("hosts");
    identity::private_dir(&hosts)?;
    let ssh = identity::home()?.join(".ssh");
    identity::private_dir(&ssh)?;
    let _lock = identity::lock(&ssh.join("drum-config.lock"))?;
    // Pins identify actual host:port endpoints, independent of API origins or aliases.
    let identifier = endpoint.identifier();
    let known_hosts = hosts.join(format!("{identifier}.known_hosts"));
    let expected = format!("{} {key}\n", endpoint.known_host());
    if let Some(saved) = identity::read(&known_hosts)? {
        anyhow::ensure!(
            saved == expected,
            "Drum Git host key changed; verify the server identity before updating {}",
            known_hosts.display()
        );
    } else {
        identity::write(&known_hosts, &expected)?;
    }
    let snippet = hosts.join(format!("{identifier}.conf"));
    identity::write(&snippet, &endpoint.snippet(directory, &known_hosts)?)?;
    let path = ssh.join("config");
    let original = identity::read(&path)?.unwrap_or_default();
    let include = format!("Include {}", quote(&snippet)?);
    if !original.lines().any(|line| line == include) {
        // Reset the Match context before the user's original configuration.
        identity::write(&path, &format!("{include}\nHost *\n{original}"))?;
    }
    Ok(endpoint.remote)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project(origin: &str) -> Project {
        Project {
            id: "internal-id".into(),
            name: "example".into(),
            short_id: "k7m2-x9p4".into(),
            ssh_url: format!("{origin}/k7m2-x9p4.git"),
            host_public_key: String::new(),
        }
    }
    #[test]
    fn remote_addresses_use_real_hosts_and_explicit_nonstandard_ports() {
        for (origin, expected) in [
            ("ssh://git@git.drum.dev", "git@git.drum.dev:k7m2-x9p4.git"),
            (
                "ssh://git@git.drum.dev:22",
                "git@git.drum.dev:k7m2-x9p4.git",
            ),
            (
                "ssh://git@localhost:2222",
                "ssh://git@localhost:2222/k7m2-x9p4.git",
            ),
            (
                "ssh://git@127.0.0.1:2222",
                "ssh://git@127.0.0.1:2222/k7m2-x9p4.git",
            ),
            ("ssh://git@[::1]:2222", "ssh://git@[::1]:2222/k7m2-x9p4.git"),
            ("ssh://git@[::1]", "ssh://git@[::1]:22/k7m2-x9p4.git"),
        ] {
            assert_eq!(Endpoint::parse(&project(origin)).unwrap().remote, expected);
        }
        for origin in [
            "ssh://git@localhost:0",
            "ssh://git@bad*host:2222",
            "ssh://git@bad,host:2222",
            "ssh://other@localhost:2222",
            "https://git@localhost:2222",
        ] {
            assert!(Endpoint::parse(&project(origin)).is_err(), "{origin}");
        }
    }
    #[test]
    fn endpoint_pins_are_distinct_by_port_and_use_standard_known_host_names() {
        let first = Endpoint::parse(&project("ssh://git@localhost:2222")).unwrap();
        let second = Endpoint::parse(&project("ssh://git@localhost:2223")).unwrap();
        assert_ne!(first.identifier(), second.identifier());
        assert_eq!(first.known_host(), "[localhost]:2222");
        assert_eq!(
            Endpoint::parse(&project("ssh://git@git.drum.dev"))
                .unwrap()
                .known_host(),
            "git.drum.dev"
        );
    }
    #[test]
    fn standard_port_remotes_do_not_depend_on_a_hidden_port_setting() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::parse(&project("ssh://git@git.drum.dev")).unwrap();
        let config = directory.path().join("config");
        let snippet = endpoint
            .snippet(directory.path(), &directory.path().join("known_hosts"))
            .unwrap();
        std::fs::write(&config, format!("{snippet}\nHost *\n    Port 9999\n")).unwrap();
        let output = std::process::Command::new("ssh")
            .args(["-G", "-F"])
            .arg(config)
            .arg("git@git.drum.dev")
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("port 22\n"));
        assert!(text.contains("hostname git.drum.dev\n"));
    }
    #[test]
    fn openssh_settings_apply_only_to_the_registered_host_port_and_user() {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::parse(&project("ssh://git@localhost:2222")).unwrap();
        let snippet = directory.path().join("config");
        std::fs::write(
            &snippet,
            endpoint
                .snippet(directory.path(), &directory.path().join("known_hosts"))
                .unwrap(),
        )
        .unwrap();
        let inspect = |user: &str, port: &str, host: &str| {
            let output = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&snippet)
                .args(["-l", user, "-p", port, host])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        let selected = inspect("git", "2222", "localhost");
        assert!(selected.contains(&format!(
            "identityfile {}/id_ed25519",
            directory.path().display()
        )));
        assert!(selected.contains("hostname localhost\n"));
        assert!(selected.contains("port 2222\n"));
        assert!(!selected.contains("hostkeyalias "));
        for (user, port, host) in [
            ("tom", "2222", "localhost"),
            ("git", "2223", "localhost"),
            ("git", "2222", "example.com"),
        ] {
            assert!(!inspect(user, port, host).contains(&directory.path().display().to_string()));
        }
    }
}
