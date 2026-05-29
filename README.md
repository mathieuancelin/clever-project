# clever-project

Declare your [Clever Cloud](https://www.clever.cloud/) apps and addons in one file, then create, update or tear them down with a single command.

`clever-project` reads a YAML, JSON or TOML project file and drives the official `clever-tools` CLI to make your org match it. Think of it as a small, declarative layer on top of Clever Cloud — no Terraform, no custom scripts.

It's built for reproducible, parameterised, disposable environments: stand up an identical preview stack per branch, hand a teammate a one-shot sandbox, or tear the whole thing down when you're done — all from the same file.

```sh
clever-project init                       # scaffold a project file
clever-project apply --env prod           # create/update everything
clever-project status                     # see what drifted from the file
clever-project delete --env staging       # tear an environment down
```

## What a project file looks like

One app, one database, wired together, with a generated slug for a unique domain:

```yaml
name: my-api
org: orga_xxxxxx-xxxx-xxxxx
region: par
variables:
  slug: ${ulid_lowercase()}
display:
  url: https://api-${slug}.cleverapps.io/
apps:
  api:
    name: api-${slug}
    kind: node
    source:
      from: https://github.com/me/my-api.git
    domains:
      - api-${slug}.cleverapps.io
    dependencies:
      - db
    env:
      DB_HOST: ${addons.db.env.POSTGRESQL_ADDON_HOST}
      DB_URI:  ${addons.db.env.POSTGRESQL_ADDON_URI}
addons:
  db:
    name: db-${slug}
    kind: postgresql
    size: xs_sml
```

Run `clever-project apply` and you get a Node app and a PostgreSQL addon, the addon's connection details injected into the app's env, the domain attached, and the deploy kicked off — in the right order.

## Features

- **One file, three formats.** YAML, JSON or TOML — the same schema, your choice.
- **Apps, addons & network groups.** Create and reconcile all three from one descriptor, with dependencies wired automatically.
- **Environments from one file.** Template names and values with `${env}` and flip the whole stack with `--env prod` / `--env staging` / `--env dev`.
- **Variables & secrets.** Reusable `variables:`, per-env overrides, and a git-ignored `.secrets` sidecar referenced as `${secrets.key}`.
- **Generators.** Built-in `${ulid()}`, `${uuid()}`, `${random_alphanumeric(N)}` for unique slugs, keys and identifiers.
- **Cross-resource references.** Pull a value live from another app or addon — `${addons.db.env.POSTGRESQL_ADDON_HOST}` — and have it injected at apply time.
- **Dry-run plans.** A Terraform-style diff of exactly what will be created, updated or left alone before anything happens.
- **Drift detection.** `status` compares your file against the live org and reports what changed, what's missing, and what's orphaned.
- **Reverse-engineering.** `read` generates a project file from an existing org so you can adopt `clever-project` on a stack you already have.
- **Hooks.** Run your own commands before/after apply and delete — builds, migrations, notifications.
- **Managed addons.** Push env and domains onto otoroshi, keycloak, matomo, metabase and pulsar entrypoints.
- **CI-friendly.** `--yes`, `--format json` output, `check` for static validation, and `--exit-on-drift` for guard jobs.

Looking for a head start? The [`recipes/`](recipes/) folder ships ready-to-apply project files for common stacks (n8n, more to come) — grab one, tweak the names and `org`, and apply.

## Install

### Pre-built binary (recommended)

Download the archive for your platform from the [latest release](https://github.com/mathieuancelin/clever-project/releases/latest) (Linux, macOS and Windows, x86_64 + aarch64), extract it, and put `clever-project` on your `PATH`.

### From crates.io

```sh
cargo install clever-project
```

### From source

```sh
git clone https://github.com/mathieuancelin/clever-project.git
cd clever-project && cargo install --path .
```

## Prerequisites

The official `clever-tools` CLI must be installed, on your `PATH`, and logged in:

```sh
npm i -g clever-tools
clever login          # one-shot, opens the browser
```

Make sure `clever --version` works in the shell you invoke `clever-project` from.

## Quick start

```sh
# 1. Scaffold a project file (prompts you for the essentials)
clever-project init

# 2. Preview what would happen — no changes are made
clever-project apply --env prod --dry-run

# 3. Create everything for prod
clever-project apply --env prod

# 4. Spin up the same stack in another environment
clever-project apply --env staging

# 5. Check the live org still matches the file
clever-project status

# 6. Tear one environment down
clever-project delete --env staging
```

Save your file as `project.clever.yaml` (or `.yml` / `.toml` / `.json`) in the working directory and `clever-project` finds it automatically — no path argument needed.

## Commands

| Command | What it does |
|---|---|
| `init` | Scaffold a fresh project file, interactively or from flags. |
| `apply` | Create or update everything in the file. Prints a plan and asks for confirmation first. |
| `delete` | Tear down the resources in the file (best-effort). |
| `check` | Validate the file — syntax, variables, kinds, regions, dependencies. Great for CI. |
| `status` | Compare the file against the live org and report drift. |
| `read` | Generate a project file from an existing org. |
| `completions` | Print a shell completion script (bash, zsh, fish, elvish, powershell). |

Every command takes `--format json` for machine-readable output, and `apply` / `delete` show a confirmation plan you can preview with `--dry-run`.

## Documentation

The [full reference](docs/reference.md) covers every flag and file format in detail:

- [Every command and its flags](docs/reference.md#commands)
- [Project file schema](docs/reference.md#project-file-format)
- [Variables, secrets & generators](docs/reference.md#variables)
- [Cross-resource references](docs/reference.md#cross-resource-references)
- [Managed addons](docs/reference.md#managed-addons-env-and-domains)
- [Hooks](docs/reference.md#hooks)
- [The state file](docs/reference.md#state-file)
- [JSON output for CI](docs/reference.md#machine-readable-output---format-json)
- [Behaviour notes & limitations](docs/reference.md#behaviour-notes--limitations)

## Building from source

```sh
cargo build
cargo test
```

See [Build & test](docs/reference.md#build--test) and [Releasing](docs/reference.md#releasing) in the reference for the full CI parity commands and the release process.
