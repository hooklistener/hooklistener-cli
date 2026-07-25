#!/usr/bin/env python3

import argparse
import re


STABLE_VERSION = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$"
)
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--arm64-sha", required=True)
    parser.add_argument("--x86-64-sha", required=True)
    parser.add_argument("--linux-sha", required=True)
    return parser.parse_args()


def main() -> None:
    args = arguments()
    if STABLE_VERSION.fullmatch(args.version) is None:
        raise SystemExit("Homebrew formula version must be a stable semantic version")

    digests = {
        "arm64": args.arm64_sha,
        "x86_64": args.x86_64_sha,
        "linux": args.linux_sha,
    }
    invalid = [name for name, digest in digests.items() if SHA256.fullmatch(digest) is None]
    if invalid:
        raise SystemExit(
            "Homebrew formula digests must be lowercase SHA-256 values: "
            + ", ".join(invalid)
        )

    print(
        f"""# typed: false
# frozen_string_literal: true

class Hooklistener < Formula
  desc "CLI tool for webhook inspection and local tunnel exposure"
  homepage "https://github.com/hooklistener/hooklistener-cli"
  version "{args.version}"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/hooklistener/hooklistener-cli/releases/download/v#{{version}}/hooklistener-aarch64-apple-darwin.tar.gz"
      sha256 "{args.arm64_sha}"
    end

    on_intel do
      url "https://github.com/hooklistener/hooklistener-cli/releases/download/v#{{version}}/hooklistener-x86_64-apple-darwin.tar.gz"
      sha256 "{args.x86_64_sha}"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/hooklistener/hooklistener-cli/releases/download/v#{{version}}/hooklistener-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "{args.linux_sha}"
    end
  end

  def install
    bin.install "hooklistener"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/hooklistener --version")
  end
end"""
    )


if __name__ == "__main__":
    main()
