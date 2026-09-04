# Google OAuth / OpenID Connect registration

> **Status: created and configured.** The Google Cloud project, consent screen, and all
> **three** OAuth clients exist. Their client IDs, the Desktop client secret, and the
> allowlisted email are **not recorded in this document** — they live only in the git-ignored
> `.secrets/` folder and in the git-ignored local config files generated from it.

## Overview

Authentication uses **Google OpenID Connect ID tokens**. The backend validates each token's
signature (Google JWKS), issuer, expiry, and audience, and enforces a single **allowlisted,
verified email**. There is no custom user database and the backend holds no Google secret —
ID-token validation only needs Google's public keys.

**Three** client registrations exist in one Google Cloud project — not two. The extra one is
the *Web* client: Android's Credential Manager mints an ID token whose `aud` is a **Web/server**
client (`serverClientId`), so the Android client alone is not a valid audience.

| # | Client | Google type | Used by | Flow | Token store |
|---|--------|-------------|---------|------|-------------|
| 1 | Android | **Android** (package + SHA-1/SHA-256) | Android app | Credential Manager / Sign in with Google | Managed by Credential Manager |
| 2 | Web/server | **Web application** | Android app as `serverClientId` | — (audience only) | — |
| 3 | Desktop | **Desktop app** | Windows tray app | Authorization Code + **PKCE** via system browser + loopback redirect | Refresh token encrypted with **Windows DPAPI** |

**Backend audience allowlist = Web client id + Desktop client id.** The *Android* client id is
deliberately **not** an audience: it exists so Google trusts the signed app, but the ID token it
produces is issued for the Web client.

## What was created

### 1. Google Cloud project & OAuth consent screen
- Consent screen type **External**, publishing status **Testing**.
- Scopes: `openid`, `email`, `profile` only — no sensitive or restricted scopes, so no
  verification is required.
- The single owner account is registered as a **test user** and is the only allowlisted email.

### 2. Android OAuth client
- Type **Android**, package name `com.tomaskubica.voiceprompt`.
- Release signing certificate fingerprints (non-secret; from the local sideload keystore
  `CN=VoicePrompt Sideload`, alias `voiceprompt`):
  - SHA-1: `90:C6:5C:05:83:37:9B:54:20:76:24:CA:70:49:08:F5:91:75:AA:D5`
  - SHA-256: `91:A3:4E:B3:C5:8B:B9:D2:98:62:2E:69:70:50:6C:9E:2B:FD:AA:A4:1B:C9:B5:D7:C4:E1:68:7E:86:86:B4:6C`
- If the keystore is ever regenerated these fingerprints change and must be updated both here
  and in the Google Android client registration.

### 3. Web (server) OAuth client
- Type **Web application**.
- Its client id is supplied to the Android build as `googleWebClientId` /
  `GOOGLE_WEB_CLIENT_ID` and is the `aud` the backend validates for Android tokens.

### 4. Desktop OAuth client
- Type **Desktop app**.
- Redirect: loopback. The Windows app binds an **ephemeral** `http://127.0.0.1:<port>/` listener
  per sign-in and passes that exact URI as `redirect_uri`, per Google's current
  loopback-IP guidance (`localhost`/`127.0.0.1` with any port).
- The tray app opens the **system browser** (never an embedded web view), completes
  Authorization Code + PKCE (S256) with `access_type=offline`, and stores the refresh token
  encrypted with **DPAPI (CurrentUser)**; it exchanges/refreshes to obtain fresh **ID tokens**
  for backend calls.
- A desktop client's "secret" is not truly confidential (PKCE is the real protection), but it is
  still treated as sensitive operational config: it is loaded only from a git-ignored file, never
  logged, and never committed.

## `.secrets/` workflow

Download each client's JSON from the Google Cloud console into the repo-root `.secrets/`
folder. **`.secrets/` is git-ignored and must stay that way.** Filenames do not matter — the
tooling classifies each file **by JSON shape**:

| Shape | Client |
|-------|--------|
| root `installed`, **no** `client_secret` | Android |
| root `installed`, **has** `client_secret`, redirect `http://localhost` | Desktop |
| root `web` | Web/server |

Then run, from the repo root:

```powershell
./scripts/Configure-OAuth.ps1            # optional: -OwnerEmail <address>
```

It writes **only git-ignored** files and never prints a client id or secret:

| Output | Contents |
|--------|----------|
| `android/keystore.properties` | sets/updates `googleWebClientId=<Web client id>`; existing signing values are preserved |
| `infra/main.parameters.local.json` | `googleAllowedAudiences = <Web>,<Desktop>` and `allowlistedEmail` |
| `windows/installer/staging/google-desktop-client.json` | copy of the Desktop client JSON for the installer to bundle |

Re-run it any time a client is rotated; it is idempotent.

## Backend configuration

The backend reads these from the environment (Bicep sets them on the API app, the worker app,
and the cleanup job):

```
VR_GOOGLE_ALLOWED_AUDIENCES = <WEB_CLIENT_ID>,<DESKTOP_CLIENT_ID>
VR_GOOGLE_ALLOWED_ISSUERS   = https://accounts.google.com,accounts.google.com
VR_ALLOWLISTED_EMAIL        = <owner email>
```

Apply them by redeploying with the generated local parameters file:

```powershell
cd infra
./deploy.ps1 -LocalParametersFile main.parameters.local.json -SkipBuild -ImageTag <current-tag>
```

Validation rules enforced on every `/v1` request:

1. Signature verifies against Google JWKS.
2. `iss` is a Google issuer.
3. `exp` is in the future (and `iat`/`nbf` are sane).
4. `aud` ∈ `VR_GOOGLE_ALLOWED_AUDIENCES`.
5. `email == VR_ALLOWLISTED_EMAIL` **and** `email_verified == true`.

Failures return `401` (missing/invalid/expired token, wrong audience) or `403` (valid token, not
allowlisted). `GET /health/live` and `GET /health/ready` are unauthenticated.

## ⚠️ Testing vs Production: the 7-day refresh-token expiry

While the consent screen's publishing status is **Testing**, Google expires refresh tokens
issued to test users after **7 days**. For the Windows tray app that means sign-in silently
stops working roughly once a week: the refresh returns `invalid_grant`, the app moves to
*Sign in required*, and the user must sign in again through the browser.

**Recommendation for durable personal use: move the consent screen to "In production."**

- With only the **non-sensitive** scopes `openid`, `email`, and `profile`, publishing to
  production requires **no Google verification review** and no security assessment — the app
  simply stops being restricted to test users.
- Refresh tokens then remain valid until they are explicitly revoked, the password changes, or
  they go unused for 6 months.
- Access remains restricted to one person regardless of publishing status, because the
  **backend** enforces the single allowlisted email — publishing does not widen access to your
  data.

To switch: Google Cloud console → **APIs & Services → OAuth consent screen** → **Publish app** →
confirm. Existing client IDs and secrets are unchanged, so no rebuild or redeploy is needed;
sign in once more in the tray app to obtain a long-lived refresh token.

If you prefer to stay in Testing, expect to re-authenticate weekly — the app handles this
gracefully (it shows *Sign in required* rather than failing silently), but it is not
maintenance-free.

## Checklist

- [x] Backend deployed: `https://ca-api.ambitiousdesert-517ec9ed.swedencentral.azurecontainerapps.io`.
- [x] Android package name fixed (`com.tomaskubica.voiceprompt`); release keystore created; SHAs recorded above.
- [x] Consent screen created (External / Testing) with the owner as test user; scopes limited to `openid email profile`.
- [x] **Android** OAuth client created (package + fingerprints above).
- [x] **Web** OAuth client created and wired into the Android build as `googleWebClientId`.
- [x] **Desktop** OAuth client created; loopback redirect; JSON staged for the Windows installer.
- [x] Backend audience allowlist (Web + Desktop) and allowlisted email deployed to the API app, worker app, and cleanup job.
- [ ] *Optional but recommended:* publish the consent screen to **In production** to avoid the 7-day refresh-token expiry.
