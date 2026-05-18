# clever-project

A Rust CLI that syncs a project description (YAML/JSON) with the resources of a Clever Cloud organisation. The actual orchestration is delegated to the official `clever-tools` CLI.

See [specs.md](./specs.md) for the detailed specification.

## Prototype status

Legend: ✅ done · 🚧 in progress · ⏳ todo · ⛔ out of scope for the prototype

### Done
- ✅ Cargo skeleton + dependencies (`clap`, `serde`, `serde_yaml`, `serde_json`, `anyhow`, `tracing`, `which`, `indexmap`, `regex`)
- ✅ Data model + YAML/JSON loading (format detected by file extension)
- ✅ `${var}` interpolation (file `variables:` section, `--variable foo=bar` CLI overrides take precedence, special variables `${env}`/`${org}`/`${region}`, reserved-name rejection, hard error on missing variable)
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
- ✅ 19 unit tests green, build with no warnings

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
- ⛔ **Source code push**: no automatic clone/push. GitHub source → `--github`. Otherwise the app is created empty.
- ⛔ **`config`** (`clever config`): not exposed in JSON by `clever-tools`. Ignored on both `read` and `apply`. The field is kept in the model for later.
- ⛔ **`scalability` on `read`**: `clever scale` has no read/JSON mode. The section can't be regenerated from existing resources (`scalability: None`).
- ⛔ **Addon updates**: if an addon already exists, no update is performed (neither `size` nor `version`). A warning is logged if the `kind`/`providerId` diverges.
- ⛔ **`network_groups`**: field present in the model, not handled.
- ⛔ **`crypted`** on addons: passed as `--option encryption=true` at creation (to be validated per provider), not detected on `read`.
- ⛔ **`backup_path`** on addons: field present, not handled.
- ⛔ **Rollback** on partial failure: not implemented (stop + log).
- ⛔ **`--dry-run`** mode: not implemented.
- ⛔ **Parallelism**: sequential.
- ⛔ Auto-managed `*.cleverapps.io` domains: excluded from `read` and never removed by `apply`.

### Still open (to validate in real use)
- The `--option encryption=true` flag on addon creation is best-effort; the option name needs to be confirmed per provider (PostgreSQL supports it under this name, to be confirmed for Redis/Cellar/etc.).
- `clever create --type java` vs. `clever applications list` returning `type: jar`: we tolerate both (mapping `java` ↔ `jar`).
- Materia KV / Cellar addons have no configurable `version` — to be ignored on `apply`.

## Usage

```sh
# Read existing resources to bootstrap a project file
clever-project read --org orga_xxx --app frontend --addon main-db -o project.yaml

# Read everything in an org
clever-project read --org orga_xxx --all -o project.yaml

# Apply a project file
clever-project apply project.yaml [--org ...] [--region ...] [--env staging] [--variable foo=bar]

# Delete the resources listed in a project file
clever-project delete project.yaml [--org ...] [--env staging]
```

Verbose mode:
```sh
clever-project --verbose apply project.yaml
```

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
