# Contributing to Hooklistener CLI

Thank you for your interest in contributing to Hooklistener CLI! We welcome contributions from the community and are grateful for any help you can provide.

## Code of Conduct

Please note that this project is released with a [Code of Conduct](CODE_OF_CONDUCT.md). By participating in this project you agree to abide by its terms.

## How to Contribute

### Reporting Issues

- Check if the issue has already been reported in the [Issues](https://github.com/hooklistener/hooklistener-cli/issues) section
- If not, create a new issue with:
  - A clear, descriptive title
  - Steps to reproduce the problem
  - Expected vs actual behavior
  - Your environment (OS, Rust version, etc.)
  - Any relevant logs or screenshots

### Suggesting Features

- Open a [Discussion](https://github.com/hooklistener/hooklistener-cli/discussions) first to gauge interest
- For approved features, create an issue with the `enhancement` label
- Provide clear use cases and implementation ideas

### Pull Requests

1. **Fork the repository** and create your branch from `main`
2. **Follow the setup instructions** in the README
3. **Make your changes**:
   - Write clear, concise commit messages
   - Follow the existing code style
   - Add tests for new functionality
   - Update documentation as needed
4. **Test your changes**:

   ```bash
   cargo test --all-targets --all-features --locked
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   ```

5. **Submit a Pull Request**:
   - Reference any related issues
   - Describe your changes in detail
   - Include screenshots for UI changes

## Development Guidelines

### Code Style

- Follow Rust standard conventions
- Use `cargo fmt` to format your code
- Use `cargo clippy` to catch common mistakes
- Write meaningful variable and function names
- Add comments for complex logic

### Testing

- Write unit tests for new functions
- Add integration tests for new features
- Ensure all tests pass before submitting PR
- Aim for good test coverage

Saved-case changes also have a release-profile inventory gate on Linux, macOS,
and Windows. Run `mise exec -- make check-cases` and update the inventory when
adding or renaming tests. See [Saved-case conformance](docs/cases-conformance.md)
for the CI contract and per-platform evidence.

### Build size and compilation time

See [Build footprint](docs/build-footprint.md) for the dependency/release-profile
choices and an isolated measurement tool. Measure clean builds, no-op builds,
and source rebuilds separately; do not compare a warm build with a cold one.

### Documentation

- Update README.md if adding new features
- Add inline documentation for public APIs
- Update CHANGELOG.md for user-facing changes
- Include examples where appropriate

### Commit Messages

Follow conventional commit format:

```
type(scope): description

[optional body]

[optional footer]
```

Types:

- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `test`: Test additions or corrections
- `chore`: Maintenance tasks

Example:

```
feat(ui): add search functionality

Implements fuzzy search for webhook requests
with keyboard shortcut (/) and filter persistence.

Closes #42
```

## Release Controls

Stable releases are created only by `.github/workflows/release.yml` from an exact
`vMAJOR.MINOR.PATCH` tag whose commit is contained in `main`.

Prepare each version in a normal pull request. `Cargo.toml`, the root
`Cargo.lock` entry for `hooklistener-cli`, and
`npm/packages/hooklistener/package.json` must contain the same exact version.
The repository's `release.toml` can prepare the local Cargo version commit, but
it intentionally cannot tag, push, or publish.

Before merging the version pull request or creating its tag, repository
administrators must verify these controls:

- Protect the `release` environment with required reviewers and restrict it to
  one custom deployment pattern, `v*.*.*`. Prevent self-approval and disable
  administrator bypass of environment protection rules. The workflow separately
  enforces exact semantic versions.
- Store `CARGO_REGISTRY_TOKEN`, `NPM_TOKEN`, and `HOMEBREW_TAP_TOKEN` only as
  `release` environment secrets; delete the repository-scoped copies after
  rotating them. Rotate them periodically and immediately after suspected
  exposure.
- Apply effective `main` rules—not merely an active ruleset with no matching
  ref—that prevent deletion and force pushes, require at least one pull-request
  approval, require branches to be current, and require all of these checks:
  `Rustfmt`, `Clippy`, `Tests (stable)`, `Cargo Audit`, `Analyze`, the four
  `Build (...)` targets, and
  `Authenticated lifecycle (linux|macos|windows)`. Pin every context to the
  GitHub Actions app rather than accepting the same name from any source.
- Apply two active tag rulesets to `refs/tags/v*.*.*`: a creation-only ruleset
  whose only bypass actors are the designated release maintainers, and a
  separate update-and-deletion ruleset with no bypass actors. Do not combine
  them: a maintainer allowed to bypass creation in a combined ruleset could
  also bypass immutability. Leave both rulesets' exclusion lists empty, and
  review all ruleset and environment bypass actors manually.

Inspect the effective state rather than trusting settings-page names:

```sh
gh api --paginate --slurp \
  'repos/hooklistener/hooklistener-cli/rules/branches/main?per_page=100'
gh api --paginate --slurp \
  'repos/hooklistener/hooklistener-cli/rulesets?targets=tag&per_page=100'
gh api repos/hooklistener/hooklistener-cli/environments/release
gh api --paginate --slurp \
  'repos/hooklistener/hooklistener-cli/environments/release/deployment-branch-policies?per_page=100'
```

An empty effective-rules response, a missing `release` environment, empty
reviewer protection, or a null/unmatched deployment policy blocks release.
The release workflow checks the readable metadata and effective credential
presence, but an administrator must verify secret scope and bypass actors.
The sole `v*.*.*` environment deployment pattern must be of type **tag**; the
read-only policy-list response does not expose that type. Confirm that CodeQL
is active rather than `disabled_inactivity` before requiring its `Analyze`
check.

After the version pull request and all required checks pass, create one
annotated tag at that exact merged commit and push only that tag:

```sh
version=1.8.0
release_sha=<full-merged-version-commit-sha>
git fetch origin main --tags
git merge-base --is-ancestor "${release_sha}" origin/main
git tag -a "v${version}" "${release_sha}" -m "Release v${version}"
git push origin "refs/tags/v${version}"
```

The tag starts a release-profile V3 conformance gate on Linux, macOS, and
Windows. Every public release, package-registry, and tap mutation waits for
that exact tag SHA to pass. Pre-publication jobs may still upload private
Actions evidence and update the workflow's security check or issue.

If a publish partially fails, use **Re-run failed jobs** on the original workflow
run. Do not re-run all jobs or create another tag for the same version. The
workflow verifies an already-published package before continuing. Immediately
before every public mutation it also rereads the complete GitHub release,
crates.io, and npm version inventories. A delayed run cannot publish or promote
behind a newer version; if its exact GitHub release is already stable, the
workflow verifies it without moving GitHub Latest backward.

A release is stable only after the exact version and contents are confirmed on
crates.io, npm, the Homebrew tap, and the non-prerelease GitHub release. Until
all four agree, treat it as an incomplete release.

## Getting Help

- Join our [Discussions](https://github.com/hooklistener/hooklistener-cli/discussions)
- Check the [Wiki](https://github.com/hooklistener/hooklistener-cli/wiki)
- Reach out to maintainers in issues

## Recognition

Contributors will be recognized in:

- The project README
- Release notes
- GitHub's contributor graph

Thank you for helping make Hooklistener CLI better!
