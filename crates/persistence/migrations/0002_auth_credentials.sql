-- Authentication state: credentials, sessions, and the signing-key registry.
-- Credential, session, and refresh state is authoritative in PostgreSQL per
-- Section 3.2; signing keys are file-provisioned in this milestone and the
-- registry records only public metadata and revisions.

create table if not exists auth_credentials (
    principal_id text primary key,
    argon2_phc text not null,
    created_at_unix_nanos bigint not null
);

create table if not exists auth_sessions (
    session_id text primary key,
    principal_id text not null references auth_credentials (principal_id),
    session_secret_sha256 text not null,
    created_at_unix_nanos bigint not null,
    expires_at_unix_nanos bigint not null,
    revoked_at_unix_nanos bigint
);

create index if not exists auth_sessions_principal
    on auth_sessions (principal_id);
