#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=retry-read.sh
source "${script_directory}/retry-read.sh"

if [[ -z "${GH_TOKEN:-}" ]] || [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
  echo "::error::Release governance requires GH_TOKEN and GITHUB_REPOSITORY."
  exit 1
fi

main_rules="${RUNNER_TEMP:-/tmp}/hooklistener-effective-main-rules.json"
tag_rulesets="${RUNNER_TEMP:-/tmp}/hooklistener-tag-rulesets.json"
release_environment="${RUNNER_TEMP:-/tmp}/hooklistener-release-environment.json"
release_policies="${RUNNER_TEMP:-/tmp}/hooklistener-release-policies.json"

api_headers=(
  --header "Accept: application/vnd.github+json"
  --header "X-GitHub-Api-Version: 2022-11-28"
)

retry_read_to_file "${main_rules}" gh api \
  "${api_headers[@]}" \
  --paginate \
  --slurp \
  "repos/${GITHUB_REPOSITORY}/rules/branches/main?per_page=100"

# The endpoint returns only active rules that apply to main. GitHub layers every
# matching ruleset and applies the most restrictive version of duplicate rules,
# so strictness and required checks are intentionally evaluated across that
# complete effective set.
if ! jq -e '
  [
    "Rustfmt",
    "Clippy",
    "Tests (stable)",
    "Cargo Audit",
    "Analyze",
    "Build (x86_64-unknown-linux-gnu)",
    "Build (x86_64-pc-windows-msvc)",
    "Build (x86_64-apple-darwin)",
    "Build (aarch64-apple-darwin)",
    "Authenticated lifecycle (linux)",
    "Authenticated lifecycle (macos)",
    "Authenticated lifecycle (windows)"
  ] as $expected_checks
  | add as $rules
  | [
      $rules[]
      | select(.type == "required_status_checks")
      | .parameters.required_status_checks[]?
    ] as $actual_checks
  | any($rules[]; .type == "deletion")
    and any($rules[]; .type == "non_fast_forward")
    and any(
      $rules[];
      .type == "pull_request"
    )
    and any(
      $rules[];
      .type == "required_status_checks"
      and .parameters.strict_required_status_checks_policy == true
    )
    and all(
      $expected_checks[];
      . as $expected
      | any(
          $actual_checks[];
          .context == $expected and .integration_id == 15368
        )
    )
' "${main_rules}" >/dev/null; then
  echo "::error::Effective main rules do not enforce the complete release check policy."
  exit 1
fi

retry_read_to_file "${tag_rulesets}" gh api \
  "${api_headers[@]}" \
  --paginate \
  --slurp \
  "repos/${GITHUB_REPOSITORY}/rulesets?targets=tag&per_page=100"

mapfile -t active_tag_ruleset_ids < <(
  jq -r '
    [
      .[][]
      | select(.target == "tag" and .enforcement == "active")
      | .id
    ]
    | unique[]
  ' "${tag_rulesets}"
)

tag_creation_policy_ready=false
tag_immutability_policy_ready=false
for ruleset_id in "${active_tag_ruleset_ids[@]}"; do
  ruleset_path="${RUNNER_TEMP:-/tmp}/hooklistener-ruleset-${ruleset_id}.json"
  retry_read_to_file "${ruleset_path}" gh api \
    "${api_headers[@]}" \
    "repos/${GITHUB_REPOSITORY}/rulesets/${ruleset_id}"

  if jq -e '
    .target == "tag"
    and .enforcement == "active"
    and (
      (.conditions.ref_name.include // [])
      | index("refs/tags/v*.*.*") != null
    )
    and ((.conditions.ref_name.exclude // []) | length) == 0
    and (
      [.rules[]?.type] as $types
      | ($types | index("creation") != null)
        and ($types | index("update") == null)
        and ($types | index("deletion") == null)
    )
  ' "${ruleset_path}" >/dev/null; then
    tag_creation_policy_ready=true
  fi

  if jq -e '
    .target == "tag"
    and .enforcement == "active"
    and (
      (.conditions.ref_name.include // [])
      | index("refs/tags/v*.*.*") != null
    )
    and ((.conditions.ref_name.exclude // []) | length) == 0
    and (
      [.rules[]?.type] as $types
      | ($types | index("creation") == null)
        and ($types | index("update") != null)
        and ($types | index("deletion") != null)
    )
  ' "${ruleset_path}" >/dev/null; then
    tag_immutability_policy_ready=true
  fi
done

if [[ "${tag_creation_policy_ready}" != "true" ]]; then
  echo "::error::No dedicated active v*.*.* tag-creation ruleset is configured."
  exit 1
fi

if [[ "${tag_immutability_policy_ready}" != "true" ]]; then
  echo "::error::No separate active v*.*.* tag ruleset restricts update and deletion."
  exit 1
fi

if ! retry_read_to_file "${release_environment}" gh api \
  "${api_headers[@]}" \
  "repos/${GITHUB_REPOSITORY}/environments/release"; then
  echo "::error::The protected release environment does not exist."
  exit 1
fi

if ! jq -e '
  .name == "release"
  and .deployment_branch_policy.protected_branches == false
  and .deployment_branch_policy.custom_branch_policies == true
' "${release_environment}" >/dev/null; then
  echo "::error::The release environment must exist and use a custom deployment policy."
  exit 1
fi

retry_read_to_file "${release_policies}" gh api \
  "${api_headers[@]}" \
  --paginate \
  --slurp \
  "repos/${GITHUB_REPOSITORY}/environments/release/deployment-branch-policies?per_page=100"

if ! jq -e '
  ([.[].branch_policies[]?]) as $policies
  | ($policies | length) == 1
    and $policies[0].name == "v*.*.*"
' "${release_policies}" >/dev/null; then
  echo "::error::The release environment must allow only the v*.*.* deployment pattern."
  exit 1
fi

echo "Release governance metadata verified."
