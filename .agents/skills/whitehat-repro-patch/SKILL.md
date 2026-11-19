---
name: whitehat-repro-patch
description: Verify and remediate vulnerabilities using deterministic Fail-Before-Fix reproduction harnesses, parser/auth/setpoint boundary tests, and 5-Gate completion receipts.
---

# White-Hat Reproduction & Defensive Patching (`whitehat-repro-patch`)

Use this skill inside `timberwolf-red`, `cyber-lab`, `code-builder`, or `verifier` when auditing source code for OWASP/CWE vulnerabilities, testing parser or authorization boundaries, or authoring defensive countermeasures.

## 1. Mandatory Three-Step "Fail-Before-Fix" Protocol

Never claim a vulnerability is fixed based on code inspection alone. Every defensive patch must follow three observable steps:

1. **Reproduce (Fail-Before-Fix):**
   - Author a deterministic, isolated unit or property test (in Rust `#[test]` or Node `node:test`) that exercises the exact flaw inside the workspace with zero external network side effects.
   - Run the test and record the failure proving the vulnerability or missing boundary check is real.
2. **Remediate (Minimal Type-Checked Patch):**
   - Apply the smallest, strictly typed fix keeping every source file below the `700`-line ceiling.
   - Enforce security in compiled control-plane logic rather than prompt text:
     - **Path & Symlink Confinement:** Resolve and prefix-check all workspace paths (`self.path(rel)` / `canonicalize()`).
     - **Loopback & Bearer Auth:** Bind local servers to `127.0.0.1` (or authenticated private tunnel IPs `100.64.x.x`) and require `0600` bearer tokens on every mutation route.
     - **Bounded Parsers & Buffers:** Enforce explicit byte ceilings on HTTP bodies, chunked transfer decoding (`dechunk`), and child stdout streams.
     - **Hard Physical/Actuator Setpoint Clamps:** In OT/control logic, clamp values to safe physical min/max ranges before actuation.
3. **Verify (Pass-After-Fix + Regression Sweep):**
   - Re-run the new reproduction test (must pass), the full workspace test suite (`cargo test --workspace` and `node --test`), plus native `audit_ot_exposure` and `nexus_topology` checks.
   - Report the exact pass counts, runtime durations, and file paths back to `centra-actual` or `conductor`.
