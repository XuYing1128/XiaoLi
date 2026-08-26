//! Privacy-preserving connection-origin classification for Codex sessions.
//!
//! The classifier deliberately answers a narrower question than model routing:
//! it describes the configured connection origin. It never treats an endpoint,
//! provider id, latency, token usage, or response behavior as proof of the
//! physical model that served a request.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionOriginKind {
    OfficialChatGpt,
    OfficialOpenAiApi,
    OfficialAnthropicApi,
    ManagedProvider,
    CustomEndpoint,
    LocalEndpoint,
    #[default]
    Unknown,
}

impl ConnectionOriginKind {
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::OfficialChatGpt => "officialChatGpt",
            Self::OfficialOpenAiApi => "officialOpenAiApi",
            Self::OfficialAnthropicApi => "officialAnthropicApi",
            Self::ManagedProvider => "managedProvider",
            Self::CustomEndpoint => "customEndpoint",
            Self::LocalEndpoint => "localEndpoint",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionAuthMode {
    ChatGpt,
    ApiKey,
    External,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionOriginConfidence {
    Configured,
    Partial,
    #[default]
    Unknown,
}

/// Endpoint classes are evidence about the configured network destination,
/// not evidence about the physical model behind that destination.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EndpointClass {
    OfficialChatGpt,
    OfficialOpenAi,
    OfficialAnthropic,
    ManagedProvider,
    CustomEndpoint,
    LocalEndpoint,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionOriginSnapshot {
    pub kind: ConnectionOriginKind,
    pub auth_mode: ConnectionAuthMode,
    pub confidence: ConnectionOriginConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub endpoint_class: EndpointClass,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

impl ConnectionOriginSnapshot {
    pub fn unknown() -> Self {
        Self::default()
    }
}

/// Sanitized inputs used by [`classify_connection_origin`].
///
/// URL values are borrowed and used only during this call. They are never
/// copied into [`ConnectionOriginSnapshot`]. `openai_base_url` is treated as
/// the effective override only for an OpenAI-like (or unspecified) provider.
#[derive(Clone, Copy, Default)]
pub struct ConnectionOriginInput<'a> {
    pub session_provider_id: Option<&'a str>,
    pub provider_base_url: Option<&'a str>,
    pub openai_base_url: Option<&'a str>,
    pub auth_mode: ConnectionAuthMode,
    pub hook_endpoint_class: Option<EndpointClass>,
}

/// The only Codex configuration fields retained by the minimal TOML parser.
///
/// Endpoint strings contain only a normalized credential-free endpoint scope:
/// `scheme://host:effective-port/base-path`. Userinfo, query, and fragment data
/// are discarded so credentials embedded in a URL cannot escape through logs
/// or serialization by a caller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedCodexConnectionConfig {
    pub model_provider: Option<String>,
    pub provider_base_url: Option<String>,
    pub openai_base_url: Option<String>,
    provider_base_urls: BTreeMap<String, String>,
}

impl ParsedCodexConnectionConfig {
    /// Resolves a provider URL as a sanitized origin. Session metadata takes
    /// precedence over the config-wide selected provider when supplied.
    pub fn provider_base_url_for(&self, provider_id: Option<&str>) -> Option<&str> {
        let provider = provider_id
            .and_then(sanitize_provider_id)
            .or_else(|| self.model_provider.clone())?;
        self.provider_base_urls
            .get(&provider)
            .map(String::as_str)
            .or_else(|| {
                (self.model_provider.as_deref() == Some(provider.as_str()))
                    .then_some(self.provider_base_url.as_deref())
                    .flatten()
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderFamily {
    OpenAi,
    Anthropic,
    Managed,
    Local,
    Other,
}

/// Classifies the configured origin using only explicit configuration and hook
/// evidence. Conflicting independent evidence always degrades to `unknown`.
pub fn classify_connection_origin(input: ConnectionOriginInput<'_>) -> ConnectionOriginSnapshot {
    let provider_id = input.session_provider_id.and_then(sanitize_provider_id);
    let provider_family = input
        .session_provider_id
        .map(provider_family)
        .unwrap_or(ProviderFamily::Other);

    let provider_endpoint = input.provider_base_url.map(classify_endpoint);
    let openai_override = input.openai_base_url.map(classify_endpoint);

    let mut evidence = Vec::new();
    let mut limitations = Vec::new();
    if provider_id.is_some() {
        push_unique(&mut evidence, "sessionProvider");
    } else if input.session_provider_id.is_some() {
        push_unique(&mut limitations, "providerIdOmitted");
    }
    if input.provider_base_url.is_some() {
        push_unique(&mut evidence, "providerEndpoint");
        if provider_endpoint == Some(EndpointClass::Unknown) {
            push_unique(&mut limitations, "providerEndpointUnparseable");
        }
    }
    if input.openai_base_url.is_some() {
        push_unique(&mut evidence, "openAiBaseUrlOverride");
        if openai_override == Some(EndpointClass::Unknown) {
            push_unique(&mut limitations, "openAiBaseUrlUnparseable");
        }
    }
    if input.hook_endpoint_class.is_some() {
        push_unique(&mut evidence, "hookEndpoint");
    }
    if input.auth_mode != ConnectionAuthMode::Unknown {
        push_unique(&mut evidence, "authMode");
    }

    let (configured_endpoint, configuration_conflict) = match provider_family {
        // The explicit OpenAI base URL is the higher-precedence override for
        // OpenAI-like providers. The lower-precedence provider URL therefore
        // is not independent conflicting evidence.
        ProviderFamily::OpenAi => (
            known_endpoint(openai_override).or_else(|| known_endpoint(provider_endpoint)),
            false,
        ),
        // An OpenAI override is unrelated to a selected non-OpenAI provider.
        ProviderFamily::Anthropic | ProviderFamily::Managed | ProviderFamily::Local => {
            (known_endpoint(provider_endpoint), false)
        }
        ProviderFamily::Other => {
            let provider = known_endpoint(provider_endpoint);
            let override_endpoint = known_endpoint(openai_override);
            let conflict =
                matches!((provider, override_endpoint), (Some(left), Some(right)) if left != right);
            (override_endpoint.or(provider), conflict)
        }
    };

    let hook_endpoint = input
        .hook_endpoint_class
        .filter(|class| *class != EndpointClass::Unknown);
    let runtime_conflict = matches!(
        (configured_endpoint, hook_endpoint),
        (Some(configured), Some(runtime)) if configured != runtime
    );
    let provider_conflict = configured_endpoint
        .map(|endpoint| provider_endpoint_conflicts(provider_family, endpoint))
        .unwrap_or(false);

    if configuration_conflict || runtime_conflict || provider_conflict {
        push_unique(&mut limitations, "conflictingOriginEvidence");
        return ConnectionOriginSnapshot {
            kind: ConnectionOriginKind::Unknown,
            auth_mode: input.auth_mode,
            confidence: ConnectionOriginConfidence::Unknown,
            provider_id,
            endpoint_class: EndpointClass::Unknown,
            evidence,
            limitations,
        };
    }

    let (endpoint_class, endpoint_confidence) = match configured_endpoint.or(hook_endpoint) {
        Some(endpoint) => {
            let confidence = if configured_endpoint.is_some() {
                ConnectionOriginConfidence::Configured
            } else {
                ConnectionOriginConfidence::Partial
            };
            (endpoint, confidence)
        }
        None => {
            push_unique(&mut limitations, "endpointMissing");
            return ConnectionOriginSnapshot {
                kind: ConnectionOriginKind::Unknown,
                auth_mode: input.auth_mode,
                confidence: ConnectionOriginConfidence::Unknown,
                provider_id,
                endpoint_class: EndpointClass::Unknown,
                evidence,
                limitations,
            };
        }
    };

    let (kind, confidence) = match endpoint_class {
        EndpointClass::OfficialChatGpt if input.auth_mode == ConnectionAuthMode::ChatGpt => {
            (ConnectionOriginKind::OfficialChatGpt, endpoint_confidence)
        }
        EndpointClass::OfficialOpenAi if input.auth_mode == ConnectionAuthMode::ApiKey => {
            (ConnectionOriginKind::OfficialOpenAiApi, endpoint_confidence)
        }
        EndpointClass::OfficialAnthropic if input.auth_mode == ConnectionAuthMode::ApiKey => (
            ConnectionOriginKind::OfficialAnthropicApi,
            endpoint_confidence,
        ),
        EndpointClass::OfficialChatGpt
        | EndpointClass::OfficialOpenAi
        | EndpointClass::OfficialAnthropic => {
            push_unique(&mut limitations, "officialEndpointNeedsMatchingAuth");
            (
                ConnectionOriginKind::Unknown,
                ConnectionOriginConfidence::Unknown,
            )
        }
        EndpointClass::ManagedProvider => {
            (ConnectionOriginKind::ManagedProvider, endpoint_confidence)
        }
        EndpointClass::CustomEndpoint => {
            (ConnectionOriginKind::CustomEndpoint, endpoint_confidence)
        }
        EndpointClass::LocalEndpoint => (ConnectionOriginKind::LocalEndpoint, endpoint_confidence),
        EndpointClass::Unknown => {
            push_unique(&mut limitations, "endpointUnclassified");
            (
                ConnectionOriginKind::Unknown,
                ConnectionOriginConfidence::Unknown,
            )
        }
    };

    ConnectionOriginSnapshot {
        kind,
        auth_mode: input.auth_mode,
        confidence,
        provider_id,
        endpoint_class,
        evidence,
        limitations,
    }
}

/// Resolves parsed Codex configuration and session evidence into a snapshot.
/// This is the preferred integration entrypoint for the collector/app layer.
pub fn resolve_connection_origin(
    config: Option<&ParsedCodexConnectionConfig>,
    session_provider_id: Option<&str>,
    auth_mode: ConnectionAuthMode,
    hook_endpoint_class: Option<EndpointClass>,
) -> ConnectionOriginSnapshot {
    let provider_id =
        session_provider_id.or_else(|| config.and_then(|parsed| parsed.model_provider.as_deref()));
    let provider_base_url = config.and_then(|parsed| parsed.provider_base_url_for(provider_id));
    let openai_base_url = config.and_then(|parsed| parsed.openai_base_url.as_deref());

    classify_connection_origin(ConnectionOriginInput {
        session_provider_id: provider_id,
        provider_base_url,
        openai_base_url,
        auth_mode,
        hook_endpoint_class,
    })
}

/// Parses `auth.json` without deserializing or retaining any token fields.
pub fn parse_codex_auth_mode(auth_json: &str) -> ConnectionAuthMode {
    #[derive(Deserialize)]
    struct AuthModeOnly {
        #[serde(default, alias = "authMode")]
        auth_mode: Option<String>,
    }

    serde_json::from_str::<AuthModeOnly>(auth_json)
        .ok()
        .and_then(|auth| auth.auth_mode)
        .map(|mode| parse_auth_mode(&mode))
        .unwrap_or(ConnectionAuthMode::Unknown)
}

/// Normalizes the small set of authentication labels XiaoLi can safely use.
/// Unknown labels remain unknown rather than being guessed from provider data.
pub fn parse_auth_mode(value: &str) -> ConnectionAuthMode {
    match normalize_identifier(value).as_str() {
        "chatgpt" | "chat-gpt" | "chat-gpt-auth" => ConnectionAuthMode::ChatGpt,
        "api-key" | "apikey" => ConnectionAuthMode::ApiKey,
        "external" | "external-auth" | "managed-identity" | "agent-identity" => {
            ConnectionAuthMode::External
        }
        _ => ConnectionAuthMode::Unknown,
    }
}

/// Extracts only origin-related fields from Codex TOML configuration.
///
/// This intentionally is not a general TOML parser. It understands the simple
/// scalar assignments and `[model_providers.<id>]` tables emitted by Codex and
/// ignores every other key, including credentials and headers.
pub fn parse_codex_connection_config(config_toml: &str) -> ParsedCodexConnectionConfig {
    let mut selected_provider = None;
    let mut openai_base_url = None;
    let mut provider_urls = BTreeMap::<String, String>::new();
    let mut current_provider_section = None::<String>;

    for raw_line in config_toml.lines() {
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            current_provider_section = parse_provider_section(&line[1..line.len() - 1]);
            continue;
        }

        let Some((raw_key, raw_value)) = split_toml_assignment(line) else {
            continue;
        };
        let key = raw_key.trim();
        let Some(value) = parse_toml_string(raw_value.trim()) else {
            continue;
        };

        if let Some(provider) = current_provider_section.as_ref() {
            if key == "base_url" {
                if let Some(origin) = sanitize_endpoint_origin(&value) {
                    provider_urls.insert(provider.clone(), origin);
                }
            }
            continue;
        }

        match key {
            "model_provider" => selected_provider = sanitize_provider_id(&value),
            "openai_base_url" => {
                openai_base_url = sanitize_endpoint_origin(&value);
            }
            _ => {
                if let Some(provider) = dotted_provider_base_url_key(key) {
                    if let (Some(provider), Some(origin)) = (
                        sanitize_provider_id(&provider),
                        sanitize_endpoint_origin(&value),
                    ) {
                        provider_urls.insert(provider, origin);
                    }
                }
            }
        }
    }

    let provider_base_url = selected_provider
        .as_ref()
        .and_then(|provider| provider_urls.get(provider).cloned());

    ParsedCodexConnectionConfig {
        model_provider: selected_provider,
        provider_base_url,
        openai_base_url,
        provider_base_urls: provider_urls,
    }
}

/// Classifies a URL/endpoint without retaining it. Only the authority host is
/// inspected, with exact or label-boundary suffix comparisons.
pub fn classify_endpoint(value: &str) -> EndpointClass {
    let Some(host) = endpoint_host(value) else {
        return EndpointClass::Unknown;
    };

    if is_loopback_host(&host) {
        EndpointClass::LocalEndpoint
    } else if host == "chatgpt.com" || host == "chat.openai.com" {
        EndpointClass::OfficialChatGpt
    } else if host == "api.openai.com" {
        EndpointClass::OfficialOpenAi
    } else if host == "api.anthropic.com" {
        EndpointClass::OfficialAnthropic
    } else if is_managed_provider_host(&host) {
        EndpointClass::ManagedProvider
    } else {
        EndpointClass::CustomEndpoint
    }
}

fn provider_family(value: &str) -> ProviderFamily {
    match normalize_identifier(value).as_str() {
        "openai" | "openai-api" | "chatgpt" | "openai-chatgpt" | "codex" => ProviderFamily::OpenAi,
        "anthropic" | "claude" => ProviderFamily::Anthropic,
        "azure" | "azure-openai" | "aws-bedrock" | "bedrock" | "vertex" | "vertex-ai"
        | "google-vertex" => ProviderFamily::Managed,
        "local" | "ollama" | "lmstudio" | "lm-studio" | "vllm" | "llamacpp" | "llama-cpp" => {
            ProviderFamily::Local
        }
        _ => ProviderFamily::Other,
    }
}

fn provider_endpoint_conflicts(family: ProviderFamily, endpoint: EndpointClass) -> bool {
    match family {
        ProviderFamily::OpenAi => endpoint == EndpointClass::OfficialAnthropic,
        ProviderFamily::Anthropic => matches!(
            endpoint,
            EndpointClass::OfficialOpenAi | EndpointClass::OfficialChatGpt
        ),
        ProviderFamily::Managed => endpoint != EndpointClass::ManagedProvider,
        ProviderFamily::Local => endpoint != EndpointClass::LocalEndpoint,
        ProviderFamily::Other => false,
    }
}

fn known_endpoint(value: Option<EndpointClass>) -> Option<EndpointClass> {
    value.filter(|class| *class != EndpointClass::Unknown)
}

fn normalize_identifier(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| match character {
            '_' | ' ' => '-',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

fn sanitize_provider_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 64
        || !trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn endpoint_host(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }

    let authority_and_path = if let Some((scheme, remainder)) = trimmed.split_once("://") {
        if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
            return None;
        }
        remainder
    } else {
        trimmed.strip_prefix("//").unwrap_or(trimmed)
    };
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, tail)| tail);
    if host_port.is_empty() {
        return None;
    }

    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        let end = bracketed.find(']')?;
        &bracketed[..end]
    } else if host_port.matches(':').count() == 1 {
        host_port
            .split_once(':')
            .map_or(host_port, |(host, _)| host)
    } else {
        host_port
    };

    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | ':' | '_'))
        })
    {
        None
    } else {
        Some(normalized)
    }
}

fn sanitize_endpoint_origin(value: &str) -> Option<String> {
    normalize_endpoint_scope(value)
}

/// Produces the private endpoint scope used for conservative relay-profile
/// binding. The value is never serialized: callers retain only its short
/// SHA-256 digest. Default ports and base paths are included so endpoints that
/// share a host cannot be silently conflated.
pub(crate) fn normalize_endpoint_scope(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&candidate).ok()?;
    let scheme = url.scheme().to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }
    let host = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let port = url.port_or_known_default()?;

    let rendered_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let segments = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let base_path = if segments.is_empty() {
        String::new()
    } else {
        format!("/{}", segments.join("/"))
    };
    Some(format!("{scheme}://{rendered_host}:{port}{base_path}"))
}

/// Returns a private 64-bit prefix of SHA-256 for one normalized endpoint
/// scope. Sixteen hex characters preserve the existing hook/cache wire shape.
pub(crate) fn endpoint_scope_hash(value: &str) -> Option<String> {
    let scope = normalize_endpoint_scope(value)?;
    Some(endpoint_scope_digest(&scope))
}

/// Combines multiple environment endpoint scopes deterministically. Duplicate
/// spellings normalize to one scope; distinct scopes remain distinct so a
/// conflicting environment cannot accidentally match a single saved profile.
pub(crate) fn combined_endpoint_scope_hash(values: &[String]) -> Option<String> {
    let mut scopes = values
        .iter()
        .filter_map(|value| normalize_endpoint_scope(value))
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();
    (!scopes.is_empty()).then(|| endpoint_scope_digest(&scopes.join("|")))
}

fn endpoint_scope_digest(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_loopback_host(host: &str) -> bool {
    if matches!(
        host,
        "localhost" | "::1" | "host.docker.internal" | "gateway.docker.internal"
    ) {
        return true;
    }

    let octets = host.split('.').collect::<Vec<_>>();
    octets.len() == 4
        && octets[0] == "127"
        && octets.iter().all(|octet| octet.parse::<u8>().is_ok())
}

fn is_managed_provider_host(host: &str) -> bool {
    domain_matches(host, "openai.azure.com")
        || domain_matches(host, "services.ai.azure.com")
        || domain_matches(host, "aiplatform.googleapis.com")
        || host.ends_with("-aiplatform.googleapis.com")
        || domain_matches(host, "generativelanguage.googleapis.com")
        || (domain_matches(host, "amazonaws.com")
            && host
                .split('.')
                .any(|label| label == "bedrock" || label.starts_with("bedrock-")))
        || (domain_matches(host, "api.aws")
            && host
                .split('.')
                .any(|label| label == "bedrock" || label.starts_with("bedrock-")))
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_owned());
    }
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quote = None::<char>;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, character) {
            (Some('"'), '\\') => escaped = true,
            (Some(current), candidate) if current == candidate => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '#') => return &line[..index],
            _ => {}
        }
    }
    line
}

fn split_toml_assignment(line: &str) -> Option<(&str, &str)> {
    let mut quote = None::<char>;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, character) {
            (Some('"'), '\\') => escaped = true,
            (Some(current), candidate) if current == candidate => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '=') => return Some((&line[..index], &line[index + 1..])),
            _ => {}
        }
    }
    None
}

fn parse_toml_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        serde_json::from_str::<String>(trimmed).ok()
    } else if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        Some(trimmed[1..trimmed.len() - 1].to_owned())
    } else if !trimmed.is_empty() && !trimmed.chars().any(|character| character.is_whitespace()) {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

fn parse_provider_section(section: &str) -> Option<String> {
    let suffix = section.trim().strip_prefix("model_providers.")?.trim();
    let provider = parse_toml_string(suffix).unwrap_or_else(|| suffix.to_owned());
    sanitize_provider_id(&provider)
}

fn dotted_provider_base_url_key(key: &str) -> Option<String> {
    let suffix = key.strip_prefix("model_providers.")?;
    let provider = suffix.strip_suffix(".base_url")?;
    parse_toml_string(provider).or_else(|| Some(provider.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        provider: Option<&'a str>,
        endpoint: Option<&'a str>,
        auth_mode: ConnectionAuthMode,
    ) -> ConnectionOriginInput<'a> {
        ConnectionOriginInput {
            session_provider_id: provider,
            provider_base_url: endpoint,
            auth_mode,
            ..ConnectionOriginInput::default()
        }
    }

    #[test]
    fn serializes_contract_names_in_camel_case() {
        let value = serde_json::to_value(ConnectionOriginSnapshot {
            kind: ConnectionOriginKind::OfficialOpenAiApi,
            auth_mode: ConnectionAuthMode::ApiKey,
            confidence: ConnectionOriginConfidence::Configured,
            provider_id: Some("openai".to_owned()),
            endpoint_class: EndpointClass::OfficialOpenAi,
            evidence: vec!["authMode".to_owned()],
            limitations: Vec::new(),
        })
        .unwrap();

        assert_eq!(value["kind"], "officialOpenAiApi");
        assert_eq!(value["authMode"], "apiKey");
        assert_eq!(value["confidence"], "configured");
        assert_eq!(value["endpointClass"], "officialOpenAi");
        assert!(value.get("provider_base_url").is_none());
    }

    #[test]
    fn classifies_official_chatgpt_only_with_matching_auth() {
        let result = classify_connection_origin(input(
            Some("openai"),
            Some("https://chatgpt.com/backend-api/codex"),
            ConnectionAuthMode::ChatGpt,
        ));
        assert_eq!(result.kind, ConnectionOriginKind::OfficialChatGpt);
        assert_eq!(result.confidence, ConnectionOriginConfidence::Configured);

        let mismatch = classify_connection_origin(input(
            Some("openai"),
            Some("https://chatgpt.com/backend-api/codex"),
            ConnectionAuthMode::ApiKey,
        ));
        assert_eq!(mismatch.kind, ConnectionOriginKind::Unknown);
        assert!(mismatch
            .limitations
            .contains(&"officialEndpointNeedsMatchingAuth".to_owned()));
    }

    #[test]
    fn classifies_official_openai_api_only_with_api_key_auth() {
        let result = classify_connection_origin(input(
            Some("openai"),
            Some("https://api.openai.com/v1"),
            ConnectionAuthMode::ApiKey,
        ));
        assert_eq!(result.kind, ConnectionOriginKind::OfficialOpenAiApi);
        assert_eq!(result.endpoint_class, EndpointClass::OfficialOpenAi);
    }

    #[test]
    fn classifies_official_anthropic_api_only_with_api_key_auth() {
        let result = classify_connection_origin(input(
            Some("anthropic"),
            Some("https://api.anthropic.com/v1/messages"),
            ConnectionAuthMode::ApiKey,
        ));
        assert_eq!(result.kind, ConnectionOriginKind::OfficialAnthropicApi);
        assert_eq!(result.endpoint_class, EndpointClass::OfficialAnthropic);
    }

    #[test]
    fn classifies_custom_and_loopback_endpoints_without_claiming_official() {
        let custom = classify_connection_origin(input(
            Some("relay"),
            Some("https://relay.example/v1"),
            ConnectionAuthMode::ApiKey,
        ));
        assert_eq!(custom.kind, ConnectionOriginKind::CustomEndpoint);

        let local = classify_connection_origin(input(
            Some("ollama"),
            Some("http://127.0.0.7:11434/v1"),
            ConnectionAuthMode::Unknown,
        ));
        assert_eq!(local.kind, ConnectionOriginKind::LocalEndpoint);
    }

    #[test]
    fn classifies_azure_bedrock_and_vertex_as_managed() {
        for (provider, endpoint) in [
            (
                "azure-openai",
                "https://sample-resource.openai.azure.com/openai/deployments/model",
            ),
            (
                "bedrock",
                "https://bedrock-runtime.us-east-1.amazonaws.com/model/invoke",
            ),
            (
                "vertex-ai",
                "https://us-central1-aiplatform.googleapis.com/v1/projects/project",
            ),
        ] {
            let result = classify_connection_origin(input(
                Some(provider),
                Some(endpoint),
                ConnectionAuthMode::External,
            ));
            assert_eq!(result.kind, ConnectionOriginKind::ManagedProvider);
            assert_eq!(result.endpoint_class, EndpointClass::ManagedProvider);
        }
    }

    #[test]
    fn returns_unknown_when_endpoint_is_missing() {
        let result =
            classify_connection_origin(input(Some("openai"), None, ConnectionAuthMode::ApiKey));
        assert_eq!(result.kind, ConnectionOriginKind::Unknown);
        assert_eq!(result.confidence, ConnectionOriginConfidence::Unknown);
        assert!(result.limitations.contains(&"endpointMissing".to_owned()));
    }

    #[test]
    fn returns_unknown_when_config_and_hook_conflict() {
        let result = classify_connection_origin(ConnectionOriginInput {
            session_provider_id: Some("openai"),
            provider_base_url: Some("https://api.openai.com/v1"),
            auth_mode: ConnectionAuthMode::ApiKey,
            hook_endpoint_class: Some(EndpointClass::CustomEndpoint),
            ..ConnectionOriginInput::default()
        });
        assert_eq!(result.kind, ConnectionOriginKind::Unknown);
        assert_eq!(result.endpoint_class, EndpointClass::Unknown);
        assert!(result
            .limitations
            .contains(&"conflictingOriginEvidence".to_owned()));
    }

    #[test]
    fn returns_unknown_for_known_provider_family_conflict() {
        let result = classify_connection_origin(input(
            Some("anthropic"),
            Some("https://api.openai.com/v1"),
            ConnectionAuthMode::ApiKey,
        ));
        assert_eq!(result.kind, ConnectionOriginKind::Unknown);
        assert!(result
            .limitations
            .contains(&"conflictingOriginEvidence".to_owned()));
    }

    #[test]
    fn openai_base_url_overrides_builtin_openai_endpoint() {
        let result = classify_connection_origin(ConnectionOriginInput {
            session_provider_id: Some("openai"),
            provider_base_url: Some("https://api.openai.com/v1"),
            openai_base_url: Some("https://relay.example/v1"),
            auth_mode: ConnectionAuthMode::ApiKey,
            hook_endpoint_class: Some(EndpointClass::CustomEndpoint),
        });
        assert_eq!(result.kind, ConnectionOriginKind::CustomEndpoint);
        assert_eq!(result.endpoint_class, EndpointClass::CustomEndpoint);
    }

    #[test]
    fn unrelated_openai_override_does_not_conflict_with_anthropic_provider() {
        let result = classify_connection_origin(ConnectionOriginInput {
            session_provider_id: Some("anthropic"),
            provider_base_url: Some("https://api.anthropic.com/v1"),
            openai_base_url: Some("https://relay-for-openai.example/v1"),
            auth_mode: ConnectionAuthMode::ApiKey,
            hook_endpoint_class: None,
        });
        assert_eq!(result.kind, ConnectionOriginKind::OfficialAnthropicApi);
    }

    #[test]
    fn parses_auth_mode_without_retaining_secret_fields() {
        let mode = parse_codex_auth_mode(
            r#"{"auth_mode":"api_key","OPENAI_API_KEY":"sk-never-retain","tokens":{"access_token":"secret"}}"#,
        );
        assert_eq!(mode, ConnectionAuthMode::ApiKey);
        assert_eq!(
            parse_codex_auth_mode("not-json"),
            ConnectionAuthMode::Unknown
        );
    }

    #[test]
    fn parses_only_selected_provider_and_sanitizes_endpoint_origins() {
        let parsed = parse_codex_connection_config(
            r#"
model_provider = "relay"
openai_base_url = "https://user:global-secret@override.example/v1?api_key=hidden"
api_key = "must-not-be-retained"

[model_providers.openai]
base_url = "https://api.openai.com/v1"

[model_providers.relay]
base_url = "https://user:relay-secret@relay.example:8443/v1?token=hidden#fragment"
env_key = "RELAY_SECRET"
http_headers = { Authorization = "Bearer never" }
"#,
        );

        assert_eq!(parsed.model_provider.as_deref(), Some("relay"));
        assert_eq!(
            parsed.provider_base_url.as_deref(),
            Some("https://relay.example:8443/v1")
        );
        assert_eq!(
            parsed.openai_base_url.as_deref(),
            Some("https://override.example:443/v1")
        );
        let debug = format!("{parsed:?}");
        for forbidden in ["global-secret", "relay-secret", "hidden", "must-not"] {
            assert!(!debug.contains(forbidden));
        }
    }

    #[test]
    fn public_origin_snapshot_never_serializes_endpoint_or_credentials() {
        const PRIVATE_HOST: &str = "private-relay.example";
        const PRIVATE_USER: &str = "PRIVATE_USER_MUST_NOT_PERSIST";
        const PRIVATE_PASSWORD: &str = "PRIVATE_PASSWORD_MUST_NOT_PERSIST";
        const PRIVATE_QUERY: &str = "PRIVATE_QUERY_TOKEN_MUST_NOT_PERSIST";
        let endpoint = format!(
            "https://{PRIVATE_USER}:{PRIVATE_PASSWORD}@{PRIVATE_HOST}/v1?token={PRIVATE_QUERY}"
        );
        let snapshot = classify_connection_origin(ConnectionOriginInput {
            session_provider_id: Some("relay"),
            provider_base_url: Some(&endpoint),
            auth_mode: ConnectionAuthMode::ApiKey,
            hook_endpoint_class: Some(EndpointClass::CustomEndpoint),
            ..ConnectionOriginInput::default()
        });

        assert_eq!(snapshot.kind, ConnectionOriginKind::CustomEndpoint);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        for forbidden in [
            PRIVATE_HOST,
            PRIVATE_USER,
            PRIVATE_PASSWORD,
            PRIVATE_QUERY,
            "baseUrl",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "origin snapshot leaked {forbidden}"
            );
        }
        let value = serde_json::to_value(&snapshot).unwrap();
        for forbidden_field in ["baseUrl", "endpoint", "credential", "apiKeyValue"] {
            assert!(value.get(forbidden_field).is_none());
        }
    }

    #[test]
    fn supports_dotted_provider_base_url_and_ipv6_loopback() {
        let parsed = parse_codex_connection_config(
            r#"
model_provider = "local"
model_providers.local.base_url = "http://[::1]:11434/v1"
"#,
        );
        assert_eq!(
            parsed.provider_base_url.as_deref(),
            Some("http://[::1]:11434/v1")
        );
        assert_eq!(
            classify_endpoint(parsed.provider_base_url.as_deref().unwrap()),
            EndpointClass::LocalEndpoint
        );
    }

    #[test]
    fn endpoint_scope_digest_covers_scheme_port_and_normalized_base_path() {
        let canonical = "https://relay.example:443/v1";
        assert_eq!(
            normalize_endpoint_scope(
                "https://user:secret@RELAY.example/v1//?token=hidden#fragment"
            )
            .as_deref(),
            Some(canonical)
        );
        assert_eq!(
            endpoint_scope_hash("https://relay.example/v1/"),
            endpoint_scope_hash(canonical)
        );
        assert_ne!(
            endpoint_scope_hash("https://relay.example/v1"),
            endpoint_scope_hash("https://relay.example:8443/v1")
        );
        assert_ne!(
            endpoint_scope_hash("https://relay.example/v1"),
            endpoint_scope_hash("https://relay.example/compatible/v1")
        );
        assert_ne!(
            endpoint_scope_hash("https://relay.example/v1"),
            endpoint_scope_hash("http://relay.example/v1")
        );
    }

    #[test]
    fn combined_endpoint_scope_digest_is_order_independent_but_preserves_conflicts() {
        let duplicates = vec![
            "https://relay.example/v1/".to_owned(),
            "https://RELAY.example:443/v1".to_owned(),
        ];
        let single = vec!["https://relay.example/v1".to_owned()];
        assert_eq!(
            combined_endpoint_scope_hash(&duplicates),
            combined_endpoint_scope_hash(&single)
        );

        let mut conflicting = vec![
            "https://relay.example/v1".to_owned(),
            "https://relay.example/other".to_owned(),
        ];
        let forward = combined_endpoint_scope_hash(&conflicting);
        conflicting.reverse();
        assert_eq!(forward, combined_endpoint_scope_hash(&conflicting));
        assert_ne!(forward, combined_endpoint_scope_hash(&single));
    }

    #[test]
    fn resolver_uses_session_provider_instead_of_config_default() {
        let parsed = parse_codex_connection_config(
            r#"
model_provider = "openai"

[model_providers.openai]
base_url = "https://api.openai.com/v1"

[model_providers.relay]
base_url = "https://relay.example/v1"
"#,
        );
        let result = resolve_connection_origin(
            Some(&parsed),
            Some("relay"),
            ConnectionAuthMode::ApiKey,
            Some(EndpointClass::CustomEndpoint),
        );

        assert_eq!(result.provider_id.as_deref(), Some("relay"));
        assert_eq!(result.kind, ConnectionOriginKind::CustomEndpoint);
    }

    #[test]
    fn suffix_checks_do_not_trust_lookalike_domains() {
        assert_eq!(
            classify_endpoint("https://api.openai.com.evil.example/v1"),
            EndpointClass::CustomEndpoint
        );
        assert_eq!(
            classify_endpoint("https://openai.azure.com.evil.example/v1"),
            EndpointClass::CustomEndpoint
        );
    }
}
