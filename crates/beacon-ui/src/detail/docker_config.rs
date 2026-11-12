use super::*;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    auths: Option<BTreeMap<String, Credentials>>,
}

#[derive(Deserialize)]
struct Credentials {
    auth: Option<String>,
    username: Option<String>,
    email: Option<String>,
    password: Option<String>,
    identitytoken: Option<String>,
    registrytoken: Option<String>,
}

pub(super) struct Registry {
    name: String,
    username: Option<String>,
    email: Option<String>,
    authentication: &'static str,
}

pub(super) fn inspect(object: &DynamicObject) -> Result<Vec<Registry>, &'static str> {
    let encoded = object
        .data
        .pointer("/data/.dockerconfigjson")
        .and_then(Value::as_str)
        .ok_or("This Docker config Secret has no .dockerconfigjson entry.")?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ".dockerconfigjson is not valid base64.")?;
    // Deserialization errors can quote input values, including credentials.
    let config: Config = serde_json::from_slice(&bytes)
        .map_err(|_| ".dockerconfigjson is not a valid Docker configuration.")?;
    Ok(config
        .auths
        .unwrap_or_default()
        .into_iter()
        .map(|(name, entry)| Registry::from_credentials(name, entry))
        .collect())
}

impl Registry {
    fn from_credentials(name: String, entry: Credentials) -> Self {
        let mut registry = Self {
            name,
            username: entry.username.filter(|name| !name.is_empty()),
            email: entry.email.filter(|email| !email.is_empty()),
            authentication: "None",
        };
        let has_password = if let Some(auth) = entry.auth.filter(|auth| !auth.is_empty()) {
            let Some((username, has_password)) = decode_auth(&auth) else {
                registry.username = None;
                registry.authentication = "Invalid auth field";
                return registry;
            };
            registry.username = (!username.is_empty()).then_some(username);
            has_password
        } else {
            entry.password.is_some_and(|value| !value.is_empty())
        };
        let has_token = [entry.identitytoken, entry.registrytoken]
            .into_iter()
            .flatten()
            .any(|value| !value.is_empty());
        registry.authentication = match (has_password, has_token) {
            (true, true) => "Password and token",
            (true, false) => "Password",
            (false, true) => "Token",
            (false, false) => "None",
        };
        registry
    }
}

fn decode_auth(auth: &str) -> Option<(String, bool)> {
    let decoded = STANDARD
        .decode(auth)
        .or_else(|_| STANDARD_NO_PAD.decode(auth))
        .ok()?;
    let mut parts = decoded.splitn(2, |&byte| byte == b':');
    let username = std::str::from_utf8(parts.next()?).ok()?.to_owned();
    let has_password = !parts.next()?.is_empty();
    Some((username, has_password))
}

impl DetailView {
    pub(super) fn render_docker_config(
        &self,
        registries: &Result<Vec<Registry>, &'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let heading = self
            .heading("Docker registry configuration", cx)
            .into_any_element();
        let (message, tone) = match registries {
            Ok(registries) if !registries.is_empty() => {
                return v_flex()
                    .gap_3()
                    .child(heading)
                    .children(registries.iter().enumerate().map(|(index, registry)| {
                        let mut fields = vec![
                            ("Registry".into(), Some(registry.name.clone())),
                            ("Username".into(), registry.username.clone()),
                            (
                                "Credentials".into(),
                                Some(crate::i18n::text(registry.authentication).to_string()),
                            ),
                        ];
                        if let Some(email) = &registry.email {
                            fields.push(("Email".into(), Some(email.clone())));
                        }
                        div()
                            .id(("docker-registry", index))
                            .when(index > 0, |row| {
                                row.pt_3().border_t_1().border_color(cx.theme().border)
                            })
                            .child(self.field_rows(&fields, cx))
                    }))
                    .into_any_element();
            }
            Ok(_) => ("No registry entries", cx.theme().muted_foreground),
            Err(message) => (*message, cx.theme().tone(Tone::Critical)),
        };
        v_flex()
            .gap_1()
            .child(heading)
            .child(
                div()
                    .text_sm()
                    .text_color(tone)
                    .child(crate::i18n::text(message)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beacon_kube::{ApiResource, GroupVersionKind};
    use serde_json::json;

    #[::core::prelude::v1::test]
    fn reads_explicit_and_encoded_credentials_without_retaining_secrets() {
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk("", "v1", "Secret"));
        let config = json!({"auths": {
            "a.example": {"username": "fallback", "auth": STANDARD_NO_PAD.encode("reader:password:with-colons")},
            "b.example": {"username": "user", "email": "user@example.test", "password": "password"},
            "c.example": {"identitytoken": "example-token"},
            "d.example": {"auth": "invalid auth"}
        }});
        let secret = DynamicObject::new("registry", &resource).data(json!({
            "data": {".dockerconfigjson": STANDARD.encode(config.to_string())}
        }));
        let registries = inspect(&secret).unwrap();
        let details: Vec<_> = registries
            .iter()
            .map(|registry| {
                (
                    registry.name.as_str(),
                    registry.username.as_deref(),
                    registry.email.as_deref(),
                    registry.authentication,
                )
            })
            .collect();
        assert_eq!(
            details,
            [
                ("a.example", Some("reader"), None, "Password"),
                (
                    "b.example",
                    Some("user"),
                    Some("user@example.test"),
                    "Password"
                ),
                ("c.example", None, None, "Token"),
                ("d.example", None, None, "Invalid auth field"),
            ]
        );
    }
}
