# Signed macOS releases

The Release workflow requires Apple signing and notarization credentials. It fails
before building if any required secret is missing; there is no ad-hoc release
fallback. Complete this setup before merging the workflow change or tagging a
release. Local builds still use ad-hoc signing unless an Apple identity is supplied.

## Reusing the existing certificate

Recon is signed with the same Developer ID certificate as Shipyard:
`Developer ID Application: RazorKode LLC (969ABKM962)`. If that certificate is
already installed in your login keychain, skip the Apple setup below. Export it as a
`.p12` (step 4) and use the same six secret values as the Shipyard repository.
You can inspect past notarization submissions locally with
`xcrun notarytool history --keychain-profile shipyard`.

## One-time Apple setup

1. Enroll in the [Apple Developer Program](https://developer.apple.com/programs/enroll/)
   and wait for the membership to become active.
2. In Keychain Access on your Mac, use Certificate Assistant → Request a Certificate
   From a Certificate Authority to save a certificate signing request (CSR).
3. In [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources/certificates/list),
   create a **Developer ID Application** certificate using that CSR. Download and
   open the certificate on the same Mac, where its private key was generated.
4. In Keychain Access → login → My Certificates, export the certificate and its
   private key as a password-protected `.p12` file.
5. Find the complete identity with `security find-identity -v -p codesigning`.
   It looks like `Developer ID Application: Your Name (TEAMID)`.
6. At [account.apple.com](https://account.apple.com/), generate an **app-specific
   password** for notarization. Find your Team ID in your Apple Developer membership
   details.

See [Tauri's macOS signing guide](https://v2.tauri.app/distribute/sign/macos/)
for the certificate export and notarization details.

## GitHub repository secrets

In the repository's Settings → Secrets and variables → Actions, add:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64-encoded `.p12` containing certificate and private key |
| `APPLE_CERTIFICATE_PASSWORD` | Password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | Complete `Developer ID Application: … (TEAMID)` identity |
| `APPLE_ID` | Apple account email used for notarization |
| `APPLE_PASSWORD` | App-specific password, not your account login password |
| `APPLE_TEAM_ID` | Developer Program Team ID |

To copy the base64 certificate directly to the macOS clipboard:

```bash
openssl base64 -A -in /path/to/certificate.p12 | pbcopy
```

Paste it into the GitHub secret field. Keep certificate files and passwords out of
the repository, issue comments, and build logs.

Retain the existing `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets. Those authenticate updater downloads;
they serve a separate purpose from Apple's code-signing certificate.

## Release and verification

Bump the app version using the project's normal process, then push a matching new
`v*` tag. The existing manual Release trigger also publishes, so use it only when
ready to distribute that version; do not use an existing public version for a test.

Tauri imports the certificate and overrides the local ad-hoc signing identity using
`APPLE_SIGNING_IDENTITY`. Hardened Runtime is explicitly enabled. Tauri submits the
app to Apple's notarization service using `notarytool` and staples the accepted
ticket before packaging it.

The action runs `scripts/build-macos-release.sh` as its build command. Before
returning control to the upload step, the script verifies the original app, the app
extracted from the updater archive, and the app mounted from the DMG. Each must have:

- A valid deep, strict code signature from the configured Developer ID and team.
- Hardened Runtime enabled.
- A valid stapled notarization ticket.
- A successful Gatekeeper assessment with `spctl`.

The ticket is stapled to the **app inside the DMG and updater archive**. The outer
DMG does not receive a separate notarization submission in this workflow.
A failed verification stops the action before it creates or uploads release assets.
The wrapper's artifact paths match the workflow's Apple Silicon target; update both
if another architecture or build profile is added.

After the first signed release, download the new DMG through a browser on another
Mac (or a clean account), install Recon, and confirm it launches normally with
quarantine intact. Existing downloads are not retroactively signed or notarized.
