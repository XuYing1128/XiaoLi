import { readFileSync } from "node:fs";

const expected = process.argv[2];
if (!expected) throw new Error("Expected version argument is required");
const packageJson = JSON.parse(readFileSync("package.json", "utf8"));
const tauri = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
const cargo = readFileSync("src-tauri/Cargo.toml", "utf8");
const runtime = readFileSync("src-tauri/src/lib.rs", "utf8");
const plugin = JSON.parse(
  readFileSync("plugin/xiaoli-model-monitor/.codex-plugin/plugin.json", "utf8"),
);
const cargoVersion = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const cargoLicense = cargo.match(/^license\s*=\s*"([^"]+)"/m)?.[1];
const runtimePluginVersion = runtime.match(
  /^const PLUGIN_VERSION:\s*&str\s*=\s*"([^"]+)";/m,
)?.[1];
const versions = {
  "package.json": packageJson.version,
  "tauri.conf.json": tauri.version,
  "Cargo.toml": cargoVersion,
  "plugin.json": plugin.version,
  "src-tauri/src/lib.rs PLUGIN_VERSION": runtimePluginVersion,
};
for (const [source, version] of Object.entries(versions)) {
  if (version !== expected) throw new Error(`${source}: expected ${expected}, received ${version}`);
}
const expectedLicense = "PolyForm-Noncommercial-1.0.0";
for (const [source, license] of Object.entries({
  "package.json": packageJson.license,
  "Cargo.toml": cargoLicense,
  "plugin.json": plugin.license,
})) {
  if (license !== expectedLicense) {
    throw new Error(`${source}: expected license ${expectedLicense}, received ${license}`);
  }
}
if (
  tauri.productName !== "XiaoLi" ||
  tauri.identifier !== "io.github.xuying1128.xiaoli" ||
  tauri.bundle?.publisher !== "XuYing1128"
) {
  throw new Error("Tauri product, bundle identifier, or publisher metadata is inconsistent");
}
if (tauri.bundle?.targets?.length !== 0) {
  throw new Error("tauri.conf.json must remain portable-only with no default installer targets");
}
console.log(`Version and portable-only configuration verified: ${expected}`);
