# Crypto AutoPay — TODO & Roadmap

> **Titre logiciel** : Crypto AutoPay (fenêtre OS) / Monero AutoPay (défaut, modifiable)
> **Dernière mise à jour** : 2026-10-23

---

## Dépendances — notes de version

- **`tauri-plugin-deep-link`** : pinné à `=2.3.0` car la v2.6.2 ne compile pas (bug #upstream : `ini::ParseOption` manque les champs `enabled_indented_mutiline_value` + `enabled_preserve_key_leading_whitespace`).
  - Surveiller les releases sur https://github.com/tauri-apps/plugins-workspace
  - Quand le bug sera corrigé : retirer le `=` dans `Cargo.toml` et faire `cargo update -p tauri-plugin-deep-link`
  - → Signaler sur GitHub : https://github.com/tauri-apps/plugins-workspace/issues

---

## Domaine / URL officielle
- [x] URL provisoire : `autopay.libre-market.com`
- [ ] Héberger le canal de mise à jour signé sur ce domaine
- [ ] Page landing minimaliste avec lien de téléchargement + empreinte GPG

## Site web / Générateur de code (`web/`)
- [x] Dossier `web/` — Vite + Svelte, partage `SubscriptionCodeEditor` avec le wallet via alias `$lib`
- [ ] Déployer sur `autopay.libre-market.com`
- [ ] Génération QR code dans le générateur (`qr-creator` lib)
- [ ] Aperçu animé du code généré (PaymentAnimation réutilisé)
- [ ] Page `/spec/v1` qui sert `SPEC.md` en HTML
- [ ] DNS TXT `_autopay.autopay.libre-market.com` → découverte marchande

---

## Phase 0 — Fondations & Standard ✅ (partiellement)

### Standard crypto-autopay:1: ✅
- [x] Rédiger la spec complète du standard `crypto-autopay:1:` (SPEC.md)
- [x] Champ `token` + `token_contract` pour tokens sur EVM/Solana/Tron
- [x] Champ `repeat_every` pour récurrences pluriannuelles (2 ans, 3 ans…)
- [x] `number_of_payments` : limitation d'occurrences avec compteur wallet-side
- [x] Section post-quantique (ML-DSA-65 / Dilithium) dans la spec
- [x] Section agents IA (terms_url, meta_url, ai_tags, rate limit) dans la spec
- [x] Découverte DNS (`_autopay.merchant.com TXT`)
- [x] URI scheme + MIME type documentés
- [x] Implémenter le parser Rust (lit aussi `monero-request:1:` pour compatibilité)
- [x] Valider le schéma JSON avec serde
- [x] Implémenter la vérification de signature Ed25519 sur le payload
- [x] Implémenter le checksum BLAKE3
- [x] Champs avancés : pay_window, occurrence_note_url, cancel, max_conversion_fee_pct, proportional_amount
- [ ] Tests unitaires du parser (round-trip encode/decode)
- [ ] Publier la spec sur le site

### Architecture Tauri 2 ✅
- [x] Init projet Tauri 2 (Rust backend + Svelte frontend)
- [x] Workspace Cargo multi-crates : autopay-core, autopay-crypto, autopay-rpc, src-tauri
- [x] Frontend Svelte 4 + Vite 5 (sans SvelteKit)
- [ ] CI/CD : build multiplateforme (Windows x64, macOS arm64+x64) via GitHub Actions
- [ ] Signing des binaires

### Frontend UX ✅
- [x] Thème clair warm par défaut (grand public)
- [x] Dark mode toggle (persistant localStorage)
- [x] i18n : FR/EN/DE/ES auto-détecté + sélecteur globe
- [x] Sélecteur de réseau (Monero / Bitcoin) dans le titre
- [x] Écran de bienvenue avec animation de paiement (J-3/J-2/J-1, cycle Le Média Libre ↔ VPN)
- [x] Onboarding 3 étapes avec vérification du mot de seed (style Monero GUI)
- [x] Bouton Précédent sur chaque étape
- [x] Mode Restauration (import seed existante, retour vers Welcome)
- [x] Dashboard avec sidebar, page Abonnements, Wallets, Historique, Archivés
- [x] Bannière "Prochain rechargement de portefeuille" avec calcul automatique
- [x] Page Wallets avec nom/description/barre de remplissage/estimation rechargement
- [x] Abonnements : badge wallet source, date NEXT (masquée si suspendu), canary status
- [x] Page "Trouver des souscriptions" avec démos grisées IVPN/Mullvad
- [x] Page "Comment ça marche" avec animation
- [x] Paramètres : langue, thème, fuseau (UTC+XX:XX), réseau, notifications/alertes (5 toggles)
- [x] Fenêtre 1050×550px, scrollable, responsive mobile

---

## Phase 1 — V1 Core (Monero uniquement) 🚧 EN COURS

### Seed Vault & Wallet Management 🚧
- [ ] **[NEXT]** Commande Tauri `generate_seed()` → vraie seed 25 mots Monero (wordlist intégrée)
- [ ] **[NEXT]** Commande Tauri `create_wallet(seed, password)` → SeedVault chiffré (Argon2id + AES-GCM)
- [ ] **[NEXT]** Commande Tauri `unlock_wallet(password)` → déchiffre seed, ouvre wallet-rpc
- [ ] Persistance wallet dans app data dir (pas dans le bundle)
- [ ] Commande `check_wallet_exists()` → switch Welcome / Dashboard au démarrage
- [ ] Support multi-wallets avec master password unique (style Keepass)
- [ ] Warning automatique si solde wallet ≥ 1.5 XMR (orange) ou ≥ 2 XMR (rouge/bloquant)
- [ ] Zeroization mémoire après usage (Zeroizing<String>)

### Monero Wallet RPC Adapter 🚧
- [ ] **[NEXT]** Spawn du processus monero-wallet-rpc local depuis Tauri
- [ ] Client JSON-RPC Rust (méthodes : get_balance, get_address, transfer, open_wallet) ✅ codebase
- [ ] Gestion des subaddresses (une par abonnement côté récepteur)
- [ ] Téléchargement optionnel du binaire monero-wallet-rpc si absent
- [ ] Mode remote node (connexion à un noeud Monero tiers ou local)

### Scheduler & Subscription Engine 🚧
- [ ] Moteur de scheduling (tokio-cron-scheduler) ✅ codebase
- [ ] Persistance locale des abonnements (SQLite via rusqlite) ✅ codebase
- [ ] Commande Tauri `add_subscription(code)` → parse monero-autopay:1:
- [ ] Commande Tauri `get_subscriptions()` → liste pour le frontend
- [ ] Commande Tauri `pause_subscription(id)` / `resume_subscription(id)`
- [ ] Commande Tauri `tick()` → appelée toutes les X minutes
- [ ] États : active, paused, past_due, cancelled, expired, suspended ✅ models
- [ ] Report automatique si app offline au moment du paiement ✅ scheduler

### Canary System 🚧
- [ ] Fetch périodique du canary JSON (signé Ed25519) ✅ codebase
- [ ] Commande Tauri `check_canary(subscription_id)`
- [ ] Suspension automatique si canary invalide/expiré ✅ scheduler

### UI ↔ Backend Bridge
- [ ] Remplacer les mocks frontend par de vrais appels `invoke()` Tauri
- [ ] Gestion des erreurs Tauri → affichage dans l'UI
- [ ] Spinner / état de chargement pour les opérations longues

---

## Phase 2 — V2 Améliorations

### Occurrence Note
- [ ] Fetch `occurrence_note_url` avant chaque paiement
- [ ] Si `new_amount` differ → dialog de confirmation bloquante
- [ ] Signature optionnelle de la note par le créateur

### UX avancée
- [ ] Import via QR code (scanner avec Tauri camera API ou parser URL)
- [ ] Export/import de la config des abonnements (backup chiffré)
- [ ] Historique des paiements persistant avec détails tx
- [ ] Notifications système OS (via tauri-plugin-notification)
- [ ] Mode systray discret (icône, tooltip solde, pause/resume rapide)
- [ ] Page d'archivage avec historique complet
- [ ] Gestion fuseau horaire pour affichage vs scheduling (UTC interne)

### Standard
- [ ] Soumettre une PR au repo monero-request de lukeprofits pour compatibilité
- [ ] Publier la spec monero-autopay:1: sur un domaine stable (IPFS + Tor)

---

## Phase 3 — V3 Multi-réseau (Bitcoin + Lightning)

### Architecture multi-réseau (décision de design)
> **Principe** : chaque réseau est complètement cloisonné — wallets séparés, abonnements séparés.
> Un abonnement Monero n'est pas disponible en Bitcoin (sauf si le marchand l'a publié dans les deux formats).
> Un utilisateur peut avoir 0, 1 ou N wallets par réseau, avec des soldes totalement indépendants.

**Vue souhaitée :**
- Onglet par réseau (Monero | Bitcoin | Tout)
- L'onglet "Tout" affiche TOUS les abonnements, groupés par réseau avec leur couleur
- Au démarrage, l'app ouvre sur le réseau qui a le plus d'activité récente
- Les wallets sont tagués par réseau — un wallet Monero ne peut pas payer un abonnement Bitcoin

- [ ] Adapter PaymentAdapter trait pour Bitcoin on-chain (PSBT)
- [ ] Adapter PaymentAdapter trait pour Lightning (BOLT 12 quand stable)
- [ ] UI : onglets par réseau (Monero | Bitcoin | Tout) dans la sidebar ou le header
- [ ] Wallets taggés par réseau (`network: "monero" | "bitcoin"`)
- [ ] Abonnements filtrés par réseau actif, onglet "Tout" = vue combinée
- [ ] Canary multi-réseau
- [ ] Tests intégration multi-adapter

---

## Décisions en attente
- [ ] Nom de domaine définitif
- [ ] Licence des icônes/artwork
- [ ] Stratégie de distribution (GitHub Releases + site auto-hébergé vs. stores)
- [ ] Monero wordlist à inclure : EN officielle (1626 mots) ? + FR/DE/ES ?

---

## Notes techniques
- Seed mock actuelle (25 mots) = placeholder, sera remplacé par vraie génération Rust
- `color-scheme: light dark` sur les `<select>` pour dark mode natif
- `data-network="bitcoin"` sur `<html>` pour override couleur accent (#f7931a)
- PaymentAnimation.svelte : cycle 7s CSS pur, labels reactifs JS
- i18n via store Svelte `derived(currentLang, ...)`, localStorage persist
