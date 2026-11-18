---
name: decision-routing
description: Choose a bounded local worker or tool from an explicit eligible set while preserving scope, resource, and permission constraints.
---

# Decision routing

Remove candidates that are unavailable, out of scope, over budget, or blocked by host and tool policy. If one deterministic route remains, use it. Otherwise choose only from the listed candidates and preserve an abstain or stop outcome.

Record the chosen capability, relevant limit, observed result, and evidence path in the action ledger. A route does not create permission, credentials, or reliability. Do not replay arbitrary side-effecting tools after an uncertain outcome.
