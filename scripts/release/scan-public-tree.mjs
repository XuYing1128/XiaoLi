import { execFileSync } from "node:child_process";
import { closeSync, openSync, readSync, statSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const CHUNK_BYTES = 1024 * 1024;
const TEXT_OVERLAP = 512;

const forbiddenPaths = [
  /(?:^|\/)(?:target|dist|node_modules)(?:\/|$)/i,
  /(?:^|\/)(?:edge-profile-test|race-state|shadow-state)(?:\/|$)/i,
  /(?:^|\/)(?:release-out(?:-[^/]+)?|release-smoke(?:-[^/]+)?)(?:\/|$)/i,
  /\.(?:db|db3|sqlite|sqlite3|jsonl|log)$/i,
];

const uuid = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const textFindings = [
  {
    label: "Windows user profile path",
    pattern:
      /\b[A-Za-z]:[\\/]+Users[\\/]+(?!(?:<(?:user|username|name)>|example|username|your-name)(?:[\\/]|$))[^\\/\s\"'<>]+/i,
  },
  { label: "Windows account SID", pattern: /\bS-1-5-21-(?:\d+-){3}\d+\b/ },
  {
    label: "local Codex rollout path",
    pattern: /(?:\\|\/)\.codex(?:\\|\/)sessions(?:\\|\/)/i,
  },
  {
    label: "private macOS home path",
    pattern:
      /\/Users\/(?!(?:<(?:user|username|name)>|example|username|your-name)(?:\/|$))[^/\s\"'<>]+\//i,
  },
  {
    label: "private Linux home path",
    pattern:
      /\/home\/(?!(?:<(?:user|username|name)>|example|username|runner|your-name)(?:\/|$))[^/\s\"'<>]+\//i,
  },
  {
    label: "real thread/turn/session UUID",
    pattern: new RegExp(
      `\\b(?:thread(?:_?id)?|turn(?:_?id)?|session(?:_?id)?|conversation(?:_?id)?)\\b[^\\r\\n]{0,32}?(${uuid})`,
      "i",
    ),
    accept: (match) => !isClearlySyntheticUuid(match[1]),
  },
];

function isClearlySyntheticUuid(value) {
  return /^00000000-0000-(?:0000|4000)-[089a-f]000-000000000[0-9a-f]{3}$/i.test(
    value ?? "",
  );
}

function detectEncoding(header) {
  if (header.length >= 2 && header[0] === 0xff && header[1] === 0xfe) {
    return { encoding: "utf-16le", bomBytes: 2 };
  }
  if (header.length >= 2 && header[0] === 0xfe && header[1] === 0xff) {
    return { encoding: "utf-16be", bomBytes: 2 };
  }
  if (
    header.length >= 3 &&
    header[0] === 0xef &&
    header[1] === 0xbb &&
    header[2] === 0xbf
  ) {
    return { encoding: "utf-8", bomBytes: 3 };
  }
  if (header.includes(0)) {
    // Some Windows tools emit BOM-less UTF-16. Recognize the regular NUL-byte
    // lane without mistaking arbitrary binaries for text.
    const sampleLength = Math.min(header.length, 8192);
    let evenNuls = 0;
    let oddNuls = 0;
    for (let index = 0; index < sampleLength; index += 1) {
      if (header[index] !== 0) continue;
      if (index % 2 === 0) evenNuls += 1;
      else oddNuls += 1;
    }
    const pairs = Math.max(1, Math.floor(sampleLength / 2));
    if (oddNuls / pairs > 0.3 && evenNuls / pairs < 0.05) {
      return { encoding: "utf-16le", bomBytes: 0 };
    }
    if (evenNuls / pairs > 0.3 && oddNuls / pairs < 0.05) {
      return { encoding: "utf-16be", bomBytes: 0 };
    }
    return null;
  }
  return { encoding: "utf-8", bomBytes: 0 };
}

function findingsInText(text) {
  const found = [];
  for (const finding of textFindings) {
    const match = finding.pattern.exec(text);
    if (match && (!finding.accept || finding.accept(match))) {
      found.push(finding.label);
    }
  }
  return found;
}

export function scanTextFile(path) {
  const size = statSync(path).size;
  if (size === 0) return [];

  const descriptor = openSync(path, "r");
  try {
    const first = Buffer.allocUnsafe(Math.min(CHUNK_BYTES, size));
    const firstLength = readSync(descriptor, first, 0, first.length, 0);
    const encoding = detectEncoding(first.subarray(0, firstLength));
    if (!encoding) return [];

    const decoder = new TextDecoder(encoding.encoding, { fatal: false });
    const labels = new Set();
    let position = 0;
    let overlap = "";
    let firstChunk = true;

    while (position < size) {
      const buffer = Buffer.allocUnsafe(Math.min(CHUNK_BYTES, size - position));
      const length = readSync(descriptor, buffer, 0, buffer.length, position);
      if (length === 0) break;
      position += length;

      let chunk = buffer.subarray(0, length);
      if (firstChunk && encoding.bomBytes > 0) {
        chunk = chunk.subarray(encoding.bomBytes);
      }
      firstChunk = false;
      const text = overlap + decoder.decode(chunk, { stream: position < size });
      for (const label of findingsInText(text)) labels.add(label);
      overlap = text.slice(-TEXT_OVERLAP);
    }
    const tail = overlap + decoder.decode();
    for (const label of findingsInText(tail)) labels.add(label);
    return [...labels];
  } finally {
    closeSync(descriptor);
  }
}

export function listPublicFiles(root) {
  // Include non-ignored, not-yet-staged files during local release
  // preparation; CI is clean and therefore scans the immutable tag tree.
  return execFileSync(
    "git",
    ["-C", root, "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
    { encoding: "utf8" },
  )
    .split("\0")
    .filter(Boolean);
}

export function scanPublicTree(root, paths = listPublicFiles(root)) {
  const findings = [];
  for (const relativePath of paths) {
    const normalizedPath = relativePath.replaceAll("\\", "/");
    if (forbiddenPaths.some((pattern) => pattern.test(normalizedPath))) {
      findings.push(`${relativePath}: forbidden generated/private artifact path`);
      continue;
    }
    for (const label of findingsInText(relativePath)) {
      findings.push(`${relativePath}: ${label}`);
    }
    const absolutePath = resolve(root, relativePath);
    if (!statSync(absolutePath).isFile()) continue;
    for (const label of scanTextFile(absolutePath)) {
      findings.push(`${relativePath}: ${label}`);
    }
  }
  return findings;
}

const isMain =
  process.argv[1] &&
  resolve(fileURLToPath(import.meta.url)).toLowerCase() ===
    resolve(process.argv[1]).toLowerCase();
if (isMain) {
  const root = resolve(process.argv[2] ?? ".");
  const tracked = listPublicFiles(root);
  const findings = scanPublicTree(root, tracked);
  if (findings.length) {
    throw new Error(`Public-tree privacy scan failed:\n${findings.join("\n")}`);
  }
  console.log(`Public-tree privacy scan passed (${tracked.length} public files)`);
}
