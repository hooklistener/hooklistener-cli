#!/usr/bin/env bash
set -euo pipefail

# Usage: publish-npm.sh <version> <package-tarball> <expected-integrity>
# Example: publish-npm.sh 1.8.0 /tmp/hooklistener-1.8.0.tgz sha512-...
#
# Expects:
#   - NODE_AUTH_TOKEN or NPM_TOKEN environment variable to be set
#   - Working directory is the repository root

VERSION="${1:?Missing exact version}"
PACKAGE_TARBALL="${2:?Missing exact package tarball}"
EXPECTED_INTEGRITY="${3:?Missing expected package integrity}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PKG_DIR="${ROOT}/npm/packages/hooklistener"

if [[ ! "${VERSION}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "::error::npm publication requires an exact stable semantic version."
  exit 1
fi

if ! jq -e \
  --arg version "${VERSION}" \
  '.name == "hooklistener" and .version == $version' \
  "${PKG_DIR}/package.json" >/dev/null; then
  echo "::error::Committed npm package identity does not match ${VERSION}."
  exit 1
fi

if [[ ! -f "${PACKAGE_TARBALL}" ]] || [[ -L "${PACKAGE_TARBALL}" ]]; then
  echo "::error::The exact npm package tarball is missing or unsafe."
  exit 1
fi

if [[ ! "${EXPECTED_INTEGRITY}" =~ ^sha512-[A-Za-z0-9+/]+={0,2}$ ]]; then
  echo "::error::The expected npm package integrity is malformed."
  exit 1
fi

actual_integrity="$(
  PACKAGE_TARBALL="${PACKAGE_TARBALL}" node <<'NODE'
const crypto = require("node:crypto");
const fs = require("node:fs");

const contents = fs.readFileSync(process.env.PACKAGE_TARBALL);
const digest = crypto.createHash("sha512").update(contents).digest("base64");
process.stdout.write(`sha512-${digest}`);
NODE
)"
if [[ "${actual_integrity}" != "${EXPECTED_INTEGRITY}" ]]; then
  echo "::error::The npm package tarball changed after its registry-state check."
  exit 1
fi

NODE_AUTH_TOKEN="${NODE_AUTH_TOKEN:-${NPM_TOKEN:-}}"
if [[ -z "${NODE_AUTH_TOKEN}" ]]; then
  echo "::error::npm publication requires an effective auth token."
  exit 1
fi
export NODE_AUTH_TOKEN

echo "Publishing hooklistener@${VERSION}..."
npm publish "${PACKAGE_TARBALL}" --access public --ignore-scripts

echo "npm package published successfully!"
