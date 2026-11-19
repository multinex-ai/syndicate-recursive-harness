---
name: sbom-supply-chain-audit
description: Audit lockfiles, commit pins, container base images, and CI/CD workflow permissions to prevent dependency confusion, unpinned weight drift, and supply-chain injection.
---

# SBOM & Supply-Chain Audit (`sbom-supply-chain-audit`)

When auditing repository dependencies, model artifacts, or CI/CD pipelines (`greyfox-recon`, `timberwolf-red`, or `verifier`), verify deterministic provenance across every build and runtime layer.

## 1. Lockfile & Dependency Pinning Invariants

- **Rust (`Cargo.toml` / `Cargo.lock`):** Verify workspace dependencies resolve without floating git branches; check native FFI crates and build scripts (`build.rs`) for unexpected network calls or shell execution.
- **Node / TypeScript (`package.json` / `package-lock.json`):** Audit `preinstall` / `postinstall` lifecycle hooks (`npm pkg get scripts`) and reject unverified install-time script execution.
- **Python ML Runtime (`requirements.lock.txt`):** Require exact `==` version pins and `--require-hashes` SHA-256 digests where applicable; never execute `pip install` from unpinned mutable URLs in production profiles.
- **Model Weights (`models/*.json`):** Every Hugging Face model or adapter entry must pin an immutable 40-character commit SHA (`revision`) and expected file byte size / SHA-256 digest.

## 2. CI/CD & GitHub Actions Hardening

Inspect `.github/workflows/*.yml` for supply-chain escalation vectors:
- Flag `pull_request_target` workflows that check out untrusted PR head commits (`actions/checkout` with `ref: ${{ github.event.pull_request.head.sha }}`).
- Verify explicit least-privilege `permissions:` blocks (`contents: read` by default; never implicit `write-all`).
- Reject unescaped `${{ github.event.issue.title }}`, `${{ github.event.pull_request.body }}`, or branch names interpolated directly inside `run:` shell scripts.

## 3. Audit Receipt Output

Report findings as a structured table listing:
1. **Artifact / Manifest Path**
2. **Pin / Digest Status (`PINNED_VERIFIED`, `FLOATING_REF`, `UNSAFE_HOOK`, `WORKFLOW_INJECTION`)**
3. **Remediation Diff or Hash Pin**
