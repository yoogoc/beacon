//! Local inspection only: decoded JWT claims are never authentication results.
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use beacon_columns::{Timestamp, format_duration};
use beacon_kube::DynamicObject;
use serde_json::Value;

pub(crate) const TYPE: &str = "kubernetes.io/service-account-token";
pub(crate) const ACCOUNT: &str = "kubernetes.io/service-account.name";
pub(crate) const UID: &str = "kubernetes.io/service-account.uid";
pub(crate) const INVALID_SINCE: &str = "kubernetes.io/legacy-token-invalid-since";
pub(crate) const LAST_USED: &str = "kubernetes.io/legacy-token-last-used";

pub(crate) fn supported(group: &str, kind: &str, object: &DynamicObject) -> bool {
    group.is_empty() && kind == "Secret" && object.data["type"].as_str() == Some(TYPE)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Token,
    Namespace,
    Certificate,
}

impl Key {
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "token" => Some(Self::Token),
            "namespace" => Some(Self::Namespace),
            "ca.crt" => Some(Self::Certificate),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Token => "Service account token",
            Self::Namespace => "Namespace",
            Self::Certificate => "CA certificate bundle",
        }
    }

    pub fn order(self) -> u8 {
        match self {
            Self::Token => 0,
            Self::Namespace => 1,
            Self::Certificate => 2,
        }
    }
}

pub(crate) enum Inspection {
    Token(Result<Token, String>),
    Certificate(Result<Vec<crate::tls::CertificateInfo>, String>),
    Namespace(String),
}

pub(crate) fn inspect(name: &str, text: &str) -> Option<Inspection> {
    Some(match Key::of(name)? {
        Key::Token => Inspection::Token(Token::parse(text)),
        Key::Certificate => {
            Inspection::Certificate(crate::tls::inspect_pem(text.as_bytes(), "ca.crt"))
        }
        Key::Namespace => Inspection::Namespace(text.trim().to_owned()),
    })
}

pub(crate) struct Token {
    pub header: Value,
    pub claims: Value,
    pub expires: Option<i64>,
    pub not_before: Option<i64>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Timing {
    Expired,
    NotYetActive,
    ExpiresIn(i64),
    NoExpiryClaim,
}

impl Timing {
    pub fn label(&self) -> String {
        match self {
            Self::Expired => "Expired (exp claim)".into(),
            Self::NotYetActive => "Not yet active (nbf claim)".into(),
            Self::ExpiresIn(seconds) => crate::i18n::message(
                "messages.expires_in",
                &[("value0", (format_duration(*seconds)).to_string())],
            )
            .to_string(),
            Self::NoExpiryClaim => "No expiry claim".into(),
        }
    }
}

impl Token {
    fn parse(text: &str) -> Result<Self, String> {
        // Bound the work independently of the Secret's maximum object size.
        if text.len() > 256 * 1024 {
            return Err("Token is too large to inspect. Use Raw value.".into());
        }
        let segments = text.trim().split('.').collect::<Vec<_>>();
        if segments.len() != 3 || segments.iter().any(|segment| segment.is_empty()) {
            return Err("Expected a JWT with three non-empty segments. Use Raw value to inspect this token.".into());
        }
        let header = decode(segments[0], "header")?;
        let claims = decode(segments[1], "payload")?;
        let expires = numeric_date(&claims, "exp")?;
        let not_before = numeric_date(&claims, "nbf")?;
        numeric_date(&claims, "iat")?;
        Ok(Self {
            header,
            claims,
            expires,
            not_before,
        })
    }

    pub fn timing(&self, now: i64) -> Timing {
        if self.expires.is_some_and(|expires| expires <= now) {
            Timing::Expired
        } else if self.not_before.is_some_and(|not_before| not_before > now) {
            Timing::NotYetActive
        } else if let Some(expires) = self.expires {
            Timing::ExpiresIn(expires.saturating_sub(now))
        } else {
            Timing::NoExpiryClaim
        }
    }

    pub fn fields(&self) -> Vec<(String, Option<String>)> {
        let claim = |key: &str| string(self.claims.get(key));
        let nested = |path: &str| string(self.claims.pointer(path));
        let identity = |old: &str, path: &str| claim(old).or_else(|| nested(path));
        let audience = match self.claims.get("aud") {
            Some(Value::String(value)) => Some(value.clone()),
            Some(Value::Array(values)) => Some(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        };
        let mut fields = vec![
            ("Subject".into(), claim("sub")),
            ("Issuer".into(), claim("iss")),
            ("Audience".into(), audience),
            ("Token ID".into(), claim("jti")),
            (
                "Service account".into(),
                identity(
                    "kubernetes.io/serviceaccount/service-account.name",
                    "/kubernetes.io/serviceaccount/name",
                ),
            ),
            (
                "Service account UID".into(),
                identity(
                    "kubernetes.io/serviceaccount/service-account.uid",
                    "/kubernetes.io/serviceaccount/uid",
                ),
            ),
            (
                "Namespace".into(),
                identity(
                    "kubernetes.io/serviceaccount/namespace",
                    "/kubernetes.io/namespace",
                ),
            ),
            (
                "Secret".into(),
                claim("kubernetes.io/serviceaccount/secret.name"),
            ),
            ("Bound Pod".into(), nested("/kubernetes.io/pod/name")),
            ("Bound Pod UID".into(), nested("/kubernetes.io/pod/uid")),
            ("Bound Node".into(), nested("/kubernetes.io/node/name")),
            ("Bound Node UID".into(), nested("/kubernetes.io/node/uid")),
            ("Algorithm (header)".into(), string(self.header.get("alg"))),
            ("Key ID (header)".into(), string(self.header.get("kid"))),
            (
                "Issued at".into(),
                self.claims.get("iat").and_then(Value::as_i64).map(date),
            ),
            ("Not before".into(), self.not_before.map(date)),
            ("Expires at".into(), self.expires.map(date)),
        ];
        fields.retain(|(_, value)| value.as_ref().is_some_and(|value| !value.is_empty()));
        fields
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn date(seconds: i64) -> String {
    // parse() checks the supported timestamp range before presentation.
    Timestamp::from_second(seconds)
        .map(|time| time.to_string())
        .unwrap_or_else(|_| format!("{seconds} (Unix seconds)"))
}

fn numeric_date(claims: &Value, key: &str) -> Result<Option<i64>, String> {
    let Some(value) = claims.get(key) else {
        return Ok(None);
    };
    let seconds = value
        .as_i64()
        .ok_or_else(|| format!("The {key} claim is not an integer timestamp."))?;
    Timestamp::from_second(seconds)
        .map_err(|_| format!("The {key} claim is outside the supported timestamp range."))?;
    Ok(Some(seconds))
}

fn decode(segment: &str, name: &str) -> Result<Value, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .or_else(|_| URL_SAFE.decode(segment))
        .map_err(|_| format!("JWT {name} is not valid base64url."))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| format!("JWT {name} is not valid JSON."))?;
    if !value.is_object() {
        return Err(format!("JWT {name} must be a JSON object."));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jwt(header: Value, claims: Value) -> String {
        format!(
            "{}.{}.not-a-verified-signature",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    #[test]
    fn legacy_identity_and_absent_expiry_are_inspected_without_claiming_validity() {
        let token = Token::parse(&jwt(json!({"alg":"RS256", "kid":"test-key"}), json!({
            "iss":"kubernetes/serviceaccount", "sub":"system:serviceaccount:default:worker",
            "kubernetes.io/serviceaccount/namespace":"default", "kubernetes.io/serviceaccount/service-account.name":"worker",
            "kubernetes.io/serviceaccount/service-account.uid":"account-uid", "kubernetes.io/serviceaccount/secret.name":"worker-token"
        }))).unwrap();
        assert_eq!(token.timing(1_700_000_000), Timing::NoExpiryClaim);
        assert_eq!(token.timing(1_700_000_000).label(), "No expiry claim");
        let fields: std::collections::BTreeMap<_, _> = token.fields().into_iter().collect();
        assert_eq!(fields["Service account"].as_deref(), Some("worker"));
        assert_eq!(
            fields["Service account UID"].as_deref(),
            Some("account-uid")
        );
        assert_eq!(fields["Namespace"].as_deref(), Some("default"));
        assert_eq!(fields["Secret"].as_deref(), Some("worker-token"));
        assert!(!fields.contains_key("Expires at"));
    }

    #[test]
    fn projected_claims_include_audiences_bound_objects_and_time_boundaries() {
        let token = Token::parse(&jwt(json!({"alg":"RS256"}), json!({
            "aud":["https://cluster.example", "internal"], "iss":"https://issuer.example", "sub":"system:serviceaccount:default:worker",
            "iat":1_700_000_000, "nbf":1_700_000_010, "exp":1_700_000_100,
            "kubernetes.io":{"namespace":"default", "serviceaccount":{"name":"worker", "uid":"sa-id"},
                "pod":{"name":"job-pod", "uid":"pod-id"}, "node":{"name":"node", "uid":"node-id"}}
        }))).unwrap();
        assert_eq!(token.timing(1_700_000_009), Timing::NotYetActive);
        assert_eq!(token.timing(1_700_000_010), Timing::ExpiresIn(90));
        assert_eq!(token.timing(1_700_000_100), Timing::Expired);
        let fields: std::collections::BTreeMap<_, _> = token.fields().into_iter().collect();
        assert_eq!(
            fields["Audience"].as_deref(),
            Some("https://cluster.example\ninternal")
        );
        assert_eq!(fields["Service account"].as_deref(), Some("worker"));
        assert_eq!(fields["Bound Pod UID"].as_deref(), Some("pod-id"));
        assert!(fields["Expires at"].as_deref().unwrap().ends_with('Z'));
    }

    #[test]
    fn malformed_tokens_and_timestamps_have_redacted_diagnostics() {
        let private = "private-token-material";
        let bad = [
            private.into(),
            format!("{}.e30.signature", URL_SAFE_NO_PAD.encode(private)),
            jwt(json!([]), json!({})),
            jwt(json!({}), json!([])),
            jwt(json!({}), json!({"exp":private})),
            jwt(json!({}), json!({"exp":i64::MAX})),
            jwt(json!({}), json!({"nbf":false})),
            "x".repeat(256 * 1024 + 1),
        ];
        for text in bad {
            let error = Token::parse(&text)
                .err()
                .expect("malformed token must be described");
            assert!(!error.contains(private));
            assert!(!error.contains(&text));
        }
    }

    #[test]
    fn padded_segments_and_string_audience_are_supported() {
        let header = URL_SAFE.encode(b"{\"alg\":\"RS256\"}");
        let payload = URL_SAFE.encode(b"{\"aud\":\"audience\"}");
        let token = Token::parse(&format!(" {header}.{payload}.signature \n")).unwrap();
        assert!(
            token
                .fields()
                .iter()
                .any(|(name, value)| name == "Audience" && value.as_deref() == Some("audience"))
        );
    }

    #[test]
    fn standard_keys_have_specific_inspectors_and_other_keys_stay_raw() {
        assert!(inspect("password", "private").is_none());
        assert!(
            matches!(inspect("namespace", "default\n"), Some(Inspection::Namespace(value)) if value == "default")
        );
        let pem = include_str!("testdata/tls.crt");
        let Some(Inspection::Certificate(Ok(chain))) = inspect("ca.crt", &format!("{pem}\n{pem}"))
        else {
            panic!("CA bundle was not parsed")
        };
        assert_eq!(chain.len(), 2);
        assert!(matches!(
            inspect("ca.crt", "not a certificate"),
            Some(Inspection::Certificate(Err(_)))
        ));
    }
}
