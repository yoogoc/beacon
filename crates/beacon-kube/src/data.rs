//! The `data` of a ConfigMap or a Secret.
//!
//! Both hold a flat map of key to value, and both encode it differently: a
//! ConfigMap's `data` is plain text and its `binaryData` is base64, while
//! every value of a Secret is base64 whether it is text or not. Editing one
//! through the YAML pane therefore means base64 in your hands, which is not
//! editing it at all.
//!
//! So this reads the map back into what the value actually is, and says which
//! field and encoding it has to go back into.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use kube::api::DynamicObject;

/// Which field of the object a key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// `data`: plain on a ConfigMap, base64 on a Secret.
    Data,
    /// `binaryData`, which only a ConfigMap has, and which is always base64.
    BinaryData,
}

impl Field {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::BinaryData => "binaryData",
        }
    }
}

/// One key of a ConfigMap or Secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub field: Field,
    /// The value as text. `None` when the bytes are not UTF-8 -- a TLS key, a
    /// keystore -- which is the case an editor must not offer to edit,
    /// because saving would corrupt it.
    pub text: Option<String>,
    /// How big the value is, decoded. Worth showing either way, and the only
    /// thing there is to show when it is not text.
    pub bytes: usize,
}

/// Reads a ConfigMap's or Secret's data, decoded, sorted by key.
///
/// `secret` says which encoding `data` is in, and is the caller's to pass
/// rather than something to read off the object. An object that came from a
/// watch has no `apiVersion` or `kind` on it -- the list they arrived in
/// carried those, once, for all of them -- so sniffing `types` here silently
/// decided every Secret was a ConfigMap and showed base64 as if it were text.
///
/// Sorted because the API's order is not one, and a list that reshuffles
/// between refreshes is unusable.
pub fn read(object: &DynamicObject, secret: bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    for field in [Field::Data, Field::BinaryData] {
        let Some(map) = object.data.get(field.name()).and_then(|it| it.as_object()) else {
            continue;
        };

        for (key, value) in map {
            let Some(value) = value.as_str() else {
                // A non-string under `data` is malformed; the YAML pane is
                // where somebody should go and look at it.
                continue;
            };

            // Everything is base64 except a ConfigMap's `data`.
            let encoded = secret || field == Field::BinaryData;
            let bytes = if encoded {
                match STANDARD.decode(value) {
                    Ok(bytes) => bytes,
                    // Base64 that does not decode is not something to guess
                    // at: show its length and leave it alone.
                    Err(_) => {
                        entries.push(Entry {
                            key: key.clone(),
                            field,
                            text: None,
                            bytes: value.len(),
                        });
                        continue;
                    }
                }
            } else {
                value.as_bytes().to_vec()
            };

            entries.push(Entry {
                key: key.clone(),
                field,
                bytes: bytes.len(),
                text: String::from_utf8(bytes).ok(),
            });
        }
    }

    entries.sort_by(|left, right| left.key.cmp(&right.key));
    entries
}

/// Turns edited text back into what belongs in the object.
pub fn encode(field: Field, secret: bool, text: &str) -> String {
    if secret || field == Field::BinaryData {
        STANDARD.encode(text)
    } else {
        text.to_string()
    }
}

/// Whether this kind keeps data worth editing a key at a time.
pub fn is_keyed(group: &str, kind: &str) -> bool {
    group.is_empty() && matches!(kind, "ConfigMap" | "Secret")
}

#[cfg(test)]
mod tests {
    use super::{Field, encode, is_keyed, read};
    use kube::api::DynamicObject;

    fn object(kind: &str, data: serde_json::Value) -> DynamicObject {
        let resource = kube::api::ApiResource::from_gvk_with_plural(
            &kube::core::GroupVersionKind::gvk("", "v1", kind),
            &format!("{}s", kind.to_lowercase()),
        );
        DynamicObject::new("thing", &resource).data(data)
    }

    #[test]
    fn a_config_map_is_read_as_it_is_written() {
        let entries = read(
            &object(
                "ConfigMap",
                serde_json::json!({ "data": { "b.conf": "two", "a.conf": "one" } }),
            ),
            false,
        );

        assert_eq!(
            entries.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            ["a.conf", "b.conf"],
            "sorted, because the API's order is not one"
        );
        assert_eq!(entries[0].text.as_deref(), Some("one"));
        assert_eq!(entries[0].bytes, 3);
        assert_eq!(entries[0].field, Field::Data);
    }

    /// The whole reason this module exists: a Secret read through YAML is
    /// base64 and cannot be edited by a person.
    #[test]
    fn a_secret_is_decoded() {
        let entries = read(
            &object(
                "Secret",
                serde_json::json!({ "data": { "password": "aHVudGVyMg==" } }),
            ),
            true,
        );
        assert_eq!(entries[0].text.as_deref(), Some("hunter2"));
        assert_eq!(entries[0].bytes, 7);
    }

    /// A ConfigMap's binaryData is base64 even though its data is not.
    #[test]
    fn binary_data_is_decoded_and_kept_apart() {
        let entries = read(
            &object(
                "ConfigMap",
                serde_json::json!({
                    "data": { "a": "plain" },
                    "binaryData": { "z": "aGVsbG8=" }
                }),
            ),
            false,
        );
        assert_eq!(entries[0].field, Field::Data);
        assert_eq!(entries[1].field, Field::BinaryData);
        assert_eq!(entries[1].text.as_deref(), Some("hello"));
    }

    /// Bytes that are not text have no business in a text box: saving one
    /// would write back whatever the editor made of them.
    #[test]
    fn a_value_that_is_not_text_has_no_text() {
        let entries = read(
            // 0xFF is not valid UTF-8 anywhere.
            &object(
                "Secret",
                serde_json::json!({ "data": { "tls.key": "/w==" } }),
            ),
            true,
        );
        assert_eq!(entries[0].text, None);
        assert_eq!(entries[0].bytes, 1);
    }

    #[test]
    fn base64_that_does_not_decode_is_left_alone() {
        let entries = read(
            &object(
                "Secret",
                serde_json::json!({ "data": { "broken": "not base64!" } }),
            ),
            true,
        );
        assert_eq!(entries[0].text, None);
    }

    /// The bug a real cluster found: objects from a watch carry no `kind`,
    /// so anything that sniffed the object for it read every Secret as plain
    /// text and showed base64 in the editor.
    #[test]
    fn the_caller_decides_the_encoding_not_the_object() {
        let mut secret = object(
            "Secret",
            serde_json::json!({ "data": { "p": "aHVudGVyMg==" } }),
        );
        secret.types = None;

        assert_eq!(read(&secret, true)[0].text.as_deref(), Some("hunter2"));
    }

    #[test]
    fn encoding_is_the_inverse_of_reading() {
        assert_eq!(encode(Field::Data, true, "hunter2"), "aHVudGVyMg==");
        assert_eq!(encode(Field::Data, false, "plain"), "plain");
        assert_eq!(encode(Field::BinaryData, false, "hello"), "aGVsbG8=");
    }

    #[test]
    fn only_config_maps_and_secrets_are_keyed() {
        assert!(is_keyed("", "ConfigMap"));
        assert!(is_keyed("", "Secret"));
        assert!(!is_keyed("", "Pod"));
        assert!(
            !is_keyed("example.com", "Secret"),
            "a CRD that borrowed the name"
        );
    }
}
