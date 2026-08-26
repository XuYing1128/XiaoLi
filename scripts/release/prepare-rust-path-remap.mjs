import { appendFileSync } from "node:fs";
import { resolve } from "node:path";

const githubEnv = process.env.GITHUB_ENV;
if (!githubEnv) {
  throw new Error("GITHUB_ENV is required; this helper is intended for CI builds");
}

const candidates = [
  ["workspace", process.env.GITHUB_WORKSPACE ?? resolve(".")],
  ["home", process.env.USERPROFILE ?? process.env.HOME],
  ["cargo", process.env.CARGO_HOME],
  ["rustup", process.env.RUSTUP_HOME],
  ["target", process.env.CARGO_TARGET_DIR],
];
const seen = new Set();
const mappings = [];
for (const [label, raw] of candidates) {
  if (!raw) continue;
  const source = resolve(raw);
  const key = process.platform === "win32" ? source.toLowerCase() : source;
  if (seen.has(key)) continue;
  if (/\s/.test(source)) {
    throw new Error(
      `Cannot safely encode the ${label} path in RUSTFLAGS because it contains whitespace`,
    );
  }
  seen.add(key);
  mappings.push({ label, source });
}

if (mappings.length < 2) {
  throw new Error("Expected at least workspace and home path remapping entries");
}

function appendFlags(name, flags) {
  const existing = process.env[name] ?? "";
  if (/\r|\n/u.test(existing)) {
    throw new Error(`Cannot preserve multiline ${name} in GITHUB_ENV`);
  }
  const value = existing.length > 0 ? `${existing} ${flags.join(" ")}` : flags.join(" ");
  appendFileSync(githubEnv, `${name}=${value}\n`, "utf8");
}

const rustFlags = mappings.map(
  ({ label, source }) => `--remap-path-prefix=${source}=<${label}>`,
);
const nativeFlags =
  process.platform === "win32"
    ? [
        "/experimental:deterministic",
        ...mappings.map(({ label, source }) => `/pathmap:${source}=<${label}>`),
      ]
    : mappings.flatMap(({ label, source }) => [
        `-ffile-prefix-map=${source}=<${label}>`,
        `-fdebug-prefix-map=${source}=<${label}>`,
      ]);

appendFlags("RUSTFLAGS", rustFlags);
appendFlags("CFLAGS", nativeFlags);
appendFlags("CXXFLAGS", nativeFlags);
console.log(
  `Configured ${mappings.length} private path remapping entries for Rust, C and C++`,
);
