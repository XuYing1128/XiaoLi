import { createHash } from "node:crypto";
import { createReadStream, existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { basename, resolve } from "node:path";

const directory = resolve(process.argv[2] ?? "release-out");
const version = process.argv[3];
if (!version) throw new Error("Version argument is required");
const expected = [
  `XiaoLi-v${version}-Windows-x64-portable.zip`,
  `XiaoLi-v${version}-macOS-universal.app.zip`,
  `XiaoLi-v${version}-Linux-x64-portable.tar.gz`,
  `XiaoLi-v${version}-Linux-x64-portable.zip`,
  `xiaoli-codex-plugin-v${version}.zip`,
  "THIRD_PARTY_LICENSES.html",
  `XiaoLi-v${version}.spdx.json`,
  "SHA256SUMS.txt",
].sort();
for (const name of expected) {
  const path = resolve(directory, name);
  if (!existsSync(path) || !statSync(path).isFile() || statSync(path).size === 0) {
    throw new Error(`Missing or empty release asset: ${name}`);
  }
}
const sbom = JSON.parse(
  readFileSync(resolve(directory, `XiaoLi-v${version}.spdx.json`), "utf8"),
);
if (!String(sbom.spdxVersion ?? "").startsWith("SPDX-")) {
  throw new Error("SPDX SBOM is not a valid SPDX JSON document");
}
const notices = readFileSync(resolve(directory, "THIRD_PARTY_LICENSES.html"), "utf8");
if (
  !notices.includes("XiaoLi third-party license catalog") ||
  !notices.includes('data-complete-license-catalog="true"') ||
  !notices.includes("Full license and NOTICE texts") ||
  !notices.includes('class="legal-document"')
) {
  throw new Error("Third-party license catalog is incomplete or invalid");
}
const actual = readdirSync(directory)
  .filter((name) => statSync(resolve(directory, name)).isFile())
  .sort();
const unexpected = actual.filter((name) => !expected.includes(name));
if (unexpected.length) throw new Error(`Unexpected release assets: ${unexpected.join(", ")}`);

async function hash(path) {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest("hex");
}
const checksumLines = readFileSync(resolve(directory, "SHA256SUMS.txt"), "utf8")
  .trim()
  .split(/\r?\n/);
const checksums = new Map(
  checksumLines.map((line) => {
    const match = line.match(/^([a-f0-9]{64})  (.+)$/);
    if (!match) throw new Error(`Invalid checksum line: ${line}`);
    return [match[2], match[1]];
  }),
);
for (const name of expected.filter((name) => name !== "SHA256SUMS.txt")) {
  const observed = await hash(resolve(directory, name));
  if (checksums.get(name) !== observed) throw new Error(`Checksum mismatch: ${name}`);
}
if (checksums.size !== expected.length - 1) throw new Error("Checksum manifest entry count mismatch");
console.log(`Complete portable release validated (${expected.length} assets): ${directory}`);
