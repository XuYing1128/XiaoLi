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
if (manifest.version !== "0.2.0-beta.1") throw new Error("Unexpected plugin version");
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
const expectedHooks = [
  "SessionStart",
  "UserPromptSubmit",
  "SubagentStart",
  "SubagentStop",
  "Stop",
];
const actualHooks = Object.keys(hooks.hooks ?? {}).sort();
if (JSON.stringify(actualHooks) !== JSON.stringify([...expectedHooks].sort())) {
  throw new Error(`Expected exactly five metadata hooks; received: ${actualHooks.join(", ")}`);
}
for (const eventName of expectedHooks) {
  const registrations = hooks.hooks[eventName];
  if (!Array.isArray(registrations) || registrations.length === 0) {
    throw new Error(`Missing ${eventName} hook`);
  }
  const commands = registrations.flatMap((entry) => entry.hooks ?? []);
  if (
    commands.length === 0 ||
    commands.some(
      (hook) =>
        hook.type !== "command" ||
        !String(hook.command ?? "").includes("{{XIAOLI_EXECUTABLE}}") ||
        !String(hook.command ?? "").includes("--hook-capture"),
    )
  ) {
    throw new Error(`${eventName} is not wired to the fail-open Rust hook handler`);
  }
}
if (!hooksText.includes("{{XIAOLI_EXECUTABLE}}") || !hooksText.includes("--hook-capture")) {
  throw new Error("Plugin hooks are not wired to the Rust executable placeholder");
}

const skillText = readFileSync(join(root, "skills/model-monitor/SKILL.md"), "utf8");
const readmeText = readFileSync(join(root, "README.md"), "utf8");
const expectedTools = [
  "get_monitor_summary",
  "get_session_detail",
  "render_monitor_card",
  "get_connection_origin",
  "list_relay_audits",
  "get_relay_audit",
];
for (const tool of expectedTools) {
  if (!skillText.includes(`\`${tool}`) || !readmeText.includes(`\`${tool}`)) {
    throw new Error(`Plugin documentation is missing read-only MCP tool: ${tool}`);
  }
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
