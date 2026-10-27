//! Optional daily release notices. Never involved in authentication or command success.
use crate::identity;
use crate::protocol::{CliRelease, api_origin};
use anyhow::Context;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DAY: u64 = 24 * 60 * 60;
const TIMEOUT: Duration = Duration::from_millis(750);
const INSTALL: &str = "curl -fsSL https://drum.dev/cli | sh";

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    checks: BTreeMap<String, Check>,
    #[serde(default)]
    last_notified_at: Option<u64>,
}
#[derive(Serialize, Deserialize)]
struct Check {
    checked_at: u64,
    latest_version: Option<String>,
}
fn due(timestamp: Option<u64>, now: u64) -> bool {
    timestamp.is_none_or(|timestamp| now < timestamp || now - timestamp >= DAY)
}
fn newer(current: &str, latest: &str) -> bool {
    if latest.len() > 128 {
        return false;
    }
    let (Ok(current), Ok(latest)) = (Version::parse(current), Version::parse(latest)) else {
        return false;
    };
    // Stable installations are never prompted to install a prerelease.
    (!current.pre.is_empty() || latest.pre.is_empty()) && latest.cmp_precedence(&current).is_gt()
}
fn fetch(api: &str) -> anyhow::Result<Option<String>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    // No identity, signature, token, project ID or command arguments are sent.
    let response = client
        .get(format!("{api}/v1/cli/version"))
        .send()?
        .error_for_status()?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::OK,
        "unexpected release response"
    );
    let mut bytes = Vec::new();
    response.take(4097).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 4096, "release response too large");
    let release: CliRelease = serde_json::from_slice(&bytes)?;
    Ok(release
        .latest_version
        .filter(|version| version.len() <= 128 && Version::parse(version).is_ok()))
}
/// Network checks are per API; notification throttling is global across APIs.
fn evaluate(
    cache: &mut Cache,
    api: &str,
    current: &str,
    now: u64,
    fetch: impl FnOnce() -> anyhow::Result<Option<String>>,
) -> (Option<String>, bool) {
    let mut changed = false;
    if due(cache.checks.get(api).map(|check| check.checked_at), now) {
        // Failed/unsupported/offline checks also spend today's attempt, without a notice.
        let latest = fetch().ok().flatten();
        cache.checks.insert(
            api.into(),
            Check {
                checked_at: now,
                latest_version: latest,
            },
        );
        while cache.checks.len() > 16 {
            let oldest = cache
                .checks
                .iter()
                .filter(|(key, _)| key.as_str() != api)
                .min_by_key(|(_, check)| check.checked_at)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                cache.checks.remove(&key);
            } else {
                break;
            }
        }
        changed = true;
    }
    let latest = cache
        .checks
        .get(api)
        .and_then(|check| check.latest_version.as_deref());
    if let Some(latest) = latest.filter(|latest| newer(current, latest))
        && due(cache.last_notified_at, now)
    {
        let latest = Version::parse(latest).expect("validated semantic version");
        cache.last_notified_at = Some(now);
        return (
            Some(format!(
                "A newer Drum version is available: {current} → {latest}\nUpdate: {INSTALL}"
            )),
            true,
        );
    }
    (None, changed)
}
fn check(api: Option<&str>) -> anyhow::Result<Option<String>> {
    let api =
        api_origin(api.unwrap_or(crate::protocol::DEFAULT_API_URL)).map_err(anyhow::Error::msg)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let directory = identity::directory()?;
    identity::private_dir(&directory)?;
    let Some(_guard) = identity::try_lock(&directory.join("updates.lock"))? else {
        return Ok(None);
    };
    let path = directory.join("updates.json");
    let mut cache = identity::read(&path)?
        .and_then(|data| serde_json::from_str::<Cache>(&data).ok())
        .unwrap_or_default();
    let (notice, changed) = evaluate(&mut cache, &api, env!("CARGO_PKG_VERSION"), now, || {
        fetch(&api)
    });
    if changed {
        identity::write(&path, &serde_json::to_string(&cache)?).context("saving release check")?;
    }
    Ok(notice)
}
pub fn notify(api: Option<&str>) {
    // Cache, permission, network, parse and output errors are all deliberately silent.
    if let Ok(Some(notice)) = check(api) {
        let _ = writeln!(std::io::stderr().lock(), "\n{notice}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_versions_ignore_build_metadata_and_stable_ignores_prereleases() {
        assert!(newer("0.9.0", "0.10.0"));
        assert!(!newer("0.2.0", "0.1.0"));
        assert!(!newer("1.0.0+local", "1.0.0+release"));
        assert!(!newer("1.0.0", "2.0.0-rc.1"));
        assert!(newer("2.0.0-rc.1", "2.0.0"));
        assert!(!newer("1.0.0", "2.0.0\u{1b}[31m"));
    }
    #[test]
    fn checking_and_notifying_have_separate_daily_timestamps() {
        let mut cache = Cache::default();
        let now = 1_000_000;
        let (notice, changed) = evaluate(&mut cache, "https://api.drum.dev", "0.1.0", now, || {
            Ok(Some("0.2.0".into()))
        });
        assert!(changed && notice.unwrap().contains(INSTALL));
        assert!(
            evaluate(
                &mut cache,
                "https://api.drum.dev",
                "0.1.0",
                now + 1,
                || panic!("cached check made a request")
            )
            .0
            .is_none()
        );
        // Another API may be checked today, but cannot cause a second notice.
        assert!(
            evaluate(
                &mut cache,
                "http://localhost:3000",
                "0.1.0",
                now + 1,
                || Ok(Some("0.3.0".into()))
            )
            .0
            .is_none()
        );
        cache.last_notified_at = None;
        assert!(
            evaluate(
                &mut cache,
                "https://api.drum.dev",
                "0.1.0",
                now + 2,
                || panic!("cached check made a request")
            )
            .0
            .is_some()
        );
        assert!(
            evaluate(
                &mut cache,
                "https://api.drum.dev",
                "0.1.0",
                now + DAY + 2,
                || Ok(Some("0.2.0".into()))
            )
            .0
            .is_some()
        );
        assert!(
            evaluate(
                &mut cache,
                "https://api.drum.dev",
                "0.2.0",
                now + DAY * 2 + 3,
                || Ok(Some("0.2.0".into()))
            )
            .0
            .is_none()
        );
    }
    #[test]
    fn offline_failure_is_quiet_and_backed_off_even_with_an_old_newer_release() {
        let mut cache = Cache::default();
        let now = 1_000_000;
        evaluate(&mut cache, "https://api.drum.dev", "0.1.0", now, || {
            Ok(Some("0.2.0".into()))
        });
        let (notice, changed) = evaluate(
            &mut cache,
            "https://api.drum.dev",
            "0.1.0",
            now + DAY,
            || anyhow::bail!("offline"),
        );
        assert!(changed && notice.is_none());
        assert!(
            evaluate(
                &mut cache,
                "https://api.drum.dev",
                "0.1.0",
                now + DAY + 1,
                || panic!("failed check was not cached")
            )
            .0
            .is_none()
        );
        assert!(due(Some(now + DAY), now)); // Recover from a local clock rollback.
    }
}
