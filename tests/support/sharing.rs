use super::services::Fixture;
use crate::support::*;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Duration};

fn invite(fixture: &Fixture, repo: &Path) -> (String, String) {
    let output = cli(&["invite"], repo, &fixture.env).success();
    let id = output
        .lines()
        .find_map(|line| line.strip_prefix("Invitation: "))
        .unwrap()
        .to_string();
    uuid::Uuid::parse_str(&id).unwrap();
    let link = output
        .lines()
        .find(|line| line.contains("#token="))
        .unwrap()
        .to_string();
    (id, link)
}
pub fn check(f: &Fixture, repo: &Path) {
    let project = f.project(repo);
    let short = git(&["config", "drum.short-id"], repo, &f.env);
    let url = format!("{}/{short}.git", f.http);
    let mut anonymous = f.env.clone();
    anonymous.insert(
        "DRUM_CONFIG_DIR".into(),
        path(&f.base.join("anonymous-identity")).into(),
    );
    run(command("git", &["ls-remote", &url], repo, &anonymous), "").failure();
    cli(&["invite"], repo, &f.env).failure();
    cli(&["publish"], repo, &f.env).failure();
    assert!(
        cli(&["publish", "--yes"], repo, &f.env)
            .success()
            .contains(&url)
    );
    let public = f.base.join("anonymous-clone");
    git(&["clone", &url, path(&public)], &f.base, &anonymous);
    assert!(public.join("hello.txt").exists());
    assert!(!Path::new(&anonymous["DRUM_CONFIG_DIR"]).exists());
    assert!(git(&["ls-remote", &url], repo, &anonymous).contains("refs/heads/test"));
    run(
        command("git", &["push", &url, "test"], repo, &anonymous),
        "",
    )
    .failure();
    let reader = f.base.join("drum-reader");
    cli(
        &["clone", "--trust", &url, path(&reader)],
        &f.base,
        &anonymous,
    )
    .success();
    assert_eq!(
        git(&["remote", "get-url", "drum"], &reader, &anonymous),
        url
    );
    assert!(
        !Path::new(&anonymous["DRUM_CONFIG_DIR"])
            .join("id_ed25519")
            .exists()
    );
    cli(&["push"], &reader, &anonymous).failure();
    let (invite_id, link) = invite(f, repo);
    let parsed = url::Url::parse(&link).unwrap();
    let token = parsed.fragment().unwrap().strip_prefix("token=").unwrap();
    assert_eq!(
        f.sql(&format!(
            "SELECT token_hash FROM drum.write_invitations WHERE id='{invite_id}'"
        )),
        format!("{:x}", Sha256::digest(token.as_bytes()))
    );
    let mut writer_env = f.env.clone();
    let writer_config = f.base.join("writer-identity");
    writer_env.insert("DRUM_CONFIG_DIR".into(), path(&writer_config).into());
    let writer = f.base.join("writer-clone");
    cli(
        &["clone", "--trust", &link, path(&writer)],
        &f.base,
        &writer_env,
    )
    .success();
    assert_eq!(
        git(&["remote", "get-url", "drum"], &writer, &writer_env),
        url
    );
    let saved = fs::read_to_string(writer.join(".git/config")).unwrap();
    assert!(!saved.contains(token) && !saved.contains("#token="));
    cli(
        &[
            "clone",
            "--trust",
            &link,
            path(&f.base.join("writer-retry")),
        ],
        &f.base,
        &writer_env,
    )
    .success();
    cli(
        &[
            "clone",
            "--trust",
            &link,
            path(&f.base.join("stolen-invite")),
        ],
        &f.base,
        &anonymous,
    )
    .failure();
    for args in [
        vec!["unpublish"],
        vec!["revoke", &invite_id],
        vec!["invite"],
    ] {
        cli(&args, &writer, &writer_env).failure();
    }
    for (key, value) in [
        ("user.name", "Writer test"),
        ("user.email", "writer@example.invalid"),
    ] {
        git(&["config", key, value], &writer, &writer_env);
    }
    git(&["checkout", "-b", "collaborator"], &writer, &writer_env);
    fs::write(writer.join("writer.txt"), "written by collaborator\n").unwrap();
    git(&["add", "."], &writer, &writer_env);
    git(
        &["commit", "-m", "collaborator change"],
        &writer,
        &writer_env,
    );
    cli(&["push"], &writer, &writer_env).success();
    let mut writer_ssh = writer_env.clone();
    let known = files(&f.config.join("hosts"), "known_hosts").pop().unwrap();
    writer_ssh.insert("GIT_SSH_COMMAND".into(), format!("ssh -F /dev/null -i {} -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}", shell_quote(path(&writer_config.join("id_ed25519"))), shell_quote(path(&known))));
    let ssh_url = git(&["remote", "get-url", "drum"], repo, &f.env);
    git(&["push", &ssh_url, "collaborator"], &writer, &writer_ssh);
    assert!(git(&["ls-remote", &url], repo, &anonymous).contains("refs/heads/collaborator"));
    let owner = f.sql(&format!(
        "SELECT owner_id FROM drum.projects WHERE id='{project}'"
    ));
    uuid::Uuid::parse_str(&owner).unwrap();
    let allowance: i64 = f
        .sql(&format!(
            "SELECT allowance_bytes FROM drum.identities WHERE id='{owner}'"
        ))
        .parse()
        .unwrap();
    let before = git(&["ls-remote", &url], repo, &anonymous);
    let mut bytes = vec![0; 16384];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    fs::write(writer.join("quota.bin"), bytes).unwrap();
    git(&["add", "."], &writer, &writer_env);
    git(&["commit", "-m", "quota change"], &writer, &writer_env);
    {
        let _restore = f.restore_sql(format!(
            "UPDATE drum.identities SET allowance_bytes={allowance} WHERE id='{owner}'"
        ));
        f.sql(&format!(
            "UPDATE drum.identities SET allowance_bytes=0 WHERE id='{owner}'"
        ));
        cli(&["push"], &writer, &writer_env).failure();
        assert_eq!(git(&["ls-remote", &url], repo, &anonymous), before);
    }
    git(&["push", "drum", "collaborator"], &writer, &writer_env);
    let output = run(
        command(
            CLI,
            &[
                "credential",
                "--api-url",
                &f.api,
                "--git-origin",
                "http://127.0.0.1:1",
                "--project",
                &short,
                "get",
            ],
            &writer,
            &writer_env,
        ),
        &format!("protocol=http\nhost=127.0.0.1:1\npath={short}.git\n\n"),
    );
    assert!(
        !output.status.success()
            && !output.stdout.contains("password=")
            && output.stdout.contains("quit=true")
    );
    let origin = url::Url::parse(&f.http).unwrap();
    let host = format!("127.0.0.1:{}", origin.port().unwrap());
    let credential = run(
        command(
            CLI,
            &[
                "credential",
                "--api-url",
                &f.api,
                "--git-origin",
                &f.http,
                "--project",
                &short,
                "get",
            ],
            &writer,
            &writer_env,
        ),
        &format!("protocol=http\nhost={host}\npath={short}.git\n\n"),
    )
    .success();
    let password = credential
        .lines()
        .find_map(|line| line.strip_prefix("password="))
        .unwrap();
    let authorization = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("drum:{password}"))
    );
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let cached_access = || {
        client
            .get(format!("{url}/info/refs?service=git-upload-pack"))
            .header("Authorization", &authorization)
            .send()
            .unwrap()
            .status()
            .as_u16()
    };
    assert_eq!(cached_access(), 200);
    let (unused_id, unused) = invite(f, repo);
    cli(&["unpublish"], repo, &f.env).success();
    run(command("git", &["ls-remote", &url], repo, &anonymous), "").failure();
    cli(&["pull"], &writer, &writer_env).success();
    cli(
        &[
            "clone",
            "--trust",
            &url,
            path(&f.base.join("private-clone")),
        ],
        &f.base,
        &writer_env,
    )
    .success();
    cli(
        &[
            "clone",
            "--trust",
            &unused,
            path(&f.base.join("unused-private")),
        ],
        &f.base,
        &anonymous,
    )
    .failure();
    cli(&["publish", "--yes"], repo, &f.env).success();
    cli(
        &[
            "clone",
            "--trust",
            &unused,
            path(&f.base.join("unused-public")),
        ],
        &f.base,
        &anonymous,
    )
    .failure();
    let listing = cli(&["invite", "--list"], repo, &f.env).success();
    assert!(
        listing.contains(&invite_id) && listing.contains(&unused_id) && !listing.contains(token)
    );
    cli(&["revoke", &invite_id], repo, &f.env).success();
    assert_eq!(cached_access(), 401);
    run(
        command(
            "git",
            &["push", &ssh_url, "collaborator"],
            &writer,
            &writer_ssh,
        ),
        "",
    )
    .failure();
    cli(&["push"], &writer, &writer_env).failure();
    assert!(git(&["ls-remote", "drum"], &writer, &writer_env).contains("refs/heads/collaborator"));
    cli(
        &[
            "clone",
            "--trust",
            &link,
            path(&f.base.join("revoked-retry")),
        ],
        &f.base,
        &writer_env,
    )
    .failure();
    let (expired_id, expired) = invite(f, repo);
    f.sql(&format!("UPDATE drum.write_invitations SET expires_at=now()-interval '1 second' WHERE id='{expired_id}'"));
    cli(
        &["clone", "--trust", &expired, path(&f.base.join("expired"))],
        &f.base,
        &anonymous,
    )
    .failure();
    cli(&["unpublish"], repo, &f.env).success();
    run(
        command("git", &["ls-remote", "drum"], &writer, &writer_env),
        "",
    )
    .failure();
    assert_eq!(
        client.get(format!("{url}/HEAD")).send().unwrap().status(),
        404
    );
}
