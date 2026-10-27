fn main() {
    tauri_build::build();

    // games.json is kept readable for hand edits; the binary gets it minified.
    println!("cargo:rerun-if-changed=src/activity/games.json");
    let games = std::fs::read_to_string("src/activity/games.json").expect("games.json");
    let games: serde_json::Value =
        serde_json::from_str(&games).expect("games.json is not valid JSON");
    let out = std::path::Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("games.min.json");
    std::fs::write(out, games.to_string()).expect("write games.min.json");

    // A release build with the updater key still at its placeholder would ship an app whose
    // updates can never verify. Fail loudly instead.
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-env-changed=MUDRIX_ALLOW_PLACEHOLDER_UPDATER_KEY");
    let release = std::env::var("PROFILE").is_ok_and(|p| p == "release");
    let allowed = std::env::var_os("MUDRIX_ALLOW_PLACEHOLDER_UPDATER_KEY").is_some();
    if release && !allowed {
        let conf = std::fs::read_to_string("tauri.conf.json").unwrap_or_default();
        if conf.contains("REPLACE_WITH") {
            panic!(
                "plugins.updater.pubkey in src-tauri/tauri.conf.json is still the placeholder. \
Run `npx tauri signer generate` and paste the public key there (see RELEASING.md). \
For a throwaway local build set MUDRIX_ALLOW_PLACEHOLDER_UPDATER_KEY=1."
            );
        }
    }
}
