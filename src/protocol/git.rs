//! Wire types for key-based project initialization. No private keys cross the API.
use serde::{Deserialize, Serialize};

pub const SIGNING_NAMESPACE: &str = "drum-project-init-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectChallengeRequest {
    pub public_key: String,
    pub project_id: String,
    pub name: String,
    pub default_branch: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectChallenge {
    pub id: String,
    pub message: String,
    pub expires_in: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    pub challenge_id: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub short_id: String,
    pub ssh_url: String,
    pub host_public_key: String,
}

/// Public project path: eight lowercase letters/digits grouped for readability.
pub fn valid_short_id(value: &str) -> bool {
    value.len() == 9
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 4 {
                byte == b'-'
            } else {
                byte.is_ascii_lowercase() || byte.is_ascii_digit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_ids_have_one_unambiguous_safe_path_format() {
        for value in ["abcd-1234", "0000-0000", "a1b2-c3d4"] {
            assert!(valid_short_id(value));
        }
        for value in [
            "abcd1234",
            "ABCD-1234",
            "abcd-12345",
            "abc-12345",
            "abcd-../x",
            "abcd-123\n",
        ] {
            assert!(!valid_short_id(value));
        }
    }
}

pub const SHARING_NAMESPACE: &str = "drum-sharing-v1";
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingAction {
    Inspect,
    Publish,
    Unpublish,
    Invite,
    ListInvites,
    RevokeInvite,
    Claim,
    Access,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingIntent {
    pub project: String,
    pub action: SharingAction,
    pub token_hash: Option<String>,
    pub invite_id: Option<String>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingRequest {
    pub public_key: String,
    pub intent: SharingIntent,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharingProof {
    pub challenge_id: String,
    pub signature: String,
    pub token: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct GitDiscovery {
    pub api_url: String,
}
#[derive(Serialize, Deserialize)]
pub struct PublicProject {
    pub project: Project,
    pub clone_url: String,
}
#[derive(Serialize, Deserialize)]
pub struct Invitation {
    pub id: String,
    pub expires_at: String,
    pub claimed_by: Option<String>,
    pub revoked: bool,
    pub active: bool,
}
// Deliberately no Debug: these responses can contain an invitation or short-lived credential.
#[derive(Serialize, Deserialize)]
pub struct SharingResult {
    pub project: Project,
    pub clone_url: String,
    pub is_public: bool,
    pub role: String,
    pub identity_id: String,
    pub credential: Option<String>,
    pub write_url: Option<String>,
    pub invitation: Option<Invitation>,
    pub invitations: Vec<Invitation>,
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn signed_initialization_matches_v1_wire_fixture() {
        let request = ProjectChallengeRequest {
            public_key: "ssh-ed25519 fixture-key".into(),
            project_id: "00000000-0000-0000-0000-000000000001".into(),
            name: "sample".into(),
            default_branch: "main".into(),
        };
        assert_eq!(
            serde_json::to_string(&(
                SIGNING_NAMESPACE,
                "https://api.drum.dev",
                "fixture-nonce",
                &request
            ))
            .unwrap(),
            r#"["drum-project-init-v1","https://api.drum.dev","fixture-nonce",{"public_key":"ssh-ed25519 fixture-key","project_id":"00000000-0000-0000-0000-000000000001","name":"sample","default_branch":"main"}]"#
        );
    }

    #[test]
    fn signed_sharing_matches_v1_wire_fixture() {
        let request = SharingRequest {
            public_key: "ssh-ed25519 fixture-key".into(),
            intent: SharingIntent {
                project: "abcd-1234".into(),
                action: SharingAction::Publish,
                token_hash: None,
                invite_id: None,
            },
        };
        assert_eq!(
            serde_json::to_string(&(
                SHARING_NAMESPACE,
                "https://git.drum.dev",
                "fixture-nonce",
                &request
            ))
            .unwrap(),
            r#"["drum-sharing-v1","https://git.drum.dev","fixture-nonce",{"public_key":"ssh-ed25519 fixture-key","intent":{"project":"abcd-1234","action":"publish","token_hash":null,"invite_id":null}}]"#
        );
    }
}
