//! Strict loader for user-authored deterministic relay audit probes.
//!
//! Pack bodies are intentionally non-serializable. Callers may persist only
//! [`PrivateProbePackReference`], then re-open and hash-check the file before
//! every audit. Responses are evaluated by local exact-text or exact-JSON
//! scorers; this module has no code, URL, tool, or LLM-judge execution path.

use crate::relay_audit::{PrivateProbePackReference, QualityDomain};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const MAX_PRIVATE_PROBE_PACK_BYTES: u64 = 256 * 1024;
pub const MAX_PRIVATE_PROBE_TASKS: usize = 64;
const MAX_VERSION_CHARS: usize = 64;
const MAX_ID_CHARS: usize = 64;
const MAX_BATCH_CHARS: usize = 32;
const MAX_PROMPT_CHARS: usize = 16_384;
const MAX_EXPECTED_CHARS: usize = 4_096;
const MAX_TOTAL_TEXT_CHARS: usize = 131_072;
const MAX_EXACT_JSON_DEPTH: usize = 16;
const MAX_PRIVATE_OUTPUT_TOKENS: u32 = 256;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PrivateProbeScorer {
    ExactText,
    ExactJson,
}

#[derive(Clone, Debug)]
pub struct LoadedPrivateProbeTask {
    pub id: String,
    pub batch: String,
    pub domain: QualityDomain,
    pub scorer: PrivateProbeScorer,
    pub prompt: String,
    pub expected: String,
    pub max_output_tokens: u32,
}

#[derive(Debug)]
pub struct LoadedPrivateProbePack {
    pub reference: PrivateProbePackReference,
    pub tasks: Vec<LoadedPrivateProbeTask>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrivateProbePackDocument {
    schema_version: u32,
    version: String,
    tasks: Vec<PrivateProbeTaskDocument>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrivateProbeTaskDocument {
    id: String,
    batch: String,
    domain: QualityDomain,
    scorer: PrivateProbeScorer,
    prompt: String,
    expected: String,
    max_output_tokens: u32,
}

/// Resolves a user-selected path and returns the metadata that is safe to
/// persist. This is the only operation that accepts a new hash.
pub fn resolve_private_probe_pack(path: &str) -> Result<LoadedPrivateProbePack, String> {
    load_private_probe_pack(path, None)
}

/// Re-opens a previously selected pack. A missing file, changed version, or
/// changed hash fails closed before any audit request is issued.
pub fn load_verified_private_probe_pack(
    reference: &PrivateProbePackReference,
) -> Result<LoadedPrivateProbePack, String> {
    load_private_probe_pack(reference.path.as_str(), Some(reference))
}

fn load_private_probe_pack(
    path_text: &str,
    expected_reference: Option<&PrivateProbePackReference>,
) -> Result<LoadedPrivateProbePack, String> {
    let path = Path::new(path_text.trim());
    if path_text.trim().is_empty() || !path.is_absolute() {
        return Err("private probe pack path must be an absolute local file path".to_owned());
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "private probe pack is missing or inaccessible".to_owned())?;
    let canonical_text = canonical
        .to_str()
        .ok_or_else(|| "private probe pack path must be valid Unicode".to_owned())?
        .to_owned();
    if canonical_text.chars().count() > 4_096 {
        return Err("private probe pack path is too long".to_owned());
    }
    let metadata = canonical
        .metadata()
        .map_err(|_| "private probe pack is missing or inaccessible".to_owned())?;
    if !metadata.is_file() {
        return Err("private probe pack path must identify a regular file".to_owned());
    }
    if metadata.len() == 0 || metadata.len() > MAX_PRIVATE_PROBE_PACK_BYTES {
        return Err(format!(
            "private probe pack must be between 1 byte and {MAX_PRIVATE_PROBE_PACK_BYTES} bytes"
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(&canonical)
        .and_then(|file| {
            file.take(MAX_PRIVATE_PROBE_PACK_BYTES + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| "private probe pack is missing or inaccessible".to_owned())?;
    if bytes.len() as u64 > MAX_PRIVATE_PROBE_PACK_BYTES {
        return Err("private probe pack exceeds the size limit".to_owned());
    }

    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if expected_reference.is_some_and(|expected| expected.sha256 != sha256) {
        return Err(
            "private probe pack hash changed; reselect and save the file before auditing"
                .to_owned(),
        );
    }
    let document: PrivateProbePackDocument = serde_json::from_slice(&bytes)
        .map_err(|_| "private probe pack is not valid strict schemaVersion 1 JSON".to_owned())?;
    validate_document(&document)?;
    let reference = PrivateProbePackReference {
        path: canonical_text,
        version: document.version.clone(),
        sha256,
    };
    if let Some(expected) = expected_reference {
        if expected.version != reference.version {
            return Err(
                "private probe pack version changed; reselect and save the file before auditing"
                    .to_owned(),
            );
        }
    }

    Ok(LoadedPrivateProbePack {
        reference,
        tasks: document
            .tasks
            .into_iter()
            .map(|task| LoadedPrivateProbeTask {
                id: task.id,
                batch: task.batch,
                domain: task.domain,
                scorer: task.scorer,
                prompt: task.prompt,
                expected: task.expected,
                max_output_tokens: task.max_output_tokens,
            })
            .collect(),
    })
}

fn validate_document(document: &PrivateProbePackDocument) -> Result<(), String> {
    if document.schema_version != 1 {
        return Err("private probe pack schemaVersion must be 1".to_owned());
    }
    if !is_safe_identifier(&document.version, MAX_VERSION_CHARS) {
        return Err(
            "private probe pack version must use 1-64 ASCII letters, digits, '.', '_' or '-'"
                .to_owned(),
        );
    }
    if document.tasks.is_empty() || document.tasks.len() > MAX_PRIVATE_PROBE_TASKS {
        return Err(format!(
            "private probe pack tasks must contain 1-{MAX_PRIVATE_PROBE_TASKS} items"
        ));
    }
    let mut ids = BTreeSet::new();
    let mut total_chars = 0_usize;
    for task in &document.tasks {
        if !is_safe_identifier(&task.id, MAX_ID_CHARS) {
            return Err("private probe task id is invalid".to_owned());
        }
        if !ids.insert(task.id.as_str()) {
            return Err("private probe task ids must be unique".to_owned());
        }
        if !is_safe_identifier(&task.batch, MAX_BATCH_CHARS) {
            return Err("private probe task batch is invalid".to_owned());
        }
        if !matches!(
            task.domain,
            QualityDomain::StructuredOutput
                | QualityDomain::LongContextRetrieval
                | QualityDomain::ConstraintReasoning
                | QualityDomain::Multilingual
        ) {
            return Err("private probes support only deterministic quality domains".to_owned());
        }
        validate_text(&task.prompt, 1, MAX_PROMPT_CHARS, "prompt")?;
        validate_text(&task.expected, 1, MAX_EXPECTED_CHARS, "expected")?;
        total_chars = total_chars
            .saturating_add(task.prompt.chars().count())
            .saturating_add(task.expected.chars().count());
        if total_chars > MAX_TOTAL_TEXT_CHARS {
            return Err("private probe pack contains too much task text".to_owned());
        }
        if task.max_output_tokens == 0 || task.max_output_tokens > MAX_PRIVATE_OUTPUT_TOKENS {
            return Err(format!(
                "private probe maxOutputTokens must be between 1 and {MAX_PRIVATE_OUTPUT_TOKENS}"
            ));
        }
        if task.scorer == PrivateProbeScorer::ExactJson {
            let expected = serde_json::from_str::<serde_json::Value>(&task.expected)
                .map_err(|_| "exactJson expected value must be valid JSON".to_owned())?;
            if json_depth(&expected, 0) > MAX_EXACT_JSON_DEPTH {
                return Err("exactJson expected value is nested too deeply".to_owned());
            }
        } else if task.expected.trim() != task.expected {
            return Err(
                "exactText expected value must not have leading or trailing whitespace".to_owned(),
            );
        }
    }
    Ok(())
}

fn is_safe_identifier(value: &str, max_chars: usize) -> bool {
    let count = value.chars().count();
    count > 0
        && count <= max_chars
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_text(value: &str, min: usize, max: usize, field: &str) -> Result<(), String> {
    let count = value.chars().count();
    if count < min || count > max {
        return Err(format!(
            "private probe {field} length is outside the allowed range"
        ));
    }
    if value.chars().any(|character| {
        character == '\0'
            || matches!(
                character,
                '\u{202a}'
                    | '\u{202b}'
                    | '\u{202c}'
                    | '\u{202d}'
                    | '\u{202e}'
                    | '\u{2066}'
                    | '\u{2067}'
                    | '\u{2068}'
                    | '\u{2069}'
            )
    }) {
        return Err(format!(
            "private probe {field} contains forbidden control characters"
        ));
    }
    Ok(())
}

fn json_depth(value: &serde_json::Value, depth: usize) -> usize {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| json_depth(value, depth + 1))
            .max()
            .unwrap_or(depth + 1),
        serde_json::Value::Object(values) => values
            .values()
            .map(|value| json_depth(value, depth + 1))
            .max()
            .unwrap_or(depth + 1),
        _ => depth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_pack(contents: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("xiaoli-private-probe-{suffix}"));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("pack.json");
        fs::write(&path, contents).unwrap();
        path
    }

    fn valid_pack(prompt: &str) -> String {
        serde_json::json!({
            "schemaVersion": 1,
            "version": "local-v1",
            "tasks": [{
                "id": "nonce-a",
                "batch": "a",
                "domain": "constraintReasoning",
                "scorer": "exactText",
                "prompt": prompt,
                "expected": "42",
                "maxOutputTokens": 8
            }]
        })
        .to_string()
    }

    #[test]
    fn resolves_reference_without_serializing_body() {
        let body = valid_pack("Return only 42. PRIVATE-PROMPT-MARKER");
        let path = temp_pack(&body);
        let loaded = resolve_private_probe_pack(path.to_str().unwrap()).unwrap();
        assert_eq!(loaded.reference.version, "local-v1");
        assert_eq!(loaded.reference.sha256.len(), 64);
        assert_eq!(loaded.tasks.len(), 1);
        let serialized = serde_json::to_string(&loaded.reference).unwrap();
        assert!(!serialized.contains("PRIVATE-PROMPT-MARKER"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rejects_missing_and_changed_files() {
        let path = temp_pack(&valid_pack("Return only 42."));
        let reference = resolve_private_probe_pack(path.to_str().unwrap())
            .unwrap()
            .reference;
        fs::write(&path, valid_pack("Return only 43.")).unwrap();
        assert!(load_verified_private_probe_pack(&reference)
            .unwrap_err()
            .contains("hash changed"));
        fs::remove_file(&path).unwrap();
        assert!(load_verified_private_probe_pack(&reference).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rejects_unknown_schema_fields_and_unsafe_domains() {
        let unknown = valid_pack("Return only 42.").replace(
            "\"maxOutputTokens\":8",
            "\"maxOutputTokens\":8,\"judge\":\"llm\"",
        );
        let path = temp_pack(&unknown);
        assert!(resolve_private_probe_pack(path.to_str().unwrap()).is_err());
        fs::write(
            &path,
            valid_pack("Return only 42.").replace("constraintReasoning", "toolSelection"),
        )
        .unwrap();
        assert!(resolve_private_probe_pack(path.to_str().unwrap()).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn enforces_task_and_file_limits() {
        let tasks = (0..=MAX_PRIVATE_PROBE_TASKS)
            .map(|index| {
                serde_json::json!({
                    "id": format!("task-{index}"),
                    "batch": "a",
                    "domain": "multilingual",
                    "scorer": "exactText",
                    "prompt": "只返回好",
                    "expected": "好",
                    "maxOutputTokens": 4
                })
            })
            .collect::<Vec<_>>();
        let path = temp_pack(
            &serde_json::json!({"schemaVersion":1,"version":"v1","tasks":tasks}).to_string(),
        );
        assert!(resolve_private_probe_pack(path.to_str().unwrap()).is_err());
        fs::write(&path, vec![b' '; MAX_PRIVATE_PROBE_PACK_BYTES as usize + 1]).unwrap();
        assert!(resolve_private_probe_pack(path.to_str().unwrap()).is_err());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
