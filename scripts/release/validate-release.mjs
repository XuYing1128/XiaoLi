import { createHash } from "node:crypto";
import {
  createReadStream,
  existsSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { basename, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const CRITICAL_CARGO_PACKAGES = [
  "xiaoli",
  "tauri",
  "reqwest",
  "rusqlite",
  "tiktoken-rs",
  "keyring",
  "serde",
  "serde_json",
];
const CRITICAL_PNPM_PACKAGES = [
  "@tauri-apps/api",
  "@tauri-apps/plugin-opener",
  "@tauri-apps/cli",
  "spdx-license-list",
  "typescript",
  "vite",
];

function expectedAssets(version) {
  return [
    `XiaoLi-v${version}-Windows-x64-portable.zip`,
    `XiaoLi-v${version}-macOS-universal.app.zip`,
    `XiaoLi-v${version}-Linux-x64-portable.tar.gz`,
    `XiaoLi-v${version}-Linux-x64-portable.zip`,
    `xiaoli-codex-plugin-v${version}.zip`,
    "THIRD_PARTY_LICENSES.html",
    `XiaoLi-v${version}.spdx.json`,
    "SHA256SUMS.txt",
  ].sort();
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function parseCargoLock(text) {
  const packages = new Map();
  for (const block of text.split(/\[\[package\]\]/).slice(1)) {
    const name = block.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
    const version = block.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
    if (name && version && !packages.has(name)) packages.set(name, version);
  }
  return packages;
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function parsePnpmCriticalVersions(text) {
  const versions = new Map();
  for (const name of CRITICAL_PNPM_PACKAGES) {
    const match = text.match(
      new RegExp(`^  ['\"]?${escapeRegExp(name)}@([^'\":\\s]+)['\"]?:`, "m"),
    );
    if (match) versions.set(name, match[1]);
  }
  return versions;
}

function packageMatches(packages, name, version) {
  return packages.some(
    (candidate) =>
      String(candidate.name ?? "").toLowerCase() === name.toLowerCase() &&
      String(candidate.versionInfo ?? "") === version,
  );
}

function collectSpdxIds(sbom) {
  const ids = new Set(["SPDXRef-DOCUMENT"]);
  for (const collection of [sbom.packages, sbom.files, sbom.snippets]) {
    for (const item of Array.isArray(collection) ? collection : []) {
      assert(
        typeof item.SPDXID === "string" && item.SPDXID.startsWith("SPDXRef-"),
        "SPDX element is missing a valid SPDXID",
      );
      assert(!ids.has(item.SPDXID), `Duplicate SPDX element ID: ${item.SPDXID}`);
      ids.add(item.SPDXID);
    }
  }
  return ids;
}

function isKnownSpdxReference(sbom, ids, value) {
  if (ids.has(value) || value === "NONE" || value === "NOASSERTION") return true;
  if (typeof value !== "string") return false;
  return (Array.isArray(sbom.externalDocumentRefs) ? sbom.externalDocumentRefs : []).some(
    (reference) =>
      typeof reference.externalDocumentId === "string" &&
      value.startsWith(`${reference.externalDocumentId}:SPDXRef-`),
  );
}

export function validateSbom(sbom, version, sourceRoot = resolve(".")) {
  assert(String(sbom.spdxVersion ?? "").startsWith("SPDX-"), "Invalid SPDX version");
  assert(sbom.SPDXID === "SPDXRef-DOCUMENT", "Invalid SPDX document ID");
  assert(sbom.dataLicense === "CC0-1.0", "Invalid SPDX data license");
  assert(
    sbom.name === `XiaoLi-v${version}-source-dependency-sbom`,
    "SPDX source dependency SBOM is mislabeled",
  );
  assert(
    String(sbom.documentNamespace ?? "").startsWith("http"),
    "SPDX document namespace is missing",
  );
  assert(
    Array.isArray(sbom.creationInfo?.creators) && sbom.creationInfo.creators.length > 0,
    "SPDX creationInfo.creators is missing",
  );
  assert(
    String(sbom.creationInfo?.comment ?? "").includes(
      "not a binary provenance attestation",
    ),
    "SPDX source-inventory limitation is missing",
  );
  assert(Array.isArray(sbom.packages) && sbom.packages.length > 0, "SPDX packages are missing");
  assert(
    Array.isArray(sbom.relationships) && sbom.relationships.length > 0,
    "SPDX relationships are missing",
  );

  const ids = collectSpdxIds(sbom);
  for (const relationship of sbom.relationships) {
    assert(
      typeof relationship.relationshipType === "string" &&
        relationship.relationshipType.length > 0,
      "SPDX relationship type is missing",
    );
    for (const endpoint of [
      relationship.spdxElementId,
      relationship.relatedSpdxElement,
    ]) {
      assert(
        isKnownSpdxReference(sbom, ids, endpoint),
        `SPDX relationship references an unknown element: ${String(endpoint)}`,
      );
    }
  }

  const appPackage = sbom.packages.find(
    (candidate) =>
      String(candidate.name ?? "").toLowerCase() === "xiaoli" &&
      String(candidate.versionInfo ?? "") === version,
  );
  assert(appPackage, `SPDX is missing the XiaoLi ${version} root package`);
  const describedRoots = new Set(
    sbom.relationships
      .filter(
        (relationship) =>
          relationship.spdxElementId === "SPDXRef-DOCUMENT" &&
          relationship.relationshipType === "DESCRIBES",
      )
      .map((relationship) => relationship.relatedSpdxElement),
  );
  assert(describedRoots.size > 0, "SPDX document has no DESCRIBES relationship");
  assert(
    describedRoots.has(appPackage.SPDXID) ||
      sbom.relationships.some(
        (relationship) =>
          describedRoots.has(relationship.spdxElementId) &&
          relationship.relationshipType === "CONTAINS" &&
          relationship.relatedSpdxElement === appPackage.SPDXID,
      ),
    "SPDX DESCRIBES root does not contain the XiaoLi root package",
  );

  const cargoLockPath = resolve(sourceRoot, "src-tauri", "Cargo.lock");
  const pnpmLockPath = resolve(sourceRoot, "pnpm-lock.yaml");
  assert(existsSync(cargoLockPath), `Cargo.lock is missing: ${cargoLockPath}`);
  assert(existsSync(pnpmLockPath), `pnpm-lock.yaml is missing: ${pnpmLockPath}`);
  const cargoPackages = parseCargoLock(readFileSync(cargoLockPath, "utf8"));
  for (const name of CRITICAL_CARGO_PACKAGES) {
    const lockedVersion = cargoPackages.get(name);
    assert(lockedVersion, `Critical Cargo.lock dependency is missing: ${name}`);
    assert(
      packageMatches(sbom.packages, name, lockedVersion),
      `SPDX does not cover Cargo.lock dependency ${name}@${lockedVersion}`,
    );
  }
  const pnpmPackages = parsePnpmCriticalVersions(readFileSync(pnpmLockPath, "utf8"));
  for (const name of CRITICAL_PNPM_PACKAGES) {
    const lockedVersion = pnpmPackages.get(name);
    assert(lockedVersion, `Critical pnpm lock dependency is missing: ${name}`);
    assert(
      packageMatches(sbom.packages, name, lockedVersion),
      `SPDX does not cover pnpm lock dependency ${name}@${lockedVersion}`,
    );
  }
}

async function hash(path) {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest("hex");
}

export async function validateRelease(
  directory,
  version,
  { sourceRoot = resolve(".") } = {},
) {
  if (!version) throw new Error("Version argument is required");
  const absoluteDirectory = resolve(directory);
  const expected = expectedAssets(version);
  for (const name of expected) {
    const path = resolve(absoluteDirectory, name);
    if (!existsSync(path) || !statSync(path).isFile() || statSync(path).size === 0) {
      throw new Error(`Missing or empty release asset: ${name}`);
    }
  }

  const sbom = JSON.parse(
    readFileSync(resolve(absoluteDirectory, `XiaoLi-v${version}.spdx.json`), "utf8"),
  );
  validateSbom(sbom, version, sourceRoot);

  const notices = readFileSync(
    resolve(absoluteDirectory, "THIRD_PARTY_LICENSES.html"),
    "utf8",
  );
  if (
    !notices.includes("XiaoLi third-party license catalog") ||
    !notices.includes('data-complete-license-catalog="true"') ||
    !notices.includes("Full license and NOTICE texts") ||
    !notices.includes('class="legal-document"')
  ) {
    throw new Error("Third-party license catalog is incomplete or invalid");
  }

  const actual = readdirSync(absoluteDirectory)
    .filter((name) => statSync(resolve(absoluteDirectory, name)).isFile())
    .sort();
  const unexpected = actual.filter((name) => !expected.includes(name));
  if (unexpected.length) {
    throw new Error(`Unexpected release assets: ${unexpected.join(", ")}`);
  }

  const checksumLines = readFileSync(
    resolve(absoluteDirectory, "SHA256SUMS.txt"),
    "utf8",
  )
    .trim()
    .split(/\r?\n/);
  const checksums = new Map(
    checksumLines.map((line) => {
      const match = line.match(/^([a-f0-9]{64})  (.+)$/);
      if (!match) throw new Error(`Invalid checksum line: ${line}`);
      return [match[2], match[1]];
    }),
  );
  if (checksumLines.length !== checksums.size) {
    throw new Error("Checksum manifest contains duplicate asset names");
  }
  for (const name of expected.filter((name) => name !== "SHA256SUMS.txt")) {
    const observed = await hash(resolve(absoluteDirectory, name));
    if (checksums.get(name) !== observed) throw new Error(`Checksum mismatch: ${name}`);
  }
  if (checksums.size !== expected.length - 1) {
    throw new Error("Checksum manifest entry count mismatch");
  }

  const completeHashes = new Map();
  for (const name of expected) {
    completeHashes.set(name, await hash(resolve(absoluteDirectory, name)));
  }
  return { directory: absoluteDirectory, expected, hashes: completeHashes };
}

export async function compareReleaseDirectories(
  localDirectory,
  remoteDirectory,
  version,
  options = {},
) {
  const local = await validateRelease(localDirectory, version, options);
  const remote = await validateRelease(remoteDirectory, version, options);
  const differences = local.expected.filter(
    (name) => local.hashes.get(name) !== remote.hashes.get(name),
  );
  if (differences.length) {
    throw new Error(
      `Existing release differs from this build by SHA256: ${differences.join(", ")}`,
    );
  }
  return local.expected;
}

const isMain =
  process.argv[1] &&
  resolve(fileURLToPath(import.meta.url)).toLowerCase() ===
    resolve(process.argv[1]).toLowerCase();
if (isMain) {
  const directory = resolve(process.argv[2] ?? "release-out");
  const version = process.argv[3];
  const comparisonDirectory = process.argv[4];
  if (comparisonDirectory) {
    const assets = await compareReleaseDirectories(
      directory,
      resolve(comparisonDirectory),
      version,
    );
    console.log(
      `Existing release is byte-identical by SHA256 (${assets.length} assets): ${basename(
        directory,
      )}`,
    );
  } else {
    const result = await validateRelease(directory, version);
    console.log(
      `Complete portable release validated (${result.expected.length} assets): ${result.directory}`,
    );
  }
}
