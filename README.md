# clever-project

Declare your [Clever Cloud](https://www.clever.cloud/) resources in a YAML, JSON or TOML file and sync them with a single command. `clever-project` reads the project file and drives the official `clever-tools` CLI to create, update or delete the corresponding apps and addons.

It's built to make spinning up reproducible, parameterised or disposable environments effortless — stand up an identical preview stack per branch, hand a teammate a one-shot sandbox, or tear the whole thing down when you're done, all from the same declarative file.

```sh
clever-project init
clever-project apply project.yaml --env prod
clever-project delete project.yaml --env staging --dry-run
clever-project read --org orga_xxx --all -o project.yaml
clever-project check project.yaml --offline
clever-project status project.yaml
```

Here's what a project file looks like — one app, one database, wired together, with a generated slug for a unique domain:

```yaml
name: my-api
org: orga_xxxxxx-xxxx-xxxxx
region: par
variables:
  slug: ${ulid_lowercase()}
  api_key: ${random_alphanumeric(32)}
display:
  url: https://api-${slug}.cleverapps.io/
apps:
  api:
    name: api-${slug}
    kind: node
    source:
      from: https://github.com/me/my-api.git
      branch: main
    domains:
      - api-${slug}.cleverapps.io
    scalability:
      auto: false
      instances: { minNumber: 1, minSize: S }
    dependencies:
      - db
    env:
      API_KEY: ${api_key}
      DB_HOST: ${addons.db.env.POSTGRESQL_ADDON_HOST}
      DB_URI:  ${addons.db.env.POSTGRESQL_ADDON_URI}
      PUBLIC_URL: https://api-${slug}.cleverapps.io/
addons:
  db:
    name: db-${slug}
    kind: postgresql
    size: xs_sml
```

Highlights: `${ulid_lowercase()}` / `${random_alphanumeric(32)}` are built-in generators evaluated once per run, so every reference to `${slug}` gets the same value. `${addons.db.env.POSTGRESQL_ADDON_HOST}` is a cross-resource ref — `clever-project` creates the addon first, then injects the live value into the app's env. The `display` block is printed at the end of `apply` so you immediately see the URL to open. Looking for more? The [`recipes/`](recipes/) folder ships ready-to-apply project files for common stacks (n8n, more to come) — grab one, tweak the names and `org`, and run `clever-project apply`.

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

The same project, expressed in each supported format. Pick the one you prefer — the CLI accepts all three interchangeably.

### YAML (`project.clever.yaml`)

```yaml
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

### JSON (`project.clever.json`)

```json
{
  "name": "my-project",
  "org": "orga_xxxxxx-xxxx-xxxxx",
  "region": "par",
  "variables": {
    "domain": "example.com"
  },
  "apps": {
    "api": {
      "name": "${env}-api",
      "kind": "node",
      "source": { "from": "https://github.com/me/my-api.git" },
      "domains": ["api.${env}.${domain}"],
      "env": {
        "NODE_ENV": "${env}",
        "PORT": "8080"
      },
      "dependencies": ["db"]
    }
  },
  "addons": {
    "db": {
      "name": "${env}-api-db",
      "kind": "postgresql",
      "size": "xs_sml",
      "crypted": true
    }
  }
}
```

### TOML (`project.clever.toml`)

```toml
name = "my-project"
org = "orga_xxxxxx-xxxx-xxxxx"
region = "par"

[variables]
domain = "example.com"

[apps.api]
name = "${env}-api"
kind = "node"
domains = ["api.${env}.${domain}"]
dependencies = ["db"]

[apps.api.source]
from = "https://github.com/me/my-api.git"

[apps.api.env]
NODE_ENV = "${env}"
PORT = "8080"

[addons.db]
name = "${env}-api-db"
kind = "postgresql"
size = "xs_sml"
crypted = true
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

`apply`, `delete`, `check` and `status` take the project file as a positional argument. If you omit it, the CLI looks for one of these (in order) in the current working directory:

1. `project.clever.yaml`
2. `project.clever.yml`
3. `project.clever.toml`
4. `project.clever.json`

If none exists, the run aborts with a clear error pointing you at the missing descriptor. Naming your file `project.clever.yaml` (or `.toml` / `.json` — your call) and running `clever-project apply --env prod` (no path) is the recommended workflow.

### `init`

Scaffold a fresh `project.clever.yaml` interactively. Useful for getting started from zero — the generated file passes `check --offline` immediately, and contains the canonical `${env}`-templated names so you can `apply --env prod` (or `--env dev`) right away.

```
clever-project init [OPTIONS]
```

Run with no arguments to be prompted for everything:

```
$ clever-project init
Project name: my-stack
Org id (orga_...): orga_xxxxxxxx
Region [par]:
App kind [node]:
GitHub source? [y/N]: y
  owner/repo or full URL: me/my-app
Addons (comma-separated, empty for none): postgresql, redis
```

Or pass everything via flags (great for templates and CI bootstrapping):

```sh
clever-project init \
  --non-interactive \
  --name my-stack \
  --org orga_xxxxxxxx \
  --kind node \
  --source me/my-app \
  --addon postgresql --addon redis \
  -o project.clever.yaml
```

| Flag | Description |
|---|---|
| `--name <NAME>` | Project name (free-form). |
| `--org <ID>` | Clever Cloud organisation id (`orga_xxx`). |
| `--region <REGION>` | Default region. Defaults to `par`. |
| `--kind <KIND>` | App kind (`node`, `docker`, `python`, `jar`, ...). |
| `--source <REPO>` | GitHub source: `owner/repo`, `github.com/owner/repo`, or a full URL. |
| `--no-source` | Explicitly create the project with no source (also `--non-interactive`'s default). |
| `--addon <KIND>` | Provision an addon alongside the app (repeatable). |
| `-o, --output <FILE>` | Output path. Default `project.clever.yaml`. |
| `--non-interactive` | Don't prompt; fail if a required field wasn't passed. |
| `--force` | Overwrite an existing output file. |

Behaviour notes:

- Inputs are validated as you type — invalid `kind` or `region` re-prompts with the list of valid values.
- `owner/repo` shorthand for GitHub sources is normalized to `https://github.com/owner/repo.git`. SSH (`git@…`) and non-GitHub URLs pass through unchanged.
- Project keys are slugified from the project name (`"My Stack"` → `my-stack`), and addons are wired in as dependencies of the app automatically.
- Addon sizes are left unset on purpose — Clever picks its default plan, and you can pin `size:` after reviewing.
- The output refuses to overwrite an existing file unless `--force` is given.

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
| `--variables-file-path <FILE>` | Load variable overrides from a YAML/JSON file (repeatable) |
| `--secrets-file-path <FILE>` | Explicit secrets file (otherwise auto-discovered, see below) |
| `--secret key=value` | One-off secret override (repeatable). Wins over secrets files. |
| `--dry-run` | Print a structured plan against the live org and exit. No mutations sent. |
| `--yes` / `--auto-approve` | Skip the confirmation prompt. Required when stdin is not a TTY. |
| `--target <SPEC>` | Restrict the run to one resource (repeatable). Syntax: `apps.KEY`, `addons.KEY`, `network_groups.KEY` (also `app.`, `addon.`, `ng.`). |
| `--skip-hooks` | Bypass every `pre_*` / `post_*` hook for this run (see [Hooks](#hooks)). |
| `-v, --verbose` | More log lines (`debug` level) |

Apps with a GitHub `source.from` are created with `clever create --github owner/repo`. Non-GitHub sources create an empty app — push your code to the Clever remote yourself afterwards.

**Confirmation gate.** Before touching anything, `apply` prints a structured plan (same one as `--dry-run`, see below) and prompts:

```
Apply these changes? [y/N]:
```

The default is **no** — hitting Enter aborts. Type `y` to proceed.

- Pass `--yes` (or `--auto-approve`) to skip the prompt. This is required when stdin is not a TTY (CI environments, piped invocations): without it, apply fails loud with `stdin is not a TTY and --yes was not given`.
- If the plan has no mutations (everything is already in sync), the prompt is skipped and apply exits 0.
- `--dry-run` always short-circuits: it prints the plan and exits, prompt or no prompt.

**Resource targeting.** Pass `--target` (repeatable) to restrict the run to a subset of the project file:

```sh
clever-project apply --target apps.api
clever-project apply --target addons.db --target apps.worker
```

The argument is `<section>.<key>` where `<section>` is `apps`, `addons`, or `network_groups` (with `app.`, `addon.`, `ng.` accepted as shorter aliases), and `<key>` is the project key under that section (the YAML key, not the resolved `name:`). Typos fail loud at start with a list of the available keys.

When `--target` is set:

- Only the targeted resources go through their normal create/update path.
- Other project resources are read-only — their ids are looked up so phase-3 dependency wiring still works, but they aren't mutated.
- If a targeted app depends on a non-targeted resource that doesn't yet exist anywhere (state or live), apply bails with a clear message: "targeting leaves dependencies unresolved — add the missing `--target` flag, or run a full apply first."
- Restarts (phase 5) only fire for targeted apps.
- The plan header reports the active targets so you can double-check.

**Dry-run plan.** `--dry-run` produces a Terraform-style plan instead of running the phases: the CLI snapshots the live org, computes a per-resource diff against the project file, and prints it in one block:

```
Plan for project `My Stack` against org `orga_xxx` (default region `par`):
  2 to create, 1 to update, 3 unchanged.

  + addon "prod-cache" (redis, xs_sml, region=par)
  = addon "prod-db"
  + app "prod-api" (node, region=par, github=https://github.com/me/api.git)
      env:
        + NODE_ENV = "prod"
        + PORT = "8080"
      domains:
        + api.example.com
      dependencies:
        + prod-cache
        + prod-db
  ~ app "prod-worker"
      env:
        + RUST_LOG = "info"
        - DEBUG = "true"
        ~ PORT: "8080" → "3000"
      domains:
        + worker.example.com
        - old.worker.example.com
  = app "prod-static"
  = network_group "vpn"
```

The plan only counts as "to update" diffs that `apply` will actually rewrite: `env`, `domains`, `dependencies` on existing apps; member set on existing network groups. Drift on fields apply won't auto-update (app `kind`, `region`, `source.from`, anything on existing addons) is surfaced below the verdict with a `!` so you know to recreate manually if needed.

`*.cleverapps.io` domains are filtered out before diffing, matching apply's runtime behaviour (it never removes them).

**Phases.** `apply` runs in this order: (1) addons, (2) apps create/update, (3) service links between apps and addons, (4) network groups (create + member sync), (5) restarts. Each phase only mutates Clever when the diff requires it.

**Restarts.** At the end of an `apply`, the CLI calls `clever restart --app <id> --quiet` for each app that needs it:

- newly created from a GitHub source (kicks off the first deployment),
- existing app whose `env` was changed during the run,
- existing app whose linked services (`dependencies`) were changed.

Newly created apps *without* a GitHub source are not restarted (no code to deploy yet — push to the Clever remote yourself). Domain, scalability, build flavor or branch changes alone don't trigger a restart — a branch change only takes effect on the next push to the Clever remote.

### `delete`

Delete the resources listed in the project file. Network groups are removed first (releasing members), then apps before addons (so service links are released first). Anything that's already gone is skipped with a warning — `delete` is best-effort.

```
clever-project delete [FILE] [OPTIONS]
```

Same flags as `apply` (minus `--region`), including `--yes` / `--auto-approve` and `--target`. With `--target apps.api`, only that resource is queued for deletion; everything else is left alone.

**Confirmation gate.** Like `apply`, `delete` prints a plan and prompts before doing anything:

```
Plan for project `My Stack` against org `orga_xxx`:
  3 to destroy: 1 network_group, 1 app, 1 addon.

  - network_group "vpn"
  - app "prod-api"
  - addon "prod-db"

Destroy these resources? [y/N]:
```

Default is **no**. Pass `--yes` to skip the prompt (required in non-TTY contexts). `--dry-run` prints the plan and exits without prompting.

The plan is built purely from the project file — no Clever API call is made at this stage. Resources that have already been deleted out of band are detected at run time and skipped with a warning (consistent with delete's best-effort semantics).

### `check`

Validate a project file without contacting Clever Cloud for any mutation. Useful in pre-commit hooks and CI.

```
clever-project check [FILE] [OPTIONS]
```

Runs, in order:

1. **YAML/JSON syntax** and project schema parsing.
2. **Variable interpolation** — every `${...}` reference must resolve. Catches typos, missing entries in `variables:`, `--variables-file-path`, `--variable`, or in the active `.secrets` file.
3. **App `kind`** — must be one of the supported types (case-insensitive, `java` alias accepted).
4. **Region** — root, per-app, per-addon, plus `--region` override.
5. **Dependencies** — every `dependencies:` entry must be a project key under `apps:` or `addons:`; self-dependencies are rejected.
6. **Network group links** — every `link:` entry in `network_groups:` must be a project key under `apps:` or `addons:`.
7. **Name uniqueness** — two apps (or two addons, or two network groups) can't resolve to the same `name`. Cross-type collisions (app vs addon vs NG) are allowed.
8. **Addon catalog (live API)** — addon `kind`, `size`, and per-addon `region` are checked against the live `clever curl /v2/products/addonproviders`.
9. **App flavor catalog (live API)** — `scalability.instances.minSize` / `maxSize` are checked against `clever curl /v2/products/instances`.

Same variable/env flags as `apply` (`--org`, `--region`, `--env`, `--variable`, `--variables-file-path`, `--secret`, `--secrets-file-path`). Plus:

| Flag | Description |
|---|---|
| `--offline` | Skip steps 8 and 9 (live API). Static validation still runs. Useful when no `clever login` is available. |

All problems are reported in a single pass — `check` keeps going after the first failure and aggregates everything into one error message:

```
Error: 3 validation problems:
  - undefined variable `apikey2` (in `${apikey2}`)
  - app `api` has unknown dependency `cellar1`: not a project key in `apps:` or `addons:`
  - addon `db` has unknown size `xs_big` for provider `postgresql-addon`. Available sizes: ...
```

The walk only stops short on truly fatal parse failures (YAML/JSON syntax, missing `org` / `region`, mixed `variables:` shape). Everything past parsing — missing variables, missing secrets, unknown kinds, regions, sizes, duplicate names, broken dependencies, broken NG links — accumulates and surfaces together. Exit code: 0 on success, non-zero on the bundled error.

### `status`

Compare a project file against the live state of its Clever Cloud org and report any drift. Read-only — never mutates anything. Together with `check`, it's the "is the file still safe and accurate?" pair: `check` validates the file's internal consistency, `status` confirms reality still matches.

```
clever-project status [FILE] [OPTIONS]
```

Same variable/env flags as `check`. Plus:

| Flag | Description |
|---|---|
| `--brief` | Hide resources that are perfectly in sync; only show drift |
| `--exit-on-drift` | Exit code 1 if any drift, pending creation, or orphan is found. For CI checks. |

For each app, addon, and network group, `status` prints one of four verdicts:

- `= name` — in sync.
- `~ name drifted` — present on both sides, but one or more fields differ. The differing fields are printed underneath.
- `+ name only in file (would be created)` — declared but doesn't exist live.
- `- name orphan (managed but missing from file)` — live and previously tracked in `<project>.state`, but no longer in the file. Apply would not touch it, `delete` would.

Orphan detection is **state-aware**: only resources that were touched by `clever-project` on a previous run (i.e. recorded in the `.state` sidecar) are flagged. Resources that exist in the org but were created out-of-band aren't reported — `status` won't drown you in noise.

Example output:

```
Status of project `My Project` in org `mlk` (default region `par`):

  = app "prod-worker"
  ~ app "prod-api" drifted
      kind: "node" → "python"
      env:
        + NEW_KEY = "value"
        - OLD_KEY = "old"  (only in org)
        ~ PORT: "8080" → "3000"
      domains:
        + api.example.com
        - legacy.example.com
  + addon "prod-cache" only in file (would be created)
  - app "legacy-worker" orphan (managed but missing from file)

Summary: 1 drifted, 1 to create, 1 orphan, 1 in sync.
```

Scalar diffs are shown `"live" → "file"` so it reads "to converge, change live to match file." Set fields (`domains`, `dependencies`, NG members) use `+`/`-` per entry; map fields (`env`) use `+` (file-only), `-` (live-only), `~` (changed). Addon kind aliases (`postgresql` vs `postgresql-addon`, `cellar` vs `s3`, ...) and plan-slug casing (`S_BIG` vs `s_big`) are normalized before comparing, matching what `apply` would do — they never register as spurious drift.

Compared fields:

- **App**: `kind`, `region` (using the org's majority region as default), `source.from`, `domains`, `dependencies`, `env`.
- **Addon**: `kind`, `region`, `size`.
- **Network group**: members.

Not compared (because `clever-tools` doesn't expose them in JSON read mode): addon `version` / `backup_path` / `crypted`, app `config`.

`scalability`, `build` and `source.branch` *are* compared, but only when the project file declares the corresponding field. `apply` follows the same rule — it doesn't touch what isn't declared. Scalability drift looks like `fixed 1× XS → auto 1-4× S-M`; build drift like `disabled → separate L`; branch drift like `main → develop`. The live values come from the per-app v2 endpoint. For `build:` with `separate: false`, the inert flavor value Clever persists is not compared (only the on/off state matters).

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

## Shell completions

`clever-project completions <SHELL>` prints a completion script to stdout. Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`.

```sh
# zsh — drop into any directory on $fpath, e.g.
clever-project completions zsh > ~/.zsh/completions/_clever-project
# then add `fpath+=(~/.zsh/completions)` to ~/.zshrc and `autoload -U compinit && compinit`

# bash — system-wide
clever-project completions bash | sudo tee /etc/bash_completion.d/clever-project
# or user-local: source it from ~/.bashrc
clever-project completions bash > ~/.local/share/bash-completion/completions/clever-project

# fish
clever-project completions fish > ~/.config/fish/completions/clever-project.fish

# elvish
clever-project completions elvish > ~/.config/elvish/lib/clever-project-completions.elv

# powershell — add the output to your $PROFILE
clever-project completions powershell >> $PROFILE
```

Completions cover every subcommand (`apply`, `delete`, `check`, `status`, `init`, `read`, `unlock`, `completions`), all their flags, and enum values (e.g. shell names, output formats).

## Project file format

YAML, JSON or TOML, detected by extension (`.yaml`, `.yml`, `.json`, `.toml`). The same schema applies to all three — pick the one your team is most comfortable with. `read` writes whichever extension you specified for `-o`, and `init` defaults to YAML but accepts any `-o` extension.

### Schema (YAML shown for brevity — JSON and TOML map 1:1)

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
    build:                               # optional dedicated build instance
      separate: true
      flavor: M
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
    # `env:` and `domains:` only apply to managed addons (otoroshi,
    # keycloak, matomo, metabase, pulsar). They're pushed onto the
    # underlying entrypoint app — see the "Managed addons" section below.
    env:
      KEY: value
    domains: [oto.example.com]
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

## Managed addons (env and domains)

Some Clever addons are *managed services* that run on a real Clever app under the hood — `otoroshi`, `keycloak`, `matomo`, `metabase`, `pulsar`. Their v4 metadata exposes a `resources.entrypoint` field holding the underlying `app_xxx` id; `clever-project` resolves it and treats `addon.env` / `addon.domains` as a thin pass-through onto that entrypoint app:

```yaml
addons:
  oto:
    name: ${env}-otoroshi
    kind: otoroshi
    env:
      OTOROSHI_INITIAL_ADMIN_LOGIN: admin@example.com
      OTOROSHI_DOMAIN: oto.example.com
    domains:
      - oto.example.com
      - api.oto.example.com
```

Rules:
- `env:` and `domains:` are **only allowed on managed addon kinds**. Declaring them on `postgresql`, `redis`, `cellar`, etc. fails at load time (the field would silently no-op since those addons have no entrypoint). The full list of supported kinds is `otoroshi`, `keycloak`, `matomo`, `metabase`, `pulsar` (with or without the `addon-` prefix).
- Apply semantics are **additive**, never replacing — the entrypoint app already carries internal vars Clever sets at provisioning (`CC_OTOROSHI_API_CLIENT_ID`, etc.) and its own `*.clever-cloud.com` / `*.cleverapps.io` vhosts. We don't want to wipe those.
  - **env** is *merged*: apply pulls the current entrypoint env, overlays the keys from your project file, and pushes the merged map back. Keys you remove from the file are **not deleted** from the entrypoint; clean them up by hand if needed (`clever env rm KEY --app <entrypoint>`).
  - **domains** is *add-only*: apply only adds entries from your file that aren't already attached. It never removes domains from the entrypoint. To take a domain down, do it on Clever's side (`clever domain rm`).
- `status` and the apply plan mirror those semantics: extra env keys or domains on the live side are *not* flagged as drift — only entries declared in the file that are missing or have a different value count.
- Restart behaviour mirrors apps: a managed addon's entrypoint is restarted when its env actually changed (i.e. the merged env differs from what was already there). Domain-only changes don't trigger a restart.
- If the addon was just created in this run, apply polls the v4 metadata endpoint a few times (≈10 s total) to give Clever time to provision the underlying entrypoint. After ~5 attempts it bails with a hint to rerun apply once provisioning finishes.
- `read` does **not** populate `env:` or `domains:` on addons; those fields are write-only from the project file's perspective. Use `status` to inspect what's live.

## Display block

Top-level `display:` is a flat `Map<String, String>` of values surfaced at the end of `apply` (and inside the plan output, so you also see them with `--dry-run`). The same interpolation pipeline as `env:` applies — plain `${var}` lookups, generator functions, and cross-resource refs.

```yaml
display:
  api_url:        https://${apps.api.name}.cleverapps.io
  pg_host:        ${apps.api.env.POSTGRESQL_ADDON_HOST}
  pg_password:    ${addons.db.env.POSTGRESQL_ADDON_PASSWORD}
  session_secret: ${random_alphanumeric_lowercase(48)}
```

The block is rendered as aligned `key  value` pairs after a successful apply, and included verbatim in the `--format json` output under the `display` key.

Notes:
- `display` keys aren't variables — you can't reference them from elsewhere in the project. They're output-only.
- Cross-refs (`${apps.X.env.Y}` / `${addons.X.env.Y}`) only resolve once the source resource is live. On the first apply, the source may not exist yet — the value comes out empty. Re-run `apply` once the source is up to populate it.
- Values are not redacted. If you put `${addons.db.env.POSTGRESQL_ADDON_PASSWORD}` here, it will print to the terminal. Don't enable this on shared screens / CI logs that you don't control.

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

### Cross-resource references

You can pull a value from another app or addon's live env into a project app's `env:` block — or from an addon's provider-specific metadata for managed services that expose more than just env vars:

```yaml
apps:
  worker:
    env:
      # Forward the PG host/password an addon injects into another app.
      PG_HOST: ${apps.api.env.POSTGRESQL_ADDON_HOST}
      PG_PASSWORD: ${apps.api.env.POSTGRESQL_ADDON_PASSWORD}
      # Or read directly from the addon's env endpoint.
      REDIS_URL: ${addons.cache.env.REDIS_URL}
      # Or from the addon's v4 metadata endpoint (`.addon.<dotted.path>`),
      # for managed services like otoroshi / keycloak / matomo / metabase
      # that expose credentials and connection URLs there.
      OTO_USER:     ${addons.otoroshi.addon.initialCredentials.user}
      OTO_PASSWORD: ${addons.otoroshi.addon.initialCredentials.password}
      OTO_API_URL:  ${addons.otoroshi.addon.api.url}
```

The first part (`apps` or `addons`) picks the namespace, the second part is the **project key** (not the Clever name — though those often match), the third part picks the source: `env` for a runtime env var, or `addon` for a field of the v4 provider metadata. The remaining segments are joined with `.` to form the lookup key:

- `${apps.KEY.env.VAR}`            — fetch `VAR` from the app's live env (includes vars Clever injects from linked addons)
- `${addons.KEY.env.VAR}`          — fetch `VAR` from the addon's env endpoint
- `${addons.KEY.addon.a.b.c}`      — fetch `a.b.c` from the provider-specific metadata JSON (only meaningful for managed services that expose this endpoint — otoroshi, keycloak, matomo, metabase; database addons return 404 and fall back to empty + warning)

Hyphens are allowed in project keys (`apps.n8n-test-pg.env.…`).

**Resolution model.** These references aren't resolved at load time — they're left as `${…}` literals and substituted in a later pass against live Clever state. Source apps' "live env" includes vars Clever injects from linked addons (`POSTGRESQL_ADDON_HOST` etc.), so the most common use case — forwarding one app's addon credentials into a sibling — works in one ref.

**Addons created in the same run resolve in one apply.** `apply` resolves cross-refs *twice* internally: once before the plan output (against the pre-mutation snapshot, so the plan shows what would happen if nothing was created), and once again after the addon-creation phase. By then the new addons exist and the second pass picks up their real `POSTGRESQL_ADDON_HOST` etc. — so phase 2 (app create/update) pushes the resolved env in one go.

**App→app cross-refs still need a second apply** when both apps are being created in the same run. The second pass only updates addon state; per-app cross-refs to apps being created in the same apply resolve to empty. Re-run `apply` once the source app exists.

**Dry-run shows empty for to-be-created addons.** `--dry-run` only sees the pre-mutation snapshot. If your plan shows `+ DB_HOST = ""` for a cross-ref to an addon that's also being created, that's the dry-run quirk — the real apply will populate it.

**Restrictions.**
- Cross-refs work in `env:` values only — not in `name:`, `domains:`, `dependencies:`, etc.
- Resolved at `apply` and `status` time. `check --offline` doesn't see the live values and won't catch typos.
- Re-evaluated on every `apply`/`status`. A rotating addon password will surface as drift the next time you run.

### Generator functions

The interpolator also recognises `${name(args)}` to call built-in generators. Each occurrence produces a fresh value at load time.

| Function | Result |
|---|---|
| `${ulid()}` | 26-char uppercase ULID (Crockford Base32) |
| `${ulid_lowercase()}` | same, lowercased |
| `${uuid()}` | uppercase hyphenated UUID v4 |
| `${uuid_lowercase()}` | standard lowercase hyphenated UUID v4 |
| `${random_alphanumeric(N)}` | `N` random chars, mixed-case `[A-Za-z0-9]` |
| `${random_alphanumeric_lowercase(N)}` | `N` random chars, `[a-z0-9]` only |

The size argument is capped at 1024 to catch typos. Unknown function names and bad arguments surface as load-time errors the same way undefined variables do.

```yaml
apps:
  api:
    env:
      SESSION_SECRET: ${random_alphanumeric_lowercase(64)}
      REQUEST_ID_PREFIX: ${ulid_lowercase()}
```

**Caveat — non-determinism.** These functions return a different value on every load. If you write `${uuid()}` in your project file and run `apply` twice, the second run will see drift on the env var and trigger a restart. Treat them as **one-shot bootstrap helpers**: generate the value on first apply, then `clever-project read` to pin the resolved value into your project file (or extract it into a `.secrets` sidecar).

**Tip — share one generated value across multiple references.** Functions called *directly* inside multiple env values fire independently:

```yaml
env:
  N8N_HOST: app-${ulid_lowercase()}.cleverapps.io   # ulid A
  WEBHOOK_URL: https://app-${ulid_lowercase()}.cleverapps.io/   # ulid B (different)
```

To share, declare the function in a variable — it's evaluated *once* at resolver-build time, and every `${slug}` reference picks up the same value:

```yaml
variables:
  common:
    slug: ${ulid_lowercase()}
env:
  N8N_HOST: app-${slug}.cleverapps.io
  WEBHOOK_URL: https://app-${slug}.cleverapps.io/
```

### Loading variables from a file

The variables file can be YAML, JSON or TOML — same flat shape, format detected from the extension.

```yaml
# vars.yaml
domain: example.com
apikey: from-file
```

```json
// vars.json
{ "domain": "example.com", "apikey": "from-file" }
```

```toml
# vars.toml
domain = "example.com"
apikey = "from-file"
```

```sh
clever-project apply project.clever.yaml --variables-file-path vars.yaml
clever-project apply project.clever.toml --variables-file-path vars.toml
```

The flag is repeatable; later files override earlier ones.

### Precedence (low → high)

1. Project file `variables:` section (group merged with `common` if per-env form)
2. `--variables-file-path FILE` entries (in order; later files win)
3. `--variable foo=bar` entries
4. `--env <value>` for the special `${env}` variable

The two `variables:` shapes can't be mixed — every top-level value must be either all scalars (flat) or all mappings (per-env).

## Secrets

Anything you don't want committed (API keys, tokens, passwords) lives in a sidecar `.secrets` file and is referenced from the project file using the namespaced `${secrets.<key>}` syntax.

### Lookup order

Given a project file `myproj.yaml` and an active `${env}` value of e.g. `dev`:

1. If `--secrets-file-path FILE` is given: **only that file** is loaded (and it must exist).
2. Otherwise, both files below are auto-discovered next to the project file, when present:
   - `myproj.secrets` — env-agnostic defaults
   - `myproj.dev.secrets` — env-specific overrides (the basename matches the `${env}` value)

   When both exist, entries from the env-specific file override the defaults.
3. `--secret key=value` overrides are layered on top of whatever the file step produced. Repeatable; the last value for a given key wins. CI workflows that don't want plaintext secrets on disk can use this exclusively — no `.secrets` file required.

If neither a file nor any `--secret` override provides a value, the secrets map is simply empty. Referencing `${secrets.X}` with no value for `X` errors out.

### File format

A flat `Map<String, scalar>`. The content can be **YAML, JSON or TOML** — each parser is tried in turn (root must be a mapping/object/table). The file name is the same either way; the format is inferred from the content.

```yaml
# myproj.secrets  (YAML)
apikey: shared-secret
db_password: hunter2
```

```toml
# myproj.secrets  (TOML)
apikey = "shared-secret"
db_password = "hunter2"
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

## Hooks

Pre / post hooks let you orchestrate external steps (builds, DB migrations, Slack notifications, DNS updates) around `apply` and `delete` without a custom wrapper. Declared in the project file at the project root or per-app:

```yaml
hooks:
  pre_apply:   ./scripts/build-all.sh
  post_apply:  ./scripts/notify-deploy.sh

apps:
  api:
    name: ${env}-api
    kind: node
    hooks:
      pre_apply:   npm ci && npm run build
      post_apply:  ./scripts/run-migrations.sh
      pre_delete:  ./scripts/backup-data.sh
      post_delete: ./scripts/notify-teardown.sh
```

### Available events

- `pre_apply` — runs before any mutation during `apply`
- `post_apply` — runs after `apply` finishes successfully
- `pre_delete` — runs before any deletion during `delete`
- `post_delete` — runs after `delete` finishes

### Execution model

- **Order:** project `pre_apply` → each targeted app's `pre_apply` (project file order) → mutation phases → each targeted app's `post_apply` → project `post_apply`. Symmetric for delete.
- **Failure:** pre-hook failure aborts the run before any mutation; post-hook failure surfaces as a non-zero exit even though the mutations already landed (no rollback).
- **Targets:** with `--target apps.api`, only `api`'s app-level hooks fire. Project-level hooks always fire.
- **Shell:** commands are run through `sh -c '<command>'` on Unix and `cmd /C '<command>'` on Windows, so pipes, `&&`, redirects and env-var expansion work as expected.
- **Working directory:** the directory containing the project file. Relative paths like `./scripts/build.sh` resolve against it.
- **Stdout / stderr:** inherited from the parent process — you see hook output live.
- **Dry-run:** hooks don't fire in `--dry-run` mode (they're not part of the plan).
- **Skip:** `--skip-hooks` bypasses every hook for one run.

### Environment variables exposed to hooks

| Variable | Meaning |
|---|---|
| `CLEVER_PROJECT_FILE` | Absolute path to the project file |
| `CLEVER_PROJECT_ORG` | Org id |
| `CLEVER_PROJECT_REGION` | Default region |
| `CLEVER_PROJECT_ENV` | Active `${env}` value |
| `CLEVER_PROJECT_OPERATION` | `apply` or `delete` |
| `CLEVER_PROJECT_PHASE` | `pre` or `post` |
| `CLEVER_PROJECT_APP_KEY` | App key in the project file (per-app hooks only) |
| `CLEVER_PROJECT_APP_NAME` | Resolved app name (per-app hooks only) |
| `CLEVER_PROJECT_APP_KIND` | App kind (per-app hooks only) |

Secrets and per-app env vars are not injected — read them from the secrets/env file directly if a hook needs them.

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

## Machine-readable output (`--format json`)

Every command accepts `--format json`. The CLI then emits a single JSON document on **stdout**; tracing logs are routed to **stderr** so piping into `jq` (or capturing for CI) stays clean:

```sh
clever-project check --offline --format json | jq '.ok'
clever-project status --format json | jq '.summary'
clever-project apply --dry-run --format json | jq '.summary.to_create'
```

JSON mode is non-interactive: `apply` and `delete` require `--yes` (no prompt), and `init` implies `--non-interactive` (every required field must come from a flag). Exit codes still mean what they did in text mode — `check` exits 1 on validation failure, `apply`/`delete` exit non-zero on abort or error.

### Sample shapes

**`check --format json`**
```json
{
  "ok": false,
  "project": "demo",
  "org": "orga_xxx",
  "apps": 1,
  "addons": 0,
  "network_groups": 0,
  "issues": ["app `api` has unknown kind `cobol`. …"]
}
```

**`status --format json`**
```json
{
  "project": "demo",
  "org": "orga_xxx",
  "region": "par",
  "summary": { "synced": 1, "drifted": 1, "to_create": 1, "orphan": 0 },
  "apps": [
    { "name": "prod-api", "tag": "drifted", "diffs": [
      { "field": "env", "body": { "kind": "map", "entries": [
        { "op": "~", "key": "PORT", "file": "3000", "live": "8080" }
      ]}}
    ]}
  ],
  "addons": [],
  "network_groups": []
}
```

**`apply --dry-run --format json`** (same shape as the structured plan, just JSON)
```json
{
  "project": "demo",
  "org": "orga_xxx",
  "region": "par",
  "summary": { "to_create": 1, "to_update": 0, "unchanged": 1 },
  "apps": [
    {
      "name": "prod-api",
      "op": "create",
      "kind": "node",
      "region": "par",
      "source": "https://github.com/me/api.git",
      "env": { "PORT": "8080" },
      "domains": ["api.example.com"],
      "dependencies": ["prod-db"]
    }
  ],
  "addons": [],
  "network_groups": []
}
```

**`delete --format json`**
```json
{
  "project": "demo",
  "org": "orga_xxx",
  "summary": { "to_destroy": 2 },
  "network_groups": [],
  "apps": ["prod-api"],
  "addons": ["prod-db"]
}
```

**`read --format json`** and **`init --format json`** emit a small post-action report listing the file written and the resources captured.

## Behaviour notes / limitations

- **Source code push is not handled.** GitHub sources get `clever create --github owner/repo`; for non-GitHub sources the app is created empty and you deploy via Clever's git remote yourself.
- **`apply` is full-replace.** Existing apps have their env vars, domains, scalability and service links overwritten to match the project file. Domains served by `*.cleverapps.io` are never removed (they're auto-managed by Clever).
- **Addons aren't updated** if they already exist (plan, version, etc. stay as-is). Only their existence is reconciled.
- **`clever config`** isn't supported — `clever-tools` doesn't expose it as JSON. The `config:` field is parsed but ignored on both `read` and `apply`.
- **`scalability` and `build` on `read` / `status`** are populated via the per-app v2 endpoint (`Clever::get_app_details`). That endpoint also returns the app's env vars and vhosts, so `read` and `status` fold env + domains + scalability + build into a single round-trip per app (instead of three separate calls). Services still come from a separate endpoint. `status` only flags scalability or build drift when the project file declares the corresponding block (mirrors apply's "don't touch if absent").
- **Build flavor lifecycle**: `clever scale --build-flavor <name>` enables the dedicated build instance with the given flavor; `clever scale --build-flavor disabled` turns it off. `apply` pushes the right side based on `build.separate` in the project file. If you want to stop managing the build flavor entirely, drop the `build:` block — apply then stops touching it.
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
