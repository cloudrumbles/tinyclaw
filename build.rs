fn main() {
    // Read .env at compile time and bake each key=value into the binary.
    // This means `option_env!("TELEGRAM_BOT_TOKEN")` etc. resolve at compile time.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let env_path = std::path::Path::new(&manifest_dir).join(".env");
    if let Ok(content) = std::fs::read_to_string(&env_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                println!("cargo:rustc-env={}={}", key.trim(), value.trim());
            }
        }
    }
    println!("cargo:rerun-if-changed=.env");
}
