# AF_XDP and DPDK retirement

## Decision

AF_XDP and DPDK are retired from the active Axiusflow consumer product. The desktop and managed
provider runtime do not select them, the root workspace does not build them, the Linux installer
does not install their toolchains, the readiness manifest does not advertise them, and active CI
does not compile or exercise them.

## Preserved boundary

The provider-neutral transport contracts remain authoritative: bounded receive ownership and
release, canonical source sequence, explicit timestamp provenance, overflow accounting, bounded
partition and fanout queues, and deterministic replay.

**Supersedes the prior “remain in the repository” retention sentence.** Historical adapter
source, vendored patches, fuzz targets, privileged harnesses, and recorded evidence are
**deleted from `main`**. Audit and contract extraction use git history and an annotated tag
`retired/af_xdp_dpdk_<shortsha>` created immediately before deletion (Stage 0 of
`docs/platform_creation_plan.md`). Live excluded crates are not an audit surface.

The transport profile enum retains the retired variants so old evidence and serialized diagnostic
vocabulary remain interpretable. Runtime readiness rejects both variants because neither appears
in the embedded manifest. Retention of an enum variant is not product availability.

## Reintroduction gate

Reintroduction requires a new architecture and security decision after a contracted raw or
multicast feed demonstrates that tuned kernel sockets are the measured bottleneck. That decision
must restore dependency, safety, license, provenance, maintenance, hardware, provider, installer,
runtime, and CI qualification from first principles; historical evidence alone is insufficient.
