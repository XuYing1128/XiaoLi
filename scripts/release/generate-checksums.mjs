import { createHash } from "node:crypto";
import { createReadStream, readdirSync, statSync, writeFileSync } from "node:fs";
import { basename, resolve } from "node:path";

const directory = resolve(process.argv[2] ?? "release-out");
const output = resolve(process.argv[3] ?? `${directory}/SHA256SUMS.txt`);
const files = readdirSync(directory)
  .map((name) => resolve(directory, name))
  .filter((path) => statSync(path).isFile() && resolve(path) !== output)
  .sort((a, b) => basename(a).localeCompare(basename(b), "en"));
if (!files.length) throw new Error("No release files found for checksum generation");

async function hash(path) {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest("hex");
}
const lines = [];
for (const path of files) lines.push(`${await hash(path)}  ${basename(path)}`);
writeFileSync(output, `${lines.join("\n")}\n`, "utf8");
console.log(`Wrote ${lines.length} checksums to ${output}`);
