# Third-party notices

This file distinguishes software shipped as part of XiaoLi from projects consulted only as references.

## Runtime and build dependencies

XiaoLi uses [Tauri](https://github.com/tauri-apps/tauri) and official Tauri plugins. Tauri is available under the MIT License or Apache License 2.0. Individual Rust and JavaScript dependencies retain the licenses declared in their package metadata and lockfiles. Release archives include the generated `THIRD_PARTY_LICENSES.html` inventory.

Signed relay-baseline packages are verified locally with [`ed25519-dalek`](https://github.com/dalek-cryptography/curve25519-dalek) (BSD-3-Clause) and decoded with the Rust [`base64`](https://github.com/marshallpierce/rust-base64) crate (MIT OR Apache-2.0). The generated license inventory and SPDX SBOM include the exact locked versions and transitive dependencies.

## Protocol reference

[OpenAI Codex](https://github.com/openai/codex) is the authoritative reference for rollout, model-switching, and reroute-event semantics. XiaoLi does not vendor OpenAI Codex source code or artwork. OpenAI Codex is available under Apache License 2.0.

## Release-pinned community fingerprint data

XiaoLi includes small, normalized count tables derived from the following public reference datasets. They are used only for low-confidence, cross-protocol relative ranking when no live matched official API reference is available. They cannot produce a physical-model proof or a hard PASS/FAIL verdict.

- [`fpverify`](https://github.com/Mohamed7415/fpverify) at commit `bcd60d955c92efdc6419a628f10de07a6d123ee5`, copyright © 2026 Mohamed7415 and fpverify contributors, MIT License. XiaoLi uses the July 2026 GPT-5.6 Sol and Terra agent-harness count tables.
- [`llm-fingerprint`](https://github.com/dreamor/llm-fingerprint) at commit `133d40c117980b5c52d0873b8e25d5cc7616e043`, tool copyright © 2026 dreamor, MIT License. XiaoLi uses a reduced July 2026 GPT-5.5 distribution derived from the bundled research data. The upstream project identifies its research data as CC-BY and cites Tomáš Bruckner's “Single-token output distributions as behavioral fingerprints of large language models” dataset.

The included tables preserve source commit, collection date, channel, protocol mismatch and sample-count metadata in every report. XiaoLi does not copy either project's network client or scoring implementation.

## Reference-only projects

The community projects named in [DESIGN.md](./DESIGN.md) were consulted only for interaction, information-hierarchy, and token-accounting principles. No source code, character, game asset, screenshot, logo, or illustration from those projects is copied or distributed with XiaoLi. Because they are not redistributed components, this file does not add them as bundled copyright items.

The generated character and icon masters have a separate provenance and distribution record in [ASSET_PROVENANCE.md](./ASSET_PROVENANCE.md). The user's local visual reference is not included in the product or its installation package.
