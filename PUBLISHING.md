# Publishing Asur

The release tag workflow already verifies the crate, builds Windows, macOS, and Linux archives, creates the GitHub release, and publishes crates.io. It also calls `distribution.yml` for WinGet and Homebrew. Missing package-manager secrets are reported as skipped and do not break the main release.

## One-time WinGet setup

WinGet requires the first `Asura0666.Asur` manifest to be submitted interactively and accepted before release automation can update it.

1. Publish the GitHub release containing `asur-windows-x86_64.zip`.
2. On Windows, install Microsoft's manifest tool:

   ```powershell
   winget install Microsoft.WingetCreate
   ```

3. Start the initial submission with the release archive URL:

   ```powershell
   wingetcreate new https://github.com/Asura0666/Asur/releases/download/v1.3.0/asur-windows-x86_64.zip
   ```

4. Use these package values when prompted:

   - Package identifier: `Asura0666.Asur`
   - Package version: `1.3.0`
   - Publisher: `Asura0666`
   - Package name: `Asur`
   - License: `MIT OR Apache-2.0`
   - Installer type: portable ZIP containing `asur.exe`

5. Let WingetCreate submit the pull request to `microsoft/winget-pkgs` and wait for it to be merged.
6. Create a GitHub personal access token that can create the public fork/PR required by WingetCreate. Store it in the Asur repository under **Settings → Secrets and variables → Actions → New repository secret** as `WINGET_TOKEN`. Give it an expiry and only the access needed for public repository submissions.

After the initial manifest is accepted, every release invokes `wingetcreate update ... --submit`. Users can then install Asur with:

```powershell
winget install Asura0666.Asur
```

## One-time Homebrew setup

The automated route uses a maintained third-party tap. This is immediately usable and does not depend on acceptance into `homebrew/core`.

1. Create a public GitHub repository named `Asura0666/homebrew-tap`. Initialize its `main` branch with a README.
2. Create a fine-grained GitHub personal access token limited to that repository with **Contents: Read and write**. Add it to the Asur repository's Actions secrets as `HOMEBREW_TAP_TOKEN`.
3. Open **Actions → Publish package managers → Run workflow**, enter an existing version such as `1.3.0`, and run it once.

The workflow downloads the immutable tag source, calculates its checksum, creates `Formula/asur.rb`, builds and tests it on macOS, and pushes it to the tap. Users install it with:

```bash
brew tap Asura0666/tap
brew install asur
```

For the shorter official command `brew install asur`, submit the tested formula to `Homebrew/homebrew-core`. That is a separate, manually reviewed contribution. Asur must have a stable tagged release, build from source, include its licence, and pass Homebrew's formula audit and test requirements.

## Normal release

After CI on `main` passes:

```bash
git tag -a v1.3.0 -m "Asur v1.3.0"
git push origin v1.3.0
```

If a package-manager submission needs to be retried after fixing a token, run **Publish package managers** manually with the existing release version and enable only the destination that needs retrying. Do not recreate the Git tag. Do not run a WinGet update for `1.3.0` after submitting that same version as the initial manifest; automation should begin with the next release.
