#!/usr/bin/env node

"use strict";

const { execFileSync } = require("child_process");
const path = require("path");

const PLATFORMS = {
  "linux-x64": "@hooklistener/hooklistener-linux-x64",
  "darwin-x64": "@hooklistener/hooklistener-darwin-x64",
  "darwin-arm64": "@hooklistener/hooklistener-darwin-arm64",
  "win32-x64": "@hooklistener/hooklistener-win32-x64",
};

const platformKey = `${process.platform}-${process.arch}`;
const pkg = PLATFORMS[platformKey];

if (!pkg) {
  console.error(
    `Unsupported platform: ${process.platform}-${process.arch}\n` +
    `hooklistener currently supports: ${Object.keys(PLATFORMS).join(", ")}`
  );
  process.exit(1);
}

let binPath;
try {
  const pkgDir = path.dirname(require.resolve(`${pkg}/package.json`));
  const binName = process.platform === "win32" ? "hooklistener-cli.exe" : "hooklistener-cli";
  binPath = path.join(pkgDir, "bin", binName);
} catch {
  console.error(
    `The package ${pkg} could not be found. This usually means the optional\n` +
    `dependency was not installed. Try reinstalling with:\n\n` +
    `  npm install -g hooklistener\n`
  );
  process.exit(1);
}

const args = process.argv.slice(2);

try {
  const result = execFileSync(binPath, args, {
    stdio: "inherit",
    windowsHide: false,
  });
  process.exit(0);
} catch (err) {
  if (err.status !== null) {
    process.exit(err.status);
  }
  throw err;
}
