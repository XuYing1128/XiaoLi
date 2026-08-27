import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  compareReleaseDirectories,
  validateRelease,
  validateSbom,
} from "../validate-release.mjs";
import { findPrivateBuildPathLeaks } from "../assert-no-private-build-paths.mjs";
import { scanPublicTree } from "../scan-public-tree.mjs";

const VERSION = "0.2.0-beta.1";
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const cargoPackages = new Map([
  ["xiaoli", VERSION],
  ["tauri", "2.11.5"],
  ["reqwest", "0.13.4"],
  ["rusqlite", "0.32.1"],
  ["tiktoken-rs", "0.12.0"],
  ["keyring", "4.1.0"],
  ["serde", "1.0.228"],
  ["serde_json", "1.0.149"],
  ["base64", "0.22.1"],
  ["ed25519-dalek", "2.2.0"],
]);
const pnpmPackages = new Map([
  ["@tauri-apps/api", "2.11.1"],
  ["@tauri-apps/plugin-opener", "2.5.4"],
  ["@tauri-apps/cli", "2.11.4"],
  ["spdx-license-list", "6.12.0"],
  ["typescript", "5.6.3"],
  ["vite", "6.4.3"],
]);

function temporaryRoot(t, prefix) {
  const root = mkdtempSync(join(tmpdir(), prefix));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

function writeSourceFixture(root) {
  mkdirSync(join(root, "src-tauri"), { recursive: true });
  writeFileSync(
    join(root, "src-tauri", "Cargo.lock"),
    [...cargoPackages]
      .map(
        ([name, version]) =>
          `[[package]]\nname = ${JSON.stringify(name)}\nversion = ${JSON.stringify(version)}\n`,
      )
      .join("\n"),
  );
  writeFileSync(
    join(root, "pnpm-lock.yaml"),
    `lockfileVersion: '9.0'\npackages:\n${[...pnpmPackages]
      .map(([name, version]) => `  '${name}@${version}':\n    resolution: {}`)
      .join("\n")}\n`,
  );
}

function spdxId(name) {
  return `SPDXRef-Package-${name.replace(/[^A-Za-z0-9.-]/g, "-")}`;
}

function makeSbom() {
  const dependencyPackages = [...new Map([...cargoPackages, ...pnpmPackages])].map(
    ([name, version]) => ({
      name,
      versionInfo: version,
      SPDXID: spdxId(name),
    }),
  );
  return {
    spdxVersion: "SPDX-2.3",
    SPDXID: "SPDXRef-DOCUMENT",
    dataLicense: "CC0-1.0",
    name: `XiaoLi-v${VERSION}-source-dependency-sbom`,
    documentNamespace: "https://example.invalid/spdx/xiaoli-fixture",
    creationInfo: {
      creators: ["Tool: fixture"],
      comment:
        "Dependency inventory generated from source; it is not a binary provenance attestation.",
    },
    packages: [
      { name: "fixture-root", SPDXID: "SPDXRef-DocumentRoot" },
      ...dependencyPackages,
    ],
    files: [{ fileName: "Cargo.lock", SPDXID: "SPDXRef-File-CargoLock" }],
    relationships: [
      {
        spdxElementId: "SPDXRef-DOCUMENT",
        relationshipType: "DESCRIBES",
        relatedSpdxElement: "SPDXRef-DocumentRoot",
      },
      ...dependencyPackages.map((dependency) => ({
        spdxElementId: "SPDXRef-DocumentRoot",
        relationshipType: "CONTAINS",
        relatedSpdxElement: dependency.SPDXID,
      })),
    ],
  };
}

function assetNames() {
  return [
    `XiaoLi-v${VERSION}-Windows-x64-portable.zip`,
    `XiaoLi-v${VERSION}-macOS-universal.app.zip`,
    `XiaoLi-v${VERSION}-Linux-x64-portable.tar.gz`,
    `XiaoLi-v${VERSION}-Linux-x64-portable.zip`,
    `xiaoli-codex-plugin-v${VERSION}.zip`,
    "THIRD_PARTY_LICENSES.html",
    `XiaoLi-v${VERSION}.spdx.json`,
  ];
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function writeChecksums(directory) {
  const lines = assetNames()
    .sort()
    .map((name) => `${sha256(join(directory, name))}  ${name}`);
  writeFileSync(join(directory, "SHA256SUMS.txt"), `${lines.join("\n")}\n`);
}

function writeReleaseFixture(directory) {
  mkdirSync(directory, { recursive: true });
  for (const name of assetNames()) {
    if (name.endsWith(".spdx.json")) {
      writeFileSync(join(directory, name), `${JSON.stringify(makeSbom())}\n`);
    } else if (name === "THIRD_PARTY_LICENSES.html") {
      writeFileSync(
        join(directory, name),
        '<html data-complete-license-catalog="true"><h1>XiaoLi third-party license catalog</h1><p>Full license and NOTICE texts</p><section class="legal-document">fixture</section></html>',
      );
    } else {
      writeFileSync(join(directory, name), `fixture:${name}\n`);
    }
  }
  writeChecksums(directory);
}

test("public-tree scanner accepts explicit placeholders and synthetic IDs", (t) => {
  const root = temporaryRoot(t, "xiaoli-scan-pass-");
  const body = [
    ["C:", "Users", "<username>", "XiaoLi"].join("/"),
    ["D:", "Users", "example", "XiaoLi"].join("\\"),
    "/Users/<username>/Library/Application Support/XiaoLi/",
    "/home/runner/work/XiaoLi/",
    "threadId: 00000000-0000-4000-8000-000000000101",
    "turn_id: 00000000-0000-0000-0000-000000000002",
    "SID: S-1-5-21-<domain>-<domain>-<domain>-<rid>",
  ].join("\n");
  writeFileSync(join(root, "safe.txt"), body);
  assert.deepEqual(scanPublicTree(root, ["safe.txt"]), []);
});

test("public-tree scanner detects slash and backslash Windows profiles", (t) => {
  const root = temporaryRoot(t, "xiaoli-scan-windows-");
  writeFileSync(
    join(root, "private.txt"),
    [
      ["C:", "Users", "real-account", "AppData"].join("/"),
      ["D:", "Users", "another-account", "Desktop"].join("\\"),
    ].join("\n"),
  );
  const findings = scanPublicTree(root, ["private.txt"]);
  assert.equal(
    findings.filter((finding) => finding.includes("Windows user profile path")).length,
    1,
  );
});

test("public-tree scanner detects real IDs but permits fixture UUIDs", (t) => {
  const root = temporaryRoot(t, "xiaoli-scan-identifiers-");
  const realUuid = ["01a02ca4", "b52d", "7970", "becb", "04505af4159f"].join("-");
  const realSid = ["S-1-5-21", "1234567890", "2345678901", "3456789012", "1001"].join(
    "-",
  );
  writeFileSync(
    join(root, "identifiers.txt"),
    `threadId: ${realUuid}\naccount=${realSid}\n`,
  );
  const findings = scanPublicTree(root, ["identifiers.txt"]);
  assert(findings.some((finding) => finding.includes("real thread/turn/session UUID")));
  assert(findings.some((finding) => finding.includes("Windows account SID")));
});

test("public-tree scanner reads BOM-marked UTF-16 text", (t) => {
  const root = temporaryRoot(t, "xiaoli-scan-utf16-");
  const privatePath = ["C:", "Users", "utf16-account", "state"].join("/");
  const encoded = Buffer.from(privatePath, "utf16le");
  writeFileSync(
    join(root, "utf16.txt"),
    Buffer.concat([Buffer.from([0xff, 0xfe]), encoded]),
  );
  assert(
    scanPublicTree(root, ["utf16.txt"]).some((finding) =>
      finding.includes("Windows user profile path"),
    ),
  );
});

test("public-tree scanner recognizes BOM-less Windows UTF-16 text", (t) => {
  const root = temporaryRoot(t, "xiaoli-scan-utf16-no-bom-");
  const privatePath = ["D:", "Users", "utf16-no-bom", "state"].join("\\");
  writeFileSync(join(root, "utf16-no-bom.txt"), Buffer.from(privatePath, "utf16le"));
  assert(
    scanPublicTree(root, ["utf16-no-bom.txt"]).some((finding) =>
      finding.includes("Windows user profile path"),
    ),
  );
});

test("public-tree scanner does not skip findings beyond five MiB", (t) => {
  const root = temporaryRoot(t, "xiaoli-scan-large-");
  const privatePath = ["C:", "Users", "large-file-account", "state"].join("/");
  writeFileSync(
    join(root, "large.txt"),
    Buffer.concat([Buffer.alloc(6 * 1024 * 1024, 0x61), Buffer.from(`\n${privatePath}\n`)]),
  );
  assert(
    scanPublicTree(root, ["large.txt"]).some((finding) =>
      finding.includes("Windows user profile path"),
    ),
  );
});

test("public-tree scanner rejects generated state paths before opening them", (t) => {
  const root = temporaryRoot(t, "xiaoli-scan-path-");
  assert.deepEqual(scanPublicTree(root, ["shadow-state/private.bin"]), [
    "shadow-state/private.bin: forbidden generated/private artifact path",
  ]);
});

test("binary build-path scanner catches UTF-8 and UTF-16LE private prefixes", (t) => {
  const root = temporaryRoot(t, "xiaoli-binary-paths-");
  const privateHome = join(root, "private-home");
  const utf8 = join(root, "utf8.bin");
  const utf16 = join(root, "utf16.bin");
  const clean = join(root, "clean.bin");
  writeFileSync(utf8, Buffer.concat([Buffer.alloc(1024 * 1024 - 3, 0x61), Buffer.from(privateHome)]));
  writeFileSync(utf16, Buffer.from(privateHome, "utf16le"));
  writeFileSync(clean, "<home>/cargo/<workspace>/source.rs");
  const options = { env: { HOME: privateHome }, cwd: join(root, "workspace") };
  assert.equal(findPrivateBuildPathLeaks([utf8], options).length, 1);
  assert.equal(findPrivateBuildPathLeaks([utf16], options).length, 1);
  assert.deepEqual(findPrivateBuildPathLeaks([clean], options), []);
});

test("binary build-path scanner inspects private symlink targets without following links", (t) => {
  const root = temporaryRoot(t, "xiaoli-binary-symlink-");
  const outside = temporaryRoot(t, "xiaoli-binary-symlink-outside-");
  const privateHome = join(outside, "private-home");
  const relativeTarget = join(outside, "relative-target");
  mkdirSync(privateHome, { recursive: true });
  mkdirSync(relativeTarget, { recursive: true });
  writeFileSync(join(relativeTarget, "must-not-be-read.bin"), privateHome);

  symlinkSync(
    privateHome,
    join(root, "absolute-private-link"),
    process.platform === "win32" ? "junction" : "dir",
  );
  symlinkSync(
    relativeTarget,
    join(root, "relative-external-link"),
    process.platform === "win32" ? "junction" : "dir",
  );

  const findings = findPrivateBuildPathLeaks([root], {
    env: { HOME: privateHome, USERPROFILE: privateHome },
    cwd: join(root, "workspace"),
  });
  assert.equal(findings.length, 1);
  assert.match(findings[0], /absolute-private-link.*home.*symbolic link/u);

  if (process.platform !== "win32") {
    const relativePrivateLink = join(root, "relative-private-link");
    symlinkSync(
      relative(root, privateHome),
      relativePrivateLink,
      "dir",
    );
    const relativeFindings = findPrivateBuildPathLeaks([relativePrivateLink], {
      env: { HOME: privateHome },
      cwd: join(root, "workspace"),
    });
    assert.equal(relativeFindings.length, 1);
    assert.match(relativeFindings[0], /relative-private-link.*home.*symbolic link/u);
  }
});

test("path remap helper covers Rust and native C or C++ compilers while preserving flags", (t) => {
  const root = temporaryRoot(t, "xiaoli-remap-env-");
  const githubEnv = join(root, "github-env");
  const workspace = join(root, "workspace");
  const privateHome = join(root, "private-home");
  const cargoHome = join(privateHome, ".cargo");
  const rustupHome = join(privateHome, ".rustup");
  const targetDirectory = join(root, "cargo-target");
  for (const path of [workspace, privateHome, cargoHome, rustupHome, targetDirectory]) {
    mkdirSync(path, { recursive: true });
  }
  const run = spawnSync(
    process.execPath,
    [join(repositoryRoot, "scripts", "release", "prepare-rust-path-remap.mjs")],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        GITHUB_ENV: githubEnv,
        GITHUB_WORKSPACE: workspace,
        HOME: privateHome,
        USERPROFILE: privateHome,
        CARGO_HOME: cargoHome,
        RUSTUP_HOME: rustupHome,
        CARGO_TARGET_DIR: targetDirectory,
        RUSTFLAGS: "-Cdebuginfo=1",
        CFLAGS: "-DEXISTING_C_FLAG=1",
        CXXFLAGS: "-DEXISTING_CXX_FLAG=1",
      },
    },
  );
  assert.equal(run.status, 0, run.stderr || run.stdout);
  const output = readFileSync(githubEnv, "utf8");
  assert.match(output, /^RUSTFLAGS=-Cdebuginfo=1 .*--remap-path-prefix=/mu);
  assert.match(output, new RegExp(`--remap-path-prefix=${targetDirectory.replaceAll("\\", "\\\\")}=<target>`, "u"));
  assert.match(output, /^CFLAGS=-DEXISTING_C_FLAG=1 /mu);
  assert.match(output, /^CXXFLAGS=-DEXISTING_CXX_FLAG=1 /mu);
  if (process.platform === "win32") {
    assert.match(output, /^CFLAGS=.*\/experimental:deterministic .*\/pathmap:/mu);
    assert.match(output, /^CXXFLAGS=.*\/experimental:deterministic .*\/pathmap:/mu);
  } else {
    assert.match(output, /^CFLAGS=.*-ffile-prefix-map=.*-fdebug-prefix-map=/mu);
    assert.match(output, /^CXXFLAGS=.*-ffile-prefix-map=.*-fdebug-prefix-map=/mu);
  }
});

test(
  "Linux packager extracts and scans compressed AppImage payloads",
  { skip: process.platform !== "linux" },
  (t) => {
    const root = temporaryRoot(t, "xiaoli-appimage-payload-");
    const appimageDirectory = join(root, "target", "release", "bundle", "appimage");
    const nativeBinary = join(root, "target", "release", "xiaoli");
    const appimage = join(appimageDirectory, "XiaoLi-fixture.AppImage");
    const output = join(root, "release-out");
    const privateHome = join(root, "private-home");
    mkdirSync(appimageDirectory, { recursive: true });
    mkdirSync(privateHome, { recursive: true });
    writeFileSync(nativeBinary, "#!/usr/bin/env sh\nexit 0\n");
    chmodSync(nativeBinary, 0o755);

    const writeFixtureAppImage = (payload) => {
      const compressedPayload = gzipSync(Buffer.from(payload)).toString("base64");
      writeFileSync(
        appimage,
        `#!/usr/bin/env bash
set -euo pipefail
[[ "\${1:-}" == "--appimage-extract" ]]
mkdir -p squashfs-root/usr/bin
printf '%s' '${compressedPayload}' | base64 --decode | gzip --decompress > squashfs-root/usr/bin/private-path.bin
`,
      );
      chmodSync(appimage, 0o755);
    };
    writeFixtureAppImage(privateHome);

    const run = spawnSync(
      "bash",
      [
        join(repositoryRoot, "scripts", "release", "package-linux.sh"),
        appimageDirectory,
        output,
        VERSION,
      ],
      {
        cwd: repositoryRoot,
        encoding: "utf8",
        env: { ...process.env, HOME: privateHome },
        maxBuffer: 8 * 1024 * 1024,
      },
    );
    assert.notEqual(run.status, 0, "compressed private path payload reached a release archive");
    assert.match(`${run.stdout}\n${run.stderr}`, /unremapped home prefix/u);

  },
);

test(
  "macOS bundle verifier accepts both deployment encodings and rejects semantic mismatches",
  { skip: process.platform === "win32" },
  (t) => {
    const root = temporaryRoot(t, "xiaoli-macos-verifier-");
    const app = join(root, "XiaoLi.app");
    const binary = join(app, "Contents", "MacOS", "xiaoli");
    const plist = join(app, "Contents", "Info.plist");
    const toolDirectory = join(root, "tools");
    mkdirSync(dirname(binary), { recursive: true });
    mkdirSync(toolDirectory, { recursive: true });
    writeFileSync(binary, "fixture executable\n");
    chmodSync(binary, 0o755);
    writeFileSync(plist, "fixture plist\n");

    const writeTool = (name, source) => {
      const path = join(toolDirectory, name);
      writeFileSync(path, source);
      chmodSync(path, 0o755);
    };
    writeTool(
      "lipo",
      `#!/usr/bin/env bash
set -euo pipefail
if [[ "\${2:-}" == "-verify_arch" ]]; then
  exit "\${XIAOLI_TEST_LIPO_VERIFY_EXIT:-0}"
fi
if [[ "\${1:-}" == "-archs" ]]; then
  printf '%s\\n' "\${XIAOLI_TEST_ARCHS:-arm64 x86_64}"
  exit 0
fi
if [[ "\${2:-}" == "-thin" && "\${4:-}" == "-output" ]]; then
  cp "\${1}" "\${5}"
  chmod +x "\${5}"
  exit 0
fi
exit 2
`,
    );
    writeTool(
      "file",
      `#!/usr/bin/env bash
if [[ -n "\${XIAOLI_TEST_FILE_DESCRIPTION:-}" ]]; then
  printf '%s\\n' "\${XIAOLI_TEST_FILE_DESCRIPTION}"
  exit 0
fi
case "\${1}" in
  *arm64) printf '%s: Mach-O 64-bit executable arm64\\n' "\${1}" ;;
  *x86_64) printf '%s: Mach-O 64-bit executable x86_64\\n' "\${1}" ;;
  *) exit 2 ;;
esac
`,
    );
    writeTool(
      "xcrun",
      `#!/usr/bin/env bash
set -euo pipefail
[[ "\${1:-}" == "vtool" && "\${2:-}" == "-arch" && "\${4:-}" == "-show-build" ]]
case "\${3}" in
  arm64) printf '%s\\n' "\${XIAOLI_TEST_VTOOL_ARM64}" ;;
  x86_64) printf '%s\\n' "\${XIAOLI_TEST_VTOOL_X86_64}" ;;
  *) exit 2 ;;
esac
`,
    );
    writeTool(
      "plutil",
      `#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "\${XIAOLI_TEST_PLIST_MINOS}"
`,
    );
    writeTool(
      "codesign",
      `#!/usr/bin/env bash
set -euo pipefail
exit "\${XIAOLI_TEST_CODESIGN_EXIT:-0}"
`,
    );

    const modern = `Load command 1
      cmd LC_BUILD_VERSION
 platform MACOS
    minos 12.0`;
    const legacy = `Load command 1
      cmd LC_VERSION_MIN_MACOSX
  version 12.0`;
    const runVerifier = (overrides = {}) =>
      spawnSync(
        "bash",
        [join(repositoryRoot, "scripts", "release", "verify-macos-bundle.sh"), app, "12.0"],
        {
          cwd: repositoryRoot,
          encoding: "utf8",
          env: {
            ...process.env,
            PATH: `${toolDirectory}:${process.env.PATH}`,
            XIAOLI_TEST_ARCHS: "arm64 x86_64",
            XIAOLI_TEST_VTOOL_ARM64: modern,
            XIAOLI_TEST_VTOOL_X86_64: modern,
            XIAOLI_TEST_PLIST_MINOS: "12.0",
            ...overrides,
          },
        },
      );

    let run = runVerifier();
    assert.equal(run.status, 0, run.stderr || run.stdout);
    run = runVerifier({ XIAOLI_TEST_VTOOL_X86_64: legacy });
    assert.equal(run.status, 0, run.stderr || run.stdout);
    run = runVerifier({ XIAOLI_TEST_LIPO_VERIFY_EXIT: "1" });
    assert.notEqual(run.status, 0, "lipo verification failure was ignored");
    run = runVerifier({ XIAOLI_TEST_CODESIGN_EXIT: "1" });
    assert.notEqual(run.status, 0, "codesign verification failure was ignored");

    const failures = [
      [{ XIAOLI_TEST_ARCHS: "arm64" }, /expected exactly arm64 and x86_64 slices/u],
      [{ XIAOLI_TEST_ARCHS: "arm64 ppc64" }, /missing x86_64/u],
      [
        {
          XIAOLI_TEST_VTOOL_ARM64: modern.replace("platform MACOS", "platform IOS"),
        },
        /does not target macOS/u,
      ],
      [{ XIAOLI_TEST_VTOOL_ARM64: "cmd LC_UUID" }, /no recognized macOS deployment target/u],
      [
        { XIAOLI_TEST_FILE_DESCRIPTION: "fixture: data" },
        /slice is not Mach-O 64-bit/u,
      ],
      [
        { XIAOLI_TEST_VTOOL_X86_64: modern.replace("minos 12.0", "minos 13.0") },
        /minimum macOS version is 13\.0/u,
      ],
      [
        { XIAOLI_TEST_VTOOL_X86_64: modern.replace("minos 12.0", "minos malformed") },
        /minimum macOS version is missing or malformed/u,
      ],
      [{ XIAOLI_TEST_PLIST_MINOS: "13.0" }, /LSMinimumSystemVersion is 13\.0/u],
      [{ XIAOLI_TEST_PLIST_MINOS: "not-a-version" }, /missing or malformed/u],
    ];
    for (const [environment, pattern] of failures) {
      run = runVerifier(environment);
      assert.notEqual(run.status, 0, `unexpected pass for ${JSON.stringify(environment)}`);
      assert.match(`${run.stdout}\n${run.stderr}`, pattern);
    }
  },
);

test("release validator checks SPDX root, graph references, and lock coverage", (t) => {
  const sourceRoot = temporaryRoot(t, "xiaoli-sbom-source-");
  writeSourceFixture(sourceRoot);
  assert.doesNotThrow(() => validateSbom(makeSbom(), VERSION, sourceRoot));

  const brokenReference = makeSbom();
  brokenReference.relationships.push({
    spdxElementId: "SPDXRef-DocumentRoot",
    relationshipType: "CONTAINS",
    relatedSpdxElement: "SPDXRef-Missing",
  });
  assert.throws(
    () => validateSbom(brokenReference, VERSION, sourceRoot),
    /unknown element/,
  );

  const missingLockedPackage = makeSbom();
  missingLockedPackage.packages = missingLockedPackage.packages.filter(
    (candidate) => candidate.name !== "reqwest",
  );
  missingLockedPackage.relationships = missingLockedPackage.relationships.filter(
    (relationship) => relationship.relatedSpdxElement !== spdxId("reqwest"),
  );
  assert.throws(
    () => validateSbom(missingLockedPackage, VERSION, sourceRoot),
    /Cargo\.lock dependency reqwest/,
  );
});

test("published-release comparison is idempotent only for identical SHA256", async (t) => {
  const sourceRoot = temporaryRoot(t, "xiaoli-release-source-");
  const local = temporaryRoot(t, "xiaoli-release-local-");
  const remote = temporaryRoot(t, "xiaoli-release-remote-");
  writeSourceFixture(sourceRoot);
  writeReleaseFixture(local);
  writeReleaseFixture(remote);

  await validateRelease(local, VERSION, { sourceRoot });
  await compareReleaseDirectories(local, remote, VERSION, { sourceRoot });

  const changed = `XiaoLi-v${VERSION}-Windows-x64-portable.zip`;
  writeFileSync(join(remote, changed), "different but internally consistent\n");
  writeChecksums(remote);
  await assert.rejects(
    compareReleaseDirectories(local, remote, VERSION, { sourceRoot }),
    /differs from this build by SHA256/,
  );
});

test("release workflow gates published assets and builds Universal on macOS 15", () => {
  const ciWorkflow = readFileSync(
    join(repositoryRoot, ".github", "workflows", "ci.yml"),
    "utf8",
  );
  const workflow = readFileSync(
    join(repositoryRoot, ".github", "workflows", "release.yml"),
    "utf8",
  );
  const ciRemap = ciWorkflow.indexOf("prepare-rust-path-remap.mjs");
  const ciBuild = ciWorkflow.indexOf("tauri-apps/tauri-action", ciRemap);
  const ciBinaryScan = ciWorkflow.indexOf("assert-no-private-build-paths.mjs", ciBuild);
  assert(ciRemap >= 0, "CI does not configure native path remapping");
  assert(ciBuild > ciRemap, "CI builds before configuring path remapping");
  assert(ciBinaryScan > ciBuild, "CI does not scan the resulting native executable");
  assert.match(workflow, /runs-on: macos-15/);
  assert.match(workflow, /MACOSX_DEPLOYMENT_TARGET: "12\.0"/);
  const tauriConfig = JSON.parse(
    readFileSync(join(repositoryRoot, "src-tauri", "tauri.conf.json"), "utf8"),
  );
  assert.equal(
    tauriConfig.bundle?.macOS?.minimumSystemVersion,
    "12.0",
    "Tauri must not replace the claimed macOS 12 deployment target with its 10.13 default",
  );
  assert.match(workflow, /bash \.\/scripts\/release\/verify-macos-bundle\.sh/);
  const macBundleVerifier = readFileSync(
    join(repositoryRoot, "scripts", "release", "verify-macos-bundle.sh"),
    "utf8",
  );
  assert.match(macBundleVerifier, /lipo "\$\{binary\}" -verify_arch arm64 x86_64/);
  assert.match(macBundleVerifier, /archs="\$\(lipo -archs "\$\{binary\}"\)"/);
  assert.match(
    macBundleVerifier,
    /xcrun vtool -arch "\$\{arch\}" -show-build "\$\{binary\}"/,
    "the release gate must inspect each architecture's semantic build target",
  );
  assert.doesNotMatch(
    macBundleVerifier,
    /xcrun vtool -show-build -arch/,
    "vtool options must use the documented order so CI does not rely on permissive parsing",
  );
  assert.match(macBundleVerifier, /LC_BUILD_VERSION/);
  assert.match(
    macBundleVerifier,
    /LC_VERSION_MIN_MACOSX/,
    "both valid Mach-O deployment-target encodings must be accepted",
  );
  assert.match(
    macBundleVerifier,
    /plutil -extract LSMinimumSystemVersion raw/,
    "the bundle metadata must agree with both Mach-O slices",
  );
  assert.doesNotMatch(
    macBundleVerifier,
    /otool -l "\$\{thin\}" \| grep -q "LC_BUILD_VERSION"/,
    "grep -q can SIGPIPE otool under bash pipefail and turn a successful match into a failed step",
  );
  const macArchiveSmoke = readFileSync(
    join(repositoryRoot, "scripts", "release", "test-macos-archive.sh"),
    "utf8",
  );
  assert.match(
    macArchiveSmoke,
    /verify-macos-bundle\.sh" "\$\{app\}" 12\.0/,
    "the final extracted Release archive must receive the same semantic bundle verification",
  );
  assert.match(workflow, /syft-version: v1\.51\.0/);
  assert.equal(
    (workflow.match(/prepare-rust-path-remap\.mjs/g) ?? []).length,
    3,
    "every native release build must remap private paths",
  );
  assert.match(workflow, /assert-no-private-build-paths\.mjs/);
  assert.match(workflow, /Extract, scan and package AppImage archives/);

  const portableSmoke = readFileSync(
    join(repositoryRoot, "scripts", "release", "Test-Portable.ps1"),
    "utf8",
  );
  assert.match(portableSmoke, /\$hookFallbackAttempts = 4/);
  assert.match(
    portableSmoke,
    /\$attempt -le \$hookFallbackAttempts/,
    "offline hook fallback must use a bounded number of independent attempts",
  );
  assert.match(
    portableSmoke,
    /No hook capture attempt wrote to isolated XIAOLI_STATE_DIR/,
    "portable smoke must still fail when every fallback attempt is lost",
  );

  const linuxPackager = readFileSync(
    join(repositoryRoot, "scripts", "release", "package-linux.sh"),
    "utf8",
  );
  const extractPayload = linuxPackager.indexOf("--appimage-extract");
  const scanPayload = linuxPackager.indexOf('"${payload_root}"', extractPayload);
  const createLinuxArchive = linuxPackager.indexOf('tar -C "${stage}"', scanPayload);
  assert(extractPayload >= 0, "Linux packager does not extract the AppImage payload");
  assert(scanPayload > extractPayload, "Linux packager does not scan the extracted payload");
  assert(createLinuxArchive > scanPayload, "Linux archive is created before payload scanning");

  const macPackager = readFileSync(
    join(repositoryRoot, "scripts", "release", "package-macos.sh"),
    "utf8",
  );
  const generatedMacNotice = macPackager.indexOf('"${stage}/THIRD_PARTY_LICENSES.html"');
  const scanMacStage = macPackager.indexOf('"${stage}"', generatedMacNotice);
  const createMacArchive = macPackager.indexOf("ditto -c -k", scanMacStage);
  assert(scanMacStage > generatedMacNotice, "macOS staged payload is not scanned");
  assert(createMacArchive > scanMacStage, "macOS archive is created before staged scanning");

  const publishedGuard = workflow.indexOf('if [[ "${is_draft}" != "true" ]]');
  const compareRemote = workflow.indexOf(
    'release-out "${XIAOLI_VERSION}" "${published_dir}"',
  );
  const successfulNoOp = workflow.indexOf("exit 0", compareRemote);
  const deleteDraftAssets = workflow.indexOf("gh release delete-asset");
  const mutableUpload = workflow.indexOf('gh release upload "${RELEASE_TAG}"');
  assert(publishedGuard >= 0, "published release guard is missing");
  assert(compareRemote > publishedGuard, "published assets are not SHA-compared");
  assert(successfulNoOp > compareRemote, "identical published release is not idempotent");
  assert(deleteDraftAssets > successfulNoOp, "published release can reach draft deletion");
  assert(mutableUpload > deleteDraftAssets, "published release can reach upload mutation");
});
