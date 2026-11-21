# Installing makiba

makiba is one binary. A release holds one archive per target, a `manifest.json` and `scripts/install.sh`; no Rust toolchain is needed to install it. The releases are on GitHub, in a public repository, so no URL below needs a token.

Targets: `linux-x86_64`, `linux-aarch64`, `macos-aarch64`, `macos-x86_64`. Windows is not a target. The Linux binaries are built on Ubuntu 22.04 and need glibc 2.35 or newer.

## Install

The installer needs `sh`, `curl` (or `wget`), `tar`, `uname` and `sha256sum` (or `shasum`). It does not need Rust, Python or `jq`. This downloads it to a file first, so that a failed download stops here instead of running nothing silently:

```sh
curl -fsSL -o install.sh https://github.com/suisya-systems/makiba/releases/latest/download/install.sh && sh install.sh
```

It installs the latest release as `~/.local/bin/makiba` and prints the version it installed. `--prefix DIR` installs `DIR/bin/makiba` instead. If `~/.local/bin` is not in your `PATH`, it says so.

### Pin a version

```sh
curl -fsSL -o install.sh https://github.com/suisya-systems/makiba/releases/download/v0.1.0/install.sh && sh install.sh --version 0.1.0
```

The installer of a release and `--version` of the same number belong together; `--version` refuses a manifest of another version.

### What the installer refuses

It places nothing unless every check passes. It prints the cause and exits with:

| Exit | Meaning |
|---|---|
| 0 | Installed. |
| 2 | Usage: an unknown option, a missing value, a version that is not `X.Y.Z`, no `--prefix` and no `HOME`. |
| 3 | This host: no release for its OS or CPU, or `curl`/`wget`, `sha256sum`/`shasum` or `tar` is missing. |
| 4 | A download failed (the manifest or the archive). |
| 5 | The manifest is unusable: no entry for this host's target, no `url` or no 64-digit `sha256`, an archive name that is not this target's, or a version other than the one asked for. |
| 6 | The archive's sha256 is not the manifest's. |
| 7 | The archive is unusable: no `makiba` file in it, it does not run on this host, or it prints a version other than the manifest's. |
| 8 | `DIR/bin` cannot be created or written, or `DIR/bin/makiba` is a directory. |

`MAKIBA_RELEASES_URL` replaces `https://github.com/suisya-systems/makiba/releases` (for a mirror, or a local copy as under "Check by hand"). Under it the manifest is `latest/download/manifest.json` or `download/vX.Y.Z/manifest.json`.

## Update

Run the install command again; it replaces `DIR/bin/makiba` in one step and keeps the old file if anything is refused. A running `makibad` and the agents it started keep paths that name the old file, so stop makiba first and start it after:

```sh
makiba down
sh install.sh
makiba up
```

## Check by hand

Each release's `manifest.json` lists, per target, the archive's `url` and `sha256`:

```sh
curl -fsSL https://github.com/suisya-systems/makiba/releases/download/v0.1.0/manifest.json
curl -fsSLO https://github.com/suisya-systems/makiba/releases/download/v0.1.0/makiba-0.1.0-linux-x86_64.tar.gz
sha256sum makiba-0.1.0-linux-x86_64.tar.gz     # on macOS: shasum -a 256
```

The two hashes must be equal. The archive holds `makiba`, `LICENSE` and `THIRD-PARTY-NOTICES`. To see the installer refuse a changed archive, copy a release to a folder laid out as above (`download/v0.1.0/manifest.json` and the archive next to it, with the manifest's `url` pointing at the copy), change one byte of the archive, and run `MAKIBA_RELEASES_URL=file:///that/folder sh install.sh --version 0.1.0 --prefix /tmp/try`: it exits 6 and `/tmp/try/bin` stays empty.

## What the checksum protects

The sha256 in the manifest catches a damaged or partial download, a wrong archive behind a right manifest, and a mirror that fell behind; https protects the manifest itself on the way. It does not protect against someone who can write the release itself (the owner's GitHub account, or the release workflow's token): they replace the archive and the manifest together. A signature would not close that either, unless its key were kept away from GitHub and its public half reached you by another route, and checking it would need another tool on the host. So releases are checked by sha256 only. This is revisited when releases are cut by someone other than the owner, or when unattended installs on servers are the main way makiba is installed; GitHub's build attestations are the first thing to add then.

## Making a release (the owner)

1. The release workflow is `docs/release/release.yml`; it takes effect only as `.github/workflows/release.yml`, and `tests/release_install.rs` fails if the two differ. Activate it first.
2. Set `version` in `Cargo.toml` (and `Cargo.lock`) to the new version.
3. Optionally rehearse: Actions > Release > Run workflow. It builds the four archives and checks the manifest, and cannot create a release.
4. Push the tag `vX.Y.Z`. If it differs from `Cargo.toml` the first job fails and nothing is built or published.
5. Once, after the first release: on a clean Linux host with only `curl` (a container will do), run the install command and see `makiba --version` print the version; change one byte of a downloaded archive and see exit 6 as above; compare the manifest's four hashes with `sha256sum` of the four archives.
