#!/usr/bin/env python3
"""Measure this crate in an isolated source/target directory; never clean the checkout."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import time


ROOT = Path(__file__).resolve().parents[1]
INPUTS = ("Cargo.toml", "Cargo.lock", "src", "fixtures")


def snapshot_source(root, destination):
    """Copy the current working sources, including uncommitted changes."""
    destination.mkdir()
    for name in INPUTS:
        source = root / name
        if source.is_dir():
            shutil.copytree(source, destination / name)
        else:
            shutil.copy2(source, destination / name)
    # Keep project Cargo configuration if one is introduced later.
    if (root / ".cargo").exists():
        shutil.copytree(root / ".cargo", destination / ".cargo")
    digest = hashlib.sha256()
    for path in sorted(destination.rglob("*")):
        if path.is_file():
            digest.update(path.relative_to(destination).as_posix().encode() + b"\0")
            digest.update(hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def archive_binary(binary, archive):
    """Binary-only, deterministic ustar/gzip at level 6, comparable across runs."""
    with archive.open("wb") as output:
        with gzip.GzipFile(filename="", fileobj=output, mode="wb", mtime=0,
                           compresslevel=6) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|", format=tarfile.USTAR_FORMAT) as tar:
                entry = tarfile.TarInfo(binary.name)
                entry.size = binary.stat().st_size
                entry.mode = 0o755
                entry.uname = entry.gname = "root"
                with binary.open("rb") as content:
                    tar.addfile(entry, content)
    return archive.stat().st_size


def timed_build(command, source, log):
    start = time.perf_counter()
    with log.open("w", encoding="utf-8") as output:
        result = subprocess.run(command, cwd=source, stdout=output,
                                stderr=subprocess.STDOUT, check=False)
    elapsed = time.perf_counter() - start
    if result.returncode:
        raise RuntimeError(f"Build failed ({result.returncode}); see {log}")
    return round(elapsed, 3)


def measure(root, output, profile, jobs):
    # Refuse reuse: a 'clean' measurement must not reuse compiled artifacts.
    for name in (*INPUTS, ".cargo"):
        if output == root / name or root / name in output.parents:
            raise ValueError("Output must be outside the copied build inputs")
    output.mkdir(parents=True, exist_ok=False)
    source = output / "source"
    source_hash = snapshot_source(root, source)
    rustc = subprocess.check_output(["rustc", "-vV"], text=True).strip()
    host = next(line.removeprefix("host: ") for line in rustc.splitlines()
                if line.startswith("host: "))
    command = ["cargo", "build", "--locked", "--offline", "--timings",
               "--bin", "hooklistener", "--profile", profile, "--target", host,
               "--target-dir", str(output / "target"), "--jobs", str(jobs)]
    report = {
        "schema_version": 1,
        "source_sha256": source_hash,
        "cargo_toml_sha256": hashlib.sha256((source / "Cargo.toml").read_bytes()).hexdigest(),
        "cargo_lock_sha256": hashlib.sha256((source / "Cargo.lock").read_bytes()).hexdigest(),
        "rustc": rustc,
        "cargo": subprocess.check_output(["cargo", "--version"], text=True).strip(),
        "platform": platform.platform(),
        "logical_cpus": os.cpu_count(),
        "profile": profile,
        "jobs": jobs,
        "command": command,
        "build_environment": {key: value for key, value in os.environ.items()
                              if key.startswith("CARGO_PROFILE_") or key in (
                                  "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER",
                                  "RUSTC_WORKSPACE_WRAPPER", "CARGO_INCREMENTAL")},
        "seconds": {},
    }
    report_path = output / "report.json"
    for stage in ("clean", "noop", "source_rebuild"):
        if stage == "source_rebuild":
            # Trigger only the root crate in the private copy. This is a warm
            # rebuild, NOT an incremental compilation benchmark in release mode.
            with (source / "src/main.rs").open("a", encoding="utf-8") as main:
                main.write("\n// Build measurement: root-crate rebuild probe.\n")
        print(f"{profile}: {stage} build (log: {output / (stage + '.log')})", flush=True)
        report["seconds"][stage] = timed_build(command, source, output / f"{stage}.log")
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    directory = "debug" if profile == "dev" else "release"
    name = "hooklistener.exe" if "windows" in host else "hooklistener"
    binary = output / "target" / host / directory / name
    report["binary_bytes"] = binary.stat().st_size
    report["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    report["tar_gzip_bytes"] = archive_binary(binary, output / "hooklistener.tar.gz")
    # --version and --help exit before authentication or network activity.
    report["version"] = subprocess.check_output([str(binary), "--version"], text=True).strip()
    subprocess.run([str(binary), "--help"], stdout=subprocess.DEVNULL, check=True)
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True,
                        help="new directory for source copy, artifacts, logs, and report.json")
    parser.add_argument("--profile", choices=("dev", "release"), default="release")
    parser.add_argument("--jobs", type=int, default=8)
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    try:
        measure(ROOT, args.output.resolve(), args.profile, args.jobs)
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
