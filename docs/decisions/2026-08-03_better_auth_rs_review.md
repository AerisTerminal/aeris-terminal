# S2-14 review: `better-auth-rs` 0.10.0 — rejected, primitives path confirmed

**Date:** 2026-08-03
**Reviewer:** implementation agent, evidence-linked below
**Scope:** Section 3.2.1 adoption review for the leading authentication-service
candidate, including the cheapest decisive test (do issued tokens verify
unchanged through `crates/security`?) performed first as required.

## Decision

Reject adoption of `better-auth-rs`. The composed exact-pinned primitives
fallback path in Section 3.2 remains the authentication-service strategy.
Each primitive adopted for `S2-13` (candidates: `argon2`, `oauth2`,
`openidconnect`, `webauthn-rs`, `totp-rs`) still requires its own safety,
license, provenance, fuzzing, and maintenance review before adoption; this
document closes only the `better-auth-rs` evaluation.

## Decisive test result (fails statically)

Section 3.2 requires the service to issue short-lived Ed25519 tokens and
publish revisioned JWKS, with `crates/security` verifying those tokens offline
against immutable revisioned key snapshots. Static review of the exact
registry source (checksum verified, see below) shows:

- `better-auth-core 0.10.0` `JwtConfig` defaults to **HS256** with the
  algorithm as a free-form string; token support is through the `jsonwebtoken`
  crate with no EdDSA/Ed25519 issuance path anywhere in the crate.
- Sessions are opaque `session_`-prefixed bearer tokens, not JWTs.
- There is no JWKS publication endpoint or key-revision model.

The crate therefore cannot issue tokens that `crates/security` verifies, and
adopting it would still leave Axiusflow to build Ed25519 issuance and JWKS
publication itself. The cheapest decisive test is negative without any
integration work.

## Measured signals (2026-08-03)

| Signal | Value | Assessment |
|---|---|---|
| Crate | `better-auth 0.10.0`, published 2026-04-12, checksum `c13f256cc4f6d4bfadbddc7509e60775127ada519c3cfdfec3e7ca43dc211dd6` (verified against the downloaded artifact) | Exact pin possible |
| Downloads | 5,670 total (5,619 on 2026-08-01) | Axiusflow would be the primary serious downstream |
| Repository | 232 stars, 15 forks, 23 open issues, last `master` push 2026-06-06 | Stale for ~2 months |
| Contributors | AprilNEA 109 commits; devin-ai-integration bot 22; better-auth-rs bot 13; three humans with 1–2 commits each | Single-maintainer credential implementation |
| License | MIT OR Apache-2.0 (crate), Apache-2.0 (repository) | Compatible |
| Package contents | Rust sources plus JavaScript, TSX, and TypeScript files (OpenAPI sync tooling) | Foreign-language artifacts inside the package; not a runtime, but review-relevant |
| Password hashing | `argon2` crate with `Argon2::default()` | Meets the memory-hard requirement |
| Unsafe FFI | None observed | Positive |

## Compatibility-gap evidence

The project claims compatibility with `better-auth@1.4.19`. Official Better
Auth latest stable is `v1.6.25` (2026-07-23) with `v1.7.0-rc.2` in flight.
Security-relevant upstream changes inside the gap:

- `v1.6.24` #10368: magic-link and email-OTP send endpoints now validate the
  `Origin` header on cookieless requests, preventing cross-origin abuse. The
  Rust crate has no magic-link/email-OTP implementation at all — both missing
  functionality and an unported security fix class.
- `v1.6.25` #10479: Google One Tap no longer creates users when sign-up is
  disabled on the provider — authorization-relevant behavior with no reviewed
  counterpart in the Rust crate.
- `v1.6.25` #10294: Apple OAuth now sends the PKCE code challenge during
  authorization.
- `v1.7.0-rc`: account identity is rewritten (issuer-scoped
  `providerAccountId`), so the compatibility target itself is moving away from
  the claimed base.

The differential-review burden Section 3.2.1 priced is real, growing, and now
includes a hard incompatibility at the token-issuance boundary that no amount
of differential patching resolves.

## Consequence for S2-13

`S2-13` proceeds with composed exact-pinned primitives. `argon2` is the
front-runner for password verification (already positively reviewed inside
this crate's dependency tree, pending its own independent S2-14 review before
direct adoption). OAuth/OIDC, WebAuthn, and TOTP candidates are reviewed
individually at adoption time. Ed25519 token issuance and revisioned JWKS are
first-party Axiusflow work built on the already-verified `crates/security`
contract, not sourced from any candidate.
