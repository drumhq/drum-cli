use crate::protocol::{api_origin, git::*};
use crate::{config, identity, output, response, set_config};
use anyhow::Context;
use sha2::{Digest, Sha256};
use ssh_key::{HashAlg, LineEnding, PublicKey};
use std::{
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use zeroize::Zeroizing;

fn client() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn hash(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn key() -> anyhow::Result<ssh_key::PrivateKey> {
    let directory = identity::directory()?;
    identity::private_dir(&directory)?;
    let _lock = identity::lock(&directory.join("init.lock"))?;
    identity::load_or_create(&directory)
}
pub fn invoke(
    api: &str,
    project: &str,
    action: SharingAction,
    token: Option<&str>,
    invite_id: Option<String>,
) -> anyhow::Result<SharingResult> {
    invoke_for_origin(api, project, action, token, invite_id, None)
}
fn invoke_for_origin(
    api: &str,
    project: &str,
    action: SharingAction,
    token: Option<&str>,
    invite_id: Option<String>,
    expected_origin: Option<&str>,
) -> anyhow::Result<SharingResult> {
    let api = api_origin(api).map_err(anyhow::Error::msg)?;
    let key = key()?;
    let request = SharingRequest {
        public_key: PublicKey::new(key.public_key().key_data().clone(), "").to_openssh()?,
        intent: SharingIntent {
            project: project.into(),
            action,
            token_hash: token.map(hash),
            invite_id,
        },
    };
    let client = client()?;
    let challenge: ProjectChallenge = response(
        client
            .post(format!("{api}/v1/sharing/challenges"))
            .json(&request)
            .send()
            .context("connecting to Drum API")?,
    )?;
    let (namespace, audience, _nonce, bound): (String, String, String, SharingRequest) =
        serde_json::from_str(&challenge.message).context("invalid sharing challenge")?;
    anyhow::ensure!(
        namespace == SHARING_NAMESPACE && bound == request,
        "API challenge does not match the requested operation"
    );
    if let Some(expected) = expected_origin {
        anyhow::ensure!(
            api_origin(&audience).map_err(anyhow::Error::msg)? == expected,
            "API challenge targets a different Git server"
        );
    }
    let signature = key
        .sign(
            SHARING_NAMESPACE,
            HashAlg::Sha256,
            challenge.message.as_bytes(),
        )?
        .to_pem(LineEnding::LF)?;
    let result: SharingResult = response(
        client
            .post(format!("{api}/v1/sharing/complete"))
            .json(&SharingProof {
                challenge_id: challenge.id,
                signature,
                token: token.map(String::from),
            })
            .send()
            .context("completing Drum operation")?,
    )?;
    anyhow::ensure!(
        result.project.short_id == project,
        "API returned a different repository"
    );
    Ok(result)
}
fn local_project(repo: &Path) -> anyhow::Result<(String, String)> {
    let api = config(repo, "drum.api-url")?.context("run drum init or drum clone first")?;
    let short = if let Some(value) = config(repo, "drum.short-id")? {
        value
    } else {
        let remote = config(repo, "remote.drum.url")?.context("no Drum remote configured")?;
        if let Ok(url) = url::Url::parse(&remote) {
            url.path()
                .trim_start_matches('/')
                .trim_end_matches(".git")
                .into()
        } else {
            remote
                .rsplit_once(':')
                .context("invalid Drum remote")?
                .1
                .trim_end_matches(".git")
                .into()
        }
    };
    anyhow::ensure!(valid_short_id(&short), "invalid saved Drum project ID");
    Ok((api, short))
}
pub fn publish(repo: &Path, yes: bool) -> anyhow::Result<()> {
    let (api, short) = local_project(repo)?;
    let inspected = invoke(&api, &short, SharingAction::Inspect, None, None)?;
    if !inspected.is_public {
        if !yes {
            eprintln!(
                "Publishing exposes all branches, tags, and reachable Git history. Existing clones cannot be recalled by unpublishing."
            );
            anyhow::ensure!(
                io::stdin().is_terminal(),
                "run interactively to confirm publication, or use drum publish --yes"
            );
            eprint!("Make this repository public? [y/N] ");
            io::stderr().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                println!("Publication cancelled.");
                return Ok(());
            }
        }
        let result = invoke(&api, &short, SharingAction::Publish, None, None)?;
        println!("Public read-only URL:\n{}", result.clone_url);
    } else {
        println!("Public read-only URL:\n{}", inspected.clone_url);
    }
    Ok(())
}
pub fn unpublish(repo: &Path) -> anyhow::Result<()> {
    let (api, short) = local_project(repo)?;
    invoke(&api, &short, SharingAction::Unpublish, None, None)?;
    println!(
        "Repository is private. Existing writer grants remain; unused invitations have been revoked."
    );
    Ok(())
}
pub fn invite(repo: &Path, list: bool) -> anyhow::Result<()> {
    let (api, short) = local_project(repo)?;
    let action = if list {
        SharingAction::ListInvites
    } else {
        SharingAction::Invite
    };
    let result = invoke(&api, &short, action, None, None)?;
    if action == SharingAction::Invite {
        let invitation = result
            .invitation
            .context("API did not return an invitation")?;
        println!(
            "Invitation: {}\nExpires: {}\nSingle-use write URL (keep secret):\n{}",
            invitation.id,
            invitation.expires_at,
            result.write_url.context("API did not return a write URL")?
        );
    } else {
        for invitation in result.invitations {
            println!(
                "{}  expires={}  claimed={}  revoked={}  write_active={}",
                invitation.id,
                invitation.expires_at,
                invitation.claimed_by.as_deref().unwrap_or("unused"),
                invitation.revoked,
                invitation.active
            );
        }
    }
    Ok(())
}
pub fn revoke(repo: &Path, invitation_id: String) -> anyhow::Result<()> {
    uuid::Uuid::parse_str(&invitation_id).context("invalid invitation ID")?;
    let (api, short) = local_project(repo)?;
    invoke(
        &api,
        &short,
        SharingAction::RevokeInvite,
        None,
        Some(invitation_id),
    )?;
    println!("Invitation and its current write grant revoked.");
    Ok(())
}

struct CloneUrl {
    clean: url::Url,
    short: String,
    token: Option<Zeroizing<String>>,
}
impl CloneUrl {
    fn parse(value: &str) -> anyhow::Result<Self> {
        let mut url =
            url::Url::parse(value).map_err(|_| anyhow::anyhow!("invalid repository URL"))?;
        let origin = api_origin(&url.origin().ascii_serialization()).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            url.username().is_empty() && url.password().is_none() && url.query().is_none(),
            "clone URL must not contain credentials or query parameters"
        );
        let short = url
            .path()
            .trim_start_matches('/')
            .strip_suffix(".git")
            .filter(|value| valid_short_id(value))
            .context("expected a Drum repository URL ending in xxxx-xxxx.git")?
            .to_string();
        anyhow::ensure!(
            url.path() == format!("/{short}.git"),
            "invalid repository path"
        );
        let token = match url.fragment() {
            None => None,
            Some(value) => Some(Zeroizing::new(
                value
                    .strip_prefix("token=")
                    .filter(|value| {
                        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    .context("invalid write invitation")?
                    .to_string(),
            )),
        };
        url.set_fragment(None);
        anyhow::ensure!(
            url.origin().ascii_serialization() == origin,
            "invalid repository origin"
        );
        Ok(Self {
            clean: url,
            short,
            token,
        })
    }
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn helper(api: &str, origin: &str, project: &str) -> anyhow::Result<String> {
    let executable = std::env::current_exe()?
        .to_str()
        .context("CLI executable path must be UTF-8")?
        .to_string();
    Ok(format!(
        "!{} credential --api-url {} --git-origin {} --project {}",
        shell_quote(&executable),
        shell_quote(api),
        shell_quote(origin),
        shell_quote(project)
    ))
}
fn configure(
    repo: &Path,
    api: &str,
    url: &url::Url,
    project: &Project,
    identity_id: Option<&str>,
) -> anyhow::Result<()> {
    let clean = url.as_str();
    set_config(repo, "drum.api-url", api)?;
    set_config(repo, "drum.project-id", &project.id)?;
    set_config(repo, "drum.short-id", &project.short_id)?;
    set_config(repo, "drum.project-name", &project.name)?;
    set_config(repo, "drum.remote-url", clean)?;
    if let Some(identity) = identity_id {
        set_config(repo, "drum.identity-id", identity)?;
    }
    output(repo, &["remote", "add", "drum", clean])?;
    let context = format!("credential.{clean}");
    set_config(repo, &format!("{context}.helper"), "")?;
    output(
        repo,
        &[
            "config",
            "--local",
            "--add",
            &format!("{context}.helper"),
            &helper(api, &url.origin().ascii_serialization(), &project.short_id)?,
        ],
    )?;
    set_config(repo, "credential.useHttpPath", "true")?;
    set_config(repo, &format!("http.{clean}.followRedirects"), "false")?;
    Ok(())
}
fn directory_name(name: &str, short: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches(['.', '-']);
    if name.is_empty() {
        short.into()
    } else {
        name.into()
    }
}
pub fn clone_repository(
    raw: String,
    destination: Option<PathBuf>,
    api_override: Option<String>,
    trust: bool,
) -> anyhow::Result<String> {
    let raw = Zeroizing::new(raw);
    let link = CloneUrl::parse(&raw)?;
    let client = client()?;
    let origin = link.clean.origin().ascii_serialization();
    let api = if let Some(api) = api_override {
        api_origin(&api).map_err(anyhow::Error::msg)?
    } else {
        let config: GitDiscovery = response(
            client
                .get(format!("{origin}/drum/config"))
                .send()
                .context("discovering Drum API")?,
        )?;
        api_origin(&config.api_url).map_err(anyhow::Error::msg)?
    };
    // Discovery is anonymous. Obtain consent before claiming an invitation,
    // sending identity proofs, fetching project metadata, or starting Git.
    let effective = output(
        &std::env::current_dir()?,
        &["ls-remote", "--get-url", "--", link.clean.as_str()],
    )?;
    crate::trust::approve(
        &crate::trust::Source::with_effective_remote(link.clean.as_str(), &effective, &api)?,
        trust,
    )?;
    let mut identity_id = None;
    let project = if let Some(token) = &link.token {
        let claimed = invoke_for_origin(
            &api,
            &link.short,
            SharingAction::Claim,
            Some(token.as_str()),
            None,
            Some(&origin),
        )?;
        identity_id = Some(claimed.identity_id);
        claimed.project
    } else {
        let metadata = client
            .get(format!("{origin}/{}.git/drum", link.short))
            .send()
            .context("reading repository metadata")?;
        if metadata.status() == reqwest::StatusCode::NOT_FOUND {
            let access = invoke_for_origin(
                &api,
                &link.short,
                SharingAction::Access,
                None,
                None,
                Some(&origin),
            )?;
            identity_id = Some(access.identity_id);
            access.project
        } else {
            response::<PublicProject>(metadata)?.project
        }
    };
    anyhow::ensure!(
        project.short_id == link.short && uuid::Uuid::parse_str(&project.id).is_ok(),
        "server returned invalid repository metadata"
    );
    let destination =
        destination.unwrap_or_else(|| PathBuf::from(directory_name(&project.name, &link.short)));
    let helper = helper(&api, &origin, &link.short)?;
    let status = Command::new("git")
        .args(["-c", "credential.helper=", "-c"])
        .arg(format!("credential.helper={helper}"))
        .args([
            "-c",
            "credential.useHttpPath=true",
            "-c",
            "http.followRedirects=false",
            "clone",
            "--",
        ])
        .arg(link.clean.as_str())
        .arg(&destination)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("running Git clone")?;
    anyhow::ensure!(
        status.success(),
        "clone failed; any claimed grant remains attached to your identity, so you can retry"
    );
    configure(
        &destination,
        &api,
        &link.clean,
        &project,
        identity_id.as_deref(),
    )?;
    println!("Cloned {}.\nRemote: {}", project.name, link.clean);
    if link.token.is_some() {
        println!(
            "Write access granted to your identity. The invitation token was removed from the remote URL."
        );
    }
    Ok(api)
}
pub fn credential(
    api: String,
    origin: String,
    project: String,
    operation: String,
) -> anyhow::Result<()> {
    let mut input = Zeroizing::new(String::new());
    io::stdin().take(8193).read_to_string(&mut input)?;
    if operation != "get" {
        return Ok(());
    }
    let result = (|| -> anyhow::Result<String> {
        anyhow::ensure!(
            input.len() <= 8192 && valid_short_id(&project),
            "invalid Git credential request"
        );
        let fields: std::collections::HashMap<_, _> = input
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        let protocol = fields
            .get("protocol")
            .context("credential request missing protocol")?;
        let host = fields
            .get("host")
            .context("credential request missing host")?;
        let requested = api_origin(&format!("{protocol}://{host}")).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            requested == api_origin(&origin).map_err(anyhow::Error::msg)?,
            "credential request targets another host"
        );
        let expected = format!("{project}.git");
        anyhow::ensure!(
            fields
                .get("path")
                .is_some_and(|value| value.trim_matches('/') == expected),
            "credential request targets another repository"
        );
        let access = invoke_for_origin(
            &api,
            &project,
            SharingAction::Access,
            None,
            None,
            Some(&origin),
        )?;
        access.credential.context(
            "this identity has read-only access; request a write invitation from the owner",
        )
    })();
    match result {
        Ok(token) => {
            println!("username=drum\npassword={token}\n");
            Ok(())
        }
        Err(error) => {
            println!("quit=true\n");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_are_removed_before_git_urls_are_used() {
        let token = "a".repeat(64);
        let parsed = CloneUrl::parse(&format!(
            "http://localhost:3001/abcd-1234.git#token={token}"
        ))
        .unwrap();
        assert!(parsed.clean.fragment().is_none());
        assert!(!parsed.clean.as_str().contains(&token));
        assert!(CloneUrl::parse("http://example.com/abcd-1234.git").is_err());
        assert!(CloneUrl::parse("https://user:secret@example.com/abcd-1234.git").is_err());
        assert!(CloneUrl::parse("https://example.com/abcd-1234.git?token=secret").is_err());
        assert_eq!(directory_name("../../evil", "abcd-1234"), "evil");
    }
}
