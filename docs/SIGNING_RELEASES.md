# Signing releases (maintainers)

Two independent layers:

1. **Release signature (all platforms, mandatory)**: a `SHA256SUMS` file listing every artifact,
   signed with the project's OpenPGP key. Same scheme as Monero's `hashes.txt`, so users already
   know it. It proves the files come from us, whatever the download mirror.
2. **OS code signing (macOS, Windows)**: removes Gatekeeper / SmartScreen warnings. It proves
   the files come from the holder of a certificate, not that this certificate is ours.

The release key never goes into CI: a compromised runner or repository secret must not be able
to produce a "valid" release. CI builds, a maintainer verifies and signs offline.

---

## 1. One-time setup: the release key

```bash
# Ed25519 primary key for certification, signing subkey for releases.
gpg --quick-gen-key "Crypto AutoPay Releases <releases@example.org>" ed25519 cert 3y
gpg --list-keys --with-subkey-fingerprints "Crypto AutoPay Releases"
gpg --quick-add-key <PRIMARY_FPR> ed25519 sign 1y

# Revocation certificate + offline backup of the primary key (paper / encrypted USB, 2 copies).
gpg --gen-revoke <PRIMARY_FPR> > autopay-revoke.asc
gpg --export-secret-keys --armor <PRIMARY_FPR> > autopay-primary-SECRET.asc
```

- Keep the primary key offline; the signing subkey can live on a hardware token (`gpg --edit-key`, `keytocard`).
- Publish the public key in at least **three independent places**, with the same fingerprint:
  `keys/releases.asc` in the repository, the project website, the Matrix room topic
  (and ideally a keyserver: `gpg --send-keys`).
- Renew the subkey before it expires (`gpg --quick-set-expire`), then re-publish.

---

## 2. Each release

```bash
# 1. Tag; CI builds the draft GitHub release (release.yml).
git tag -s v0.3.0 -m "v0.3.0" && git push origin v0.3.0

# 2. Download every artifact of the draft into an empty folder.
mkdir rel && cd rel && gh release download v0.3.0

# 3. Sanity checks before signing.
ls -l                                   # expected files only, plausible sizes
#    Install and smoke-test at least the Linux build (unlock, one payment, pause).

# 4. Hash and sign.
sha256sum -- *.AppImage *.deb *.msi *.exe *.dmg > SHA256SUMS
gpg --local-user <SIGNING_SUBKEY_FPR> --armor --detach-sign SHA256SUMS   # -> SHA256SUMS.asc
gpg --verify SHA256SUMS.asc SHA256SUMS && sha256sum -c SHA256SUMS

# 5. Upload, then publish the draft.
gh release upload v0.3.0 SHA256SUMS SHA256SUMS.asc
```

- Sign the git tag too (`git tag -s`), with the same key or a maintainer key cross-signed by it.
- Release notes: paste the `SHA256SUMS` content and the key fingerprint, and link `docs/VERIFY_DOWNLOAD.md`.
- Never re-upload an artifact under the same name after publication: bump the version.

---

## 3. OS code signing

### macOS (already wired in `release.yml`)

Apple Developer account (99 $/year), "Developer ID Application" certificate exported as `.p12`.
Repository secrets: `APPLE_CERTIFICATE` (base64 of the .p12), `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (app-specific password), `APPLE_TEAM_ID`.
Tauri then signs and notarizes. Without them the build is ad-hoc signed (`"signingIdentity": "-"`).

Check: `codesign --verify --deep --strict -v "Crypto AutoPay.app"` and `spctl -a -vv "Crypto AutoPay.app"`.

### Windows (not wired yet)

Options, cheapest first:
- **SignPath Foundation**: free code signing for open-source projects, signs from CI with their HSM.
- **Azure Trusted Signing**: low monthly fee, no hardware token.
- **OV/EV certificate** (Sectigo, Certum Open Source...): key on a hardware token, so it is signed
  on a maintainer machine with `signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 file.msi`.

Tauri can call a custom command: `bundle.windows.signCommand` in `tauri.conf.json`.
Check: `signtool verify /pa /v file.msi`.

### Linux

No OS-level signing; `SHA256SUMS.asc` is the signature. Optional later: signed APT repository.

---

## 4. Later

- **In-app updates**: Tauri updater with a minisign key (`cargo tauri signer generate`), public key
  embedded in the binary, private key offline. Notification only, no silent install (see `TODO.md`).
- **Reproducible builds**: pinned toolchains and lockfiles already help; goal is that anyone can
  rebuild and get the same `SHA256SUMS`, so no single maintainer has to be trusted.
