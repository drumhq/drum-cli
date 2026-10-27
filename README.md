# Drum

Push, pull, and share Git projects. Private by default, with no account sign-up.

## Install

```sh
curl -fsSL https://drum.dev/cli | sh
```

Requires Git and OpenSSH. Supports macOS and Linux on Intel/AMD 64-bit and ARM64.
Run the installer again to update.

## Get started

Inside your project:

```sh
drum init
git add .
git commit -m "Initial commit"
drum push
```

Already have commits? Just run `drum init` and `drum push`.
Your existing Git remotes and branch tracking are preserved.

## Commands

| Command                                  | What it does                                                                 |
| ---------------------------------------- | ---------------------------------------------------------------------------- |
| `drum init`                              | Create a private project and add its Drum remote. Initializes Git if needed. |
| `drum push [branch]`                     | Push your current branch, or a branch you specify.                           |
| `drum pull [branch] [--trust]`           | Pull changes from Drum into your current or specified branch.                |
| `drum status`                            | Show your project, remote, branch, and identity key location.                |
| `drum publish`                           | Make the repository public and print its read-only clone URL.                |
| `drum unpublish`                         | Make it private again. Existing writers retain access.                       |
| `drum invite`                            | Create a secret URL that grants a collaborator write access.                 |
| `drum invite --list`                     | List invitations and their IDs.                                              |
| `drum revoke <invitation-id>`            | Revoke an invitation and the write access it granted.                        |
| `drum clone <url> [directory] [--trust]` | Clone a repository. An invitation URL also claims write access.              |

Run `drum <command> --help` for options and examples.

## Share a project

```sh
drum publish                    # Get a public read-only URL.
drum invite                     # Get a secret write invitation URL.
drum clone '<shared-url>'       # Clone a public project or accept an invitation.
```

Public repositories also work with ordinary `git clone`. Invitations expire after
24 hours; claimed write access lasts until revoked.

## Repository trust

On first use, `drum clone` and `drum pull` ask you to trust the repository,
Git remote, and API server. The default is no. Clone asks before claiming an
invitation, requesting authenticated access, or downloading the repository;
anonymous API discovery may happen first.

Approvals are remembered in your local Drum configuration. A changed repository
URL, Git remote, or API requires approval again. For automation, pass `--trust`
to explicitly approve and remember the source without prompting:

```sh
drum clone <url> --trust
drum pull --trust -- --ff-only
```

Trust records consent to a source; it does not certify that downloaded code is
safe to run. Ordinary `git clone` and `git pull` do not use this check.

## Your identity

Your private key is `~/.config/drum/id_ed25519`. Keep a secure backup—losing it
loses access to the projects it owns. Account recovery is not available yet.
