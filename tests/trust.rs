#![cfg(unix)]
mod support;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
use support::*;

#[test]
fn trust_is_required_before_claims_and_transfers_and_tracks_source_changes() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let server = Server::start(move |request, origin| {
        recorded
            .lock()
            .unwrap()
            .push((request.method.clone(), request.path.clone()));
        if request.method == "POST" {
            return Response::json(403, json!({"error":{"message":"fixture refuses claims"}}));
        }
        Response::json(
            200,
            if request.path == "/drum/config" {
                json!({"api_url":origin})
            } else {
                json!({"project":{"id":"00000000-0000-0000-0000-000000000001","short_id":"abcd-1234","name":"fixture","ssh_url":"ssh://git@localhost:2222/abcd-1234.git","host_public_key":""},"clone_url":format!("{origin}/abcd-1234.git")})
            },
        )
    });
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let mut env = environment(base);
    let config = base.join("config");
    let bin = base.join("bin");
    let log = base.join("transfers");
    fs::create_dir(&bin).unwrap();
    let real_git = run(command("sh", &["-c", "command -v git"], base, &env), "").success();
    // Intercept transfers only; configuration still uses real Git. No extra runtime.
    executable(
        &bin.join("git"),
        r#"#!/bin/sh
for arg do
    case "$arg" in
        clone|pull)
            printf '%s\n' "$arg" >> "$TRUST_GIT_LOG"
            if [ "$arg" = clone ]; then
                previous= last=
                for value do previous=$last; last=$value; done
                "$TRUST_REAL_GIT" init --initial-branch=main "$last" >/dev/null 2>&1 || exit 1
                "$TRUST_REAL_GIT" -C "$last" remote add origin "$previous" || exit 1
            fi
            exit 0 ;;
    esac
done
exec "$TRUST_REAL_GIT" "$@"
"#,
    );
    env.insert("TRUST_REAL_GIT".into(), real_git.clone());
    env.insert("TRUST_GIT_LOG".into(), path(&log).into());
    env.insert("PATH".into(), format!("{}:{}", bin.display(), env["PATH"]));
    let transfers = || fs::read_to_string(&log).unwrap_or_default().lines().count();
    let expected = |method: &str, route: &str| vec![(method.to_string(), route.to_string())];
    let origin = server.origin.as_str();
    let remote = format!("{origin}/abcd-1234.git");
    for op in ["clone", "pull"] {
        assert!(
            cli(&[op, "--help"], base, &env)
                .success()
                .contains("--trust")
        );
    }
    let clone = ["clone", &remote, "repo", "--api-url", origin];
    let output = cli(&clone, base, &env);
    assert!(!output.status.success() && output.text().contains("--trust"));
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(transfers(), 0);
    git(&["init", "--initial-branch=main", "parent"], base, &env);
    git(
        &["config", "drum.trusted", "true"],
        &base.join("parent"),
        &env,
    );
    cli(&clone, &base.join("parent"), &env).failure();
    assert_eq!(transfers(), 0);
    cli(&["clone", &remote, "repo"], base, &env).failure();
    assert_eq!(*calls.lock().unwrap(), expected("GET", "/drum/config"));
    calls.lock().unwrap().clear();
    let token = "a".repeat(64);
    let link = format!("{remote}#token={token}");
    let invite = ["clone", &link, "repo", "--api-url", origin];
    for answer in ["n\n", "\n", "\x04"] {
        let output = terminal(command(CLI, &invite, base, &env), Some(answer), false);
        assert!(!output.status.success() && output.text().contains("cancelled"));
        assert!(!output.text().contains(&token));
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(transfers(), 0);
        assert!(!base.join("repo").exists() && !config.join("id_ed25519").exists());
        assert!(files(&config.join("trust-v1"), "json").is_empty());
    }
    let output = terminal(command(CLI, &clone, base, &env), Some("y\n"), false);
    assert!(
        output.status.success()
            && output.text().contains(&remote)
            && output.text().contains(origin)
    );
    assert_eq!(
        *calls.lock().unwrap(),
        expected("GET", "/abcd-1234.git/drum")
    );
    assert_eq!(transfers(), 1);
    assert!(!config.join("id_ed25519").exists());
    let records = files(&config.join("trust-v1"), "json");
    assert_eq!(records.len(), 1);
    assert_eq!(
        fs::metadata(&records[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let repo = base.join("repo");
    let output = cli(&["pull", "--", "--ff-only"], &repo, &env);
    assert!(output.status.success() && !output.text().contains("Trust this"));
    assert_eq!(transfers(), 2);
    cli(
        &["clone", &remote, "another", "--api-url", origin],
        base,
        &env,
    )
    .success();
    assert_eq!(transfers(), 3);
    git(
        &["config", "drum.api-url", "https://changed.example"],
        &repo,
        &env,
    );
    cli(&["pull"], &repo, &env).failure();
    assert_eq!(transfers(), 3);
    cli(&["pull", "--trust"], &repo, &env).success();
    assert_eq!(transfers(), 4);
    git(&["config", "drum.api-url", origin], &repo, &env);
    git(
        &[
            "remote",
            "set-url",
            "drum",
            "https://other.example/abcd-1234.git",
        ],
        &repo,
        &env,
    );
    cli(&["pull"], &repo, &env).failure();
    assert_eq!(transfers(), 4);
    git(&["remote", "set-url", "drum", &remote], &repo, &env);
    git(
        &[
            "config",
            "url.https://rewrite.example/.insteadOf",
            &format!("{origin}/"),
        ],
        &repo,
        &env,
    );
    let output = terminal(command(CLI, &["pull"], &repo, &env), Some("no\n"), false);
    assert!(!output.status.success() && output.text().contains("rewrite.example"));
    assert_eq!(transfers(), 4);
    env.insert(
        "DRUM_CONFIG_DIR".into(),
        path(&base.join("fresh-config")).into(),
    );
    calls.lock().unwrap().clear();
    cli(
        &["clone", &remote, "explicit", "--api-url", origin, "--trust"],
        base,
        &env,
    )
    .success();
    assert_eq!(transfers(), 5);
    env.insert(
        "DRUM_CONFIG_DIR".into(),
        path(&base.join("invite-config")).into(),
    );
    calls.lock().unwrap().clear();
    let output = cli(
        &["clone", &link, "repo", "--api-url", origin, "--trust"],
        base,
        &env,
    );
    assert!(!output.status.success() && !output.text().contains(&token));
    assert_eq!(
        *calls.lock().unwrap(),
        expected("POST", "/v1/sharing/challenges")
    );
    assert_eq!(transfers(), 5);
    for record in files(&base.join("invite-config/trust-v1"), "json") {
        assert!(!fs::read_to_string(record).unwrap().contains(&token));
    }
}
