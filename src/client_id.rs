//! Resolves the `clientId` advertised in `getClusterNodes` to a friendly client
//! name (Solana v4.0 exposes it as a string). Kept dependency-free so it can be
//! shared verbatim with other tooling.

const KNOWN_CLIENTS: &[(u16, &str)] = &[
    (0, "SolanaLabs"),
    (1, "JitoLabs"),
    (2, "Frankendancer"),
    (3, "Agave"),
    (4, "AgavePaladin"),
    (5, "Firedancer"),
    (6, "AgaveBam"),
    (7, "Sig"),
    (8, "Rakurai"),
    (9, "HarmonicFiredancer"),
    (10, "HarmonicAgave"),
    (11, "HarmonicFrankendancer"),
    (12, "FireBAM"),
    (13, "Raiku"),
];

/// Resolves a raw `clientId` (name, number, or `Unknown(N)` form) to a friendly
/// client name. Falls back to the trimmed raw value, or `"unknown"` when absent.
pub fn resolve_client_name(raw: Option<&str>) -> String {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => "unknown".to_string(),
        Some(raw) => resolve_known_client(raw)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| raw.to_string()),
    }
}

fn resolve_known_client(raw: &str) -> Option<(u16, &'static str)> {
    if let Some(code) = parse_client_code(raw) {
        return KNOWN_CLIENTS
            .iter()
            .copied()
            .find(|(known_code, _)| *known_code == code);
    }

    let normalized = normalize_name(raw);
    KNOWN_CLIENTS
        .iter()
        .copied()
        .find(|(_, name)| normalize_name(name) == normalized)
}

/// Parses a numeric client id. Accepts plain integers (the legacy wire format)
/// and the `Unknown(N)` form emitted by the node's `ClientId` display.
fn parse_client_code(raw: &str) -> Option<u16> {
    if let Ok(code) = raw.parse::<u16>() {
        return Some(code);
    }
    let inner = raw.strip_prefix("Unknown(")?.strip_suffix(')')?;
    inner.trim().parse::<u16>().ok()
}

/// Normalizes a client name for tolerant matching: lowercase and drop any
/// non-alphanumeric characters so "Solana Labs", "SolanaLabs" and "solanalabs"
/// all resolve to the same client.
fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::resolve_client_name;

    #[test]
    fn resolves_by_name() {
        assert_eq!(resolve_client_name(Some("Firedancer")), "Firedancer");
    }

    #[test]
    fn resolves_ignoring_spaces_and_case() {
        assert_eq!(resolve_client_name(Some("Solana Labs")), "SolanaLabs");
        assert_eq!(resolve_client_name(Some("agave bam")), "AgaveBam");
    }

    #[test]
    fn resolves_new_clients() {
        assert_eq!(resolve_client_name(Some("FireBAM")), "FireBAM");
        assert_eq!(resolve_client_name(Some("Raiku")), "Raiku");
    }

    #[test]
    fn resolves_numeric_and_unknown_forms() {
        assert_eq!(resolve_client_name(Some("5")), "Firedancer");
        assert_eq!(resolve_client_name(Some("Unknown(13)")), "Raiku");
    }

    #[test]
    fn keeps_unresolved_raw() {
        assert_eq!(resolve_client_name(Some("Mango")), "Mango");
    }

    #[test]
    fn empty_is_unknown() {
        assert_eq!(resolve_client_name(Some("  ")), "unknown");
        assert_eq!(resolve_client_name(None), "unknown");
    }
}
