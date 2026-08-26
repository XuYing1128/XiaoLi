#!/usr/bin/env bash
# Kept LF-only by the repository's .gitattributes for portable CI execution.
set -euo pipefail

tar_archive="${1:?Linux tar.gz archive is required}"
zip_archive="${2:?Linux ZIP archive is required}"
scratch_root="${3:?scratch root is required}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

[[ -f "${tar_archive}" ]] || { echo "Missing Linux tar archive: ${tar_archive}" >&2; exit 1; }
[[ -f "${zip_archive}" ]] || { echo "Missing Linux ZIP archive: ${zip_archive}" >&2; exit 1; }
mkdir -p "${scratch_root}"

smoke_archive() {
  local kind="$1"
  local archive="$2"
  local extract_root
  extract_root="$(mktemp -d "${scratch_root%/}/${kind}.XXXXXX")"
  if [[ "${kind}" == "tar" ]]; then
    tar -xzf "${archive}" -C "${extract_root}"
  else
    unzip -q "${archive}" -d "${extract_root}"
  fi

  local appimage="${extract_root}/XiaoLi/XiaoLi-x86_64.AppImage"
  [[ -f "${appimage}" ]] || {
    echo "${kind} archive is missing XiaoLi/XiaoLi-x86_64.AppImage" >&2
    exit 1
  }
  grep -Fq 'data-complete-license-catalog="true"' \
    "${extract_root}/XiaoLi/THIRD_PARTY_LICENSES.html" || {
    echo "${kind} archive is missing the complete offline license catalog" >&2
    exit 1
  }
  node "${repo_root}/scripts/release/validate-plugin.mjs" \
    "${extract_root}/XiaoLi/plugin/xiaoli-model-monitor"
  if [[ "${kind}" == "tar" && ! -x "${appimage}" ]]; then
    echo "tar archive did not preserve the AppImage executable bit" >&2
    exit 1
  fi
  # ZIP extraction does not preserve Unix mode on every client, which is why
  # the public quick start documents this one-time chmod.
  chmod +x "${appimage}"

  APPIMAGE_EXTRACT_AND_RUN=1 pwsh -NoLogo -NoProfile -NonInteractive -File \
    "${repo_root}/scripts/release/Test-Portable.ps1" \
    -Executable "${appimage}" \
    -ScratchRoot "${extract_root}/smoke-state"

  # Exercise the exact plugin commands that survive after this shell exits.
  # Passing the AppImage runtime flag only to the GUI is insufficient: Codex
  # starts MCP and hooks later in independent processes that inherit no such
  # environment variable.
  local plugin_home="${extract_root}/plugin-home"
  local plugin_state="${extract_root}/plugin-state"
  local install_result="${extract_root}/install-plugin.json"
  mkdir -p "${plugin_home}" "${plugin_state}"
  HOME="${plugin_home}" \
    XDG_CONFIG_HOME="${plugin_home}/.config" \
    XDG_DATA_HOME="${plugin_home}/.local/share" \
    XIAOLI_STATE_DIR="${plugin_state}" \
    "${appimage}" --appimage-extract-and-run --install-plugin > "${install_result}"
  node -e '
    const fs = require("node:fs");
    const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    if (result.ok !== true) throw new Error(`plugin install failed: ${JSON.stringify(result)}`);
  ' "${install_result}"

  local installed_plugin="${plugin_home}/plugins/xiaoli-model-monitor"
  [[ -f "${installed_plugin}/.mcp.json" ]] || {
    echo "${kind} AppImage did not install an MCP manifest into the isolated HOME" >&2
    exit 1
  }
  [[ -f "${installed_plugin}/hooks/hooks.json" ]] || {
    echo "${kind} AppImage did not install hooks into the isolated HOME" >&2
    exit 1
  }

  node - \
    "${installed_plugin}/.mcp.json" \
    "${installed_plugin}/hooks/hooks.json" \
    "${plugin_home}" \
    "${plugin_state}" <<'NODE'
const fs = require("node:fs");
const { spawnSync } = require("node:child_process");

const [mcpPath, hooksPath, pluginHome, stateRoot] = process.argv.slice(2);
const mcp = JSON.parse(fs.readFileSync(mcpPath, "utf8"));
const hooks = JSON.parse(fs.readFileSync(hooksPath, "utf8"));
const server = mcp?.mcpServers?.["xiaoli-model-monitor"];
if (!server || typeof server.command !== "string" || !Array.isArray(server.args)) {
  throw new Error("installed MCP manifest has no runnable XiaoLi server");
}
if (
  server.args[0] !== "--appimage-extract-and-run" ||
  server.args[1] !== "--mcp-server"
) {
  throw new Error(`installed MCP command does not persist no-FUSE mode: ${JSON.stringify(server)}`);
}

const hookCommand = hooks?.hooks?.SessionStart?.[0]?.hooks?.[0]?.command;
if (
  typeof hookCommand !== "string" ||
  !hookCommand.includes("--appimage-extract-and-run --hook-capture")
) {
  throw new Error(`installed hook command does not persist no-FUSE mode: ${hookCommand}`);
}

const cleanEnvironment = {
  ...process.env,
  HOME: pluginHome,
  XDG_CONFIG_HOME: `${pluginHome}/.config`,
  XDG_DATA_HOME: `${pluginHome}/.local/share`,
  XIAOLI_STATE_DIR: stateRoot,
};
delete cleanEnvironment.APPIMAGE_EXTRACT_AND_RUN;
delete cleanEnvironment.APPIMAGE;
delete cleanEnvironment.APPDIR;
delete cleanEnvironment.ARGV0;

const mcpInput = [
  { jsonrpc: "2.0", id: 1, method: "initialize", params: {} },
  { jsonrpc: "2.0", id: 2, method: "tools/list", params: {} },
].map(JSON.stringify).join("\n") + "\n";
const mcpRun = spawnSync(server.command, server.args, {
  input: mcpInput,
  encoding: "utf8",
  env: cleanEnvironment,
  timeout: 60_000,
  maxBuffer: 8 * 1024 * 1024,
});
if (mcpRun.error || mcpRun.status !== 0) {
  throw new Error(
    `installed MCP command failed without inherited AppImage mode: ${mcpRun.error ?? mcpRun.stderr}`,
  );
}
const responses = mcpRun.stdout
  .split(/\r?\n/u)
  .filter(Boolean)
  .map((line) => JSON.parse(line));
const tools = responses.find((response) => response.id === 2)?.result?.tools;
if (!Array.isArray(tools) || tools.length < 6) {
  throw new Error("installed MCP command did not return the expected read-only tools");
}

const hookInput = JSON.stringify({
  hook_event_name: "UserPromptSubmit",
  session_id: "00000000-0000-0000-0000-000000000001",
  turn_id: "00000000-0000-0000-0000-000000000002",
  model: "gpt-5.6-sol",
});
const hookEnvironment = {
  ...cleanEnvironment,
  PLUGIN_DATA: stateRoot,
};
const hookRun = spawnSync("/bin/sh", ["-c", hookCommand], {
  input: hookInput,
  encoding: "utf8",
  env: hookEnvironment,
  timeout: 60_000,
  maxBuffer: 8 * 1024 * 1024,
});
if (hookRun.error || hookRun.status !== 0) {
  throw new Error(
    `installed hook command failed without inherited AppImage mode: ${hookRun.error ?? hookRun.stderr}`,
  );
}
const hookResponse = JSON.parse(hookRun.stdout.trim());
if (hookResponse.continue !== true || hookResponse.suppressOutput !== true) {
  throw new Error("installed hook command did not return the fail-open response");
}
NODE
}

smoke_archive tar "${tar_archive}"
smoke_archive zip "${zip_archive}"
echo "Linux archive smoke passed: ${tar_archive} and ${zip_archive}"
