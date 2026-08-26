import {
  closeSync,
  lstatSync,
  openSync,
  readSync,
  readdirSync,
  readlinkSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const CHUNK_BYTES = 1024 * 1024;

function collectEntries(path, files, symbolicLinks) {
  const stat = lstatSync(path);
  if (stat.isSymbolicLink()) {
    symbolicLinks.push({ path, target: readlinkSync(path) });
    return;
  }
  if (stat.isDirectory()) {
    for (const entry of readdirSync(path)) {
      collectEntries(join(path, entry), files, symbolicLinks);
    }
    return;
  }
  if (stat.isFile()) files.push(path);
}

function normalizeComparisonPath(path) {
  let candidate = path;
  if (process.platform === "win32") {
    if (/^\\\\\?\\UNC\\/iu.test(candidate)) {
      candidate = `\\\\${candidate.slice(8)}`;
    } else if (/^\\\\\?\\/u.test(candidate)) {
      candidate = candidate.slice(4);
    }
  }
  const normalized = resolve(candidate);
  return process.platform === "win32" ? normalized.toLowerCase() : normalized;
}

function privatePrefixForSymbolicLinkTarget(linkPath, target, prefixes) {
  const resolvedTarget = isAbsolute(target) ? target : resolve(dirname(linkPath), target);
  const normalizedTarget = normalizeComparisonPath(resolvedTarget);
  for (const prefix of prefixes) {
    const normalizedPrefix = normalizeComparisonPath(prefix.source);
    const remainder = relative(normalizedPrefix, normalizedTarget);
    if (
      remainder === "" ||
      (remainder !== ".." && !remainder.startsWith(`..${sep}`) && !isAbsolute(remainder))
    ) {
      return prefix;
    }
  }
  return null;
}

function privatePrefixes(env, cwd) {
  const values = [
    ["workspace", env.GITHUB_WORKSPACE],
    ["home", env.USERPROFILE ?? env.HOME],
    ["cargo", env.CARGO_HOME],
    ["rustup", env.RUSTUP_HOME],
    ["target", env.CARGO_TARGET_DIR],
    ["current working directory", cwd],
  ];
  const seen = new Set();
  const result = [];
  for (const [label, raw] of values) {
    if (!raw) continue;
    const source = resolve(raw);
    if (source.length < 4 || source === resolve(source, "..")) continue;
    const key = process.platform === "win32" ? source.toLowerCase() : source;
    if (seen.has(key)) continue;
    seen.add(key);
    result.push({ label, source });
  }
  return result;
}

function needlesFor(prefixes) {
  const needles = [];
  for (const prefix of prefixes) {
    const variants = new Set([
      prefix.source,
      prefix.source.replaceAll("\\", "/"),
      prefix.source.replaceAll("/", "\\"),
    ]);
    for (const value of variants) {
      if (!value) continue;
      needles.push({ label: prefix.label, encoding: "UTF-8", bytes: Buffer.from(value) });
      needles.push({
        label: prefix.label,
        encoding: "UTF-16LE",
        bytes: Buffer.from(value, "utf16le"),
      });
    }
  }
  return needles;
}

function scanFile(path, needles) {
  const overlap = Math.max(0, ...needles.map((needle) => needle.bytes.length - 1));
  const descriptor = openSync(path, "r");
  const chunk = Buffer.allocUnsafe(CHUNK_BYTES);
  let tail = Buffer.alloc(0);
  try {
    while (true) {
      const count = readSync(descriptor, chunk, 0, chunk.length, null);
      if (count === 0) break;
      const window = Buffer.concat([tail, chunk.subarray(0, count)]);
      for (const needle of needles) {
        if (window.indexOf(needle.bytes) >= 0) return needle;
      }
      tail = overlap > 0 ? Buffer.from(window.subarray(Math.max(0, window.length - overlap))) : Buffer.alloc(0);
    }
  } finally {
    closeSync(descriptor);
  }
  return null;
}

export function findPrivateBuildPathLeaks(paths, options = {}) {
  const env = options.env ?? process.env;
  const cwd = resolve(options.cwd ?? ".");
  const prefixes = privatePrefixes(env, cwd);
  if (prefixes.length === 0) throw new Error("No private build prefixes were available to scan");
  const needles = needlesFor(prefixes);
  const files = [];
  const symbolicLinks = [];
  for (const input of paths) collectEntries(resolve(input), files, symbolicLinks);
  const findings = [];
  for (const symbolicLink of symbolicLinks) {
    const match = privatePrefixForSymbolicLinkTarget(
      symbolicLink.path,
      symbolicLink.target,
      prefixes,
    );
    if (match) {
      findings.push(
        `${basename(symbolicLink.path)} resolves into the unremapped ${match.label} prefix through a symbolic link`,
      );
    }
  }
  for (const path of files) {
    const match = scanFile(path, needles);
    if (match) {
      findings.push(
        `${basename(path)} contains the unremapped ${match.label} prefix (${match.encoding})`,
      );
    }
  }
  return findings;
}

const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const paths = process.argv.slice(2);
  if (paths.length === 0) throw new Error("At least one executable, app bundle, or archive path is required");
  const findings = findPrivateBuildPathLeaks(paths);
  if (findings.length > 0) {
    for (const finding of findings) console.error(finding);
    process.exitCode = 1;
  } else {
    console.log(`Private build-path scan passed (${paths.length} target${paths.length === 1 ? "" : "s"})`);
  }
}
