pub fn agent_path_allowed(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if crate::workspace::normalize_path(&normalized).is_err() {
        return false;
    }
    let name = normalized.rsplit('/').next().unwrap_or("");
    !(name == ".env"
        || name.starts_with(".env.")
        || matches!(
            name,
            ".npmrc" | ".pypirc" | ".netrc" | "id_rsa" | "id_ed25519" | "credentials.json"
        )
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|suffix| name.ends_with(suffix)))
}
