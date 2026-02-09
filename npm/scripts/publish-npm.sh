#!/usr/bin/env bash
set -euo pipefail

# Usage: publish-npm.sh <version>
# Example: publish-npm.sh 0.1.0
#
# Expects:
#   - NPM_TOKEN environment variable to be set
#   - Platform binaries already placed in each package's bin/ directory
#   - Working directory is the repository root

VERSION="${1:?Usage: publish-npm.sh <version>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
NPM_DIR="${ROOT}/npm/packages"

echo "Publishing hooklistener npm packages v${VERSION}"

# Configure npm auth (fallback if NODE_AUTH_TOKEN not set by setup-node)
if [[ -n "${NPM_TOKEN:-}" ]]; then
  echo "//registry.npmjs.org/:_authToken=${NPM_TOKEN}" > "${HOME}/.npmrc"
fi

# Update version in all package.json files
for pkg_dir in \
  "${NPM_DIR}/hooklistener-linux-x64" \
  "${NPM_DIR}/hooklistener-darwin-x64" \
  "${NPM_DIR}/hooklistener-darwin-arm64" \
  "${NPM_DIR}/hooklistener-win32-x64"; do

  pkg_name="$(jq -r .name "${pkg_dir}/package.json")"
  echo "Setting ${pkg_name} to v${VERSION}"

  jq --arg v "${VERSION}" '.version = $v' "${pkg_dir}/package.json" > "${pkg_dir}/package.json.tmp"
  mv "${pkg_dir}/package.json.tmp" "${pkg_dir}/package.json"
done

# Update main package version + optionalDependencies versions
MAIN_DIR="${NPM_DIR}/hooklistener"
jq --arg v "${VERSION}" '
  .version = $v |
  .optionalDependencies |= with_entries(.value = $v)
' "${MAIN_DIR}/package.json" > "${MAIN_DIR}/package.json.tmp"
mv "${MAIN_DIR}/package.json.tmp" "${MAIN_DIR}/package.json"

# Publish platform packages first
for pkg_dir in \
  "${NPM_DIR}/hooklistener-linux-x64" \
  "${NPM_DIR}/hooklistener-darwin-x64" \
  "${NPM_DIR}/hooklistener-darwin-arm64" \
  "${NPM_DIR}/hooklistener-win32-x64"; do

  pkg_name="$(jq -r .name "${pkg_dir}/package.json")"
  echo "Publishing ${pkg_name}@${VERSION}..."
  npm publish "${pkg_dir}" --access public
done

# Publish main package
echo "Publishing hooklistener@${VERSION}..."
npm publish "${MAIN_DIR}" --access public

echo "All npm packages published successfully!"
