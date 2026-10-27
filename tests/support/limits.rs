use super::services::Fixture;
use crate::support::*;
use std::{fs, path::Path, thread};

fn random(size: usize) -> Vec<u8> {
    let mut bytes = vec![0; size];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    bytes
}
pub fn check(f: &mut Fixture, repo: &Path) {
    let project = f.project(repo);
    let owner = f.sql(&format!(
        "SELECT owner_id FROM drum.projects WHERE id='{project}'"
    ));
    uuid::Uuid::parse_str(&owner).unwrap();
    let saved_allowance: i64 = f
        .sql(&format!(
            "SELECT allowance_bytes FROM drum.identities WHERE id='{owner}'"
        ))
        .parse()
        .unwrap();
    let saved_global: i64 = f
        .sql("SELECT allowance_bytes FROM drum.storage_budget WHERE singleton")
        .parse()
        .unwrap();
    let _restore = f.restore_sql(format!("BEGIN; UPDATE drum.identities SET allowance_bytes={saved_allowance} WHERE id='{owner}'; UPDATE drum.storage_budget SET allowance_bytes={saved_global} WHERE singleton; COMMIT;"));
    let before = git(&["ls-remote", "drum"], repo, &f.env);
    let usage: i64 = f
        .sql(&format!(
            "SELECT sum(used_bytes) FROM drum.projects WHERE owner_id='{owner}'"
        ))
        .parse()
        .unwrap();
    assert!(usage > 0);
    f.sql(&format!(
        "UPDATE drum.identities SET allowance_bytes=0 WHERE id='{owner}'"
    ));
    f.commit(repo, "over-quota.bin", &random(16384));
    cli(&["push"], repo, &f.env).failure();
    assert_eq!(git(&["ls-remote", "drum"], repo, &f.env), before);
    f.sql(&format!(
        "UPDATE drum.identities SET allowance_bytes={saved_allowance} WHERE id='{owner}'"
    ));
    cli(&["push"], repo, &f.env).success();
    let total: i64 = f
        .sql("SELECT sum(used_bytes) FROM drum.projects")
        .parse()
        .unwrap();
    f.sql(&format!(
        "UPDATE drum.storage_budget SET allowance_bytes={total} WHERE singleton"
    ));
    let before = git(&["ls-remote", "drum"], repo, &f.env);
    f.commit(repo, "global-quota.bin", &random(16384));
    cli(&["push"], repo, &f.env).failure();
    assert_eq!(git(&["ls-remote", "drum"], repo, &f.env), before);
    f.sql(&format!(
        "UPDATE drum.storage_budget SET allowance_bytes={saved_global} WHERE singleton"
    ));
    cli(&["push"], repo, &f.env).success();
    let mut competitors = Vec::new();
    for index in 0..2 {
        let directory = f.base.join(format!("concurrent-{index}"));
        f.init(&directory);
        f.commit(&directory, "payload.bin", &random(200000));
        f.register(&directory);
        competitors.push(directory);
    }
    let usage: i64 = f
        .sql(&format!(
            "SELECT sum(used_bytes) FROM drum.projects WHERE owner_id='{owner}'"
        ))
        .parse()
        .unwrap();
    f.sql(&format!(
        "UPDATE drum.identities SET allowance_bytes={} WHERE id='{owner}'",
        usage + 350000
    ));
    let results = thread::scope(|scope| {
        let env = &f.env;
        let handles: Vec<_> = competitors
            .iter()
            .map(|directory| scope.spawn(move || cli(&["push"], directory, env).status.success()))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|ok| **ok).count(),
        1,
        "concurrent quota admission"
    );
    let actual: i64 = f
        .sql(&format!(
            "SELECT sum(used_bytes) FROM drum.projects WHERE owner_id='{owner}'"
        ))
        .parse()
        .unwrap();
    assert!(actual <= usage + 350000);
    f.sql(&format!(
        "UPDATE drum.identities SET allowance_bytes={saved_allowance} WHERE id='{owner}'"
    ));
    let before = git(&["ls-remote", "drum"], repo, &f.env);
    let usage_query = format!(
        "SELECT used_bytes || ':' || storage_generation FROM drum.projects WHERE id='{project}'"
    );
    let before_usage = f.sql(&usage_query);
    f.commit(repo, "oversized.bin", &random(100000001));
    cli(&["push"], repo, &f.env).failure();
    assert_eq!(git(&["ls-remote", "drum"], repo, &f.env), before);
    assert_eq!(f.sql(&usage_query), before_usage);
    git(&["reset", "--hard", "HEAD~1"], repo, &f.env);
    let count: usize = f
        .sql(&format!(
            "SELECT count(*) FROM drum.projects WHERE owner_id='{owner}'"
        ))
        .parse()
        .unwrap();
    for _ in count..20 {
        let id = uuid::Uuid::new_v4();
        let hex = id.simple().to_string();
        let short = format!("{}-{}", &hex[..4], &hex[4..8]);
        f.projects.borrow_mut().push(id.to_string());
        f.sql(&format!("INSERT INTO drum.projects(id,owner_id,name,default_branch,short_id) VALUES ('{id}','{owner}','quota fixture','main','{short}')"));
    }
    let rejected = f.base.join("twenty-first-project");
    fs::create_dir(&rejected).unwrap();
    cli(&["init", "--api-url", &f.api], &rejected, &f.env).failure();
    assert_eq!(
        f.sql(&format!(
            "SELECT count(*) FROM drum.projects WHERE owner_id='{owner}'"
        )),
        "20"
    );
    cli(&["init"], repo, &f.env).success();
    f.git_service.take().unwrap().stop();
    let generation = f.sql(&format!(
        "SELECT storage_generation FROM drum.projects WHERE id='{project}'"
    ));
    uuid::Uuid::parse_str(&generation).unwrap();
    fs::rename(
        f.base.join(format!("data/generations/{generation}.git")),
        f.base.join(format!("data/repos/{project}.git")),
    )
    .unwrap();
    f.sql(&format!(
        "UPDATE drum.projects SET storage_generation=NULL,used_bytes=0 WHERE id='{project}'"
    ));
    let orphan = f
        .base
        .join(format!("data/generations/{}.git", uuid::Uuid::new_v4()));
    fs::create_dir(&orphan).unwrap();
    fs::write(orphan.join("unused"), "abandoned generation").unwrap();
    f.restart_git(true);
    assert_eq!(git(&["ls-remote", "drum"], repo, &f.env), before);
    assert!(!orphan.exists());
    let usage: i64 = f
        .sql(&format!(
            "SELECT used_bytes FROM drum.projects WHERE id='{project}'"
        ))
        .parse()
        .unwrap();
    assert!(usage > 0);
    cli(&["push"], repo, &f.env).failure();
}
