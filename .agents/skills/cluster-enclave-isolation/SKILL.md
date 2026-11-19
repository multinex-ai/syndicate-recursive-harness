---
name: cluster-enclave-isolation
description: Enforce zero-trust Docker enclave sandboxing, capability dropping, and unidirectional userspace tunnel ACLs across local Apple Silicon and remote GPU cluster nodes.
---

# Cluster & Enclave Isolation (`cluster-enclave-isolation`)

When executing red-team reproduction harnesses, running untrusted code inside Docker enclaves (`cyber-lab`, `timberwolf-red`), or dispatching tasks over remote GPU cluster nodes (`centra-actual`, `local_cluster_rpc`), enforce strict container and network boundary invariants.

## 1. Docker Tool Enclave Hardening

Every container enclave spawned or audited by the harness must enforce:
- **Zero Linux Capabilities:** `cap_drop: ["ALL"]` with `security_opt: ["no-new-privileges:true"]`.
- **Immutable Root Filesystem:** `read_only: true` with bounded, non-executable ephemeral scratch mounts (`tmpfs: /tmp:rw,noexec,nosuid,size=64m`).
- **No Docker Socket Exposure:** Never bind-mount `/var/run/docker.sock`, host `$HOME`, `.ssh`, `.aws`, or `.git/config` into an execution container.
- **Network Quarantine:** Use `network_mode: "none"` for vulnerability reproduction and parser fuzzing (`whitehat-repro-patch`); use explicit internal bridge networks with egress allowlists only when integration testing requires local service reachability.

## 2. Multi-Node Cluster & Userspace Tunnel Isolation

When routing model inference or agent tasks across an overlay mesh (`100.64.x.x` Tailscale / WireGuard userspace tunnels or `models/cluster-qwen35-node.json`):
- **Unidirectional Initiation:** The local coordinator initiates outbound requests to remote worker endpoints (`100.64.0.20:18089`); remote GPU nodes must never be granted inbound SSH, SMB, or arbitrary RPC dial-back into the primary workstation.
- **Loopback & Subnet Binding:** Local harness listeners (`127.0.0.1:18080..18097`) remain bound to `127.0.0.1` unless explicitly exposed to an authenticated point-to-point tunnel interface.
- **Payload Scrubbing:** Strip credentials, local environment variables, and unrelated realm files before serializing a remote `local_cluster_rpc` request.
