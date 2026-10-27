//! Valhalla's derivation of edge attributes from OSM tags (`lua/graph.lua`,
//! `src/mjolnir/pbfgraphparser.cc`, `valhalla/mjolnir/adminconstants.h`), applied to what the
//! Shortbread `streets` layer keeps of the tags.
//!
//! Shortbread reduces the `surface` tag to `paved` / `unpaved` (and drops `smoothness`,
//! `mtb:scale`, `sac_scale`, `cycleway:*`, `foot`, `access`, `segregated`), so where Valhalla
//! would read the exact OSM value, a fixed choice stands for the whole class; see
//! [`surface_of`] and the README for the table.

use crate::config::AccessRules;
use crate::mvt::StreetAttrs;
use serde::Serialize;

/// `baldr::Surface`, best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    PavedSmooth,
    Paved,
    PavedRough,
    Compacted,
    Dirt,
    Gravel,
    Path,
    Impassable,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::PavedSmooth => "paved_smooth",
            Surface::Paved => "paved",
            Surface::PavedRough => "paved_rough",
            Surface::Compacted => "compacted",
            Surface::Dirt => "dirt",
            Surface::Gravel => "gravel",
            Surface::Path => "path",
            Surface::Impassable => "impassable",
        }
    }

    /// Valhalla's `unpaved` flag: anything worse than `paved_rough`.
    pub fn unpaved(self) -> bool {
        self > Surface::PavedRough
    }
}

/// Valhalla's mapping of a raw OSM `surface` value (pbfgraphparser.cc, `tag_handlers_["surface"]`),
/// substring matches in the same order. `None`: not recognised, the defaults apply.
pub fn surface_from_osm(value: &str) -> Option<Surface> {
    let v = value.to_ascii_lowercase();
    let has = |s: &str| v.contains(s);
    if has("unpaved") {
        Some(Surface::Gravel)
    } else if has("paved")
        || has("pavement")
        || has("asphalt")
        || has("concrete")
        || has("cement")
        || has("chipseal")
        || has("metal")
    {
        Some(Surface::PavedSmooth)
    } else if has("tartan")
        || has("pavingstone")
        || has("paving_stones")
        || has("sett")
        || has("grass_paver")
        || has("rubber")
        || has("acrylic")
        || has("plastic")
    {
        Some(Surface::Paved)
    } else if has("cobblestone") || has("brick") || v == "stone" {
        Some(Surface::PavedRough)
    } else if has("compacted") || has("wood") || has("boardwalk") {
        Some(Surface::Compacted)
    } else if has("dirt")
        || has("natural")
        || has("earth")
        || has("ground")
        || has("mud")
        || has("clay")
        || has("laterite")
        || has("soil")
    {
        Some(Surface::Dirt)
    } else if has("gravel") || has("pebblestone") || has("sand") {
        Some(Surface::Gravel)
    } else if has("grass") || has("artificial_turf") || has("rock") || has("stepping_stones") {
        Some(Surface::Path)
    } else {
        None
    }
}

/// Valhalla's mapping of `tracktype` (used only when there is no `surface` tag).
pub fn surface_from_tracktype(grade: &str) -> Option<Surface> {
    match grade {
        "grade1" => Some(Surface::PavedRough),
        "grade2" => Some(Surface::Compacted),
        "grade3" => Some(Surface::Dirt),
        "grade4" => Some(Surface::Gravel),
        "grade5" => Some(Surface::Path),
        _ => None,
    }
}

/// `baldr::RoadClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadClass {
    Motorway,
    Trunk,
    Primary,
    Secondary,
    Tertiary,
    Unclassified,
    Residential,
    ServiceOther,
}

impl RoadClass {
    pub fn as_str(self) -> &'static str {
        match self {
            RoadClass::Motorway => "motorway",
            RoadClass::Trunk => "trunk",
            RoadClass::Primary => "primary",
            RoadClass::Secondary => "secondary",
            RoadClass::Tertiary => "tertiary",
            RoadClass::Unclassified => "unclassified",
            RoadClass::Residential => "residential",
            RoadClass::ServiceOther => "service_other",
        }
    }

    fn is_road(self) -> bool {
        !matches!(self, RoadClass::ServiceOther)
    }
}

/// `baldr::Use`, the values that can come out of the Shortbread kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Use {
    Road,
    Ramp,
    Track,
    Driveway,
    Alley,
    ParkingAisle,
    EmergencyAccess,
    DriveThrough,
    LivingStreet,
    ServiceRoad,
    Cycleway,
    Footway,
    Steps,
    Path,
    Pedestrian,
    Bridleway,
    Bus,
    Other,
}

impl Use {
    pub fn as_str(self) -> &'static str {
        match self {
            Use::Road => "road",
            Use::Ramp => "ramp",
            Use::Track => "track",
            Use::Driveway => "driveway",
            Use::Alley => "alley",
            Use::ParkingAisle => "parking_aisle",
            Use::EmergencyAccess => "emergency_access",
            Use::DriveThrough => "drive_through",
            Use::LivingStreet => "living_street",
            Use::ServiceRoad => "service_road",
            Use::Cycleway => "cycleway",
            Use::Footway => "footway",
            Use::Steps => "steps",
            Use::Path => "path",
            Use::Pedestrian => "pedestrian",
            Use::Bridleway => "bridleway",
            Use::Bus => "bus",
            Use::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleLane {
    None,
    Shared,
    Dedicated,
    Separated,
}

impl CycleLane {
    pub fn as_str(self) -> &'static str {
        match self {
            CycleLane::None => "none",
            CycleLane::Shared => "shared",
            CycleLane::Dedicated => "dedicated",
            CycleLane::Separated => "separated",
        }
    }
}

/// The travel mode a costing filters the network for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Bicycle,
    Pedestrian,
    Auto,
}

impl Mode {
    /// The Valhalla costings, grouped by the access they need.
    pub fn of_costing(costing: &str) -> Option<Mode> {
        match costing {
            "bicycle" | "bikeshare" | "motor_scooter" => Some(Mode::Bicycle),
            "pedestrian" | "multimodal" => Some(Mode::Pedestrian),
            "auto" | "auto_shorter" | "auto_data_fix" | "taxi" | "bus" | "truck" | "motorcycle"
            | "hov" => Some(Mode::Auto),
            _ => None,
        }
    }
}

/// What Valhalla would store on the edges of a Shortbread street.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct EdgeInfo {
    pub surface: Surface,
    pub use_: Use,
    pub cycle_lane: CycleLane,
    pub road_class: RoadClass,
    pub bike: bool,
    pub foot: bool,
    pub auto: bool,
}

impl EdgeInfo {
    pub fn allows(&self, mode: Mode) -> bool {
        match mode {
            Mode::Bicycle => self.bike,
            Mode::Pedestrian => self.foot,
            Mode::Auto => self.auto,
        }
    }
}

/// Kinds that are not part of a routable street network (rails, runways...).
pub fn is_routable_kind(kind: &str) -> bool {
    matches!(
        kind,
        "motorway"
            | "trunk"
            | "primary"
            | "secondary"
            | "tertiary"
            | "unclassified"
            | "residential"
            | "living_street"
            | "pedestrian"
            | "service"
            | "track"
            | "footway"
            | "sidewalk"
            | "crossing"
            | "steps"
            | "path"
            | "cycleway"
            | "bridleway"
            | "busway"
            | "road"
    )
}

fn road_class_of(kind: &str) -> RoadClass {
    match kind {
        "motorway" => RoadClass::Motorway,
        "trunk" => RoadClass::Trunk,
        "primary" => RoadClass::Primary,
        "secondary" => RoadClass::Secondary,
        "tertiary" => RoadClass::Tertiary,
        "unclassified" => RoadClass::Unclassified,
        "residential" => RoadClass::Residential,
        _ => RoadClass::ServiceOther,
    }
}

/// Valhalla's `access` table for a `bicycle=` value: `Some(true)` allows, `Some(false)` forbids.
fn bicycle_tag(value: Option<&str>) -> Option<bool> {
    match value? {
        "yes" | "private" | "permissive" | "use_sidepath" | "delivery" | "designated"
        | "dismount" | "destination" | "customers" | "official" | "public" | "restricted"
        | "allowed" | "permit" | "residents" | "mtb" => Some(true),
        "no" | "agricultural" | "discouraged" | "forestry" | "emergency" | "psv" => Some(false),
        _ => None,
    }
}

/// The edge attributes of a street, Valhalla style.
pub fn derive(a: &StreetAttrs, rules: AccessRules) -> EdgeInfo {
    let kind = a.kind.as_str();
    let road_class = road_class_of(kind);
    let france = rules == AccessRules::France;

    // Default access per highway (graph.lua `highway` table), then the country defaults
    // (adminconstants.h: France opens footways to bicycles, cycleways to pedestrians, and
    // closes trunks to both).
    let (mut auto, mut foot, mut bike) = match kind {
        "motorway" => (true, false, false),
        "trunk" if france => (true, false, false),
        "trunk" | "primary" | "secondary" | "tertiary" | "unclassified" | "residential"
        | "living_street" | "service" | "track" | "road" => (true, true, true),
        "footway" | "sidewalk" | "crossing" => (false, true, france),
        "pedestrian" => (false, true, false),
        "steps" => (false, true, true),
        "path" => (false, true, true),
        "cycleway" => (false, france, true),
        "bridleway" => (false, false, false),
        "busway" => (false, false, false),
        _ => (false, false, false),
    };
    if let Some(b) = bicycle_tag(a.bicycle.as_deref()) {
        bike = b;
    }
    if kind == "busway" {
        auto = false;
        foot = false;
    }

    let use_ = if a.link && road_class.is_road() {
        Use::Ramp
    } else {
        match kind {
            "track" => Use::Track,
            "living_street" => Use::LivingStreet,
            "service" => match a.service.as_deref() {
                Some("driveway") => Use::Driveway,
                Some("alley") => Use::Alley,
                Some("parking_aisle") => Use::ParkingAisle,
                Some("emergency_access") => Use::EmergencyAccess,
                Some("drive-through") => Use::DriveThrough,
                _ => Use::ServiceRoad,
            },
            "cycleway" => Use::Cycleway,
            "footway" | "sidewalk" | "crossing" => Use::Footway,
            "steps" => Use::Steps,
            "path" => Use::Path,
            "pedestrian" => Use::Pedestrian,
            "bridleway" => Use::Bridleway,
            "busway" => Use::Bus,
            _ => Use::Road,
        }
    };

    // graph.lua: on a cycleway, footway or path open to bicycles, the lane is the way itself;
    // without `foot` / `segregated`, a cycleway is "separated" (no pedestrians in the lua
    // defaults) and a footway or path "shared". Elsewhere it comes from `cycleway:*`, which
    // Shortbread drops.
    let lua_bike = match kind {
        "footway" | "sidewalk" | "crossing" => bicycle_tag(a.bicycle.as_deref()).unwrap_or(false),
        _ => bike,
    };
    let cycle_lane = match use_ {
        Use::Cycleway if lua_bike => CycleLane::Separated,
        Use::Footway | Use::Path if lua_bike => CycleLane::Shared,
        _ => CycleLane::None,
    };

    EdgeInfo {
        surface: surface_of(a, road_class, use_),
        use_,
        cycle_lane,
        road_class,
        bike,
        foot,
        auto,
    }
}

/// The surface of a street.
///
/// Valhalla: the `surface` tag wins, then `tracktype`, then a default by road class and use.
/// Shortbread only says `paved` or `unpaved` for the tag, so:
/// - `paved` is `paved_smooth` (asphalt, concrete: by far the most common paved values;
///   sett and paving stones would be `paved`, the same PAVED class, and cobblestone
///   `paved_rough`, which cannot be told apart);
/// - `unpaved` cannot be resolved into compacted / gravel / dirt / grass. The `tracktype`
///   (which Valhalla ignores when there is a surface tag) is the best hint left of what it
///   is; without it, the kind decides (see [`unpaved_guess`]).
pub fn surface_of(a: &StreetAttrs, road_class: RoadClass, use_: Use) -> Surface {
    match a.surface.as_deref() {
        Some("paved") => return Surface::PavedSmooth,
        Some("unpaved") => return unpaved_guess(a),
        Some(other) => {
            if let Some(s) = surface_from_osm(other) {
                return s;
            }
            // Valhalla: an unknown surface value skips the tracktype and falls to defaults.
            return default_surface(road_class, use_);
        }
        None => {}
    }
    if let Some(s) = a.tracktype.as_deref().and_then(surface_from_tracktype) {
        return s;
    }
    default_surface(road_class, use_)
}

/// What an `unpaved` street most likely is in Valhalla's terms, the exact OSM value being
/// lost. Valhalla's own reading of a literal `surface=unpaved` is `gravel`, which stands for
/// the unpaved values by default. Two exceptions, from how `surface` is used on OSM (taginfo)
/// and checked against Stadia on the eval routes:
/// - a `path` or `bridleway` without a (hard) tracktype is mostly `ground` / `dirt`: `dirt`;
/// - a soft tracktype (grade4, grade5: earth, grass) is `dirt`, whatever the kind;
///
/// and an unpaved way designated for bicycles (a French "voie verte") is `compacted`.
pub fn unpaved_guess(a: &StreetAttrs) -> Surface {
    if a.bicycle.as_deref() == Some("designated")
        && !matches!(a.tracktype.as_deref(), Some("grade4") | Some("grade5"))
    {
        return Surface::Compacted;
    }
    match a.tracktype.as_deref() {
        Some("grade4") | Some("grade5") => Surface::Dirt,
        Some("grade1") | Some("grade2") | Some("grade3") => Surface::Gravel,
        _ => match a.kind.as_str() {
            "path" | "bridleway" => Surface::Dirt,
            _ => Surface::Gravel,
        },
    }
}

/// pbfgraphparser.cc: the surface of a way without a usable surface or tracktype tag.
pub fn default_surface(road_class: RoadClass, use_: Use) -> Surface {
    if road_class.is_road() {
        return Surface::PavedSmooth;
    }
    match use_ {
        Use::Footway | Use::Pedestrian | Use::Path | Use::Bridleway => Surface::Compacted,
        Use::Track => Surface::Dirt,
        Use::Road
        | Use::ParkingAisle
        | Use::Driveway
        | Use::Alley
        | Use::EmergencyAccess
        | Use::DriveThrough
        | Use::LivingStreet
        | Use::ServiceRoad => Surface::PavedSmooth,
        Use::Cycleway | Use::Steps => Surface::Paved,
        _ => Surface::Paved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn street(kind: &str, surface: Option<&str>, tracktype: Option<&str>) -> StreetAttrs {
        StreetAttrs {
            kind: kind.into(),
            surface: surface.map(Into::into),
            tracktype: tracktype.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn osm_surface_values_follow_valhalla() {
        assert_eq!(surface_from_osm("asphalt"), Some(Surface::PavedSmooth));
        assert_eq!(surface_from_osm("unpaved"), Some(Surface::Gravel));
        assert_eq!(surface_from_osm("paving_stones"), Some(Surface::Paved));
        assert_eq!(surface_from_osm("cobblestone"), Some(Surface::PavedRough));
        assert_eq!(surface_from_osm("compacted"), Some(Surface::Compacted));
        assert_eq!(surface_from_osm("fine_gravel"), Some(Surface::Gravel));
        assert_eq!(surface_from_osm("ground"), Some(Surface::Dirt));
        assert_eq!(surface_from_osm("grass"), Some(Surface::Path));
        assert_eq!(surface_from_osm("grass_paver"), Some(Surface::Paved));
        assert_eq!(surface_from_osm("stepping_stones"), Some(Surface::Path));
        assert_eq!(surface_from_osm("whatever"), None);
    }

    #[test]
    fn tracktype_then_defaults() {
        let r = AccessRules::France;
        assert_eq!(
            derive(&street("track", None, Some("grade1")), r).surface,
            Surface::PavedRough
        );
        assert_eq!(
            derive(&street("track", None, Some("grade2")), r).surface,
            Surface::Compacted
        );
        assert_eq!(
            derive(&street("track", None, Some("grade3")), r).surface,
            Surface::Dirt
        );
        assert_eq!(
            derive(&street("track", None, Some("grade5")), r).surface,
            Surface::Path
        );
        assert_eq!(
            derive(&street("track", Some("unpaved"), Some("grade5")), r).surface,
            Surface::Dirt
        );
        assert_eq!(
            derive(&street("track", Some("unpaved"), None), r).surface,
            Surface::Gravel
        );
        assert_eq!(
            derive(&street("path", Some("unpaved"), None), r).surface,
            Surface::Dirt
        );
        let mut green = street("path", Some("unpaved"), None);
        green.bicycle = Some("designated".into());
        assert_eq!(derive(&green, r).surface, Surface::Compacted);
        assert_eq!(
            derive(&street("track", None, None), r).surface,
            Surface::Dirt
        );
        assert_eq!(
            derive(&street("residential", None, None), r).surface,
            Surface::PavedSmooth
        );
        assert_eq!(
            derive(&street("service", None, None), r).surface,
            Surface::PavedSmooth
        );
        assert_eq!(
            derive(&street("footway", None, None), r).surface,
            Surface::Compacted
        );
        assert_eq!(
            derive(&street("path", None, None), r).surface,
            Surface::Compacted
        );
        assert_eq!(
            derive(&street("cycleway", None, None), r).surface,
            Surface::Paved
        );
        // the surface tag wins over the tracktype
        assert_eq!(
            derive(&street("track", Some("paved"), Some("grade3")), r).surface,
            Surface::PavedSmooth
        );
    }

    #[test]
    fn use_cycle_lane_and_access() {
        let r = AccessRules::France;
        let cw = derive(&street("cycleway", Some("paved"), None), r);
        assert_eq!(
            (cw.use_, cw.cycle_lane, cw.bike),
            (Use::Cycleway, CycleLane::Separated, true)
        );
        let path = derive(&street("path", None, None), r);
        assert_eq!((path.use_, path.cycle_lane), (Use::Path, CycleLane::Shared));
        let fw = derive(&street("footway", None, None), r);
        assert_eq!(
            (fw.use_, fw.cycle_lane, fw.bike),
            (Use::Footway, CycleLane::None, true)
        );
        assert!(!derive(&street("footway", None, None), AccessRules::World).bike);
        let mut designated = street("footway", None, None);
        designated.bicycle = Some("designated".into());
        assert_eq!(derive(&designated, r).cycle_lane, CycleLane::Shared);
        let mut no = street("path", None, None);
        no.bicycle = Some("no".into());
        assert!(!derive(&no, r).bike);
        assert!(!derive(&street("motorway", Some("paved"), None), r).bike);
        assert!(!derive(&street("trunk", Some("paved"), None), r).bike);
        let mut ramp = street("primary", Some("paved"), None);
        ramp.link = true;
        assert_eq!(derive(&ramp, r).use_, Use::Ramp);
        let mut drive = street("service", None, None);
        drive.service = Some("driveway".into());
        assert_eq!(derive(&drive, r).use_, Use::Driveway);
    }
}
