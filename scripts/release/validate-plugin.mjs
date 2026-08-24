import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { resolve, join, relative } from "node:path";

const root = resolve(process.argv[2] ?? "plugin/xiaoli-model-monitor");
const required = [
  ".codex-plugin/plugin.json",
  ".mcp.json",
  "hooks/hooks.json",
  "skills/model-monitor/SKILL.md",
  "README.md",
  "assets/icon.svg",
];
for (const path of required) {
  if (!existsSync(join(root, path))) throw new Error(`Missing plugin file: ${path}`);
}

const manifest = JSON.parse(readFileSync(join(root, ".codex-plugin/plugin.json"), "utf8"));
if (manifest.name !== "xiaoli-model-monitor") throw new Error("Unexpected plugin name");
if (manifest.version !== "0.1.0-beta.1") throw new Error("Unexpected plugin version");
if (manifest.license !== "PolyForm-Noncommercial-1.0.0") {
  throw new Error("Unexpected plugin license");
}
if (manifest.skills !== "./skills/" || manifest.mcpServers !== "./.mcp.json") {
  throw new Error("Plugin skill or MCP entry is not portable-relative");
}

const mcp = JSON.parse(readFileSync(join(root, ".mcp.json"), "utf8"));
const server = mcp.mcpServers?.["xiaoli-model-monitor"];
if (server?.command !== "{{XIAOLI_EXECUTABLE}}" || server?.args?.[0] !== "--mcp-server") {
  throw new Error("Plugin MCP server is not wired to the Rust executable placeholder");
}

const hooksText = readFileSync(join(root, "hooks/hooks.json"), "utf8");
const hooks = JSON.parse(hooksText);
for (const eventName of ["SessionStart", "UserPromptSubmit", "SubagentStart", "Stop"]) {
  if (!Array.isArray(hooks.hooks?.[eventName])) throw new Error(`Missing ${eventName} hook`);
}
if (!hooksText.includes("{{XIAOLI_EXECUTABLE}}") || !hooksText.includes("--hook-capture")) {
  throw new Error("Plugin hooks are not wired to the Rust executable placeholder");
}

const files = [];
function walk(directory) {
  for (const name of readdirSync(directory)) {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) walk(path);
    else files.push(relative(root, path).replaceAll("\\", "/"));
  }
}
walk(root);
const runtimeScripts = files.filter((path) => /\.(?:c?js|mjs|ts)$/i.test(path));
if (runtimeScripts.length) {
  throw new Error(`Portable plugin must not ship a Node runtime: ${runtimeScripts.join(", ")}`);
}
console.log(`Plugin validated (${files.length} files): ${root}`);
