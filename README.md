# clever-project

Declare your [Clever Cloud](https://www.clever.cloud/) resources in a YAML or JSON file and sync them with a single command. `clever-project` reads the project file and drives the official `clever-tools` CLI to create, update or delete the corresponding apps and addons.

```sh
clever-project apply project.yaml --env prod
clever-project delete project.yaml --env staging --dry-run
clever-project read --org orga_xxx --all -o project.yaml
clever-project check project.yaml --offline
```

## Prerequisites

- The official `clever-tools` CLI must be installed and on your `PATH`:

  ```sh
  npm i -g clever-tools
  clever login          # one-shot, opens the browser
  ```

- A Rust toolchain (only to build `clever-project` itself).

The CLI doesn't manage nvm/bun for you — make sure `clever --version` works in the shell you invoke it from.

## Install

Pick one:

### Pre-built binary (recommended)

Each tagged release publishes archives for Linux (x86_64 / aarch64, gnu + musl), macOS (x86_64 / aarch64) and Windows (x86_64 / aarch64) on the GitHub Releases page:

```
https://github.com/mathieuancelin/clever-project/releases/latest
```

Download the archive matching your platform, extract, and put `clever-project` somewhere on your `PATH`.

### From crates.io

```sh
cargo install clever-project
```

### From source

```sh
git clone https://github.com/mathieuancelin/clever-project.git
cd clever-project
cargo install --path .
# or just:
cargo build --release
./target/release/clever-project --help
```

## Quick start

```yaml
# project.yaml
name: my-project
org: orga_xxxxxx-xxxx-xxxxx
region: par
variables:
  domain: example.com
apps:
  api:
    name: ${env}-api
    kind: node
    source:
      from: https://github.com/me/my-api.git
    domains:
      - api.${env}.${domain}
    env:
      NODE_ENV: ${env}
      PORT: "8080"
    dependencies:
      - db
addons:
  db:
    name: ${env}-api-db
    kind: postgresql
    size: xs_sml
    crypted: true
```

```sh
# Save the file as `project.clever.yaml` and `clever-project` finds it
# automatically — no path argument needed.

# Create everything for env=prod
clever-project apply --env prod

# Same project, different env — every ${env} reference flips
clever-project apply --env staging

# Preview without changing anything
clever-project apply --env prod --dry-run

# Tear it all down for one env
clever-project delete --env staging
```

## Commands

### Project file lookup

`apply`, `delete` and `check` take the project file as a positional argument. If you omit it, the CLI looks for one of these (in order) in the current working directory:

1. `project.clever.yaml`
2. `project.clever.yml`
3. `project.clever.json`

If none exists, the run aborts with a clear error pointing you at the missing descriptor. Naming your file `project.clever.yaml` and running `clever-project apply --env prod` (no path) is the recommended workflow.

### `apply`

Create or update the resources described by the project file. The project file is the source of truth: existing apps have their `env`, `domains`, `scalability` and service links replaced to match. Addons that already exist are left untouched (only their existence is reconciled).

```
clever-project apply [FILE] [OPTIONS]
```

Options:

| Flag | Description |
|---|---|
| `--org <ID>` | Override `org` from the project file |
| `--region <REGION>` | Override the default `region` |
| `--env <VALUE>` | Set `${env}` (default `prod`) |
| `--variable key=value` | One-off variable override (repeatable) |
| `--variable-path <FILE>` | Load variable overrides from a YAML/JSON file (repeatable) |
| `--secrets-path <FILE>` | Explicit secrets file (otherwise auto-discovered, see below) |
| `--dry-run` | Read current state and log mutations as `[dry-run] would ...`, no changes applied |
| `-v, --verbose` | More log lines (`debug` level) |

Apps with a GitHub `source.from` are created with `clever create --github owner/repo`. Non-GitHub sources create an empty app — push your code to the Clever remote yourself afterwards.

**Phases.** `apply` runs in this order: (1) addons, (2) apps create/update, (3) service links between apps and addons, (4) network groups (create + member sync), (5) restarts. Each phase only mutates Clever when the diff requires it.

**Restarts.** At the end of an `apply`, the CLI calls `clever restart --app <id> --quiet` for each app that needs it:

- newly created from a GitHub source (kicks off the first deployment),
- existing app whose `env` was changed during the run,
- existing app whose linked services (`dependencies`) were changed.

Newly created apps *without* a GitHub source are not restarted (no code to deploy yet — push to the Clever remote yourself). Domain or scalability changes alone don't trigger a restart.

### `delete`

Delete the resources listed in the project file. Network groups are removed first (releasing members), then apps before addons (so service links are released first). Anything that's already gone is skipped with a warning — `delete` is best-effort.

```
clever-project delete [FILE] [OPTIONS]
```

Same flags as `apply` (minus `--region`).

### `check`

Validate a project file without contacting Clever Cloud for any mutation. Useful in pre-commit hooks and CI.

```
clever-project check [FILE] [OPTIONS]
```

Runs, in order:

1. **YAML/JSON syntax** and project schema parsing.
2. **Variable interpolation** — every `${...}` reference must resolve. Catches typos, missing entries in `variables:`, `--variable-path`, `--variable`, or in the active `.secrets` file.
3. **App `kind`** — must be one of the supported types (case-insensitive, `java` alias accepted).
4. **Region** — root, per-app, per-addon, plus `--region` override.
5. **Dependencies** — every `dependencies:` entry must be a project key under `apps:` or `addons:`; self-dependencies are rejected.
6. **Network group links** — every `link:` entry in `network_groups:` must be a project key under `apps:` or `addons:`.
7. **Name uniqueness** — two apps (or two addons, or two network groups) can't resolve to the same `name`. Cross-type collisions (app vs addon vs NG) are allowed.
8. **Addon catalog (live API)** — addon `kind`, `size`, and per-addon `region` are checked against the live `clever curl /v2/products/addonproviders`.
9. **App flavor catalog (live API)** — `scalability.instances.minSize` / `maxSize` are checked against `clever curl /v2/products/instances`.

Same variable/env flags as `apply` (`--org`, `--region`, `--env`, `--variable`, `--variable-path`, `--secrets-path`). Plus:

| Flag | Description |
|---|---|
| `--offline` | Skip steps 8 and 9 (live API). Static validation still runs. Useful when no `clever login` is available. |

Exit code: 0 on success, non-zero on first detected issue.

### `read`

Reverse-engineer a project file from an existing org. Useful for bootstrapping.

```
clever-project read --org <ID> [--app NAME_OR_ID]... [--addon NAME_OR_ID]... [--all] -o <FILE>
```

| Flag | Description |
|---|---|
| `--org <ID>` | Source organisation |
| `--app <NAME\|ID>` | Read this app (repeatable) |
| `--addon <NAME\|ID>` | Read this addon (repeatable) |
| `--all` | Read every app and addon in the org (mutually exclusive with `--app`/`--addon`) |
| `-o, --output <FILE>` | Output path (`.yaml` / `.yml` / `.json`) |

## Project file format

YAML or JSON, detected by extension.

```yaml
name: <project name>
description: <optional>
org: orga_xxxxxxxx
region: par
variables: { ... }     # see Variables
apps:
  <key>:
    name: <clever app name>            # required; usually templated with ${env}
    kind: node                         # clever instance type — see the list below
    region: par                        # optional; defaults to project region
    source:                            # optional
      from: https://github.com/owner/repo.git
      branch: main
    domains: [foo.example.com]
    scalability:
      auto: false
      instances:
        minNumber: 1
        maxNumber: 2
        minSize: XS
        maxSize: S
    dependencies: [<other-project-key>, ...]
    env:
      KEY: value
addons:
  <key>:
    name: <clever addon name>
    kind: postgresql                   # see provider mapping below
    size: xs_sml                       # plan slug
    crypted: true                      # encryption-at-rest (best-effort, may not apply to every provider)
    region: par
    version: "16"
network_groups:
  <key>:
    name: <clever ng label>            # the Network Group label on Clever
    description: <optional>            # free-form text
    link:                              # project keys (apps and/or addons) to attach
      - api
      - db
```

- Resource references inside `dependencies:` use the **project keys** (`db`, `api`, etc.), not Clever names or ids.
- Network group `link:` entries are project keys too. Apps are attached via their `app_xxx` id, addons via the underlying provider id (`postgresql_xxx`, `redis_xxx`, ...) which `clever-project` tracks automatically in the state file.
- App `kind:` must be one of: `docker`, `dotnet`, `elixir`, `frankenphp`, `go`, `gradle`, `haskell`, `jar`, `linux`, `maven`, `meteor`, `node`, `php`, `play1`, `play2`, `python`, `ruby`, `rust`, `sbt`, `static`, `static-apache`, `v`, `war`. Values are matched case-insensitively, and `java` is accepted as an alias for `jar`. Anything else is rejected at load time with the full list.
- `region:` (root, per-app or per-addon) must be one of: `par`, `parhds`, `scw`, `grahds`, `ldn`, `mtl`, `rbx`, `rbxhds`, `sgp`, `syd`, `wsw`. Unknown regions are rejected at load time (this also applies to `--region` overrides).
- Addon `kind:` accepts the short form (`postgresql`, `redis`, `cellar`, `matomo`, ...) and is mapped to the right Clever provider id (`postgresql-addon`, `redis-addon`, `cellar-addon`, `addon-matomo`, ...). Unknown values pass through unchanged.
- Addon `kind`, `size`, and `region` are validated at the start of every `apply` against the live provider catalog returned by Clever's API (`clever curl /v2/products/addonproviders?orgaId=...`). Typos and unsupported combinations fail fast, before any mutation. Plan slug casing is normalized to the canonical value from the API (so `size: S_BIG` works even though Clever expects `s_big` for PostgreSQL). Skipped automatically when the project has no addons.
- App `scalability.instances.minSize` / `maxSize` are validated at the start of every `apply` against the live instance catalog (`clever curl /v2/products/instances?for=...`). Unknown flavors are rejected with the list of valid sizes for that kind, and casing is normalized to Clever's canonical form (`s` → `S`, etc.). Skipped if no app declares a flavor.

## Variables

The `variables:` section supports two shapes — pick the one that fits.

### Flat form

```yaml
variables:
  domain: foo.bar
  apikey: shared-secret
```

Every variable is always available regardless of `${env}`.

### Per-env form

```yaml
variables:
  common:
    domain: foo.bar     # available in every env
  prod:
    apikey: secret_for_prod
  dev:
    apikey: secret_for_dev
    domain: dev.bar     # overrides common when ${env}=dev
```

The group named `common` is always merged in, then the group whose name matches the resolved value of `${env}` is merged on top.

The active env is picked, in priority order:
1. `--env <value>` on the command line
2. `--variable env=<value>`
3. default `prod`

If `${env}` doesn't match any per-env group, only `common` applies — references to env-specific variables will fail loudly.

### Special variables

Always available, can't be redefined in `variables:`:
- `${env}` — defaults to `prod`, overridable by `--env`
- `${org}` — comes from the project file's `org` (or `--org`)
- `${region}` — comes from the project file's `region` (or `--region`)

### Loading variables from a file

```yaml
# vars.yaml
domain: example.com
apikey: from-file
```

```sh
clever-project apply project.yaml --variable-path vars.yaml
```

The flag is repeatable; later files override earlier ones.

### Precedence (low → high)

1. Project file `variables:` section (group merged with `common` if per-env form)
2. `--variable-path FILE` entries (in order; later files win)
3. `--variable foo=bar` entries
4. `--env <value>` for the special `${env}` variable

The two `variables:` shapes can't be mixed — every top-level value must be either all scalars (flat) or all mappings (per-env).

## Secrets

Anything you don't want committed (API keys, tokens, passwords) lives in a sidecar `.secrets` file and is referenced from the project file using the namespaced `${secrets.<key>}` syntax.

### Lookup order

Given a project file `myproj.yaml` and an active `${env}` value of e.g. `dev`:

1. If `--secrets-path FILE` is given: **only that file** is loaded (and it must exist).
2. Otherwise, both files below are auto-discovered next to the project file, when present:
   - `myproj.secrets` — env-agnostic defaults
   - `myproj.dev.secrets` — env-specific overrides (the basename matches the `${env}` value)

   When both exist, entries from the env-specific file override the defaults.

If neither file exists, the secrets map is simply empty. Referencing `${secrets.X}` with no value for `X` errors out.

### File format

A flat `Map<String, scalar>`. The content can be **YAML or JSON** — both parsers are tried and the first one that succeeds wins. The file name is the same either way.

```yaml
# myproj.secrets  (YAML)
apikey: shared-secret
db_password: hunter2
```

```json
{
  "apikey": "shared-secret",
  "db_password": "hunter2"
}
```

### Using secrets inside `variables:`

Secrets are expanded before the variables section is processed, so you can compose other variables from them:

```yaml
variables:
  api_url: https://api.example.com/?token=${secrets.apikey}
apps:
  api:
    name: my-app
    kind: node
    env:
      API_URL: ${api_url}
      RAW_KEY: ${secrets.apikey}
```

The default `.gitignore` excludes `*.secrets` — keep it that way.

## State file

After every successful `apply` or `delete`, the CLI writes a sidecar `<project>.state` JSON file next to the project file. It records the Clever resources managed by that project so subsequent runs can resolve `name → id` without an org-wide `clever ... list`.

### Format

```json
[
  {
    "kind": "app",
    "id": "app_xxxxxxx-xxxxx-xxxxxx",
    "org_id": "orga_xxxxxx-xxxx-xxxxx",
    "region": "par",
    "env": "prod",
    "name": "prod-api"
  },
  {
    "kind": "addon",
    "id": "addon_xxxxxx-xxxxxx-xxxxxx",
    "org_id": "orga_xxxxxx-xxxx-xxxxx",
    "region": "par",
    "env": "prod",
    "name": "prod-api-db"
  }
]
```

`kind` is `app` or `addon`. Multiple envs coexist (names differ because of `${env}` interpolation), so applying with `--env dev` and `--env prod` against the same project file both write to the same `<project>.state` without clobbering each other.

### Stale entries (out-of-sync state)

If a resource is deleted out of band (Clever console, raw `clever delete`, a teammate), the state's `id` for it becomes stale. The CLI handles this automatically: when a call against a state-known id fails, the entry is dropped, listings are refreshed from `clever`, and the operation is retried with the corrected id (or skipped with a warning if the resource truly doesn't exist anymore). The state file is rewritten at the end of the run so it's correct for the next invocation.

You shouldn't need to delete `<project>.state` by hand — but you can, and the next run will rebuild it from scratch.

The default `.gitignore` excludes `*.state` — it's a per-machine cache.

## Behaviour notes / limitations

- **Source code push is not handled.** GitHub sources get `clever create --github owner/repo`; for non-GitHub sources the app is created empty and you deploy via Clever's git remote yourself.
- **`apply` is full-replace.** Existing apps have their env vars, domains, scalability and service links overwritten to match the project file. Domains served by `*.cleverapps.io` are never removed (they're auto-managed by Clever).
- **Addons aren't updated** if they already exist (plan, version, etc. stay as-is). Only their existence is reconciled.
- **`clever config`** isn't supported — `clever-tools` doesn't expose it as JSON. The `config:` field is parsed but ignored on both `read` and `apply`.
- **`scalability` on `read`** isn't populated — `clever scale` has no read mode. You'll need to add it manually if you read-bootstrap a project.
- **`apply` is sequential** and stops at the first error (except on `delete`, which is best-effort and continues).
- **Verbose logging**: pass `-v` / `--verbose` to see the underlying `clever` commands and per-step state lookups.

## Build & test

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs all four on Linux, macOS and Windows on every push and pull request (`.github/workflows/ci.yml`).

## Releasing

Tag-driven. Pushing an annotated tag matching `v*.*.*` runs `.github/workflows/release.yml` which:

1. Checks `Cargo.toml`'s `version` matches the tag and creates a GitHub Release (with auto-generated notes).
2. Builds the binary across a matrix of targets in parallel (Linux x86_64 gnu+musl, Linux aarch64 gnu+musl, macOS x86_64+aarch64, Windows x86_64+aarch64) and uploads each archive to the release.
3. Publishes to crates.io with `cargo publish --locked`.

Setup:

- Add `CARGO_REGISTRY_TOKEN` to the repo secrets (Settings → Secrets and variables → Actions). The publish job runs in a `crates-io` environment — create that environment if you want manual approval before publishing.
- Cut a release with the helper script (recommended — it does all the safety checks for you):

  ```sh
  ./scripts/release.sh 0.2.0
  ```

  The script bumps `Cargo.toml`, runs fmt/clippy/tests, commits, tags `v0.2.0`, and pushes (with confirmation prompts). It refuses to run if the tree is dirty, you're not on `main`, the tag already exists, or `main` is out of sync with `origin`.

  To do it manually instead:

  ```sh
  # 1. bump version in Cargo.toml + Cargo.lock, commit
  # 2. tag and push
  git tag -a v0.2.0 -m "v0.2.0"
  git push origin main
  git push origin v0.2.0
  ```
