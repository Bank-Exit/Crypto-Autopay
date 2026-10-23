# Crypto AutoPay Protocol — Specification v1

> **Protocol identifier:** `crypto-autopay:1:`
> **Hosted at:** https://autopay.libre-market.com/spec/v1
> **Status:** Draft — 2026-10-23
> **Backward compatible with:** `monero-request:1:` (read-only import)

---

## 1. What is it?

**Crypto AutoPay** is an open, non-custodial recurring payment protocol for cryptocurrencies.

It lets a merchant (creator, service, media outlet) publish a single short code — a URL, QR code, or text string — that tells any compatible wallet *"charge me X every month"*, without any middleman, without storing card details, and without trusting the payment processor.

Unlike traditional subscriptions, the **payer stays in full control**: they can pause, cancel, or audit every payment at any time. The merchant has no ability to pull funds without an explicit scheduled authorization.

---

## 2. Quick example — read this first

### As a QR code / link
```
crypto-autopay:1:H4sIAAAAAAAAA6tWKkktLlGyUlIqS04tLk5MTQUAVfAHlhAAAAA=
```

### What it decodes to (human-readable JSON)
```json
{
  "version": 1,
  "network": "monero",
  "custom_label": "Le Média Libre — Soutien mensuel",
  "description": "Accès complet aux archives + newsletter hebdomadaire",
  "destination_address": "4At3X5rvVypTofgmueN9s9QtrzdRe5BueFrskAZi17BoYbhzysozzoMFB6zWnTKdGC6AxEAbEE5czFR3hbEEJbsm4hCeX2S",
  "amount": "0.05",
  "currency": "XMR",
  "schedule": "0 0 9 1 * *",
  "number_of_payments": 0,
  "start_date": "2026-11-01",
  "canary_url": "https://media-libre.fr/.well-known/autopay-canary.json",
  "merchant_pubkey": "a3f2...e8c1",
  "signature": "b7d4...9f2a"
}
```

**What the wallet does:** On the 1st of each month at 09:00 local time, send 0.05 XMR to the merchant's address — after checking the canary signal.

---

## 2b. Vocabulary — network vs token vs currency

These three concepts are distinct and must not be confused:

| Field | Answers | Example |
|-------|---------|---------|
| `network` | *Which blockchain?* | `"ethereum"`, `"tron"`, `"solana"`, `"monero"` |
| `token` | *Which asset on that chain?* | `"USDT"`, `"USDC"`, `null` (= native coin) |
| `token_contract` | *Which exact smart contract?* | `"0xdac17f...ec7"` (ERC-20 address) |
| `currency` | *What unit is `amount` expressed in?* | `"USDT"`, `"EUR"`, `"XMR"` |

### Why the distinction matters

**USDT on Ethereum vs USDT on Tron** are two different tokens on two different chains — entirely separate payment rails. A wallet must refuse to pay on the wrong chain.

```json
// USDT on Ethereum (ERC-20)
{ "network": "ethereum", "token": "USDT",
  "token_contract": "0xdac17f958d2ee523a2206206994597c13d831ec7",
  "currency": "USDT", "amount": "10.00" }

// USDT on Tron (TRC-20)
{ "network": "tron", "token": "USDT",
  "token_contract": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
  "currency": "USDT", "amount": "10.00" }

// USDT on Solana (SPL)
{ "network": "solana", "token": "USDT",
  "token_contract": "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
  "currency": "USDT", "amount": "10.00" }

// Native XMR — no token field needed
{ "network": "monero", "currency": "XMR", "amount": "0.05" }
```

Known networks: `"monero"`, `"bitcoin"`, `"ethereum"`, `"tron"`, `"solana"`, `"polygon"`, `"lightning"`.

---

## 3. Encoding format

```
crypto-autopay:1:<payload>
```

Where `<payload>` = `base64url( gzip( canonical-json ) )`

| Step | Operation | Detail |
|------|-----------|--------|
| 1 | Canonical JSON | Fields sorted alphabetically (recursive BTreeMap), no whitespace |
| 2 | Gzip | RFC 1952, default compression |
| 3 | Base64url | RFC 4648 §5, no padding (`=`) |

The `signature` field (if present) is computed **before** encoding over the canonical JSON **without** the `signature` field itself (i.e., the canonical JSON of all other fields).

### Encoding example (Python)
```python
import gzip, json, base64

code = {
    "version": 1,
    "network": "monero",
    "custom_label": "Le Média Libre — Soutien mensuel",
    # ... all fields ...
}
canonical = json.dumps(dict(sorted(code.items())), separators=(',',':'), ensure_ascii=False)
compressed = gzip.compress(canonical.encode('utf-8'))
payload = base64.urlsafe_b64encode(compressed).rstrip(b'=').decode()
print(f"crypto-autopay:1:{payload}")
```

### Decoding example (Python)
```python
raw = "crypto-autopay:1:H4sIAAAAAAAAA..."
payload = raw.split(":", 2)[2]
padded = payload + "=" * (-len(payload) % 4)
canonical = gzip.decompress(base64.urlsafe_b64decode(padded)).decode('utf-8')
code = json.loads(canonical)
```

---

## 4. Field reference

### 4.1 Required fields

| Field | Type | Example | Description |
|-------|------|---------|-------------|
| `version` | integer | `1` | Protocol version. Must be `1` for this spec. |
| `network` | string | `"monero"` | Payment network: `"monero"` or `"bitcoin"` |
| `custom_label` | string | `"Le Média Libre"` | Short human-readable name shown in the wallet UI |
| `destination_address` | string | `"4At3X5..."` | Recipient address (Monero primary or subaddress; Bitcoin segwit/taproot) |
| `amount` | string | `"0.05"` | Amount in `currency` units. Decimal string, no trailing zeros required. |
| `currency` | string | `"XMR"` | `"XMR"`, `"BTC"`, `"USD"`, `"EUR"` (fiat = converted at payment time) |
| `schedule` | string | `"0 0 9 1 * *"` | Cron expression (UTC) for payment timing |

### 4.2 Optional fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `description` | string | `""` | Longer description shown in wallet details |
| `token` | string | `null` | Token symbol for non-native assets. `null` = native coin (XMR, BTC, ETH…) |
| `token_contract` | string | `null` | Contract address or program ID for the token. Required if `token` is set |
| `repeat_every` | integer | `1` | Execute every Nth cron trigger. `2` with annual cron = every 2 years |
| `number_of_payments` | integer | `0` | Max payments. `0` = unlimited. Wallet auto-archives after reaching this count |
| `payments_made` | integer | `0` | Current count (set by wallet, not merchant). Included in backup/export |
| `start_date` | string | today | ISO 8601 date `YYYY-MM-DD`. First payment not before this date |
| `end_date` | string | `null` | ISO 8601 date. Subscription expires after this date |
| `canary_url` | string | `null` | URL of the canary document (see §6). Highly recommended |
| `merchant_pubkey` | string (hex) | `null` | Public key. See `signature_scheme` for the algorithm |
| `signature` | string (hex) | `null` | Signature over canonical JSON without `signature` field |
| `signature_scheme` | string | `"ed25519"` | `"ed25519"` or `"ml-dsa-65"` (CRYSTALS-Dilithium, quantum-resistant) |
| `occurrence_note_url` | string | `null` | URL fetched before each payment. May announce price changes |
| `pay_window_before_secs` | integer | `3600` | Seconds before scheduled time when payment is acceptable |
| `pay_window_after_secs` | integer | `86400` | Seconds after scheduled time before payment is considered missed |
| `proportional_amount` | boolean | `false` | If true, prorate partial periods |
| `cancel` | boolean | `false` | If true, wallet should cancel and archive this subscription |
| `max_conversion_fee_pct` | float | `2.0` | Max acceptable fee % when converting fiat currency to crypto |
| `webhook_url` | string | `null` | Optional HTTPS URL to ping after each payment (POST with tx id) |
| `terms_url` | string | `null` | URL to the service's terms of use (machine-readable for AI agents) |
| `meta_url` | string | `null` | URL to merchant metadata JSON (name, logo, contact, AI tags) |
| `trial_days` | integer | `0` | Number of free trial days before first charge. `0` = no trial |
| `trial_end` | string | `null` | ISO 8601 date overriding `trial_days` (absolute trial end date) |
| `tax_rate_pct` | float | `0.0` | Informational VAT/tax rate. Added to `amount` to compute gross display. Wallets SHOULD show "X + Y% tax = Z". Does not affect the crypto amount transferred. |
| `split_payments` | array | `null` | Multi-recipient payment split (see §4.4). If present, `destination_address` is ignored. |
| `notes` | string | `""` | Free-text notes for the payer (not displayed to merchant) |
| `_ext` | object | `null` | Extension namespace for custom fields. Wallets MUST ignore unknown keys. Use `"_ext": {"your_app": {...}}` |

### 4.3 Schedule format (6-field cron + repeat_every)

The `schedule` field uses **6-field Quartz-style cron** (seconds prefix), as implemented by `tokio-cron-scheduler` and most modern schedulers:

```
┌────────── second (0-59)
│ ┌──────── minute (0-59)
│ │ ┌────── hour (0-23)
│ │ │ ┌──── day of month (1-31)
│ │ │ │ ┌── month (1-12)
│ │ │ │ │ ┌ day of week (0-7, 0=Sun)
│ │ │ │ │ │
* * * * * *
```

The seconds field allows sub-minute precision (useful for testing, or for distributed wallets that stagger payments to avoid simultaneous RPC load). For human-visible schedules, seconds = `0` is standard.

Standard cron covers up to yearly intervals. For multi-year or irregular periods, use `repeat_every` as a multiplier: the wallet counts cron triggers and only executes every Nth one.

| Goal | `schedule` | `repeat_every` | Result |
|------|-----------|----------------|--------|
| Monthly | `0 0 9 1 * *` | 1 (default) | Every month, 1st at 09:00 |
| Weekly | `0 0 9 * * 1` | 1 | Every Monday at 09:00 |
| Quarterly | `0 0 9 1 */3 *` | 1 | Every 3 months |
| Yearly | `0 0 9 1 1 *` | 1 | Every January 1st |
| **Every 2 years** | `0 0 9 1 1 *` | **2** | Every other January 1st |
| **Every 3 years** | `0 0 9 1 1 *` | **3** | Every 3rd year |
| Every 2 weeks | `0 0 9 * * 1` | 2 | Every other Monday |
| Every 6 months | `0 0 9 1 */6 *` | 1 | June & December 1st |
| Daily at 3:30:15 AM | `15 30 3 * * *` | 1 | Every day, precise second |

**Limited occurrences:** `number_of_payments: 3` = the wallet pays exactly 3 times, then auto-archives the subscription. The wallet stores `payments_made` locally and compares it to `number_of_payments` before each scheduled run. Example: a seasonal 3-month subscription, or a 12-payment annual plan.

```json
// Biennial subscription (every 2 years)
{ "schedule": "0 0 9 1 1 *", "repeat_every": 2, "number_of_payments": 0 }

// 3-payment installment plan (monthly)
{ "schedule": "0 0 9 1 * *", "repeat_every": 1, "number_of_payments": 3 }
```

---

### 4.4 Split payments

The `split_payments` field allows distributing a payment across multiple recipients — useful for platform commissions, shared revenue, or multi-party agreements.

```json
{
  "split_payments": [
    { "address": "4At3X5...", "share_pct": 90, "label": "Creator"       },
    { "address": "4Bk2...",   "share_pct": 10, "label": "Platform fee"  }
  ]
}
```

Rules:
- `share_pct` values MUST sum to exactly 100.
- Each entry has `address` (required), `share_pct` (required, integer), `label` (optional, display only).
- If `split_payments` is present, `destination_address` MUST be absent or empty.
- Each split results in a separate on-chain transaction (Monero doesn't support multi-output to different addresses in the standard wallet-rpc interface). For Bitcoin, a wallet MAY combine them into a single multi-output tx for efficiency.
- Minimum split = 1% (to avoid dust transactions).

```json
// Example: media + platform commission (Monero)
{
  "network": "monero",
  "amount": "0.05",
  "currency": "XMR",
  "split_payments": [
    { "address": "4At3X5...", "share_pct": 95, "label": "Le Média Libre" },
    { "address": "4Xyz...",   "share_pct": 5,  "label": "Libre-Market commission" }
  ]
}
```

The wallet MUST display the split clearly before adding the subscription: "0.0475 XMR → Le Média Libre + 0.0025 XMR → Libre-Market commission".

---

## 5. Signature verification

The `merchant_pubkey` + `signature` pair allows the wallet to verify that the subscription code was created by the merchant (not forged or tampered with).

**Signing algorithm:**
1. Take the JSON object with all fields **except** `signature`
2. Serialize to canonical JSON (BTreeMap sorted, no spaces)
3. Sign with Ed25519 using the merchant's private key
4. Store as hex in `signature`

**Verification (wallet side):**
1. Remove `signature` from the parsed object
2. Re-serialize to canonical JSON
3. Verify Ed25519 signature against `merchant_pubkey`
4. Reject subscription if verification fails

**Checksum (BLAKE3):**
A BLAKE3 checksum of the full canonical JSON payload is embedded to detect accidental corruption:
```json
{ "checksum": "a3f2b9e1..." }
```
This is distinct from the Ed25519 signature (content integrity vs. author authenticity).

---

## 6. Canary document

The canary is a signed JSON document the merchant publishes at `canary_url`. The wallet fetches it periodically before each payment.

### Purpose
- Confirm the merchant is still active and accepting payments
- Allow the merchant to stop all subscriptions remotely (set `active: false`)
- Detect address hijacking (check `wallet_address` matches)
- Auto-expire stale subscriptions (`valid_until`)

### Format
```json
{
  "version": 1,
  "merchant_name": "Le Média Libre",
  "wallet_address": "4At3X5rvVypTofgmueN9s9QtrzdRe5BueFrskAZi17BoYbhzysozzoMFB6zWnTKdGC6AxEAbEE5czFR3hbEEJbsm4hCeX2S",
  "active": true,
  "valid_until": "2027-01-01T00:00:00Z",
  "message": "All good, thank you for your support!",
  "signature": "b7d4...9f2a",
  "pubkey": "a3f2...e8c1"
}
```

### Canary status values (wallet side)

| Status | Trigger | Wallet action |
|--------|---------|---------------|
| `Ok` | Fetch OK, signature valid, address matches, not expired, active=true | Pay as scheduled |
| `NotConfigured` | No `canary_url` in the subscription code | Pay as scheduled (warn user once) |
| `Unreachable` | HTTP error or timeout | Skip this cycle, retry next occurrence |
| `SignatureInvalid` | Ed25519 verification fails | **Suspend** immediately, alert user |
| `AddressMismatch` | `wallet_address` ≠ subscription `destination_address` | **Suspend** immediately, alert user |
| `Expired` | `valid_until` is in the past | **Suspend**, alert user |
| `InactiveByCreator` | `active: false` | **Suspend**, inform user merchant stopped accepting payments |

### Hosting the canary

The canary file should be served at a stable URL, ideally:
- `https://yoursite.com/.well-known/autopay-canary.json`
- Refreshed at least monthly (before `valid_until` expires)
- Signed with the same Ed25519 key as the subscription codes

A `valid_until` of 90 days is recommended.

---

## 7. Occurrence note

The occurrence note is an optional per-payment message the merchant can publish at `occurrence_note_url`.

The wallet fetches it **before** each payment.

### Format
```json
{
  "period": "2026-11",
  "message": "Merci pour votre soutien ! Ce mois-ci : édition spéciale élections.",
  "new_amount": null,
  "new_currency": null,
  "signature": "optional-ed25519-hex"
}
```

### Behavior
- If `new_amount` is **not null** and differs from the subscribed amount: wallet shows a **blocking confirmation dialog** before paying. User must approve the new amount.
- If `message` is set: displayed in payment history.
- If `signature` is present: verify with `merchant_pubkey`. Reject if invalid.

---

## 8. Network adapters

The protocol is designed to be network-agnostic via the `network` field.

| Network | `network` value | Amount unit | Notes |
|---------|----------------|-------------|-------|
| Monero | `"monero"` | XMR (piconero = 10⁻¹²) | Default, primary target |
| Bitcoin | `"bitcoin"` | BTC (satoshi = 10⁻⁸) | V3 target, PSBT-based |
| Lightning | `"lightning"` | sat | Future, pending BOLT 12 recurrence |

Wallets announce their supported networks. A wallet that receives a code for an unsupported network should display an error rather than silently ignoring it.

---

## 9. Backward compatibility — `monero-request:1:`

Crypto AutoPay wallets **must** be able to import `monero-request:1:` codes (read-only, no creation).

Mapping:
| monero-request field | crypto-autopay equivalent |
|---------------------|--------------------------|
| `custom_label` | `custom_label` |
| `sellers_wallet` | `destination_address` |
| `currency` | `currency` |
| `amount` | `amount` |
| `payment_id` | ignored (deprecated in Monero) |
| `start_date` | `start_date` |
| `billing_cycle_days` | converted to cron: `0 0 9 */{days} * *` |
| `number_of_payments` | `number_of_payments` |

Fields not present in `monero-request:1:`: `canary_url`, `merchant_pubkey`, `signature`, `occurrence_note_url`, `pay_window_*`, `cancel`.

---

## 10. Full example codes

### Monthly media support (signed, with canary)
```
crypto-autopay:1:H4sIAAAAAAAAA6tWKkktLlGyUlIqS04tLk5MTQUAVfAHlhAAAAA=
```

Decodes to:
```json
{
  "version": 1,
  "network": "monero",
  "custom_label": "Le Média Libre — Soutien mensuel",
  "description": "Accès complet aux archives et newsletter",
  "destination_address": "4At3X5rvVypTofgmueN9s9QtrzdRe5BueFrskAZi17BoYbhzysozzoMFB6zWnTKdGC6AxEAbEE5czFR3hbEEJbsm4hCeX2S",
  "amount": "0.05",
  "currency": "XMR",
  "schedule": "0 0 9 1 * *",
  "number_of_payments": 0,
  "canary_url": "https://media-libre.fr/.well-known/autopay-canary.json",
  "merchant_pubkey": "a3f2b9e1c7d4f501a8e2b6c3d9f7e4a2b1c8d5e3f2a9b7c4d1e8f5a2b9c6d3e0",
  "signature": "b7d4f9e2a1c8b5d3e6f2a9c7b4d1e8f5a2b9c6d3e0f7a4b1c8d5e2f9a6b3c0d7",
  "checksum": "3a7f2b9e1c4d5e8f"
}
```

### VPN quarterly subscription (no signature, no canary)
```json
{
  "version": 1,
  "network": "monero",
  "custom_label": "IVPN — Abonnement trimestriel",
  "destination_address": "84pr...",
  "amount": "0.05",
  "currency": "XMR",
  "schedule": "0 0 9 1 */3 *",
  "number_of_payments": 0
}
```

### One-time donation scheduled for next month
```json
{
  "version": 1,
  "network": "monero",
  "custom_label": "Don Radio Pirate — Octobre",
  "destination_address": "888...",
  "amount": "0.02",
  "currency": "XMR",
  "schedule": "0 0 9 1 11 *",
  "number_of_payments": 1
}
```

### Fiat-denominated subscription (converted at payment time)
```json
{
  "version": 1,
  "network": "monero",
  "custom_label": "Libre-Market épicerie — 15€/mois",
  "destination_address": "4Bk2...",
  "amount": "15.00",
  "currency": "EUR",
  "schedule": "0 0 9 1 * *",
  "max_conversion_fee_pct": 1.5,
  "number_of_payments": 0
}
```

---

## 11. Security considerations

- **Non-custodial**: the payer's spend key never leaves their device.
- **Hot wallet cap**: wallets implementing this protocol SHOULD warn users when a wallet balance exceeds 1.5 XMR and MUST block (or require explicit override) above 2.0 XMR. The wallet is meant to be topped up periodically, not to hold a full balance.
- **Canary freshness**: always check `valid_until`. A canary with a 2-year validity period defeats its purpose.
- **Signature verification**: unsigned codes are allowed but wallets SHOULD display a warning ("Unverified merchant").
- **URL security**: `canary_url` and `occurrence_note_url` MUST use HTTPS. Tor onion addresses are allowed and encouraged for censorship resistance.
- **Address reuse**: Merchants SHOULD use subaddresses (one per subscriber) to preserve payer privacy.

---

## 11b. Quantum resistance

Ed25519 is vulnerable to Shor's algorithm on a sufficiently powerful quantum computer. Estimates for "cryptographically relevant quantum computers" range from 2030 to 2040.

**Current default (`signature_scheme: "ed25519"`):**
- 128-bit classical security
- Fast (64-byte signatures)
- Widely supported

**Post-quantum option (`signature_scheme: "ml-dsa-65"`):**
- CRYSTALS-Dilithium, NIST FIPS 204 (2024)
- 128-bit quantum-secure security
- Larger signatures (~2420 bytes) but still fine for QR/URL use
- Compatible with the same base64url+gzip encoding

**Migration path:**
1. Merchants add `signature_scheme: "ml-dsa-65"` and a new `merchant_pubkey`
2. They publish the new pubkey in the canary document with `successor_pubkey` and `successor_scheme`
3. Wallets that support ML-DSA verify with the new key; older wallets fall back to unverified-but-warned mode

The canary document should include:
```json
{
  "successor_pubkey": "...",
  "successor_scheme": "ml-dsa-65"
}
```

**Note on symmetric crypto:** AES-256-GCM (used for the SeedVault) is quantum-safe. BLAKE3 (used for checksums) is quantum-safe. Only the Ed25519 asymmetric signature needs upgrading.

---

## 11c. AI agent considerations

With the rise of autonomous AI agents managing finances, the protocol must be safe for machine-to-machine interactions.

### For wallet developers

- **Rate limit subscription creation**: An AI agent should not be able to subscribe to 10,000 services in 1 second. Wallets SHOULD require explicit human confirmation or a per-session cap.
- **Dry-run mode**: An `estimate_only: true` flag in the invocation (not stored in the code) asks the wallet to return the full schedule without paying.
- **Audit trail**: All subscriptions added by an agent SHOULD be tagged with `added_by: "agent"` in the local DB.

### For merchant developers

Add a `meta.json` at `meta_url` for structured AI consumption:

```json
{
  "merchant_name": "Le Média Libre",
  "logo_url": "https://media-libre.fr/logo.svg",
  "description": "Independent French investigative media",
  "ai_tags": ["journalism", "independent", "french", "investigative"],
  "contact": "contact@media-libre.fr",
  "terms_url": "https://media-libre.fr/terms",
  "cancellation_policy": "cancel_anytime",
  "refund_policy": "no_refund",
  "locale": "fr"
}
```

AI agents MUST read `terms_url` and `cancellation_policy` before subscribing on behalf of a user. A wallet should surface these to the user even in autonomous mode.

### DNS-based merchant discovery

To find a merchant's subscription code without having the QR code, publish a DNS TXT record:

```
_autopay.merchant.com.  TXT  "v=1 url=https://merchant.com/.well-known/autopay.json"
```

The `autopay.json` file lists available subscription tiers:
```json
{
  "tiers": [
    { "name": "Supporter", "code": "crypto-autopay:1:H4sI..." },
    { "name": "Premium",   "code": "crypto-autopay:1:H4sI..." }
  ]
}
```

### URI scheme and MIME type

```
crypto-autopay:1:<payload>         ← URI scheme (desktop app handler)
application/x-crypto-autopay       ← MIME type for file downloads
.autopay                           ← file extension
```

NFC tags should use the URI scheme directly — readable by any NFC-enabled phone without a dedicated app.

---

## 12. QR code format

For display as a QR code, use the full `crypto-autopay:1:<payload>` string.

Recommended QR settings:
- Error correction: **M** (15%)
- Encoding: **binary** (not alphanumeric, payload contains mixed chars)
- Version: auto (typically V10-V15 for average payloads)

For very short subscriptions (no canary, no signature), payload fits in V8.

---

## 13. Implementation checklist

### Wallet (payer side)
- [ ] Parse `crypto-autopay:1:` codes (decode + decompress + JSON)
- [ ] Import `monero-request:1:` codes (backward compat)
- [ ] Display subscription details before adding
- [ ] Verify Ed25519 signature if `merchant_pubkey` is present
- [ ] Warn if no signature ("Unverified merchant")
- [ ] Schedule payments via cron expression
- [ ] Check canary before each payment
- [ ] Suspend on canary failure (SignatureInvalid, AddressMismatch, Expired, Inactive)
- [ ] Fetch occurrence note, require confirmation on amount change
- [ ] Support pause / resume / cancel / skip
- [ ] Display next payment date (hidden if suspended)
- [ ] Balance cap: warn ≥1.5 XMR, block ≥2.0 XMR

### Merchant (creator side)
- [ ] Generate subscription codes (webapp or CLI tool)
- [ ] Sign codes with Ed25519 keypair
- [ ] Host canary file at `/.well-known/autopay-canary.json`
- [ ] Refresh canary regularly (before `valid_until`)
- [ ] Optionally: publish occurrence notes per billing period

---

## 14. Comparison with similar protocols

| Feature | crypto-autopay:1: | monero-request:1: | BOLT 12 recurrence | LNURL-pay |
|---------|------------------|------------------|-------------------|-----------|
| Non-custodial | ✅ | ✅ | ✅ | ✅ |
| Canary / stop signal | ✅ | ❌ | ⚠ (in discussion) | ❌ |
| Ed25519 signature | ✅ | ❌ | ✅ | ❌ |
| Fiat currency support | ✅ | ✅ | ❌ | ✅ |
| Arbitrary cron schedule | ✅ | ⚠ (daily only) | ⚠ | ❌ |
| Multi-network | ✅ (trait) | ❌ (XMR only) | ❌ (Lightning) | ❌ (Lightning) |
| Occurrence note | ✅ | ❌ | ❌ | ❌ |
| Human-readable | ✅ (JSON) | ✅ | ❌ (binary TLV) | ⚠ (JSON/URL) |
| QR code size | Medium | Medium | Small | Small |
| Open source | ✅ (GPL-3.0) | ✅ (MIT) | ✅ | ✅ |

---

## 14b. Forward compatibility — no version bump needed

Because the payload is JSON and parsers follow the **robustness principle** ("be liberal in what you accept"), adding new optional fields to `crypto-autopay:1:` does **not** require a version bump.

- An older wallet that encounters an unknown field (e.g. `trial_days`) simply ignores it.
- A newer wallet that receives a code without `trial_days` applies the default (`0`).
- Only **breaking changes** (removing a required field, changing encoding format, changing signature algorithm in an incompatible way) would require `crypto-autopay:2:`.

The `_ext` field provides a blessed namespace for extensions that third-party apps can use without risk of conflict with future official fields:

```json
{ "_ext": { "my_saas_platform": { "plan_id": "pro_monthly", "seats": 5 } } }
```

**In practice**: this protocol is designed to never need a v2 for subscription features. The main scenario for v2 would be a breaking change in encoding (e.g. switching from gzip to zstd, or adding a mandatory outer signature envelope).

---

## 14c. Comparison with Stripe Subscriptions API

| Feature | Stripe | crypto-autopay:1: | Notes |
|---------|--------|-------------------|-------|
| Recurring schedule | `billing_cycle_anchor` + interval | `schedule` (cron) + `repeat_every` | Cron is more flexible |
| Trial period | `trial_period_days` | `trial_days` | ✅ Added |
| Limited payments | `cancel_at_period_end` | `number_of_payments` | ✅ Direct count |
| Proration | `proration_behavior` | `proportional_amount` | ✅ |
| Amount | `unit_amount` + `currency` | `amount` + `currency` | ✅ |
| Fiat conversion | ✗ (Stripe IS fiat) | `max_conversion_fee_pct` | ✅ |
| Grace period | `days_until_due` | `pay_window_after_secs` | ✅ Seconds-precision |
| Stop signal | ✗ (call API) | `canary_url` + `cancel` field | ✅ Non-custodial |
| Metadata | `metadata` (key-value) | `_ext` + `notes` | ✅ |
| Fraud/signature | Internal | Ed25519 / ML-DSA-65 | ✅ |
| Multi-network | Fiat only | Monero, BTC, ETH, Tron, Solana… | ✅ |
| Trial end (absolute) | `trial_end` (Unix ts) | `trial_end` (ISO 8601) | ✅ Added |
| Payment method | Card, SEPA… | Crypto wallet | — |
| Webhooks | ✅ (server-side) | `webhook_url` (optional) | ✅ |
| Split payments | `transfer_data` | `split_payments` (§4.4) | ✅ Multi-recipient |
| Tax rates | `default_tax_rates` | `tax_rate_pct` (informational) | ✅ Display only |
| Quantity / seats | `quantity` | ✗ | Add via `_ext` if needed |
| Invoice items | `add_invoice_items` | `occurrence_note_url` | ✅ Via note |

**Conclusion**: the protocol covers all the essential Stripe subscription features that make sense for crypto. The remaining missing ones (`quantity/seats`) can be added via `_ext` without a version bump.

---

## 15. Versioning

This document describes `crypto-autopay:1:`. Future versions will use `crypto-autopay:2:` etc.

Breaking changes that require a new version number:
- Changing the encoding format (base64url/gzip/canonical-json)
- Removing or renaming required fields
- Changing the signature algorithm

Backward-compatible additions (new optional fields) do NOT require a version bump.

---

## References

- Monero Payment Request Standard (Luke Profits): https://github.com/lukeprofits/Monero_Payment_Request_Standard
- BOLT 12 recurrence PR: https://github.com/lightning/bolts/pull/1240
- kycnot.me VPN directory: https://kycnot.me/vpn
- Libre-Market: https://libre-market.com
- Protocol hosted at: https://autopay.libre-market.com/spec/v1
