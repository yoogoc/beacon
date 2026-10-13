//! Browser destinations use the public listener, never a backend Service port.
use std::net::Ipv6Addr;

use serde_json::Value;
use url::Url;

use crate::web_links::WebLink;

fn entries<'a>(data: &'a Value, path: &str) -> impl Iterator<Item = &'a Value> {
    data.pointer(path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

pub(crate) fn addresses(data: &Value) -> Vec<&str> {
    entries(data, "/status/loadBalancer/ingress")
        .filter_map(|entry| {
            entry
                .get("ip")
                .and_then(Value::as_str)
                .filter(|address| !address.is_empty())
                .or_else(|| entry.get("hostname").and_then(Value::as_str))
        })
        .filter(|address| !address.is_empty())
        .collect()
}

fn tls_host_matches(pattern: &str, host: &str) -> bool {
    pattern.eq_ignore_ascii_case(host)
        || pattern.strip_prefix("*.").is_some_and(|suffix| {
            host.split_once('.')
                .is_some_and(|(label, rest)| !label.is_empty() && rest.eq_ignore_ascii_case(suffix))
        })
}

fn https(data: &Value, host: &str) -> bool {
    entries(data, "/spec/tls").any(|tls| {
        let hosts = tls.get("hosts").and_then(Value::as_array);
        hosts.is_none_or(Vec::is_empty)
            || hosts
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|pattern| tls_host_matches(pattern, host))
    })
}

pub(crate) fn url(data: &Value, host: &str, path: &str) -> Option<String> {
    // A wildcard is a routing pattern, not a browsable hostname.
    if host.is_empty()
        || host.contains(['*', '/', '?', '#', '@', '\\'])
        || host.chars().any(char::is_whitespace)
    {
        return None;
    }
    let authority = if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let scheme = if https(data, host) { "https" } else { "http" };
    let mut url = Url::parse(&format!("{scheme}://{authority}")).ok()?;
    // Set a path explicitly: // in an Ingress path must not replace the host.
    url.set_path(if path.is_empty() { "/" } else { path });
    Some(url.into())
}

pub(crate) fn path_urls(data: &Value, host: Option<&str>, path: &str) -> Vec<String> {
    match host.filter(|host| !host.is_empty()) {
        Some(host) => url(data, host, path).into_iter().collect(),
        None => addresses(data)
            .into_iter()
            .filter_map(|host| url(data, host, path))
            .collect(),
    }
}

pub(crate) fn table_links(data: &Value, header: &str, text: &str) -> Vec<WebLink> {
    let hosts = if header == "Hosts" {
        entries(data, "/spec/rules")
            .filter_map(|rule| rule.get("host").and_then(Value::as_str))
            .collect()
    } else {
        addresses(data)
    };
    let mut cursor = 0;
    let mut links = Vec::new();
    for host in hosts {
        if let Some(start) = text[cursor..].find(host) {
            let start = cursor + start;
            cursor = start + host.len();
            if let Some(url) = url(data, host, "/") {
                links.push(WebLink {
                    range: start..cursor,
                    url,
                });
            }
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::{path_urls, table_links, url};
    use serde_json::json;

    #[test]
    fn each_rule_uses_its_own_tls_host_and_public_port() {
        let data = json!({"spec":{"tls":[{"hosts":["secure.example.com", "*.apps.example.com"]}]}});
        assert_eq!(
            url(&data, "secure.example.com", "/api").as_deref(),
            Some("https://secure.example.com/api")
        );
        assert_eq!(
            url(&data, "plain.example.com", "/").as_deref(),
            Some("http://plain.example.com/")
        );
        assert_eq!(
            url(&data, "web.apps.example.com", "/").as_deref(),
            Some("https://web.apps.example.com/")
        );
        assert_eq!(
            url(&data, "nested.web.apps.example.com", "/").as_deref(),
            Some("http://nested.web.apps.example.com/")
        );
        assert!(url(&data, "*.apps.example.com", "/").is_none());
    }

    #[test]
    fn hostless_paths_use_all_addresses_and_ipv6_is_bracketed() {
        let data = json!({"status":{"loadBalancer":{"ingress":[{"ip":"2001:db8::1"},{"hostname":"lb.example.com"}]}}});
        assert_eq!(
            path_urls(&data, None, "/app"),
            ["http://[2001:db8::1]/app", "http://lb.example.com/app"]
        );
        assert!(path_urls(&json!({}), None, "/app").is_empty());
        assert!(path_urls(&data, Some("*.example.com"), "/app").is_empty());
    }

    #[test]
    fn listener_tls_defaults_and_literal_paths_keep_the_host() {
        let data = json!({"spec":{"tls":[{}]}});
        assert_eq!(
            url(&data, "app.example.com", "").as_deref(),
            Some("https://app.example.com/")
        );
        assert_eq!(
            url(&data, "app.example.com", "//other.example/a").as_deref(),
            Some("https://app.example.com//other.example/a")
        );
        assert_eq!(
            url(&data, "app.example.com", "/hello world/%E4%B8%AD").as_deref(),
            Some("https://app.example.com/hello%20world/%E4%B8%AD")
        );
        assert!(url(&data, "example.com@other.example", "/").is_none());
    }

    #[test]
    fn list_hosts_keep_separate_destinations_without_linking_wildcards() {
        let data = json!({"spec":{"rules":[{"host":"secure.example.com"},{"host":"*.apps.example.com"},{"host":"plain.example.com"}], "tls":[{"hosts":["secure.example.com"]}]}});
        let text = "secure.example.com, *.apps.example.com, plain.example.com";
        let links = table_links(&data, "Hosts", text);
        assert_eq!(links.len(), 2);
        assert_eq!(&text[links[0].range.clone()], "secure.example.com");
        assert_eq!(links[0].url, "https://secure.example.com/");
        assert_eq!(&text[links[1].range.clone()], "plain.example.com");
        assert_eq!(links[1].url, "http://plain.example.com/");
    }
}
