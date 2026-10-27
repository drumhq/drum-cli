# Development reference

## Local setup

Requires Rust, Git, and OpenSSH (`ssh` on PATH). Key storage currently supports
Unix/macOS/Linux. Run the sibling `drum-api` and `drum-git` services against the
same Postgres database. The API applies migrations; see their READMEs.

```sh
# In drum-cli:
cargo build --locked
# Or install the binary on PATH:
cargo install --path . --locked
```

With API listening at localhost:3000 and Git SSH at localhost:2222:

```sh
cd /path/to/existing/git/project
drum init --api-url http://localhost:3000
drum status
drum push

git switch -c test
# Edit, git add, and git commit as usual.
drum push                       # Pushes local test to Drum's test branch.
# Alternatively:
git push drum test

drum pull                       # Fetches and integrates Drum's current branch.
```

`drum init --api-url http://localhost:3000 --name my-project` overrides the API
and display name. `DRUM_API_URL` also supplies the API during init. Repeated init
reuses the saved API, project ID, and identity. Run init from any subdirectory of
the repository. Outside Git, init first runs `git init` in the current directory,
respecting Git's configured default branch. Files remain untracked; no automatic
commits are made. A conflicting existing `drum` remote is rejected.

`drum push [branch]` and `drum pull [branch]` run Git with the explicit `drum`
remote and the supplied branch, or your current branch. They inherit Git's output
and exit code. With detached HEAD, specify a branch or switch to one. Additional
Git options go after `--`, for example `drum push -- --dry-run` or
`drum pull -- --ff-only`. No commits, force pushes, or upstream changes are
performed automatically. Git handles divergence and merge conflicts normally.

Standard Git works too:

```sh
git push drum main
git pull drum main
git push drum --tags
git clone <remote-url-printed-by-drum-init> another-directory
```

Existing `origin`, branch tracking, and plain `git push`/`git pull` defaults are
preserved. Only committed Git history is transferred. Untracked/uncommitted files
stay local. Branches and tags are standard Git refs.

## Identity and SSH configuration

By default keys are `~/.config/drum/id_ed25519` and `id_ed25519.pub`.
`XDG_CONFIG_HOME` is respected; `DRUM_CONFIG_DIR` overrides the whole directory
and must be an absolute path. The private directory is mode 0700 and the private
key is mode 0600. Existing keys are reused, never silently replaced.

The CLI signs a one-use, five-minute API challenge bound to the project ID,
name, default branch, and public key. The private key never leaves your machine.
The API requires HTTPS outside loopback and redirects are disabled.

Init configures SSH for the real Git server hostname, the explicit port, and user
`git`. There are no invented hostname aliases. Standard-port addresses look like
`git@git.drum.dev:k7m2-x9p4.git`; local/nonstandard ports are encoded in the URL,
for example `ssh://git@localhost:2222/k7m2-x9p4.git`. IPv6 uses bracketed SSH URLs.

The CLI selects the Drum identity and pins the server key returned by the API in
standard OpenSSH known-host format. It prepends an `Include` to `~/.ssh/config`,
preserving existing contents. The included configuration uses `Match originalhost`,
`user git`, and a fixed local `/bin/test %p -eq <port>` condition to apply only to
that endpoint. Other hosts, users, and ports keep their existing settings. This
condition contains only a numeric port; it does not run project or server code.
Git's `core.sshCommand` is not changed. Configuration and pins live under the Drum
config directory, keyed by the real host and port. A changed host key is rejected;
verify a legitimate rotation before updating that endpoint's pin. OpenSSH with
`Include` support (7.3+) is required.

The remote is portable: its hostname resolves normally and any nonstandard port
is in the address. Another machine still needs an authorized identity and host-key
verification, as with any private SSH Git repository. Public anonymous cloning
uses the HTTP(S) endpoint described below. The API advertises the endpoint via `DRUM_GIT_SSH_URL`; no
particular production hostname or hosting provider is hardcoded. The default is
local testing at `localhost:2222`.

This initial account model has one identity per key. Another machine needs the
same private key to manage its owned projects; recovery, device linking, and revocation
commands are not implemented yet. Keep your identity safe: losing it loses access.
Private keys are currently unencrypted on disk with private filesystem permissions.

Ordinary `git clone` transfers Git history, not local `drum.*` configuration. The cloned repo
can use its `origin` immediately; the Drum convenience commands target a remote
named `drum`. Running init in a clone without saved Drum metadata creates a new
project rather than silently adopting an existing one.

## Checks

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

The default `cargo test` run includes the self-contained trust and update-notice
integration tests. All CLI tests are Rust; no Python runtime is needed.

For the real-service smoke test, build the sibling API and Git binaries, install
`psql`, configure development Postgres/Redis in `drum-api/.env`, then run:

```sh
cargo test --locked --test e2e git_lifecycle -- --ignored --exact --test-threads=1
```

The smoke test starts local API/SSH processes on temporary ports and uses
temporary identities, SSH settings, and Git repositories. It applies pending
migrations and creates unique project/identity rows in the configured database;
use a development database. It verifies retry safety, origin/tracking preservation,
branches, tags, push/pull/clone, rejected non-fast-forward pushes, signature replay
protection, ownership isolation, real-host address portability, host-key pinning, and persistence across restart.

## Resource-limit integration checks

With `psql` installed and development Postgres/Redis configured:

```sh
cargo test --locked --test e2e resource_limits -- --ignored --exact --test-threads=1
```

This also tests quota rejection without publishing refs, concurrent pushes sharing
one allowance, the real 100 MB pack cap, the 20-project allowance, and read-only
Git access without Redis. It generates a roughly 100 MB incompressible fixture.
It temporarily changes test-identity allowances and the database's global budget,
restores them, and removes its unique test identities/projects afterward. Run it
against a development database, not a live deployment.

Service-backed tests are ignored by default so ordinary tests need no sibling
checkouts or database. Run all three serially with
`cargo test --locked --test e2e -- --ignored --test-threads=1`. Run resource-limit
checks with exclusive use of the development database because they change its
global storage budget. Fixture processes and rows are cleaned up on test failure
as well as success; allowance changes are restored before cleanup.

## Publishing and write invitations

Restart the API first to apply migration `0003_sharing`, restart Git, and reinstall
this CLI. No database reset is needed. Local defaults are API `localhost:3000`,
Git SSH `localhost:2222`, and Git HTTP `localhost:3001`.

```sh
# Owner, inside an initialized project:
drum push
drum publish                  # Confirms exposure of all branches, tags, and history.
# Prints http://localhost:3001/k7m2-x9p4.git locally.
# Use drum publish --yes for noninteractive operation.
drum invite                   # Prints one secret write invitation URL.
drum invite --list            # Shows invitation IDs/status, never their tokens.
drum revoke <id>              # Revokes the invitation and its associated writer grant.
drum unpublish                # Private again; existing writer grants remain.

# Anyone, with the public read URL:
git clone http://localhost:3001/k7m2-x9p4.git
# Or configure Drum commands too:
drum clone http://localhost:3001/k7m2-x9p4.git [directory]

# Invited collaborator, using the full quoted URL:
drum clone 'http://localhost:3001/k7m2-x9p4.git#token=<token>' [directory]
cd <directory>
# Edit, add, commit, switch branches as usual.
drum push
```

`publish` changes visibility; `invite` grants write access. Only the owner can
publish/unpublish or manage invitations. Creating or claiming an invitation
requires the project to be public. Invitations expire after 24 hours and can be
claimed by one identity. A successful claim attaches read/write permission to
that identity; permission persists after invitation expiry until revoked. The
same identity can retry a failed clone while the invitation is valid. Unpublishing
also revokes unused invitations. Public access grants read-only Git operations.
Existing downloaded clones cannot be recalled by unpublishing.

`drum clone` uses HTTP(S), creates a clean `origin` and `drum` remote, and saves
Drum metadata. Plain public clones create no identity until an authenticated
operation needs one. Claiming a write invitation creates/reuses your local key
without a login command. The token fragment is stripped before invoking Git and
never saved in remote URLs or Git configuration. Quote invitation URLs and treat
them as secrets; your shell history may retain the original command.

A repository-scoped Git credential helper signs API challenges and obtains
five-minute credentials for the current owner/writer identity. Credentials are
kept out of remote URLs and persistent credential stores. Every Git HTTP request
checks current permissions, so revocation also invalidates still-unexpired
credentials on subsequent requests. Helpers verify that the signed challenge
names the expected Git host. Redirects are disabled. This also lets an existing
writer clone a private repository using the clean URL. Collaborator pushes count
towards the owner's storage allowance.

Use the same `DRUM_GIT_HTTP_URL` on API and Git. Set `DRUM_API_URL` on Git to the
API's reachable origin for discovery. `drum clone --api-url <origin>` overrides
discovery. Public deployment requires HTTPS (HTTP is allowed only on loopback).
Existing owner SSH remotes keep working; publication prints the HTTP(S) read URL.

Run `cargo test --locked --test e2e sharing_lifecycle -- --ignored --exact --test-threads=1`
against development Postgres/Redis to check
anonymous cloning, invitations, writer pushes, quotas, expiry and revocation.

## Repository trust

`drum clone` and `drum pull` require first-use approval of the repository URL,
Git remote (including Git URL rewrites), and API origin. The prompt defaults to
no. Noninteractive first use fails unless `--trust` explicitly approves the
source. Subsequent operations on the same approved source need no prompt.
Clone performs only anonymous API discovery before the trust check; claims,
identity proofs, repository metadata requests and Git clone happen afterward.

Approvals are private JSON files under `trust-v1/` in the Drum config directory,
keyed by a SHA-256 digest of the source tuple. They are never read from repository
config, and creating an approval does not create an identity. Changes to any
source field need their own approval. Remove the matching record (or the whole
`trust-v1` directory) to forget approvals. Approval persists even if a later
network operation fails. It is consent to the source, not code verification or a
sandbox. Direct Git commands do not enforce these approvals.

Run `cargo test --locked --test trust` to check prompts,
refusal before claims/transfers, remembered approval, changed sources and
noninteractive opt-in using temporary repos and a fake API; no database needed.

## Update notices

After a successful interactive command, the CLI can check its selected API's
`GET /v1/cli/version` endpoint and show a notice on stderr:

```text
A newer Drum version is available: 0.1.0 → 0.2.0
Update: curl -fsSL https://drum.dev/cli | sh
```

Checks are cached per API for 24 hours, including failed or unsupported checks.
A separate notification timestamp limits notices to once every 24 hours across
APIs. The cache is `updates.json` in the Drum configuration directory; it contains
only release metadata and timestamps. It never creates an identity key. Checks
use the API selected by init/clone, or the API saved in an existing project's
configuration. The server must advertise a newer published CLI semantic version;
stable installations ignore prereleases, and build metadata alone is not an
update. Installing the advertised version stops the notice immediately.

No check runs for help, `--version`, failed commands, noninteractive stderr, or
internal Git credential-helper calls. Set `DRUM_NO_UPDATE_CHECK=1` to opt out.
Checks use HTTPS outside loopback, refuse redirects, bound response size and have
a 750 ms HTTP timeout. A busy cache lock skips the check. Network/cache/permission
errors remain silent and never change the command's output or exit status. Nothing
is downloaded or installed automatically; the install command is fixed in the CLI,
not supplied by the API. Publish the corresponding installer/release before setting
the API's `DRUM_CLI_LATEST_VERSION`.

Run `cargo test --locked --test updates` to test interactive notices against a local fake
API, including command-output ordering, daily throttling, version comparison,
opt-outs, redirects, failed requests and timeouts. No development database needed.

## Repository-local protocol

This repository builds independently; no sibling source checkouts are required.
API wire definitions and signed-message formats live in `src/protocol/`. Keep
them compatible with the API and Git service when changing the protocol.
Sibling running services are only needed for end-to-end integration checks.

## Manually publishing CLI releases

The public installer at `https://drum.dev/cli` downloads assets from GitHub
Releases on `drumhq/drum-cli`. It resolves `version.txt` from the latest published
release, then pins downloads to its `vMAJOR.MINOR.PATCH` tag. It never fetches a
moving branch or compiles on the user's machine.

1. Update the package version and lockfile and commit the release source.
2. Create and push the corresponding semver tag, for example `v0.1.0`.
3. Create a draft GitHub Release for that tag.
4. Manually run **Build CLI release assets** with that existing tag, or build and
   attach the assets yourself. The workflow tests and packages macOS/Linux Intel
   and ARM64 binaries, and attaches them to the existing draft. It does not bump
   versions, create tags, create releases, or publish them.
5. Review and manually publish the complete GitHub Release as the latest release.
6. Set `DRUM_CLI_LATEST_VERSION` on the API to the published version.

Required release assets are `version.txt` (the plain version plus newline), and
`drum-<target>.tar.gz` plus `drum-<target>.tar.gz.sha256` for each supported target:
`aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-musl`, and
`x86_64-unknown-linux-musl`. Each archive contains only the executable `drum`.
Checksum files use `SHA256  archive-filename` format. The package helper is
`sh scripts/package-release.sh <target>`. The public installer serves stable
three-part semantic versions; prereleases do not become its default download.

For local integration tests explicitly configure the API's `DRUM_GIT_SSH_URL`
and `DRUM_GIT_HTTP_URL` to the local Git listeners. Defaults advertise production.
