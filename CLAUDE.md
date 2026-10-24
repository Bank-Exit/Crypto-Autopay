# Crypto AutoPay — Synthèse technique pour Claude

> Document de contexte pour reprendre le projet rapidement.
> Mis à jour : 2026-10-24

---

## Vue d'ensemble

**Crypto AutoPay** est une application desktop non-custodiale (Tauri 2) qui envoie automatiquement des paiements récurrents en Monero (ou Bitcoin V3) à des créateurs, médias, et services.

- **Non-custodial** : la spend key ne quitte jamais l'appareil
- **Hot wallet de sécurité** : max 2 XMR par wallet, rechargement manuel tous les 6-12 mois
- **Canary system** : détecte les abonnements discontinués ou QR usurpés
- **Standard** : `monero-autopay:1:` (propre, base64url(gzip(canonical-json))) + compat `monero-request:1:`

---

## Structure du projet

```
/home/node/Documents/Gitlab/Monero_AutoPay_Subscription/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── autopay-core/             # Logique métier : scheduler, subscriptions, SQLite
│   │   ├── src/models.rs         # Subscription, PaymentRecord, OccurrenceNote
│   │   ├── src/scheduler.rs      # tick() : canary check → payment
│   │   ├── src/db.rs             # rusqlite CRUD
│   │   └── src/schema.sql        # Tables subscriptions + payments
│   ├── autopay-crypto/           # Crypto : seed vault, signatures, standard
│   │   ├── src/seed_vault.rs     # Argon2id + AES-256-GCM
│   │   ├── src/signatures.rs     # Ed25519 (ed25519-dalek)
│   │   └── src/standard.rs       # AutoPayCodeV1 parse/encode, BLAKE3 checksum
│   └── autopay-rpc/              # Réseau : PaymentAdapter trait + Monero + Canary
│       ├── src/adapter.rs        # Trait PaymentAdapter (async)
│       ├── src/monero.rs         # MoneroAdapter (wallet-rpc JSON-RPC)
│       └── src/canary.rs         # CanaryChecker : fetch + verify signed JSON
├── src-tauri/
│   ├── src/lib.rs                # Tauri app setup (commandes à ajouter ici)
│   ├── src/main.rs               # Entry point
│   └── tauri.conf.json           # 1050×550px, title "Crypto AutoPay"
└── src-frontend/
    ├── src/
    │   ├── main.ts               # Entry, theme init
    │   ├── App.svelte            # Router : welcome / onboarding / restore / dashboard
    │   ├── app.css               # Variables CSS thème light/dark + data-network bitcoin
    │   └── lib/
    │       ├── i18n.js           # Store derived, 4 langues FR/EN/DE/ES
    │       ├── theme.js          # Store writable light/dark, localStorage
    │       ├── network.js        # Store selectedNetwork (monero/bitcoin), NETWORKS[]
    │       ├── Welcome.svelte    # Écran 0 : logo, animation, 3 steps, CTA, lang+theme
    │       ├── Onboarding.svelte # Étapes 1-3 : password / seed (25 mots) / backup+verify
    │       ├── Dashboard.svelte  # Layout sidebar + pages : subs, wallets, history, find, settings, howto
    │       ├── SubscriptionList.svelte # Cartes : statut, NEXT (masqué si suspendu), badge wallet
    │       └── PaymentAnimation.svelte # SVG animé 7s : dot A→B, ring J-3/J-2/J-1, cycle 2 sujets
    └── package.json              # Svelte 4, Vite 5, svelte-preprocess
```

---

## Décisions clés

### Standard monero-autopay:1:
- Format : `monero-autopay:1:<base64url(gzip(canonical-json))>`
- JSON canonique : BTreeMap trié récursivement
- Signature Ed25519 sur canonical JSON sans le champ `signature`
- Checksum BLAKE3 du payload entier
- Champs : amount, currency, schedule (cron), merchant_pubkey, canary_url, occurrence_note_url, pay_window_before/after_secs, proportional_amount, cancel, max_conversion_fee_pct

### Canary system
- Créateur publie un JSON signé à `canary_url`
- AutoPay vérifie : signature valide, adresse wallet match, `valid_until` non expiré, `active: true`
- Si invalide → suspension du paiement + alerte utilisateur
- Statuts : Ok, NotConfigured, Unreachable, SignatureInvalid, AddressMismatch, Expired, InactiveByCreator

### Seed vault
- Argon2id (m=128MB, t=3, p=1) pour dériver la clé depuis le mot de passe maître
- AES-256-GCM pour chiffrer la seed
- `Zeroizing<String>` pour effacer la seed de la mémoire

### PaymentAdapter trait (network-agnostic)
```rust
#[async_trait]
pub trait PaymentAdapter: Send + Sync {
    fn network_id(&self) -> &str;           // "monero", "bitcoin"
    async fn balance(&self) -> Result<Balance, AdapterError>;
    async fn send(&self, dest: &str, amount_atomic: u64, memo: Option<&str>) -> Result<TxId, AdapterError>;
    fn to_atomic(&self, amount: &str) -> Result<u64, AdapterError>;
    fn from_atomic(&self, amount: u64) -> String;
}
```

### Hot wallet safety
- 1 XMR piconero = 1_000_000_000_000
- Warning à ≥ 1.5 XMR, blocage à ≥ 2.0 XMR
- Affichage barre de remplissage par wallet dans l'UI

---

## Frontend — points importants

### i18n
```js
import { t, currentLang, LANGUAGES } from "./lib/i18n.js";
// Utilisation : $t.clé
// Changement : currentLang.set("en")
```

### Réseau actif
```js
import { selectedNetwork, currentNetwork } from "./lib/network.js";
// $currentNetwork.label → "Monero" ou "Bitcoin"
// $currentNetwork.symbol → "XMR" ou "BTC"
// data-network sur <html> → override CSS --accent (#f7931a pour bitcoin)
```

### Thème
```js
import { theme, toggleTheme } from "./lib/theme.js";
// $theme === "light" | "dark"
// data-theme sur <html> → CSS variables light/dark
```

### Appels Tauri (à venir)
```js
import { invoke } from "@tauri-apps/api/core";
const seed = await invoke("generate_seed");
await invoke("create_wallet", { seed, password });
const balance = await invoke("get_balance");
```

---

## Backend — prochaines commandes Tauri à implémenter

```rust
// Dans src-tauri/src/lib.rs

#[tauri::command]
async fn generate_seed() -> Result<String, String> {
    // 25 mots Monero via wordlist intégrée + getrandom
}

#[tauri::command]
async fn create_wallet(seed: String, password: String) -> Result<(), String> {
    // SeedVault::encrypt(seed, password) → fichier dans app_data_dir()
}

#[tauri::command]
async fn unlock_wallet(password: String) -> Result<(), String> {
    // SeedVault::decrypt(password) → spawn monero-wallet-rpc
}

#[tauri::command]
async fn get_balance() -> Result<f64, String> {
    // MoneroAdapter::balance() → XMR
}

#[tauri::command]
async fn check_wallet_exists() -> bool {
    // Vérifie si seed.vault existe dans app_data_dir()
}

#[tauri::command]
async fn get_subscriptions() -> Result<Vec<Subscription>, String> {
    // DB::get_all_subscriptions()
}

#[tauri::command]
async fn add_subscription(code: String) -> Result<Subscription, String> {
    // parse_code(&code) → insert DB
}
```

---

## Wordlist Monero (25 mots)

La seed Monero utilise l'algorithme Electrum :
- 32 bytes aléatoires → 24 indices dans la wordlist (1626 mots anglais)
- 25ème mot = checksum (3 premiers chars de chaque mot → crc32)
- Wordlist officielle EN : https://github.com/monero-project/monero/blob/master/src/mnemonics/english.h
- À intégrer dans autopay-crypto/src/wordlist_en.rs (array de 1626 &str)

---

## Commandes utiles

```bash
# Dev frontend seul
cd src-frontend && npm run dev

# Dev Tauri complet
cargo tauri dev

# Build prod
cargo tauri build

# Build frontend seul
cd src-frontend && npm run build
```

---

## Captures d'écran (état 2026-10-23)

1. **Welcome screen** — thème warm, animation J-3/VPN, Monero ▾ cliquable
2. **Backup step** — vérification du mot n°X de la seed (style Monero GUI)
3. **Dashboard** — sidebar, bannière rechargement, abonnements avec badge wallet

---

## Pièges connus

- `lang="ts"` dans les `<script>` Svelte 4 + svelte-preprocess → imports Svelte stripés au bundle → utiliser JS pur dans les .svelte
- `verbatimModuleSyntax: true` en tsconfig → remplacer par `isolatedModules: true`
- `protocol-asset` feature Tauri 2 → ne pas inclure (cause build error)
- `hex = "1"` n'existe pas sur crates.io → utiliser `hex = "0.4"`
- Port 5173 déjà utilisé → `lsof -ti:5173 | xargs kill -9`
- `Zeroizing<[u8;32]>` → utiliser `.as_ref()` pour obtenir `&[u8]`

---

## Architecture multi-réseau (décision de design — V3)

### Principe fondamental
Chaque réseau crypto est **complètement cloisonné** :
- Un wallet Monero ≠ un wallet Bitcoin (balances, seeds, RPCs totalement séparés)
- Un abonnement lié à Monero ne peut pas être payé en Bitcoin (sauf si le marchand publie deux codes séparés)
- Un utilisateur peut avoir 0, 1 ou N wallets par réseau

### UI souhaitée (à implémenter en V3)
- Onglets dans le dashboard : **Monero | Bitcoin | Tout**
- L'onglet "Tout" = vue combinée, groupée par réseau avec couleurs (orange XMR, orange BTC)
- Le sélecteur de réseau global (Welcome screen "Monero ▾") devient un FILTRE de vue, pas une isolation
- Les wallets ont un champ `network: "monero" | "bitcoin"` en base
- Les abonnements héritent du réseau de leur code AutoPay (champ `network` du payload)

### Ce qui existe déjà
- `data-network` sur `<html>` → override CSS accent color (fonctionnel)
- `NETWORKS_EXTENDED` et `APP_NETWORKS` dans `autopay-encode.js`
- `PaymentAdapter` trait côté Rust (network-agnostic par design)
- `SubscriptionCodeEditor` : sélecteur Monero/Bitcoin uniquement (pas d'ETH, pas de Tron)

### Pourquoi pas d'Ethereum ?
Ethereum ne sera pas intégré : pas de RPC natif prévu, l'écosystème EVM est jugé trop complexe
et risqué pour ce cas d'usage. Monero (confidentialité) et Bitcoin (liquidité) suffisent.

---

## Protocole crypto-autopay:1: — Champs importants ajoutés

- `token` / `token_contract` : pour USDT-Tron vs USDT-Solana (surtout utile côté web, pas app)
- `repeat_every` : multiplicateur cron (tous les 2 ans = schedule annuel + repeat_every: 2)
- `trial_days` / `trial_end` : période d'essai gratuite avant premier paiement
- `tax_rate_pct` : taux TVA informationnel (affiché, n'augmente pas le montant)
- `split_payments` : tableau [{address, share_pct, label}] pour commissions multi-destinataires
- `_ext` : namespace libre pour extensions tierces sans bumper de version
- `signature_scheme` : "ed25519" (défaut) ou "ml-dsa-65" (post-quantique)

Cron format : **6 champs Quartz** (sec min hour dom month dow) comme tokio-cron-scheduler.

---

## Site web (web/)

Dossier `web/` = site Vite+Svelte pour autopay.libre-market.com.
Partage `SubscriptionCodeEditor.svelte` via alias `$lib → ../src-frontend/src/lib`.
Pour lancer : `cd web && npm run dev`


---

## Architecture UX — 3 flux d'entrée (mis à jour 2026-10-24)

| Bouton / Entry point | Modal / Page | Mode SCE | Événement | Résultat |
|---|---|---|---|---|
| ＋ Nouvel abonnement | Modal | `mode="new"` | `new-sub` | Ajout direct à la liste |
| 📥 Importer un code | Modal | `mode="import"` | `imported` | Ajout avec check doublon |
| 🔗 Créer un code (sidebar) | Pleine page | `mode="create"` | `generated` | Vue partage + préview HTML |
| URI deep link | Modal import | `mode="import"` | `imported` | Ajout avec check doublon |

## AppLogger.js — Sécurité logs

- **Logs techniques** : RAM seulement, max 500 entrées. Sanitisation automatique (adresses Monero/Bitcoin, montants redactés).
- **Logs d'envoi** : opt-in, localStorage, max 1000 entrées. Contiennent UNIQUEMENT : timestamp + subscription_id interne + type d'événement. **Aucun montant, aucune adresse**.
- Section "Journaux" dans Paramètres : filtrable info/warn/error, effaçable.

## Liens externes dans Tauri

Utiliser `openExternal(url)` dans Dashboard.svelte qui tente `@tauri-apps/plugin-shell` `open()` puis fallback `window.open`. Plugin initialisé dans `src-tauri/src/lib.rs`, capability `shell:allow-open` dans `capabilities/default.json`.

## Fix deep link même URL (réactivité Svelte)

Dans `App.svelte` : avant de set `pendingDeepLinkCode = url`, reset à `null` + `await tick()` pour forcer Svelte à détecter le changement même si l'URL est identique.
