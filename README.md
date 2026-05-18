# clever-project

CLI Rust qui synchronise une description de projet (YAML/JSON) avec les ressources d'une organisation Clever Cloud. L'orchestration réelle est déléguée au CLI officiel `clever-tools`.

Voir [specs.md](./specs.md) pour le cahier des charges détaillé.

## Statut du prototype

Légende: ✅ fait · 🚧 en cours · ⏳ à faire · ⛔ hors scope du prototype

### Fait
- ✅ Squelette Cargo + dépendances (`clap`, `serde`, `serde_yaml`, `serde_json`, `anyhow`, `tracing`, `which`, `indexmap`, `regex`)
- ✅ Modèle de données + chargement YAML/JSON détecté par extension
- ✅ Interpolation `${var}` (variables du fichier, `--variable foo=bar` prioritaires, variables spéciales `${env}`/`${org}`/`${region}`, rejet des réservées, erreur si manquante)
- ✅ Wrapper `clever-tools` (lecture + écriture) :
    - lecture : `list_apps`, `list_addons`, `get_env`, `get_domains`, `get_services`
    - écriture : `create_app`, `delete_app`, `create_addon`, `delete_addon`, `env_replace`, `domain_add`, `domain_rm`, `scale`, `link_addon`/`unlink_addon`, `link_app`/`unlink_app`
- ✅ Commande **`delete`** : supprime les apps puis addons listés dans le projet, retrouvés par `name` dans l'orga. Stop+log à la première erreur.
- ✅ Commande **`apply`** :
    - phase 1 : créer les addons absents (les existants sont laissés tels quels avec warning si `kind` diverge)
    - phase 2 : créer les apps absentes (avec `--github owner/repo` si la source est github) ; pour les apps existantes, full replace de `env`, `domains`, `scalability` *si* `kind`+`source` matchent, sinon warning et skip
    - phase 3 : résolution des `dependencies` (clés projet → ids clever) et link/unlink via `clever service` pour converger
- ✅ Commande **`read`** : lit apps/addons explicites (`--app`/`--addon`, répétables) ou `--all`, récupère env + domains + dependencies, génère le fichier de sortie au format `yaml`/`json` selon l'extension de `-o`
- ✅ 18 tests unitaires verts, build sans warning

### Décisions prises

| Sujet | Choix |
|---|---|
| `clever` dans le PATH | Pré-requis ; pas de gestion nvm/bun dans la CLI |
| Source github | `clever create --github owner/repo`, l'utilisateur déploie son code lui-même |
| Source non-github | App créée vide, warning loggé, l'utilisateur déploie via le remote git interne de Clever |
| Diff sur `apply` | Full replace : env, domains, dependencies, scalability. Le fichier est la vérité. |
| `read --all` | Disponible mais non par défaut |
| Erreur partielle | Stop + log, pas de rollback |
| Détection format | Par extension `.yaml`/`.yml`/`.json` |
| Parallélisme | Séquentiel pour le prototype |
| Ordre `delete` | Apps d'abord, puis addons (libère les liens service avant) |

### Limitations connues / hors scope du prototype
- ⛔ **Push du code source** : pas de clone/push automatique. Avec une source github → `--github`. Sinon, app créée vide.
- ⛔ **`config`** (`clever config`) : pas exposé en JSON par `clever-tools`. Ignoré côté `read` comme côté `apply`. Le champ est conservé dans le modèle pour plus tard.
- ⛔ **`scalability` côté `read`** : `clever scale` n'a pas de mode lecture/JSON. La section n'est donc pas régénérée depuis l'existant (`scalability: None`).
- ⛔ **Mise à jour des addons** : si un addon existe déjà, pas d'update (ni `size`, ni `version`). Warning si le `kind`/`providerId` diverge.
- ⛔ **`network_groups`** : champ présent dans le modèle, non traité.
- ⛔ **`crypted`** sur les addons : passé en `--option encryption=true` à la création (à valider selon le provider), non détecté côté `read`.
- ⛔ **`backup_path`** sur les addons : champ présent, non traité.
- ⛔ **Rollback** sur erreur partielle : pas implémenté (stop + log).
- ⛔ **Mode `--dry-run`** : pas implémenté.
- ⛔ **Parallélisme** : séquentiel.
- ⛔ Domaines auto-gérés `*.cleverapps.io` : exclus de `read` et jamais supprimés par `apply`.

### Questions encore ouvertes (à valider à l'usage)
- L'option `--option encryption=true` à la création d'addon est un best-effort, à vérifier selon le provider (PostgreSQL le supporte sous ce nom, à confirmer pour Redis/Cellar/etc.).
- `clever create --type java` vs `clever applications list` qui retourne `type: jar` : on tolère les deux (mapping `java` ↔ `jar`).
- Les addons Materia KV / Cellar n'ont pas de `version` configurable — à ignorer côté `apply`.

## Utilisation

```sh
# Lire des ressources existantes pour bootstrap un fichier projet
clever-project read --org orga_xxx --app frontend --addon main-db -o project.yaml

# Tout lire dans une orga
clever-project read --org orga_xxx --all -o project.yaml

# Appliquer un fichier projet
clever-project apply project.yaml [--org ...] [--region ...] [--variable env=staging]

# Supprimer les ressources listées dans un fichier
clever-project delete project.yaml [--org ...] [--variable env=staging]
```

Verbosité accrue :
```sh
clever-project --verbose apply project.yaml
```

## Structure du code

```
src/
├── main.rs              # entrée + init tracing
├── cli.rs               # clap (Cli, Command, ReadArgs, ApplyArgs, DeleteArgs)
├── model.rs             # Project / App / Addon / ... + load_and_resolve
├── interpolate.rs       # Resolver : ${var}, variables spéciales, walk de Value
├── clever.rs            # wrapper Command::new("clever") + helpers typés
└── commands/
    ├── mod.rs
    ├── apply.rs         # 3 phases : addons → apps (create/update) → service links
    ├── delete.rs        # apps puis addons, lookup par name
    └── read.rs          # introspection org → Project
```

## Build & tests

```sh
cargo build
cargo test
```
