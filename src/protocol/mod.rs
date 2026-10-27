//! Local API wire definitions. Keep signed-message formats compatible with other Drum services.
use serde::{Deserialize, Serialize};

pub const DEFAULT_API_URL: &str = "https://api.drum.dev";

/// Only HTTPS is allowed outside loopback; credentials must never enter a URL.
pub fn api_origin(value: &str) -> Result<String, &'static str> {
    let url = url::Url::parse(value).map_err(|_| "API URL must be an absolute HTTP(S) origin")?;
    let host = url.host_str().ok_or("API URL needs a host")?;
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "API URL must use HTTPS (HTTP is allowed on loopback), with no credentials, path, query or fragment",
        );
    }
    Ok(url.origin().ascii_serialization())
}

/// Latest published CLI release. None means update advertising is not configured.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CliRelease {
    pub latest_version: Option<String>,
}

pub mod git;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_api_origins_reject_credentials_and_remote_http() {
        assert_eq!(
            api_origin("http://localhost:3000/").unwrap(),
            "http://localhost:3000"
        );
        assert_eq!(
            api_origin("https://api.drum.dev:443").unwrap(),
            "https://api.drum.dev"
        );
        assert!(api_origin("http://[::1]:3000").is_ok());
        for invalid in [
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com/path",
            "https://example.com/?secret=x",
        ] {
            assert!(api_origin(invalid).is_err());
        }
    }
}
