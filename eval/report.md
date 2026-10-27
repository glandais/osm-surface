# Évaluation osm-surface vs Stadia Maps

Généré par `cargo run --release --bin eval` (en processus). Référence : plages de surface calculées par tribly à partir des vraies réponses Stadia (Valhalla) ; nos plages passent par le même portage de `TraceSurfaceProjection` + `SurfaceRanges` (MIN_SPAN_M = 20) + `TrackSurface`.

Sources : tuiles VersaTiles (OSM du 2026-06-07T23:59:58Z)

## Synthèse

| Parcours | Longueur | Points | Accord (classe) | Accord hors inconnu Stadia | Accord revêtu/non revêtu | Inconnu (nous) | Points unmatched | Latence |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| graymond-ride-138 | 90.2 km | 1513 | **86.0 %** | 86.5 % | 98.1 % | 0.9 % | 3 | 77 ms |
| np-655-gravel | 52.2 km | 780 | **91.6 %** | 91.5 % | 99.0 % | 1.3 % | 8 | 71 ms |
| np677-chill-gravel | 42.8 km | 617 | **93.5 %** | 93.9 % | 99.3 % | 0.0 % | 0 | 55 ms |

### D'où viennent les écarts

Les tuiles seules donnent 86 à 94 % d'accord par classe, 98 à 99 % sur revêtu / non revêtu.
L'écart restant (environ 20 km sur les trois parcours) a été inspecté tronçon par tronçon en
comparant aux tags OSM exacts :

- **59 % : surface réduite** à `paved` / `unpaved` par Shortbread. Valhalla lit la valeur exacte
  (`compacted`, `gravel`, `fine_gravel` → GRAVEL ; `ground`, `dirt`, `earth` → DIRT ; `grass` →
  PATH) ; pour un `unpaved`, la classe est devinée (voir README) et aucune règle sur les seuls
  attributs des tuiles ne dépasse 87,6 / 93,3 / 93,5 %.
- **14 % : âge des tuiles** (OSM du 2026-06-07, Stadia sur un OSM de fin septembre 2026 environ) :
  des `path` devenus `track` (DIRT par défaut chez Valhalla) et l'inverse, des surfaces ajoutées
  depuis.
- **6 % : `mtb:scale`** (Valhalla : 0 → dirt, 1 → gravel, ≥ 2 → path, même sur de l'asphalte ;
  graymond 18370-19320 : une route `surface=asphalt` + `mtb:scale=0` que Stadia donne DIRT),
  absent des tuiles comme `sac_scale` et `smoothness`.
- **3 % : `highway=bridleway`**, absent des tuiles (graymond 13850-14540 : 690 m de bridleway
  `surface=compacted` que Stadia voit GRAVEL, inconnus pour nous).
- **≈ 12 % : la référence** (Stadia UNKNOWN là où OSM a une surface, ou passé ailleurs depuis un
  changement d'OSM ; le chiffre « hors inconnu Stadia » retire les premiers), le reste :
  l'appariement (voies parallèles : trottoir contre chaussée, allée de service contre chemin).


## graymond-ride-138

90.2 km, 1513 points, 40 tuiles, 12974 segments de rue retenus, latence 77 ms.

| Classe | Nous (m) | Nous (%) | Stadia (m) | Stadia (%) |
|---|---:|---:|---:|---:|
| PAVED | 26895 | 29.8 % | 26166 | 29.0 % |
| PAVED_ROUGH | 44 | 0.0 % | 253 | 0.3 % |
| GRAVEL | 28270 | 31.3 % | 27286 | 30.2 % |
| DIRT | 34202 | 37.9 % | 35146 | 38.9 % |
| PATH | 0 | 0.0 % | 819 | 0.9 % |
| UNKNOWN | 836 | 0.9 % | 575 | 0.6 % |

Matrice de confusion (mètres ; lignes : Stadia, colonnes : nous)

| Stadia \ nous | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|
| PAVED | **25783** | 0 | 255 | 128 | 0 | 0 |
| PAVED_ROUGH | 16 | **44** | 193 | 0 | 0 | 0 |
| GRAVEL | 99 | 0 | **22390** | 4108 | 0 | 689 |
| DIRT | 990 | 0 | 4697 | **29374** | 0 | 85 |
| PATH | 0 | 0 | 228 | 591 | **0** | 0 |
| UNKNOWN | 6 | 0 | 507 | 0 | 0 | **62** |

Les 10 plus grands désaccords

| De (m) | À (m) | Longueur | Nous | Stadia | Milieu (lat, lon) \| rue appariée (tags → surface Valhalla) |
|---:|---:|---:|---|---|---|
| 2485 | 4151 | 1666 m | DIRT | GRAVEL | 47.188968, -1.536467 | path surface=unpaved → dirt |
| 18369 | 19323 | 954 m | PAVED | DIRT | 47.134093, -1.473840 | unclassified surface=paved tracktype=grade2 bicycle=yes → paved_smooth |
| 13851 | 14540 | 689 m | UNKNOWN | GRAVEL | 47.133001, -1.485367 | - |
| 13190 | 13848 | 658 m | GRAVEL | DIRT | 47.138034, -1.489752 | track surface=unpaved → gravel |
| 53529 | 54144 | 614 m | GRAVEL | DIRT | 47.120995, -1.416206 | path → compacted |
| 64939 | 65541 | 602 m | GRAVEL | DIRT | 47.098985, -1.421725 | path → compacted |
| 45144 | 45599 | 454 m | GRAVEL | DIRT | 47.145988, -1.398788 | path → compacted |
| 63486 | 63928 | 442 m | DIRT | GRAVEL | 47.090061, -1.412363 | path surface=unpaved → dirt |
| 37730 | 38155 | 426 m | DIRT | GRAVEL | 47.143917, -1.374325 | track → dirt |
| 7800 | 8202 | 402 m | DIRT | GRAVEL | 47.161559, -1.522876 | path surface=unpaved → dirt |

## np-655-gravel

52.2 km, 780 points, 32 tuiles, 14185 segments de rue retenus, latence 71 ms.

| Classe | Nous (m) | Nous (%) | Stadia (m) | Stadia (%) |
|---|---:|---:|---:|---:|
| PAVED | 21997 | 42.2 % | 21823 | 41.8 % |
| PAVED_ROUGH | 1321 | 2.5 % | 1321 | 2.5 % |
| GRAVEL | 14988 | 28.7 % | 15537 | 29.8 % |
| DIRT | 13197 | 25.3 % | 12968 | 24.9 % |
| UNKNOWN | 653 | 1.3 % | 507 | 1.0 % |

Matrice de confusion (mètres ; lignes : Stadia, colonnes : nous)

| Stadia \ nous | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|
| PAVED | **21662** | 0 | 124 | 36 | 0 | 0 |
| PAVED_ROUGH | 0 | **1321** | 0 | 0 | 0 | 0 |
| GRAVEL | 329 | 0 | **13095** | 1966 | 0 | 146 |
| DIRT | 5 | 0 | 1768 | **11194** | 0 | 0 |
| PATH | 0 | 0 | 0 | 0 | **0** | 0 |
| UNKNOWN | 0 | 0 | 0 | 0 | 0 | **507** |

Les 10 plus grands désaccords

| De (m) | À (m) | Longueur | Nous | Stadia | Milieu (lat, lon) \| rue appariée (tags → surface Valhalla) |
|---:|---:|---:|---|---|---|
| 6316 | 7139 | 823 m | DIRT | GRAVEL | 47.245051, -1.573376 | path surface=unpaved bicycle=yes → dirt |
| 28131 | 28951 | 820 m | GRAVEL | DIRT | 47.312955, -1.623749 | track surface=unpaved tracktype=grade2 bicycle=official → gravel |
| 48334 | 48670 | 336 m | GRAVEL | DIRT | 47.236609, -1.548867 | path surface=unpaved tracktype=grade2 bicycle=designated → compacted |
| 45791 | 46105 | 314 m | DIRT | GRAVEL | 47.254022, -1.538200 | path surface=unpaved → dirt |
| 36697 | 36976 | 279 m | GRAVEL | DIRT | 47.287879, -1.576650 | track surface=unpaved bicycle=yes → gravel |
| 5368 | 5624 | 256 m | DIRT | GRAVEL | 47.242479, -1.558755 | path surface=unpaved bicycle=yes → dirt |
| 43021 | 43221 | 200 m | DIRT | GRAVEL | 47.268989, -1.548028 | path surface=unpaved bicycle=yes → dirt |
| 11292 | 11441 | 149 m | GRAVEL | DIRT | 47.263450, -1.607699 | track surface=unpaved tracktype=grade3 bicycle=yes → gravel |
| 44685 | 44831 | 146 m | UNKNOWN | GRAVEL | 47.259076, -1.539007 | - |
| 12782 | 12912 | 130 m | DIRT | GRAVEL | 47.267512, -1.621558 | path surface=unpaved bicycle=yes → dirt |

## np677-chill-gravel

42.8 km, 617 points, 25 tuiles, 10463 segments de rue retenus, latence 55 ms.

| Classe | Nous (m) | Nous (%) | Stadia (m) | Stadia (%) |
|---|---:|---:|---:|---:|
| PAVED | 25421 | 59.4 % | 25369 | 59.3 % |
| GRAVEL | 8413 | 19.7 % | 10717 | 25.0 % |
| DIRT | 8982 | 21.0 % | 6580 | 15.4 % |
| UNKNOWN | 0 | 0.0 % | 151 | 0.4 % |

Matrice de confusion (mètres ; lignes : Stadia, colonnes : nous)

| Stadia \ nous | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|
| PAVED | **25246** | 0 | 121 | 2 | 0 | 0 |
| PAVED_ROUGH | 0 | **0** | 0 | 0 | 0 | 0 |
| GRAVEL | 164 | 0 | **8232** | 2321 | 0 | 0 |
| DIRT | 8 | 0 | 4 | **6567** | 0 | 0 |
| PATH | 0 | 0 | 0 | 0 | **0** | 0 |
| UNKNOWN | 3 | 0 | 56 | 92 | 0 | **0** |

Les 10 plus grands désaccords

| De (m) | À (m) | Longueur | Nous | Stadia | Milieu (lat, lon) \| rue appariée (tags → surface Valhalla) |
|---:|---:|---:|---|---|---|
| 6173 | 7186 | 1013 m | DIRT | GRAVEL | 47.233060, -1.599880 | path surface=unpaved → dirt |
| 12601 | 13192 | 592 m | DIRT | GRAVEL | 47.253246, -1.664653 | path surface=unpaved → dirt |
| 35661 | 35994 | 333 m | DIRT | GRAVEL | 47.195672, -1.662871 | path surface=unpaved → dirt |
| 8399 | 8663 | 264 m | DIRT | GRAVEL | 47.239675, -1.619913 | path surface=unpaved → dirt |
| 9707 | 9822 | 115 m | DIRT | GRAVEL | 47.243695, -1.633398 | path surface=unpaved → dirt |
| 35569 | 35661 | 92 m | DIRT | UNKNOWN | 47.195484, -1.664929 | path surface=unpaved → dirt |
| 33964 | 34051 | 87 m | GRAVEL | PAVED | 47.202114, -1.677205 | footway → compacted |
| 10103 | 10180 | 77 m | PAVED | GRAVEL | 47.244287, -1.637453 | service surface=paved tracktype=grade1 service=driveway → paved_smooth |
| 35513 | 35569 | 56 m | GRAVEL | UNKNOWN | 47.195765, -1.665817 | footway → compacted |
| 20661 | 20701 | 40 m | PAVED | GRAVEL | 47.220246, -1.688340 | residential → paved_smooth |

## Nos plages inconnues (≥ 30 m)

| Parcours | De (m) | À (m) | Longueur | Stadia y voit | Milieu (lat, lon) |
|---|---:|---:|---:|---|---|
| graymond-ride-138 | 13848 | 14540 | 692 m | GRAVEL 689, DIRT 3 | 47.133011, -1.485380 |
| graymond-ride-138 | 72199 | 72343 | 144 m | DIRT 82, UNKNOWN 62 | 47.136037, -1.433004 |
| np-655-gravel | 44178 | 44831 | 653 m | GRAVEL 146, UNKNOWN 507 | 47.261275, -1.538367 |

## Calibration : ce que dit Stadia selon les attributs de la rue appariée

Tous parcours confondus, échantillonné tous les 5 m. `kind | surface | tracktype | bicycle`.

| Attributs | Total (m) | PAVED | PAVED_ROUGH | GRAVEL | DIRT | PATH | UNKNOWN |
|---|---:|---:|---:|---:|---:|---:|---:|
| track | - | - | - | 27655 | 0 % |  | 2 % | 98 % |  |  |
| path | - | - | - | 26725 | 0 % |  | 90 % | 9 % | 1 % |  |
| path | unpaved | - | - | 19175 | 0 % |  | 31 % | 67 % | 2 % | 0 % |
| unclassified | - | - | - | 12295 | 100 % |  | 0 % | 0 % |  |  |
| cycleway | paved | - | - | 9640 | 100 % |  |  |  |  |  |
| residential | paved | - | - | 7345 | 100 % |  | 0 % | 0 % |  |  |
| path | unpaved | - | yes | 6575 |  |  | 27 % | 70 % | 4 % |  |
| tertiary | paved | - | - | 5445 | 99 % |  | 0 % | 0 % |  | 0 % |
| unclassified | paved | - | - | 5115 | 99 % |  | 0 % | 0 % |  |  |
| track | unpaved | grade3 | yes | 4410 |  |  | 97 % | 3 % |  |  |
| residential | - | - | - | 4130 | 98 % |  | 1 % | 1 % |  |  |
| path | paved | - | designated | 4025 | 100 % |  | 0 % |  |  | 0 % |
| cycleway | paved | - | designated | 3460 | 100 % |  |  |  |  |  |
| path | unpaved | - | designated | 2775 | 1 % |  | 99 % |  |  |  |
| tertiary | - | - | - | 2705 | 99 % |  | 0 % | 1 % |  |  |
| track | unpaved | - | yes | 2520 |  |  | 71 % | 29 % |  |  |
| track | paved | grade1 | yes | 2405 | 100 % |  |  |  |  |  |
| path | - | - | yes | 2330 | 0 % |  | 99 % | 1 % |  |  |
| secondary | paved | - | - | 2265 | 100 % |  |  |  |  |  |
| track | unpaved | grade3 | - | 1960 |  |  | 54 % | 46 % |  |  |
| track | unpaved | - | - | 1950 | 0 % |  | 58 % | 42 % |  |  |
| (unmatched) | 1485 |  |  | 56 % | 6 % |  | 38 % |
| service | - | - | - | 1480 | 98 % |  | 2 % | 0 % |  |  |
| track | - | - | yes | 1450 | 5 % |  | 10 % | 86 % |  |  |
| path | paved | grade1 | designated | 1360 | 94 % |  | 6 % |  |  |  |
| service | paved | - | - | 1200 | 99 % | 1 % |  |  |  |  |
| residential | paved | - | yes | 1095 | 100 % |  |  |  |  |  |
| path | paved | - | - | 1055 | 94 % |  | 4 % | 1 % |  |  |
| cycleway | paved | - | yes | 1050 | 100 % |  |  |  |  |  |
| unclassified | paved | - | yes | 975 | 100 % |  |  |  |  |  |
| unclassified | paved | grade2 | yes | 955 |  |  |  | 100 % |  |  |
| track | - | grade2 | - | 895 |  |  | 100 % |  |  |  |
| footway | unpaved | - | - | 845 | 22 % |  |  | 20 % |  | 59 % |
| track | - | grade1 | - | 815 |  | 100 % |  |  |  |  |
| track | unpaved | grade2 | official | 815 |  |  |  | 100 % |  |  |
| track | unpaved | grade4 | - | 770 |  |  |  | 100 % |  |  |
| path | unpaved | grade2 | designated | 755 |  |  | 60 % | 40 % |  |  |
| path | unpaved | grade1 | designated | 655 |  |  | 95 % | 5 % |  |  |
| track | - | grade4 | - | 655 |  |  | 100 % |  |  |  |
| path | - | - | designated | 645 | 10 % |  | 90 % |  |  |  |
| tertiary | paved | - | yes | 640 | 98 % |  | 1 % | 1 % |  |  |
| service | paved | - | yes | 590 | 86 % |  | 14 % |  |  |  |
| primary | paved | - | yes | 585 | 100 % |  |  |  |  |  |
| track | - | grade1 | yes | 550 |  | 100 % |  |  |  |  |
| track | unpaved | grade2 | yes | 530 |  |  | 100 % |  |  |  |
| footway | unpaved | - | yes | 510 | 9 % |  | 91 % |  |  |  |
| pedestrian | paved | - | yes | 500 | 99 % | 1 % |  |  |  |  |
| footway | paved | - | yes | 485 | 100 % |  |  |  |  |  |
| footway | - | - | permissive | 425 |  |  | 100 % |  |  |  |
| living_street | paved | - | - | 400 | 100 % |  |  |  |  |  |
| track | unpaved | grade1 | - | 400 |  |  | 85 % | 15 % |  |  |
| track | unpaved | grade2 | - | 385 |  |  | 100 % |  |  |  |
| path | paved | grade2 | designated | 335 | 100 % |  |  |  |  |  |
| footway | - | - | - | 330 | 56 % |  | 23 % | 5 % |  | 17 % |
| primary | paved | - | - | 270 | 100 % |  |  |  |  |  |
| secondary | - | - | - | 270 | 100 % |  |  |  |  |  |
| track | - | - | designated | 260 |  |  |  | 100 % |  |  |
| secondary | paved | - | yes | 240 | 100 % |  |  |  |  |  |
| unclassified | unpaved | grade1 | - | 220 |  |  | 100 % |  |  |  |
| track | paved | - | yes | 210 | 100 % |  |  |  |  |  |
| service | unpaved | - | - | 205 | 5 % |  | 10 % | 85 % |  |  |
| cycleway | paved | grade1 | - | 195 | 100 % |  |  |  |  |  |
| footway | - | - | dismount | 195 | 3 % | 97 % |  |  |  |  |
| service | paved | grade1 | - | 195 | 67 % |  | 33 % |  |  |  |
| footway | paved | - | - | 180 | 94 % |  | 6 % |  |  |  |
| path | unpaved | grade3 | designated | 155 |  |  | 100 % |  |  |  |
| track | unpaved | grade4 | yes | 150 |  |  | 3 % | 97 % |  |  |
| track | unpaved | grade5 | - | 145 |  |  |  | 100 % |  |  |
| path | - | - | mtb | 140 |  |  | 100 % |  |  |  |
| footway | paved | - | designated | 135 | 100 % |  |  |  |  |  |
| service | paved | - | designated | 130 | 4 % |  | 96 % |  |  |  |
| pedestrian | paved | - | designated | 115 | 100 % |  |  |  |  |  |
| track | paved | - | - | 110 | 100 % |  |  |  |  |  |
| cycleway | - | - | designated | 105 | 43 % |  | 57 % |  |  |  |
