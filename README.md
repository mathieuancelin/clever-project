# clever-project

A Rust CLI that syncs a project description (YAML/JSON) with the resources of a Clever Cloud organisation. The actual orchestration is delegated to the official `clever-tools` CLI.

See [specs.md](./specs.md) for the detailed specification.

## Prototype status

Legend: ✅ done · 🚧 in progress · ⏳ todo · ⛔ out of scope for the prototype

### Done
- ✅ `${var}` interpolation (file `variables:` section, `--variable foo=bar` CLI overrides take precedence, special variables `${env}`/`${org}`/`${region}`, reserved-name rejection, hard error on missing variable)
- ✅ Per-env variables: the `variables:` section accepts either a flat `key: value` map (default) or a `group → key: value` map keyed by `${env}` with a special `common` group always merged in
- ✅ Secrets: `${secrets.foo}` references resolved from a `<project>.secrets` (or `<project>.<env>.secrets`) sidecar file or from `--secrets-path FILE`. Usable inside the `variables:` section too.
- ✅ `--variable-path FILE` (repeatable): load CLI-level variable overrides from a YAML/JSON file (flat `key: value` map), same precedence layer as `--variable foo=bar` but lower priority.
- ✅ `clever-tools` wrapper (read + write):
    - read: `list_apps`, `list_addons`, `get_env`, `get_domains`, `get_services`
    - write: `create_app`, `delete_app`, `create_addon`, `delete_addon`, `env_replace`, `domain_add`, `domain_rm`, `scale`, `link_addon`/`unlink_addon`, `link_app`/`unlink_app`
- ✅ **`delete`** command: removes the apps then addons listed in the project, looked up by `name` in the org. Stops and logs on the first error.
- ✅ **`apply`** command:
    - phase 1: create missing addons (existing ones are left untouched, with a warning if the `kind` diverges)
    - phase 2: create missing apps (with `--github owner/repo` when the source is a GitHub URL); for existing apps, full-replace `env`, `domains`, `scalability` *iff* `kind`+`source` match, otherwise warn and skip
    - phase 3: resolve `dependencies` (project keys → clever ids) and link/unlink via `clever service` to converge
- ✅ **`read`** command: reads the explicitly requested apps/addons (`--app`/`--addon`, repeatable, by name or id) or `--all`, fetches env + domains + dependencies, writes the output file as `yaml` or `json` based on the `-o` extension
- ✅ Provider-name mapping for addon creation (`postgresql` → `postgresql-addon`, `cellar` → `cellar-addon`, `matomo` → `addon-matomo`, etc. — pass-through for anything unknown)
- ✅ `--env <value>` shortcut on `apply` and `delete` to set the special `${env}` variable
- ✅ `--dry-run` flag on `apply` and `delete`: reads current state but logs `[dry-run]` mutations instead of executing them
- ✅ Local state file (`<project>.state`, JSON): records `{kind, id, org_id, region, env, name}` per resource; used as a state-first cache to avoid org-wide `clever ... list` calls on `apply`/`delete`. Falls back to listing when a resource is missing from state.
- ✅ 38 unit tests green, build with no warnings

### Decisions

| Topic | Choice |
|---|---|
| `clever` on PATH | Prerequisite; no nvm/bun handling inside the CLI |
| GitHub source | `clever create --github owner/repo`; the user deploys their own code afterwards |
| Non-GitHub source | App created empty, warning logged; user deploys via Clever's internal git remote |
| Diff on `apply` | Full replace: env, domains, dependencies, scalability. The file is the source of truth. |
| `read --all` | Available but not the default |
| Partial failure | Stop + log, no rollback |
| Format detection | By `.yaml`/`.yml`/`.json` extension |
| Parallelism | Sequential for the prototype |
| `delete` order | Apps first, then addons (releases service links before touching addons) |

### Known limitations / out of scope for the prototype
- ⛔ **`config`** (`clever config`): not exposed in JSON by `clever-tools`. Ignored on both `read` and `apply`. The field is kept in the model for later.
- ⛔ **`scalability` on `read`**: `clever scale` has no read/JSON mode. The section can't be regenerated from existing resources (`scalability: None`).
- ⛔ **Addon updates**: if an addon already exists, no update is performed (neither `size` nor `version`). A warning is logged if the `kind`/`providerId` diverges.
- ⛔ **`network_groups`**: field present in the model, not handled.
- ⛔ **`backup_path`** on addons: field present, not handled.
- ⛔ **Rollback** on partial failure: not implemented (stop + log).
- ⛔ **Parallelism**: sequential.

## Usage

```sh
# Read existing resources to bootstrap a project file
clever-project read --org orga_xxx --app frontend --addon main-db -o project.yaml

# Read everything in an org
clever-project read --org orga_xxx --all -o project.yaml

# Apply a project file
clever-project apply project.yaml [--org ...] [--region ...] [--env staging] [--variable foo=bar] [--variable-path vars.yaml] [--secrets-path FILE] [--dry-run]

# Delete the resources listed in a project file
clever-project delete project.yaml [--org ...] [--env staging] [--variable-path vars.yaml] [--secrets-path FILE] [--dry-run]

# real world example app creation
clever-project apply ./test_project.yaml --org orga_xxxxxxx --env dev
clever-project apply ./test_project.yaml --org orga_xxxxxxx --env prod
clever-project apply ./test_project.yaml --org orga_xxxxxxx --env dev --region rbx
clever-project apply ./test_project.yaml --org orga_xxxxxxx --env prod --region rbx

# real world example app delete
clever-project delete ./test_project.yaml --org orga_xxxxxxx --env dev
clever-project delete ./test_project.yaml --org orga_xxxxxxx --env prod
clever-project delete ./test_project.yaml --org orga_xxxxxxx --env dev --region rbx
clever-project delete ./test_project.yaml --org orga_xxxxxxx --env prod --region rbx
```

Verbose mode:
```sh
clever-project --verbose apply project.yaml
```

## Variables

The `variables:` section of a project file supports two shapes — pick the one that fits.

### Flat form

A simple `key: value` map. Every variable is always available regardless of `${env}`.

```yaml
variables:
  domain: foo.bar
  apikey: shared-secret
```

### Per-env form

A `group → key: value` map. The group named `common` is always merged in, then the group whose name matches the resolved value of `${env}` is merged on top (env-specific entries override `common`).

```yaml
variables:
  common:
    domain: foo.bar     # available in every env
  prod:
    apikey: secret_for_prod
  dev:
    apikey: secret_for_dev
    domain: dev.bar     # overrides common for ${env}=dev
```

The active env is picked, in priority order:
1. `--env <value>` on the command line
2. `--variable env=<value>`
3. default `prod`

So with the example above:
- `clever-project apply project.yaml` (env defaults to `prod`) → `domain=foo.bar`, `apikey=secret_for_prod`
- `clever-project apply project.yaml --env dev` → `domain=dev.bar`, `apikey=secret_for_dev`
- `clever-project apply project.yaml --env staging` → only `common` applies; any reference to `${apikey}` errors out

### Loading variables from a file (`--variable-path`)

You can also load CLI-level overrides from a YAML or JSON file (flat `key: value` map, scalars only). The flag is repeatable; later files override earlier ones.

```yaml
# vars.yaml
domain: example.com
apikey: from-file
```

```sh
clever-project apply project.yaml --variable-path vars.yaml
```

### Precedence (low → high)
1. Project file `variables:` section (group merged with `common` if per-env form)
2. `--variable-path FILE` entries (in order, later files override earlier ones)
3. `--variable foo=bar` entries
4. `--env <value>` for the special `${env}` variable

### Rules
- The two shapes can't be mixed — every top-level value must be either all scalars (flat) or all mappings (per-env).
- The reserved names `env`, `org`, `region` cannot be redefined in `variables:` (any form).
- `--variable foo=bar` on the CLI overrides any value from the file or from `--variable-path`.
- If `${env}` doesn't match any per-env group, only `common` is available — references to env-specific variables will fail loudly.

## Secrets

Anything you don't want committed (API keys, tokens, passwords) lives in a sidecar `.secrets` file and is referenced from the project file using the namespaced `${secrets.<key>}` syntax.

### Lookup order

Given a project file `myproj.yaml` and an active `${env}` value of e.g. `dev`:

1. If `--secrets-path FILE` is given on the command line: **only that file** is loaded (and it must exist).
2. Otherwise, both files below are auto-discovered next to the project file, when present:
   - `myproj.secrets` — env-agnostic defaults
   - `myproj.dev.secrets` — env-specific overrides (the basename matches the `${env}` value)

   When both exist, entries from the env-specific file override the defaults.

If neither file exists and no `--secrets-path` is given, the secrets map is simply empty. Referencing `${secrets.X}` with no value for `X` errors out.

### File format

A flat `Map<String, scalar>`. The content can be **YAML or JSON** — both parsers are tried and the first one that succeeds wins. The file name is the same either way (`<stem>.secrets` / `<stem>.<env>.secrets`).

```yaml
# myproj.secrets  (YAML)
apikey: shared-secret
db_password: hunter2
```

```json
// myproj.secrets  (JSON — same name, same lookup)
{
  "apikey": "shared-secret",
  "db_password": "hunter2"
}
```

If a file is neither valid YAML nor valid JSON, both parser errors are surfaced so you can see what went wrong.

### Use anywhere — including inside `variables:`

Secrets are expanded before the variables section is processed, so you can use them to compose other variables:

```yaml
# myproj.yaml
variables:
  api_url: https://api.example.com/?token=${secrets.apikey}
apps:
  a:
    name: my-app
    kind: node
    env:
      API_URL: ${api_url}
      RAW_KEY: ${secrets.apikey}
```

### gitignore

The default `.gitignore` already excludes `*.secrets`. Keep it that way — these files are meant to be local-only or distributed through a secret manager out-of-band.

## State file

After every `apply` and `delete`, the CLI writes a sidecar `<project>.state` JSON file next to the project file. It records the resources currently managed by that project so that the next run can resolve `name → id` correlations without paying for an org-wide `clever ... list` round-trip.

### Format

A JSON array, one entry per resource:

```json
[
  {
    "kind": "app",
    "id": "app_e9ba25b2-0168-46df-849c-fedf016f3c28",
    "org_id": "orga_004eedf6-2624-4030-b549-9d4895934f13",
    "region": "par",
    "env": "prod",
    "name": "prod-apptest"
  },
  {
    "kind": "addon",
    "id": "addon_84758ebf-db0d-40b5-8857-be5b7dce51e6",
    "org_id": "orga_004eedf6-2624-4030-b549-9d4895934f13",
    "region": "par",
    "env": "prod",
    "name": "prod-apptest-cellar"
  }
]
```

`kind` is one of `app` or `addon`. Multiple envs coexist in the same file (names differ because of `${env}` interpolation), so running `apply --env dev` and `apply --env prod` both write to the same `<project>.state` and entries don't clobber each other.

### Lookup behaviour

On `apply` and `delete`, for each `(name, org_id)` lookup the CLI:
1. checks the state file first — if there's a matching entry, the id is used directly;
2. otherwise falls back to a single `clever applications list` or `clever addon list` call, then caches that listing in-memory for the rest of the run.

After every successful create or delete, the state file is updated and written back to disk. On `apply`, when state already has the resource, the kind/source diff check is skipped (state is presumed authoritative since the entry was recorded when the CLI itself created/managed the resource).

### Out-of-sync state

If a resource was deleted out-of-band (Clever console, raw `clever delete`, a colleague's terminal, etc.) the state's `id` becomes stale. The CLI handles this automatically:

- **`apply`** — for apps, the first clever call against a state-known id (the `env` import in `update_app`) acts as a probe. If it fails, the stale state entry is dropped, the in-memory listing cache is invalidated, and we fall back to looking up the resource by name in a fresh `clever applications list`. Addons aren't probed in phase 1, but phase 3 (`clever service link-...`) does the same one-shot retry: if a link fails, state is refreshed against fresh listings, the dep maps are rebuilt, and phase 3 is replayed once.
- **`delete`** — `clever delete` against a state-known id is the probe. On failure the entry is dropped, listings are refreshed, and the delete is retried via the fresh id (if the resource actually exists under a different id) or skipped with a warning.

In every case the state file is rewritten at the end of the run so subsequent invocations start from a corrected snapshot. You shouldn't need to delete `<project>.state` by hand — but you can, and the next run will rebuild it from scratch.

### gitignore

The default `.gitignore` excludes `*.state`. The file is a per-machine cache that's safe to regenerate from clever-tools, so there's no value in committing it.

## Code layout

```
src/
├── main.rs              # entry point + tracing init
├── cli.rs               # clap (Cli, Command, ReadArgs, ApplyArgs, DeleteArgs)
├── model.rs             # Project / App / Addon / ... + load_and_resolve + secrets + variable files
├── interpolate.rs       # Resolver: ${var}, special variables, Value walk
├── clever.rs            # Command::new("clever") wrapper + typed helpers
├── state.rs             # <project>.state sidecar (load/save/find/upsert)
└── commands/
    ├── mod.rs
    ├── apply.rs         # 3 phases: addons → apps (create/update) → service links — state-first lookups
    ├── delete.rs        # apps then addons, state-first lookups with listing fallback
    └── read.rs          # org introspection → Project
```

## Build & tests

```sh
cargo build
cargo test
```
