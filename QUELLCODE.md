# Pedalsteuerung bauen

Voraussetzungen sind macOS mit Xcode Command Line Tools und ein aktuelles stabiles Rust. Die Tauri-CLI muss mindestens die signierte Versionsbindung für Update-Artefakte unterstützen (ab 2.12).

```sh
cargo install tauri-cli --version 2.12.4 --locked
cargo test --manifest-path src-tauri/Cargo.toml --locked
MACOSX_DEPLOYMENT_TARGET=13.0 cargo tauri build --ci --bundles app \
  --config '{"bundle":{"createUpdaterArtifacts":false,"macOS":{"signingIdentity":"-"}}}'
```

Der normale lokale Build liegt unter `src-tauri/target/release/bundle/macos/`. Tauri verarbeitet die Oberfläche und das Video aus `ui/`.

## Release

Offizielle Versionen werden mit dem privaten Tauri-Updateschlüssel signiert. Der öffentliche Schlüssel steht in der App-Konfiguration. Der Release enthält die DMG, das signierte App-Archiv und `latest.json`. Letzteres referenziert das Archiv und enthält die Signatur. Signaturdateien enthalten die signierte App-Version. Ein privater Schlüssel gehört in den lokalen Schlüsselspeicher oder einen geschützten CI-Secret-Speicher.
