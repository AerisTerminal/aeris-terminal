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

> **One code change decides the auth design.** The desktop client only accepts `EdDSA` ID tokens,
> requires every OIDC endpoint on the issuer's origin and an issuer ending in `/api/auth`, and binds
> an OS-assigned loopback port. Cognito signs with RS256, its issuer
> (`cognito-idp.us-east-1.amazonaws.com/...`) differs from its login domain, and it requires exact
> callback URLs. Using Cognito means changing `oidc.rs` and `loopback.rs`: accept RS256, validate a
> configured issuer plus login origin, configure the API origin for link and lease separately, and
> register a small fixed set of loopback ports. The Ed25519 entitlement lease stays exactly as it is.

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
| 1 | Desktop | Opens browser to `auth.<domain>` managed login with PKCE; waits on a loopback callback |
| 2 | Cognito | Email code or Google sign-in; redirects back with an authorization code |
| 3 | Desktop | Exchanges code for ID, access and refresh tokens; stores refresh token in the OS vault |
| 4 | Desktop to `/api/aeris/link` | Access token checked by the API Gateway JWT authorizer; Lambda maps Cognito `sub` to the stable Aeris account ID |
| 5 | Desktop to `/api/aeris/lease` (every 6 h) | Lambda reads the entitlement, KMS signs an Ed25519 lease bound to account + device; desktop verifies against the JWKS and caches it for 24 h offline |

The website's account pages stay static: they use a separate Cognito public web client with PKCE and
call the same `/api/*` routes with a bearer token. No server-side rendering is needed.

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
- [ ] Desktop sign-in changes for Cognito (RS256, split origins, fixed callback ports).
- [ ] Identity + ControlPlane stacks: Cognito, link/lease/billing Lambdas, KMS, SQS, Scheduler.
- [ ] Paddle sandbox end to end, then live; move CloudFront to Pro when installers ship.

Non-AWS costs to budget: domain renewal, Paddle fees, Windows code signing for installers, business
email. The AWS account must be on the paid plan to use CloudFront flat-rate plans; confirm whether
the USD 1,000 credits cover the plan fee.
