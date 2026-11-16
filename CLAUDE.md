# Crypto AutoPay — Synthèse technique pour Claude

> Document de contexte pour reprendre le projet rapidement.
> Mis à jour : 2026-11-16

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
│   │   ├── src/seed_gen.rs       # Seed 25 mots : génération (EN), décodage EN + FR
│   │   ├── src/wordlist_en.rs    # Liste officielle Monero anglaise (préfixe 3)
│   │   ├── src/wordlist_fr.rs    # Liste officielle Monero française (préfixe 4)
│   │   ├── src/signatures.rs     # Ed25519 (ed25519-dalek)
│   │   └── src/standard.rs       # AutoPayCodeV1 parse/encode, BLAKE3 checksum
│   └── autopay-rpc/              # Réseau : PaymentAdapter trait + Monero + Canary
│       ├── src/adapter.rs        # Trait PaymentAdapter (async)
│       ├── src/monero.rs         # MoneroAdapter (wallet-rpc JSON-RPC)
│       └── src/canary.rs         # CanaryChecker : fetch + verify signed JSON
├── src-tauri/
│   ├── src/lib.rs                # Commandes Tauri, AppState, scheduler, session wallet-rpc
│   ├── src/rpc_manager.rs        # Sidecar autopay-wallet-rpc (spawn, JSON-RPC)
│   ├── src/unattended.rs         # Paiements écran verrouillé : trousseau de l'OS
│   ├── src/boot_log.rs           # startup.log + page de secours si l'UI ne s'affiche pas
│   ├── src/unlock_guard.rs       # Délais après mots de passe faux
│   ├── src/payment_policy.rs     # Plafonds de paiement (policy.vault)
│   ├── src/main.rs               # Entry point
│   └── tauri.conf.json           # 1050×550px, title "Crypto AutoPay"
└── src-frontend/
    ├── src/
    │   ├── main.ts               # Entry, theme init
    │   ├── App.svelte            # Router : welcome / onboarding / restore / unlock / dashboard
    │   ├── app.css               # Variables CSS thème light/dark + data-network bitcoin
    │   └── lib/
    │       ├── i18n.js           # Store derived, 4 langues FR/EN/DE/ES
    │       ├── theme.js          # Store writable light/dark, localStorage
    │       ├── network.js        # Store selectedNetwork (monero/bitcoin), NETWORKS[]
    │       ├── Welcome.svelte    # Écran 0 : logo, animation, 3 steps, CTA, lang+theme
    │       ├── Onboarding.svelte # Création (mot de passe / seed / vérif) et restauration (mot de passe / 25 mots + date)
    │       ├── Unlock.svelte     # Déverrouillage + panneau « Mot de passe oublié » (restaurer avec 25 mots)
    │       ├── Dashboard.svelte  # Sidebar + pages : Compte (wallets + relevé), abonnements, find, settings, howto
    │       ├── SubscriptionList.svelte # Cartes : statut, NEXT (masqué si suspendu), badge wallet
    │       ├── SpendingForecast.svelte # Trésorerie prévisionnelle (graphique SVG, get_spending_forecast)
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

### Clés et mots de passe (principe central)

Deux mots de passe distincts :

- **Mot de passe maître** (tapé par l'utilisateur, ≥ 12 caractères) : déchiffre `seed.vault` (Argon2id m=128MB t=3 p=1 + AES-256-GCM) et dérive `history_key` (Argon2id + `history.salt`), qui chiffre `history.vault`, `policy.vault` et `wallet_pw.vault`. **Il n'ouvre jamais le wallet Monero.**
- **Mot de passe du wallet** : 32 octets aléatoires en hex, générés par l'app, stockés chiffrés dans `wallet_pw.vault`. C'est lui qui ouvre le fichier wallet dans wallet-rpc. Les anciens wallets (ouverts avec le mot de passe maître) sont migrés à la prochaine ouverture via `change_wallet_password` : le fichier est écrit d'abord, supprimé si le changement échoue ; si le mot de passe aléatoire n'ouvre pas le wallet, le mot de passe maître est essayé (migration interrompue).

En mémoire (`AppState`) : `session_password` (maître, seulement interface déverrouillée), `wallet_password`, `history_key`, tous effacés via `zeroize`.

**Paiements sans surveillance** (Paramètres > Démarrage, activé par défaut) : `{wallet_password, history_key}` est rangé dans le trousseau de l'OS (`keyring` : Secret Service / Keychain / Credential Manager ; service `com.libre-market.autopay`, compte `unattended-payments`). Au lancement (session ouverte, autostart), `start_unattended` le relit, vérifie qu'il déchiffre bien `wallet_pw.vault`, et démarre les paiements sans afficher l'interface. Le mot de passe maître n'y est jamais stocké. Désactiver le réglage, restaurer ou réinitialiser le wallet supprime l'entrée.

**Verrouillage** :
- `lock_ui` (cadenas, auto-lock 3 min) : efface `session_password`, les paiements continuent. Seed, historique, relevé, plafonds et réglages sensibles redemandent le mot de passe.
- `lock_wallet` : arrêt complet (tous les secrets effacés, wallet-rpc stoppé). Utilisé par « Réessayer » de la fenêtre binaire manquant.

### Flux wallet (état actuel)

- **Créer** : Welcome → mot de passe → seed générée (EN) → vérification de mots → `create_wallet(seed, password, restoreFrom: null)` → Unlock.
- **Restaurer** : Welcome « Restaurer avec mes 25 mots », Unlock « Mot de passe oublié » ou le diagnostic « Seed invalide » du Dashboard (qui verrouille l'écran et ouvre ce même panneau) → mot de passe → 25 mots (EN ou FR, casse indifférente, préfixes acceptés) + mois/année de création ou « je ne sais pas » → `create_wallet(seed, password, restoreFrom)`. Le backend normalise la seed (mots complets), écrit `<wallet>.restore_height` (`monero_height_at`, marge ~30 jours ; 1 si date inconnue) ; le premier déverrouillage restaure depuis cette hauteur. Depuis Unlock, `reset_wallet` archive d'abord les fichiers liés au wallet dans `archive/<YYYYmmdd-HHMMSS>/` (rien n'est supprimé).
- **Codes d'erreur** `create_wallet` : `PASSWORD_TOO_SHORT`, `SEED_WORD_COUNT:n`, `SEED_INVALID:…`, `WALLET_EXISTS`.
- **Compte** (1er menu) : wallets + relevé bancaire (`get_account_statement` : entrées, sorties, en attente, échecs via `get_transfers`, libellés par abonnement depuis l'historique).

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

### Appels Tauri
```js
import { invoke } from "@tauri-apps/api/core";
await invoke("create_wallet", { seed, password, restoreFrom }); // restoreFrom: null (création) | ts | 0 (date inconnue)
await invoke("unlock_wallet", { password });
await invoke("lock_ui");      // écran verrouillé, paiements actifs
await invoke("reset_wallet"); // archive les fichiers du wallet avant restauration
```
`tauriInvoke` (Dashboard) avale les erreurs ; utiliser `invokeStrict` quand l'erreur doit remonter.

---

## Wordlist Monero (25 mots)

Algorithme Electrum de Monero (`crates/autopay-crypto/src/seed_gen.rs`) :
- 32 octets → 24 mots (3 mots par bloc de 4 octets, base 1626) + 25e mot = checksum (`crc32` des préfixes des 24 mots, modulo 24 → mot répété).
- Préfixe unique : 3 lettres en anglais, 4 en français. Les mots sont reconnus par leur préfixe.
- Génération : anglais uniquement. Restauration : anglais ou français, langue détectée (un mot français peut aussi matcher un préfixe anglais : la langue n'est retenue que si le checksum passe). Décodage FR validé contre `monero-wallet-rpc` (clé de dépense identique).
- wallet-rpc détecte lui-même la langue au `restore_deterministic_wallet`.

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
- `cargo tauri dev` affiche une vieille fenêtre / un vieux `startup.log` → une autre instance tourne déjà (plugin single-instance : la nouvelle se contente de la mettre au premier plan). `pkill -f target/debug/monero-autopay` (ou l'app installée) avant de relancer. Les builds debug n'enregistrent pas l'autostart, mais une ancienne entrée `~/.config/autostart/Crypto AutoPay.desktop` peut rester.
- `/tmp` est un tmpfs de 100 Mo presque plein : faire les essais dans `~/.cache`.
- Tests navigateur sans Tauri : mock `__TAURI_INTERNALS__` (page temporaire chargeant un `zz-mock.js` à la place de `boot-guard.js`, Vite sur un autre port) ; `unlock_wallet` doit renvoyer `{address, balance_xmr}` (plus de contournement dev du mot de passe), `get_payment_history` `{entries: []}` sinon le Dashboard se fige. Supprimer les fichiers mock ensuite.
- Prévisions de dépenses : une seule source, `spending_forecast` (Rust) — bannière de rechargement, alerte `BALANCE_LOW`, graphique `SpendingForecast.svelte`. Ne pas refaire d'extrapolation « montant × fréquence » côté frontend.
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

- **Logs techniques** : tiroir en RAM (max 500 entrées) + fichier `logs/autopay.log` (0600, rétention glissante configurable). Sanitisation automatique (adresses Monero/Bitcoin, montants redactés). Uniquement identifiant interne + date : **aucun montant, aucune adresse**.
- Les "logs d'envoi" (opt-in localStorage) ont été supprimés.
- Section "Journaux" dans Paramètres : filtrable info/warn/error, effaçable.

## Liens externes dans Tauri

Utiliser `openExternal(url)` dans Dashboard.svelte qui tente `@tauri-apps/plugin-shell` `open()` puis fallback `window.open`. Plugin initialisé dans `src-tauri/src/lib.rs`, capability `shell:allow-open` dans `capabilities/default.json`.

## Fix deep link même URL (réactivité Svelte)

Dans `App.svelte` : avant de set `pendingDeepLinkCode = url`, reset à `null` + `await tick()` pour forcer Svelte à détecter le changement même si l'URL est identique.
Un lien reçu sur Welcome (pas encore de wallet) reste en attente : il s'ouvre une fois le wallet créé et déverrouillé (pas de saut vers Unlock sans vault).
