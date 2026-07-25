#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=retry-read.sh
source "${script_directory}/retry-read.sh"

tag_name="${1:-}"
source_sha="${2:-}"

if [[ ! "${tag_name}" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "::error::Invalid stable release tag."
  exit 1
fi

if [[ ! "${source_sha}" =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::Invalid workflow source SHA."
  exit 1
fi

if ! remote_refs="$(
  retry_read_capture \
    git ls-remote \
    --exit-code \
    origin \
    "refs/tags/${tag_name}" \
    "refs/tags/${tag_name}^{}"
)"; then
  echo "::error::Could not resolve remote release tag ${tag_name}."
  exit 1
fi

if ! tag_commit="$(
  awk \
    -v direct="refs/tags/${tag_name}" \
    -v peeled="refs/tags/${tag_name}^{}" '
      NF != 2 { exit 1 }
      $2 == direct {
        direct_sha = $1
        direct_count += 1
        next
      }
      $2 == peeled {
        peeled_sha = $1
        peeled_count += 1
        next
      }
      { exit 1 }
      END {
        if (direct_count != 1 || peeled_count != 1) exit 1
        print peeled_sha
      }
    ' <<<"${remote_refs}"
)"; then
  echo "::error::Remote release tag ${tag_name} must be one unambiguous annotated tag."
  exit 1
fi

if [[ "${tag_commit}" != "${source_sha}" ]]; then
  echo "::error::Release tag ${tag_name} moved away from the workflow commit."
  exit 1
fi

echo "Remote release tag verified: ${tag_name} -> ${source_sha}"
