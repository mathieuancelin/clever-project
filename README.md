# clever-project

A Rust CLI that syncs a project description (YAML/JSON) with the resources of a Clever Cloud organisation. The actual orchestration is delegated to the official `clever-tools` CLI.

See [specs.md](./specs.md) for the detailed specification.

## Prototype status

Legend: ✅ done · 🚧 in progress · ⏳ todo · ⛔ out of scope for the prototype

### Done
- ✅ Cargo skeleton + dependencies (`clap`, `serde`, `serde_yaml`, `serde_json`, `anyhow`, `tracing`, `which`, `indexmap`, `regex`)
- ✅ Data model + YAML/JSON loading (format detected by file extension)
- ✅ `${var}` interpolation (file `variables:` section, `--variable foo=bar` CLI overrides take precedence, special variables `${env}`/`${org}`/`${region}`, reserved-name rejection, hard error on missing variable)
- ✅ Per-env variables: the `variables:` section accepts either a flat `key: value` map (default) or a `group → key: value` map keyed by `${env}` with a special `common` group always merged in
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
- ✅ 23 unit tests green, build with no warnings

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
- ⛔ **`crypted`** on addons: passed as `--option encryption=true` at creation (to be validated per provider), not detected on `read`.
- ⛔ **`backup_path`** on addons: field present, not handled.
- ⛔ **Rollback** on partial failure: not implemented (stop + log).
- ⛔ **Parallelism**: sequential.
- ⛔ Auto-managed `*.cleverapps.io` domains: excluded from `read` and never removed by `apply`.

## Usage

```sh
# Read existing resources to bootstrap a project file
clever-project read --org orga_xxx --app frontend --addon main-db -o project.yaml

# Read everything in an org
clever-project read --org orga_xxx --all -o project.yaml

# Apply a project file
clever-project apply project.yaml [--org ...] [--region ...] [--env staging] [--variable foo=bar] [--dry-run]

# Delete the resources listed in a project file
clever-project delete project.yaml [--org ...] [--env staging] [--dry-run]

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

### Rules
- The two shapes can't be mixed — every top-level value must be either all scalars (flat) or all mappings (per-env).
- The reserved names `env`, `org`, `region` cannot be redefined in `variables:` (any form).
- `--variable foo=bar` on the CLI overrides any value from the file, regardless of the form.
- If `${env}` doesn't match any per-env group, only `common` is available — references to env-specific variables will fail loudly.

## Code layout

```
src/
├── main.rs              # entry point + tracing init
├── cli.rs               # clap (Cli, Command, ReadArgs, ApplyArgs, DeleteArgs)
├── model.rs             # Project / App / Addon / ... + load_and_resolve
├── interpolate.rs       # Resolver: ${var}, special variables, Value walk
├── clever.rs            # Command::new("clever") wrapper + typed helpers
└── commands/
    ├── mod.rs
    ├── apply.rs         # 3 phases: addons → apps (create/update) → service links
    ├── delete.rs        # apps then addons, looked up by name
    └── read.rs          # org introspection → Project
```

## Build & tests

```sh
cargo build
cargo test
```
