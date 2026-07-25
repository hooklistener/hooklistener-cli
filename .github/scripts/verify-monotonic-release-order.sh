#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=retry-read.sh
source "${script_directory}/retry-read.sh"

tag_name="${1:-}"
stage="${2:-}"

if [[ ! "${tag_name}" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "::error::Invalid stable release tag." >&2
  exit 1
fi

case "${stage}" in
  verify | prerelease-create | prerelease-resume | \
    crates-publish | npm-publish | homebrew-push | promote)
    ;;
  *)
    echo "::error::Invalid release-order stage." >&2
    exit 1
    ;;
esac

if [[ -z "${GH_TOKEN:-}" ]] || [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
  echo "::error::Release-order verification requires GH_TOKEN and GITHUB_REPOSITORY." >&2
  exit 1
fi

order_directory="$(
  mktemp -d \
    "${RUNNER_TEMP:-/tmp}/hooklistener-release-order.XXXXXX"
)"
github_releases="${order_directory}/github-releases.json"
crates_state="${order_directory}/crates.json"
npm_state="${order_directory}/npm.json"

if ! retry_read_to_file "${github_releases}" gh api \
  --header "Accept: application/vnd.github+json" \
  --header "X-GitHub-Api-Version: 2022-11-28" \
  --paginate \
  --slurp \
  "repos/${GITHUB_REPOSITORY}/releases?per_page=100"; then
  echo "::error::Could not read the complete GitHub release history." >&2
  exit 1
fi

fetch_registry() {
  local output="$1"
  local url="$2"
  shift 2

  local status
  if ! status="$(
    curl \
      --silent \
      --show-error \
      --connect-timeout 15 \
      --max-time 60 \
      --retry 2 \
      --retry-delay 2 \
      --retry-max-time 120 \
      --retry-connrefused \
      --retry-all-errors \
      --output "${output}" \
      --write-out '%{http_code}' \
      "$@" \
      "${url}"
  )"; then
    return 1
  fi

  [[ "${status}" == "200" ]]
}

if ! fetch_registry \
  "${crates_state}" \
  "https://crates.io/api/v1/crates/hooklistener-cli" \
  --header \
  "User-Agent: hooklistener-release-workflow (${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY})"; then
  echo "::error::Could not read the complete crates.io release history." >&2
  exit 1
fi

if ! fetch_registry \
  "${npm_state}" \
  "https://registry.npmjs.org/hooklistener"; then
  echo "::error::Could not read the complete npm release history." >&2
  exit 1
fi

TAG_NAME="${tag_name}" \
  RELEASE_STAGE="${stage}" \
  GITHUB_RELEASES_JSON="${github_releases}" \
  CRATES_JSON="${crates_state}" \
  NPM_JSON="${npm_state}" \
  python3 <<'PY'
import json
import os
import re

stable = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$"
)


def parse(value: object, source: str, *, prefix: bool = False) -> tuple[int, int, int]:
    if not isinstance(value, str):
        raise SystemExit(f"{source} did not return a stable semantic version")
    normalized = value.removeprefix("v") if prefix else value
    match = stable.fullmatch(normalized)
    if match is None:
        raise SystemExit(f"{source} returned unsupported version {value!r}")
    return tuple(int(part) for part in match.groups())


def stable_versions(values: list[object], source: str) -> list[tuple[int, int, int]]:
    parsed: list[tuple[int, int, int]] = []
    for value in values:
        if isinstance(value, str) and stable.fullmatch(value):
            parsed.append(parse(value, source))
    if not parsed:
        raise SystemExit(f"{source} returned no stable releases")
    return parsed


target_tag = os.environ["TAG_NAME"]
target = parse(target_tag, "release tag", prefix=True)
stage = os.environ["RELEASE_STAGE"]

with open(os.environ["GITHUB_RELEASES_JSON"], encoding="utf-8") as release_file:
    release_pages = json.load(release_file)
if not isinstance(release_pages, list) or not all(
    isinstance(page, list) for page in release_pages
):
    raise SystemExit("GitHub releases did not use the expected paginated shape")
releases = [release for page in release_pages for release in page]
if not all(isinstance(release, dict) for release in releases):
    raise SystemExit("GitHub returned a malformed release entry")

exact_releases = [
    release for release in releases if release.get("tag_name") == target_tag
]
if len(exact_releases) > 1:
    raise SystemExit(f"GitHub returned duplicate releases for {target_tag}")

exact_state = "absent"
if exact_releases:
    exact = exact_releases[0]
    if exact.get("draft") is not False:
        raise SystemExit("the exact GitHub release must never be a draft")
    if exact.get("prerelease") is True:
        exact_state = "prerelease"
    elif exact.get("prerelease") is False:
        exact_state = "stable"
    else:
        raise SystemExit("the exact GitHub release has an invalid prerelease state")

github_versions: list[tuple[int, int, int]] = []
for release in releases:
    if release.get("draft") is True:
        continue
    if release.get("draft") is not False or not isinstance(
        release.get("prerelease"), bool
    ):
        raise SystemExit("GitHub returned an invalid public release state")
    github_versions.append(
        parse(release.get("tag_name"), "GitHub release", prefix=True)
    )
if not github_versions:
    raise SystemExit("GitHub returned no public releases")

with open(os.environ["CRATES_JSON"], encoding="utf-8") as crates_file:
    crates = json.load(crates_file)
crate_entries = crates.get("versions")
if not isinstance(crate_entries, list):
    raise SystemExit("crates.io returned no version inventory")
crate_values = [
    entry.get("num")
    for entry in crate_entries
    if isinstance(entry, dict)
]
available_crate_values = [
    entry.get("num")
    for entry in crate_entries
    if isinstance(entry, dict) and entry.get("yanked") is False
]

with open(os.environ["NPM_JSON"], encoding="utf-8") as npm_file:
    npm = json.load(npm_file)
npm_inventory = npm.get("versions")
if not isinstance(npm_inventory, dict):
    raise SystemExit("npm returned no version inventory")

crate_versions = stable_versions(crate_values, "crates.io")
available_crate_versions = stable_versions(
    available_crate_values,
    "crates.io available versions",
)
npm_versions = stable_versions(list(npm_inventory), "npm")
current_versions = {
    "GitHub": max(github_versions),
    "crates.io": max(crate_versions),
    "npm": max(npm_versions),
}
newer = {
    source: version
    for source, version in current_versions.items()
    if version > target
}
equal_sources = [
    source for source, version in current_versions.items() if version == target
]

if stage == "verify":
    if exact_state == "stable":
        raise SystemExit(f"{target_tag} is already a stable GitHub release")
    if equal_sources and exact_state != "prerelease":
        raise SystemExit(
            f"{target_tag} already exists on {', '.join(equal_sources)} "
            "without a resumable GitHub prerelease"
        )
elif stage == "prerelease-create":
    if exact_state != "absent":
        raise SystemExit(f"{target_tag} already has a GitHub release")
    if equal_sources:
        raise SystemExit(
            f"{target_tag} already exists on {', '.join(equal_sources)}"
        )
elif stage in {
    "prerelease-resume",
    "crates-publish",
    "npm-publish",
    "homebrew-push",
}:
    if exact_state != "prerelease":
        raise SystemExit(
            f"{stage} requires the exact resumable GitHub prerelease"
        )
elif stage == "promote":
    if exact_state == "absent":
        raise SystemExit("promotion requires the exact GitHub release")
    missing_registries = []
    if target not in available_crate_versions:
        missing_registries.append("crates.io")
    if target not in npm_versions:
        missing_registries.append("npm")
    if missing_registries:
        raise SystemExit(
            f"promote requires {target_tag} to be visible on "
            + " and ".join(missing_registries)
        )
    if newer and exact_state == "stable":
        print("superseded")
        raise SystemExit(0)

if stage == "homebrew-push":
    missing_registries = []
    if target not in available_crate_versions:
        missing_registries.append("crates.io")
    if target not in npm_versions:
        missing_registries.append("npm")
    if missing_registries:
        raise SystemExit(
            f"homebrew-push requires {target_tag} to be visible on "
            + " and ".join(missing_registries)
        )

if newer:
    rendered = ", ".join(
        f"{source} v{'.'.join(map(str, version))}"
        for source, version in sorted(newer.items())
    )
    raise SystemExit(f"release {target_tag} would move backward from {rendered}")

print("promote" if stage == "promote" else "proceed")
PY
