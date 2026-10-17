# PlexFileRenamer

Renomme et réorganise une bibliothèque de films et séries à partir des métadonnées **déjà matchées dans Plex**. L'outil lit directement la base SQLite de Plex, en lecture seule. Le nommage se décrit avec des templates Jinja ([minijinja](https://github.com/mitsuhiko/minijinja)) : conditions, filtres, valeurs par défaut.

```
amelie/Amelie.DVDRip.mkv
  → Le Fabuleux Destin d'Amélie Poulain (2001) {tmdb-194}/Le.Fabuleux.Destin.d.Amelie.Poulain.2001.576p.DVD.MPEG2.AC3-5.1.FR-EN.SUB-FR.mkv
```

## Sécurité

- La base Plex est ouverte en `mode=ro`, et l'outil n'y écrit jamais. Le mode WAL est supporté : les données encore dans `-wal` sont lues. S'il n'y a pas de `-wal` (Plex arrêté, ou copie de la base), `immutable=1` est activé automatiquement.
- `plan` est un dry-run. `apply` affiche le plan puis demande confirmation, sauf avec `--yes`.
- Aucun fichier n'est jamais écrasé. Les collisions sont détectées sur le plan entier avant toute action : deux items vers la même cible, ou une cible qui existe déjà. Avec `collision = "fail"` (le défaut), rien n'est fait tant qu'il reste un conflit.
- Avant chaque déplacement, l'outil vérifie que la source existe et que sa taille correspond à `media_parts.size`. Un fichier modifié depuis le dernier scan Plex n'est pas touché, et ses sidecars non plus.
- Sur un même système de fichiers, le déplacement est un `rename`. Entre deux systèmes de fichiers, c'est une copie, puis une vérification de taille, puis la suppression de la source.
- Chaque action est journalisée au fil de l'eau (JSON Lines, avec un `fsync` par ligne). `undo` rejoue le journal à l'envers, sans jamais écraser non plus.

## Démarrage

```bash
cargo build --release
```

```bash
plexfilerenamer init
```

Pour travailler sur une copie de la base (recommandé pour les premiers essais) :

```bash
cp "/var/lib/plexmediaserver/Library/Application Support/Plex Media Server/Plug-in Support/Databases/com.plexapp.plugins.library.db" plex.db
```

```bash
plexfilerenamer libraries plex.db
```

```bash
plexfilerenamer vars plex.db --item "Amélie"
```

```bash
plexfilerenamer plan plex.db --only-changed
```

```bash
plexfilerenamer apply plex.db
```

Emplacements habituels de la base : macOS `~/Library/Application Support/Plex Media Server/Plug-in Support/Databases/`, Windows `%LOCALAPPDATA%\Plex Media Server\Plug-in Support\Databases\`, Docker `<config>/Library/Application Support/Plex Media Server/Plug-in Support/Databases/`.

Après un `apply`, lance un scan de la bibliothèque dans Plex. Les fichiers sont retrouvés, et le matching est conservé grâce à `.plexmatch` (option `write_plexmatch`).

## Commandes

| Commande | Rôle |
|---|---|
| `init [-o fichier] [--force]` | Écrit la config commentée avec tous les défauts. |
| `libraries <db>` | Liste les librairies (id, type, nom, racines). |
| `vars <db> --item <id\|titre>` | Affiche en JSON toutes les variables d'un item. Indispensable pour écrire ses templates. |
| `render <db> --item … [--template "…"]` | Teste un template à la volée. Sans `--template`, affiche le chemin cible complet. |
| `plan <db> [--only-changed] [--json]` | Dry-run : liste avant/après, ou plan JSON pour d'autres outils. |
| `script <db> --shell bash\|pwsh\|cmd -o rename.sh` | Génère un script équivalent à `apply --mode move`. Il n'écrase pas un script existant. |
| `apply <db> [--mode move\|copy\|hardlink\|symlink] [--yes] [--journal f] [--clean-empty-dirs]` | Exécute le plan. |
| `undo <journal.jsonl> [--yes]` | Annule un `apply`. |

Options communes : `--config`, `--profile`, `--library <nom|id>` (répétable), `--filter <expr>` (répétable, combinées en ET), `--item`, `--limit`, `--collision skip|suffix|fail`, `--immutable`.

### Filtres

`champ op valeur`, avec les opérateurs `=` `!=` `<` `<=` `>` `>=` et `~` (regex, insensible à la casse). Le champ est un chemin dans les variables (voir `vars`). Si une liste est traversée, il suffit qu'un élément corresponde. Un nom court qui n'existe pas à la racine est cherché dans `video.*`.

```
--filter 'year<2000'   --filter 'title~^Star'   --filter 'audio.lang=fr'
--filter 'resolution=576p'   --filter 'video.hdr=DV'   --filter 'kind=episode'
```

Les comparaisons de langue normalisent les codes : `fr`, `fre`, `fra` et `French` sont équivalents.

## Variables

Toujours disponibles :

| Variable | Exemple |
|---|---|
| `title`, `original_title`, `sort_title`, `year`, `edition` | `Le Fabuleux Destin d'Amélie Poulain`, `2001` |
| `ext`, `container` | `mkv` |
| `ids.plex`, `ids.plex_guid`, `ids.tmdb`, `ids.imdb`, `ids.tvdb` | `194`, `tt0211915` |
| `video.codec`, `video.width`, `video.height`, `video.resolution`, `video.fps`, `video.aspect`, `video.bit_depth`, `video.hdr` | `mpeg2video`, `720`, `576`, `576p`, `25.0`, `1.78`, `8`, `""` / `HDR10` / `DV` / `HLG` |
| `audio[]` : `codec`, `channels`, `channels_label`, `language`, `title`, `profile`, `default` | `ac3`, `6`, `5.1`, `fre` |
| `audio_main` | Piste par défaut, sinon la première. |
| `subs[]` : `language`, `codec`, `title`, `forced`, `default`, `external`, `path` | |
| `part` / `parts_total` | Multi-CD. |
| `version` / `versions_total` | Plusieurs fichiers pour le même film. |
| `size` (octets), `bitrate` (kbps), `duration` (ms), `duration_min` | |
| `genres`, `directors`, `studio`, `content_rating` | |
| `library`, `library_id`, `kind` (`movie` / `episode`), `path`, `plex_path`, `root` | |
| Toutes les `vars` et `rules` du profil | `source` |

Épisodes uniquement :

| Variable | Exemple |
|---|---|
| `show.title`, `show.year`, `show.ids.*` | |
| `season`, `episode`, `episode_title`, `air_date` | |
| `episode_end` | Dernier épisode quand un fichier en contient plusieurs (`S01E03-E04`). |

Template `subtitle`, en plus des variables de l'item : `base` (nouveau nom de la vidéo, sans extension), `lang` (`fr`), `language` (`fre`), `forced`, `default`, `codec`, `title`, `n` (rang parmi les sous-titres de même langue et même statut forcé), `ext`.

Une variable absente vaut une chaîne vide. Après rendu, les séparateurs orphelins sont supprimés automatiquement (`..`, `.-.`, ` - - `, `()`, en début ou en fin), donc pas besoin d'entourer chaque champ d'un `{% if %}`.

## Filtres de template

| Filtre | Effet |
|---|---|
| `scene` | Translitère (é→e), sépare par des `.`. Gère les apostrophes : `Ocean's`→`Oceans`, `d'Amélie`→`d.Amelie`. |
| `sanitize(sep=" ")` | Version douce : garde accents et espaces, remplace les caractères interdits (`<>:"/\|?*`, caractères de contrôle). Ajoute `_` aux noms réservés Windows (`CON`, `NUL`…). |
| `translit`, `upper`, `lower`, `title_case`, `truncate(n)` | |
| `vcodec`, `acodec` | Libellé à partir de `[mapping.vcodec]` / `[mapping.acodec]` (`mpeg2video`→`MPEG2`, `hevc`→`H265`, `dca`→`DTS`…). |
| `lang(style)` / `langs(style)` | Styles `iso1` (FR), `iso2` (FRE), `iso2t` (FRA), `name` (French), `native` (Français), ou `[mapping.languages]`. `langs` dédoublonne en gardant l'ordre et retire `und`. |
| `pad(n=2)` | `1` → `01`. |
| Builtins minijinja | `default`, `join`, `map`, `replace`, `first`… |

Quel que soit le template, chaque segment de chemin est rendu sûr pour Windows, macOS et Linux : `: ` devient ` - `, un `/` présent dans un titre ne crée jamais de sous-dossier (`Face/Off` → `Face-Off`), et un point ou un espace final est retiré. `max_length` (255 octets par défaut) raccourcit d'abord le titre, puis le titre d'épisode. Il ne coupe jamais les infos techniques ni l'extension.

## Exemples de templates

À mettre dans `[profiles.movies]`. Testés dans `tests/render.rs`.

**Style scene** (par défaut) :

```toml
folder = '''{{ title }} ({{ year }}){% if ids.tmdb %} {tmdb-{{ ids.tmdb }}}{% endif %}'''
file   = '''{{ title|scene }}.{{ year }}.{{ video.resolution }}.{{ source }}.{{ video.codec|vcodec }}.{{ audio_main.codec|acodec }}-{{ audio_main.channels_label }}.{{ audio|map(attribute='language')|langs|join('-') }}{% if subs %}.SUB-{{ subs|map(attribute='language')|langs|join('-') }}{% endif %}{% if parts_total > 1 %}.CD{{ part }}{% endif %}'''
```
→ `Star Wars - Episode IV - A New Hope (1977) {tmdb-11}/Star.Wars.Episode.IV.A.New.Hope.1977.2160p.DVD.H265.TrueHD-7.1.EN-FR.SUB-FR.mkv` (avec `source = "DVD"`)

**Style Plex propre**, conforme aux conventions de nommage Plex (éditions, versions, parties) :

```toml
folder = '''{{ title }} ({{ year }}){% if edition %} {edition-{{ edition }}}{% endif %}'''
file   = '''{{ title }} ({{ year }}){% if versions_total > 1 %} - {{ video.resolution }}{% endif %}{% if parts_total > 1 %} - pt{{ part }}{% endif %}'''
```
→ `Ocean's Eleven (2001)/Ocean's Eleven (2001) - pt1.avi`

**Style Kodi** :

```toml
folder = '''{{ title|sanitize }} ({{ year }})'''
file   = '''{{ title|sanitize }} ({{ year }}){% if versions_total > 1 %} - {{ video.resolution }}{% endif %}{% if parts_total > 1 %}-cd{{ part }}{% endif %}'''
```
→ `Star Wars Episode IV - A New Hope (1977)/Star Wars Episode IV - A New Hope (1977) - 480p.mkv`

**Séries** (profil `series` par défaut) :

```toml
folder = '''{{ show.title }} ({{ show.year }}){% if show.ids.tvdb %} {tvdb-{{ show.ids.tvdb }}}{% endif %}/Season {{ season|pad }}'''
file   = '''{{ show.title }} - S{{ season|pad }}E{{ episode|pad }}{% if episode_end %}-E{{ episode_end|pad }}{% endif %} - {{ episode_title }}'''
```
→ `Kaamelott (2005) {tvdb-79175}/Season 01/Kaamelott - S01E03-E04 - La Tarte aux myrtilles.mkv`

## Configuration

`plexfilerenamer init` écrit [`src/default_config.toml`](src/default_config.toml), qui documente chaque clé. Ta config est fusionnée par-dessus ces défauts, donc elle ne contient que ce que tu changes. Elle est cherchée dans `./plexfilerenamer.toml`, ou à l'emplacement donné par `--config`. Exemple complet :

```toml
collision = "suffix"
write_plexmatch = true
clean_empty_dirs = true

[[path_map]]            # Plex tourne dans Docker
from = "/movies"
to   = "/mnt/media/movies"

[[replace]]             # regex, appliquée à chaque segment avant le nettoyage
pattern = " & "
with    = " and "

[mapping.languages]
fre = "VFF"

[mapping.vcodec]
h264 = "x264"

[profiles.dvd]
kind = "movie"
libraries = ["Films DVD", 4]
folder = '''{{ title }} ({{ year }})'''
file = '''{{ title|scene }}.{{ year }}.{{ source }}'''
subtitle = '''{{ base }}.{{ lang }}{% if forced %}.forced{% endif %}.{{ ext }}'''

[profiles.dvd.vars]
source = "DVD"

[[profiles.dvd.rules]]
when = "video.height >= 1080"
set  = { source = "BluRay" }
```

Choix du profil pour chaque item :
1. `--profile`, s'il est donné ;
2. sinon le premier profil qui liste la librairie (par nom ou par id) ;
3. sinon le premier profil du bon `kind` dont `libraries` est vide.

Sidecars (`[sidecars]`) :
- Les sous-titres externes connus de Plex suivent la vidéo, avec le template `subtitle`. Une paire `.idx`/`.sub` garde le même nom.
- Les fichiers `<nom de la vidéo>.<x>.<ext>` ou `<nom de la vidéo>-<x>.<ext>` (`.nfo`, `.srt`, `-poster.jpg`…) suivent aussi la vidéo.
- Les fichiers de dossier (`poster.jpg`, `fanart.jpg`, `.plexmatch`…) suivent uniquement si le dossier source ne contient qu'un seul item.

## Développement

```bash
cargo test
```

La fixture (`tests/common/mod.rs`) construit une base avec le schéma réel de Plex Media Server 1.43.4 (`tests/fixtures/plex_schema.sql`), et crée les fichiers correspondants dans un tempdir. Elle couvre :
- un film multi-pistes ;
- des épisodes, dont un fichier contenant deux épisodes ;
- un film multi-part et un film multi-version ;
- des sous-titres externes, dont une paire VobSub ;
- des titres avec accents, apostrophes, `:` et `/` ;
- un média supprimé et un fichier hors racine ;
- les deux formats de `extra_data` (JSON et url-encoded).

Snapshots `insta` dans `tests/snapshots/` ; `cargo insta review` après un changement voulu.

Tout le SQL est dans `src/db/mod.rs`. `check_compat` liste précisément les colonnes manquantes si une version de Plex diffère, et les colonnes qui n'existent que dans certaines versions (`edition_title`, `frames_per_second`…) sont lues comme `NULL` quand elles sont absentes.
