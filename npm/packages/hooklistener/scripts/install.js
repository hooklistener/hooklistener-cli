#!/usr/bin/env node

"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const { execSync } = require("child_process");
const os = require("os");

const REPO = "hooklistener/hooklistener-cli";

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

function download(url) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "hooklistener-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return download(res.headers.location).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

function extractArchive(buffer, archiveType, destDir) {
  const tmpFile = path.join(os.tmpdir(), `hooklistener-${Date.now()}.${archiveType}`);
  fs.writeFileSync(tmpFile, buffer);
  try {
    fs.mkdirSync(destDir, { recursive: true });
    if (archiveType === "zip") {
      if (process.platform === "win32") {
        execSync(
          `powershell -Command "Expand-Archive -Path '${tmpFile}' -DestinationPath '${destDir}' -Force"`,
          { stdio: "pipe" }
        );
      } else {
        execSync(`unzip -o "${tmpFile}" -d "${destDir}"`, { stdio: "pipe" });
      }
    } else {
      execSync(`tar -xzf "${tmpFile}" -C "${destDir}"`, { stdio: "pipe" });
    }
  } finally {
    fs.unlinkSync(tmpFile);
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
  const archiveName = `hooklistener-${platform.target}.${platform.archive}`;
  const url = `https://github.com/${REPO}/releases/download/v${version}/${archiveName}`;

  console.log(`Downloading hooklistener v${version} for ${platformKey}...`);

  try {
    const buffer = await download(url);
    extractArchive(buffer, platform.archive, BIN_DIR);

    if (!fs.existsSync(BIN_PATH)) {
      console.error(`Binary not found at expected path: ${BIN_PATH}`);
      process.exit(1);
    }

    markExecutable(BIN_PATH);
    console.log(`hooklistener v${version} installed successfully.`);
  } catch (err) {
    console.error(`Failed to download hooklistener v${version}: ${err.message}`);
    console.error(
      `\nYou can manually install the binary and set HOOKLISTENER_BINARY_PATH to its location.`
    );
    process.exit(1);
  }
}

main();
