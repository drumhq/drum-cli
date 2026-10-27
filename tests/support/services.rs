use crate::support::*;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs::{self, File},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

pub struct Fixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub base: PathBuf,
    pub config: PathBuf,
    pub env: Environment,
    pub api: String,
    pub git_port: u16,
    pub http: String,
    pub projects: RefCell<Vec<String>>,
    pub git_service: Option<Process>,
    api_service: Option<Process>,
    git_log: File,
    api_log: File,
}
fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
impl Fixture {
    pub fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        for binary in [
            "drum-api/target/debug/drum-api",
            "drum-git/target/debug/drum-git",
        ] {
            assert!(root.join(binary).is_file(), "build sibling binaries first");
        }
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().to_path_buf();
        let config = base.join("config");
        let mut env = environment(&base);
        let (api_port, git_port, http_port) = (port(), port(), port());
        let api = format!("http://127.0.0.1:{api_port}");
        let http = format!("http://127.0.0.1:{http_port}");
        for (key, value) in [
            ("DRUM_TRUSTED_PROXIES", "".into()),
            ("DRUM_GIT_HOST_PUBLIC_KEY", "".into()),
            (
                "GIT_SSH_COMMAND",
                format!("ssh -F {}", shell_quote(path(&base.join(".ssh/config")))),
            ),
            ("DATA_DIR", path(&base.join("data")).into()),
            ("DRUM_GIT_BIND_ADDR", format!("127.0.0.1:{git_port}")),
            ("DRUM_BIND_ADDR", format!("127.0.0.1:{api_port}")),
            (
                "DRUM_GIT_SSH_URL",
                format!("ssh://git@127.0.0.1:{git_port}"),
            ),
            (
                "DRUM_GIT_HOST_KEY_FILE",
                path(&base.join("data/ssh_host_ed25519.pub")).into(),
            ),
            ("DRUM_GIT_HTTP_BIND_ADDR", format!("127.0.0.1:{http_port}")),
            ("DRUM_GIT_HTTP_URL", http.clone()),
            ("DRUM_API_URL", api.clone()),
            (
                "DRUM_LIMIT_NAMESPACE",
                format!("test-{}", uuid::Uuid::new_v4().simple()),
            ),
        ] {
            env.insert(key.into(), value);
        }
        let mut fixture = Self {
            _temp: temp,
            root,
            base,
            config,
            env,
            api,
            git_port,
            http,
            projects: RefCell::new(Vec::new()),
            git_service: None,
            api_service: None,
            git_log: tempfile::tempfile().unwrap(),
            api_log: tempfile::tempfile().unwrap(),
        };
        // Preflight SQL before starting processes or creating test rows.
        fixture.sql("SELECT 1");
        fixture.api_service = Some(fixture.spawn(
            "drum-api/target/debug/drum-api",
            &fixture.api_log,
            &fixture.env,
            &fixture.root.join("drum-api"),
        ));
        Self::ready(&fixture.api, fixture.api_service.as_mut().unwrap(), false);
        fixture.restart_git(false);
        fixture
    }
    fn spawn(&self, binary: &str, log: &File, env: &Environment, cwd: &Path) -> Process {
        Process(
            command(self.root.join(binary), &[], cwd, env)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log.try_clone().unwrap())
                .spawn()
                .unwrap(),
        )
    }
    fn ready(origin: &str, process: &mut Process, readonly: bool) {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(1))
            .no_proxy()
            .build()
            .unwrap();
        let start = Instant::now();
        loop {
            // A Git service without Redis intentionally fails readiness while
            // continuing to serve reads; wait for liveness in that fixture.
            let route = if readonly {
                "/health/live"
            } else {
                "/health/ready"
            };
            if client
                .get(format!("{origin}{route}"))
                .send()
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "service exited during startup"
            );
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "service not ready"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }
    pub fn restart_git(&mut self, readonly: bool) {
        if let Some(mut process) = self.git_service.take() {
            process.stop();
        }
        let mut env = self.env.clone();
        let cwd = if readonly {
            env.remove("REDIS_URL");
            env.insert("DATABASE_URL".into(), self.database_url());
            self.base.clone()
        } else {
            self.root.join("drum-api")
        };
        self.git_service =
            Some(self.spawn("drum-git/target/debug/drum-git", &self.git_log, &env, &cwd));
        Self::ready(&self.http, self.git_service.as_mut().unwrap(), readonly);
    }
    fn database_url(&self) -> String {
        self.env
            .get("DATABASE_URL")
            .cloned()
            .or_else(|| {
                dotenvy::from_path_iter(self.root.join("drum-api/.env"))
                    .ok()?
                    .filter_map(Result::ok)
                    .find(|(key, _)| key == "DATABASE_URL")
                    .map(|(_, value)| value)
            })
            .expect("DATABASE_URL or drum-api/.env is required")
    }
    pub fn try_sql(&self, query: &str) -> Result<String, String> {
        let env = self.database_env();
        let output = run(
            command(
                "psql",
                &[
                    "-X",
                    "-qAt",
                    "-v",
                    "ON_ERROR_STOP=1",
                    "-v",
                    "VERBOSITY=sqlstate",
                    "-c",
                    query,
                ],
                &self.base,
                &env,
            ),
            "",
        );
        if output.status.success() {
            Ok(output.stdout.trim().into())
        } else {
            // Report the failure category without exposing a host, URI or password.
            let error = output.stderr.to_ascii_lowercase();
            let category = if error.contains("could not translate host name") {
                "fixture SQL: database hostname did not resolve"
            } else if error.contains("connection refused") {
                "fixture SQL: database connection refused"
            } else if error.contains("timeout") || error.contains("timed out") {
                "fixture SQL: database connection timed out"
            } else if error.contains("password authentication failed") {
                "fixture SQL: database authentication failed"
            } else {
                "fixture SQL failed (connection details suppressed)"
            };
            let code = output.stderr.split_whitespace().find(|word| {
                word.len() == 5
                    && word.starts_with(|c: char| c.is_ascii_digit())
                    && word
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            });
            Err(match code {
                Some(code) => format!("{category}; SQLSTATE {code}"),
                None => category.into(),
            })
        }
    }
    pub fn sql(&self, query: &str) -> String {
        self.try_sql(query).unwrap()
    }
    pub fn restore_sql(&self, query: String) -> SqlRestore {
        SqlRestore {
            env: self.database_env(),
            cwd: self.base.clone(),
            query,
        }
    }
    fn database_env(&self) -> Environment {
        let url = url::Url::parse(&self.database_url()).expect("invalid development database URL");
        let decode = |value: &str| {
            percent_encoding::percent_decode_str(value)
                .decode_utf8()
                .expect("invalid database URL encoding")
                .into_owned()
        };
        let mut env = self.env.clone();
        for (key, value) in [
            ("PGHOST", url.host_str().unwrap_or("").into()),
            ("PGPORT", url.port().unwrap_or(5432).to_string()),
            ("PGUSER", decode(url.username())),
            ("PGPASSWORD", decode(url.password().unwrap_or(""))),
            ("PGDATABASE", decode(url.path().trim_start_matches('/'))),
            ("PGCONNECT_TIMEOUT", "5".into()),
        ] {
            env.insert(key.into(), value);
        }
        for (key, value) in url.query_pairs() {
            let variable = match key.as_ref() {
                "sslmode" => "PGSSLMODE",
                "channel_binding" => "PGCHANNELBINDING",
                "sslrootcert" => "PGSSLROOTCERT",
                _ => continue,
            };
            env.insert(variable.into(), value.into_owned());
        }
        env
    }
    pub fn request(&self, route: &str, body: &Value) -> (u16, Value) {
        let response = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{}{route}", self.api))
            .json(body)
            .send()
            .unwrap();
        (response.status().as_u16(), response.json().unwrap())
    }
    pub fn project(&self, repo: &Path) -> String {
        git(&["config", "drum.project-id"], repo, &self.env)
    }
    pub fn init(&self, directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        git(&["init", "--initial-branch=main"], directory, &self.env);
        for (key, value) in [
            ("user.name", "Drum test"),
            ("user.email", "test@example.invalid"),
        ] {
            git(&["config", key, value], directory, &self.env);
        }
    }
    pub fn register(&self, repo: &Path) {
        cli(&["init", "--api-url", &self.api], repo, &self.env).success();
        self.projects.borrow_mut().push(self.project(repo));
    }
    pub fn commit(&self, repo: &Path, name: &str, bytes: &[u8]) {
        fs::write(repo.join(name), bytes).unwrap();
        git(&["add", name], repo, &self.env);
        git(&["commit", "-m", name], repo, &self.env);
    }
    /// Sharing and limits need a pushed repository, not a repeat of every smoke assertion.
    pub fn project_fixture(&self) -> PathBuf {
        let repo = self.base.join("project with spaces");
        self.init(&repo);
        self.commit(&repo, "hello.txt", b"hello Drum\n");
        self.register(&repo);
        cli(&["push"], &repo, &self.env).success();
        git(&["checkout", "-b", "test"], &repo, &self.env);
        self.commit(&repo, "branch.txt", b"on test branch\n");
        cli(&["push"], &repo, &self.env).success();
        repo
    }
    fn cleanup(&self, ids: &str) -> Result<(), String> {
        // Read the identities before cascading project deletion. Avoid temporary
        // tables: development connection poolers can reuse their server sessions.
        let owners = self.try_sql(&format!("SELECT owner_id FROM drum.projects WHERE id IN ({ids}) UNION SELECT identity_id FROM drum.project_writers WHERE project_id IN ({ids}) UNION SELECT claimed_by FROM drum.write_invitations WHERE project_id IN ({ids}) AND claimed_by IS NOT NULL"))?;
        let owners = owners
            .lines()
            .map(|id| uuid::Uuid::parse_str(id).map(|id| format!("'{id}'")))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid fixture identity ID".to_string())?;
        let owners = if owners.is_empty() {
            "NULL".into()
        } else {
            owners.join(",")
        };
        self.try_sql(&format!("BEGIN; DELETE FROM drum.project_challenges WHERE project_id IN ({ids}) OR public_key IN (SELECT public_key FROM drum.identities WHERE id IN ({owners})); DELETE FROM drum.projects WHERE id IN ({ids}); DELETE FROM drum.identities WHERE id IN ({owners}) AND NOT EXISTS (SELECT 1 FROM drum.projects WHERE owner_id=drum.identities.id); COMMIT;"))?;
        Ok(())
    }
    pub fn lifecycle(&mut self) -> PathBuf {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap();
        for origin in [&self.api, &self.http] {
            for route in ["/health", "/health/ready", "/health/live"] {
                let response = client.head(format!("{origin}{route}")).send().unwrap();
                assert_eq!(response.status(), 200);
                assert!(response.bytes().unwrap().is_empty());
            }
        }
        let fresh = self.base.join("fresh-project");
        fs::create_dir(&fresh).unwrap();
        fs::write(
            fresh.join("untracked.txt"),
            "developer decides what to commit\n",
        )
        .unwrap();
        cli(&["status"], &fresh, &self.env).failure();
        assert!(!fresh.join(".git").exists());
        self.register(&fresh);
        assert!(fresh.join(".git").is_dir());
        assert!(git(&["ls-files"], &fresh, &self.env).is_empty());
        cli(&["push"], &fresh, &self.env).failure();
        let fresh_id = self.project(&fresh);
        let nested = fresh.join("nested");
        fs::create_dir(&nested).unwrap();
        cli(&["init"], &nested, &self.env).success();
        assert!(!nested.join(".git").exists());
        assert_eq!(self.project(&nested), fresh_id);
        let repo = self.base.join("project with spaces");
        self.init(&repo);
        git(
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/existing.git",
            ],
            &repo,
            &self.env,
        );
        git(
            &["config", "branch.main.remote", "origin"],
            &repo,
            &self.env,
        );
        git(
            &["config", "branch.main.merge", "refs/heads/main"],
            &repo,
            &self.env,
        );
        self.commit(&repo, "hello.txt", b"hello Drum\n");
        self.register(&repo);
        let project = self.project(&repo);
        let identity = fs::read(self.config.join("id_ed25519")).unwrap();
        let remote = git(&["remote", "get-url", "drum"], &repo, &self.env);
        let parsed = url::Url::parse(&remote).unwrap();
        assert_eq!(parsed.scheme(), "ssh");
        assert_eq!(parsed.username(), "git");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(self.git_port));
        let short = parsed
            .path()
            .trim_start_matches('/')
            .strip_suffix(".git")
            .unwrap();
        assert_eq!(short.len(), 9);
        assert_eq!(&short[4..5], "-");
        assert!(
            short
                .chars()
                .filter(|c| *c != '-')
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
        assert!(!self.config.join("hosts/aliases.json").exists());
        for args in [
            vec![
                "remote",
                "set-url",
                "drum",
                &format!("git@drum:{short}.git"),
            ],
            vec![
                "config",
                "drum.remote-url",
                &format!("git@drum:{short}.git"),
            ],
        ] {
            git(&args, &repo, &self.env);
        }
        cli(&["init"], &repo, &self.env).success();
        assert_eq!(
            git(&["remote", "get-url", "drum"], &repo, &self.env),
            remote
        );
        assert_eq!(self.project(&repo), project);
        assert_eq!(fs::read(self.config.join("id_ed25519")).unwrap(), identity);
        assert_eq!(
            git(&["remote", "get-url", "origin"], &repo, &self.env),
            "https://example.invalid/existing.git"
        );
        assert_eq!(
            git(&["config", "branch.main.remote"], &repo, &self.env),
            "origin"
        );
        cli(&["push"], &repo, &self.env).success();
        git(&["push", "drum", "main"], &repo, &self.env);
        let marker = self.base.join("server-hook-executed");
        let generation = files(&self.base.join("data/generations"), "git")
            .into_iter()
            .next()
            .unwrap();
        let hooks = generation.join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        for hook in ["pre-receive", "post-receive", "update", "post-update"] {
            executable(
                &hooks.join(hook),
                &format!(
                    "#!/bin/sh\nprintf executed > {}\nexit 1\n",
                    shell_quote(path(&marker))
                ),
            );
        }
        git(&["checkout", "-b", "test"], &repo, &self.env);
        self.commit(&repo, "branch.txt", b"on test branch\n");
        cli(&["push"], &repo, &self.env).success();
        git(&["tag", "v0.1"], &repo, &self.env);
        git(&["push", "drum", "--tags"], &repo, &self.env);
        assert!(!marker.exists());
        let refs = git(&["ls-remote", "drum"], &repo, &self.env);
        run(
            command(
                "git",
                &[
                    "ls-remote",
                    &format!("ssh://git@127.0.0.1:{}/{project}.git", self.git_port),
                ],
                &repo,
                &self.env,
            ),
            "",
        )
        .failure();
        for name in ["refs/heads/main", "refs/heads/test", "refs/tags/v0.1"] {
            assert!(refs.contains(name));
        }
        let clone = self.base.join("clone");
        git(&["clone", &remote, path(&clone)], &self.base, &self.env);
        assert_eq!(fs::read(clone.join("hello.txt")).unwrap(), b"hello Drum\n");
        assert!(!clone.join("branch.txt").exists());
        let pinned = files(&self.config.join("hosts"), "known_hosts")
            .pop()
            .unwrap();
        let mut portable = self.env.clone();
        portable.insert("GIT_SSH_COMMAND".into(), format!("ssh -F /dev/null -i {} -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}", shell_quote(path(&self.config.join("id_ed25519"))), shell_quote(path(&pinned))));
        let portable_repo = self.base.join("clone-without-local-aliases");
        git(
            &["clone", &remote, path(&portable_repo)],
            &self.base,
            &portable,
        );
        assert_eq!(
            fs::read(portable_repo.join("hello.txt")).unwrap(),
            b"hello Drum\n"
        );
        git(&["checkout", "test"], &clone, &self.env);
        for (key, value) in [
            ("user.name", "Drum test"),
            ("user.email", "test@example.invalid"),
        ] {
            git(&["config", key, value], &clone, &self.env);
        }
        self.commit(&clone, "branch.txt", b"updated elsewhere\n");
        git(&["push", "origin", "test"], &clone, &self.env);
        cli(&["pull", "--trust"], &repo, &self.env).success();
        assert_eq!(
            fs::read(repo.join("branch.txt")).unwrap(),
            b"updated elsewhere\n"
        );
        git(&["reset", "--hard", "HEAD~1"], &clone, &self.env);
        self.commit(&clone, "diverged.txt", b"diverged\n");
        run(
            command("git", &["push", "origin", "test"], &clone, &self.env),
            "",
        )
        .failure();
        let public_key = fs::read_to_string(self.config.join("id_ed25519.pub")).unwrap();
        let mut body = json!({"public_key":public_key.trim(),"project_id":project,"name":"repeated","default_branch":"main"});
        let sign = |challenge: &Value, key: &Path| {
            run(
                command(
                    "ssh-keygen",
                    &["-Y", "sign", "-n", "drum-project-init-v1", "-f", path(key)],
                    &repo,
                    &self.env,
                ),
                challenge["message"].as_str().unwrap(),
            )
            .success()
        };
        let (status, challenge) = self.request("/v1/projects/challenges", &body);
        assert_eq!(status, 200);
        let claim = json!({"challenge_id":challenge["id"],"signature":sign(&challenge, &self.config.join("id_ed25519"))});
        assert_eq!(self.request("/v1/projects", &claim).0, 200);
        assert_eq!(self.request("/v1/projects", &claim).0, 401);
        let other = self.base.join("other");
        fs::create_dir(&other).unwrap();
        let key = other.join("key");
        git_key(&key, &repo, &self.env);
        let other_pub = fs::read_to_string(other.join("key.pub")).unwrap();
        body["public_key"] = json!(other_pub.trim());
        let (status, challenge) = self.request("/v1/projects/challenges", &body);
        assert_eq!(status, 200);
        assert_eq!(
            self.request(
                "/v1/projects",
                &json!({"challenge_id":challenge["id"],"signature":sign(&challenge, &key)})
            )
            .0,
            403
        );
        let other_id = uuid::Uuid::new_v4().to_string();
        self.projects.borrow_mut().push(other_id.clone());
        body["project_id"] = json!(other_id);
        let (status, challenge) = self.request("/v1/projects/challenges", &body);
        assert_eq!(status, 200);
        assert_eq!(
            self.request(
                "/v1/projects",
                &json!({"challenge_id":challenge["id"],"signature":sign(&challenge, &key)})
            )
            .0,
            200
        );
        portable.insert("GIT_SSH_COMMAND".into(), format!("ssh -F /dev/null -i {} -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}", shell_quote(path(&key)), shell_quote(path(&pinned))));
        run(
            command("git", &["ls-remote", &remote], &repo, &portable),
            "",
        )
        .failure();
        let host = Path::new(&self.env["DRUM_GIT_HOST_KEY_FILE"]);
        let original = fs::read(host).unwrap();
        fs::write(host, other_pub).unwrap();
        cli(&["init"], &repo, &self.env).failure();
        fs::write(host, original).unwrap();
        self.restart_git(false);
        assert!(git(&["ls-remote", "drum"], &repo, &self.env).contains("refs/heads/test"));
        repo
    }
}
pub struct SqlRestore {
    env: Environment,
    cwd: PathBuf,
    query: String,
}
impl Drop for SqlRestore {
    fn drop(&mut self) {
        let output = run(
            command(
                "psql",
                &["-X", "-qAt", "-v", "ON_ERROR_STOP=1", "-c", &self.query],
                &self.cwd,
                &self.env,
            ),
            "",
        );
        if !output.status.success() {
            if thread::panicking() {
                eprintln!("fixture allowance restoration failed");
            } else {
                panic!("fixture allowance restoration failed");
            }
        }
    }
}
fn git_key(key: &Path, cwd: &Path, env: &Environment) {
    run(
        command(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-f", path(key)],
            cwd,
            env,
        ),
        "",
    )
    .success();
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for mut process in [self.git_service.take(), self.api_service.take()]
            .into_iter()
            .flatten()
        {
            // Drain service tasks before deleting their database rows. The
            // Process guard still kills/reaps a child if shutdown fails.
            process.shutdown();
        }
        let ids = self
            .projects
            .borrow()
            .iter()
            .map(|id| format!("'{}'", uuid::Uuid::parse_str(id).unwrap()))
            .collect::<Vec<_>>()
            .join(",");
        if ids.is_empty() {
            return;
        }
        let result = self.cleanup(&ids);
        if let Err(error) = result {
            eprintln!("fixture projects requiring cleanup: {ids}");
            if thread::panicking() {
                eprintln!("development fixture cleanup failed: {error}");
            } else {
                panic!("development fixture cleanup failed: {error}");
            }
        }
    }
}
