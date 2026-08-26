use crate::relay_audit::{
    compare_cell_fingerprints, is_strict_model_id, normalize_audit_effort,
    normalize_probe_response, CellFingerprint, IdentityAssessment, IdentityAssessmentKind,
    NormalizedProbeResponse, ProbeCellKey, ProbeFamily, ProbeLanguage, RelayProtocol,
    MIN_VALID_SAMPLES_PER_CELL,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, LocalResult, NaiveDateTime, TimeZone,
    Utc,
};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SIGNED_RELAY_BASELINE_SCHEMA_VERSION: u32 = 1;
pub const SIGNED_RELAY_BASELINE_ALGORITHM: &str = "ed25519";
pub const SIGNED_RELAY_BASELINE_DOMAIN: &[u8] = b"XiaoLi relay baseline v1\0";
pub const FINGERPRINT_GENERATOR_VERSION: &str = "xiaoli-fingerprint-v1";
pub const FINGERPRINT_NORMALIZATION_VERSION: &str = "xiaoli-one-word-v1";
/// Scorer-compatible signed baselines may be dated at most this far ahead of
/// the local clock. This tolerates ordinary publisher/host clock skew without
/// allowing a far-future `createdAt` to defeat replacement ordering.
pub const MAX_SIGNED_BASELINE_FUTURE_SKEW_HOURS: i64 = 24;
/// Static fingerprint distributions drift as providers update models and
/// serving policy. A scorer-compatible package must therefore expire within
/// this many days of its signed `createdAt`.
pub const MAX_SIGNED_BASELINE_VALIDITY_DAYS: i64 = 180;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayBaselineSummary {
    pub id: String,
    pub label: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub protocol: RelayProtocol,
    /// `official`, `community`, or `user`. These namespaces are stored in
    /// separate rows and relay observations can never overwrite them.
    pub source: String,
    pub version: String,
    pub sample_count: usize,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub signed: bool,
    /// True only after verification against a key that was already present in
    /// XiaoLi's local trust store. A key embedded in the package is never a
    /// trust anchor.
    #[serde(default)]
    pub signature_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<String>,
    #[serde(default)]
    pub usable_for_scoring: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoring_mode: Option<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

/// A public key explicitly trusted by the local user. Trust anchors are kept
/// separately from imported baseline packages so a package cannot bless its
/// own arbitrary public key.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayBaselineTrustAnchor {
    pub key_id: String,
    pub label: String,
    pub public_key_base64: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayBaselineFingerprintParameters {
    pub generator_version: String,
    pub normalization_version: String,
    /// Integer milli-units keep the signed representation deterministic.
    /// The current fingerprint scorer only accepts 1000 (= temperature 1.0).
    pub temperature_milli: u16,
    pub max_output_tokens: u32,
    /// Signed calibration bounds in millionths of base-2 JSD.
    pub same_model_max_mean_jsd_micros: u32,
    pub different_model_min_mean_jsd_micros: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayBaselineFingerprintCell {
    pub family: ProbeFamily,
    pub language: ProbeLanguage,
    /// Already-normalized one-word outputs and their observed counts.
    pub counts: BTreeMap<String, u32>,
}

/// Canonical, signed scorer material. Every applicability field is inside the
/// payload covered by the signature; no envelope metadata can change how the
/// baseline is selected or interpreted.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayBaselinePayloadV1 {
    pub id: String,
    pub label: String,
    pub source: String,
    pub version: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub protocol: RelayProtocol,
    pub sample_count: usize,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub parameters: RelayBaselineFingerprintParameters,
    pub fingerprint_cells: Vec<RelayBaselineFingerprintCell>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedRelayBaselinePackageV1 {
    pub schema_version: u32,
    pub algorithm: String,
    pub key_id: String,
    pub payload: RelayBaselinePayloadV1,
    pub signature_base64: String,
}

/// Persisted only after trust-anchor lookup, payload validation, and signature
/// verification all succeed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedRelayBaselinePackage {
    pub signing_key_id: String,
    pub verified_at: String,
    pub signature_base64: String,
    pub payload: RelayBaselinePayloadV1,
}

impl RelayBaselineTrustAnchor {
    pub fn validate(&self) -> Result<VerifyingKey, String> {
        validate_id(&self.key_id, "keyId")?;
        validate_short_text(&self.label, 100, "label")?;
        DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|_| "trust anchor createdAt must be RFC3339".to_owned())?;
        let decoded = BASE64_STANDARD
            .decode(self.public_key_base64.trim())
            .map_err(|_| "trust anchor public key is not valid base64".to_owned())?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| "Ed25519 public keys must contain exactly 32 bytes".to_owned())?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| "trust anchor public key is not a valid Ed25519 key".to_owned())?;
        if key.is_weak() {
            return Err("trust anchor public key is a weak Ed25519 key".to_owned());
        }
        Ok(key)
    }
}

impl RelayBaselinePayloadV1 {
    /// Validates the bounded, canonical v1 payload shape without claiming the
    /// current scorer understands its generator or calibration parameters.
    /// This separation lets XiaoLi authenticate a future/unsupported package
    /// and show it as verified metadata while still keeping it out of scoring.
    pub fn validate_signed_structure(&self) -> Result<(), String> {
        validate_id(&self.id, "payload.id")?;
        validate_short_text(&self.label, 100, "payload.label")?;
        validate_short_text(&self.version, 60, "payload.version")?;
        if self.source != "community" {
            return Err(
                "signed imported baselines must use the community namespace; official evidence is live-only"
                    .to_owned(),
            );
        }
        if !is_strict_model_id(&self.model) {
            return Err("payload.model is not a strict model identifier".to_owned());
        }
        if normalize_audit_effort(self.effort.as_deref())? != self.effort {
            return Err("payload.effort must already be normalized".to_owned());
        }
        if self.protocol == RelayProtocol::AnthropicMessages && self.effort.is_some() {
            return Err(
                "Anthropic Messages baselines cannot claim an OpenAI reasoning effort".to_owned(),
            );
        }
        let created_at = DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|_| "payload.createdAt must be RFC3339".to_owned())?;
        if let Some(expires_at) = self.expires_at.as_deref() {
            let expires_at = DateTime::parse_from_rfc3339(expires_at)
                .map_err(|_| "payload.expiresAt must be RFC3339".to_owned())?;
            if expires_at <= created_at {
                return Err("payload.expiresAt must be later than createdAt".to_owned());
            }
        }
        validate_short_text(
            &self.parameters.generator_version,
            80,
            "payload.parameters.generatorVersion",
        )?;
        validate_short_text(
            &self.parameters.normalization_version,
            80,
            "payload.parameters.normalizationVersion",
        )?;
        if !(4..=40).contains(&self.fingerprint_cells.len()) {
            return Err("baseline must contain between 4 and 40 fingerprint cells".to_owned());
        }
        let mut seen = BTreeSet::new();
        let mut total = 0usize;
        for cell in &self.fingerprint_cells {
            if !seen.insert((cell.family, cell.language)) {
                return Err("baseline contains a duplicate fingerprint cell".to_owned());
            }
            let cell_total = cell.counts.values().try_fold(0usize, |acc, count| {
                let count = usize::try_from(*count)
                    .map_err(|_| "baseline sample count is too large".to_owned())?;
                acc.checked_add(count)
                    .ok_or_else(|| "baseline sample count overflow".to_owned())
            })?;
            if !(MIN_VALID_SAMPLES_PER_CELL..=1_000).contains(&cell_total) {
                return Err(format!(
                    "each fingerprint cell must contain {MIN_VALID_SAMPLES_PER_CELL}..=1000 samples"
                ));
            }
            for output in cell.counts.keys() {
                if output.is_empty() || output.chars().count() > 128 {
                    return Err("baseline output labels must contain 1..=128 characters".to_owned());
                }
                match normalize_probe_response(cell.family, cell.language, output) {
                    NormalizedProbeResponse::Valid(normalized) if normalized == *output => {}
                    _ => return Err(
                        "baseline output labels must already use XiaoLi's canonical normalization"
                            .to_owned(),
                    ),
                }
            }
            total = total
                .checked_add(cell_total)
                .ok_or_else(|| "baseline sample count overflow".to_owned())?;
        }
        if total != self.sample_count || !(40..=40_000).contains(&total) {
            return Err("payload.sampleCount must exactly equal the signed cell counts".to_owned());
        }
        if self.limitations.len() > 32
            || self
                .limitations
                .iter()
                .any(|item| item.is_empty() || item.chars().count() > 240)
        {
            return Err("payload.limitations exceeds the supported bounds".to_owned());
        }
        Ok(())
    }

    /// Requires exact compatibility with the scorer shipped in this XiaoLi
    /// build. Callers must pass this gate before using any signed distribution
    /// in an evidence calculation.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_signed_structure()?;
        let created_at = DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|_| "payload.createdAt must be RFC3339".to_owned())?
            .with_timezone(&Utc);
        let expires_at = self
            .expires_at
            .as_deref()
            .ok_or_else(|| "scorer-compatible signed baselines must include expiresAt".to_owned())
            .and_then(|value| {
                DateTime::parse_from_rfc3339(value)
                    .map(|value| value.with_timezone(&Utc))
                    .map_err(|_| "payload.expiresAt must be RFC3339".to_owned())
            })?;
        if created_at > Utc::now() + ChronoDuration::hours(MAX_SIGNED_BASELINE_FUTURE_SKEW_HOURS) {
            return Err("payload.createdAt is too far in the future for scoring".to_owned());
        }
        if expires_at - created_at > ChronoDuration::days(MAX_SIGNED_BASELINE_VALIDITY_DAYS) {
            return Err(format!(
                "scorer-compatible signed baselines may be valid for at most {MAX_SIGNED_BASELINE_VALIDITY_DAYS} days"
            ));
        }
        let parameters = &self.parameters;
        if parameters.generator_version != FINGERPRINT_GENERATOR_VERSION
            || parameters.normalization_version != FINGERPRINT_NORMALIZATION_VERSION
            || parameters.temperature_milli != 1_000
            || parameters.max_output_tokens != 16
        {
            return Err(
                "baseline fingerprint parameters are not supported by this XiaoLi release"
                    .to_owned(),
            );
        }
        let same = parameters.same_model_max_mean_jsd_micros;
        let different = parameters.different_model_min_mean_jsd_micros;
        if !(10_000..=250_000).contains(&same)
            || !(100_000..=750_000).contains(&different)
            || same.saturating_add(50_000) > different
        {
            return Err("baseline JSD calibration bounds are invalid".to_owned());
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|value| value.with_timezone(&Utc) <= now)
    }

    pub fn fingerprint_samples(&self) -> BTreeMap<ProbeCellKey, Vec<String>> {
        self.fingerprint_cells
            .iter()
            .map(|cell| {
                let samples = cell
                    .counts
                    .iter()
                    .flat_map(|(value, count)| std::iter::repeat_n(value.clone(), *count as usize))
                    .collect();
                (
                    ProbeCellKey {
                        family: cell.family,
                        language: cell.language,
                    },
                    samples,
                )
            })
            .collect()
    }
}

/// Scores only the normalized fingerprint material covered by the verified
/// signature. This is a low-confidence static comparison: callers must not
/// use a consistent result by itself to set the overall audit verdict to
/// `consistent` or to identify a physical serving model.
pub fn score_trusted_relay_baseline(
    observed: &BTreeMap<ProbeCellKey, Vec<String>>,
    package: &TrustedRelayBaselinePackage,
    protocol: RelayProtocol,
    requested_model: &str,
    requested_effort: Option<&str>,
    now: DateTime<Utc>,
) -> IdentityAssessment {
    let mut limitations = vec![
        "the reference is a locally trusted signed static package, not a live matched official endpoint"
            .to_owned(),
        "signature verification authenticates the package publisher, not the physical model serving this request"
            .to_owned(),
        "a static reference can drift after provider model, region, or sampling changes".to_owned(),
        "this comparison alone cannot set overallVerdict to consistent or name an actual model"
            .to_owned(),
    ];
    if package.payload.validate().is_err()
        || package.payload.protocol != protocol
        || package.payload.model != requested_model
        || package.payload.effort.as_deref() != requested_effort
    {
        limitations.push(
            "the signed baseline parameters do not exactly match this protocol, requested model, and effort"
                .to_owned(),
        );
        return IdentityAssessment {
            state: IdentityAssessmentKind::Unproven,
            eligible_cells: 0,
            mean_js_divergence: None,
            compared_reference: None,
            string_kernel_mmd: None,
            reasons: vec!["signed baseline is not applicable to this audit".to_owned()],
            limitations,
        };
    }
    if package.payload.is_expired_at(now) {
        limitations.push("the signed baseline has expired".to_owned());
        return IdentityAssessment {
            state: IdentityAssessmentKind::Unproven,
            eligible_cells: 0,
            mean_js_divergence: None,
            compared_reference: Some(package.payload.id.clone()),
            string_kernel_mmd: None,
            reasons: vec!["signed baseline is expired and was not scored".to_owned()],
            limitations,
        };
    }
    let reference = package.payload.fingerprint_samples();
    let mut divergences = Vec::new();
    for (cell, reference_values) in reference {
        let Some(observed_values) = observed.get(&cell) else {
            continue;
        };
        let observed_fingerprint =
            CellFingerprint::from_responses(cell, observed_values.iter().map(String::as_str));
        let reference_fingerprint =
            CellFingerprint::from_responses(cell, reference_values.iter().map(String::as_str));
        if let Some(comparison) =
            compare_cell_fingerprints(&observed_fingerprint, &reference_fingerprint)
        {
            divergences.push(comparison.js_divergence);
        }
    }
    let eligible_cells = divergences.len();
    let mean =
        (eligible_cells >= 4).then(|| divergences.iter().sum::<f64>() / eligible_cells as f64);
    let same_threshold =
        f64::from(package.payload.parameters.same_model_max_mean_jsd_micros) / 1_000_000.0;
    let different_threshold = f64::from(
        package
            .payload
            .parameters
            .different_model_min_mean_jsd_micros,
    ) / 1_000_000.0;
    let (state, reason) = match mean {
        Some(value) if value <= same_threshold => (
            IdentityAssessmentKind::ReferenceConsistent,
            format!(
                "{eligible_cells} eligible cells were within the signed static same-model calibration"
            ),
        ),
        Some(value) if value >= different_threshold => (
            IdentityAssessmentKind::ReferenceDifferent,
            format!(
                "{eligible_cells} eligible cells exceeded the signed static different-model calibration"
            ),
        ),
        Some(_) => (
            IdentityAssessmentKind::Unproven,
            "the signed static comparison fell between calibrated decision bounds".to_owned(),
        ),
        None => (
            IdentityAssessmentKind::Unproven,
            "fewer than four matching eligible cells were available for the signed baseline"
                .to_owned(),
        ),
    };
    IdentityAssessment {
        state,
        eligible_cells,
        mean_js_divergence: mean,
        compared_reference: Some(package.payload.id.clone()),
        string_kernel_mmd: None,
        reasons: vec![reason],
        limitations,
    }
}

impl TrustedRelayBaselinePackage {
    pub fn summary(&self) -> RelayBaselineSummary {
        let mut limitations = self.payload.limitations.clone();
        limitations.push("签名仅证明该包由本机已信任密钥发布，不证明服务器物理模型身份".to_owned());
        limitations.push("静态基线可能因模型更新、区域、时间和提供商参数漂移而过期".to_owned());
        RelayBaselineSummary {
            id: self.payload.id.clone(),
            label: self.payload.label.clone(),
            model: self.payload.model.clone(),
            effort: self.payload.effort.clone(),
            protocol: self.payload.protocol,
            source: self.payload.source.clone(),
            version: self.payload.version.clone(),
            sample_count: self.payload.sample_count,
            created_at: self.payload.created_at.clone(),
            expires_at: self.payload.expires_at.clone(),
            signed: true,
            signature_verified: true,
            signing_key_id: Some(self.signing_key_id.clone()),
            usable_for_scoring: self.payload.validate().is_ok()
                && !self.payload.is_expired_at(Utc::now()),
            scoring_mode: Some("trustedSignedFingerprint".to_owned()),
            limitations,
        }
    }
}

/// Verifies a signed package against an independently supplied trust anchor.
/// The signed bytes are a domain separator followed by serde's deterministic
/// representation of the fully typed payload. All maps are BTreeMaps and the
/// structs reject unknown fields, avoiding key-order and ignored-field
/// ambiguities.
pub fn verify_signed_relay_baseline(
    package: &SignedRelayBaselinePackageV1,
    anchor: &RelayBaselineTrustAnchor,
    verified_at: String,
) -> Result<TrustedRelayBaselinePackage, String> {
    if package.schema_version != SIGNED_RELAY_BASELINE_SCHEMA_VERSION {
        return Err("unsupported signed baseline schemaVersion".to_owned());
    }
    if package.algorithm != SIGNED_RELAY_BASELINE_ALGORITHM {
        return Err("signed baseline algorithm must be ed25519".to_owned());
    }
    validate_id(&package.key_id, "keyId")?;
    if package.key_id != anchor.key_id {
        return Err("signed package keyId does not match the selected trust anchor".to_owned());
    }
    package.payload.validate_signed_structure()?;
    let key = anchor.validate()?;
    let decoded = BASE64_STANDARD
        .decode(package.signature_base64.trim())
        .map_err(|_| "baseline signature is not valid base64".to_owned())?;
    let signature = Signature::from_slice(&decoded)
        .map_err(|_| "Ed25519 signatures must contain exactly 64 bytes".to_owned())?;
    let signed_bytes = canonical_signed_payload_bytes(&package.payload)?;
    key.verify_strict(&signed_bytes, &signature)
        .map_err(|_| "baseline signature verification failed".to_owned())?;
    DateTime::parse_from_rfc3339(&verified_at)
        .map_err(|_| "verifiedAt must be RFC3339".to_owned())?;
    Ok(TrustedRelayBaselinePackage {
        signing_key_id: package.key_id.clone(),
        verified_at,
        signature_base64: package.signature_base64.clone(),
        payload: package.payload.clone(),
    })
}

impl TrustedRelayBaselinePackage {
    pub fn reverify(&self, anchor: &RelayBaselineTrustAnchor) -> Result<(), String> {
        verify_signed_relay_baseline(
            &SignedRelayBaselinePackageV1 {
                schema_version: SIGNED_RELAY_BASELINE_SCHEMA_VERSION,
                algorithm: SIGNED_RELAY_BASELINE_ALGORITHM.to_owned(),
                key_id: self.signing_key_id.clone(),
                payload: self.payload.clone(),
                signature_base64: self.signature_base64.clone(),
            },
            anchor,
            self.verified_at.clone(),
        )
        .map(|_| ())
    }
}

pub fn canonical_signed_payload_bytes(payload: &RelayBaselinePayloadV1) -> Result<Vec<u8>, String> {
    payload.validate_signed_structure()?;
    let encoded = serde_json::to_vec(payload).map_err(|error| error.to_string())?;
    let mut output = Vec::with_capacity(SIGNED_RELAY_BASELINE_DOMAIN.len() + encoded.len());
    output.extend_from_slice(SIGNED_RELAY_BASELINE_DOMAIN);
    output.extend_from_slice(&encoded);
    Ok(output)
}

fn validate_short_text(value: &str, max: usize, field: &str) -> Result<(), String> {
    if value.trim() != value || value.is_empty() || value.chars().count() > max {
        return Err(format!("{field} is invalid"));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditSchedule {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_baseline_profile_id: Option<String>,
    pub cadence: String,
    #[serde(default = "default_weekday")]
    pub weekday: u8,
    pub local_time: String,
    pub pair_official: bool,
    pub monthly_request_limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_retention_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_month: Option<String>,
    #[serde(default)]
    pub monthly_reserved_requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_audit_id: Option<String>,
}

const fn default_weekday() -> u8 {
    1
}

impl Default for AuditSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            profile_id: None,
            official_baseline_profile_id: None,
            cadence: "weekly".to_owned(),
            weekday: default_weekday(),
            local_time: "20:00".to_owned(),
            pair_official: false,
            monthly_request_limit: 1_000,
            history_retention_days: Some(180),
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            budget_month: None,
            monthly_reserved_requests: 0,
            active_audit_id: None,
        }
    }
}

impl AuditSchedule {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.cadence.as_str(), "daily" | "weekly") {
            return Err("cadence must be daily or weekly".to_owned());
        }
        if self.weekday > 6 {
            return Err("weekday must be between 0 and 6".to_owned());
        }
        let Some((hour, minute)) = self.local_time.split_once(':') else {
            return Err("localTime must be HH:MM".to_owned());
        };
        let hour = hour.parse::<u8>().map_err(|_| "invalid localTime")?;
        let minute = minute.parse::<u8>().map_err(|_| "invalid localTime")?;
        if hour > 23 || minute > 59 {
            return Err("localTime must be HH:MM".to_owned());
        }
        let minimum_budget = if self.pair_official { 300 } else { 150 };
        if self.monthly_request_limit < minimum_budget || self.monthly_request_limit > 100_000 {
            return Err("monthlyRequestLimit is outside the supported range".to_owned());
        }
        if self.enabled {
            let Some(profile_id) = self.profile_id.as_deref() else {
                return Err("enabled schedules require profileId".to_owned());
            };
            validate_id(profile_id, "profileId")?;
            if self.pair_official {
                let Some(official_id) = self.official_baseline_profile_id.as_deref() else {
                    return Err(
                        "paired scheduled audits require officialBaselineProfileId".to_owned()
                    );
                };
                validate_id(official_id, "officialBaselineProfileId")?;
                if official_id == profile_id {
                    return Err("paired endpoints must be different profiles".to_owned());
                }
            }
        }
        if self
            .history_retention_days
            .is_some_and(|days| !(1..=36_500).contains(&days))
        {
            return Err("historyRetentionDays is outside the supported range".to_owned());
        }
        Ok(())
    }

    pub const fn request_reservation(&self) -> u32 {
        if self.pair_official {
            300
        } else {
            150
        }
    }

    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        self.enabled
            && self
                .next_run_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|value| value.with_timezone(&Utc) <= now)
    }
}

pub fn current_budget_month(now: DateTime<Utc>) -> String {
    now.with_timezone(&Local).format("%Y-%m").to_string()
}

/// Computes one randomized local due time. The random offset is generated by
/// the operating system and deliberately persisted by the caller so process
/// restarts do not continuously reroll the schedule.
pub fn next_scheduled_run(schedule: &AuditSchedule, now: DateTime<Utc>) -> Result<String, String> {
    schedule.validate()?;
    let jitter = random_jitter_minutes()?;
    let local_now = now.with_timezone(&Local);
    let candidate = next_local_naive(schedule, local_now.naive_local(), jitter)?;
    let local_candidate = resolve_local_datetime(candidate)?;
    Ok(local_candidate
        .with_timezone(&Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn random_jitter_minutes() -> Result<i64, String> {
    let mut bytes = [0_u8; 2];
    getrandom::fill(&mut bytes)
        .map_err(|_| "operating-system random source is unavailable".to_owned())?;
    Ok(i64::from(u16::from_le_bytes(bytes) % 61) - 30)
}

fn next_local_naive(
    schedule: &AuditSchedule,
    now: NaiveDateTime,
    jitter_minutes: i64,
) -> Result<NaiveDateTime, String> {
    let (hour, minute) = parse_local_time(&schedule.local_time)?;
    let mut days_ahead = if schedule.cadence == "daily" {
        0_i64
    } else {
        let current = i64::from(now.weekday().num_days_from_sunday());
        (i64::from(schedule.weekday) - current).rem_euclid(7)
    };
    let mut base = now
        .date()
        .and_hms_opt(u32::from(hour), u32::from(minute), 0)
        .ok_or_else(|| "localTime could not be represented".to_owned())?
        + ChronoDuration::days(days_ahead);
    if base <= now {
        days_ahead = if schedule.cadence == "daily" { 1 } else { 7 };
        base += ChronoDuration::days(days_ahead);
    }
    let mut candidate = base + ChronoDuration::minutes(jitter_minutes.clamp(-30, 30));
    if candidate <= now {
        candidate += ChronoDuration::days(if schedule.cadence == "daily" { 1 } else { 7 });
    }
    Ok(candidate)
}

fn parse_local_time(value: &str) -> Result<(u8, u8), String> {
    let Some((hour, minute)) = value.split_once(':') else {
        return Err("localTime must be HH:MM".to_owned());
    };
    let hour = hour.parse::<u8>().map_err(|_| "invalid localTime")?;
    let minute = minute.parse::<u8>().map_err(|_| "invalid localTime")?;
    if hour > 23 || minute > 59 {
        return Err("localTime must be HH:MM".to_owned());
    }
    Ok((hour, minute))
}

fn resolve_local_datetime(mut candidate: NaiveDateTime) -> Result<DateTime<Local>, String> {
    for _ in 0..=3 {
        match Local.from_local_datetime(&candidate) {
            LocalResult::Single(value) => return Ok(value),
            LocalResult::Ambiguous(earlier, _) => return Ok(earlier),
            LocalResult::None => candidate += ChronoDuration::hours(1),
        }
    }
    Err("scheduled local time is unavailable around a clock transition".to_owned())
}

fn validate_id(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("{field} is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    fn signed_fixture() -> (
        SignedRelayBaselinePackageV1,
        RelayBaselineTrustAnchor,
        SigningKey,
    ) {
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let anchor = RelayBaselineTrustAnchor {
            key_id: "local-test-key".to_owned(),
            label: "Local test release key".to_owned(),
            public_key_base64: BASE64_STANDARD.encode(signing_key.verifying_key().to_bytes()),
            created_at: "2026-08-27T00:00:00Z".to_owned(),
        };
        let payload = RelayBaselinePayloadV1 {
            id: "trusted-test-gpt56".to_owned(),
            label: "Trusted test baseline".to_owned(),
            source: "community".to_owned(),
            version: "2026.08.27".to_owned(),
            model: "gpt-5.6-sol".to_owned(),
            effort: None,
            protocol: RelayProtocol::OpenAiResponses,
            sample_count: 40,
            created_at: "2026-08-27T00:00:00Z".to_owned(),
            expires_at: Some("2026-11-27T00:00:00Z".to_owned()),
            parameters: RelayBaselineFingerprintParameters {
                generator_version: FINGERPRINT_GENERATOR_VERSION.to_owned(),
                normalization_version: FINGERPRINT_NORMALIZATION_VERSION.to_owned(),
                temperature_milli: 1_000,
                max_output_tokens: 16,
                same_model_max_mean_jsd_micros: 120_000,
                different_model_min_mean_jsd_micros: 300_000,
            },
            fingerprint_cells: [
                (ProbeFamily::Number, "7"),
                (ProbeFamily::Letter, "a"),
                (ProbeFamily::Color, "blue"),
                (ProbeFamily::Animal, "cat"),
            ]
            .into_iter()
            .map(|(family, output)| RelayBaselineFingerprintCell {
                family,
                language: ProbeLanguage::English,
                counts: BTreeMap::from([(output.to_owned(), 10)]),
            })
            .collect(),
            limitations: vec!["fixture only".to_owned()],
        };
        let signature = signing_key.sign(&canonical_signed_payload_bytes(&payload).unwrap());
        (
            SignedRelayBaselinePackageV1 {
                schema_version: SIGNED_RELAY_BASELINE_SCHEMA_VERSION,
                algorithm: SIGNED_RELAY_BASELINE_ALGORITHM.to_owned(),
                key_id: anchor.key_id.clone(),
                payload,
                signature_base64: BASE64_STANDARD.encode(signature.to_bytes()),
            },
            anchor,
            signing_key,
        )
    }

    #[test]
    fn signed_baseline_requires_an_independent_matching_trust_anchor() {
        let (package, anchor, _) = signed_fixture();
        let trusted =
            verify_signed_relay_baseline(&package, &anchor, "2026-08-27T01:00:00Z".to_owned())
                .unwrap();
        assert!(trusted.summary().signature_verified);
        assert!(trusted.summary().usable_for_scoring);
        assert_eq!(trusted.payload.fingerprint_samples().len(), 4);

        let attacker_key = SigningKey::from_bytes(&[9_u8; 32]);
        let attacker_anchor = RelayBaselineTrustAnchor {
            public_key_base64: BASE64_STANDARD.encode(attacker_key.verifying_key().to_bytes()),
            ..anchor.clone()
        };
        assert!(verify_signed_relay_baseline(
            &package,
            &attacker_anchor,
            "2026-08-27T01:00:00Z".to_owned(),
        )
        .is_err());
    }

    #[test]
    fn canonical_v1_golden_vector_is_stable() {
        let (package, anchor, _) = signed_fixture();
        let bytes = canonical_signed_payload_bytes(&package.payload).unwrap();
        assert_eq!(bytes.len(), 780);
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "b5949957a3edbc06ec6ff93a84ec99f69d7a2791c01a35e3b54c41b38ff7ebfa"
        );
        assert_eq!(
            anchor.public_key_base64,
            "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw="
        );
        assert_eq!(
            package.signature_base64,
            "e0WnJ+y+pSbgcHb15aYsZBWbwqI3BuTXI3vb8x3wLIOsStAT5KPYCtsMRI7OHGOphf3gLPGMvcFHJ3dl/LT9DQ=="
        );
    }

    #[test]
    fn scorer_compatible_package_requires_bounded_current_validity() {
        let (package, _, _) = signed_fixture();

        let mut missing_expiry = package.payload.clone();
        missing_expiry.expires_at = None;
        assert!(missing_expiry
            .validate()
            .unwrap_err()
            .contains("must include expiresAt"));

        let mut future = package.payload.clone();
        let future_created =
            Utc::now() + ChronoDuration::hours(MAX_SIGNED_BASELINE_FUTURE_SKEW_HOURS + 1);
        future.created_at = future_created.to_rfc3339();
        future.expires_at = Some((future_created + ChronoDuration::days(1)).to_rfc3339());
        assert!(future
            .validate()
            .unwrap_err()
            .contains("too far in the future"));

        let mut overlong = package.payload;
        let created = Utc::now() - ChronoDuration::hours(1);
        overlong.created_at = created.to_rfc3339();
        overlong.expires_at = Some(
            (created + ChronoDuration::days(MAX_SIGNED_BASELINE_VALIDITY_DAYS + 1)).to_rfc3339(),
        );
        assert!(overlong
            .validate()
            .unwrap_err()
            .contains("valid for at most"));
    }

    #[test]
    fn weak_ed25519_trust_anchor_is_rejected_before_signature_verification() {
        // curve25519-dalek's EIGHT_TORSION[4]. ed25519-dalek documents that
        // ordinary `verify` accepts signatures under small-order keys that can
        // be valid for multiple messages; publisher identity therefore needs
        // both this import-time check and `verify_strict` above.
        let weak_key = [
            236, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 127,
        ];
        let anchor = RelayBaselineTrustAnchor {
            key_id: "weak-test-key".to_owned(),
            label: "Weak test key".to_owned(),
            public_key_base64: BASE64_STANDARD.encode(weak_key),
            created_at: "2026-08-27T00:00:00Z".to_owned(),
        };

        let error = anchor.validate().expect_err("weak key must not be trusted");
        assert!(error.contains("weak Ed25519 key"));
    }

    #[test]
    fn signature_covers_scoring_parameters_and_distribution() {
        let (package, anchor, _) = signed_fixture();
        let mut changed_parameters = package.clone();
        changed_parameters
            .payload
            .parameters
            .same_model_max_mean_jsd_micros = 1;
        assert!(verify_signed_relay_baseline(
            &changed_parameters,
            &anchor,
            "2026-08-27T01:00:00Z".to_owned(),
        )
        .is_err());

        let mut changed_distribution = package;
        changed_distribution.payload.fingerprint_cells[0]
            .counts
            .insert("8".to_owned(), 1);
        changed_distribution.payload.sample_count += 1;
        assert!(verify_signed_relay_baseline(
            &changed_distribution,
            &anchor,
            "2026-08-27T01:00:00Z".to_owned(),
        )
        .is_err());
    }

    #[test]
    fn unsupported_but_validly_signed_parameters_are_metadata_only() {
        let (mut package, anchor, signing_key) = signed_fixture();
        package.payload.parameters.generator_version = "xiaoli-fingerprint-v2".to_owned();
        let signature =
            signing_key.sign(&canonical_signed_payload_bytes(&package.payload).unwrap());
        package.signature_base64 = BASE64_STANDARD.encode(signature.to_bytes());

        let trusted =
            verify_signed_relay_baseline(&package, &anchor, "2026-08-27T01:00:00Z".to_owned())
                .expect("publisher signature is still independently verifiable");
        let summary = trusted.summary();
        assert!(summary.signature_verified);
        assert!(!summary.usable_for_scoring);
        assert!(trusted.payload.validate().is_err());
    }

    #[test]
    fn package_cannot_embed_a_self_approving_public_key() {
        let (package, anchor, _) = signed_fixture();
        let mut json = serde_json::to_value(package).unwrap();
        json.as_object_mut().unwrap().insert(
            "publicKeyBase64".to_owned(),
            Value::String(anchor.public_key_base64),
        );
        assert!(serde_json::from_value::<SignedRelayBaselinePackageV1>(json).is_err());
    }

    #[test]
    fn trusted_static_scorer_is_parameter_bound_and_expiry_aware() {
        let (package, anchor, _) = signed_fixture();
        let trusted =
            verify_signed_relay_baseline(&package, &anchor, "2026-08-27T01:00:00Z".to_owned())
                .unwrap();
        let observed = trusted.payload.fingerprint_samples();
        let consistent = score_trusted_relay_baseline(
            &observed,
            &trusted,
            RelayProtocol::OpenAiResponses,
            "gpt-5.6-sol",
            None,
            DateTime::parse_from_rfc3339("2026-08-28T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert_eq!(
            consistent.state,
            IdentityAssessmentKind::ReferenceConsistent
        );
        assert_eq!(
            consistent.compared_reference.as_deref(),
            Some("trusted-test-gpt56")
        );

        let wrong_protocol = score_trusted_relay_baseline(
            &observed,
            &trusted,
            RelayProtocol::AnthropicMessages,
            "gpt-5.6-sol",
            None,
            Utc::now(),
        );
        assert_eq!(wrong_protocol.state, IdentityAssessmentKind::Unproven);

        let wrong_effort = score_trusted_relay_baseline(
            &observed,
            &trusted,
            RelayProtocol::OpenAiResponses,
            "gpt-5.6-sol",
            Some("high"),
            Utc::now(),
        );
        assert_eq!(wrong_effort.state, IdentityAssessmentKind::Unproven);

        let expired = score_trusted_relay_baseline(
            &observed,
            &trusted,
            RelayProtocol::OpenAiResponses,
            "gpt-5.6-sol",
            None,
            DateTime::parse_from_rfc3339("2100-08-28T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert_eq!(expired.state, IdentityAssessmentKind::Unproven);
        assert!(expired
            .reasons
            .iter()
            .any(|reason| reason.contains("expired")));
    }

    #[test]
    fn schedule_is_off_by_default_and_validates_budget() {
        let schedule = AuditSchedule::default();
        assert!(!schedule.enabled);
        assert!(schedule.validate().is_ok());
        let mut invalid = schedule;
        invalid.monthly_request_limit = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn enabled_schedule_requires_bound_profiles_and_pair_budget() {
        let mut schedule = AuditSchedule {
            enabled: true,
            profile_id: Some("relay-one".to_owned()),
            ..AuditSchedule::default()
        };
        assert!(schedule.validate().is_ok());
        schedule.pair_official = true;
        assert!(schedule.validate().is_err());
        schedule.official_baseline_profile_id = Some("official-one".to_owned());
        schedule.monthly_request_limit = 299;
        assert!(schedule.validate().is_err());
        schedule.monthly_request_limit = 300;
        assert!(schedule.validate().is_ok());
    }

    #[test]
    fn next_local_time_applies_bounded_jitter_and_rolls_forward() {
        let schedule = AuditSchedule {
            enabled: true,
            profile_id: Some("relay-one".to_owned()),
            cadence: "daily".to_owned(),
            local_time: "20:00".to_owned(),
            ..AuditSchedule::default()
        };
        let now =
            NaiveDateTime::parse_from_str("2026-08-27 19:50:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let early = next_local_naive(&schedule, now, -30).unwrap();
        assert_eq!(early.date().to_string(), "2026-08-28");
        assert_eq!(early.time().format("%H:%M").to_string(), "19:30");
        let late = next_local_naive(&schedule, now, 30).unwrap();
        assert_eq!(late.date().to_string(), "2026-08-27");
        assert_eq!(late.time().format("%H:%M").to_string(), "20:30");
    }
}
