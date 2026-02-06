# Homebrew Tap for Hooklistener CLI

This is the official Homebrew tap for [Hooklistener CLI](https://github.com/hooklistener/hooklistener-cli).

## Installation

```bash
brew tap hooklistener/tap
brew install hooklistener
```

## Upgrade

```bash
brew upgrade hooklistener
```

## Uninstall

```bash
brew uninstall hooklistener
brew untap hooklistener/tap
```

---

## Setup Instructions (for maintainers)

### 1. Create the `homebrew-tap` repository

Create a new public repository at `github.com/hooklistener/homebrew-tap` with this structure:

```
homebrew-tap/
├── README.md
└── Formula/
    └── hooklistener.rb
```

Copy the contents of this directory to that repository.

### 2. Create a Personal Access Token

1. Go to GitHub Settings → Developer settings → Personal access tokens → Fine-grained tokens
2. Create a new token with:
   - **Repository access**: Select `hooklistener/homebrew-tap`
   - **Permissions**: Contents (Read and Write)
3. Copy the token

### 3. Add the secret to the main repository

1. Go to `hooklistener/hooklistener-cli` → Settings → Secrets and variables → Actions
2. Create a new repository secret:
   - **Name**: `HOMEBREW_TAP_TOKEN`
   - **Value**: The token from step 2

### 4. Test the setup

Create a new release (e.g., `v0.1.0`) and verify:
1. The release workflow builds and publishes binaries
2. The `update-homebrew` workflow updates the tap repository
3. Users can install via `brew tap hooklistener/tap && brew install hooklistener`

### How it works

When you publish a new release:
1. The `release.yml` workflow builds binaries and creates a GitHub release
2. The `update-homebrew.yml` workflow:
   - Downloads the SHA256 checksums from the release
   - Updates the formula with the new version and checksums
   - Pushes to the `homebrew-tap` repository
