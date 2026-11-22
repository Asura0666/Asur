# Publishing Asur

The release tag workflow verifies the crate, builds Windows, macOS, and Linux archives, creates the GitHub release, publishes crates.io, and invokes the WinGet updater. Official Homebrew publication is intentionally handled through a reviewed `Homebrew/homebrew-core` pull request; it does not require a tap repository or an Asur Actions token.

## Release prerequisites

Before submitting either package, merge the release commit into `main`, let CI pass, and publish a stable GitHub release:

```bash
git tag -a v1.3.0 -m "Asur v1.3.0"
git push origin v1.3.0
```

The release must contain `asur-windows-x86_64.zip`, and the source tag must remain immutable.

## Official WinGet publication

The first `Asura0666.Asur` manifest must be submitted interactively and accepted before release automation can update it.

1. On Windows, install Microsoft's manifest creator:

   ```powershell
   winget install Microsoft.WingetCreate
   ```

2. Start the initial submission using the released Windows archive:

   ```powershell
   wingetcreate new https://github.com/Asura0666/Asur/releases/download/v1.3.0/asur-windows-x86_64.zip
   ```

3. Use these values when prompted:

   - Package identifier: `Asura0666.Asur`
   - Package version: `1.3.0`
   - Publisher: `Asura0666`
   - Package name: `Asur`
   - Moniker: `asur`
   - License: `MIT OR Apache-2.0`
   - Default locale: `en-US`
   - Installer architecture: `x64`
   - Installer type: `zip`
   - Nested installer type: `portable`
   - Nested installer file: `asur.exe`
   - Portable command alias: `asur`

4. Let WingetCreate submit the pull request to `microsoft/winget-pkgs`, complete Microsoft's contributor agreement if prompted, and wait for validation and review.
5. Confirm the accepted package:

   ```powershell
   winget install asur
   # Unambiguous alternative:
   winget install --id Asura0666.Asur --exact
   ```

### Automate later WinGet versions

After the initial manifest is merged, create a GitHub token that can create the public fork and pull request used by WingetCreate. Give it an expiry and only the necessary public-repository access. Add it to the Asur repository under **Settings → Secrets and variables → Actions** as `WINGET_TOKEN`.

Future release tags call `wingetcreate update Asura0666.Asur --submit`. If an update needs retrying, run **Actions → Publish WinGet → Run workflow** with the existing release version. Do not submit an update for the same version used to create the initial manifest.

## Official Homebrew Core publication

The goal is an accepted formula in `Homebrew/homebrew-core`, which enables:

```bash
brew install asur
```

This is a reviewed upstream contribution, not a release-workflow upload. It must be prepared on macOS or Linux with Homebrew installed. Windows maintainers can use WSL2 with Homebrew for local preparation; Homebrew's pull-request CI will exercise its supported platform matrix.

### 1. Check eligibility and existing work

After `v1.3.0` is public:

```bash
brew update
brew search asur
```

Also search open and closed `Homebrew/homebrew-core` pull requests. The project needs a stable immutable release, a public homepage, an accepted open-source licence, a reproducible source build, and a meaningful formula test. Final acceptance is decided by Homebrew's reviewers.

### 2. Fork and prepare homebrew/core

Fork `https://github.com/Homebrew/homebrew-core` to `Asura0666/homebrew-core`, then run:

```bash
brew tap --force homebrew/core
cd "$(brew --repository homebrew/core)"
git remote add Asura0666 https://github.com/Asura0666/homebrew-core.git
git switch -c add-asur-1.3.0
brew create --rust --set-name asur https://github.com/Asura0666/Asur/archive/refs/tags/v1.3.0.tar.gz
```

Review the generated `Formula/a/asur.rb` and make it equivalent to:

```ruby
class Asur < Formula
  desc "CLI for local containers, ports, environment files, and secret scanning"
  homepage "https://github.com/Asura0666/Asur"
  url "https://github.com/Asura0666/Asur/archive/refs/tags/v1.3.0.tar.gz"
  sha256 "SHA256_GENERATED_BY_BREW_CREATE"
  license any_of: ["Apache-2.0", "MIT"]

  depends_on "rust" => :build

  deny_network_access!

  def fetch
    system "cargo", "fetch", *std_cargo_fetch_args
  end

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/asur --version")
  end
end
```

Keep the checksum generated for the real tag archive; do not submit the placeholder.

### 3. Build, test, and audit

From the `homebrew/core` checkout:

```bash
HOMEBREW_NO_INSTALL_FROM_API=1 brew install --build-from-source asur
brew test asur
brew audit --strict --new --online asur
brew style --fix --formula asur
brew lgtm --online asur
```

Resolve every reported problem before opening the pull request.

### 4. Submit the official formula

```bash
git add Formula/a/asur.rb
git commit -m "asur 1.3.0 (new formula)"
git push --set-upstream Asura0666 add-asur-1.3.0
```

Open a pull request from that branch to `Homebrew/homebrew-core`, complete its template, disclose that the submitter is the upstream maintainer, and keep review discussion in the pull request. Once merged and available through Homebrew's API, verify:

```bash
brew update
brew install asur
asur --version
```

No personal tap, Homebrew-specific Actions secret, or Asur release job is needed. For later releases, use Homebrew's supported `brew bump-formula-pr` flow if its automation has not already opened an update.
