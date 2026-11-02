# Verify your download (users)

AutoPay sends real money on your behalf: check that the file you downloaded is the one published
by the project, before installing. 5 minutes, once per version.

You need three files from the release page: **your installer**, `SHA256SUMS` and `SHA256SUMS.asc`.

Release key fingerprint (compare it with at least one other source: website, repository
`keys/releases.asc`, Matrix room topic):

```
<FINGERPRINT — published with the first signed release>
```

If any step fails: **do not install**, and report it in the Matrix room.

---

## Linux

```bash
# 1. Import the key and check its fingerprint against the one above.
gpg --import releases.asc            # or: curl -sL <repo>/keys/releases.asc | gpg --import
gpg --fingerprint "Crypto AutoPay Releases"

# 2. Check the signature: must say "Good signature" with the expected fingerprint.
gpg --verify SHA256SUMS.asc SHA256SUMS

# 3. Check your file: must say "OK".
sha256sum -c SHA256SUMS --ignore-missing
```

The "This key is not certified with a trusted signature" warning is normal, as long as the
fingerprint matches.

## macOS

Install GPG once: [GPG Suite](https://gpgtools.org) or `brew install gnupg`, then same commands
as Linux, replacing the last one with:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing
```

First launch: if macOS says the developer cannot be verified, right-click the app > **Open**
(only needed on builds not yet notarized).

## Windows

1. Install [Gpg4win](https://www.gpg4win.org) (Kleopatra included).
2. Kleopatra > **Import** `releases.asc`, then check the fingerprint (right-click > Details).
3. Kleopatra > **Decrypt/Verify** > select `SHA256SUMS.asc`: it must show a valid signature.
4. Hash your installer in PowerShell and compare it with its line in `SHA256SUMS`:

```powershell
Get-FileHash .\Crypto-AutoPay_0.3.0_x64_en-US.msi -Algorithm SHA256
```

The two values must be identical (case does not matter). A SmartScreen warning can appear while
Windows code signing is not in place: click **More info > Run anyway** only after these checks.

---

## Why both steps?

- The **hash** proves the file was not altered or truncated.
- The **signature** proves the list of hashes was written by the project key, so a compromised
  mirror cannot simply publish a modified file together with a matching hash.
