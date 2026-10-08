# Aeris on AWS: services, cost and end-to-end flow

Based on `aeris-website` (static Next.js 16 export and its README AWS phases) and the desktop account
client in `crates/account_runtime`. Prices are AWS list prices in us-east-1 as of September 2026.
Payments use Paddle as merchant of record.

## Summary

| Stage | Estimated AWS spend |
| --- | --- |
| Now: site + waitlist | ~$2/mo |
| Beta: ~1k users, installers | ~$21/mo |
| Launch: ~10k active users | ~$35/mo |
| Scale: ~50k active users | ~$700/mo (Cognito dominates) |

Paddle fees (5% + $0.50 per transaction) are separate and scale with revenue.

> **One code change decides the auth design.** The desktop client was written for a Better Auth
> issuer: it only accepts `EdDSA` ID tokens, requires every OIDC endpoint on the issuer's origin and
> an issuer ending in `/api/auth`, binds an OS-assigned port on `127.0.0.1`, and sends the ID token in
> the link and lease request bodies. Cognito signs with RS256, its issuer
> (`cognito-idp.us-east-1.amazonaws.com/...`) differs from its login domain, it requires exact
> callback URLs, and it only allows plain HTTP for `http://localhost`. The Ed25519 entitlement lease
> stays exactly as it is. The full list of desktop changes is in
> [Desktop changes for Cognito](#desktop-changes-for-cognito).

## Monthly AWS spend by stage

USD per month, estimated from list prices.

| Component | Now: site + waitlist | Beta: ~1k users | Launch: ~10k users | Scale: ~50k users |
| --- | ---: | ---: | ---: | ---: |
| CloudFront flat-rate plan (CDN, WAF, DNS, TLS) | $0 | $15 | $15 | $15 |
| Cognito sign-in | $0 | $0 | $0 | $600 |
| KMS lease signing + SES email | $0.05 | $3.10 | $12 | $55 |
| API Gateway, Lambda, DynamoDB, SQS, S3, logs | $0.50 | $2 | $6.50 | $29 |
| Domain (.com, yearly / 12) | $1.25 | $1.25 | $1.25 | $1.25 |
| **Total** | **~$2** | **~$21** | **~$35** | **~$700** |

At 50k users, Cognito Lite instead of Essentials cuts sign-in cost from ~$600 to ~$220 but drops
email-code and passkey sign-in. Add CloudFront Business ($200) only if the Pro plan's 10M requests /
50 TB are exceeded.

## Services

| Service | Role | When |
| --- | --- | --- |
| Route 53 | DNS for the domain; zone fee included when attached to the CloudFront plan | Now |
| ACM (us-east-1) | Free TLS certificates for the site, API and auth domains | Now |
| S3 (private) + CloudFront OAC | Static Next.js export (`out/`); never public-bucket hosting | Now |
| CloudFront flat-rate plan | One distribution: site, `/api/*` to API Gateway, later `/releases/*`; bundles WAF, DDoS, DNS, logs | Now (Free), Pro at beta |
| API Gateway HTTP API | All dynamic endpoints; built-in JWT authorizer validates Cognito tokens for free | Now |
| Lambda (Rust, ARM64) | Waitlist, account link, lease, billing, webhook, SQS worker; reuse the `aeris_account` crate | Now |
| DynamoDB on-demand | One table: waitlist, accounts, subject links, devices, subscriptions, webhook idempotency (TTL) | Now |
| SES | Waitlist confirmation now; Cognito sign-in codes later (Cognito's built-in email caps at 50/day) | Now |
| CloudWatch + Budgets + Cost Anomaly Detection | Alarms to email, 14–30 day log retention, spend alerts | Now |
| Cognito User Pool (Essentials) | Managed login at `auth.<domain>`: email code + Google, passkeys later; 10,000 MAU free | Beta |
| KMS Ed25519 key | Signs entitlement leases; private key never leaves KMS (EdDSA supported since Nov 2025) | Beta |
| SQS + DLQ | Durable Paddle webhook processing; alarm on DLQ depth | Beta |
| SSM Parameter Store (SecureString) | Paddle API key and webhook secret; free vs $0.40/secret in Secrets Manager | Beta |
| EventBridge Scheduler | Nightly Paddle reconciliation and cleanup jobs | Beta |
| S3 release bucket (versioned) | Immutable installers and signed update manifests behind the same CloudFront | Desktop release |

## Domains

- `<domain>`: CloudFront. Default route to the S3 site, `/api/*` to API Gateway (no cache, same
  origin, no CORS), later `/releases/*` to the release bucket.
- `auth.<domain>`: Cognito managed login custom domain (AWS runs its CloudFront at no charge to you).
- Allowlist Paddle webhook IPs in the WAF so rate rules never drop billing events.

## Deployment

- TypeScript CDK, separate stacks: Edge, Waitlist, Identity, ControlPlane, Release. Prod and dev in
  separate AWS accounts.
- GitHub Actions assumes a short-lived deploy role through OIDC; no AWS keys in GitHub.
- Website deploy: `next build`, `aws s3 sync out/`, CloudFront invalidation. Lambdas built with
  `cargo lambda` for ARM64.

## How sign-in and licensing work

| Step | Actor | What happens |
| :---: | --- | --- |
| 1 | Desktop | Binds the first free fixed loopback port, opens the browser to `auth.<domain>` managed login with PKCE, `state`, `nonce` and `prompt=login` |
| 2 | Cognito | Email code or Google sign-in; redirects to `http://localhost:<port>/callback` with an authorization code |
| 3 | Desktop | Exchanges the code for ID, access and refresh tokens; verifies the RS256 ID token; stores the refresh token in the OS vault |
| 4 | Desktop to `/api/aeris/link` | `Authorization: Bearer <access token>`; the API Gateway JWT authorizer checks issuer, `client_id` and expiry; Lambda takes `sub` from the authorizer claims (never from the body), maps it to the stable Aeris account ID and returns the profile |
| 5 | Desktop to `/api/aeris/lease` (every 6 h + up to 30 min jitter) | Same bearer check; body carries only `device_id`; KMS signs an Ed25519 lease bound to account + device; desktop verifies it against the entitlement key directory and caches it for up to 24 h offline |

The website's account pages stay static: they use a separate Cognito public web client with PKCE and
call the same `/api/*` routes with a bearer token. No server-side rendering is needed.

### Lease signing key

The lease is verified with Aeris's own key, never Cognito's JWKS. The desktop already fetches it from
`{api origin}/.well-known/entitlement-jwks.json` on every online lease refresh and caches the last
verified directory in the vault (`lease.rs`, `fetch_directory`).

- The directory is a static JSON file in the site bucket, served by CloudFront with a 5-minute TTL.
  It holds `OKP` / `Ed25519` keys with a `kid` and base64url `x`.
- The deploy pipeline builds it from KMS `GetPublicKey` (DER SubjectPublicKeyInfo; the last 32 bytes
  are `x`). No Lambda serves it and no private material ever leaves KMS.
- `kid` names the KMS key (`ent-2026-10`, for example), and the lease Lambda puts the same `kid` in
  the lease header.
- Rotation: create the new KMS key and publish both keys. Once the CloudFront TTL has passed, switch
  signing; the desktop fetches the directory alongside every lease, so no longer wait is needed.
  Keep the old key published for 24 hours after the switch so leases it signed stay valid until they
  expire.
- Emergency revocation removes the key from the directory immediately. Online desktops reject leases
  it signed at their next refresh, while offline desktops can keep a cached lease until it expires
  (at most 24 hours).
- Trust rests on TLS to the API origin. The directory is never loaded from the Cognito domain.

### One account per person

Without linking, Cognito creates a separate `Google_<id>` user when someone who signed up with an
email code later signs in with Google using the same address. This would give one person two Aeris
accounts.

- The user pool signs in by email (`UsernameAttributes: email`, case-insensitive), with `email`
  required and auto-verified. Google maps `email`, `email_verified`, `name` and `picture`.
- A pre sign-up Lambda handles `PreSignUp_ExternalProvider`:
  1. It rejects the sign-up unless Google reports `email_verified: true`. Unverified addresses are
     never linked.
  2. It looks up a local user with that email. If none exists, it creates one with
     `AdminCreateUser` (`email_verified: true`, `MessageAction: SUPPRESS`).
  3. It links the Google identity to that local user with `AdminLinkProviderForUser`
     (`ProviderName: Google`, `ProviderAttributeName: Cognito_Subject`, value = Google `sub`).
- Every person therefore has exactly one local profile. Email-code and Google sign-ins both return
  that profile's `sub`, and it counts as one MAU.
- The Aeris account table is keyed by that `sub` with a conditional put. Email is profile data only
  and never an account key, so an email change never creates a second account.
- Linking only works before a federated user's first sign-in. An existing `Google_<id>` profile
  must be deleted and re-linked, so the trigger ships with the Identity stack, before any real
  users sign in.
- Verify first-time Google sign-in end to end in the dev pool. Community reports describe a one-time
  error when linking runs inside the trigger. If it reproduces, the trigger links and then returns a
  "sign in again" error, and the second attempt succeeds.

### Loopback callback

- Cognito allows plain HTTP only for `http://localhost`, so the redirect URI is
  `http://localhost:<port>/callback`. `127.0.0.1` redirect URIs are not used.
- There are three fixed ports, registered as three callback URLs on the desktop app client and
  hard-coded in `loopback.rs`. They sit below Windows' dynamic range (49152–65535), where Hyper-V
  and WinNAT reserve port blocks. The exact numbers are chosen once when the Identity stack is
  written and never changed without shipping a desktop update first.
- The listener tries the ports in order. On each port it binds both `127.0.0.1` and `[::1]` because
  browsers may resolve `localhost` to either. It continues if only one address family is available
  and accepts the first valid callback from either socket.
- If all three ports are busy, sign-in fails with an actionable message naming the ports. It never
  falls back to an OS-assigned port, because Cognito would reject the unregistered redirect.
- AWS describes the localhost exception as intended for testing. If it is ever withdrawn, the
  fallback is a registered `aeris://callback` URL scheme, which needs an installer protocol handler
  and single-instance forwarding.

### Sign-out and revocation

| Event | What happens | Worst-case window |
| --- | --- | --- |
| User signs out in the desktop | The existing code deletes the lease and refresh token from the vault first, then calls `/oauth2/revoke` in the background with `token` and `client_id`. That revokes the refresh token and every access token issued from it | Immediate on the device |
| Next sign-in after sign-out | `prompt=login` makes managed login ask again even if the browser still has a Cognito session cookie, so another person can sign in on that computer | None |
| Stolen or leaked access token | API Gateway checks signatures, not revocation, so access and ID tokens last 15 minutes | 15 min |
| Account disabled, refunded or banned | A DynamoDB `status` is checked by both the link and lease Lambdas. `AdminUserGlobalSignOut` revokes all refresh tokens | Up to 24 h on an offline desktop that holds a cached lease; about 6.5 h on an online one |
| Subscription ends | The lease Lambda issues a lease for the lower plan, and the desktop applies it at the next refresh | About 6.5 h online, 24 h offline |

The desktop app client sets `EnableTokenRevocation: true`, a 15-minute access and ID token validity
and a 30-day refresh token validity in the CDK stack.

### Desktop changes for Cognito

All are in `crates/account_runtime/src/account_service`:

| File | Change |
| --- | --- |
| `oidc.rs` | Accept RS256 ID tokens from the Cognito JWKS. Validate a configured issuer (`cognito-idp.<region>.amazonaws.com/<pool id>`) separately from the login origin (`auth.<domain>`) and the API origin, which replaces the `/api/auth` derivation. Request `scope=openid email profile`; Cognito has no `offline_access` scope, and the code grant always returns a refresh token. Send `prompt=login` instead of `prompt=consent`. Add `client_id` to the revocation body |
| `oidc.rs`, `lease.rs` | Send the access token as `Authorization: Bearer` to link and lease. Remove `subject` and `id_token` from the request bodies |
| `loopback.rs` | Use three fixed ports in order, `localhost` redirect URIs, and dual `127.0.0.1` / `[::1]` binds |
| `lease.rs` | No change. It already fetches `/.well-known/entitlement-jwks.json` and verifies Ed25519 |

## How Paddle billing works

| Step | Actor | What happens |
| :---: | --- | --- |
| 1 | Pricing page | Signed-in user clicks Subscribe; browser calls `POST /api/billing/checkout` |
| 2 | Checkout Lambda | Finds or creates the Paddle customer and creates a transaction with `custom_data.aeris_account_id` server-side, so the browser cannot tamper with it |
| 3 | Paddle.js overlay | Opens with the transaction ID; Paddle collects payment, tax/VAT, receipts (merchant of record) |
| 4 | Paddle to `/api/billing/paddle-webhook` | Lambda verifies `Paddle-Signature` (HMAC, timestamp window), writes the event ID with a conditional put, enqueues to SQS, returns 200 fast |
| 5 | SQS worker | Applies the event only if newer than stored state; updates plan, status, period end and grace in DynamoDB |
| 6 | Desktop | Next lease refresh (or an immediate refresh after checkout) unlocks the plan |
| 7 | Manage billing | `/api/billing/portal` returns a Paddle customer-portal session link; nightly Scheduler job reconciles drift |

> **Paddle blockers before going live.** The domain must be live on HTTPS with product, pricing,
> Terms (with your legal or sole-proprietor name), Privacy and a Refund Policy reachable from
> navigation. The site has Terms, Privacy, Cookies and Disclaimer, but no Refund Policy page yet.
> Review takes about 5–7 business days. The website README still names Dodo as the billing provider.

## Not getting stuck

- Passwordless sign-in (email code, Google) means no password hashes are locked inside Cognito;
  moving providers later is a re-sign-in, not a migration.
- The Aeris account ID is independent of the Cognito `sub`; entitlements are Aeris-signed leases, not
  Cognito claims.
- Every service is request-priced and scales without re-architecture; the only step change is the
  CloudFront plan tier.

## Deliberately not used

| Service | Why not |
| --- | --- |
| EC2, ECS, EKS, App Runner | Always-on compute billed idle |
| RDS / Aurora | Provisioned or minimum-capacity database cost; DynamoDB fits the access patterns |
| NAT Gateway, ALB, VPC Lambdas | About $32/mo and $16/mo minimums with no benefit here |
| Amplify Hosting | Per-GB and build-minute billing; worse than the flat-rate CloudFront plan |
| Secrets Manager | Parameter Store is free for this; switch only if automatic rotation is needed |
| Lambda@Edge, Shield Advanced, multi-Region | Only after a measured need |

## Rollout order

- [ ] Buy the domain; AWS Organizations with prod and dev accounts, IAM Identity Center, root MFA,
      budgets.
- [ ] CDK stacks: Edge (Route 53, ACM, S3, CloudFront Free plan, WAF) and Waitlist (API, Lambda,
      DynamoDB, SES).
- [ ] GitHub Actions deploy through OIDC role: `next build`, `s3 sync out/`, CloudFront invalidation.
- [ ] Add Refund Policy page and legal entity name in Terms; submit domain to Paddle review (5–7
      business days).
- [ ] Desktop sign-in changes for Cognito (see [Desktop changes for Cognito](#desktop-changes-for-cognito)).
- [ ] Identity + ControlPlane stacks: Cognito with the pre sign-up linking trigger, link/lease/billing
      Lambdas, KMS, the published entitlement key directory, SQS, Scheduler.
- [ ] Dev-pool end to end: email code, Google, email code then Google with the same address, sign-out
      then sign-in as another user, all three callback ports busy, key rotation.
- [ ] Paddle sandbox end to end, then live; move CloudFront to Pro when installers ship.

Non-AWS costs to budget: domain renewal, Paddle fees, Windows code signing for installers, business
email. The AWS account must be on the paid plan to use CloudFront flat-rate plans; confirm whether
the USD 1,000 credits cover the plan fee.
