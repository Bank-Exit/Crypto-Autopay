# Monero AutoPay — Choix et Décisions Initiales

> Synthèse de la session de conception (oct. 2026).
> Ce document est la référence pour les arbitrages effectués et leurs justifications.

---

## 1. Nom du projet

**→ Monero AutoPay**

- Explicite, "AutoPay" = point de vue du payeur qui automatise
- "Monero" = crypto par défaut, grand public le comprend
- Pas de confusion avec les deep links existants (`monero:`, `xmr:`)
- "not affiliated with The Monero Project" à mentionner dans le README
- La marque "Monero" (Monero Distribution Corporation, jan 2024) est déposée pour protéger l'éco, pas bloquer les outils tiers

---

## 2. Préfixe du standard de code

| Option | Verdict |
|--------|---------|
| `monero-request:2:` | ❌ point de vue récepteur, fork implicite du standard lukeprofits |
| `xmr-pay:2:` | ⚠️ compact mais confus avec `xmr:` deep link paiement simple existant |
| `monero-autopay:1:` | ✅ explicite, sans ambiguïté, cohérent avec le nom du projet |

- Deep link app : `monero-autopay://`
- V3 Bitcoin : `bitcoin-autopay:1:`
- Compatibilité lecture (import only) : `monero-request:1:` (lukeprofits)

---

## 3. Stack technique

| Choix | Justification |
|-------|---------------|
| **Tauri 2** (Rust + WebView système) | Léger (~10 MB), Rust memory-safe, gestion RPC native, Android en V2 |
| vs Electron | Plus lourd (~180 MB), Chromium bundlé, Node.js moins adapté au wallet RPC |
| **Svelte** (frontend) | Léger, réactif, pas de Virtual DOM |
| **SQLite** (rusqlite bundled) | Persistance locale, pas de serveur, futur upgrade sqlcipher |
| **Argon2id** (KDF) | Plus récent que Scrypt, résistant aux GPU/ASIC, recommandé OWASP 2024 |
| **AES-256-GCM** | Chiffrement authentifié, détecte la corruption |
| **Ed25519** | Signatures du standard et du canary |
| **BLAKE3** | Checksum rapide et moderne |

---

## 4. Architecture des crates Rust

```
autopay-crypto   → SeedVault, Ed25519, standard monero-autopay:1:
autopay-rpc      → trait PaymentAdapter, MoneroAdapter (wallet-rpc), CanaryChecker
autopay-core     → Subscription, PaymentRecord, SQLite, SubscriptionScheduler
src-tauri        → Tauri app, commandes IPC, tray icon, deep links
```

Le trait `PaymentAdapter` est **réseau-agnostique** : V3 = ajouter `BitcoinAdapter` sans toucher au scheduler.

---

## 5. Modèle non-custodial

- Spend key **jamais transmise** à un service tiers
- Monero wallet-rpc spawné **localement** par Tauri
- Seed chiffrée Argon2id + AES-256-GCM **sur disque**
- En mémoire : `Zeroizing<String>` (wiped à la désallocation)
- Multi-wallets : chacun max **2 XMR** (hot wallet cap)
- Warning orange à 1.5 XMR, rouge bloquant à 2 XMR

---

## 6. Canary — signal de vie du créateur

Le créateur publie un JSON signé Ed25519 à `canary_url` :
```json
{
  "active": true,
  "wallet_address": "4AB...xyz",
  "valid_until": "2026-12-31T00:00:00Z",
  "pubkey": "ed25519:hex",
  "signature": "ed25519:sig-de-tout-le-reste"
}
```

AutoPay suspend l'abonnement si :
- `wallet_address` ≠ `sellers_wallet` du code → **usurpation probable**
- Signature invalide → **canary falsifié**
- `valid_until` expiré → **créateur inactif**
- `active: false` → **arrêt explicite**

Flag manuel utilisateur "Marquer comme suspect" disponible en UI.

---

## 7. Standard monero-autopay:1: — champs définis

Format : `monero-autopay:1:<base64url(gzip(canonical-json))>`

Champs obligatoires : `v`, `custom_label`, `payment_network`, `sellers_wallet`, `currency`, `amount`, `start_date`, `schedule`, `number_of_payments`

Champs optionnels clés : `description`, `canary_url`, `merchant_pubkey`, `signature`, `checksum`, `webhook_url`, `expires_at`, `grace_period_hours`, `trial_days`, `min_confirmations`, `metadata`

**Idées récupérées de BOLT 12** à intégrer (v1.1) :
- `pay_window_before_hours` / `pay_window_after_hours` : fenêtre d'acceptation du paiement
- `proportional_amount` : montant réduit si paiement en retard dans la fenêtre
- `period_index` dans les webhooks : numéro de période séquentiel partagé payeur↔créateur

**Idées récupérées de lnrecc / LNbits Allowance** :
- Cron étendu avec champ secondes : `0 30 12 * * *`
- Date clamp mensuel : jan-31 → fév-28/29 (à gérer dans le scheduler)
- Retry backoff exponentiel : 1, 2, 4, 8, 16, 32, 60 min (vs retry x1 initial)

---

## 8. Relation avec l'écosystème existant

| Projet | Rôle | Relation |
|--------|------|----------|
| **Monero Subscriptions Wallet** (lukeprofits) | Même cas d'usage, Python, quasi-abandonné (GUI license) | Inspiration, pas de PR |
| **Monero Payment Request Standard** (lukeprofits) | Standard v1, parsé en import-only | Compatible lecture |
| **GhostBill** | Côté créateur (Cas B), self-hostable, AGPL | Interopérable via webhooks |
| **LibreMarket** | Marketplace Monero native, potentiel partenaire | À contacter |
| **Libre-Monero API** | Détection paiements côté créateur | Utilisée par les créateurs, pas par AutoPay |
| **Monero Blaster** | Seed vault pattern (Scrypt+AES-256-GCM) | Pattern repris en Argon2id+AES-256-GCM |

---

## 9. Roadmap V1 → V3

- **V1** : Desktop Windows + macOS, Monero uniquement, scheduler local, canary, deep link
- **V2** : Android AOSP, notifications push, historique complet, annuaire partagé
- **V3** : `BitcoinAdapter` (Bitcoin Core RPC / Electrum), standard `bitcoin-autopay:1:`

---

## 10. Questions ouvertes

- [ ] Domaine officiel (update channel, site landing)
- [ ] Licence finale (GPL-3.0 ou AGPL-3.0)
- [ ] Partenariat LibreMarket (support@libre-market.com)
- [ ] Cron étendu avec secondes : adopter le format lnrecc ou rester POSIX 5 champs ?
- [ ] Retry backoff : remplacer le retry x1 par backoff exponentiel dans le scheduler
- [ ] Date clamp mensuel dans compute_next_occurrence()
