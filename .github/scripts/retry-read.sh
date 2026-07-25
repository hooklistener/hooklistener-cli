#!/usr/bin/env bash

retry_read() {
  local attempt
  local status=1

  for attempt in 1 2 3; do
    if "$@"; then
      return 0
    else
      status=$?
    fi

    if [[ "${attempt}" -lt 3 ]]; then
      echo "::warning::Read-only command failed on attempt ${attempt}; retrying." >&2
      sleep "$((attempt * 2))"
    fi
  done

  return "${status}"
}

retry_read_capture() {
  local attempt
  local captured
  local status=1

  for attempt in 1 2 3; do
    if captured="$("$@")"; then
      printf '%s' "${captured}"
      return 0
    else
      status=$?
    fi

    if [[ "${attempt}" -lt 3 ]]; then
      echo "::warning::Read-only command failed on attempt ${attempt}; retrying." >&2
      sleep "$((attempt * 2))"
    fi
  done

  return "${status}"
}

retry_read_to_file() {
  local output="$1"
  local attempt
  local attempt_output
  local status=1
  shift

  for attempt in 1 2 3; do
    attempt_output="${output}.attempt-${attempt}"
    rm -f "${attempt_output}"
    if "$@" > "${attempt_output}"; then
      mv "${attempt_output}" "${output}"
      return 0
    else
      status=$?
      rm -f "${attempt_output}"
    fi

    if [[ "${attempt}" -lt 3 ]]; then
      echo "::warning::Read-only command failed on attempt ${attempt}; retrying." >&2
      sleep "$((attempt * 2))"
    fi
  done

  return "${status}"
}
