"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");

const { validatePackageReport } = require("./verify-package");

const VERSION = "1.8.5";

function packageReport(launcherMode = 0o755) {
  return [
    {
      name: "hooklistener",
      version: VERSION,
      files: [
        { path: "README.md", mode: 0o644 },
        { path: "bin/hooklistener.js", mode: launcherMode },
        { path: "package.json", mode: 0o644 },
        { path: "scripts/install.js", mode: 0o644 },
      ],
    },
  ];
}

test("accepts an npm package with an executable launcher", () => {
  assert.doesNotThrow(() => validatePackageReport(packageReport(), VERSION));
});

test("rejects an npm package with a non-executable launcher", () => {
  assert.throws(
    () => validatePackageReport(packageReport(0o644), VERSION),
    /unexpected mode for bin\/hooklistener\.js: expected 755, got 644/
  );
});
