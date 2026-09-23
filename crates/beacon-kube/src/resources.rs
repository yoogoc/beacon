//! The handful of kinds Beacon names in code.
//!
//! Beacon has no Rust type per resource: a list view is a [`ApiResource`] plus a
//! column set, both of which normally come from discovery. These few are the
//! exception -- they are needed before discovery has run, or they are part of
//! the application's own furniture (the namespace picker), so type-erasing the
//! `k8s-openapi` definition is cheaper and more reliable than hardcoding the
//! group/version/plural strings.

use k8s_openapi::api::core::v1::{Event, Namespace, Pod};
use kube::api::ApiResource;

pub fn pod() -> ApiResource {
    ApiResource::erase::<Pod>(&())
}

pub fn namespace() -> ApiResource {
    ApiResource::erase::<Namespace>(&())
}

pub fn event() -> ApiResource {
    ApiResource::erase::<Event>(&())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type erasure has to produce exactly the strings that build an API path,
    /// because everything downstream treats them as data.
    #[test]
    fn core_kinds_erase_to_the_right_paths() {
        let pod = pod();
        assert_eq!(pod.group, "");
        assert_eq!(pod.version, "v1");
        assert_eq!(pod.api_version, "v1");
        assert_eq!(pod.kind, "Pod");
        assert_eq!(pod.plural, "pods");

        assert_eq!(namespace().plural, "namespaces");
        assert_eq!(event().plural, "events");
    }
}
