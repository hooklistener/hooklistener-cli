#!/usr/bin/env node

"use strict";

const { execFileSync } = require("child_process");
const path = require("path");
const fs = require("fs");

const BIN_NAME = process.platform === "win32" ? "hooklistener.exe" : "hooklistener";
const BIN_PATH = path.join(__dirname, "..", "native", BIN_NAME);

if (!fs.existsSync(BIN_PATH)) {
  console.error(
    `hooklistener binary not found at ${BIN_PATH}\n\n` +
      `This usually means the postinstall script failed to download the binary.\n` +
      `Try reinstalling:\n\n` +
      `  npm install -g hooklistener\n\n` +
      `Or set HOOKLISTENER_BINARY_PATH to a pre-downloaded binary and reinstall.`
  );
  process.exit(1);
}

try {
  execFileSync(BIN_PATH, process.argv.slice(2), { stdio: "inherit" });
} catch (err) {
  if (err.status !== null) {
    process.exit(err.status);
  }
  throw err;
}
