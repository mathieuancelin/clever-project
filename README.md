# clever-project

Declare your Clever Cloud resources in a YAML or JSON file and sync them with a single command. `clever-project` reads the project file and drives the official `clever-tools` CLI to create, update or delete the corresponding apps and addons.

```sh
clever-project apply project.yaml --env prod
clever-project delete project.yaml --env staging --dry-run
clever-project read --org orga_xxx --all -o project.yaml
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

```sh
cargo install --path .
# or, from a checkout:
cargo build --release
./target/release/clever-project --help
```

## Quick start

```yaml
# project.yaml
name: my-project
org: orga_004eedf6-2624-4030-b549-9d4895934f13
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
# Create everything for env=prod
clever-project apply project.yaml --env prod

# Same project, different env — every ${env} reference flips
clever-project apply project.yaml --env staging

# Preview without changing anything
clever-project apply project.yaml --env prod --dry-run

# Tear it all down for one env
clever-project delete project.yaml --env staging
```

## Commands

### `apply`

Create or update the resources described by the project file. The project file is the source of truth: existing apps have their `env`, `domains`, `scalability` and service links replaced to match. Addons that already exist are left untouched (only their existence is reconciled).

```
clever-project apply <FILE> [OPTIONS]
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

**Restarts.** At the end of an `apply`, the CLI calls `clever restart --app <id> --quiet` for each app that needs it:

- newly created from a GitHub source (kicks off the first deployment),
- existing app whose `env` was changed during the run,
- existing app whose linked services (`dependencies`) were changed.

Newly created apps *without* a GitHub source are not restarted (no code to deploy yet — push to the Clever remote yourself). Domain or scalability changes alone don't trigger a restart.

### `delete`

Delete the resources listed in the project file. Apps are removed before addons so service links are released first. Anything that's already gone is skipped with a warning — `delete` is best-effort.

```
clever-project delete <FILE> [OPTIONS]
```

Same flags as `apply` (minus `--region`).

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
    kind: node                         # clever instance type (node, jar, python, ...)
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
```

- Resource references inside `dependencies:` use the **project keys** (`db`, `api`, etc.), not Clever names or ids.
- Addon `kind:` accepts the short form (`postgresql`, `redis`, `cellar`, `matomo`, ...) and is mapped to the right Clever provider id (`postgresql-addon`, `redis-addon`, `cellar-addon`, `addon-matomo`, ...). Unknown values pass through unchanged.
- Java apps: Clever lists them with `type: jar`, but `kind: java` in the project file is accepted too — they're treated as equivalent on update.

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
    "id": "app_e9ba25b2-0168-46df-849c-fedf016f3c28",
    "org_id": "orga_004eedf6-2624-4030-b549-9d4895934f13",
    "region": "par",
    "env": "prod",
    "name": "prod-api"
  },
  {
    "kind": "addon",
    "id": "addon_84758ebf-db0d-40b5-8857-be5b7dce51e6",
    "org_id": "orga_004eedf6-2624-4030-b549-9d4895934f13",
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
- **Java apps**: Clever reports them as `type: jar`, but `kind: java` in the project file is accepted (treated as equivalent).
- **Verbose logging**: pass `-v` / `--verbose` to see the underlying `clever` commands and per-step state lookups.

## Build & test

```sh
cargo build
cargo test
```
