# Packaging

Every way to install the Chat with Work Local Agent comes from one release: a `v*` tag on a commit already on `main`.

| Platform | What users install | Built by |
|---|---|---|
| macOS | Homebrew formula (`brew install crmne/tap/cww`), a signed and notarized `.pkg`, or the one-line installer | `release.yml`, job `macos` |
| Windows | Per-user `.msi` (x64, arm64), WinGet, Scoop, or the PowerShell installer | `release.yml`, job `build` |
| Linux | `.deb`, `.rpm`, AUR (`chatwithwork-local-agent`, `-bin`, `-git`), Homebrew, Nix flake, or the one-line installer | `release.yml` and `packaging.yml` |

Linux binaries are static (musl), so one build per architecture runs on every distribution and the packages have no dependencies.

## Files

| Path | Role |
|---|---|
| [`native-packages.yaml`](native-packages.yaml) | The packaging configuration: `.deb`/`.rpm` contents, release assets, recipe templates, and the AUR and Homebrew destinations. |
| `.github/workflows/release.yml` | Builds, signs and packages every platform on a tag, then publishes the GitHub release. Run it by hand to try a change without publishing. |
| `.github/workflows/packaging.yml` | After a release: `.deb`, `.rpm`, AUR, Homebrew, WinGet and Scoop recipes. On pushes and PRs that touch packaging: the same packages from the commit, installed in clean containers. |
| `packaging/systemd/cww.service` | The systemd user unit shipped by the Linux packages. |
| `packaging/linux/postinstall.sh` | Tells `.deb`/`.rpm` users how to start. Starts nothing: the daemon runs per user. |
| `packaging/arch/*/PKGBUILD.in`, `cww.install` | AUR recipes; `build-local.sh` builds the source recipe from the working tree. |
| `packaging/homebrew/cww.rb.in` | The formula, with a `brew services` definition. |
| `packaging/macos/` | `pkg.sh` (the installer package), its `postinstall`, `distribution.xml` and pages, and `import-installer-identity.sh` for CI. |
| `packaging/windows/` | `cww.wxs` (WiX 5 MSI), `build-msi.ps1`, and `sign.ps1` for certificate signing. |
| `packaging/winget/`, `packaging/scoop/` | Manifest templates. |
| `packaging/install.sh`, `install.ps1` | The one-line installers, attached to each release as `cww-installer.sh` and `cww-installer.ps1`. |
| `packaging/test-install.sh` | Installs, runs and removes a `.deb` or `.rpm` in a container. |
| `packaging/release-notes/vX.Y.Z.md` | Required for a stable tag. |
| `flake.nix` | The Nix package (Linux and macOS) and a dev shell. |

## Releasing

1. Bump `version` in `Cargo.toml` and run `cargo build` so `Cargo.lock` follows.
2. Write `packaging/release-notes/vX.Y.Z.md`.
3. Push `main`, wait for CI, then tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.

`release.yml` checks the tag matches `Cargo.toml` and is on `main`, builds every target, signs what it has credentials for, attests the archives (`gh attestation verify <file> --repo crmne/chatwithwork-local-agent`), and publishes the release with `checksums.txt` and the installers. Tags with a `-` (`v1.2.0-beta.1`) become draft prereleases and skip the package managers.

For a stable tag, `packaging.yml` then runs the shared [native-packages](https://github.com/crmne/native-packages/tree/v0.7.0) workflow: it downloads the Linux archives, verifies them against `checksums.txt`, builds and attaches the `.deb` and `.rpm` files, renders every recipe with the published checksums, and pushes the AUR and Homebrew ones when enabled. The rendered recipes are attached as `chatwithwork-local-agent-X.Y.Z-packaging.tar.xz`.

### Trying changes before a tag

- `gh workflow run release.yml` builds and signs everything, uploading the files as workflow artifacts without publishing.
- Any push or PR that touches packaging runs `packaging.yml` against the commit: static builds, `.deb`/`.rpm`, all recipes (with stand-ins for the macOS and Windows assets), and install tests on Ubuntu 24.04, Debian 12 and 13, Fedora 41 and latest, and Rocky 9, on amd64 and arm64.
- Locally, with `gem install native-packages --version 0.7.0`, nFPM 2.47.0 and `bsdtar`:

  ```sh
  native-packages validate
  # dist/ needs release-shaped inputs; see the "inputs" job in packaging.yml
  native-packages build --version 0.1.0 --target linux-amd64 --output /tmp/np
  bash packaging/test-install.sh ubuntu:24.04 /tmp/np
  packaging/arch/build-local.sh                 # makepkg from the working tree
  nix build .#default
  ```

## Signing

A complete set of secrets turns each kind of signing on. With none, the release is built unsigned; an incomplete set fails the build.

### macOS

The same secrets as ZapFast and TonePush. `native-packages notarize-macos` signs the universal binary with the hardened runtime and a timestamp, and notarizes it; the Homebrew formula uses that archive. A bare binary can't carry a stapled ticket, so Gatekeeper checks it online the first time.

- `APPLE_CERTIFICATE_P12`: base64 PKCS#12 Developer ID **Application** certificate and private key.
- `APPLE_CERTIFICATE_PASSWORD`: its export password.
- `APPLE_SIGNING_IDENTITY`: `Developer ID Application: PlentyLabs UG (haftungsbeschrankt) & Co. KG (JPL7999US3)`.
- `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`: Apple ID email, Team ID and an app-specific password, for notarization.

The `.pkg` needs a Developer ID **Installer** certificate as well, which a Developer ID Application certificate doesn't cover. With it, `pkg.sh` signs the package, notarizes it and staples the ticket, so it installs offline too. Without it the `.pkg` is built unsigned, which Gatekeeper refuses to open with a double-click; the Homebrew formula and the one-line installer don't need it.

#### Getting the Developer ID Installer certificate

Only the Apple Developer account holder can create Developer ID certificates.

1. On a Mac, open Keychain Access → Certificate Assistant → *Request a Certificate From a Certificate Authority*. Enter the account holder's email, choose *Saved to disk*, and save the `.certSigningRequest`.
2. At [developer.apple.com/account/resources/certificates](https://developer.apple.com/account/resources/certificates/add), choose **Developer ID Installer**, keep the *G2 Sub-CA* profile, upload the request and download the `.cer`.
3. Double-click the `.cer` so Keychain Access pairs it with the private key from step 1. It appears under *My Certificates* as `Developer ID Installer: PlentyLabs UG (haftungsbeschrankt) & Co. KG (JPL7999US3)`.
4. Right-click that certificate → *Export*, save as `.p12` with a strong password. Keep the `.p12` and its password in the 1Password item "Apple Notarization", next to the Application certificate.
5. Set the secrets:

   ```sh
   base64 -i developer-id-installer.p12 | gh secret set APPLE_INSTALLER_CERTIFICATE_P12
   gh secret set APPLE_INSTALLER_CERTIFICATE_PASSWORD   # paste the export password
   gh secret set APPLE_INSTALLER_SIGNING_IDENTITY --body "Developer ID Installer: PlentyLabs UG (haftungsbeschrankt) & Co. KG (JPL7999US3)"
   ```

6. Run `gh workflow run release.yml` and check that the `macos` job signs, notarizes and staples the `.pkg`. `pkgutil --check-signature` and `spctl -a -vv -t install` on the downloaded artifact should both report the Developer ID Installer identity.

The notarization secrets (`APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`) are shared with the binary and need no change.

### Windows

Unsigned Windows binaries that register a logon task are what Microsoft Defender's behaviour models look for, and SmartScreen warns about any unsigned MSI a browser downloaded. `cww.exe`, `cww-agent.exe` and the MSI are all signed with SHA-256 and an RFC 3161 timestamp once one of these is set up. Sign releases before publishing them to WinGet.

#### What SmartScreen needs

- **A signature.** Unsigned downloads always get the full-screen warning.
- **Reputation for the publisher.** A signature alone doesn't silence SmartScreen: since 2024 no certificate type, EV included, is trusted instantly. Reputation builds as people download and run releases signed by the same publisher identity, so keep signing every release with the same identity and don't rotate it needlessly.
- The warning only applies to files with the Mark of the Web, meaning downloaded by a browser. `winget install`, Scoop and the PowerShell one-liner don't trigger it; Defender still scans them.

#### Option A: Azure Artifact Signing (formerly Trusted Signing)

About $10 a month, keys in Microsoft's HSMs, and `release.yml` already supports it. Eligibility for Public Trust:

- Organizations in the EU (and the US, Canada, UK and others) with **at least three years of verifiable tax history**. PlentyLabs qualifies only if it has that history.
- Individuals only in the US and Canada.

Steps:

1. In the Azure portal, create an *Artifact Signing account* in West Europe (endpoint `https://weu.codesigning.azure.net/`).
2. Under *Identity validation*, start a **Public Trust** validation for the organization. Microsoft checks the registration and tax records, and one person completes an ID check with AU10TIX. It usually takes a few days.
3. Create a *certificate profile* of type **Public Trust** using the validated identity.
4. Create an app registration (Entra ID) with a client secret, and give it the **Artifact Signing Certificate Profile Signer** role on the account.
5. Set the secrets: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE`.
6. Run `gh workflow run release.yml` and check the Windows jobs report signed files (`Get-AuthenticodeSignature` on the MSI shows `Valid`).

#### Option B: a certificate from a CA with cloud signing

If the organization doesn't qualify for Option A, buy an **OV** code signing certificate for the organization (or for yourself as an individual) from a CA that offers cloud signing usable in CI, such as SSL.com (eSigner) or Certum (SimplySign). Since June 2023, publicly trusted code signing keys must live in a hardware or cloud HSM, so a new certificate can't be exported as a `.pfx`. The `WINDOWS_CERTIFICATE_PFX` path in `sign.ps1` only works for older exportable certificates. Using a cloud-signing CA needs a small change to `packaging/windows/sign.ps1` to call the CA's signing tool instead. EV costs more and no longer buys instant SmartScreen trust, so OV is enough.

#### Option C: the Microsoft Store

Individual developer accounts are free. Microsoft signs Store packages itself, so they never hit SmartScreen, and `winget install` can install from the Store. It needs an MSIX package with a full-trust startup task for the agent, which `cww.wxs` doesn't produce yet.

## Package managers

| Destination | How it's published | Needs |
|---|---|---|
| GitHub release (`.deb`, `.rpm`, recipes) | Automatic | Nothing |
| Homebrew, `crmne/homebrew-tap` `Formula/cww.rb` | Automatic when `PUBLISH_HOMEBREW=true` | `HOMEBREW_TAP_SSH_KEY` (a deploy key with write access to the tap only) or `HOMEBREW_TAP_GITHUB_TOKEN` |
| AUR `chatwithwork-local-agent`, `-bin`, `-git` | Automatic when `PUBLISH_AUR=true` | `AUR_SSH_KEY`, `AUR_KNOWN_HOSTS`, and the three AUR packages registered to that key |
| WinGet, `ChatWithWork.LocalAgent` | By hand, below | A fork of `microsoft/winget-pkgs` |
| Scoop | By hand, below | A bucket repository |
| Nix | `nix profile install github:crmne/chatwithwork-local-agent` works from the repository; nixpkgs is a separate submission | - |

`PUBLISH_AUR` and `PUBLISH_HOMEBREW` are repository **variables**; the rest are secrets.

**WinGet.** Unpack the release's packaging archive and copy `recipes/winget/*.yaml` to `manifests/c/ChatWithWork/LocalAgent/X.Y.Z/` in a `winget-pkgs` fork. On Windows, run `winget validate --manifest <dir>` and `winget install --manifest <dir>`, then open the pull request (or `wingetcreate submit <dir>`). The manifests use the MSI's fixed UpgradeCode, so upgrades replace the previous version.

**Scoop.** Put `recipes/scoop/cww.json` in a bucket (for example `crmne/scoop-bucket`, as `bucket/cww.json`). Its `checkver` and `autoupdate` keep it current: `scoop install crmne/cww` once the bucket is added with `scoop bucket add crmne https://github.com/crmne/scoop-bucket`.

## Before the first public release

The repository is private. Every download URL in the formula, the AUR recipes, the WinGet and Scoop manifests and the one-line installers points at its GitHub releases, which only work anonymously once the repository is public. Until then, installs work from a checkout (`cargo install --path .`, `packaging/arch/build-local.sh`, `nix build`) or from release files downloaded with `gh release download`.

## What the MSI and the .pkg do

- **Windows:** per-user install to `%LOCALAPPDATA%\Programs\Chat with Work Local Agent` (no UAC prompt), added to the user's `PATH`, a Start menu entry that opens the `cww` terminal UI, and `cww daemon install`, which registers the logon Scheduled Task running `cww-agent.exe` (no console window). Uninstalling runs `cww daemon uninstall` and keeps keys and settings. The UpgradeCode `{FBC69B33-161E-40DC-9D83-5B54DB1A9821}` must never change.
- **macOS:** installs `/usr/local/bin/cww` (admin password), then registers the LaunchAgent for the logged-in user. The first time the daemon reads `~/Documents`, macOS asks whether `cww` may access it.
- **Linux packages:** install `/usr/bin/cww` and the systemd user unit, and start nothing: each user runs `cww` to pair, then `systemctl --user enable --now cww`.

In every case the daemon stays idle until the computer is paired, and nothing is shared until the user picks a folder.
