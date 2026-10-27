//! Real API/SSH/HTTP integration. Explicitly opt in against development services.
#![cfg(unix)]
#[path = "support/limits.rs"]
mod limits;
#[path = "support/services.rs"]
mod services;
#[path = "support/sharing.rs"]
mod sharing;
mod support;
use services::Fixture;
use std::sync::Mutex;

// Quota fixtures temporarily change the database-wide allowance. Serialize this
// executable even when the caller omits --test-threads=1.
static SERVICES: Mutex<()> = Mutex::new(());

#[test]
#[ignore = "requires built sibling API/Git binaries, development Postgres/Redis and psql"]
fn git_lifecycle() {
    let _serial = SERVICES.lock().unwrap_or_else(|error| error.into_inner());
    Fixture::new().lifecycle();
}

#[test]
#[ignore = "requires built sibling API/Git binaries, development Postgres/Redis and psql"]
fn sharing_lifecycle() {
    let _serial = SERVICES.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let repo = fixture.project_fixture();
    sharing::check(&fixture, &repo);
}

#[test]
#[ignore = "requires exclusive development database; changes global budget and creates a 100 MB pack"]
fn resource_limits() {
    let _serial = SERVICES.lock().unwrap_or_else(|error| error.into_inner());
    let mut fixture = Fixture::new();
    let repo = fixture.project_fixture();
    limits::check(&mut fixture, &repo);
}
