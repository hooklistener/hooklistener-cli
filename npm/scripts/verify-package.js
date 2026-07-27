"use strict";

const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const PACKAGE_DIRECTORY = path.join(__dirname, "..", "packages", "hooklistener");
const NPM_COMMAND = process.platform === "win32" ? "npm.cmd" : "npm";
const EXPECTED_FILES = new Map([
  ["README.md", 0o644],
  ["bin/hooklistener.js", 0o755],
  ["package.json", 0o644],
  ["scripts/install.js", 0o644],
]);

function validatePackageReport(report, expectedVersion) {
  if (!Array.isArray(report) || report.length !== 1) {
    throw new Error("npm pack must produce exactly one package");
  }

  const packageReport = report[0];
  if (
    packageReport.name !== "hooklistener" ||
    packageReport.version !== expectedVersion
  ) {
    throw new Error(
      `unexpected npm package identity: ${packageReport.name}@${packageReport.version}`
    );
  }

  const actualFiles = new Map(
    packageReport.files.map(({ path: filePath, mode }) => [filePath, mode])
  );
  if (
    packageReport.files.length !== EXPECTED_FILES.size ||
    actualFiles.size !== EXPECTED_FILES.size ||
    [...EXPECTED_FILES.keys()].some((filePath) => !actualFiles.has(filePath))
  ) {
    throw new Error(
      `unexpected npm package contents: ${[...actualFiles.keys()].sort().join(", ")}`
    );
  }

  for (const [filePath, expectedMode] of EXPECTED_FILES) {
    const actualMode = actualFiles.get(filePath);
    if (actualMode !== expectedMode) {
      throw new Error(
        `unexpected mode for ${filePath}: expected ${expectedMode.toString(8)}, got ${actualMode?.toString(8)}`
      );
    }
  }
}

function main() {
  const expectedVersion =
    process.argv[2] ??
    JSON.parse(
      fs.readFileSync(path.join(PACKAGE_DIRECTORY, "package.json"), "utf8")
    ).version;
  const reportPath = process.argv[3];

  const reportContents = reportPath
    ? fs.readFileSync(reportPath, "utf8")
    : execFileSync(
        NPM_COMMAND,
        ["pack", "--dry-run", "--json", "--ignore-scripts", PACKAGE_DIRECTORY],
        { encoding: "utf8" }
      );
  validatePackageReport(JSON.parse(reportContents), expectedVersion);
}

if (require.main === module) {
  main();
}

module.exports = { validatePackageReport };
