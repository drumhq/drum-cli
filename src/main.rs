mod identity;
mod protocol;
mod sharing;
mod ssh;
mod trust;
mod updates;

use crate::protocol::{
    api_origin,
    git::{CreateProject, Project, ProjectChallenge, ProjectChallengeRequest, SIGNING_NAMESPACE},
};
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ssh_key::{HashAlg, LineEnding};
use std::{
    ffi::OsString,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::Duration,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "drum", version, about = "Push your projects to Drum using Git")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create your identity and project, then configure the Drum Git remote
    Init {
        #[arg(long, env = "DRUM_API_URL")]
        api_url: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Push the current branch to Drum (or specify a branch)
    Push {
        branch: Option<String>,
        /// Additional Git options, after --
        #[arg(last = true)]
        git_options: Vec<OsString>,
    },
    /// Pull the current branch from Drum (or specify a branch)
    Pull {
        branch: Option<String>,
        /// Approve and remember this repository and its servers without prompting
        #[arg(long)]
        trust: bool,
        /// Additional Git options, after --
        #[arg(last = true)]
        git_options: Vec<OsString>,
    },
    /// Make this repository public and print its read-only clone URL
    Publish {
        #[arg(long)]
        yes: bool,
    },
    /// Make this repository private again
    Unpublish,
    /// Create or list write invitations (single-use, expires in 24 hours)
    Invite {
        #[arg(long)]
        list: bool,
    },
    /// Revoke an invitation and its current writer grant
    Revoke { invitation_id: String },
    /// Clone a repository, claiming write access if the URL includes an invitation
    Clone {
        url: String,
        directory: Option<PathBuf>,
        #[arg(long, env = "DRUM_API_URL")]
        api_url: Option<String>,
        /// Approve and remember this repository and its servers without prompting
        #[arg(long)]
        trust: bool,
    },
    #[command(hide = true)]
    Credential {
        #[arg(long)]
        api_url: String,
        #[arg(long)]
        git_origin: String,
        #[arg(long)]
        project: String,
        operation: String,
    },
    /// Show the local Drum project, remote, and current branch
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let check_updates = !matches!(&cli.command, Commands::Credential { .. })
        && std::io::stderr().is_terminal()
        && std::env::var_os("DRUM_NO_UPDATE_CHECK").is_none();
    let mut api = std::env::var("DRUM_API_URL").ok();
    match run(cli, &mut api) {
        Ok(code) => {
            if code == 0 && check_updates {
                updates::notify(api.as_deref());
            }
            ExitCode::from(code)
        }
        Err(error) => {
            eprintln!("drum: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli, update_api: &mut Option<String>) -> anyhow::Result<u8> {
    match cli.command {
        Commands::Clone {
            url,
            directory,
            api_url,
            trust,
        } => {
            *update_api = Some(sharing::clone_repository(url, directory, api_url, trust)?);
            return Ok(0);
        }
        Commands::Credential {
            api_url,
            git_origin,
            project,
            operation,
        } => {
            sharing::credential(api_url, git_origin, project, operation)?;
            return Ok(0);
        }
        _ => {}
    }
    let repo = repository(matches!(&cli.command, Commands::Init { .. }))?;
    *update_api = config(&repo, "drum.api-url")
        .ok()
        .flatten()
        .or_else(|| update_api.take());
    match cli.command {
        Commands::Publish { yes } => {
            sharing::publish(&repo, yes)?;
            Ok(0)
        }
        Commands::Unpublish => {
            sharing::unpublish(&repo)?;
            Ok(0)
        }
        Commands::Invite { list } => {
            sharing::invite(&repo, list)?;
            Ok(0)
        }
        Commands::Revoke { invitation_id } => {
            sharing::revoke(&repo, invitation_id)?;
            Ok(0)
        }
        Commands::Clone { .. } | Commands::Credential { .. } => unreachable!(),
        Commands::Init { api_url, name } => {
            *update_api = Some(init(&repo, api_url, name)?);
            Ok(0)
        }
        Commands::Status => {
            status(&repo)?;
            Ok(0)
        }
        Commands::Push {
            branch,
            git_options,
        } => transfer(&repo, "push", branch, git_options),
        Commands::Pull {
            branch,
            git_options,
            trust,
        } => {
            let repository = drum_remote(&repo)?;
            let remote = output(&repo, &["remote", "get-url", "drum"])?;
            let api = config(&repo, "drum.api-url")?
                .context("no Drum API configured; run drum init or drum clone first")?;
            trust::approve(
                &trust::Source::with_effective_remote(&repository, &remote, &api)?,
                trust,
            )?;
            transfer(&repo, "pull", branch, git_options)
        }
    }
}

fn git(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo);
    command
}
fn output(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let result = git(repo)
        .args(args)
        .output()
        .context("running Git; install Git and ensure it is on PATH")?;
    anyhow::ensure!(
        result.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(String::from_utf8(result.stdout)
        .context("Git returned non-UTF-8 text")?
        .trim_end_matches(['\r', '\n'])
        .into())
}
fn repository(initialize: bool) -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let mut result = git(&cwd)
        .env("LC_ALL", "C")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running Git; ensure Git is installed")?;
    if !result.status.success() {
        let outside = String::from_utf8_lossy(&result.stderr).contains("not a git repository");
        anyhow::ensure!(
            initialize && outside,
            "run this command inside a Git working directory; use drum init to create one"
        );
        anyhow::ensure!(
            !cwd.ancestors()
                .any(|path| std::fs::symlink_metadata(path.join(".git")).is_ok()),
            "an existing Git directory could not be opened; resolve its permissions or configuration before initializing"
        );
        let status = git(&cwd)
            .arg("init")
            .status()
            .context("initializing Git repository")?;
        anyhow::ensure!(status.success(), "git init failed");
        result = git(&cwd).args(["rev-parse", "--show-toplevel"]).output()?;
        anyhow::ensure!(
            result.status.success(),
            "could not locate the initialized Git repository"
        );
    }
    Ok(PathBuf::from(
        String::from_utf8(result.stdout)?.trim_end_matches(['\r', '\n']),
    ))
}
fn config(repo: &Path, name: &str) -> anyhow::Result<Option<String>> {
    let result = git(repo)
        .args(["config", "--local", "--get", name])
        .output()?;
    if result.status.code() == Some(1) {
        return Ok(None);
    }
    anyhow::ensure!(result.status.success(), "could not read Git configuration");
    Ok(Some(
        String::from_utf8(result.stdout)?
            .trim_end_matches(['\r', '\n'])
            .into(),
    ))
}
fn set_config(repo: &Path, name: &str, value: &str) -> anyhow::Result<()> {
    output(repo, &["config", "--local", name, value])?;
    Ok(())
}
fn current_branch(repo: &Path) -> anyhow::Result<String> {
    let result = git(repo)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()?;
    anyhow::ensure!(
        result.status.success(),
        "HEAD is detached; switch to a branch or specify one for push/pull"
    );
    Ok(String::from_utf8(result.stdout)?.trim().into())
}
fn drum_remote(repo: &Path) -> anyhow::Result<String> {
    config(repo, "remote.drum.url")?.context("this project has no Drum remote; run drum init first")
}

fn init(repo: &Path, api: Option<String>, name: Option<String>) -> anyhow::Result<String> {
    let directory = identity::directory()?;
    identity::private_dir(&directory)?;
    let _lock = identity::lock(&directory.join("init.lock"))?;
    let saved_id = config(repo, "drum.project-id")?;
    let remote = config(repo, "remote.drum.url")?;
    let saved_remote = config(repo, "drum.remote-url")?;
    if remote.is_some() && (saved_id.is_none() || remote != saved_remote) {
        bail!(
            "remote 'drum' already exists with different configuration; resolve it before initializing"
        );
    }
    let saved_api = config(repo, "drum.api-url")?;
    let api = api_origin(
        &api.or_else(|| saved_api.clone())
            .unwrap_or_else(|| crate::protocol::DEFAULT_API_URL.into()),
    )
    .map_err(anyhow::Error::msg)?;
    if let Some(saved) = saved_api {
        anyhow::ensure!(
            saved == api,
            "this project is already associated with a different Drum API"
        );
    }
    let branch = current_branch(repo)?;
    let id = match saved_id {
        Some(id) => Uuid::parse_str(&id).context("invalid saved Drum project ID")?,
        None => Uuid::new_v4(),
    };
    let name = name.unwrap_or_else(|| {
        repo.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into()
    });
    anyhow::ensure!(
        !name.trim().is_empty() && name.len() <= 200 && !name.chars().any(char::is_control),
        "project name must be 1–200 bytes without control characters"
    );
    let key = identity::load_or_create(&directory)?;
    // Persist the stable ID before networking so retries cannot create duplicate projects.
    set_config(repo, "drum.project-id", &id.to_string())?;
    set_config(repo, "drum.api-url", &api)?;
    let public_key =
        ssh_key::PublicKey::new(key.public_key().key_data().clone(), "").to_openssh()?;
    let request = ProjectChallengeRequest {
        public_key,
        project_id: id.to_string(),
        name,
        default_branch: branch.clone(),
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let challenge: ProjectChallenge = response(
        client
            .post(format!("{api}/v1/projects/challenges"))
            .json(&request)
            .send()
            .context("connecting to Drum API")?,
    )?;
    // Do not sign an arbitrary document: require the exact requested operation and namespace.
    let (namespace, _audience, _nonce, bound_request): (
        String,
        String,
        String,
        ProjectChallengeRequest,
    ) = serde_json::from_str(&challenge.message).context("invalid API signing challenge")?;
    anyhow::ensure!(
        namespace == SIGNING_NAMESPACE
            && serde_json::to_value(&bound_request)? == serde_json::to_value(&request)?,
        "API challenge does not match the requested project"
    );
    let signature = key
        .sign(
            SIGNING_NAMESPACE,
            HashAlg::Sha256,
            challenge.message.as_bytes(),
        )?
        .to_pem(LineEnding::LF)?;
    let project: Project = response(
        client
            .post(format!("{api}/v1/projects"))
            .json(&CreateProject {
                challenge_id: challenge.id,
                signature,
            })
            .send()
            .context("creating Drum project")?,
    )?;
    anyhow::ensure!(
        project.id == id.to_string(),
        "API returned a different project"
    );
    let remote = ssh::configure(&directory, &project)?;
    set_config(repo, "drum.remote-url", &remote)?;
    if config(repo, "remote.drum.url")?.is_some() {
        output(repo, &["remote", "set-url", "drum", &remote])?;
    } else {
        output(repo, &["remote", "add", "drum", &remote])?;
    }
    set_config(repo, "drum.project-name", &project.name)?;
    set_config(repo, "drum.short-id", &project.short_id)?;
    println!(
        "Project: {}\nRemote:  {}\n\nReady. Push with `drum push` or `git push drum {branch}`.",
        project.name, remote
    );
    Ok(api)
}

fn response<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> anyhow::Result<T> {
    let status = response.status();
    if !status.is_success() {
        let error: serde_json::Value = response.json().unwrap_or_default();
        let message = error["error"]["message"]
            .as_str()
            .unwrap_or("the server rejected the request");
        bail!("Drum API ({status}): {message}");
    }
    response.json().context("invalid Drum API response")
}

fn transfer(
    repo: &Path,
    operation: &str,
    branch: Option<String>,
    options: Vec<OsString>,
) -> anyhow::Result<u8> {
    drum_remote(repo)?;
    let branch = branch.map(Ok).unwrap_or_else(|| current_branch(repo))?;
    anyhow::ensure!(
        !branch.starts_with('-') && !branch.is_empty(),
        "invalid branch"
    );
    let status = git(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg(operation)
        .args(options)
        .arg("--")
        .arg("drum")
        .arg(branch)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("running Git")?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1))
}

fn status(repo: &Path) -> anyhow::Result<()> {
    println!(
        "Project: {}",
        config(repo, "drum.project-name")?.unwrap_or_else(|| "not initialized".into())
    );
    println!("Remote:  {}", drum_remote(repo)?);
    println!(
        "Branch:  {}",
        current_branch(repo).unwrap_or_else(|_| "detached HEAD".into())
    );
    println!(
        "Identity: {}",
        identity::directory()?.join("id_ed25519.pub").display()
    );
    Ok(())
}
