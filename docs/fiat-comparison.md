# Crypto AutoPay vs. Standards de Paiement Fiat

## 1. Existe-t-il un standard URI pour les prélèvements fiat ?

**Oui, partiellement : `payto:` (RFC 8905, IETF 2020)**

```
payto://iban/DE89370400440532013000?amount=eur:19.90&message=Abonnement
```

C'est le seul schéma URI de paiement **enregistré à l'IANA** couvrant les rails fiat (IBAN/SEPA, ACH, etc.) et crypto. Il est utilisé par GNUTaler, certains portefeuilles open banking et l'écosystème ICAN/CorePass.

**Ce qui n'existe pas en standard ouvert :**
- Prélèvements automatiques récurrents via URI (chaque banque a son API propriétaire)
- Mandat signé cryptographiquement via lien cliquable
- Gestion de canary / révocation de subscription via URI

---

## 2. Tableau comparatif

| Critère | `payto:` (RFC 8905) | CorePass/ICAN `rc=` | **`crypto-autopay:1:`** |
|---|---|---|---|
| **Registre** | IANA (informationnel) | Propriétaire | — (à déposer) |
| **Cible** | IBAN, Bitcoin, ILP… | Wallets ICAN | Monero, Bitcoin (extensible) |
| **Récurrence** | ❌ non natif | `rc=m`, `rc=30d`, `rc=2w` | Cron 6 champs (précision à la seconde) |
| **Planning complexe** | ❌ | Limité (m, w, d, y) | Quartz cron complet + `repeat_every` |
| **Compression** | ❌ URL query | ❌ URL query | ✅ gzip + base64url |
| **Signature** | ❌ | ❌ | ✅ Ed25519 / ML-DSA-65 post-quantique |
| **Canary / révocation** | ❌ | ❌ | ✅ 7 états, URL polling |
| **Split payments** | ❌ | `split=p:10@…` (basique) | ✅ `split_payments[]` avec label |
| **Période d'essai** | ❌ | ❌ | ✅ `trial_days` / `trial_end` |
| **Nombre de paiements** | ❌ | ❌ | ✅ `number_of_payments` |
| **Taxe** | ❌ | ❌ | ✅ `tax_rate_pct` (informatif) |
| **Forward-compat** | ❌ (versioning manuel) | ❌ | ✅ `_ext` + champs optionnels ignorés |
| **IA / agent-ready** | ❌ | ❌ | ✅ (conçu pour agents autonomes) |
| **Exécution** | App bancaire / handler | Wallet ICAN | Wallet self-custodial |

---

## 3. Pourquoi chaque banque a inventé le sien ?

- **SEPA Direct Debit** (EU) : mandat XML ISO 20022, signé papier ou API propriétaire
- **ACH** (US) : format NACHA propriétaire, pas d'URI
- **GoCardless, Stripe Billing, Chargebee** : APIs REST privées, pas de schéma URI ouvert
- **Open Banking / PSD2** (EU) : API standardisée mais non URI-based pour abonnements

Le problème fondamental : les banques centralisées requièrent un **tiers de confiance** pour le prélèvement. Il n'existe pas d'équivalent du "wallet self-custodial" dans la finance traditionnelle.

---

## 4. `crypto-autopay:` pourrait-il couvrir le fiat ?

**Techniquement oui.** Le protocole est agnostique au réseau. Il suffirait de :

```json
{
  "network": "sepa",
  "address": "DE89370400440532013000",
  "token": "EUR",
  "amount": "19.90",
  "schedule": "0 0 0 1 * *"
}
```

Ou d'intégrer un champ `payto_uri` pointant vers un `payto://iban/...` pour l'exécution fiat :

```json
{
  "network": "sepa",
  "payto_uri": "payto://iban/DE89370400440532013000?receiver-name=Le+M%C3%A9dia+Libre",
  "amount": "19.90",
  "currency": "EUR",
  "schedule": "0 0 0 1 * *"
}
```

**L'exécution** resterait à la charge d'un client bancaire supportant Open Banking/PSD2 — mais le **mandat de subscription signé, auditable et révocable via canary** serait notre valeur ajoutée.

---

## 5. Conclusion et positionnement

`crypto-autopay:1:` est davantage comparable à un **mandat de prélèvement signé + règles de planification** qu'à un simple URI de paiement. `payto:` est le `mailto:` des paiements ; `crypto-autopay:` est son `CalDAV` — il encode un *contrat récurrent* complet.

**Recommandations :**
1. Mentionner la compatibilité théorique fiat dans la documentation publique (attire des cas d'usage au-delà de la crypto)
2. Prévoir un champ optionnel `payto_uri` dans `_ext` (pas de breaking change)
3. Envisager un dépôt IANA du schéma `crypto-autopay` (référence : RFC 8905 pour la procédure)
4. Signaler le projet au [GNU Taler](https://taler.net/) et aux initiatives Open Banking — intersection intéressante

---

*Sources : RFC 8905 (IETF, 2020), CorePass ICAN docs, SEPA Rulebook EPC, NACHA ACH format*
