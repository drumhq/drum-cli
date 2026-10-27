#![cfg(unix)]
mod support;
use serde_json::{Value, json};
use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::*;

#[derive(Clone)]
struct Release {
    calls: usize,
    version: Value,
    status: u16,
    delay: Duration,
    oversize: bool,
}
impl Default for Release {
    fn default() -> Self {
        Self {
            calls: 0,
            version: json!("999.0.0"),
            status: 200,
            delay: Duration::ZERO,
            oversize: false,
        }
    }
}

#[test]
fn interactive_notices_preserve_output_exit_codes_and_daily_network_limits() {
    let state = Arc::new(Mutex::new(Release::default()));
    let received = state.clone();
    let server = Server::start(move |request, _| {
        assert_eq!(request.path, "/v1/cli/version");
        assert!(
            !request
                .headers
                .to_ascii_lowercase()
                .contains("authorization:")
        );
        let mut state = received.lock().unwrap();
        state.calls += 1;
        let state = state.clone();
        Response {
            status: state.status,
            body: if state.oversize {
                "x".repeat(5000)
            } else {
                json!({"latest_version":state.version}).to_string()
            },
            delay: state.delay,
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let repo = base.join("repo");
    fs::create_dir(&repo).unwrap();
    let config = base.join("config");
    let mut env = environment(base);
    env.remove("DRUM_NO_UPDATE_CHECK");
    git(&["init", "--initial-branch=main"], &repo, &env);
    for (key, value) in [
        ("drum.api-url", server.origin.as_str()),
        ("drum.project-name", "fixture-project"),
        ("remote.drum.url", "https://example.invalid/abcd-1234.git"),
    ] {
        git(&["config", key, value], &repo, &env);
    }
    let invoke = |args: &[&str], env: &Environment, combined| {
        terminal(command(CLI, args, &repo, env), None, combined)
    };
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec![
            "credential",
            "--api-url",
            &server.origin,
            "--git-origin",
            &server.origin,
            "--project",
            "abcd-1234",
            "store",
        ],
    ] {
        let output = invoke(&args, &env, false);
        assert!(output.status.success() && !output.stderr.contains("newer Drum"));
    }
    cli(&["status"], &repo, &env).success();
    let mut opted_out = env.clone();
    opted_out.insert("DRUM_NO_UPDATE_CHECK".into(), "1".into());
    invoke(&["status"], &opted_out, false);
    assert_eq!(state.lock().unwrap().calls, 0);
    assert!(!config.exists());
    let install = "curl -fsSL https://drum.dev/cli | sh";
    let output = invoke(&["status"], &env, false);
    assert!(output.status.success() && output.stdout.contains("Project: fixture-project"));
    assert!(!output.stdout.contains("newer Drum") && output.stderr.contains(install));
    assert!(
        output
            .stderr
            .contains(&format!("{} → 999.0.0", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(state.lock().unwrap().calls, 1);
    assert!(!config.join("id_ed25519").exists());
    let cache_path = config.join("updates.json");
    let mut cache = json(&cache_path);
    assert!(
        cache["checks"][&server.origin]["checked_at"]
            .as_u64()
            .unwrap()
            > 0
            && cache["last_notified_at"].is_u64()
    );
    let output = invoke(&["status"], &env, false);
    assert!(output.status.success() && output.stderr.trim().is_empty());
    assert_eq!(state.lock().unwrap().calls, 1);
    cache["checks"][&server.origin]["checked_at"] = json!(0);
    write_json(&cache_path, &cache);
    assert!(invoke(&["status"], &env, false).stderr.trim().is_empty());
    assert_eq!(state.lock().unwrap().calls, 2);
    cache = json(&cache_path);
    cache["last_notified_at"] = Value::Null;
    write_json(&cache_path, &cache);
    assert!(invoke(&["status"], &env, false).stderr.contains(install));
    assert_eq!(state.lock().unwrap().calls, 2);
    cache = json(&cache_path);
    cache["last_notified_at"] = Value::Null;
    write_json(&cache_path, &cache);
    let output = invoke(&["status"], &env, true);
    assert!(output.status.success());
    assert!(output.stderr.find("Identity:").unwrap() < output.stderr.find("A newer Drum").unwrap());
    let mut cases = Vec::new();
    for version in [
        json!(env!("CARGO_PKG_VERSION")),
        json!("0.0.0"),
        Value::Null,
        json!("999.0.0-rc.1"),
        json!("999.0.0\u{1b}[31m"),
    ] {
        cases.push(Release {
            version,
            ..Release::default()
        });
    }
    for status in [404, 503, 302] {
        cases.push(Release {
            status,
            ..Release::default()
        });
    }
    cases.push(Release {
        oversize: true,
        ..Release::default()
    });
    cases.push(Release {
        delay: Duration::from_secs(2),
        ..Release::default()
    });
    for case in cases {
        let before = state.lock().unwrap().calls;
        *state.lock().unwrap() = Release {
            calls: before,
            ..case
        };
        write_json(&cache_path, &json!({"checks":{},"last_notified_at":null}));
        let output = invoke(&["status"], &env, false);
        assert!(output.status.success() && output.stderr.trim().is_empty());
        assert!(output.elapsed < Duration::from_secs(2));
        assert_eq!(state.lock().unwrap().calls, before + 1);
        assert!(invoke(&["status"], &env, false).stderr.trim().is_empty());
        assert_eq!(state.lock().unwrap().calls, before + 1);
    }
    write_json(&cache_path, &json!({"checks":{},"last_notified_at":null}));
    let before = state.lock().unwrap().calls;
    let output = invoke(&["revoke", "invalid-id"], &env, false);
    assert!(!output.status.success() && !output.stderr.contains("newer Drum"));
    assert_eq!(state.lock().unwrap().calls, before);
}
