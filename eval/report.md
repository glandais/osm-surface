# Évaluation osm-surface vs Stadia Maps

Généré par `cargo run --release --bin eval` (en processus). Référence : plages de surface calculées par tribly à partir des vraies réponses Stadia (Valhalla) ; nos plages passent par le même portage de `TraceSurfaceProjection` + `SurfaceRanges` (MIN_SPAN_M = 20) + `TrackSurface`.

Sources : tuiles VersaTiles (OSM du 2026-06-07T23:59:58Z) + index OSM `pays-de-la-loire-latest.osm.pbf` (846935 voies, OSM du 2026-10-25T21:21:21Z)

## Synthèse

| Parcours | Longueur | Points | Accord (classe) | Accord hors inconnu Stadia | Accord revêtu/non revêtu | Inconnu (nous) | Points unmatched | Latence |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| graymond-ride-138 | 90.2 km | 1513 | **98.0 %** | 98.6 % | 99.5 % | 0.5 % | 2 | 111 ms |
| np-655-gravel | 52.2 km | 780 | **98.9 %** | 98.9 % | 99.3 % | 1.3 % | 8 | 91 ms |
| np677-chill-gravel | 42.8 km | 617 | **98.5 %** | 98.9 % | 99.4 % | 0.0 % | 0 | 65 ms |

Les tuiles seules (sans index OSM), même appariement :

| Parcours | Longueur | Points | Accord (classe) | Accord hors inconnu Stadia | Accord revêtu/non revêtu | Inconnu (nous) | Points unmatched | Latence |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| graymond-ride-138 | 90.2 km | 1513 | **86.0 %** | 86.5 % | 98.1 % | 0.9 % | 3 | 57 ms |
| np-655-gravel | 52.2 km | 780 | **91.6 %** | 91.5 % | 99.0 % | 1.3 % | 8 | 52 ms |
| np677-chill-gravel | 42.8 km | 617 | **93.5 %** | 93.9 % | 99.3 % | 0.0 % | 0 | 41 ms |

### D'où viennent les écarts

**Avec l'index OSM** (98 à 99 % d'accord), il reste un peu plus d'un kilomètre par parcours,
en morceaux de 40 à 300 m :

- **La référence elle-même** : Stadia ne donne pas de surface (UNKNOWN) là où OSM en a une
  (graymond 58540-59120 : un `footway surface=ground` ; np677 35510-35660), ou passe ailleurs
  parce qu'OSM a changé depuis (graymond 47775-48035 : une `track` passée depuis en
  `bicycle=no`, que nous ne prenons plus). Le chiffre « hors inconnu Stadia » retire les premiers.
- **Voies parallèles** : trottoir ou `footway` contre la chaussée voisine (graymond 4410-4615,
  np655 41245-41310, np677 33965-34050), allée de service asphaltée contre un chemin parallèle
  (graymond 2150-2240, np655 44830-44955, np677 10105-10180). Les deux appariements sont
  plausibles ; un léger surcoût des footways pour le vélo en corrige déjà une partie.
- **Date des données** : l'index est construit sur l'OSM du jour (Geofabrik), Stadia sur un OSM
  de fin septembre 2026 environ ; quelques chemins ont changé de `surface` entre les deux.

**Les tuiles seules** (86 à 94 %) perdent ce que l'index rend (mesuré par la revue sur ces
trois parcours, environ 20 km d'écart en tout) :

- **59 % : surface réduite** à `paved` / `unpaved` par Shortbread. Valhalla lit la valeur exacte
  (`compacted`, `gravel`, `fine_gravel` → GRAVEL ; `ground`, `dirt`, `earth` → DIRT ; `grass` →
  PATH) ; pour un `unpaved`, la classe est devinée (voir README) et aucune règle sur les seuls
  attributs des tuiles ne dépasse 87,6 / 93,3 / 93,5 %.
- **14 % : âge des tuiles** (OSM du 2026-06-07) : des `path` devenus `track` (DIRT par défaut chez
  Valhalla) et l'inverse, des surfaces ajoutées depuis.
- **6 % : `mtb:scale`** (Valhalla : 0 → dirt, 1 → gravel, ≥ 2 → path, même sur de l'asphalte ;
  graymond 18370-19320 : une route `surface=asphalt` + `mtb:scale=0` que Stadia donne DIRT),
  absent des tuiles comme `sac_scale` et `smoothness`.
- **3 % : `highway=bridleway`**, absent des tuiles (graymond 13850-14540 : 690 m de bridleway
  `surface=compacted` que Stadia voit GRAVEL, inconnus pour nous).
- **≈ 12 % : la référence** (Stadia UNKNOWN ou en désaccord avec l'OSM actuel), le reste :
  l'appariement.


## graymond-ride-138

90.2 km, 1513 points, 40 tuiles, 12932 segments de rue retenus, latence 111 ms.

| Classe | Nous (m) | Nous (%) | Stadia (m) | Stadia (%) |
|---|---:|---:|---:|---:|
| PAVED | 25948 | 28.8 % | 26166 | 29.0 % |
| PAVED_ROUGH | 239 | 0.3 % | 253 | 0.3 % |
| GRAVEL | 27152 | 30.1 % | 27286 | 30.2 % |
| DIRT | 35652 | 39.5 % | 35146 | 38.9 % |
| PATH | 795 | 0.9 % | 819 | 0.9 % |
| UNKNOWN | 461 | 0.5 % | 575 | 0.6 % |

Matrice de confusion (mètres ; lignes : Stadia, colonnes : nous)

| Stadia \ nous | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|
| PAVED | **25791** | 2 | 257 | 22 | 0 | 94 |
| PAVED_ROUGH | 16 | **237** | 0 | 0 | 0 | 0 |
| GRAVEL | 98 | 0 | **26721** | 243 | 0 | 224 |
| DIRT | 36 | 0 | 158 | **34870** | 0 | 82 |
| PATH | 0 | 0 | 11 | 13 | **794** | 0 |
| UNKNOWN | 6 | 0 | 5 | 502 | 0 | **62** |

Les 10 plus grands désaccords

| De (m) | À (m) | Longueur | Nous | Stadia | Milieu (lat, lon) \| rue appariée (tags → surface Valhalla) |
|---:|---:|---:|---|---|---|
| 58542 | 58855 | 312 m | DIRT | UNKNOWN | 47.097530, -1.404118 | w422352280 footway surface=ground → dirt |
| 4410 | 4614 | 203 m | GRAVEL | PAVED | 47.181629, -1.526891 | w124550166 footway surface=compacted → compacted |
| 58929 | 59119 | 190 m | DIRT | UNKNOWN | 47.094681, -1.403803 | w422352280 footway surface=ground → dirt |
| 47775 | 47963 | 188 m | UNKNOWN | GRAVEL | 47.145797, -1.419734 | - |
| 2150 | 2239 | 89 m | PAVED | GRAVEL | 47.195255, -1.546263 | w235149288 service surface=asphalt service=driveway bicycle=yes → paved_smooth |
| 72261 | 72343 | 82 m | UNKNOWN | DIRT | 47.136162, -1.433372 | - |
| 60200 | 60271 | 71 m | GRAVEL | DIRT | 47.091115, -1.400543 | w1563199412 path → compacted |
| 47963 | 48033 | 70 m | UNKNOWN | PAVED | 47.146205, -1.421329 | - |
| 37489 | 37546 | 58 m | DIRT | GRAVEL | 47.145896, -1.375736 | w1188275662 track → dirt |
| 61619 | 61673 | 54 m | DIRT | GRAVEL | 47.079911, -1.397058 | w360578119 path surface=dirt → dirt |

## np-655-gravel

52.2 km, 780 points, 32 tuiles, 14169 segments de rue retenus, latence 91 ms.

| Classe | Nous (m) | Nous (%) | Stadia (m) | Stadia (%) |
|---|---:|---:|---:|---:|
| PAVED | 21895 | 42.0 % | 21823 | 41.8 % |
| PAVED_ROUGH | 1321 | 2.5 % | 1321 | 2.5 % |
| GRAVEL | 15320 | 29.4 % | 15537 | 29.8 % |
| DIRT | 12967 | 24.9 % | 12968 | 24.9 % |
| UNKNOWN | 653 | 1.3 % | 507 | 1.0 % |

Matrice de confusion (mètres ; lignes : Stadia, colonnes : nous)

| Stadia \ nous | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|
| PAVED | **21668** | 0 | 148 | 7 | 0 | 0 |
| PAVED_ROUGH | 0 | **1321** | 0 | 0 | 0 | 0 |
| GRAVEL | 222 | 0 | **15138** | 31 | 0 | 146 |
| DIRT | 5 | 0 | 33 | **12929** | 0 | 0 |
| PATH | 0 | 0 | 0 | 0 | **0** | 0 |
| UNKNOWN | 0 | 0 | 0 | 0 | 0 | **507** |

Les 10 plus grands désaccords

| De (m) | À (m) | Longueur | Nous | Stadia | Milieu (lat, lon) \| rue appariée (tags → surface Valhalla) |
|---:|---:|---:|---|---|---|
| 44685 | 44831 | 146 m | UNKNOWN | GRAVEL | 47.259076, -1.539007 | - |
| 44831 | 44956 | 125 m | PAVED | GRAVEL | 47.257933, -1.539590 | w168521015 service surface=asphalt service=driveway access=destination bicycle=designated → paved_smooth |
| 41243 | 41312 | 69 m | GRAVEL | PAVED | 47.275579, -1.559305 | w32763847 footway → compacted |
| 7395 | 7449 | 54 m | GRAVEL | PAVED | 47.249136, -1.579021 | w45273916 footway surface=compacted bicycle=yes → compacted |
| 47419 | 47449 | 30 m | PAVED | GRAVEL | 47.243605, -1.544025 | w1535642156 path surface=asphalt tracktype=grade1 smoothness=intermediate access=yes bicycle=designated → paved_smooth |
| 46258 | 46285 | 27 m | PAVED | GRAVEL | 47.251424, -1.538724 | w205503194 service access=private → paved_smooth |
| 29786 | 29810 | 24 m | PAVED | GRAVEL | 47.318922, -1.616700 | w1457768941 unclassified → paved_smooth |
| 45017 | 45039 | 23 m | GRAVEL | PAVED | 47.256884, -1.539242 | w390627032 path surface=compacted smoothness=bad → compacted |
| 42466 | 42485 | 19 m | DIRT | GRAVEL | 47.270866, -1.555400 | (tuile) path surface=unpaved → dirt |
| 41205 | 41222 | 17 m | GRAVEL | DIRT | 47.275910, -1.559976 | w32763847 footway → compacted |

## np677-chill-gravel

42.8 km, 617 points, 25 tuiles, 10311 segments de rue retenus, latence 65 ms.

| Classe | Nous (m) | Nous (%) | Stadia (m) | Stadia (%) |
|---|---:|---:|---:|---:|
| PAVED | 25432 | 59.4 % | 25369 | 59.3 % |
| GRAVEL | 10678 | 24.9 % | 10717 | 25.0 % |
| DIRT | 6706 | 15.7 % | 6580 | 15.4 % |
| UNKNOWN | 0 | 0.0 % | 151 | 0.4 % |

Matrice de confusion (mètres ; lignes : Stadia, colonnes : nous)

| Stadia \ nous | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|
| PAVED | **25270** | 0 | 97 | 2 | 0 | 0 |
| PAVED_ROUGH | 0 | **0** | 0 | 0 | 0 | 0 |
| GRAVEL | 150 | 0 | **10387** | 180 | 0 | 0 |
| DIRT | 8 | 0 | 47 | **6525** | 0 | 0 |
| PATH | 0 | 0 | 0 | 0 | **0** | 0 |
| UNKNOWN | 3 | 0 | 148 | 0 | 0 | **0** |

Les 10 plus grands désaccords

| De (m) | À (m) | Longueur | Nous | Stadia | Milieu (lat, lon) \| rue appariée (tags → surface Valhalla) |
|---:|---:|---:|---|---|---|
| 35513 | 35661 | 148 m | GRAVEL | UNKNOWN | 47.195591, -1.665266 | w30434407 path surface=compacted → compacted |
| 9707 | 9822 | 115 m | DIRT | GRAVEL | 47.243695, -1.633398 | w202983566 path surface=dirt → dirt |
| 33964 | 34051 | 87 m | GRAVEL | PAVED | 47.202114, -1.677205 | w32706564 footway → compacted |
| 10103 | 10180 | 77 m | PAVED | GRAVEL | 47.244287, -1.637453 | w202983561 service surface=asphalt tracktype=grade1 service=driveway → paved_smooth |
| 8604 | 8663 | 59 m | DIRT | GRAVEL | 47.240086, -1.621014 | w1390211484 path surface=dirt → dirt |
| 9917 | 9959 | 42 m | GRAVEL | DIRT | 47.243810, -1.635495 | w1557820329 path → compacted |
| 20661 | 20701 | 40 m | PAVED | GRAVEL | 47.220246, -1.688340 | w1476202702 residential → paved_smooth |
| 6147 | 6173 | 26 m | PAVED | GRAVEL | 47.229285, -1.597775 | w589504315 path surface=paving_stones → paved |
| 4147 | 4155 | 8 m | GRAVEL | PAVED | 47.221920, -1.578890 | w1211938914 path surface=compacted bicycle=designated → compacted |
| 18240 | 18244 | 4 m | PAVED | DIRT | 47.223601, -1.670090 | w1236334450 tertiary surface=asphalt → paved_smooth |

## Nos plages inconnues (≥ 30 m)

| Parcours | De (m) | À (m) | Longueur | Stadia y voit | Milieu (lat, lon) |
|---|---:|---:|---:|---|---|
| graymond-ride-138 | 47040 | 47099 | 60 m | PAVED 24, GRAVEL 36 | 47.147128, -1.412870 |
| graymond-ride-138 | 47775 | 48033 | 258 m | PAVED 70, GRAVEL 188 | 47.145925, -1.420156 |
| graymond-ride-138 | 72199 | 72343 | 144 m | DIRT 82, UNKNOWN 62 | 47.136037, -1.433004 |
| np-655-gravel | 44178 | 44831 | 653 m | GRAVEL 146, UNKNOWN 507 | 47.261275, -1.538367 |

## Calibration : ce que dit Stadia selon les attributs de la rue appariée

Tous parcours confondus, échantillonné tous les 5 m. `kind | surface | tracktype | bicycle`.

| Attributs | Total (m) | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|---:|
| track | - | - | - | 28680 | 0 % |  | 0 % | 99 % |  |  |
| path | - | - | - | 24690 | 0 % |  | 98 % | 1 % | 1 % |  |
| unclassified | - | - | - | 11925 | 100 % |  | 0 % | 0 % |  |  |
| path | dirt | - | - | 11625 | 0 % |  | 2 % | 97 % | 0 % |  |
| cycleway | asphalt | - | - | 8635 | 100 % |  |  |  |  |  |
| residential | asphalt | - | - | 7505 | 100 % |  | 0 % | 0 % |  |  |
| tertiary | asphalt | - | - | 5630 | 99 % |  | 0 % | 0 % |  | 0 % |
| unclassified | asphalt | - | - | 5495 | 99 % |  | 0 % | 0 % |  |  |
| path | compacted | - | - | 5075 | 0 % |  | 98 % |  |  | 2 % |
| path | dirt | - | yes | 4075 |  |  | 0 % | 100 % | 0 % |  |
| residential | - | - | - | 3975 | 98 % |  | 1 % | 1 % |  |  |
| path | asphalt | - | designated | 2925 | 100 % |  | 0 % |  |  |  |
| cycleway | asphalt | - | designated | 2890 | 100 % |  |  |  |  |  |
| path | compacted | - | designated | 2605 | 0 % |  | 100 % |  |  |  |
| track | compacted | grade3 | yes | 2605 |  |  | 100 % |  |  |  |
| tertiary | - | - | - | 2445 | 99 % |  | 0 % | 1 % |  |  |
| track | asphalt | grade1 | yes | 2355 | 100 % |  |  |  |  |  |
| path | - | - | yes | 2230 | 0 % |  | 100 % |  |  |  |
| secondary | asphalt | - | - | 2145 | 100 % |  |  |  |  |  |
| track | compacted | - | yes | 1790 |  |  | 100 % |  |  |  |
| track | unpaved | grade3 | yes | 1655 |  |  | 100 % |  |  |  |
| service | - | - | - | 1480 | 98 % |  | 2 % | 0 % |  |  |
| cycleway | concrete | - | - | 1320 | 100 % |  |  |  |  |  |
| path | asphalt | grade1 | designated | 1270 | 97 % |  | 3 % |  |  |  |
| track | - | - | yes | 1260 |  |  | 2 % | 98 % |  |  |
| service | asphalt | - | - | 1140 | 100 % |  |  |  |  |  |
| track | gravel | - | - | 1125 |  |  | 100 % |  |  |  |
| (unmatched) | 1115 | 9 % |  | 33 % | 8 % |  | 51 % |
| residential | asphalt | - | yes | 1095 | 100 % |  |  |  |  |  |
| track | dirt | grade3 | - | 1095 |  |  |  | 100 % |  |  |
| cycleway | asphalt | - | yes | 1050 | 100 % |  |  |  |  |  |
| unclassified | asphalt | - | yes | 975 | 100 % |  |  |  |  |  |
| unclassified | asphalt | grade2 | yes | 955 |  |  |  | 100 % |  |  |
| track | - | grade2 | - | 895 |  |  | 100 % |  |  |  |
| path | fine_gravel | - | yes | 815 |  |  | 100 % |  |  |  |
| track | - | grade1 | - | 815 |  | 100 % |  |  |  |  |
| track | ground | grade2 | official | 815 |  |  |  | 100 % |  |  |
| path | mud | - | - | 800 |  |  |  | 100 % |  |  |
| path | asphalt | - | - | 775 | 100 % |  |  |  |  |  |
| path | concrete | - | designated | 765 | 99 % |  |  |  |  | 1 % |
| path | ground | - | - | 730 |  |  | 1 % | 99 % |  |  |
| bridleway | compacted | - | yes | 695 |  |  | 100 % |  |  |  |
| track | earth | - | - | 665 | 1 % |  |  | 99 % |  |  |
| track | - | grade4 | - | 660 |  |  | 100 % |  |  |  |
| tertiary | asphalt | - | yes | 640 | 98 % |  | 1 % | 1 % |  |  |
| path | compacted | grade1 | designated | 625 |  |  | 100 % |  |  |  |
| path | gravel | - | - | 625 |  |  | 100 % |  |  |  |
| service | asphalt | - | yes | 590 | 86 % |  | 14 % |  |  |  |
| footway | ground | - | - | 585 |  |  |  | 16 % |  | 84 % |
| primary | asphalt | - | yes | 585 | 100 % |  |  |  |  |  |
| path | wood | - | designated | 580 | 7 % |  | 93 % |  |  |  |
| track | dirt | grade4 | - | 570 |  |  |  | 100 % |  |  |
| track | - | grade1 | yes | 550 |  | 100 % |  |  |  |  |
| track | unpaved | grade3 | - | 530 |  |  | 100 % |  |  |  |
| path | earth | - | yes | 495 |  |  |  | 100 % |  |  |
| path | gravel | - | yes | 490 |  |  | 100 % |  |  |  |
| footway | compacted | - | yes | 450 | 10 % |  | 90 % |  |  |  |
| path | compacted | grade2 | designated | 450 |  |  | 100 % |  |  |  |
| path | compacted | - | yes | 430 |  |  | 100 % |  |  |  |
| footway | - | - | permissive | 425 |  |  | 100 % |  |  |  |
| footway | paving_stones | - | yes | 410 | 100 % |  |  |  |  |  |
| track | gravel | grade2 | - | 385 |  |  | 100 % |  |  |  |
| living_street | asphalt | - | - | 375 | 100 % |  |  |  |  |  |
| track | earth | - | yes | 340 |  |  |  | 100 % |  |  |
| track | gravel | grade1 | - | 340 |  |  | 100 % |  |  |  |
| path | asphalt | grade2 | designated | 335 | 100 % |  |  |  |  |  |
| pedestrian | asphalt | - | yes | 335 | 100 % |  |  |  |  |  |
| cycleway | paved | - | designated | 330 | 100 % |  |  |  |  |  |
| track | dirt | - | - | 325 |  |  |  | 100 % |  |  |
| path | dirt | grade2 | designated | 305 |  |  |  | 100 % |  |  |
| path | grass | - | - | 300 |  |  |  |  | 100 % |  |
| track | ground | - | yes | 280 |  |  |  | 100 % |  |  |
| track | unpaved | grade2 | yes | 280 |  |  | 100 % |  |  |  |
| path | concrete | grade1 | designated | 270 | 100 % |  |  |  |  |  |
| secondary | - | - | - | 270 | 100 % |  |  |  |  |  |
| footway | - | - | - | 265 | 60 % |  | 13 % | 6 % |  | 21 % |
| track | compacted | grade3 | - | 265 |  |  | 100 % |  |  |  |
| track | - | - | designated | 260 |  |  |  | 100 % |  |  |
| track | gravel | grade3 | - | 260 |  |  | 100 % |  |  |  |
| path | concrete | - | - | 250 | 88 % |  | 6 % | 6 % |  |  |
| primary | asphalt | - | - | 250 | 100 % |  |  |  |  |  |
| track | compacted | grade2 | yes | 250 |  |  | 100 % |  |  |  |
| secondary | asphalt | - | yes | 240 | 100 % |  |  |  |  |  |
| path | grass | - | yes | 235 |  |  |  |  | 100 % |  |
| path | sand | - | - | 225 |  |  | 100 % |  |  |  |
| unclassified | gravel | grade1 | - | 220 |  |  | 100 % |  |  |  |
| path | wood | - | - | 210 | 5 % |  | 81 % | 14 % |  |  |
| track | asphalt | - | yes | 210 | 100 % |  |  |  |  |  |
| footway | unhewn_cobblestone | - | dismount | 195 | 3 % | 97 % |  |  |  |  |
| service | asphalt | grade1 | - | 195 | 67 % |  | 33 % |  |  |  |
| footway | compacted | - | - | 185 | 100 % |  |  |  |  |  |
| service | dirt | - | - | 175 |  |  |  | 100 % |  |  |
| footway | concrete | - | - | 165 | 100 % |  |  |  |  |  |
| pedestrian | paving_stones | - | yes | 165 | 97 % | 3 % |  |  |  |  |
| path | compacted | grade3 | designated | 155 |  |  | 100 % |  |  |  |
| track | dirt | grade4 | yes | 150 |  |  | 3 % | 97 % |  |  |
| track | ground | grade3 | yes | 150 |  |  |  | 100 % |  |  |
| track | dirt | grade5 | - | 145 |  |  |  | 100 % |  |  |
| track | unpaved | grade4 | - | 145 |  |  |  | 100 % |  |  |
| path | - | - | mtb | 140 |  |  | 100 % |  |  |  |
| service | asphalt | - | designated | 130 | 4 % |  | 96 % |  |  |  |
| cycleway | paving_stones | - | designated | 125 | 100 % |  |  |  |  |  |
| path | gravel | - | designated | 120 |  |  | 100 % |  |  |  |
| path | unpaved | - | - | 120 |  |  | 38 % | 62 % |  |  |
| track | mud | grade3 | - | 115 |  |  |  | 100 % |  |  |
| track | asphalt | - | - | 110 | 100 % |  |  |  |  |  |
| track | dirt | - | yes | 110 |  |  |  | 100 % |  |  |
| pedestrian | asphalt | - | designated | 100 | 100 % |  |  |  |  |  |
