//! Connection settings are per-client; the process environment is never changed.
use kube::config::Kubeconfig;
use serde::{Deserialize, Serialize};
use std::{io::Write, sync::Arc};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "url", rename_all = "snake_case")]
pub enum Proxy {
    #[default]
    System,
    Direct,
    Custom(String),
}

impl Proxy {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Custom(value) = self {
            let url = url::Url::parse(value).map_err(|_| "Enter a valid proxy URL.".to_string())?;
            if !matches!(url.scheme(), "http" | "https" | "socks5")
                || url.host_str().is_none()
                || url.query().is_some()
                || url.fragment().is_some()
                || !matches!(url.path(), "" | "/")
            {
                return Err(
                    "Proxy must be an http://, https:// or socks5:// host and port.".into(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn apply(&self, config: &mut kube::Config) -> Result<(), String> {
        self.validate()?;
        match self {
            Self::System => {}
            Self::Direct => config.proxy_url = None,
            Self::Custom(url) => {
                config.proxy_url = Some(url.parse().map_err(|_| "Invalid proxy URL.".to_string())?)
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_kubeconfig(
        &self,
        config: &mut Kubeconfig,
        context: &str,
    ) -> Result<(), String> {
        if matches!(self, Self::System) {
            return Ok(());
        }
        let entry = config
            .contexts
            .iter()
            .find(|c| c.name == context)
            .and_then(|c| c.context.as_ref())
            .ok_or("Context is missing from kubeconfig.")?;
        let cluster_name = entry.cluster.clone();
        let user = entry.user.clone();
        let cluster = config
            .clusters
            .iter_mut()
            .find(|c| c.name == cluster_name)
            .and_then(|c| c.cluster.as_mut())
            .ok_or("Cluster is missing from kubeconfig.")?;
        cluster.proxy_url = match self {
            Self::Custom(url) => Some(url.clone()),
            _ => None,
        };
        // Credential plugins receive a private environment, just like kubectl.
        if let Some(user) = user
            && let Some(exec) = config
                .auth_infos
                .iter_mut()
                .find(|u| u.name == user)
                .and_then(|u| u.auth_info.as_mut())
                .and_then(|u| u.exec.as_mut())
        {
            let environment = Transport {
                proxy: self.clone(),
                config: None,
            }
            .environment();
            let env = exec.env.get_or_insert_with(Vec::new);
            for (name, value) in environment {
                env.retain(|entry| entry.get("name").map(String::as_str) != Some(name));
                env.push(std::collections::HashMap::from([
                    ("name".into(), name.into()),
                    ("value".into(), value),
                ]));
            }
        }
        Ok(())
    }

    pub(crate) fn http_client(&self) -> Result<reqwest::Client, reqwest::Error> {
        let builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none());
        let builder = match self {
            Self::System => builder,
            Self::Direct => builder.no_proxy(),
            Self::Custom(url) => builder.no_proxy().proxy(reqwest::Proxy::all(url)?),
        };
        builder.build()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "config", rename_all = "snake_case")]
pub enum MetricsSource {
    #[default]
    Kubernetes,
    Prometheus(Prometheus),
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prometheus {
    pub url: String,
    pub bearer_token: String,
    pub pod_cpu: String,
    pub pod_memory: String,
    pub node_cpu: String,
    pub node_memory: String,
}
impl Default for Prometheus {
    fn default() -> Self {
        Self {
            url: String::new(), bearer_token: String::new(),
            pod_cpu: "sum by (namespace, pod) (rate(container_cpu_usage_seconds_total{container!=\"\",container!=\"POD\",pod!=\"\"}[5m]))".into(),
            pod_memory: "sum by (namespace, pod) (container_memory_working_set_bytes{container!=\"\",container!=\"POD\",pod!=\"\"})".into(),
            node_cpu: "sum by (node) (rate(container_cpu_usage_seconds_total{container=\"\",pod=\"\"}[5m]))".into(),
            node_memory: "max by (node) (container_memory_working_set_bytes{container=\"\",pod=\"\"})".into(),
        }
    }
}
impl Prometheus {
    pub fn validate(&self) -> Result<(), String> {
        let url =
            url::Url::parse(&self.url).map_err(|_| "Enter a valid Prometheus URL.".to_string())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "Prometheus must use http:// or https://. Use the token field for authentication."
                    .into(),
            );
        }
        if [
            &self.pod_cpu,
            &self.pod_memory,
            &self.node_cpu,
            &self.node_memory,
        ]
        .iter()
        .any(|q| q.trim().is_empty())
        {
            return Err("All four Prometheus queries are required.".into());
        }
        if self.bearer_token.contains(['\r', '\n']) {
            return Err("The bearer token contains invalid characters.".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionOptions {
    pub proxy: Proxy,
    pub metrics: MetricsSource,
}
impl ConnectionOptions {
    pub fn validate(&self) -> Result<(), String> {
        self.proxy.validate()?;
        if let MetricsSource::Prometheus(config) = &self.metrics {
            config.validate()?;
        }
        Ok(())
    }
}

/// A private temporary copy gives kubectl the same proxy as the GUI, including
/// overriding a kubeconfig proxy. The original kubeconfig is never written.
#[derive(Clone, Default)]
pub(crate) struct Transport {
    pub proxy: Proxy,
    pub config: Option<Arc<tempfile::NamedTempFile>>,
}
impl Transport {
    pub(crate) fn new(mut config: Kubeconfig, context: &str, proxy: Proxy) -> Result<Self, String> {
        if matches!(proxy, Proxy::System) {
            return Ok(Self::default());
        }
        let cluster_name = config
            .contexts
            .iter()
            .find(|c| c.name == context)
            .and_then(|c| c.context.as_ref())
            .map(|c| c.cluster.clone())
            .ok_or("Context is missing from kubeconfig.")?;
        let cluster = config
            .clusters
            .iter_mut()
            .find(|c| c.name == cluster_name)
            .and_then(|c| c.cluster.as_mut())
            .ok_or("Cluster is missing from kubeconfig.")?;
        cluster.proxy_url = match &proxy {
            Proxy::Custom(url) => Some(url.clone()),
            _ => None,
        };
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, &config).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
        Ok(Self {
            proxy,
            config: Some(Arc::new(file)),
        })
    }
    pub(crate) fn environment(&self) -> Vec<(&'static str, String)> {
        match &self.proxy {
            Proxy::System => Vec::new(),
            proxy => {
                let value = match proxy {
                    Proxy::Custom(url) => url.as_str(),
                    _ => "",
                };
                let mut vars: Vec<_> = [
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "all_proxy",
                ]
                .into_iter()
                .map(|name| (name, value.to_string()))
                .collect();
                let no_proxy = if matches!(proxy, Proxy::Direct) {
                    "*"
                } else {
                    ""
                };
                vars.extend([("NO_PROXY", no_proxy.into()), ("no_proxy", no_proxy.into())]);
                vars
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_validation_and_direct_override() {
        for url in [
            "http://127.0.0.1:8080",
            "https://proxy.example:443",
            "socks5://localhost:1080",
        ] {
            assert!(Proxy::Custom(url.into()).validate().is_ok());
        }
        for url in [
            "localhost:8080",
            "ftp://host",
            "http://host/path",
            "http://host?x=y",
        ] {
            assert!(Proxy::Custom(url.into()).validate().is_err());
        }
        let mut config = kube::Config::new("https://cluster".parse().unwrap());
        config.proxy_url = Some("http://old:8080".parse().unwrap());
        Proxy::Direct.apply(&mut config).unwrap();
        assert!(config.proxy_url.is_none());
    }
    #[test]
    fn temporary_kubeconfig_and_child_environment_agree() {
        let config = Kubeconfig::from_yaml("clusters:\n- name: local\n  cluster:\n    server: https://local\n    proxy-url: http://old:8080\ncontexts:\n- name: demo\n  context:\n    cluster: local\n").unwrap();
        let direct = Transport::new(config.clone(), "demo", Proxy::Direct).unwrap();
        let read = Kubeconfig::read_from(direct.config.as_ref().unwrap().path()).unwrap();
        assert!(
            read.clusters[0]
                .cluster
                .as_ref()
                .unwrap()
                .proxy_url
                .is_none()
        );
        assert!(
            direct
                .environment()
                .contains(&("HTTPS_PROXY", String::new()))
        );
        let custom =
            Transport::new(config, "demo", Proxy::Custom("http://new:8080".into())).unwrap();
        let read = Kubeconfig::read_from(custom.config.as_ref().unwrap().path()).unwrap();
        assert_eq!(
            read.clusters[0]
                .cluster
                .as_ref()
                .unwrap()
                .proxy_url
                .as_deref(),
            Some("http://new:8080")
        );
        assert!(custom.environment().contains(&("NO_PROXY", String::new())));
    }

    #[test]
    fn credential_plugin_overrides_are_private_and_replace_duplicate_names() {
        let original = Kubeconfig::from_yaml(
            r#"
clusters:
- name: local
  cluster:
    server: https://local
contexts:
- name: demo
  context:
    cluster: local
    user: auth
users:
- name: auth
  user:
    exec:
      command: credential-helper
      env:
      - name: HTTPS_PROXY
        value: http://old:8080
      - name: KEEP_ME
        value: unchanged
"#,
        )
        .unwrap();
        for proxy in [Proxy::Direct, Proxy::Custom("http://new:8080".into())] {
            let mut copy = original.clone();
            proxy.prepare_kubeconfig(&mut copy, "demo").unwrap();
            let env = copy.auth_infos[0]
                .auth_info
                .as_ref()
                .unwrap()
                .exec
                .as_ref()
                .unwrap()
                .env
                .as_ref()
                .unwrap();
            let expected = Transport {
                proxy,
                config: None,
            }
            .environment();
            for (name, value) in expected {
                let entries: Vec<_> = env.iter().filter(|entry| entry["name"] == name).collect();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0]["value"], value);
            }
            assert!(
                env.iter()
                    .any(|entry| entry["name"] == "KEEP_ME" && entry["value"] == "unchanged")
            );
        }
        assert_eq!(
            original.auth_infos[0]
                .auth_info
                .as_ref()
                .unwrap()
                .exec
                .as_ref()
                .unwrap()
                .env
                .as_ref()
                .unwrap()[0]["value"],
            "http://old:8080"
        );
    }

    #[tokio::test]
    async fn kubernetes_client_uses_the_selected_http_proxy() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            async fn header(stream: &mut tokio::net::TcpStream) -> String {
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(stream.read_u8().await.unwrap());
                    assert!(bytes.len() < 8192);
                }
                String::from_utf8(bytes).unwrap()
            }
            let connect = header(&mut stream).await;
            assert!(
                connect.starts_with("CONNECT cluster.invalid:80 "),
                "{connect}"
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            assert!(header(&mut stream).await.starts_with("GET /version "));
            let body = r#"{"major":"1","minor":"32","gitVersion":"v1.32.0","gitCommit":"test","gitTreeState":"clean","buildDate":"2026-01-01T00:00:00Z","goVersion":"go1.23","compiler":"gc","platform":"test"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let mut config = kube::Config::new("http://cluster.invalid:80".parse().unwrap());
        Proxy::Custom(format!("http://{address}"))
            .apply(&mut config)
            .unwrap();
        let client = kube::Client::try_from(config).unwrap();
        let version = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.apiserver_version(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(version.git_version, "v1.32.0");
        server.await.unwrap();
    }
}
