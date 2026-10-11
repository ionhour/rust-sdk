//! Slug-mode helpers: slug and API-key validation, key normalization, and the
//! bounded reader for slug-route response bodies.

use crate::error::Error;

/// Max bytes of a slug-route response body the SDK reads and parses.
const MAX_SLUG_BODY_BYTES: usize = 4 * 1024;

/// Trim a key candidate; blank counts as absent.
pub(crate) fn normalize_api_key(raw: Option<String>) -> Option<String> {
    raw.map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

/// A slug must match `^[a-z0-9_-]{1,64}$` (ASCII only). The error carries only
/// a short prefix: a rejected "slug" may be a credential passed by mistake.
pub(crate) fn validate_slug(slug: String) -> Result<String, Error> {
    let valid = (1..=64).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if valid {
        Ok(slug)
    } else {
        Err(Error::InvalidSlug {
            slug_prefix: redacted_prefix(&slug),
        })
    }
}

/// Slug mode needs a present, header-safe key. Never echoes the key.
pub(crate) fn validate_api_key(key: Option<&str>) -> Result<(), Error> {
    match key {
        None => Err(Error::MissingApiKey),
        Some(k) if k.chars().any(|c| c.is_whitespace() || c.is_control()) => {
            Err(Error::InvalidApiKey)
        }
        Some(_) => Ok(()),
    }
}

/// At most 8 characters and never more than half of `value`, plus an ellipsis.
fn redacted_prefix(value: &str) -> String {
    let keep = (value.chars().count() / 2).min(8);
    let prefix: String = value.chars().take(keep).collect();
    format!("{prefix}…")
}

/// The subset of a slug-route response the SDK reads.
#[derive(Default, serde::Deserialize)]
pub(crate) struct SlugReply {
    #[serde(default)]
    pub(crate) created: bool,
    #[serde(default)]
    code: Option<String>,
}

impl SlugReply {
    /// Read at most 4 KiB of the body chunk by chunk, then parse it. A body
    /// that fails to read or parse yields the default (nothing surfaced).
    pub(crate) async fn read(mut response: reqwest::Response) -> Self {
        let mut buf: Vec<u8> = Vec::new();
        while buf.len() < MAX_SLUG_BODY_BYTES {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let room = MAX_SLUG_BODY_BYTES - buf.len();
                    buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
                }
                _ => break,
            }
        }
        Self::parse(&buf)
    }

    fn parse(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_default()
    }

    /// The envelope code, only when well-formed (`^[A-Z][A-Z0-9_]{0,63}$`), so
    /// a hostile body can never inject arbitrary text into an error message.
    pub(crate) fn code(&self) -> Option<String> {
        let code = self.code.as_deref()?;
        let mut chars = code.chars();
        let well_formed = code.len() <= 64
            && chars.next().is_some_and(|c| c.is_ascii_uppercase())
            && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        well_formed.then(|| code.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation_matches_server_pattern() {
        assert!(validate_slug("a".into()).is_ok());
        assert!(validate_slug("a".repeat(64)).is_ok());
        assert!(validate_slug("nightly_job-2".into()).is_ok());
        for bad in ["", "A", "a.b", "a/b", "é"] {
            assert!(matches!(
                validate_slug(bad.into()),
                Err(Error::InvalidSlug { .. })
            ));
        }
        assert!(validate_slug("a".repeat(65)).is_err());
    }

    #[test]
    fn invalid_slug_error_keeps_only_a_short_prefix() {
        match validate_slug("ionh_SECRETSECRET".into()) {
            Err(Error::InvalidSlug { slug_prefix }) => assert_eq!(slug_prefix, "ionh_SEC…"),
            other => panic!("unexpected {other:?}"),
        }
        match validate_slug("ABCD".into()) {
            Err(Error::InvalidSlug { slug_prefix }) => assert_eq!(slug_prefix, "AB…"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn api_key_normalization_and_validation() {
        assert_eq!(normalize_api_key(Some("  k ".into())), Some("k".into()));
        assert_eq!(normalize_api_key(Some("   ".into())), None);
        assert_eq!(normalize_api_key(None), None);
        assert!(matches!(validate_api_key(None), Err(Error::MissingApiKey)));
        assert!(matches!(
            validate_api_key(Some("a\nb")),
            Err(Error::InvalidApiKey)
        ));
        assert!(validate_api_key(Some("ionh_ok")).is_ok());
    }

    #[test]
    fn slug_reply_code_must_be_well_formed() {
        assert_eq!(
            SlugReply::parse(br#"{"code":"KEY_SCOPE_DENIED"}"#).code(),
            Some("KEY_SCOPE_DENIED".to_string())
        );
        assert_eq!(SlugReply::parse(br#"{"code":"bad code"}"#).code(), None);
        assert_eq!(SlugReply::parse(b"not json").code(), None);
        assert!(SlugReply::parse(br#"{"created":true}"#).created);
    }
}
