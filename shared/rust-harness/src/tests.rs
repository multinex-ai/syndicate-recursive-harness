// The test suite stays in one logical `crate::tests` module so its shared
// fixtures and imports behave exactly as before. Domain chunks keep each
// source file reviewable and below the repository's 700-line ceiling.
include!("tests/core_jobs.rs");
include!("tests/runtime_reclaim.rs");
include!("tests/agent_dispatch.rs");
include!("tests/syndicate_reward.rs");
include!("tests/telemetry_syndicates.rs");
include!("tests/report_forge.rs");
include!("tests/forge_assist.rs");
