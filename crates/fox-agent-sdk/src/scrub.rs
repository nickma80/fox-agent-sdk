//! Secret scrubbing: detects and masks sensitive data in event streams
//! and log exports.
//!
//! Complies with PRD §5.2: "敏感信息在事件和日志导出中必须支持脱敏".
//!
//! Uses simple string scanning to cover the most common secret patterns
//! (API keys, JWTs, passwords, PEM blocks) without external dependencies.

/// Mask secrets in a string, replacing them with placeholder markers.
pub fn mask_secrets(text: &str) -> String {
    let mut result = text.to_string();
    result = mask_private_key_blocks(&result);
    result = mask_prefix_keys(&result);
    result = mask_jwt_tokens(&result);
    result = mask_header_secrets(&result);
    result = mask_assignment_secrets(&result);
    result
}

/// Mask PEM private key blocks.
fn mask_private_key_blocks(s: &str) -> String {
    let mut result = s.to_string();
    if let Some(start) = result.find("-----BEGIN ")
        && let Some(_rel_end) = result[start..].find("PRIVATE KEY-----")
    {
        let block_start = start;
        if let Some(rel_final) = result[start..].find("-----END ") {
            let end_abs = start + rel_final;
            if let Some(rel_end_line) = result[end_abs..].find("PRIVATE KEY-----") {
                let block_end = end_abs + rel_end_line + "PRIVATE KEY-----".len();
                result.replace_range(block_start..block_end, "[PRIVATE_KEY]");
            }
        }
    }
    result
}

/// Mask API key tokens starting with `sk-` prefix.
fn mask_prefix_keys(s: &str) -> String {
    let mut result = s.to_string();
    if let Some(pos) = result.find("sk-")
        && (pos == 0 || {
            let prev = result.as_bytes()[pos - 1];
            prev.is_ascii_whitespace()
                || prev == b':'
                || prev == b'='
                || prev == b'\''
                || prev == b'"'
                || prev == b'/'
                || prev == b'@'
        })
    {
        let token_end = result[pos..]
            .find(|c: char| {
                c.is_ascii_whitespace()
                    || c == ','
                    || c == '"'
                    || c == '\''
                    || c == ';'
                    || c == '}'
                    || c == '\n'
            })
            .map(|e| pos + e)
            .unwrap_or(result.len());
        result.replace_range(pos..token_end, "[API_KEY]");
    }
    result
}

/// Mask JWT tokens (eyJ header)
fn mask_jwt_tokens(s: &str) -> String {
    let mut result = s.to_string();
    let mut offset = 0;
    while let Some(pos) = result[offset..].find("eyJ") {
        let abs = offset + pos;
        if let Some(dot1) = result[abs..].find('.')
            && let Some(dot2) = result[abs + dot1 + 1..].find('.')
        {
            let end = result[abs + dot1 + 1 + dot2..]
                .find(|c: char| {
                    c.is_ascii_whitespace()
                        || c == '"'
                        || c == '\''
                        || c == ','
                        || c == ';'
                        || c == '}'
                })
                .map(|e| abs + dot1 + 1 + dot2 + e)
                .unwrap_or(result.len());
            result.replace_range(abs..end, "[JWT]");
            offset = abs + "[JWT]".len();
            continue;
        }
        offset = abs + 3;
    }
    result
}

/// Mask Authorization and API key header values.
fn mask_header_secrets(s: &str) -> String {
    let mut result = s.to_string();
    for pattern in &["authorization:", "x-api-key:", "api-key:"] {
        let mut search_from = 0;
        loop {
            let lower = result.to_lowercase();
            let haystack = &lower[search_from..];
            let pos = match haystack.find(pattern) {
                Some(p) => search_from + p,
                None => break,
            };
            let after = pos + pattern.len();
            let value_start = result[after..]
                .find(|c: char| !c.is_ascii_whitespace())
                .map(|vs| after + vs)
                .unwrap_or(after);
            let value_end = result[value_start..]
                .find(|c: char| c.is_ascii_whitespace())
                .map(|ve| value_start + ve)
                .unwrap_or(result.len());
            if value_end > value_start + 8 {
                let replacement = "[REDACTED]".to_string();
                result.replace_range(value_start..value_end, &replacement);
                search_from = value_start + replacement.len();
            } else {
                search_from = value_end;
            }
        }
    }
    result
}

/// Mask password=, secret=, token= assignment values.
fn mask_assignment_secrets(s: &str) -> String {
    let mut result = s.to_string();
    let lower = result.to_lowercase();
    for key in &["password=", "passwd=", "secret=", "token="] {
        let mut offset = 0;
        while let Some(pos) = lower[offset..].find(key) {
            let abs = offset + pos;
            let value_start = abs + key.len();
            let value_end = result[value_start..]
                .find(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';' || c == '\n')
                .map(|ve| value_start + ve)
                .unwrap_or(result.len());
            if value_end > value_start {
                result.replace_range(value_start..value_end, "[REDACTED]");
                offset = value_start + "[REDACTED]".len();
            } else {
                offset = value_end;
            }
        }
    }
    result
}

/// Check whether a string contains any detected secrets.
pub fn contains_secrets(text: &str) -> bool {
    mask_secrets(text) != text
}

/// Mask secrets for event payload export.
pub fn mask_event_payload(text: &str) -> String {
    mask_secrets(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_openai_key() {
        let input = "Authorization: Bearer sk-proj-abcdefghijklmnopqrstuvwxyz";
        let result = mask_secrets(input);
        assert!(!result.contains("sk-proj"), "should mask key: {result}");
    }

    #[test]
    fn mask_jwt_token() {
        let input = "token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhZG1pbiJ9.sig1234567890";
        let result = mask_secrets(input);
        assert!(!result.contains("eyJ"), "should mask JWT: {result}");
    }

    #[test]
    fn mask_password_assignment() {
        let input = "password=superSecret123!";
        let result = mask_secrets(input);
        assert!(
            !result.contains("superSecret"),
            "should mask password: {result}"
        );
    }

    #[test]
    fn harmless_content_passes_through() {
        let input = "Hello, I read Cargo.toml and found 3 dependencies";
        assert_eq!(mask_secrets(input), input);
    }
}
