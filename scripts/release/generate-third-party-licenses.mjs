import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { basename, dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const require = createRequire(import.meta.url);
const spdxLicenses = require("spdx-license-list/full");
const repositoryRoot = resolve(import.meta.dirname, "../..");
const output = resolve(process.argv[2] ?? "release-out/THIRD_PARTY_LICENSES.html");
const legalFilePattern = /^(?:licen[cs]e|copying|copyright|notice|authors)(?:$|[._ -])/i;
const licenseFilePattern = /^(?:licen[cs]e|copying|copyright)(?:$|[._ -])/i;
const maxLegalFileBytes = 4 * 1024 * 1024;

function command(program, args) {
  const windowsCommand = process.platform === "win32" && program === "pnpm";
  const executable = windowsCommand
    ? process.env.ComSpec ?? "cmd.exe"
    : process.platform === "win32"
      ? `${program}.exe`
      : program;
  const commandArgs = windowsCommand
    ? ["/d", "/s", "/c", `pnpm ${args.join(" ")}`]
    : args;
  const result = spawnSync(executable, commandArgs, {
    cwd: repositoryRoot,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`${program} ${args.join(" ")} failed:\n${result.stderr || result.stdout}`);
  }
  return result.stdout;
}

function escape(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function readLegalFile(path) {
  const size = statSync(path).size;
  if (size > maxLegalFileBytes) {
    throw new Error(`Legal document is unexpectedly large: ${basename(path)} (${size} bytes)`);
  }
  const text = readFileSync(path, "utf8").replaceAll("\r\n", "\n").trim();
  if (!text || text.includes("\0")) {
    throw new Error(`Legal document is empty or not plain text: ${basename(path)}`);
  }
  return text;
}

function packageLegalFiles(root, explicitLicenseFile) {
  const paths = new Set();
  if (explicitLicenseFile) {
    const explicit = resolve(root, explicitLicenseFile);
    if (!existsSync(explicit)) {
      throw new Error(`Declared license file is missing: ${basename(explicit)}`);
    }
    paths.add(explicit);
  }
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (entry.isFile() && legalFilePattern.test(entry.name)) {
      paths.add(resolve(root, entry.name));
    }
  }
  return [...paths].sort().map((path) => ({
    name: basename(path),
    kind: licenseFilePattern.test(basename(path)) ? "license" : "notice",
    text: readLegalFile(path),
    origin: "package",
  }));
}

function spdxIds(expression) {
  return [...new Set(String(expression).match(/[0-9A-Za-z][0-9A-Za-z.+-]*/g) ?? [])]
    .filter((token) => Object.hasOwn(spdxLicenses, token));
}

function canonicalLicenseDocuments(expression) {
  return spdxIds(expression).map((id) => ({
    name: `SPDX ${id}`,
    kind: "license",
    text: spdxLicenses[id].licenseText.replaceAll("\r\n", "\n").trim(),
    origin: "spdx",
  }));
}

const cargo = JSON.parse(
  command("cargo", [
    "metadata",
    "--format-version",
    "1",
    "--locked",
    "--manifest-path",
    "src-tauri/Cargo.toml",
  ]),
);
const rust = cargo.packages
  .filter((item) => item.name !== "xiaoli")
  .map((item) => ({
    ecosystem: "Rust",
    name: item.name,
    version: item.version,
    license: item.license ?? "NOT DECLARED",
    source: item.repository ?? item.homepage ?? "",
    attribution: (item.authors ?? []).join(", "),
    root: dirname(item.manifest_path),
    explicitLicenseFile: item.license_file,
  }));

const pnpm = JSON.parse(command("pnpm", ["licenses", "list", "--json"]));
const javascript = Object.entries(pnpm).flatMap(([license, entries]) =>
  entries.flatMap((item) =>
    item.versions.map((version, index) => ({
      ecosystem: "JavaScript",
      name: item.name,
      version,
      license: item.license ?? license,
      source: item.homepage ?? "",
      attribution: item.author ?? "",
      root: item.paths?.[index] ?? item.paths?.[0],
    })),
  ),
);

const packages = [...rust, ...javascript].sort((a, b) =>
  `${a.ecosystem}:${a.name}:${a.version}`.localeCompare(
    `${b.ecosystem}:${b.name}:${b.version}`,
    "en",
  ),
);
const undeclared = packages.filter((item) => item.license === "NOT DECLARED");
if (undeclared.length) {
  throw new Error(
    `Dependencies without declared licenses: ${undeclared.map((item) => `${item.name}@${item.version}`).join(", ")}`,
  );
}

const documents = new Map();
for (const item of packages) {
  if (!item.root || !existsSync(item.root)) {
    throw new Error(`Installed package source is unavailable: ${item.name}@${item.version}`);
  }
  const actualDocuments = packageLegalFiles(item.root, item.explicitLicenseFile);
  const canonicalDocuments = canonicalLicenseDocuments(item.license);
  const combined = [...actualDocuments, ...canonicalDocuments];
  if (!combined.some((document) => document.kind === "license")) {
    throw new Error(
      `No offline license text found for ${item.ecosystem} ${item.name}@${item.version} (${item.license})`,
    );
  }
  item.documentIds = [];
  for (const document of combined) {
    const id = createHash("sha256").update(document.text).digest("hex");
    if (!documents.has(id)) {
      documents.set(id, { ...document, id, packages: [] });
    }
    const stored = documents.get(id);
    stored.packages.push(`${item.ecosystem}: ${item.name}@${item.version}`);
    item.documentIds.push(id);
  }
  item.documentIds = [...new Set(item.documentIds)];
}

const rows = packages
  .map((item) => {
    const links = item.documentIds
      .map((id, index) => `<a href="#document-${id.slice(0, 16)}">text ${index + 1}</a>`)
      .join(", ");
    return `<tr><td>${escape(item.ecosystem)}</td><td>${escape(item.name)}</td>` +
      `<td>${escape(item.version)}</td><td>${escape(item.license)}</td>` +
      `<td>${escape(item.attribution) || "-"}</td>` +
      `<td>${item.source ? `<a href="${escape(item.source)}">project</a>` : "-"}</td>` +
      `<td>${links}</td></tr>`;
  })
  .join("\n");

const documentSections = [...documents.values()]
  .sort((a, b) => `${a.kind}:${a.name}:${a.id}`.localeCompare(`${b.kind}:${b.name}:${b.id}`, "en"))
  .map((document) => {
    const packageList = [...new Set(document.packages)].sort().join("; ");
    const label = document.origin === "spdx"
      ? `${document.name} canonical license text`
      : document.name;
    return `<section class="legal-document" id="document-${document.id.slice(0, 16)}">` +
      `<h3>${escape(label)}</h3><p><strong>Applies to:</strong> ${escape(packageList)}</p>` +
      `<pre>${escape(document.text)}</pre></section>`;
  })
  .join("\n");

const html = `<!doctype html>
<html lang="en" data-complete-license-catalog="true"><head><meta charset="utf-8"><title>XiaoLi third-party licenses</title>
<style>body{font:14px system-ui,sans-serif;max-width:1180px;margin:32px auto;padding:0 20px;color:#29242d}table{border-collapse:collapse;width:100%}th,td{padding:7px 9px;border:1px solid #d8d2dc;text-align:left;vertical-align:top}th{background:#f4f0f6}.legal-document{border-top:2px solid #d8d2dc;margin-top:32px;padding-top:12px}pre{background:#f8f6f9;border:1px solid #ded8e2;border-radius:6px;padding:14px;white-space:pre-wrap;overflow-wrap:anywhere;font:12px/1.45 ui-monospace,monospace}code{background:#f4f0f6;padding:2px 4px}</style></head>
<body><h1>XiaoLi third-party license catalog</h1>
<p>Generated entirely from the locked Rust and pnpm dependency graphs, installed package license/NOTICE files, and the bundled SPDX 3.28 license-text data. XiaoLi itself is licensed separately under <code>PolyForm-Noncommercial-1.0.0</code>. Each third-party component remains subject to its own license.</p>
<p>Generated entries: ${packages.length}. Embedded unique legal documents: ${documents.size}. The catalog is self-contained for offline reading; no local installation paths or user identifiers are included.</p>
<table><thead><tr><th>Ecosystem</th><th>Package</th><th>Version</th><th>License expression</th><th>Attribution</th><th>Project</th><th>Embedded texts and notices</th></tr></thead><tbody>
${rows}
</tbody></table><h2>Full license and NOTICE texts</h2>
${documentSections}</body></html>\n`;

if (!html.includes('data-complete-license-catalog="true"') || documents.size === 0) {
  throw new Error("Complete offline third-party license catalog gate failed");
}
if (html.includes(repositoryRoot)) {
  throw new Error("Generated third-party catalog leaked the repository path");
}
mkdirSync(dirname(output), { recursive: true });
writeFileSync(output, html, "utf8");
console.log(
  `Wrote ${packages.length} dependency entries and ${documents.size} embedded legal documents to ${output}`,
);
