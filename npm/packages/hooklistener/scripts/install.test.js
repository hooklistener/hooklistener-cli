"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const {
  extractArchive,
  parseChecksumManifest,
  verifyChecksum,
} = require("./install");

const ARCHIVE = "hooklistener-x86_64-unknown-linux-gnu.tar.gz";
const DIGEST = "a".repeat(64);

function withExtractionFixture(callback) {
  const fixtureRoot = fs.mkdtempSync(
    path.join(os.tmpdir(), "hooklistener-install-test-")
  );
  const temporaryRoot = path.join(fixtureRoot, "temporary");
  const destination = path.join(fixtureRoot, "destination");
  fs.mkdirSync(temporaryRoot);

  try {
    callback({ destination, fixtureRoot, temporaryRoot });
  } finally {
    fs.rmSync(fixtureRoot, { recursive: true, force: true });
  }
}

test("parseChecksumManifest returns the exact archive checksum", () => {
  const manifest = [
    `${"b".repeat(64)}  ${ARCHIVE}.sig`,
    `${DIGEST}  ${ARCHIVE}`,
  ].join("\n");

  assert.equal(parseChecksumManifest(Buffer.from(manifest), ARCHIVE), DIGEST);
});

test("parseChecksumManifest rejects a missing archive entry", () => {
  const manifest = `${DIGEST}  another-archive.tar.gz\n`;

  assert.throws(
    () => parseChecksumManifest(Buffer.from(manifest), ARCHIVE),
    /missing an entry/
  );
});

test("parseChecksumManifest rejects malformed digests", () => {
  const manifest = `not-a-sha256  ${ARCHIVE}\n`;

  assert.throws(
    () => parseChecksumManifest(Buffer.from(manifest), ARCHIVE),
    /Malformed checksum entry/
  );
});

test("parseChecksumManifest rejects duplicate archive entries", () => {
  const manifest = `${DIGEST}  ${ARCHIVE}\n${DIGEST}  ${ARCHIVE}\n`;

  assert.throws(
    () => parseChecksumManifest(Buffer.from(manifest), ARCHIVE),
    /duplicate entries/
  );
});

test("verifyChecksum accepts the matching SHA-256 digest", () => {
  const payload = Buffer.from("hooklistener");
  const digest = "f09e19981034811c388de0bf5af209172448fd891cda83c9a3c5dda3a703d0ca";

  assert.doesNotThrow(() => verifyChecksum(payload, digest));
});

test("verifyChecksum rejects a mismatched SHA-256 digest", () => {
  assert.throws(
    () => verifyChecksum(Buffer.from("hooklistener"), "0".repeat(64)),
    /Checksum verification failed/
  );
});

test("extractArchive uses a private workspace and tar argument array", () => {
  withExtractionFixture(({ destination, temporaryRoot }) => {
    let invocation;

    extractArchive(Buffer.from("archive contents"), "tar.gz", destination, {
      platform: "linux",
      temporaryRoot,
      execFileSync(command, args, options) {
        invocation = { args, command, options };
        const archivePath = args[1];
        const workspace = path.dirname(archivePath);

        if (process.platform !== "win32") {
          assert.equal(fs.statSync(workspace).mode & 0o777, 0o700);
          assert.equal(fs.statSync(archivePath).mode & 0o777, 0o600);
        }
        assert.equal(fs.readFileSync(archivePath, "utf8"), "archive contents");
      },
    });

    assert.equal(invocation.command, "tar");
    assert.deepEqual(invocation.args.slice(0, 1), ["-xzf"]);
    assert.deepEqual(invocation.args.slice(2), ["-C", destination]);
    assert.deepEqual(invocation.options, { stdio: "pipe" });
    assert.deepEqual(fs.readdirSync(temporaryRoot), []);
  });
});

test("extractArchive passes Windows paths as positional PowerShell arguments", () => {
  withExtractionFixture(({ fixtureRoot, temporaryRoot }) => {
    const destination = path.join(
      fixtureRoot,
      "destination with spaces; $(not-a-command) 'quoted'"
    );
    let invocation;
    let scriptContents;

    extractArchive(Buffer.from("zip contents"), "zip", destination, {
      platform: "win32",
      temporaryRoot,
      execFileSync(command, args, options) {
        const fileFlagIndex = args.indexOf("-File");
        const scriptPath = args[fileFlagIndex + 1];
        invocation = { args, command, fileFlagIndex, options };
        scriptContents = fs.readFileSync(scriptPath, "utf8");
      },
    });

    const scriptPath = invocation.args[invocation.fileFlagIndex + 1];
    const archivePath = invocation.args[invocation.fileFlagIndex + 2];
    assert.equal(invocation.command, "powershell.exe");
    assert.equal(invocation.args.includes("-Command"), false);
    assert.equal(invocation.args[invocation.fileFlagIndex + 3], destination);
    assert.equal(path.dirname(scriptPath), path.dirname(archivePath));
    assert.match(scriptContents, /Position=0/);
    assert.match(scriptContents, /Position=1/);
    assert.equal(scriptContents.includes(destination), false);
    assert.deepEqual(invocation.options, { stdio: "pipe", windowsHide: true });
    assert.deepEqual(fs.readdirSync(temporaryRoot), []);
  });
});

test("extractArchive removes its private workspace when extraction fails", () => {
  withExtractionFixture(({ destination, temporaryRoot }) => {
    assert.throws(
      () =>
        extractArchive(Buffer.from("archive contents"), "tar.gz", destination, {
          platform: "linux",
          temporaryRoot,
          execFileSync() {
            throw new Error("extractor failed");
          },
        }),
      /extractor failed/
    );
    assert.deepEqual(fs.readdirSync(temporaryRoot), []);
  });
});
