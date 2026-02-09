#!/usr/bin/env bash
set -euo pipefail

# Usage: publish-npm.sh <version>
# Example: publish-npm.sh 0.1.0
#
# Expects:
#   - NPM_TOKEN environment variable to be set
#   - Working directory is the repository root

VERSION="${1:?Usage: publish-npm.sh <version>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PKG_DIR="${ROOT}/npm/packages/hooklistener"

echo "Publishing hooklistener npm package v${VERSION}"

# Configure npm auth (fallback if NODE_AUTH_TOKEN not set by setup-node)
if [[ -n "${NPM_TOKEN:-}" ]]; then
  echo "//registry.npmjs.org/:_authToken=${NPM_TOKEN}" > "${HOME}/.npmrc"
fi

# Update version in package.json
jq --arg v "${VERSION}" '.version = $v' "${PKG_DIR}/package.json" > "${PKG_DIR}/package.json.tmp"
mv "${PKG_DIR}/package.json.tmp" "${PKG_DIR}/package.json"

# Publish
echo "Publishing hooklistener@${VERSION}..."
npm publish "${PKG_DIR}" --access public

echo "npm package published successfully!"
