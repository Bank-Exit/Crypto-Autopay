# Android Build — Crypto AutoPay

## État actuel : bloquant architectural

L'application utilise `monero-wallet-rpc` (binaire natif x86_64) lancé en sous-processus.
**Ce modèle est incompatible avec Android** : les apps Android ne peuvent pas spawner de processus arbitraires.

---

## Option A — Build Android "shell" (UI seule, sans RPC)

Pour tester l'interface sur Android rapidement, sans fonctionnalités Monero réelles :

### Prérequis (Linux/macOS)

```bash
# 1. Java 17+
sudo apt install openjdk-17-jdk   # Ubuntu

# 2. Android SDK via command-line tools
# Télécharger : https://developer.android.com/studio#command-line-tools-only
mkdir -p ~/Android/Sdk/cmdline-tools/latest
unzip commandlinetools-linux-*.zip -d ~/Android/Sdk/cmdline-tools/latest

# 3. Variables d'environnement (~/.bashrc ou ~/.zshrc)
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/$(ls $ANDROID_HOME/ndk)"
export PATH="$PATH:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools"

# 4. SDK + NDK
sdkmanager "platforms;android-34" "build-tools;34.0.0" "ndk;26.1.10909125" "platform-tools"

# 5. Targets Rust pour Android
rustup target add \
  aarch64-linux-android \
  armv7-linux-androideabi \
  i686-linux-android \
  x86_64-linux-android

# 6. cargo-ndk
cargo install cargo-ndk
```

### Init et build

```bash
# Initialiser le projet Android (une seule fois)
cargo tauri android init

# Build debug APK
cargo tauri android build --debug

# Build release APK
cargo tauri android build

# APK généré dans :
# gen/android/app/build/outputs/apk/
```

---

## Option B — Architecture Android complète (recommandée)

Pour une vraie app Android fonctionnelle, il faut remplacer `monero-wallet-rpc` par :

### B1. Connexion à un nœud distant (monero-lws ou Feather)

```
┌─────────────────────────┐        ┌───────────────────────┐
│   App Android (Tauri)   │  HTTP  │  Nœud Monero distant  │
│   Svelte UI + Rust      │◄──────►│  monero-wallet-rpc    │
│   keypair local         │        │  ou LWS / mymonero    │
└─────────────────────────┘        └───────────────────────┘
```

- Le wallet signing reste local (clé privée ne sort pas)
- Le RPC tourne sur un serveur de confiance (auto-hébergé recommandé)
- Implémentation : changer `RpcManager` pour faire des requêtes HTTP réseau

### B2. monero-rs (Rust natif, pas de binaire externe)

La crate [`monero-rs`](https://github.com/monero-rs/monero-rs) implémente le protocole Monero en Rust pur.
Compilable pour ARM Android. Plus complexe mais 100% autonome.

```toml
# Cargo.toml
[dependencies]
monero = { version = "0.21", features = ["serde"] }
```

---

## GitHub Actions — Android CI (futur)

```yaml
# À ajouter dans .github/workflows/release.yml quand prêt

  build-android:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-java@v4
        with:
          distribution: 'temurin'
          java-version: '17'
      - name: Setup Android SDK
        uses: android-actions/setup-android@v3
      - name: Install NDK
        run: sdkmanager "ndk;26.1.10909125"
      - name: Install Rust Android targets
        run: rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
      - name: Install frontend deps
        working-directory: src-frontend
        run: npm ci
      - name: Build Android APK
        run: cargo tauri android build
        env:
          NDK_HOME: ${{ env.ANDROID_NDK_HOME }}
```

---

## Recommandation

| Priorité | Action | Effort |
|---|---|---|
| 1 | Builds Windows + macOS via GitHub Actions | ✅ Déjà configuré |
| 2 | APK Android "shell" pour tester l'UI | ~2h setup |
| 3 | Android fonctionnel via nœud distant | ~3-5j dev |
| 4 | Android autonome (monero-rs) | ~2-4 semaines dev |
