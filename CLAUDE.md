# clever-project — Claude context

Rust CLI that syncs a YAML/JSON project file with Clever Cloud resources by driving the `clever-tools` CLI. User-facing doc is in `README.md`; this file captures the conventions and patterns Claude should follow when working on the codebase.

## Architecture in one minute

```
cli.rs        clap: Cli + Read/Apply/Delete/Check args
model.rs      Project struct + load_and_resolve (parse + interpolate + validate kinds/regions)
interpolate.rs  ${var} resolver, walks a serde_yaml::Value
clever.rs     Wrapper around the `clever` CLI (Command::new + parse JSON)
state.rs      <project>.state JSON sidecar (resources currently managed)
commands/
  apply.rs    5 phases: addons → apps create/update → service links → network groups → restarts
  delete.rs   network groups first → apps → addons (best-effort, no bail)
  check.rs    static checks + optional live API validation (skipped with --offline)
  read.rs     org introspection → Project file
  mod.rs      Shared: OrgCache (lazy invalidatable cache) + resolve_project_file (auto-discovery)
```

## Core patterns to follow

### State-first with listing fallback

Every lookup-by-name uses this pattern:

1. Try `state.find(kind, name, org)` — fast, no network.
2. On miss: use `OrgCache::apps(...)` / `addons(...)` (lazy, listed at most once per run).
3. If the state-known id is stale (the clever call against it fails), drop the entry, `cache.invalidate()`, fall back to the listing path. Persist the corrected id to state.

Don't add new `clever ... list` calls upfront. Always go through `OrgCache` so the calls stay lazy.

### Validation layers

Static (no network, runs at `load_and_resolve`):
- syntax, interpolation, secrets, mixed-shape variables, `app.kind` in `ALLOWED_APP_KINDS`, regions in `ALLOWED_REGIONS`.

Cross-resource static (in `check.rs`):
- `validate_dependencies` — refs to existing project keys, no self-deps.
- `validate_network_groups` — link refs exist.
- `validate_unique_names` — names unique within each type.

Live (online, in `apply.rs`, also reused from `check`):
- `validate_addons` against `clever curl /v2/products/addonproviders`.
- `validate_app_scaling` against `clever curl /v2/products/instances`.

When adding a new check, prefer static if possible. Wire it into `check::run` so the `check` command covers it.

### Error handling

- `anyhow::Result<T>` everywhere, `bail!` for hard failures, `with_context` to add the resource name to errors.
- `delete` is **best-effort**: per-resource errors → `warn!` + counter + continue. Don't bail.
- `apply` is **fail-fast** except phase 3, which is wrapped in a one-shot retry: on failure, refresh state from clever, rebuild dep maps, retry phase 3 once.
- Stale state on app update → drop the entry, invalidate cache, fall through to listing. Same pattern in `handle_app` / `delete_resource`.

### Restart heuristic (phase 5 in apply)

Restart if and only if:
- the app was just created **with** a GitHub source (kicks off first deploy), or
- it already existed and `env` or `dependencies` changed.

Created without GitHub → don't restart (no code yet). Domain, scalability, build flavor or branch changes alone don't trigger a restart. (A branch change in particular only matters at the next push to the Clever remote, so there's no point redeploying the current commit on a different branch label.)

### Dry-run

The `Clever` wrapper carries a `dry_run` flag. Every mutation method checks `self.dry_run` first and either logs `[dry-run] would ...` or returns a synthetic id (`dry-run::app::<name>`). Helpers like `update_app` / `sync_dependencies` use `is_synthetic` to skip real reads against those ids.

When adding a new mutation method to `Clever`, follow the same shape:

```rust
pub fn new_op(&self, ...) -> Result<...> {
    if self.dry_run {
        info!("[dry-run] would ...");
        return Ok(...);
    }
    self.run(&[...])
}
```

## Code conventions

- **No emojis** anywhere — in code, in user output, in commits, in docs.
- **No multi-line docstrings.** One short doc line above functions when the *why* is non-obvious. Skip the docstring entirely when the name says it.
- **No comments that describe what well-named code already does.** Comments are for hidden constraints, non-obvious invariants, workarounds.
- **No "added for X" / "used by Y" comments** — that info belongs in the commit message / PR description.
- **No backwards-compat shims** when changing a struct. The state schema is forward-compatible via `#[serde(default)]` on new fields; that's enough.
- **`pub(crate)` over `pub`** for things that don't need to escape the crate.
- **Tests live in the same file as the code**, in `#[cfg(test)] mod tests`. Put the test module **after** the items it tests (clippy's `items_after_test_module` lint is on).
- **Tests use temp files via `std::env::temp_dir()`** with `process::id() + nanos` to avoid collisions. Clean up with `std::fs::remove_file(...).ok()` at the end.

## CI parity (run before every claim "build clean")

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

CI runs these on Linux/macOS/Windows (`.github/workflows/ci.yml`).

## Adding things — quick recipes

### A new clever wrapper method

In `clever.rs`:
1. Add the call inside `impl Clever`. Read methods use `run_json`, mutations use `run` (or `Command + Stdio::piped()` for stdin).
2. Always handle dry-run (log + return early or synthetic value).
3. If parsing JSON, define the struct in `clever.rs` next to the existing `ListedApp` / `ListedAddon` family and `#[allow(dead_code)]` fields you don't use yet.

### A new CLI flag

In `cli.rs`:
1. Add the field to the relevant `*Args` struct.
2. If it's a path: `Option<PathBuf>` (auto-discovery friendly) or `PathBuf`.
3. If it's `key=value` repeatable: `value_parser = parse_kv`.
Wire it into the command's `run` in `commands/<name>.rs`.

### A new validator

If static and cross-resource: add to `check.rs`, call it from `check::run` between the existing `validate_*` calls. If live (needs the API): expose via `pub(crate)` from `apply.rs` and call from both `apply::run` and `check::run` (skipped when `--offline`).

### A new resource kind in state

In `state.rs`:
1. Add a variant to `ResourceKind` (the enum uses `#[serde(rename_all = "snake_case")]`).
2. Update `delete::delete_resource`'s match (it's exhaustive).
3. Add tracking in the relevant `apply` phase.

## Gotchas

- **Addon `kind` vs Clever provider id**: project uses `kind: postgresql`, Clever expects `postgresql-addon`. `resolve_provider` in `apply.rs` maps the common short forms; unknowns pass through. The live `validate_addons` enforces the final value against the live catalog.
- **`java` is an alias for `jar`.** Normalized at load time. The `kinds_match` helper still tolerates the alias defensively when comparing project vs clever.
- **Addons need `realId` for NG linking**, not `addonId`. We track both in `StateResource.real_id` (optional, `#[serde(default)]` for backward compat with old state files). Phase 4 (NGs) in apply builds `addon_real_id_by_key` from state or a fresh listing.
- **`*.cleverapps.io` domains are auto-managed by Clever.** `apply` never tries to remove them. `read` filters them out of the generated project file.
- **The state file is not a contract**: it's a regenerable cache. If a state-known id leads to a clever-side error, drop it and refresh. Never trust state without verification when a mutation is attempted against it.
- **Variable interpolation is single-pass.** `variables.foo: ${bar}` won't expand `${bar}` before storage — only `${secrets.X}` is pre-expanded in variable values. Document this if it bites a user.
- **`clever config` has no JSON mode** — the `config:` field in the model is parsed but ignored on both `read` and `apply`. Don't try to wire it up by parsing human output.
- **`clever scale` has no read mode**, so `read` and `status` go through the per-app v2 endpoint (`Clever::get_app_details` → `/v2/organisations/{org}/applications/{app}`) which returns env + vhosts + scalability + buildFlavor + separateBuild in one call. `read` and `live::snapshot` use it to consolidate what used to be 3 separate API calls per app into one. `auto` on scalability is inferred: equal min/max on both count and flavor → `auto: false`; any range → `auto: true`. Scalability and build drift are reported by `status` only when the project file declares the corresponding block, matching apply's "don't touch if absent" behaviour.
- **Cross-resource refs** (`${apps.KEY.env.VAR}` / `${addons.KEY.env.VAR}`) are NOT resolved at load time — `Resolver::resolve_string_inner` detects them via `parse_cross_ref` and echoes the literal `${...}` back into the resolved string. The second pass lives in `commands/cross_refs.rs::resolve_in_project`, called from `apply` and `status` between `live_snapshot` and `plan_mod::compute`. It scans every `project.apps.*.env` value, fetches the source's live env once per referenced key (`Clever::get_env_full` for apps, `Clever::get_addon_env` for addons), and substitutes in place. Missing data (resource not yet deployed, var name typo) is a warning + empty substitution, *not* a hard error — matches the "deploy once, redeploy with the value" workflow the user explicitly opted into. Only env values are rescanned; refs in `name:` / `domains:` / etc. would survive as literals and likely make apply fail.
- **Git branch (`App.source.branch`)** is read from `AppDetails.branch` (top-level on the v2 endpoint) and written via `Clever::set_branch` → `clever curl -X PUT /v2/organisations/{org}/applications/{app}/branch -d '{"branch":"…"}'`. There's no first-party `clever-tools` subcommand for it. Same opt-in rule as scalability/build: `apply` only pushes when the project file pins a branch, and `status` only reports drift in that case.
- **Build flavor (`App.build`)** mirrors the API's `buildFlavor` + `separateBuild`. `apply`:
  - `separate: true, flavor: Some(F)` → `clever scale --build-flavor F`
  - `separate: false` → `clever scale --build-flavor disabled` (yes, the literal string `disabled` is a valid value for that flag)
  - `separate: true, flavor: None` → warn and skip

  The diff in `plan::build_summary` collapses `separate: false` to a single "disabled" string regardless of the inert `flavor` value the API may have persisted — so drift only fires on real behaviour changes, not on the dead flavor field.

## Commits and PRs

- Commit messages: terse subject in lower-case, no emojis, focus on the "why".
- `release.sh` is the canonical way to cut a release. Don't tag/push manually unless asked.
- The release workflow checks `Cargo.toml`'s version matches the tag, builds a matrix of 8 targets, uploads archives, and publishes to crates.io.
