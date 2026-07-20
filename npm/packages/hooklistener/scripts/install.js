#!/usr/bin/env node

"use strict";

const https = require("https");
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");
const os = require("os");

const REPO = "hooklistener/hooklistener-cli";
const CHECKSUM_MANIFEST_MAX_BYTES = 64 * 1024;
const POWERSHELL_EXPAND_ARCHIVE_SCRIPT = [
  "param(",
  "  [Parameter(Mandatory=$true, Position=0)][string]$ArchivePath,",
  "  [Parameter(Mandatory=$true, Position=1)][string]$DestinationPath",
  ")",
  'Set-StrictMode -Version "Latest"',
  '$ErrorActionPreference = "Stop"',
  "Expand-Archive -LiteralPath $ArchivePath -DestinationPath $DestinationPath -Force",
].join("\r\n");

const PLATFORM_MAP = {
  "linux-x64": { target: "x86_64-unknown-linux-gnu", archive: "tar.gz" },
  "darwin-x64": { target: "x86_64-apple-darwin", archive: "tar.gz" },
  "darwin-arm64": { target: "aarch64-apple-darwin", archive: "tar.gz" },
  "win32-x64": { target: "x86_64-pc-windows-msvc", archive: "zip" },
};

const BIN_NAME = process.platform === "win32" ? "hooklistener.exe" : "hooklistener";
const BIN_DIR = path.join(__dirname, "..", "native");
const BIN_PATH = path.join(BIN_DIR, BIN_NAME);

function getPackageVersion() {
  const pkgPath = path.join(__dirname, "..", "package.json");
  return JSON.parse(fs.readFileSync(pkgPath, "utf8")).version;
}

function download(url, maxBytes = Number.POSITIVE_INFINITY) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "hooklistener-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return download(res.headers.location, maxBytes).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const chunks = [];
        let downloadedBytes = 0;
        res.on("data", (chunk) => {
          downloadedBytes += chunk.length;
          if (downloadedBytes > maxBytes) {
            res.destroy();
            reject(new Error(`Download exceeds ${maxBytes} bytes`));
            return;
          }
          chunks.push(chunk);
        });
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

function parseChecksumManifest(manifest, archiveName) {
  const matches = [];

  for (const line of manifest.toString("utf8").split(/\r?\n/)) {
    const fields = line.trim().split(/\s+/);
    if (fields.length < 2) {
      continue;
    }

    const listedName = fields[1].replace(/^\*/, "");
    if (listedName !== archiveName) {
      continue;
    }

    if (fields.length !== 2 || !/^[a-fA-F0-9]{64}$/.test(fields[0])) {
      throw new Error(`Malformed checksum entry for ${archiveName}`);
    }

    matches.push(fields[0].toLowerCase());
  }

  if (matches.length === 0) {
    throw new Error(`Checksum manifest is missing an entry for ${archiveName}`);
  }
  if (matches.length !== 1) {
    throw new Error(`Checksum manifest has duplicate entries for ${archiveName}`);
  }

  return matches[0];
}

function verifyChecksum(buffer, expectedChecksum) {
  if (!/^[a-f0-9]{64}$/.test(expectedChecksum)) {
    throw new Error("Expected checksum is not a valid SHA-256 digest");
  }

  const actualChecksum = crypto.createHash("sha256").update(buffer).digest("hex");
  if (actualChecksum !== expectedChecksum) {
    throw new Error(
      `Checksum verification failed: expected ${expectedChecksum}, got ${actualChecksum}`
    );
  }
}

function writeExclusivePrivateFile(filePath, contents) {
  fs.writeFileSync(filePath, contents, { flag: "wx", mode: 0o600 });
}

function extractArchive(buffer, archiveType, destDir, options = {}) {
  if (archiveType !== "zip" && archiveType !== "tar.gz") {
    throw new Error(`Unsupported archive type: ${archiveType}`);
  }

  const platform = options.platform ?? process.platform;
  const temporaryRoot = options.temporaryRoot ?? os.tmpdir();
  const runCommand = options.execFileSync ?? execFileSync;
  const temporaryDirectory = fs.mkdtempSync(
    path.join(temporaryRoot, "hooklistener-install-")
  );

  try {
    fs.mkdirSync(destDir, { recursive: true });
    const archivePath = path.join(temporaryDirectory, `archive.${archiveType}`);
    writeExclusivePrivateFile(archivePath, buffer);

    if (archiveType === "zip") {
      if (platform === "win32") {
        const scriptPath = path.join(temporaryDirectory, "expand-archive.ps1");
        writeExclusivePrivateFile(scriptPath, POWERSHELL_EXPAND_ARCHIVE_SCRIPT);
        runCommand(
          "powershell.exe",
          [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            scriptPath,
            archivePath,
            destDir,
          ],
          { stdio: "pipe", windowsHide: true }
        );
      } else {
        runCommand("unzip", ["-o", archivePath, "-d", destDir], {
          stdio: "pipe",
        });
      }
    } else {
      runCommand("tar", ["-xzf", archivePath, "-C", destDir], {
        stdio: "pipe",
      });
    }
  } finally {
    fs.rmSync(temporaryDirectory, { recursive: true, force: true });
  }
}

function markExecutable(filePath) {
  if (process.platform !== "win32") {
    fs.chmodSync(filePath, 0o755);
  }
}

async function main() {
  const envBinary = process.env.HOOKLISTENER_BINARY_PATH;
  if (envBinary) {
    console.log(`Using binary from HOOKLISTENER_BINARY_PATH: ${envBinary}`);
    fs.mkdirSync(BIN_DIR, { recursive: true });
    fs.copyFileSync(envBinary, BIN_PATH);
    markExecutable(BIN_PATH);
    return;
  }

  const platformKey = `${process.platform}-${process.arch}`;
  const platform = PLATFORM_MAP[platformKey];

  if (!platform) {
    console.error(
      `Unsupported platform: ${platformKey}\n` +
        `hooklistener currently supports: ${Object.keys(PLATFORM_MAP).join(", ")}`
    );
    process.exit(1);
  }

  const version = getPackageVersion();
  const archiveName = `${BIN_NAME}-${platform.target}.${platform.archive}`;
  const url = `https://github.com/${REPO}/releases/download/v${version}/${archiveName}`;
  const checksumsUrl = `https://github.com/${REPO}/releases/download/v${version}/SHA256SUMS.txt`;

  console.log(`Downloading hooklistener v${version} for ${platformKey}...`);

  try {
    const checksumManifest = await download(
      checksumsUrl,
      CHECKSUM_MANIFEST_MAX_BYTES
    );
    const expectedChecksum = parseChecksumManifest(checksumManifest, archiveName);
    const buffer = await download(url);
    verifyChecksum(buffer, expectedChecksum);
    extractArchive(buffer, platform.archive, BIN_DIR);

    if (!fs.existsSync(BIN_PATH)) {
      console.error(`Binary not found at expected path: ${BIN_PATH}`);
      process.exit(1);
    }

    markExecutable(BIN_PATH);
    console.log(`hooklistener v${version} installed successfully.`);
  } catch (err) {
    console.error(`Failed to install hooklistener v${version}: ${err.message}`);
    console.error(
      `\nYou can manually install the binary and set HOOKLISTENER_BINARY_PATH to its location.`
    );
    process.exit(1);
  }
}

if (require.main === module) {
  main();
}

module.exports = { extractArchive, parseChecksumManifest, verifyChecksum };
