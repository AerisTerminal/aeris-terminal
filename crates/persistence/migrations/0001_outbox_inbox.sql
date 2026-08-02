-- First persistence schema: transactional outbox, idempotent inbox, and the
-- migration ledger that tracks this file's application.

create table if not exists axiusflow_schema_migrations (
    version integer primary key,
    name text not null,
    checksum text not null,
    applied_at_unix_nanos bigint not null
);

create table if not exists axiusflow_outbox (
    sequence bigserial primary key,
    event_id text not null unique,
    topic text not null,
    partition_key bytea not null,
    payload bytea not null,
    staged_at_unix_nanos bigint not null,
    published_at_unix_nanos bigint
);

create index if not exists axiusflow_outbox_unpublished
    on axiusflow_outbox (sequence)
    where published_at_unix_nanos is null;

create table if not exists axiusflow_inbox (
    event_id text primary key,
    received_at_unix_nanos bigint not null
);
