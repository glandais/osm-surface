# osm-surface

Microservice Rust qui remplace, à l'identique du point de vue de son client, l'endpoint
`trace_attributes` de Valhalla tel que servi par Stadia Maps
(<https://docs.stadiamaps.com/routing/trace-attributes/>), pour le calcul de la surface des traces
de Pédalons (backend `tribly`, `StadiaTraceAttributesClient` / `TraceSurfaceComputer`).

Les données viennent des tuiles vectorielles **VersaTiles Shortbread 1.0** (OpenStreetMap,
`https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}`, zoom 14), téléchargées à la demande et mises
en cache. Aucun graphe Valhalla à construire.

**Recommandé : un index OSM en complément** (`OSM_INDEX`, voir *Index OSM*). Shortbread réduit
`surface` à `paved` / `unpaved`, n'a ni `smoothness`, ni `mtb:scale`, ni `sac_scale`, ni
`access`, ni les `bridleway`, et ses données ont quelques mois. L'outil `osm-index` extrait d'un
fichier Geofabrik les voies et les tags que lit Valhalla. Avec cet index, l'accord avec Stadia sur
les parcours d'évaluation passe de 86-94 % à **98-99 %**. Sans index, le service marche avec les
tuiles seules.

## Ce que fait le service

```
POST /trace_attributes/v1      (chemin Stadia ; /trace_attributes accepté aussi)
GET  /health
```

1. **Requête** : `encoded_polyline` (polyline6, lat/lon × 1e6) ou `shape: [{lat, lon}]`,
   `costing` (`bicycle`, `pedestrian`, `auto` et leurs variantes), `shape_match` (tout est traité en
   `map_snap`), `filters` (`include` / `exclude`), `trace_options.search_radius` facultatif.
   L'en-tête `Authorization: Stadia-Auth <clé>` est accepté et ignoré.
2. **Tuiles** : celles qui touchent un couloir de 100 m autour de la trace (≈ 40 tuiles pour
   90 km), plus un LRU mémoire des couches `streets` décodées. Au plus 8 téléchargements
   simultanés, un nouvel essai sur erreur transitoire, User-Agent explicite. Le cache disque garde
   les tuiles brutes 7 jours et les réutilise périmées si le serveur est injoignable. Il est rangé
   par source (`CACHE_DIR/<hôte_chemin>-<hash de TILES_URL>/z/x/y.mvt`) : changer `TILES_URL`
   n'en sert jamais l'ancien contenu. Une tuile n'y est écrite qu'une fois décodée. Un fichier
   illisible est supprimé et retéléchargé. Un 404 ou une page d'erreur du serveur donnent un 503,
   jamais une tuile vide.
3. **Graphe** : les lignes `streets` utilisables par le mode (voir *Accès*), découpées en segments
   (avec l'index OSM, chaque segment prend les tags de la voie OSM sur laquelle il est posé, et les
   `bridleway` absentes des tuiles sont ajoutées),
   coupées au carré exact de leur tuile ; nœuds retrouvés par les coordonnées (pas d'identifiant OSM
   dans les tuiles) ; bouts coupés aux bords de tuile recollés ; sommets posés sur une autre ligne du
   même niveau (sol / pont / tunnel) raccordés à elle (jonction dont le sommet partagé a disparu à la
   simplification) ; index spatial R-tree.
4. **Appariement `map_snap`** : HMM / Viterbi façon meili. La trace est densifiée tous les 10 m
   (un tracé simplifié est droit entre ses points), candidats à 40 m au plus (8 par observation),
   émission gaussienne (σ = 5 m), transition `|distance par le réseau − distance observée| / β`
   (β = 3 m, Dijkstra borné), saut en ligne droite autorisé à coût fixe quand le réseau ne relie pas
   deux candidats (trou de connectivité dans les tuiles), point sans candidat → `unmatched`. En
   vélo, un `footway`, trottoir, `steps` ou `pedestrian` coûte un peu plus à l'émission
   (`MATCH_FOOTWAY_COST` = 0,75) : à côté d'une chaussée, c'est la chaussée qui l'emporte, comme
   avec le profil vélo de Valhalla.
5. **Réponse** : `shape` (polyline6 du tracé apparié), `edges` (une arête par suite de segments de
   même classe de rue : `surface`, `use`, `cycle_lane`, `road_class`, `unpaved`, `bridge`,
   `tunnel`, `length` en km, `begin_shape_index`, `end_shape_index`), `matched_points` (un par point
   envoyé : `type`, `edge_index`, `distance_along_edge` en **fraction** 0..1 de l'arête, `lat`,
   `lon`, `distance_from_trace_point`). `osm_changeset` n'est pas fourni (tribly l'accepte nul).
   Réponse gzip (ou brotli) si le client l'accepte. Les arêtes couvrent la forme sans trou, même
   sur un saut entre deux rues non reliées : la première moitié du saut va à la rue quittée,
   la seconde à la rue atteinte. Seul le segment entre deux morceaux séparés par un point
   `unmatched` reste hors arête, et tribly le compte de toute façon comme inconnu.

Les distances le long de `shape` sont calculées comme tribly les recalcule (haversine sur les
coordonnées arrondies au 1e-6), et la forme est monotone le long de la trace : la projection de
tribly (`TraceSurfaceProjection`, garde-fou 1,2 × distance + 30 m) retombe sur les bonnes plages.
`tribly.rs` en est un portage (avec `SurfaceRanges` et `TrackSurface`) utilisé par les tests et par
l'outil d'évaluation.

### Erreurs (format Valhalla)

`HTTP 400 {"error_code": N, "error": "...", "status_code": 400, "status": "Bad Request"}`

| Code | Cas |
|---:|---|
| 100 | JSON illisible |
| 123 | moins de 2 points |
| 124 / 125 | `costing` absent / inconnu |
| 126 | ni `encoded_polyline` ni `shape` (ou polyline invalide) |
| 153 | plus de 16 000 points |
| 154 | plus de 200 km (ou coordonnée invalide, comme Stadia) |
| 171 | aucune rue près de la trace (en mer…) |
| 444 | des rues, mais aucun point envoyé n'a pu être apparié (ou trace qui traverse l'antiméridien, non gérée) |

tribly traite {123, 171, 443, 444, 154} comme définitifs (la tranche reste inconnue). Quand les
tuiles ne peuvent pas être chargées, la réponse est un **503** : tribly le voit comme transitoire et
réessaiera plus tard. C'est le cas d'un serveur injoignable, d'un 404, d'une réponse qui n'est pas
une tuile, d'un délai `TILES_DEADLINE_SECS` dépassé, d'une tuile absente du cache en
`TILES_OFFLINE`, ou de tous les créneaux d'appariement occupés au-delà de 13 s. Deux points
identiques : 200 vide, comme Stadia.

## Surface : les règles de Valhalla

Valhalla (`src/mjolnir/pbfgraphparser.cc`) prend la valeur du tag `surface` ; sans tag `surface`,
le `tracktype` ; sans l'un ni l'autre, `smoothness`. Ensuite `mtb:scale` remplace ce qui a été
trouvé, même une surface asphaltée : 0 → `dirt`, 1 → `gravel`, 2 et plus → `path`. Si rien n'a été
trouvé, `sac_scale` donne `path`, sinon une valeur par défaut selon la classe de route et l'usage
(tableau ci-dessous). Avec l'index OSM, ces règles s'appliquent telles quelles aux tags exacts de
la voie (`valhalla::surface_of_osm`).

### Sans index : de Shortbread à Valhalla

Shortbread ne garde du tag `surface` que `paved` / `unpaved` et ne garde ni `smoothness`, ni
`mtb:scale`, ni `sac_scale`. D'où :

| Shortbread `streets` | Surface Valhalla rendue | Classe tribly |
|---|---|---|
| `surface=paved` | `paved_smooth` | PAVED |
| `surface=unpaved`, `bicycle=designated` (voie verte), tracktype absent ou grade1-3 | `compacted` | GRAVEL |
| `surface=unpaved`, tracktype grade4 / grade5 | `dirt` | DIRT |
| `surface=unpaved`, tracktype grade1-3 | `gravel` | GRAVEL |
| `surface=unpaved`, `kind=path` (sans tracktype) | `dirt` | DIRT |
| `surface=unpaved`, autres (track, footway, service…) | `gravel` (lecture Valhalla de `unpaved`) | GRAVEL |
| pas de surface, `tracktype=grade1` | `paved_rough` | PAVED_ROUGH |
| pas de surface, `grade2` / `grade3` / `grade4` / `grade5` | `compacted` / `dirt` / `gravel` / `path` | GRAVEL / DIRT / GRAVEL / PATH |
| pas de surface ni tracktype : motorway … residential | `paved_smooth` | PAVED |
| idem : service, living_street | `paved_smooth` | PAVED |
| idem : track | `dirt` | DIRT |
| idem : path, footway, pedestrian, bridleway | `compacted` | GRAVEL |
| idem : cycleway, steps | `paved` | PAVED |

Les règles pour `unpaved` sont les seuls choix propres à ce service : la valeur exacte étant perdue,
on prend la plus probable (usage OSM de `surface` par type de voie, et vérification sur les
parcours d'évaluation). La table de calibration de `eval/report.md` montre, pour chaque combinaison
d'attributs Shortbread, ce que Stadia y voit.

**`use`** : `road`, `ramp` (liens), `track`, `driveway`, `alley`, `parking_aisle`,
`emergency_access`, `drive_through`, `living_street`, `service_road`, `cycleway`, `footway`,
`steps`, `path`, `pedestrian`, `bridleway`, `bus`.
**`cycle_lane`** (au mieux, Shortbread n'ayant pas les `cycleway:*`) : `separated` sur une piste
cyclable, `shared` sur un path ou un footway ouvert aux vélos, `none` ailleurs.

### Accès (filtre du `costing`)

Valeurs par défaut de Valhalla (`lua/graph.lua`), avec par défaut les règles françaises de
`adminconstants.h` (`ACCESS_RULES=fr`) : vélos admis sur les footways, piétons sur les pistes
cyclables, ni l'un ni l'autre sur les trunks ; le tag `bicycle` l'emporte (`no`, `designated`…).
Avec l'index OSM s'y ajoutent `access=no` (fermé à tous sauf tag de mode contraire),
`smoothness=impassable` (fermé aux véhicules), `vehicle`, `foot` et `mtb:scale` < 6 (ouvert aux
vélos). `ACCESS_RULES=default` donne les règles mondiales. Rails, trams, pistes d'aéroport ne font
jamais partie du réseau.

## Index OSM

```bash
curl -LO https://download.geofabrik.de/europe/france/pays-de-la-loire-latest.osm.pbf
cargo run --release --bin osm-index -- pays-de-la-loire-latest.osm.pbf data/osm-index.bin
OSM_INDEX=data/osm-index.bin cargo run --release
```

`osm-index` garde les voies `highway` du réseau (hors `area=yes`), leur géométrie et les tags
`highway`, `footway`, `surface`, `tracktype`, `smoothness`, `mtb:scale`, `sac_scale`, `service`,
`bicycle`, `foot`, `access`, `vehicle`, `horse`, `bridge`, `tunnel`, `oneway`. Il écrit un fichier
compact, puis le renomme sur l'ancien. `--bbox min_lon,min_lat,max_lon,max_lat` limite l'index à
une zone (fixture de test). Pour les Pays de la Loire : 370 Mo de PBF, 11 s, un index de 70 Mo
(847 000 voies). Le service le charge en 0,8 s et occupe alors ≈ 270 Mo de RAM. Pour la France
entière, compter environ dix fois plus (≈ 3 Go de RAM) ; on peut aussi n'indexer que les régions
utiles.

À chaque requête, chaque segment des tuiles est rattaché à la voie OSM sur laquelle il est posé :

- la voie la plus proche à moins de 8 m, à 40° près de la même direction ;
- un autre type de voie coûte 2 m s'il est voisin (`path` ↔ `track`…) et 6 m sinon. Ce sont les
  changements d'OSM depuis la construction des tuiles ;
- une voie que le mode ne peut pas emprunter coûte 3 m quand la rue des tuiles le pouvait. Exemple :
  un chemin scindé depuis en piste cyclable et en trottoir `bicycle=no` est la piste pour un vélo ;
- il faut l'accord de la moitié au moins des points pris le long du segment.

Sans voie trouvée, le segment garde les attributs des tuiles. Les `bridleway`, absentes des tuiles,
sont ajoutées au réseau depuis l'index. Leurs sommets sont arrondis à la grille des tuiles pour
retomber sur les jonctions.

**Fraîcheur** : régénérer l'index chaque semaine, par un cron qui télécharge le fichier Geofabrik
du jour et relance `osm-index`. Le service vérifie toutes les 5 minutes si le fichier a changé et
le recharge sans redémarrer. `/health` donne la date OSM de l'index (`osm_index.replication_time`)
et celle des tuiles (`tiles.osm_replication_time`, lue dans leur `tiles.json` au démarrage).

## Lancer

```bash
cargo run --release                         # écoute sur 0.0.0.0:8002
curl -s localhost:8002/health
```

| Variable | Défaut | Rôle |
|---|---|---|
| `LISTEN_ADDR` | `0.0.0.0:8002` | adresse d'écoute |
| `TILES_URL` | `https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}` | gabarit des tuiles |
| `TILE_ZOOM` | `14` | zoom des tuiles (14 = zoom max de VersaTiles) |
| `CACHE_DIR` | `./.cache/tiles` | cache disque des tuiles brutes (vide : pas de cache disque) |
| `CACHE_TTL_DAYS` | `7` | âge au-delà duquel une tuile est retéléchargée ; à 2 × TTL elle est supprimée |
| `CACHE_MAX_GB` | (aucune) | taille maximale du cache disque : les plus anciennes tuiles au-delà sont supprimées |
| `OSM_INDEX` | (aucun) | index OSM écrit par `osm-index` (recommandé) |
| `MEMORY_CACHE_MB` | `256` | LRU mémoire des tuiles décodées |
| `FETCH_CONCURRENCY` | `8` | téléchargements simultanés, tout le processus |
| `FETCH_TIMEOUT_SECS` | `8` | délai d'un téléchargement |
| `TILES_DEADLINE_SECS` | `11` | délai total de chargement des tuiles d'une requête (puis 503) |
| `TILES_OFFLINE` | `false` | `true` : le cache disque seul, jamais de téléchargement (tuile absente : 503) |
| `USER_AGENT` | `osm-surface/<version> (…)` | envoyé au serveur de tuiles |
| `ACCESS_RULES` | `fr` | `fr` ou `default` |
| `MAX_CONCURRENT_MATCHES` | nb de CPU | appariements simultanés |
| `RUST_LOG` / `LOG_FORMAT` | `info` / texte | journaux `tracing` (`LOG_FORMAT=json`) |
| `MATCH_*` | | réglages de l'appariement (`MATCH_RADIUS_M`, `MATCH_SIGMA_M`, `MATCH_BETA_M`, `MATCH_STEP_M`, `MATCH_MAX_CANDIDATES`, `MATCH_JUMP_COST`, `MATCH_MAX_JUMP_M`, `MATCH_FOOTWAY_COST`) |

Le cache disque est nettoyé au démarrage puis toutes les 6 heures. Le nettoyage supprime les
fichiers temporaires abandonnés, les tuiles plus vieilles que 2 × TTL et, avec `CACHE_MAX_GB`, les
plus anciennes au-delà de la taille fixée. Sans `CACHE_MAX_GB`, le cache grossit avec les régions
traversées : une tuile z14 fait en moyenne 100 à 300 ko en ville, quelques dizaines en campagne.

### Docker

```bash
docker build -t osm-surface .
# l'index OSM, dans le volume (à refaire chaque semaine)
docker run --rm -v osm-surface-data:/data alpine sh -c \
  'wget -qO /data/region.osm.pbf https://download.geofabrik.de/europe/france/pays-de-la-loire-latest.osm.pbf'
docker run --rm -v osm-surface-data:/data --entrypoint osm-index osm-surface \
  /data/region.osm.pbf /data/osm-index.bin
# le service
docker run -d --name osm-surface -p 8002:8002 -v osm-surface-data:/data \
  -e OSM_INDEX=/data/osm-index.bin osm-surface
curl -s localhost:8002/health
```

Image d'exécution distroless (`cc-debian12`, utilisateur non root, ≈ 67 Mo) avec `osm-surface` et
`osm-index`. Le cache des tuiles et l'index vont dans le volume `/data`. Compter quelques Mo de
cache par centaine de km de parcours (≈ 11 Mo pour les trois parcours d'évaluation autour de
Nantes). L'image de build est épinglée (`rust:1.99.1-slim-bookworm`).

## Brancher sur tribly

Le client `stadiamaps-trace` de tribly a sa propre URL (par défaut celle du planificateur Stadia).
Il suffit de la surcharger côté backend :

```bash
QUARKUS_REST_CLIENT_STADIAMAPS_TRACE_URL=http://<hôte>:8002
```

- Garder `STADIAMAPS_API_KEY` **non vide** : le worker de surface est coupé quand la clé est vide,
  et le client envoie toujours `Authorization: Stadia-Auth <clé>` (ignoré ici). Le planificateur et
  le géocodeur continuent, eux, d'utiliser Stadia.
- Le délai de lecture du client (15 s) est large : 50 à 100 ms par requête quand les tuiles sont en
  cache, ≈ 0,9 s pour les 40 tuiles du parcours de 90 km téléchargées à froid (quelques secondes
  pour une tranche de 190 km dans une région jamais vue).
- Monter l'index OSM de la zone où roulent les utilisateurs (`OSM_INDEX`) : sans lui, la répartition
  GRAVEL / DIRT est devinée. Hors de la zone indexée, le service retombe sur les tuiles seules.
- Les surfaces déjà stockées (calculées par Stadia) restent valables ; un recalcul complet n'est pas
  nécessaire, mais les nouvelles traces auront des plages légèrement différentes (voir *Évaluation*).
- Coût : plus aucun crédit Stadia pour la surface (20 crédits par tranche de 190 km jusque-là).

### Attribution (ODbL)

Les données sont celles d'OpenStreetMap (licence ODbL), via les tuiles VersaTiles et l'extrait
Geofabrik de l'index. L'interface doit
afficher « © contributeurs OpenStreetMap » avec un lien vers
<https://www.openstreetmap.org/copyright> partout où la surface est montrée. C'est déjà le cas
(`routes.surface.attribution` sur le web, `routes.surfaceDetail.attribution` sur mobile, lien vers
la page copyright) ; à faire au moment de la bascule :

- retirer toute mention de Stadia Maps comme source de la surface s'il y en a une (aucune trouvée
  dans les libellés), et ajouter « (ODbL) » au crédit si on veut être explicite ;
- dans la politique de confidentialité, la surface ne passe plus par Stadia : les traces enregistrées
  ne quittent plus nos serveurs pour ce calcul ;
- les plages de surface stockées sont tirées d'OSM : si elles sont un jour redistribuées en tant que
  base de données (export, API publique), c'est sous ODbL, avec la même attribution.

## Évaluation

```bash
cargo run --release --bin eval                         # en processus, tuiles via le cache
cargo run --release --bin eval -- --url http://localhost:8002   # contre un serveur lancé
EVAL_VERBOSE=1 cargo run --release --bin eval          # liste tous les écarts ≥ 40 m
```

Pour chaque `testdata/*.route.json`, l'outil construit la requête exacte de tribly à partir de
`tracks[0].line.coordinates` (polyline6, `bicycle`, `map_snap`, mêmes filtres), la projette comme
tribly (`TraceSurfaceComputer` → `TraceSurfaceProjection` → `SurfaceRanges`, `MIN_SPAN_M` = 20 →
`TrackSurface`) et compare mètre par mètre aux plages que tribly a calculées à partir des vraies
réponses Stadia. Rapport complet : [`eval/report.md`](eval/report.md) (répartition, matrice de
confusion, 10 plus grands écarts avec la rue appariée, nos plages inconnues, table de calibration).

L'outil évalue l'index OSM (`OSM_INDEX`, sinon `data/osm-index.bin` s'il existe ;
`--no-osm-index` pour s'en passer), puis les tuiles seules pour comparaison.

| Parcours | Longueur | Tuiles + index OSM | Tuiles seules | Revêtu / non revêtu (index) | Latence (index) |
|---|---:|---:|---:|---:|---:|
| graymond-ride-138 | 90,2 km | **98,0 %** | 86,0 % | 99,5 % | ≈ 90 ms |
| np-655-gravel | 52,2 km | **98,9 %** | 91,6 % | 99,3 % | ≈ 90 ms |
| np677-chill-gravel | 42,8 km | **98,5 %** | 93,5 % | 99,4 % | ≈ 65 ms |

Index du 2026-10-25 (Geofabrik), tuiles OSM du 2026-06-07. Sans compter les mètres où Stadia
lui-même ne donne pas de surface : 98,6 / 98,9 / 98,9 %. Avant la revue : 85,6 / 91,6 / 93,4 %
(tuiles seules).

La référence est Stadia (Valhalla sur un OSM de fin septembre 2026 environ), pas la vérité
terrain. Avec l'index, les écarts restants tiennent à la référence (Stadia sans surface, ou passé
ailleurs depuis un changement d'OSM), à des voies parallèles (trottoir ou chaussée) et à quelques
modifications d'OSM entre les deux dates. Sans l'index, ils viennent surtout de ce que les tuiles ne
disent pas (voir *Limites*). Le détail est dans le rapport.

Charge : 15 000 points bruités sur 180 km → 160 ms avec l'index, tuiles en cache. La réponse fait
1,4 Mo, 250 ko en gzip.

## Tests

```bash
cargo test
cargo clippy --all-targets
```

Tout tourne hors ligne. Les tests unitaires couvrent :

- polyline6, dont le rejet des deltas forgés ;
- la géodésie et le décodage MVT sur une vraie tuile ;
- le graphe : découpage aux bords de tuile et recollage ;
- la dérivation Valhalla, sur les tuiles et sur les tags OSM exacts (`mtb:scale`, `smoothness`,
  `sac_scale`, accès) ;
- le format, la lecture et le rattachement de l'index OSM ;
- l'appariement sur un réseau synthétique, la couverture des sauts par les arêtes et le 444
  quand seuls des points densifiés sont appariés ;
- le nettoyage du cache et le portage de la projection tribly.

Les tests d'intégration passent par le routeur axum (`tests/api.rs`). Ils utilisent 6 km du
parcours np-655, avec ses 3 tuiles réelles et un index OSM de la même zone
(`tests/fixtures/osm-index-np655.bin`, 50 ko) :

- le contrat de la réponse ;
- l'accord avec les plages Stadia : plus de 80 % avec les tuiles seules, plus de 95 % avec
  l'index ;
- la forme `shape`, les codes d'erreur Valhalla, dont le 171 sur deux vraies tuiles de mer ;
- le 503 quand le serveur de tuiles est injoignable, ou hors ligne quand la tuile manque ;
- le gzip.

`tests/tiles.rs` fait tourner un serveur de tuiles local. Il vérifie qu'une page d'erreur reçue en
200 n'est pas mise en cache et qu'un fichier du cache illisible est remplacé. Il vérifie aussi
qu'un 404 donne une erreur « indisponible » sans rien écrire.

Outil de développement : `cargo run --release --bin inspect -- near <lat> <lon> [rayon]` (les rues
autour d'un point et leurs attributs), `… osm-near <lat> <lon> [rayon]` (les voies de l'index
OSM, avec `OSM_INDEX`), `… trace <route.json> <de_m> <à_m>` (l'appariement d'un
tronçon, `CANDS=1` pour les candidats), `… prefetch|stats <route.json>…`,
`… fixture <route.json> <de_m> <à_m> <sortie.json> <dossier_tuiles>`.

## Limites connues

Sans index OSM :

- **Surface non revêtue approximative** : `unpaved` ne dit pas si c'est du gravier, du compacté,
  de la terre ou de l'herbe. La classe GRAVEL / DIRT est devinée (règles ci-dessus) : c'est la
  première source d'écart avec Stadia (59 % des écarts).
- **Tags perdus** : `mtb:scale` (qui, chez Valhalla, remplace même une surface asphaltée),
  `sac_scale` (chemin → `path`), `smoothness`, `access`, `cycleway:*`, `foot`, `segregated`,
  `footway=sidewalk` ne sont pas dans Shortbread. La classe PATH n'apparaît que par
  `tracktype=grade5`. PAVED_ROUGH n'apparaît que par `tracktype=grade1` sans surface : les pavés
  (`cobblestone`) et les valeurs que Shortbread met à nul (`wood`…) prennent la valeur par défaut
  du type de voie.
- **Voies absentes** : `highway=bridleway` n'est pas dans les tuiles. Ces passages sortent
  `unmatched` (inconnus pour tribly).
- **Fraîcheur** : les tuiles VersaTiles sont reconstruites de temps en temps (OSM du 2026-06-07
  pour celles utilisées ici, date donnée par `/health`). Les modifications OSM plus récentes n'y
  sont pas.

Dans tous les cas :

- **Topologie reconstruite** : pas d'identifiant de nœud ; les jonctions sont retrouvées par la
  géométrie (tolérance < 1 m). Deux voies qui se croisent sans sommet commun dans la tuile ne sont
  pas reliées (un saut à coût fixe compense dans l'appariement). Les sens uniques et les
  restrictions de tourner sont ignorés (une trace planifiée est déjà légale).
- **Index OSM** : en mémoire (≈ 270 Mo par région de la taille des Pays de la Loire). Le
  rattachement d'un segment se fait à la majorité ; un segment de tuile à cheval sur deux voies
  OSM prend les tags d'une seule. `mtb:scale:uphill` n'est pas lu.
- **Pas d'`osm_changeset`**, ni `admins`, `confidence_score`, `names`, `way_id`, `speed`…
- **Antiméridien** : une trace qui le traverse est refusée (444).
- **Un seul fournisseur de tuiles** : si `tiles.versatiles.org` est injoignable et que les tuiles ne
  sont pas en cache, 503 (tribly réessaie). Pour s'en affranchir, VersaTiles publie aussi le fichier
  `.versatiles` complet, que l'on peut servir soi-même et désigner par `TILES_URL`.
