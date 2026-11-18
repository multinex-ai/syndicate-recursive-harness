# Exact public publication approval draft

Nothing in this file has been posted. GitHub and Hugging Face publication remain blocked until Chris approves this exact copy and Hugging Face is signed in to the intended owner account.

## GitHub repository

- Owner: `multinex-ai`
- Name: `syndicate-recursive-harness`
- Visibility: public
- License: Apache-2.0 for code; model weights retain their own upstream licenses and are not committed to Git.
- Description: `Local-first recursive Qwen agent harness for Apple Silicon with bounded automatic handoffs, deterministic context compaction, verified model downloads, and evidence receipts.`
- Topics: `agents`, `apple-silicon`, `local-ai`, `qwen`, `mlx`, `llama-cpp`, `multi-agent`, `rust`, `mcp`, `huggingface`
- Public README: the exact current contents of `README.md`.

## First public tracking issue

Title: `Release Syndicate Recursive Harness as a standalone, model-downloading package`

Body:

> Build and qualify the first public Syndicate Recursive Harness release for Apple Silicon.
>
> Acceptance:
> - keep downstream proprietary applications out of this public repository and integrate only through a versioned CLI/MCP contract;
> - preserve bounded syndicate edges, context budgets, the machine-wide heavy-model lock, and the action/evidence ledger;
> - ship a native installer with plan, doctor, install, bounded retry, verified model download, repair, rollback, and uninstall behavior;
> - keep weights out of Git and download only license-approved, revision-pinned, SHA-256-verified files;
> - prove Qwen chat/code, a declared specialist handoff, Wan video where resources permit, clean teardown, and honest blocked behavior for the unapproved FLUX encoder;
> - pass typecheck, tests, release build, repository/privacy/provenance checks, responsive report verification, and hosted CI;
> - publish checksums, SBOM/provenance, license inventory, tutorials, and a five-gate evidence receipt.
>
> The source repository may be published once this exact copy is approved and the tracking issue exists. A production package release remains blocked until clean external-machine, signing/notarization, hosted CI, model-license, and Hugging Face owner gates are satisfied.

## Branch, commit, and pull request after the issue exists

- Branch: `issue-<number>-standalone-release`
- Commit: `issue #<number>: prepare standalone harness release`
- Pull request title: `issue #<number>: release standalone Syndicate Recursive Harness`
- Pull request body opening: `Resolves #<number>. This change publishes the standalone Apple-Silicon-first harness without proprietary downstream code or model weights in Git. The attached receipt distinguishes local source/build evidence from hosted CI, clean-machine, signed-release, and live model acceptance.`

## Proposed Hugging Face repositories

Create only after owner sign-in and a final license check. The exact public model-card text is in:

- `catalog/models/cards/local-syndicate-qwen35-9b.md`
- `catalog/models/cards/local-syndicate-flux2-klein-4b.md`
- `catalog/models/cards/local-syndicate-wan13-checkpoint.md`
- `catalog/models/cards/local-syndicate-wan21-base-assets.md`
- `catalog/models/cards/local-syndicate-umt5-mlx-q8.md`

Do not mirror `ponpoke/flux2-klein-4b-uncensored-text-encoder`. It is gated and licensed under FLUX Non-Commercial v2.1; users must accept its authoritative source terms and explicitly opt in.
