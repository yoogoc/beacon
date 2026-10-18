use std::{collections::BTreeMap, env, fs, path::PathBuf};

fn parameters(value: &str) -> std::collections::BTreeSet<&str> {
    value
        .split("%{")
        .skip(1)
        .map(|part| {
            part.split_once('}')
                .expect("unclosed translation parameter")
                .0
        })
        .collect()
}

fn main() {
    println!("cargo:rerun-if-changed=locales");
    let mut messages = BTreeMap::new();
    let mut keys = BTreeMap::new();
    let mut files: Vec<_> = fs::read_dir("locales")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    for file in files
        .into_iter()
        .filter(|file| file.extension().is_some_and(|ext| ext == "json"))
    {
        let catalog: serde_json::Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        for (key, translations) in catalog.as_object().unwrap() {
            if key == "_version" {
                continue;
            }
            let en = translations["en"].as_str().unwrap();
            let zh = translations["zh-CN"].as_str().unwrap();
            assert!(!en.is_empty() && !zh.is_empty(), "empty translation: {key}");
            assert_eq!(
                parameters(en),
                parameters(zh),
                "translation parameters differ: {key}"
            );
            assert!(
                keys.insert(key.clone(), en.to_owned()).is_none(),
                "duplicate translation key: {key}"
            );
            assert!(
                messages.insert(en.to_owned(), key.clone()).is_none(),
                "duplicate source message: {en}"
            );
        }
    }
    // Legacy view models keep their canonical English labels. Only the display
    // boundary uses this index; persisted IDs and Kubernetes values stay intact.
    let index = format!(
        "pub const MESSAGES: &[(&str, &str)] = &{:?};",
        messages.into_iter().collect::<Vec<_>>()
    );
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("messages.rs"),
        index,
    )
    .unwrap();
}
