# Community reference baselines

When no matched official API key is available, XiaoLi can compare the normalized distribution from the current one-token probes with release-pinned public reference tables. This is a low-confidence, reproducible direction—not a substitute for a live official pairing and not proof of the physical serving model.

## References included in v0.2.0-beta.1

| Reference | Source | Collection conditions | Permitted conclusion |
| --- | --- | --- | --- |
| GPT-5.6 Sol / Terra | [`fpverify`](https://github.com/Mohamed7415/fpverify), pinned commit `bcd60d955c92efdc6419a628f10de07a6d123ee5` | July 2026, Cursor agent harness, 11 independent instances per cell | Cross-protocol relative distance and ranking only |
| GPT-5.5 | [`llm-fingerprint`](https://github.com/dreamor/llm-fingerprint), pinned commit `133d40c117980b5c52d0873b8e25d5cc7616e043` | 2026-07-21, OpenRouter single-question protocol, 30 valid samples per cell | Cross-prompt-protocol relative distance and ranking only |

XiaoLi includes only normalized counts from cells that can be mapped unambiguously to its own probe domains. It includes no API key, raw response body, or upstream network client. See [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) for attribution and licenses.

## Why confidence stays low

- A fingerprint is conditional on the model, sampling parameters, system prompt, API surface, deployment, date, and request protocol.
- The Sol/Terra references were collected as a battery inside an agent harness; XiaoLi sends randomized independent API requests.
- The GPT-5.5 reference used single API questions, but its fixed prompts are not identical to XiaoLi's randomized paraphrases.
- A relay may recognize a public audit and selectively provide the genuine model.
- Provider updates and deployment drift can invalidate an older reference.

The report therefore preserves source commit, collection date, shared-cell count, sample count, and protocol mismatch. Community data alone cannot set the overall verdict to `consistent` or `significantlyDifferent`, and XiaoLi never turns its closest match into “actual model X”.

## Stronger evidence

For consequential checks, pair the relay with the official API at the same time, using the same exact model, parameters, API surface, and probes. XiaoLi randomly interleaves official and relay requests and then computes per-cell JSD and string-kernel MMD. Even a passing official pair only means that this black-box run was behaviorally consistent with its reference; it is not cryptographic identity proof.
