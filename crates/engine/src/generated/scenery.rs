//! Installed physical scenery mesh, read on planning workers without SDK calls.
//! Mesh clearance alone does not establish obstacle or controlled-airspace protection.

#![allow(clippy::default_trait_access)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub(crate) mod cache;
mod dsf;
mod library;
mod object;
use openatc_core::traffic::motion::Waypoint;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_BYTES: usize = 128 * 1024 * 1024;
/// Retained tile stacks kept between planning jobs.
const MAXIMUM_TILES: usize = 128;
const GRID: usize = 128;
/// Unresolved objects have no geometry. Keep the gate closed while their anchor is within this
/// distance of the corridor; beyond it, even a large object cannot reach the aircraft envelope.
const UNRESOLVED_OBJECT_REACH_METERS: f64 = 5_000.0;
/// Coarse cells across one tile for the obstacle placements.
const OBSTACLE_GRID: usize = 64;
/// The largest aircraft envelope the obstacle check accepts.
const MAXIMUM_ENVELOPE_METERS: f64 = 100.0;

type TileKey = (i32, i32);
type TileStack = Result<Vec<Arc<Tile>>, String>;

/// The enabled stack is retained for one planning job; no runtime scenery-folder scans.
pub(super) struct Terrain {
    packs: Vec<PathBuf>,
    cache: Option<cache::Cache>,
    dependencies: cache::Dependencies,
    touched: BTreeMap<TileKey, u64>,
    clock: u64,
    tiles: BTreeMap<TileKey, TileStack>,
    retained_bytes: usize,
    obstacle_seen: bool,
    library: library::Library,
    grid_checks: BTreeMap<[u64; 7], (Result<bool, String>, bool)>,
    obstacle_checks: BTreeMap<[u64; 7], (Result<bool, String>, bool)>,
}
impl Terrain {
    pub fn new(root: &Path) -> Result<Self, String> {
        cache::begin();
        let path = root.join("Custom Scenery/scenery_packs.ini");
        // The enabled pack list is part of the cache key, so the file's own timestamp is not a
        // dependency. Touching the ini (an installer rewrite, for example) must not throw away
        // every compiled tile and make the next preparation re-parse the whole scenery.

        let mut packs = Vec::new();
        if path.is_file() {
            if path.metadata().map_err(|e| e.to_string())?.len() > 1024 * 1024 {
                return Err("Enabled scenery list exceeds the planning budget".into());
            }
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            for line in text.lines() {
                let Some(name) = line.strip_prefix("SCENERY_PACK ").map(str::trim) else {
                    continue;
                };
                if packs.len() >= 1024 || name.len() > 8192 {
                    return Err("Enabled scenery count exceeds the planning budget".into());
                }
                let path = if name == "*GLOBAL_AIRPORTS*" {
                    root.join("Global Scenery/Global Airports")
                } else if Path::new(name).is_absolute() {
                    PathBuf::from(name)
                } else {
                    root.join(name)
                };
                if !packs.contains(&path) {
                    packs.push(path);
                }
            }
        }
        let base = root.join("Global Scenery/X-Plane 12 Global Scenery");
        if !packs.contains(&base) {
            packs.push(base);
        }
        let library = library::Library::load(root, &packs)?;
        let cache = cache::Cache::configured(root);
        Ok(Self {
            cache,
            dependencies: cache::snapshot(),
            touched: BTreeMap::new(),
            clock: 0,
            packs,
            tiles: BTreeMap::new(),
            retained_bytes: 0,
            obstacle_seen: false,
            library,
            grid_checks: BTreeMap::new(),
            obstacle_checks: BTreeMap::new(),
        })
    }

    pub fn supplement_airport(
        &self,
        airport: &mut openatc_core::airport::Airport,
    ) -> Result<usize, String> {
        let tiles = airport_tiles(airport)?;
        let mut added = 0;
        for (latitude, longitude) in tiles {
            added += self.supplement_airport_tile(airport, latitude, longitude)?;
        }
        Ok(added)
    }

    fn supplement_airport_tile(
        &self,
        airport: &mut openatc_core::airport::Airport,
        latitude: i32,
        longitude: i32,
    ) -> Result<usize, String> {
        let owner = Path::new(&airport.source)
            .parent()
            .and_then(Path::parent)
            .ok_or("Airport has no owning scenery package")?;
        let directory = format!(
            "{:+03}{:+04}",
            latitude.div_euclid(10) * 10,
            longitude.div_euclid(10) * 10
        );
        let filename = format!("{latitude:+03}{longitude:+04}.dsf");
        let mut exclusions: Vec<[f64; 4]> = Vec::new();
        let mut surfaces = BTreeMap::new();
        let mut added = 0;
        for pack in &self.packs {
            let path = pack.join("Earth nav data").join(&directory).join(&filename);
            if !path.is_file() {
                continue;
            }
            let tile = dsf::parse(&file_bytes(&path)?)?;
            if pack == owner {
                for polygon in &tile.polygons {
                    if polygon.airport.as_deref() != Some(airport.icao.as_str()) {
                        continue;
                    }
                    let curved = match (polygon.parameter == 65535, polygon.depth) {
                        (false, 2) | (true, 4) => false,
                        (false, 4) | (true, 8) => true,
                        _ => continue,
                    };
                    let bounds = polygon.rings.iter().flatten().fold(
                        [
                            f64::INFINITY,
                            f64::NEG_INFINITY,
                            f64::INFINITY,
                            f64::NEG_INFINITY,
                        ],
                        |mut bounds, point| {
                            bounds[0] = bounds[0].min(point[0]);
                            bounds[1] = bounds[1].max(point[0]);
                            bounds[2] = bounds[2].min(point[1]);
                            bounds[3] = bounds[3].max(point[1]);
                            if curved {
                                for control in [
                                    [point[2], point[3]],
                                    [2.0 * point[0] - point[2], 2.0 * point[1] - point[3]],
                                ] {
                                    bounds[0] = bounds[0].min(control[0]);
                                    bounds[1] = bounds[1].max(control[0]);
                                    bounds[2] = bounds[2].min(control[1]);
                                    bounds[3] = bounds[3].max(control[1]);
                                }
                            }
                            bounds
                        },
                    );
                    // Using the exclusion's bounding rectangle may reject extra pavement, never restore excluded pavement.
                    if exclusions.iter().any(|excluded| {
                        bounds[0] <= excluded[1]
                            && bounds[1] >= excluded[0]
                            && bounds[2] <= excluded[3]
                            && bounds[3] >= excluded[2]
                    }) {
                        continue;
                    }
                    let paved = *surfaces.entry(polygon.resource.clone()).or_insert_with(|| {
                        self.library
                            .paved_surface(pack, &polygon.resource)
                            .unwrap_or(false)
                    });
                    if !paved {
                        continue;
                    }
                    let rings = polygon
                        .rings
                        .iter()
                        .map(|ring| {
                            ring.iter()
                                .map(|point| {
                                    let node = openatc_core::airport::airport_point(
                                        airport, point[1], point[0],
                                    );
                                    let control = curved.then(|| {
                                        openatc_core::airport::airport_point(
                                            airport, point[3], point[2],
                                        )
                                    });
                                    (node, control)
                                })
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    if openatc_core::airport::append_pavement(airport, &rings).is_ok() {
                        added += 1;
                    }
                }
                break;
            }
            exclusions.extend(tile.polygon_exclusions);
        }
        Ok(added)
    }

    fn tile(&mut self, latitude: i32, longitude: i32) -> Result<&[Arc<Tile>], String> {
        // The retained set has to hold one preparation's working set, which is several dozen
        // tiles for a pair of airports. A smaller cap makes the engine evict a tile and read it
        // back for every corridor, which turns a preparation into minutes of repeated parsing.
        const BUDGET: usize = 1024 * 1024 * 1024;
        let key = (latitude, longitude);
        self.clock += 1;
        self.touched.insert(key, self.clock);
        if !self.tiles.contains_key(&key) {
            cache::restore(&self.dependencies);
            cache::preparing(key);
            let directory = format!(
                "{:+03}{:+04}",
                latitude.div_euclid(10) * 10,
                longitude.div_euclid(10) * 10
            );
            let filename = format!("{latitude:+03}{longitude:+04}.dsf");
            let result = if let Some(tiles) = self
                .cache
                .as_ref()
                .and_then(|cache| cache.load(&self.packs, key))
            {
                Ok(tiles)
            } else {
                let parsed = (|| -> Result<Vec<Arc<Tile>>, String> {
                    let mut tiles = Vec::new();
                    let mut base = false;
                    for pack in &self.packs {
                        let path = pack.join("Earth nav data").join(&directory).join(&filename);
                        cache::observe(&path);
                        if !path.is_file() {
                            continue;
                        }
                        let tile = Tile::load_with(&path, Some(&self.library))
                            .map_err(|error| format!("{}: {error}", path.display()))?;
                        let bounds = [
                            f64::from(longitude),
                            f64::from(longitude + 1),
                            f64::from(latitude),
                            f64::from(latitude + 1),
                        ];
                        if tile
                            .bounds
                            .iter()
                            .zip(bounds)
                            .any(|(actual, expected)| (actual - expected).abs() > f64::EPSILON)
                        {
                            return Err(
                                "Scenery file bounds do not match its requested tile".into()
                            );
                        }
                        base = !tile.overlay;
                        tiles.push(tile);
                        if base {
                            break;
                        }
                    }
                    if !base {
                        return Err(format!(
                            "No enabled physical base mesh for {latitude:+03}{longitude:+04}"
                        ));
                    }
                    // Resolve every layer's networks and strings against the base mesh. An overlay
                    // carries roads and object strings but no physical terrain of its own, so the
                    // base layer is the surface they are draped over.
                    let base_index = tiles
                        .iter()
                        .position(|tile| !tile.overlay)
                        .ok_or("Scenery stack has no physical base mesh")?;
                    let bounds = tiles[base_index].bounds;
                    let triangles = std::mem::take(&mut tiles[base_index].triangles);
                    let bins = std::mem::take(&mut tiles[base_index].bins);
                    for tile in &mut tiles {
                        tile.resolve_networks(bounds, &triangles, &bins);
                        tile.resolve_strings(bounds, &triangles, &bins);
                        tile.index_obstacles();
                    }
                    tiles[base_index].triangles = triangles;
                    tiles[base_index].bins = bins;
                    Ok(tiles.into_iter().map(Arc::new).collect())
                })();
                if let (Some(cache), Ok(tiles)) = (&mut self.cache, &parsed) {
                    cache.store(&self.packs, key, tiles);
                }
                parsed
            };
            self.dependencies = cache::snapshot();
            let bytes = result
                .as_ref()
                .map_or(0, |tiles| tiles.iter().map(|tile| tile.bytes()).sum());
            // Evict only unborrowed stacks. Persistent copies make revisiting a tile cheap;
            // the per-job retained-memory cap and all physical clearance checks remain intact.
            while self.tiles.len() >= MAXIMUM_TILES
                || self.retained_bytes.saturating_add(bytes) > BUDGET
            {
                let oldest = self
                    .tiles
                    .iter()
                    .filter(|(_, stack)| {
                        stack.as_ref().map_or(true, |tiles| {
                            tiles.iter().all(|tile| Arc::strong_count(tile) == 1)
                        })
                    })
                    .min_by_key(|(key, _)| self.touched.get(key).copied().unwrap_or(0))
                    .map(|(key, _)| *key);
                let Some(oldest) = oldest else {
                    break;
                };
                if let Some(Ok(tiles)) = self.tiles.remove(&oldest) {
                    self.retained_bytes = self
                        .retained_bytes
                        .saturating_sub(tiles.iter().map(|tile| tile.bytes()).sum());
                }
                self.touched.remove(&oldest);
            }
            if self.tiles.len() >= MAXIMUM_TILES
                || bytes > BUDGET
                || self.retained_bytes.saturating_add(bytes) > BUDGET
            {
                self.touched.remove(&key);
                return Err(
                    "Terminal scenery retention budget exhausted by active tile stacks".into(),
                );
            }
            self.retained_bytes += bytes;
            cache::retained(self.retained_bytes);
            self.tiles.insert(key, result);
        }
        self.tiles[&key]
            .as_ref()
            .map(Vec::as_slice)
            .map_err(Clone::clone)
    }
    /// Terrain-only result; callers must separately resolve obstacles and controller authority.
    pub fn terrain_clear(
        &mut self,
        from: Waypoint,
        to: Waypoint,
        margin: f64,
    ) -> Result<bool, String> {
        let key = corridor_key(from, to, margin);
        if let Some((result, unresolved)) = self.grid_checks.get(&key) {
            self.obstacle_seen |= *unresolved;
            return result.clone();
        }
        // Track this call's own unresolved discovery so the cached entry can reproduce it when
        // another aircraft checks the same geometry.
        let previous = self.obstacle_seen;
        self.obstacle_seen = false;
        let result = self.terrain_clear_inner(from, to, margin);
        let unresolved = self.obstacle_seen;
        self.obstacle_seen = previous | unresolved;
        self.grid_checks.insert(key, (result.clone(), unresolved));
        result
    }

    fn terrain_clear_inner(
        &mut self,
        from: Waypoint,
        to: Waypoint,
        margin: f64,
    ) -> Result<bool, String> {
        if [from, to].iter().any(|p| {
            !p.latitude.is_finite()
                || !p.longitude.is_finite()
                || p.latitude.abs() >= 85.0
                || p.longitude.abs() >= 180.0
        }) || (from.latitude - to.latitude).abs() > 1.0
            || (from.longitude - to.longitude).abs() > 1.0
        {
            return Err("Terrain corridor exceeds its coordinate or edge budget".into());
        }
        let mut cuts = vec![0.0, 1.0];
        for (a, b) in [(from.latitude, to.latitude), (from.longitude, to.longitude)] {
            if (a - b).abs() < 1e-12 {
                continue;
            }
            for edge in a.min(b).ceil() as i32..=a.max(b).floor() as i32 {
                let fraction = (f64::from(edge) - a) / (b - a);
                if fraction > 0.0 && fraction < 1.0 {
                    cuts.push(fraction);
                }
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let interpolate = |t: f64| Waypoint {
            latitude: from.latitude + (to.latitude - from.latitude) * t,
            longitude: from.longitude + (to.longitude - from.longitude) * t,
            altitude_feet: from.altitude_feet + (to.altitude_feet - from.altitude_feet) * t,
        };
        for pair in cuts.windows(2) {
            let middle = interpolate(f64::midpoint(pair[0], pair[1]));
            let stack = self
                .tile(
                    middle.latitude.floor() as i32,
                    middle.longitude.floor() as i32,
                )?
                .to_vec();
            let a = interpolate(pair[0]);
            let b = interpolate(pair[1]);
            self.obstacle_seen |= stack
                .iter()
                .any(|tile| tile.unresolved_object_near(a, b, margin));
            let mut covered = false;
            for tile in &stack {
                match tile.terrain_clear(a, b, margin) {
                    Some(false) => return Ok(false),
                    Some(true) if !tile.overlay => covered = true,
                    _ => {}
                }
            }
            if !covered {
                return Err("Physical scenery mesh leaves a gap in the terrain corridor".into());
            }
        }
        Ok(true)
    }

    pub fn has_unresolved_obstacles(&self) -> bool {
        self.obstacle_seen
    }

    /// Classes seen in any loaded tile, for refusal diagnostics.
    pub fn unresolved_classes(&self) -> BTreeSet<String> {
        let mut classes = BTreeSet::new();
        for tile in self
            .tiles
            .values()
            .filter_map(|stack| stack.as_ref().ok())
            .flatten()
        {
            classes.extend(tile.unresolved_classes.iter().cloned());
        }
        classes
    }

    pub fn obstacles_clear(
        &mut self,
        from: Waypoint,
        to: Waypoint,
        radius: f64,
    ) -> Result<bool, String> {
        let key = corridor_key(from, to, radius);
        if let Some((result, unresolved)) = self.obstacle_checks.get(&key) {
            self.obstacle_seen |= *unresolved;
            return result.clone();
        }
        let previous = self.obstacle_seen;
        self.obstacle_seen = false;
        let result = self.obstacles_clear_inner(from, to, radius);
        let unresolved = self.obstacle_seen;
        self.obstacle_seen = previous | unresolved;
        self.obstacle_checks
            .insert(key, (result.clone(), unresolved));
        result
    }

    fn obstacles_clear_inner(
        &mut self,
        from: Waypoint,
        to: Waypoint,
        radius: f64,
    ) -> Result<bool, String> {
        if !(0.0..=100.0).contains(&radius)
            || [from, to].iter().any(|point| {
                !point.latitude.is_finite()
                    || !point.longitude.is_finite()
                    || !point.altitude_feet.is_finite()
                    || point.latitude.abs() >= 85.0
                    || point.longitude.abs() >= 180.0
            })
            || (from.latitude - to.latitude).abs() > 1.0
            || (from.longitude - to.longitude).abs() > 1.0
        {
            return Err("Invalid aircraft obstacle envelope".into());
        }
        // An object is stored with the tile that places it, so an object anchored in the next
        // tile over that reaches into the corridor would otherwise be missed. Expand the stack by
        // one tile each way, bounded by the existing tile budget. An absent neighbour holds no
        // obstacles and is skipped, but a missing corridor tile is still an error.
        let low_latitude = from.latitude.min(to.latitude).floor() as i32;
        let high_latitude = from.latitude.max(to.latitude).floor() as i32;
        let low_longitude = from.longitude.min(to.longitude).floor() as i32;
        let high_longitude = from.longitude.max(to.longitude).floor() as i32;
        let min_latitude = from.latitude.min(to.latitude) - radius / 111_320.0;
        let max_latitude = from.latitude.max(to.latitude) + radius / 111_320.0;
        let min_longitude = from.longitude.min(to.longitude) - radius / 111_320.0;
        let max_longitude = from.longitude.max(to.longitude) + radius / 111_320.0;
        // The rectangle above is the bounding box of the whole segment, so a long diagonal can
        // retain many obstacles that lie well clear of the line. Measure the horizontal distance
        // from the placement to the segment before touching its geometry; a placement farther
        // away than the obstacle's own horizontal extent plus the aircraft envelope cannot
        // intersect the corridor at any height.
        let segment_latitude = from.latitude.to_radians().cos().max(0.01);
        let project = |latitude: f64, longitude: f64| {
            [
                longitude * 111_320.0 * segment_latitude,
                latitude * 111_320.0,
            ]
        };
        let start = project(from.latitude, from.longitude);
        let end = project(to.latitude, to.longitude);
        let coordinates = (low_latitude - 1..=high_latitude + 1).flat_map(|latitude| {
            (low_longitude - 1..=high_longitude + 1).map(move |longitude| (latitude, longitude))
        });
        for (latitude, longitude) in coordinates {
            let corridor = (low_latitude..=high_latitude).contains(&latitude)
                && (low_longitude..=high_longitude).contains(&longitude);
            let stack = match self.tile(latitude, longitude) {
                Ok(stack) => stack.to_vec(),
                Err(error) if !corridor && error.starts_with("No enabled physical base mesh") => {
                    continue;
                }
                Err(error) => return Err(error),
            };
            for tile in &stack {
                self.obstacle_seen |= tile.unresolved_object_near(from, to, radius);
                // The cell index keeps the walk to the obstacles the corridor can reach. A tile
                // holds tens of thousands of obstacles, and the corridor only passes a few cells
                // of them.
                let mut candidates = Vec::new();
                for cell in tile.obstacle_cells(from, to) {
                    candidates.extend_from_slice(&tile.obstacle_bins[cell]);
                }
                candidates.sort_unstable();
                candidates.dedup();
                for index in candidates {
                    let Some(obstacle) = tile.obstacles.get(index as usize) else {
                        continue;
                    };
                    let placement = &obstacle.placement;
                    // Skip obstacles that cannot reach the corridor. This keeps a dense tile
                    // of small objects from being walked for every route segment.
                    let reach = (obstacle.radius + radius + 2.0) / 111_320.0;
                    let longitude_reach =
                        reach / placement.coordinate[0].to_radians().cos().max(0.01);
                    if placement.coordinate[0] + reach < min_latitude
                        || placement.coordinate[0] - reach > max_latitude
                        || placement.coordinate[1] + longitude_reach < min_longitude
                        || placement.coordinate[1] - longitude_reach > max_longitude
                    {
                        continue;
                    }
                    let nearest = point_segment_distance(
                        project(placement.coordinate[0], placement.coordinate[1]),
                        start,
                        end,
                    );
                    if nearest > obstacle.radius + radius + 2.0 {
                        continue;
                    }
                    let ground = stack
                        .iter()
                        .filter_map(|layer| layer.mesh_height(placement.coordinate))
                        .reduce(f64::max);
                    let Some(ground) = ground else {
                        self.obstacle_seen = true;
                        continue;
                    };
                    // Four-plane placements may be absolute or use the DSF AGL placement comment.
                    let anchor = if obstacle.agl {
                        ground
                    } else {
                        placement
                            .height
                            .map_or(ground, |height| height.max(ground + height))
                    };
                    let local_top = obstacle.local_ground.and_then(|(height, reach)| {
                        let offset = [from, to]
                            .into_iter()
                            .map(|point| {
                                openatc_core::arrival::distance_nm(
                                    [point.latitude, point.longitude],
                                    placement.coordinate,
                                ) * 1852.0
                            })
                            .fold(0.0_f64, f64::max);
                        let padding = radius + reach + 5.0 + offset * 0.02;
                        let latitude = padding / 111_320.0;
                        let longitude = latitude / from.latitude.to_radians().cos().max(0.01);
                        let area = [
                            from.longitude.min(to.longitude) - longitude,
                            from.longitude.max(to.longitude) + longitude,
                            from.latitude.min(to.latitude) - latitude,
                            from.latitude.max(to.latitude) + latitude,
                        ];
                        // A cross-tile ground reference keeps the precomputed conservative bound.
                        stack
                            .iter()
                            .filter(|layer| {
                                area[0] >= layer.bounds[0]
                                    && area[1] <= layer.bounds[1]
                                    && area[2] >= layer.bounds[2]
                                    && area[3] <= layer.bounds[3]
                            })
                            .filter_map(|layer| {
                                mesh_maximum(layer.bounds, &layer.triangles, &layer.bins, area)
                            })
                            .reduce(f64::max)
                            .map(|ground| ground + height)
                    });
                    if !obstacle.clear_at_top(from, to, anchor, radius, local_top) {
                        if std::env::var_os("OPENATC_TRAFFIC_PROFILE").is_some() {
                            eprintln!(
                                "Traffic obstacle refusal: {}, placement {:?}, height {:?}, AGL {}, ground {:.1} m, anchor {:.1} m, radius {:.1} m, footprint {}, local top {:?}, geometry {:?}",
                                placement.resource,
                                placement.coordinate,
                                placement.height,
                                obstacle.agl,
                                ground,
                                anchor,
                                obstacle.radius,
                                obstacle.footprint.is_some(),
                                local_top,
                                obstacle.geometry.bounds().first()
                            );
                        }
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }
    pub fn begin_check(&mut self) {
        self.obstacle_seen = false;
    }
}

fn airport_tiles(airport: &openatc_core::airport::Airport) -> Result<Vec<(i32, i32)>, String> {
    let mut bounds = [
        airport.reference_latitude,
        airport.reference_latitude,
        airport.reference_longitude,
        airport.reference_longitude,
    ];
    for point in airport
        .nodes
        .iter()
        .map(|node| node.point)
        .chain(airport.parking.iter().map(|stand| stand.point))
        .chain(
            airport
                .runways
                .iter()
                .flat_map(|runway| [runway.first, runway.second]),
        )
    {
        let coordinate = super::position(airport, point);
        if coordinate.iter().any(|value| !value.is_finite()) {
            return Err("Airport surface tile extent is invalid".into());
        }
        bounds[0] = bounds[0].min(coordinate[0]);
        bounds[1] = bounds[1].max(coordinate[0]);
        bounds[2] = bounds[2].min(coordinate[1]);
        bounds[3] = bounds[3].max(coordinate[1]);
    }
    // Include room for the aircraft and checked connector curves at the outermost graph nodes.
    let latitude_margin = 500.0 / 111_320.0;
    let longitude_margin = latitude_margin / airport.reference_latitude.to_radians().cos();
    bounds[0] -= latitude_margin;
    bounds[1] += latitude_margin;
    bounds[2] -= longitude_margin;
    bounds[3] += longitude_margin;
    if bounds.iter().any(|value| !value.is_finite())
        || bounds[0] < -85.0
        || bounds[1] >= 85.0
        || bounds[2] < -180.0
        || bounds[3] >= 180.0
        || bounds[1] - bounds[0] > 1.0
        || bounds[3] - bounds[2] > 1.0
    {
        return Err("Airport surface exceeds its supported tile extent".into());
    }
    let mut tiles = Vec::new();
    for latitude in bounds[0].floor() as i32..=bounds[1].floor() as i32 {
        for longitude in bounds[2].floor() as i32..=bounds[3].floor() as i32 {
            tiles.push((latitude, longitude));
        }
    }
    if tiles.len() > 4 {
        return Err("Airport surface tile count exceeds its budget".into());
    }
    Ok(tiles)
}

fn file_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = path.metadata().map_err(|e| e.to_string())?;
    if metadata.len() > MAX_BYTES as u64 {
        return Err("Scenery tile exceeds the input budget".into());
    }
    let file = cache::open_file(path).map_err(|e| e.to_string())?;
    let mut prefix = [0; 8];
    (&file).read_exact(&mut prefix).map_err(|e| e.to_string())?;
    if &prefix == b"XPLNEDSF" {
        let mut output = prefix.to_vec();
        file.take(MAX_BYTES as u64 + 1 - prefix.len() as u64)
            .read_to_end(&mut output)
            .map_err(|e| e.to_string())?;
        if output.len() > MAX_BYTES {
            return Err("Scenery tile exceeds the input budget".into());
        }
        return Ok(output);
    }
    if prefix[..6] != [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c] {
        return Err("Scenery tile is neither DSF nor a supported 7z archive".into());
    }
    let mut archive =
        sevenz_rust2::ArchiveReader::open(path, Default::default()).map_err(|e| e.to_string())?;
    archive.set_thread_count(1);
    let entries = &archive.archive().files;
    if entries.len() != 1
        || entries[0].is_directory
        || entries[0].size > MAX_BYTES as u64
        || !entries[0].name.to_ascii_lowercase().ends_with(".dsf")
        || archive.archive().blocks.iter().any(|block| {
            block.get_unpack_size() > MAX_BYTES as u64
                || block
                    .coders
                    .iter()
                    .any(|coder| match coder.encoder_method_id() {
                        [0] => false,
                        [3, 1, 1] => {
                            coder.properties().len() != 5
                                || u32::from_le_bytes(
                                    coder
                                        .properties()
                                        .get(1..5)
                                        .unwrap_or(&[255; 4])
                                        .try_into()
                                        .unwrap(),
                                ) > 64 * 1024 * 1024
                        }
                        [0x21] => coder.properties().len() != 1 || coder.properties()[0] > 26,
                        _ => true,
                    })
        })
    {
        return Err("Scenery archive exceeds its file, codec or dictionary budget".into());
    }
    let mut output = Vec::new();
    archive
        .for_each_entries(|_, reader| {
            reader
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut output)
                .map_err(|error| sevenz_rust2::Error::Io(error, "scenery tile".into()))?;
            Ok(true)
        })
        .map_err(|e| e.to_string())?;
    if output.len() > MAX_BYTES {
        return Err("Expanded scenery tile exceeds the input budget".into());
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) struct Vertex {
    pub longitude: f64,
    pub latitude: f64,
    pub meters: f64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Placement {
    resource: Arc<str>,
    coordinate: [f64; 2],
    heading: f64,
    height: Option<f64>,
}
/// A polygon footprint in the obstacle's local frame: x east, z south, in metres from the
/// placement coordinate. Outer windings are solid and alternate with clear holes.
#[derive(Serialize, Deserialize)]
struct Footprint {
    /// Object overhang beyond the placement polygon.
    reach: f64,
    /// Every winding, in the published order. Nesting depth decides whether a winding is ground
    /// or a hole, so the ordinary even-odd rule applies to a point.
    rings: Vec<Vec<[f64; 2]>>,
    /// Local horizontal extent, used to reject a corridor before walking the windings.
    bounds: [f64; 4],
}
impl Footprint {
    /// AGS contours are directed polylines. Tile-spawning contours come first, so their
    /// storage order does not identify complete outer boundaries or holes.
    fn autogen(contours: &[Vec<[f64; 2]>], origin: [f64; 2], scale: f64) -> Option<Self> {
        let key = |point: [f64; 2]| point.map(f64::to_bits);
        let mut starts = BTreeMap::new();
        for (index, contour) in contours.iter().enumerate() {
            if contour.len() < 2
                || contour.iter().flatten().any(|value| !value.is_finite())
                || starts.insert(key(contour[0]), index).is_some()
            {
                return None;
            }
        }
        let mut visited = BTreeSet::new();
        let mut rings = Vec::new();
        for first in 0..contours.len() {
            if visited.contains(&first) {
                continue;
            }
            let mut ring = contours[first].clone();
            visited.insert(first);
            while key(*ring.last()?) != key(ring[0]) {
                let next = *starts.get(&key(*ring.last()?))?;
                if !visited.insert(next) {
                    return None;
                }
                ring.extend_from_slice(&contours[next][1..]);
            }
            rings.push(ring);
        }
        let area = |ring: &Vec<[f64; 2]>| {
            ring.windows(2)
                .map(|pair| {
                    (pair[0][1] - origin[1]) * (pair[1][0] - origin[0])
                        - (pair[1][1] - origin[1]) * (pair[0][0] - origin[0])
                })
                .sum::<f64>()
                .abs()
        };
        rings.sort_by(|a, b| area(b).total_cmp(&area(a)));
        Self::new(&rings, origin, scale).ok().flatten()
    }

    /// Convert and validate a published forest footprint.
    ///
    /// The first non-empty winding is the outer boundary; later windings nest inside it and
    /// alternate between holes and solid islands. Winding orientation is checked against that
    /// nesting because a misclassified hole could clear a corridor that actually crosses trees.
    /// A winding with no interior (fewer than three points or all points on one line) encloses
    /// nothing, so the placement keeps its bounding box rather than being cleared.
    fn new(rings: &[Vec<[f64; 2]>], origin: [f64; 2], scale: f64) -> Result<Option<Self>, String> {
        if rings.is_empty() {
            return Err("Forest footprint has no rings".into());
        }
        let mut valid: Vec<(Vec<[f64; 2]>, bool)> = Vec::with_capacity(rings.len());
        let mut outer_empty = false;
        for (position, ring) in rings.iter().enumerate() {
            if !ring
                .iter()
                .all(|point| point.iter().all(|value| value.is_finite()))
            {
                return Err("Forest footprint has a non-finite coordinate".into());
            }
            let local = ring
                .iter()
                .map(|point| {
                    [
                        (point[1] - origin[1]) * scale,
                        -(point[0] - origin[0]) * 111_320.0,
                    ]
                })
                .collect::<Vec<_>>();
            if ring_is_a_line(&local) {
                outer_empty |= position == 0;
                continue;
            }
            let mut twice_area = 0.0;
            for index in 0..ring.len() {
                let a = ring[index];
                let b = ring[(index + 1) % ring.len()];
                // Longitude is east and latitude is north.
                twice_area += a[1] * b[0] - b[1] * a[0];
            }
            if !twice_area.is_finite() || twice_area.abs() < 1e-15 {
                return Err("Forest footprint ring is not a simple area".into());
            }
            valid.push((local, twice_area > 0.0));
        }
        if valid.is_empty() {
            return Ok(None);
        }
        if outer_empty {
            return Err("Forest footprint outer ring has no area".into());
        }
        // The published outer orientation is not always counter-clockwise in shipped data, so
        // each later winding is compared against the outer winding rather than an absolute sign.
        let outer = valid[0].1;
        let mut depth = vec![0usize; valid.len()];
        for index in 1..valid.len() {
            let interior = &valid[index].0;
            let representative = interior[0];
            let mut parent = None;
            for candidate in 0..index {
                if point_in_ring(representative, &valid[candidate].0)
                    && bounding_box_contains(&valid[candidate].0, interior)
                    && parent.is_none_or(|best| depth[best] < depth[candidate])
                {
                    parent = Some(candidate);
                }
            }
            let Some(parent) = parent else {
                return Err("Forest footprint winding is not contained by an earlier ring".into());
            };
            depth[index] = depth[parent] + 1;
            let solid = depth[index].is_multiple_of(2);
            if valid[index].1 != (solid == outer) {
                return Err(
                    "Forest footprint winding has the wrong orientation for its nesting".into(),
                );
            }
        }
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for (points, _) in &valid {
            for point in points {
                bounds[0] = bounds[0].min(point[0]);
                bounds[1] = bounds[1].min(point[1]);
                bounds[2] = bounds[2].max(point[0]);
                bounds[3] = bounds[3].max(point[1]);
            }
        }
        Ok(Some(Self {
            reach: 0.0,
            rings: valid.into_iter().map(|(points, _)| points).collect(),
            bounds,
        }))
    }

    /// Whether a local horizontal point lies over solid ground. Windings alternate between
    /// solid ground and holes, so a point inside an odd number of them is solid.
    fn solid(&self, point: [f64; 2]) -> bool {
        self.rings
            .iter()
            .filter(|ring| point_in_ring(point, ring))
            .count()
            % 2
            == 1
    }

    /// Parameter interval of a local segment that passes over the footprint, including the
    /// horizontal envelope carried by `padding`. An interval is preferred over a single answer
    /// so the caller can compare height only where the obstacle actually reaches the corridor.
    fn blocked(&self, from: [f64; 2], to: [f64; 2], padding: f64) -> Option<(f64, f64)> {
        if segment_box_distance(from, to, self.bounds) > padding {
            return None;
        }
        let delta = [to[0] - from[0], to[1] - from[1]];
        let mut cuts = vec![0.0_f64, 1.0];
        let mut near = false;
        for ring in &self.rings {
            for index in 0..ring.len() {
                let a = ring[index];
                let b = ring[(index + 1) % ring.len()];
                if let Some(t) = segment_crossing(from, to, a, b) {
                    cuts.push(t);
                    near = true;
                }
                near |= point_segment_distance(from, a, b) <= padding
                    || point_segment_distance(to, a, b) <= padding
                    || point_segment_distance(a, from, to) <= padding
                    || point_segment_distance(b, from, to) <= padding;
            }
        }
        if cuts.len() > 4096 {
            // A pathological winding that crosses the corridor thousands of times is treated as
            // blocking the whole segment rather than spending unbounded time splitting it.
            return Some((0.0, 1.0));
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        for pair in cuts.windows(2) {
            let t = f64::midpoint(pair[0], pair[1]);
            let point = [from[0] + delta[0] * t, from[1] + delta[1] * t];
            if self.solid(point) {
                low = low.min(pair[0]);
                high = high.max(pair[1]);
            }
        }
        if near {
            // The envelope reaches some winding even where the centre line stays clear. Treating
            // the whole segment as overlapped is conservative and keeps the cost linear.
            return Some((0.0, 1.0));
        }
        (low <= high).then_some((low, high))
    }
}

fn point_in_ring(point: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut inside = false;
    let mut previous = ring.len() - 1;
    for index in 0..ring.len() {
        let a = ring[index];
        let b = ring[previous];
        if (a[1] > point[1]) != (b[1] > point[1]) {
            let crossing = (b[0] - a[0]) * (point[1] - a[1]) / (b[1] - a[1]) + a[0];
            if point[0] < crossing {
                inside = !inside;
            }
        }
        previous = index;
    }
    inside
}

fn bounding_box_contains(outer: &[[f64; 2]], inner: &[[f64; 2]]) -> bool {
    let bounds = |ring: &[[f64; 2]]| {
        ring.iter().fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |mut bounds, point| {
                bounds[0] = bounds[0].min(point[0]);
                bounds[1] = bounds[1].min(point[1]);
                bounds[2] = bounds[2].max(point[0]);
                bounds[3] = bounds[3].max(point[1]);
                bounds
            },
        )
    };
    let outer = bounds(outer);
    let inner = bounds(inner);
    inner[0] >= outer[0] && inner[1] >= outer[1] && inner[2] <= outer[2] && inner[3] <= outer[3]
}

fn ring_is_a_line(ring: &[[f64; 2]]) -> bool {
    if ring.len() < 3 {
        return true;
    }
    let origin = ring[0];
    let Some(direction) = ring
        .iter()
        .skip(1)
        .map(|point| [point[0] - origin[0], point[1] - origin[1]])
        .find(|offset| offset[0] * offset[0] + offset[1] * offset[1] > 1e-12)
    else {
        return true;
    };
    let length = direction[0].hypot(direction[1]);
    ring.iter().all(|point| {
        let offset = [point[0] - origin[0], point[1] - origin[1]];
        (direction[0] * offset[1] - direction[1] * offset[0]).abs() / length <= 1e-3
    })
}

fn point_box_distance(point: [f64; 2], bounds: [f64; 4]) -> f64 {
    let dx = (bounds[0] - point[0]).max(0.0).max(point[0] - bounds[2]);
    let dz = (bounds[1] - point[1]).max(0.0).max(point[1] - bounds[3]);
    dx.hypot(dz)
}

fn segment_box_distance(from: [f64; 2], to: [f64; 2], bounds: [f64; 4]) -> f64 {
    let corners = [
        [bounds[0], bounds[1]],
        [bounds[2], bounds[1]],
        [bounds[2], bounds[3]],
        [bounds[0], bounds[3]],
    ];
    for index in 0..4 {
        if segment_crossing(from, to, corners[index], corners[(index + 1) % 4]).is_some() {
            return 0.0;
        }
    }
    if point_box_distance(from, bounds) == 0.0 || point_box_distance(to, bounds) == 0.0 {
        return 0.0;
    }
    corners
        .iter()
        .map(|corner| point_segment_distance(*corner, from, to))
        .chain([
            point_box_distance(from, bounds),
            point_box_distance(to, bounds),
        ])
        .fold(f64::INFINITY, f64::min)
}

#[allow(clippy::many_single_char_names)]
fn segment_crossing(from: [f64; 2], to: [f64; 2], a: [f64; 2], b: [f64; 2]) -> Option<f64> {
    let r = [to[0] - from[0], to[1] - from[1]];
    let s = [b[0] - a[0], b[1] - a[1]];
    let denominator = r[0] * s[1] - r[1] * s[0];
    if denominator.abs() < 1e-12 {
        return None;
    }
    let offset = [a[0] - from[0], a[1] - from[1]];
    let t = (offset[0] * s[1] - offset[1] * s[0]) / denominator;
    let u = (offset[0] * r[1] - offset[1] * r[0]) / denominator;
    (0.0..=1.0)
        .contains(&t)
        .then_some(t)
        .filter(|_| (0.0..=1.0).contains(&u))
}

/// Exact corridor geometry key. Scenery is static while one preparation job runs, so a check
/// already made for these waypoints returns the same result for every aircraft.
fn corridor_key(from: Waypoint, to: Waypoint, parameter: f64) -> [u64; 7] {
    [
        from.latitude.to_bits(),
        from.longitude.to_bits(),
        from.altitude_feet.to_bits(),
        to.latitude.to_bits(),
        to.longitude.to_bits(),
        to.altitude_feet.to_bits(),
        parameter.to_bits(),
    ]
}

fn point_segment_distance(point: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let edge = [b[0] - a[0], b[1] - a[1]];
    let length = edge[0] * edge[0] + edge[1] * edge[1];
    let t = if length > 0.0 {
        (((point[0] - a[0]) * edge[0] + (point[1] - a[1]) * edge[1]) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let nearest = [a[0] + edge[0] * t, a[1] + edge[1] * t];
    (point[0] - nearest[0]).hypot(point[1] - nearest[1])
}

/// Round lower bounds down and upper bounds up when converting to single precision.
pub(super) fn narrow(low: bool, value: f64) -> f32 {
    let narrowed = value as f32;
    if low && f64::from(narrowed) > value {
        narrowed.next_down()
    } else if !low && f64::from(narrowed) < value {
        narrowed.next_up()
    } else {
        narrowed
    }
}

/// Store single-box obstacles inline; share the geometry of repeated object models.
#[derive(Clone, Debug, Serialize, Deserialize)]
enum Geometry {
    One(object::Bounds),
    Many(Arc<Vec<object::Bounds>>),
}
impl Geometry {
    fn bounds(&self) -> &[object::Bounds] {
        match self {
            Self::One(bound) => std::slice::from_ref(bound),
            Self::Many(bounds) => bounds,
        }
    }
    fn bytes(&self) -> usize {
        match self {
            Self::One(_) => std::mem::size_of::<object::Bounds>(),
            Self::Many(bounds) => bounds.capacity() * std::mem::size_of::<object::Bounds>(),
        }
    }
    fn key(&self) -> usize {
        match self {
            Self::One(bound) => std::ptr::from_ref(bound) as usize,
            Self::Many(bounds) => Arc::as_ptr(bounds) as usize,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Obstacle {
    /// Object height and maximum distance to its independent tile ground reference.
    local_ground: Option<(f64, f64)>,
    placement: Placement,
    geometry: Geometry,
    /// Forests are checked against their published polygon instead of its bounding box.
    footprint: Option<Arc<Footprint>>,
    /// The published height is above ground rather than an absolute elevation.
    agl: bool,
    /// Greatest horizontal distance from the placement coordinate to any checked box corner.
    radius: f64,
}
impl Obstacle {
    #[cfg(test)]
    fn clear(&self, from: Waypoint, to: Waypoint, anchor: f64, radius: f64) -> bool {
        self.clear_at_top(from, to, anchor, radius, None)
    }
    fn clear_at_top(
        &self,
        from: Waypoint,
        to: Waypoint,
        anchor: f64,
        radius: f64,
        local_top: Option<f64>,
    ) -> bool {
        let (sin, cos) = self.placement.heading.to_radians().sin_cos();
        let local = |point: Waypoint| {
            let east = (point.longitude - self.placement.coordinate[1])
                * 111_320.0
                * self.placement.coordinate[0].to_radians().cos();
            let north = (point.latitude - self.placement.coordinate[0]) * 111_320.0;
            [
                east * cos - north * sin,
                point.altitude_feet * 0.3048,
                -east * sin - north * cos,
            ]
        };
        let a = local(from);
        let b = local(to);
        let padding =
            radius + 1.0 + 0.02 * a[0].abs().max(a[2].abs()).max(b[0].abs()).max(b[2].abs());
        if let Some(footprint) = &self.footprint {
            let Some(bounds) = self.geometry.bounds().first() else {
                return true;
            };
            let Some((low, high)) =
                footprint.blocked([a[0], a[2]], [b[0], b[2]], padding + footprint.reach)
            else {
                return true;
            };
            let first = a[1] + (b[1] - a[1]) * low;
            let last = a[1] + (b[1] - a[1]) * high;
            return first.min(last) > local_top.unwrap_or(anchor + f64::from(bounds.high[1]));
        }
        self.geometry.bounds().iter().all(|bounds| {
            let mut low: f64 = 0.0;
            let mut high: f64 = 1.0;
            for axis in [0, 2] {
                let delta = b[axis] - a[axis];
                let low_edge = f64::from(bounds.low[axis]);
                let high_edge = f64::from(bounds.high[axis]);
                if delta.abs() < 1e-12 {
                    if a[axis] < low_edge - padding || a[axis] > high_edge + padding {
                        return true;
                    }
                } else {
                    let first = (low_edge - padding - a[axis]) / delta;
                    let last = (high_edge + padding - a[axis]) / delta;
                    low = low.max(first.min(last));
                    high = high.min(first.max(last));
                }
            }
            low > high
                || (a[1] + (b[1] - a[1]) * low).min(a[1] + (b[1] - a[1]) * high)
                    > local_top.unwrap_or(anchor + f64::from(bounds.high[1]))
        })
    }
}

#[derive(Serialize, Deserialize)]
struct DrapedPolygon {
    airport: Option<Arc<str>>,
    resource: Arc<str>,
    parameter: u16,
    depth: usize,
    rings: Vec<Vec<[f64; 4]>>,
}

/// Turn a published facade into an obstacle box.
///
/// The DSF polygon parameter is the building height in metres and the rings are its geographic
/// footprint, so no geometry has to be invented. A concave building is covered by its bounding
/// box, which errs toward reporting an obstruction rather than clearing one.
#[allow(dead_code)]
fn facade_obstacle(facade: dsf::Facade) -> Result<Obstacle, String> {
    facade_obstacle_in(facade, None)
}

/// Turn a published facade into an obstacle box, optionally anchoring it inside the tile that
/// stores it. A facade footprint can cross a tile boundary, and the anchor has to sit on the
/// mesh of the owning tile so its ground can be resolved.
fn facade_obstacle_in(facade: dsf::Facade, bounds: Option<[f64; 4]>) -> Result<Obstacle, String> {
    if !facade.height_meters.is_finite() || !(0.0..=1000.0).contains(&facade.height_meters) {
        return Err("Facade height is outside its published range".into());
    }
    let mut points = 0usize;
    let mut low_latitude = f64::INFINITY;
    let mut high_latitude = f64::NEG_INFINITY;
    let mut low_longitude = f64::INFINITY;
    let mut high_longitude = f64::NEG_INFINITY;
    for ring in &facade.rings {
        for point in ring {
            points += 1;
            if points > 100_000 {
                return Err("Facade footprint exceeds its point budget".into());
            }
            if point.iter().any(|value| !value.is_finite()) {
                return Err("Facade footprint has a non-finite coordinate".into());
            }
            low_latitude = low_latitude.min(point[0]);
            high_latitude = high_latitude.max(point[0]);
            low_longitude = low_longitude.min(point[1]);
            high_longitude = high_longitude.max(point[1]);
        }
    }
    if points == 0 {
        return Err("Facade has no footprint".into());
    }
    let origin = match bounds {
        Some(bounds) => [
            f64::midpoint(low_latitude, high_latitude).clamp(bounds[2] + 1e-6, bounds[3] - 1e-6),
            f64::midpoint(low_longitude, high_longitude).clamp(bounds[0] + 1e-6, bounds[1] - 1e-6),
        ],
        None => [low_latitude, low_longitude],
    };
    let scale = 111_320.0 * origin[0].to_radians().cos();
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    for ring in &facade.rings {
        for point in ring {
            let east = (point[1] - origin[1]) * scale;
            let north = (point[0] - origin[0]) * 111_320.0;
            low[0] = low[0].min(east);
            low[2] = low[2].min(-north);
            high[0] = high[0].max(east);
            high[2] = high[2].max(-north);
        }
    }
    if low[0] > high[0] || low[2] > high[2] {
        return Err("Facade footprint has no area".into());
    }
    low[1] = 0.0;
    high[1] = facade.height_meters;
    // A facade or a forest fills the area its windings enclose, so the real footprint is carried
    // through instead of covering every point inside its bounding box. A building that stands
    // beside a runway is then clear of the runway itself, while a building on it still refuses
    // the corridor. Windings that cannot be validated keep the box, which reports an obstruction
    // rather than clearing one.
    let footprint = if facade.forest {
        Footprint::new(&facade.rings, origin, scale)?.map(Arc::new)
    } else {
        Footprint::new(&facade.rings, origin, scale)
            .ok()
            .flatten()
            .map(Arc::new)
    };
    Ok(Obstacle {
        local_ground: None,
        placement: Placement {
            resource: facade.resource,
            coordinate: origin,
            heading: 0.0,
            // The parameter is height above ground, which is how obstacles are anchored.
            height: Some(facade.height_meters),
        },
        geometry: Geometry::One(object::Bounds {
            low: [
                narrow(true, low[0]),
                narrow(true, low[1]),
                narrow(true, low[2]),
            ],
            high: [
                narrow(false, high[0]),
                narrow(false, high[1]),
                narrow(false, high[2]),
            ],
        }),
        footprint,
        agl: true,
        radius: low[0]
            .abs()
            .max(high[0].abs())
            .max(low[2].abs())
            .max(high[2].abs()),
    })
}

/// How far a network point must stand above the draped terrain before its section is treated as
/// a bridge rather than a road lying on the ground.
const BRIDGE_METERS: f64 = 2.0;

/// Resolve one network chain against the mesh.
///
/// The third vector plane is a height above the draped terrain, not an absolute elevation, even
/// though the DSF specification describes vertical values as metres above mean sea level. Across
/// the installed global scenery the published value is 0 to 2 m over terrain that climbs past
/// 4,000 m, so an absolute reading would place every road kilometres underground and miss every
/// bridge. The elevation plane uses a signed-metre scale covering -32768..32767 m, so a road on
/// the ground sits near zero. A run of points published above the threshold is a bridge and
/// obstructs; a run on the terrain is ground and does not. Requiring physical terrain under every
/// point keeps a section over water or an unknown gap unresolved rather than clearing it. A run
/// is covered by its bounding box, matching the facade approach and erring toward an obstruction.
fn bridge_obstacles(
    bounds: [f64; 4],
    triangles: &[[Vertex; 3]],
    bins: &[Vec<u32>],
    chain: &[[f64; 3]],
) -> Result<Vec<Obstacle>, String> {
    let mut ground = Vec::with_capacity(chain.len());
    for point in chain {
        ground.push(
            mesh_height(bounds, triangles, bins, [point[0], point[1]])
                .ok_or("Network chain crosses terrain with no physical mesh")?,
        );
    }
    let mut bridges = Vec::new();
    let mut at = 0usize;
    while at < chain.len() {
        if chain[at][2] <= BRIDGE_METERS {
            at += 1;
            continue;
        }
        let start = at;
        while at < chain.len() && chain[at][2] > BRIDGE_METERS {
            at += 1;
        }
        if at - start < 2 {
            continue;
        }
        let section = &chain[start..at];
        // Anchor the section at its own ground and carry the highest published clearance in the
        // geometry, so the checked top is the deck at its real point rather than twice the
        // clearance.
        let top = section
            .iter()
            .zip(&ground[start..at])
            .map(|(point, ground)| ground + point[2])
            .fold(f64::NEG_INFINITY, f64::max);
        let height = top - ground[start];
        if !height.is_finite() || !(0.0..=1000.0).contains(&height) {
            return Err("Network bridge height is outside its published range".into());
        }
        bridges.push(bridge_obstacle(section, height)?);
    }
    Ok(bridges)
}

fn bridge_obstacle(points: &[[f64; 3]], height_meters: f64) -> Result<Obstacle, String> {
    let origin = points[0];
    let scale = 111_320.0 * origin[0].to_radians().cos();
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    for point in points {
        if point.iter().any(|value| !value.is_finite()) {
            return Err("Network bridge has a non-finite coordinate".into());
        }
        let east = (point[1] - origin[1]) * scale;
        let north = (point[0] - origin[0]) * 111_320.0;
        low[0] = low[0].min(east);
        high[0] = high[0].max(east);
        low[2] = low[2].min(-north);
        high[2] = high[2].max(-north);
    }
    low[1] = 0.0;
    high[1] = height_meters;
    Ok(Obstacle {
        local_ground: None,
        placement: Placement {
            resource: Arc::<str>::from("network"),
            coordinate: [origin[0], origin[1]],
            heading: 0.0,
            height: None,
        },
        geometry: Geometry::One(object::Bounds {
            low: [
                narrow(true, low[0]),
                narrow(true, low[1]),
                narrow(true, low[2]),
            ],
            high: [
                narrow(false, high[0]),
                narrow(false, high[1]),
                narrow(false, high[2]),
            ],
        }),
        footprint: None,
        agl: false,
        radius: low[0]
            .abs()
            .max(high[0].abs())
            .max(low[2].abs())
            .max(high[2].abs()),
    })
}

/// Geometry of an airport ground polygon (`.agp`).
///
/// An AGP places catalogue objects at texture positions. `TEXTURE_SCALE` gives the texture size
/// in pixels and `TEXTURE_WIDTH`/`TEXTURE_HEIGHT` the world size of that texture, so a pixel
/// offset converts to metres. Positions are taken relative to `ANCHOR_PT`, or the centre of the
/// placed tile when the definition has no anchor. One box covers every placed object plus its
/// own horizontal reach, and the tallest object sets the height.
fn agp_geometry(
    pack: &Path,
    library: Option<&library::Library>,
    path: &Path,
) -> Result<Vec<object::Bounds>, String> {
    let mut text = String::new();
    cache::open_file(path)
        .map_err(|e| e.to_string())?
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 4 * 1024 * 1024 {
        return Err("Airport ground polygon exceeds its input budget".into());
    }
    let mut lines = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty());
    if !matches!(lines.next(), Some("A" | "I"))
        || !matches!(lines.next(), Some("800" | "1000"))
        || lines.next() != Some("AG_POINT")
    {
        return Err("Scenery ground polygon requires an AG_POINT definition".into());
    }
    let number = |fields: &mut std::str::SplitWhitespace| -> Result<f64, String> {
        fields
            .next()
            .ok_or("Airport ground polygon field is missing")?
            .parse::<f64>()
            .map_err(|_| "Airport ground polygon has an invalid number".into())
    };
    let mut objects = Vec::new();
    let mut vegetation = Vec::new();
    let mut placed = Vec::new();
    let mut width = 0.0_f64;
    let mut height = 0.0_f64;
    let mut scale = [0.0_f64; 2];
    let mut tile = None;
    let mut anchor = None;
    for line in lines {
        let mut fields = line.split_whitespace();
        let command = fields.next();
        match command {
            Some("OBJECT") => {
                let name = fields
                    .next()
                    .ok_or("Airport ground polygon object is missing")?;
                if objects.len() >= 1024 {
                    return Err("Airport ground polygon catalogue exceeds its budget".into());
                }
                objects.push(name.to_owned());
            }
            Some("VEGETATION") => {
                let name = fields
                    .last()
                    .ok_or("Airport ground polygon vegetation is missing")?;
                if vegetation.len() >= 256 {
                    return Err("Airport ground polygon vegetation exceeds its budget".into());
                }
                vegetation.push(name.to_owned());
            }
            Some(command) if command.starts_with("OBJ_") => {
                // OBJ_DRAPED and OBJ_GRADED place catalogue objects directly; OBJ_DELTA adds a
                // published elevation offset before the object index.
                let x = number(&mut fields)?;
                let y = number(&mut fields)?;
                number(&mut fields)?;
                if command == "OBJ_DELTA" {
                    number(&mut fields)?;
                }
                let index = number(&mut fields)?;
                if !index.is_finite() || index < 0.0 || index > 1024.0 {
                    return Err("Airport ground polygon object index is invalid".into());
                }
                if placed.len() >= 4096 {
                    return Err("Airport ground polygon placements exceed their budget".into());
                }
                placed.push((x, y, index as usize));
            }
            Some("TEXTURE_WIDTH") => width = number(&mut fields)?,
            Some("TEXTURE_HEIGHT") => height = number(&mut fields)?,
            Some("TEXTURE_SCALE") => {
                scale[0] = number(&mut fields)?;
                scale[1] = number(&mut fields)?;
            }
            Some("TILE") => {
                let mut values = [0.0_f64; 4];
                for value in &mut values {
                    *value = number(&mut fields)?;
                }
                tile = Some(values);
            }
            Some("ANCHOR_PT") => {
                let x = number(&mut fields)?;
                let y = number(&mut fields)?;
                anchor = Some([x, y]);
            }
            _ => {}
        }
    }
    let tile = tile.ok_or("Airport ground polygon has no placed tile")?;
    // The shipped definitions sometimes publish only one texture dimension. The texture scale
    // gives the aspect ratio, so the missing side follows from the published one.
    if width == 0.0 && height > 0.0 && scale[0] > 0.0 && scale[1] > 0.0 {
        width = height * scale[0] / scale[1];
    }
    if height == 0.0 && width > 0.0 && scale[0] > 0.0 && scale[1] > 0.0 {
        height = width * scale[1] / scale[0];
    }
    if !width.is_finite()
        || !height.is_finite()
        || !(0.0..=10_000.0).contains(&width)
        || !(0.0..=10_000.0).contains(&height)
        || width <= 0.0
        || height <= 0.0
        || !scale.iter().all(|value| value.is_finite() && *value > 0.0)
    {
        return Err("Airport ground polygon has no usable extent".into());
    }
    let pixels = [width / scale[0], height / scale[1]];
    if pixels
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0 || *value > 100.0)
    {
        return Err("Airport ground polygon has an invalid texture scale".into());
    }
    let tile_extent = [
        (tile[2] - tile[0]).abs() / 2.0,
        (tile[3] - tile[1]).abs() / 2.0,
    ];
    let (anchor, anchored) = match anchor {
        Some(anchor) => (anchor, true),
        None => (
            [
                f64::midpoint(tile[0], tile[2]),
                f64::midpoint(tile[1], tile[3]),
            ],
            false,
        ),
    };
    if placed.is_empty() && vegetation.is_empty() {
        return Err("Airport ground polygon places nothing".into());
    }
    let parent = path
        .parent()
        .ok_or("Airport ground polygon has no package directory")?;
    let mut top = 0.0_f64;
    let mut low = 0.0_f64;
    let mut reach = 0.0_f64;
    let mut extent = if anchored { [0.0_f64; 2] } else { tile_extent };
    for (x, y, index) in placed {
        let name = objects
            .get(index)
            .ok_or("Airport ground polygon references a missing object")?;
        extent[0] = extent[0].max((x - anchor[0]).abs() * pixels[0]);
        extent[1] = extent[1].max((y - anchor[1]).abs() * pixels[1]);
        for bound in agp_object_geometry(pack, library, parent, name)? {
            top = top.max(f64::from(bound.high[1]));
            low = low.min(f64::from(bound.low[1]));
            reach = reach.max(horizontal_reach(&bound));
        }
    }
    if !vegetation.is_empty() {
        // Vegetation is scattered across the placed tile, so the whole tile is covered.
        extent[0] = extent[0].max(tile_extent[0]);
        extent[1] = extent[1].max(tile_extent[1]);
        for name in &vegetation {
            let height = library
                .and_then(|library| library.forest_height(pack, name).ok())
                .ok_or("Airport ground polygon vegetation is unresolvable")?;
            top = top.max(height);
        }
    }
    Ok(vec![object::Bounds {
        low: [
            (-(extent[0] + reach)) as f32,
            low as f32,
            (-(extent[1] + reach)) as f32,
        ],
        high: [
            (extent[0] + reach) as f32,
            top as f32,
            (extent[1] + reach) as f32,
        ],
    }])
}

fn geometry_reach(geometry: &[object::Bounds]) -> f64 {
    geometry
        .iter()
        .map(horizontal_reach)
        .fold(0.0_f64, f64::max)
}

fn horizontal_reach(bound: &object::Bounds) -> f64 {
    bound
        .low
        .iter()
        .zip(bound.high)
        .enumerate()
        .filter(|(axis, _)| *axis != 1)
        .map(|(_, (low, high))| f64::from(low.abs().max(high.abs())))
        .fold(0.0_f64, f64::max)
}

/// Resolve one object named by an `.agp` or string catalogue.
fn agp_object_geometry(
    pack: &Path,
    library: Option<&library::Library>,
    parent: &Path,
    name: &str,
) -> Result<Vec<object::Bounds>, String> {
    let resources = if name.to_ascii_lowercase().starts_with("lib/") {
        let relative = library::relative(name).map_err(|error| format!("{name}: {error}"))?;
        library.map_or_else(
            || Ok(vec![pack.join(relative)]),
            |library| library.resources(pack, name),
        )?
    } else {
        // A catalogue entry without a library path names a file beside the definition.
        vec![library::nested(parent, name).map_err(|error| format!("{name}: {error}"))?]
    };
    let mut geometry = Vec::new();
    for resource in resources {
        geometry.extend(object::load(&resource).map_err(|error| format!("{name}: {error}"))?);
    }
    Ok(geometry)
}

#[derive(Serialize, Deserialize)]
pub(super) struct Tile {
    pub bounds: [f64; 4],
    pub overlay: bool,
    pub unresolved_obstacles: bool,
    unresolved_classes: BTreeSet<String>,
    unresolved_objects: Vec<[f64; 2]>,
    unresolved_objects_overflow: bool,
    objects: Vec<Placement>,
    #[serde(with = "cache::obstacles")]
    obstacles: Vec<Obstacle>,
    /// Obstacle indices by coarse cell, so a corridor check examines nearby obstacles only.
    #[serde(skip)]
    obstacle_bins: Vec<Vec<u32>>,
    draped_resources: BTreeSet<Arc<str>>,
    polygons: Vec<DrapedPolygon>,
    facades: Vec<dsf::Facade>,
    strings: Vec<dsf::StringPolygon>,
    /// Vector network chains as latitude, longitude and published height above the draped
    /// terrain, pending the terrain comparison that decides which sections are bridges.
    networks: Vec<Vec<[f64; 3]>>,
    polygon_exclusions: Vec<[f64; 4]>,
    triangles: Vec<[Vertex; 3]>,
    bins: Vec<Vec<u32>>,
}
impl Tile {
    fn bytes(&self) -> usize {
        self.triangles.capacity() * std::mem::size_of::<[Vertex; 3]>()
            + self
                .bins
                .iter()
                .map(|bin| bin.capacity() * std::mem::size_of::<u32>())
                .sum::<usize>()
            + self.bins.capacity() * std::mem::size_of::<Vec<u32>>()
            + self.object_bytes()
    }
    fn unresolved(&mut self, class: &'static str) {
        self.unresolved_obstacles = true;
        self.unresolved_classes.insert(class.into());
    }

    fn unresolved_object(&mut self, coordinate: [f64; 2]) {
        self.unresolved_obstacles = true;
        self.unresolved_classes.insert("object".into());
        if self.unresolved_objects.len() < 100_000 {
            self.unresolved_objects.push(coordinate);
        } else {
            self.unresolved_objects_overflow = true;
        }
    }

    /// Whether an unresolved placement can reach this corridor. Area features (forests, facades,
    /// strings, draped polygons and networks) keep the tile-level refusal; point objects are
    /// filtered by their anchor, with a conservative reach because their geometry is unknown.
    fn unresolved_object_near(&self, from: Waypoint, to: Waypoint, radius: f64) -> bool {
        if self.unresolved_objects_overflow
            || self
                .unresolved_classes
                .iter()
                .any(|class| *class != "object")
            || (self.unresolved_obstacles && self.unresolved_classes.is_empty())
        {
            return true;
        }
        if self.unresolved_objects.is_empty() {
            return false;
        }
        let segment_latitude = from.latitude.to_radians().cos().max(0.01);
        let project = |latitude: f64, longitude: f64| {
            [
                longitude * 111_320.0 * segment_latitude,
                latitude * 111_320.0,
            ]
        };
        let start = project(from.latitude, from.longitude);
        let end = project(to.latitude, to.longitude);
        self.unresolved_objects.iter().any(|point| {
            point_segment_distance(project(point[0], point[1]), start, end)
                <= radius + UNRESOLVED_OBJECT_REACH_METERS
        })
    }

    fn object_bytes(&self) -> usize {
        let mut geometry = BTreeSet::new();
        let resources = self
            .objects
            .iter()
            .map(|placement| placement.resource.as_ref())
            .collect::<BTreeSet<_>>();
        self.polygon_exclusions.capacity() * std::mem::size_of::<[f64; 4]>()
            + self.polygons.capacity() * std::mem::size_of::<DrapedPolygon>()
            + self
                .polygons
                .iter()
                .map(|polygon| {
                    polygon.resource.len()
                        + polygon.airport.as_ref().map_or(0, |airport| airport.len())
                        + polygon.rings.capacity() * std::mem::size_of::<Vec<[f64; 4]>>()
                        + polygon
                            .rings
                            .iter()
                            .map(|ring| ring.capacity() * std::mem::size_of::<[f64; 4]>())
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self.objects.capacity() * std::mem::size_of::<Placement>()
            + self.obstacles.capacity() * std::mem::size_of::<Obstacle>()
            + self
                .draped_resources
                .iter()
                .map(|resource| resource.len() + std::mem::size_of::<Arc<str>>())
                .sum::<usize>()
            + resources
                .iter()
                .map(|resource| resource.len())
                .sum::<usize>()
            + self
                .obstacles
                .iter()
                .filter(|obstacle| geometry.insert(obstacle.geometry.key()))
                .map(|obstacle| obstacle.geometry.bytes())
                .sum::<usize>()
            + self
                .obstacles
                .iter()
                .filter_map(|obstacle| obstacle.footprint.as_ref())
                .map(|footprint| {
                    footprint.rings.capacity() * std::mem::size_of::<Vec<[f64; 2]>>()
                        + footprint
                            .rings
                            .iter()
                            .map(|ring| ring.capacity() * std::mem::size_of::<[f64; 2]>())
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
    fn mesh_height(&self, coordinate: [f64; 2]) -> Option<f64> {
        mesh_height(self.bounds, &self.triangles, &self.bins, coordinate)
    }
    /// Compare this tile's network chains against the physical mesh of the stack it belongs to.
    ///
    /// Airport overlay tiles publish roads but no mesh, so resolving them in isolation would
    /// leave every overlay network unresolved. The mesh of the base layer covers the same tile
    /// bounds and is what the road is actually draped over.
    fn resolve_networks(&mut self, bounds: [f64; 4], triangles: &[[Vertex; 3]], bins: &[Vec<u32>]) {
        let mut bridges = Vec::new();
        let mut unknown = false;
        for chain in std::mem::take(&mut self.networks) {
            match bridge_obstacles(bounds, triangles, bins, &chain) {
                Ok(mut found) => bridges.append(&mut found),
                Err(_) => {
                    unknown = true;
                }
            }
        }
        self.obstacles.append(&mut bridges);
        self.unresolved_obstacles |= unknown;
        if unknown {
            self.unresolved_classes.insert("network".into());
        }
    }
    fn resolve_strings(&mut self, bounds: [f64; 4], triangles: &[[Vertex; 3]], bins: &[Vec<u32>]) {
        let mut created = Vec::new();
        let mut unknown = false;
        for string in std::mem::take(&mut self.strings) {
            if string.autogen {
                // An autogen area is filled with scattered objects, so the whole area is an
                // obstacle. Modelling only its boundary as a line of objects both cost a box
                // per segment and cleared corridors that actually cross vegetation.
                let Some(first) = string.rings.iter().flatten().next().copied() else {
                    continue;
                };
                let anchor = [
                    first[0].clamp(bounds[2] + 1e-6, bounds[3] - 1e-6),
                    first[1].clamp(bounds[0] + 1e-6, bounds[1] - 1e-6),
                ];
                let scale = 111_320.0 * anchor[0].to_radians().cos();
                let mut low = [f64::INFINITY; 3];
                let mut high = [f64::NEG_INFINITY; 3];
                for ring in &string.rings {
                    for point in ring {
                        let east = (point[1] - anchor[1]) * scale;
                        let north = (point[0] - anchor[0]) * 111_320.0;
                        low[0] = low[0].min(east);
                        low[2] = low[2].min(-north);
                        high[0] = high[0].max(east);
                        high[2] = high[2].max(-north);
                    }
                }
                if low[0] > high[0] || low[2] > high[2] {
                    continue;
                }
                let Some(anchor_ground) = mesh_height(bounds, triangles, bins, anchor) else {
                    unknown = true;
                    continue;
                };
                let reach = string.reach_meters;
                low[0] -= reach;
                low[2] -= reach;
                high[0] += reach;
                high[2] += reach;
                // Include all terrain under the complete footprint and object reach, including
                // interior ridges. Bin membership only selects candidates: a triangle may extend
                // far outside this area. Clip it before taking its maximum linear elevation.
                let area = [
                    anchor[1] + low[0] / scale,
                    anchor[1] + high[0] / scale,
                    anchor[0] - high[2] / 111_320.0,
                    anchor[0] - low[2] / 111_320.0,
                ];
                let Some(ground) = mesh_maximum(bounds, triangles, bins, area) else {
                    unknown = true;
                    continue;
                };
                let ground = ground.max(anchor_ground);
                low[1] = 0.0;
                high[1] = ground - anchor_ground + string.height_meters;
                // The area is kept as a real winding when it is well formed. A winding that
                // cannot be classified falls back to its bounding box, which reports an
                // obstruction everywhere the area is drawn rather than clearing it.
                let footprint =
                    Footprint::autogen(&string.rings, anchor, scale).map(|mut footprint| {
                        footprint.reach = reach;
                        Arc::new(footprint)
                    });
                created.push(Obstacle {
                    local_ground: string
                        .ground_reach_meters
                        .map(|ground_reach| (string.height_meters, ground_reach + reach)),
                    placement: Placement {
                        resource: string.resource.clone(),
                        coordinate: anchor,
                        heading: 0.0,
                        height: None,
                    },
                    geometry: Geometry::One(object::Bounds {
                        low: low.map(|value| narrow(true, value)),
                        high: high.map(|value| narrow(false, value)),
                    }),
                    footprint,
                    agl: false,
                    radius: low[0]
                        .abs()
                        .max(high[0].abs())
                        .hypot(low[2].abs().max(high[2].abs())),
                });
                continue;
            }
            for ring in &string.rings {
                if ring.is_empty() {
                    continue;
                }
                for index in 0..ring.len() {
                    let a = ring[index];
                    let b = ring[(index + 1) % ring.len()];
                    // A string can span kilometres. Each segment is cut into short pieces so its
                    // box follows the published line instead of covering the whole diagonal.
                    let scale = 111_320.0 * a[0].to_radians().cos();
                    let full_east = (b[1] - a[1]) * scale;
                    let full_north = (b[0] - a[0]) * 111_320.0;
                    let pieces = ((full_east.hypot(full_north) / 1000.0).ceil() as usize).max(1);
                    for piece in 0..pieces {
                        let start = piece as f64 / pieces as f64;
                        let end = (piece + 1) as f64 / pieces as f64;
                        let from = [a[0] + (b[0] - a[0]) * start, a[1] + (b[1] - a[1]) * start];
                        let to = [a[0] + (b[0] - a[0]) * end, a[1] + (b[1] - a[1]) * end];
                        // A long facade ring can continue past the tile that stores it. The box
                        // keeps its published position, but its anchor and ground are taken at
                        // the nearest point inside the owning tile so the wall is not dropped;
                        // the neighbouring tile carries the same definition for its own area.
                        let anchor = [
                            from[0].clamp(bounds[2] + 1e-6, bounds[3] - 1e-6),
                            from[1].clamp(bounds[0] + 1e-6, bounds[1] - 1e-6),
                        ];
                        let sample = |point: [f64; 2]| {
                            mesh_height(
                                bounds,
                                triangles,
                                bins,
                                [
                                    point[0].clamp(bounds[2] + 1e-6, bounds[3] - 1e-6),
                                    point[1].clamp(bounds[0] + 1e-6, bounds[1] - 1e-6),
                                ],
                            )
                        };
                        let Some(ground_a) = sample(anchor) else {
                            unknown = true;
                            continue;
                        };
                        let Some(ground_b) = sample(to) else {
                            unknown = true;
                            continue;
                        };
                        // A string repeats its catalogue along the segment. The checked box is
                        // the piece widened by the widest placed object and carried up from its
                        // own ground, so a long string does not borrow a hilltop height
                        // elsewhere. The box is expressed relative to the clamped anchor.
                        let shift = [
                            (from[1] - anchor[1]) * scale,
                            (from[0] - anchor[0]) * 111_320.0,
                        ];
                        let east = (to[1] - from[1]) * scale + shift[0];
                        let north = (to[0] - from[0]) * 111_320.0 + shift[1];
                        let reach = string.reach_meters;
                        let low = [
                            east.min(shift[0]) - reach,
                            0.0,
                            (-north).min(-shift[1]) - reach,
                        ];
                        let high = [
                            east.max(shift[0]) + reach,
                            (ground_a.max(ground_b) - ground_a) + string.height_meters,
                            (-north).max(-shift[1]) + reach,
                        ];
                        if high[1] <= low[1] {
                            continue;
                        }
                        created.push(Obstacle {
                            local_ground: None,
                            placement: Placement {
                                resource: string.resource.clone(),
                                coordinate: anchor,
                                heading: 0.0,
                                height: None,
                            },
                            geometry: Geometry::One(object::Bounds {
                                low: [
                                    narrow(true, low[0]),
                                    narrow(true, low[1]),
                                    narrow(true, low[2]),
                                ],
                                high: [
                                    narrow(false, high[0]),
                                    narrow(false, high[1]),
                                    narrow(false, high[2]),
                                ],
                            }),
                            footprint: None,
                            agl: false,
                            radius: low[0]
                                .abs()
                                .max(high[0].abs())
                                .max(low[2].abs())
                                .max(high[2].abs()),
                        });
                    }
                }
            }
        }
        self.obstacles.append(&mut created);
        self.unresolved_obstacles |= unknown;
        if unknown {
            self.unresolved_classes.insert("string".into());
        }
    }

    #[cfg(test)]
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_with(path, None)
    }
    fn load_with(path: &Path, library: Option<&library::Library>) -> Result<Self, String> {
        let mut tile = dsf::parse(&file_bytes(path)?)?;
        let pack = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or("Scenery tile has no package root")?;
        let mut loaded = BTreeMap::new();
        let mut geometry_bytes = 0;
        // A tile can reference the same forest hundreds of times, so each definition is read
        // once and its height reused.
        let mut forest_heights: BTreeMap<Arc<str>, Option<f64>> = BTreeMap::new();
        let mut retained_strings: Vec<dsf::StringPolygon> = Vec::new();
        let mut facade_solid: BTreeMap<Arc<str>, Option<bool>> = BTreeMap::new();
        for mut placed in std::mem::take(&mut tile.facades) {
            if placed.forest {
                // A forest's height is published by its tree definitions, so it is resolved
                // through the same library stack. Without it the polygon stays unresolved.
                let height = *forest_heights
                    .entry(placed.resource.clone())
                    .or_insert_with(|| {
                        library
                            .and_then(|library| library.forest_height(pack, &placed.resource).ok())
                    });
                if let Some(height) = height {
                    placed.height_meters = height;
                } else {
                    tile.unresolved("forest");
                    continue;
                }
            }
            if !placed.forest {
                // A definition with no roof is a fence or an open structure. Its obstacle is the
                // wall along each ring, not the area the ring encloses, so it is resolved against
                // the mesh in the same way as an object string.
                let solid = library.is_none_or(|library| {
                    if let Some(solid) = facade_solid.get(&placed.resource) {
                        return solid.unwrap_or(true);
                    }
                    let solid = if let Ok(solid) = library.facade_solid(pack, &placed.resource) {
                        Some(solid)
                    } else {
                        tile.unresolved("facade");
                        None
                    };
                    facade_solid.insert(placed.resource.clone(), solid);
                    solid.unwrap_or(true)
                });
                if !solid {
                    if !placed.height_meters.is_finite()
                        || !(0.0..=1000.0).contains(&placed.height_meters)
                    {
                        tile.unresolved("facade");
                        continue;
                    }
                    retained_strings.push(dsf::StringPolygon {
                        resource: placed.resource,
                        rings: placed.rings,
                        autogen: false,
                        height_meters: placed.height_meters,
                        reach_meters: 0.0,
                        ground_reach_meters: None,
                    });
                    continue;
                }
            }
            match facade_obstacle_in(placed, Some(tile.bounds)) {
                Ok(obstacle) => tile.obstacles.push(obstacle),
                Err(_) => {
                    tile.unresolved("facade");
                }
            }
        }
        let mut string_heights: BTreeMap<Arc<str>, Option<(f64, f64)>> = BTreeMap::new();
        let mut string_objects: BTreeMap<String, Option<(f64, f64)>> = BTreeMap::new();
        for mut string in std::mem::take(&mut tile.strings) {
            let height = if let Some(height) = string_heights.get(&string.resource) {
                *height
            } else {
                let height = library.and_then(|library| {
                    library
                        .string_height(pack, &string.resource, string.autogen, &mut string_objects)
                        .ok()
                });
                string_heights.insert(string.resource.clone(), height);
                height
            };
            match height {
                Some((height, reach)) => {
                    string.height_meters = height;
                    string.reach_meters = reach;
                    string.ground_reach_meters = string
                        .autogen
                        .then(|| {
                            library.and_then(|library| {
                                library.autogen_ground_reach(pack, &string.resource)
                            })
                        })
                        .flatten();
                    retained_strings.push(string);
                }
                None => {
                    tile.unresolved("string");
                }
            }
        }
        tile.strings = retained_strings;
        if tile.draped_resources.iter().any(|resource| {
            !library.is_some_and(|library| library.ground_only(pack, resource).unwrap_or(false))
        }) {
            tile.unresolved("draped");
        }
        let objects = std::mem::take(&mut tile.objects);
        let mut object_failures = 0usize;
        for placement in &objects {
            if !loaded.contains_key(&placement.resource) {
                if loaded.len() >= 4096 {
                    tile.unresolved_object(placement.coordinate);
                    continue;
                }
                let relative = Path::new(placement.resource.trim_start_matches('/'));
                let geometry = if relative.components().all(|part| {
                    matches!(
                        part,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                }) {
                    let resources = library.map_or_else(
                        || Ok(vec![pack.join(relative)]),
                        |library| library.resources(pack, &placement.resource),
                    );
                    resources
                        .and_then(|resources| {
                            let ground_polygon = Path::new(placement.resource.as_ref())
                                .extension()
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("agp"));
                            let mut geometry = Vec::new();
                            for resource in resources {
                                let part = if ground_polygon {
                                    agp_geometry(pack, library, &resource)?
                                } else {
                                    object::load(&resource)?
                                };
                                if (geometry.len() + part.len())
                                    * std::mem::size_of::<object::Bounds>()
                                    > 32 * 1024 * 1024
                                {
                                    return Err("Object variants exceed 32 MiB".into());
                                }
                                geometry.extend(part);
                            }
                            Ok(geometry)
                        })
                        .and_then(|geometry| {
                            let bytes = geometry.capacity() * std::mem::size_of::<object::Bounds>();
                            if geometry_bytes + bytes > 32 * 1024 * 1024 {
                                return Err("Tile object geometry exceeds 32 MiB".into());
                            }
                            geometry_bytes += bytes;
                            Ok(Arc::new(geometry))
                        })
                } else {
                    Err("Invalid scenery object resource path".into())
                };
                loaded.insert(placement.resource.clone(), geometry);
            }
            match loaded.get(&placement.resource) {
                Some(Ok(geometry)) => tile.obstacles.push(Obstacle {
                    local_ground: None,
                    placement: placement.clone(),
                    geometry: Geometry::Many(geometry.clone()),
                    footprint: None,
                    agl: false,
                    radius: geometry_reach(geometry),
                }),
                Some(Err(error)) => {
                    if object_failures < 3 {
                        eprintln!(
                            "Generated traffic scenery: unresolved object {} at {:.5}/{:.5}: {error}",
                            placement.resource, placement.coordinate[0], placement.coordinate[1]
                        );
                        object_failures += 1;
                    }
                    tile.unresolved_object(placement.coordinate);
                }
                None => {
                    tile.unresolved_object(placement.coordinate);
                }
            }
        }
        tile.objects = objects;
        // Network chains are resolved against the mesh stack once the tile is placed in it: an
        // airport overlay publishes roads without any physical mesh of its own.
        Ok(tile)
    }

    /// Index the obstacle placements into coarse cells.
    ///
    /// An obstacle is listed in every cell its own extent can reach, so a corridor that passes
    /// close to the placement finds it even when the obstacle covers several cells. The index is
    /// rebuilt after a tile is read, so it costs a pass over the obstacles and no stored bytes.
    fn index_obstacles(&mut self) {
        self.obstacle_bins = vec![Vec::new(); OBSTACLE_GRID * OBSTACLE_GRID];
        for (index, obstacle) in self.obstacles.iter().enumerate() {
            let coordinate = obstacle.placement.coordinate;
            let reach = (obstacle.radius + MAXIMUM_ENVELOPE_METERS) / 111_320.0;
            let low = self.obstacle_cell(coordinate);
            let high = self.obstacle_cell([
                coordinate[0] + reach,
                coordinate[1] + reach / coordinate[0].to_radians().cos().max(0.01),
            ]);
            for y in low[1]..=high[1] {
                for x in low[0]..=high[0] {
                    self.obstacle_bins[y * OBSTACLE_GRID + x].push(index as u32);
                }
            }
        }
    }

    /// The cell that holds a coordinate, clamped to the tile.
    fn obstacle_cell(&self, coordinate: [f64; 2]) -> [usize; 2] {
        let bin = |value: f64, low: f64, high: f64| {
            ((value - low) / (high - low) * OBSTACLE_GRID as f64)
                .floor()
                .clamp(0.0, (OBSTACLE_GRID - 1) as f64) as usize
        };
        [
            bin(coordinate[1], self.bounds[0], self.bounds[1]),
            bin(coordinate[0], self.bounds[2], self.bounds[3]),
        ]
    }

    /// The cells a corridor crosses, so a check examines the nearby obstacles only.
    ///
    /// The walk follows the segment itself rather than its bounding box, and every sample is
    /// clamped into the tile, so an obstacle whose placement lies near a tile edge is still
    /// found when the corridor leaves the tile.
    fn obstacle_cells(&self, from: Waypoint, to: Waypoint) -> Vec<usize> {
        let distance = openatc_core::arrival::distance_nm(
            [from.latitude, from.longitude],
            [to.latitude, to.longitude],
        ) * 1852.0;
        let steps = ((distance / 400.0).ceil() as usize).clamp(1, 8192);
        let mut cells = Vec::new();
        for step in 0..=steps {
            let fraction = step as f64 / steps as f64;
            let coordinate = [
                from.latitude + (to.latitude - from.latitude) * fraction,
                from.longitude + (to.longitude - from.longitude) * fraction,
            ];
            let cell = self.obstacle_cell(coordinate);
            cells.push(cell[1] * OBSTACLE_GRID + cell[0]);
        }
        cells.sort_unstable();
        cells.dedup();
        cells
    }

    fn index(&mut self) -> Result<(), String> {
        self.bins = vec![Vec::new(); GRID * GRID];
        let mut references = 0usize;
        for (index, triangle) in self.triangles.iter().enumerate() {
            let low = [
                triangle
                    .iter()
                    .map(|p| p.longitude)
                    .fold(f64::INFINITY, f64::min),
                triangle
                    .iter()
                    .map(|p| p.latitude)
                    .fold(f64::INFINITY, f64::min),
            ];
            let high = [
                triangle
                    .iter()
                    .map(|p| p.longitude)
                    .fold(f64::NEG_INFINITY, f64::max),
                triangle
                    .iter()
                    .map(|p| p.latitude)
                    .fold(f64::NEG_INFINITY, f64::max),
            ];
            let a = self.cell(low);
            let b = self.cell(high);
            for y in a[1]..=b[1] {
                for x in a[0]..=b[0] {
                    references += 1;
                    if references > 8 * 1024 * 1024 {
                        return Err("DSF mesh index exceeds the tile budget".into());
                    }
                    self.bins[y * GRID + x].push(index as u32);
                }
            }
        }
        Ok(())
    }

    fn cell(&self, coordinate: [f64; 2]) -> [usize; 2] {
        let bin = |value: f64, low: f64, high: f64| {
            ((value - low) / (high - low) * GRID as f64)
                .floor()
                .clamp(0.0, (GRID - 1) as f64) as usize
        };
        [
            bin(coordinate[0], self.bounds[0], self.bounds[1]),
            bin(coordinate[1], self.bounds[2], self.bounds[3]),
        ]
    }

    /// Check the full line against every intersecting physical triangle, leaving gaps unknown.
    pub fn terrain_clear(&self, from: Waypoint, to: Waypoint, margin_feet: f64) -> Option<bool> {
        if !(0.0..=3000.0).contains(&margin_feet)
            || [from, to].iter().any(|p| {
                !p.altitude_feet.is_finite()
                    || !(self.bounds[0]..=self.bounds[1]).contains(&p.longitude)
                    || !(self.bounds[2]..=self.bounds[3]).contains(&p.latitude)
            })
        {
            return None;
        }
        let a = self.cell([
            from.longitude.min(to.longitude),
            from.latitude.min(to.latitude),
        ]);
        let b = self.cell([
            from.longitude.max(to.longitude),
            from.latitude.max(to.latitude),
        ]);
        let mut candidates: BTreeSet<u32> = BTreeSet::new();
        for y in a[1]..=b[1] {
            for x in a[0]..=b[0] {
                candidates.extend(self.bins[y * GRID + x].iter().copied());
            }
        }
        let mut covered = Vec::new();
        for index in candidates {
            let triangle = &self.triangles[index as usize];
            let Some(weights) = barycentric(triangle, from) else {
                continue;
            };
            let Some(last) = barycentric(triangle, to) else {
                continue;
            };
            let mut low: f64 = 0.0;
            let mut high: f64 = 1.0;
            for (start, end) in weights.into_iter().zip(last) {
                let change = end - start;
                if change.abs() < 1e-15 {
                    if start < -1e-9 {
                        low = 1.0;
                        high = 0.0;
                        break;
                    }
                } else {
                    let edge = -start / change;
                    if change > 0.0 {
                        low = low.max(edge);
                    } else {
                        high = high.min(edge);
                    }
                }
            }
            if low > high + 1e-10 {
                continue;
            }
            low = low.clamp(0.0, 1.0);
            high = high.clamp(0.0, 1.0);
            if low > high {
                continue;
            }
            for at in [low, high] {
                let ground = weights
                    .iter()
                    .zip(last)
                    .zip(triangle)
                    .map(|((start, end), p)| (start + (end - start) * at) * p.meters)
                    .sum::<f64>()
                    / 0.3048;
                let aircraft = from.altitude_feet + (to.altitude_feet - from.altitude_feet) * at;
                if aircraft + 1e-6 < ground + margin_feet {
                    return Some(false);
                }
            }
            covered.push((low, high));
        }
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut through = 0.0;
        for (low, high) in covered {
            if low > through + 1e-8 {
                return None;
            }
            through = high.max(through);
        }
        (through >= 1.0 - 1e-8).then_some(true)
    }
}

fn cell(bounds: [f64; 4], coordinate: [f64; 2]) -> [usize; 2] {
    let bin = |value: f64, low: f64, high: f64| {
        ((value - low) / (high - low) * GRID as f64)
            .floor()
            .clamp(0.0, (GRID - 1) as f64) as usize
    };
    [
        bin(coordinate[0], bounds[0], bounds[1]),
        bin(coordinate[1], bounds[2], bounds[3]),
    ]
}

fn mesh_height(
    bounds: [f64; 4],
    triangles: &[[Vertex; 3]],
    bins: &[Vec<u32>],
    coordinate: [f64; 2],
) -> Option<f64> {
    let point = Waypoint {
        latitude: coordinate[0],
        longitude: coordinate[1],
        altitude_feet: 0.0,
    };
    let cell = cell(bounds, [coordinate[1], coordinate[0]]);
    bins[cell[1] * GRID + cell[0]]
        .iter()
        .filter_map(|index| {
            let triangle = &triangles[*index as usize];
            let weights = barycentric(triangle, point)?;
            (weights.iter().all(|weight| *weight >= -1e-9)).then(|| {
                triangle
                    .iter()
                    .map(|vertex| vertex.meters)
                    .fold(f64::NEG_INFINITY, f64::max)
            })
        })
        .reduce(f64::max)
}

/// Maximum linear terrain elevation inside an axis-aligned geographic footprint.
/// Clipped triangle vertices include edge intersections and every interior ridge vertex.
fn mesh_maximum(
    bounds: [f64; 4],
    triangles: &[[Vertex; 3]],
    bins: &[Vec<u32>],
    area: [f64; 4],
) -> Option<f64> {
    if area.iter().any(|v| !v.is_finite()) || area[0] > area[1] || area[2] > area[3] {
        return None;
    }
    let a = cell(bounds, [area[0], area[2]]);
    let b = cell(bounds, [area[1], area[3]]);
    let mut selected = BTreeSet::new();
    for y in a[1]..=b[1] {
        for x in a[0]..=b[0] {
            selected.extend(bins[y * GRID + x].iter().copied());
        }
    }
    let mut maximum: Option<f64> = None;
    for index in selected {
        let mut polygon = triangles[index as usize].to_vec();
        for (axis, boundary, minimum) in [
            (0, area[0], true),
            (0, area[1], false),
            (1, area[2], true),
            (1, area[3], false),
        ] {
            let coordinate = |p: Vertex| if axis == 0 { p.longitude } else { p.latitude };
            let inside = |p: Vertex| {
                if minimum {
                    coordinate(p) >= boundary
                } else {
                    coordinate(p) <= boundary
                }
            };
            let Some(mut previous) = polygon.last().copied() else {
                break;
            };
            let mut clipped = Vec::with_capacity(7);
            for current in polygon {
                if inside(current) != inside(previous) {
                    let fraction = (boundary - coordinate(previous))
                        / (coordinate(current) - coordinate(previous));
                    clipped.push(Vertex {
                        longitude: previous.longitude
                            + (current.longitude - previous.longitude) * fraction,
                        latitude: previous.latitude
                            + (current.latitude - previous.latitude) * fraction,
                        meters: previous.meters + (current.meters - previous.meters) * fraction,
                    });
                }
                if inside(current) {
                    clipped.push(current);
                }
                previous = current;
            }
            polygon = clipped;
        }
        for point in polygon {
            maximum = Some(maximum.map_or(point.meters, |m| m.max(point.meters)));
        }
    }
    maximum
}

fn barycentric(triangle: &[Vertex; 3], point: Waypoint) -> Option<[f64; 3]> {
    let [a, b, c] = triangle;
    let determinant = (b.latitude - c.latitude) * (a.longitude - c.longitude)
        + (c.longitude - b.longitude) * (a.latitude - c.latitude);
    if determinant.abs() < 1e-20 {
        return None;
    }
    let first = ((b.latitude - c.latitude) * (point.longitude - c.longitude)
        + (c.longitude - b.longitude) * (point.latitude - c.latitude))
        / determinant;
    let second = ((c.latitude - a.latitude) * (point.longitude - c.longitude)
        + (a.longitude - c.longitude) * (point.latitude - c.latitude))
        / determinant;
    Some([first, second, 1.0 - first - second])
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn atom(name: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let mut result = name.iter().rev().copied().collect::<Vec<_>>();
        result.extend(((payload.len() + 8) as u32).to_le_bytes());
        result.extend(payload);
        result
    }
    fn append(bytes: &mut Vec<u8>, id: [u8; 4], more: &[u8]) {
        let key = id.iter().rev().copied().collect::<Vec<_>>();
        let at = bytes.windows(4).position(|part| part == key).unwrap();
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        bytes.splice(at + size..at + size, more.iter().copied());
        bytes[at + 4..at + 8].copy_from_slice(&((size + more.len()) as u32).to_le_bytes());
    }

    pub(super) fn mesh_tile(
        west: i32,
        south: i32,
        meters: f32,
        overlay: bool,
        obstacles: bool,
    ) -> Vec<u8> {
        let props = [
            "sim/west".into(),
            west.to_string(),
            "sim/east".into(),
            (west + 1).to_string(),
            "sim/south".into(),
            south.to_string(),
            "sim/north".into(),
            (south + 1).to_string(),
            "sim/overlay".into(),
            if overlay { "1".into() } else { "0".into() },
        ]
        .into_iter()
        .flat_map(|s: String| s.into_bytes().into_iter().chain([0]))
        .collect::<Vec<_>>();
        let mut pool = 4u32.to_le_bytes().to_vec();
        pool.push(5);
        for values in [
            [0, 65535, 65535, 0],
            [0, 0, 65535, 65535],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
        ] {
            pool.push(0);
            for value in values {
                pool.extend((value as u16).to_le_bytes());
            }
        }
        let mut scale = Vec::new();
        for (factor, offset) in [
            (1.0, west as f32),
            (1.0, south as f32),
            (1.0, meters),
            (0.0, 0.0),
            (0.0, 0.0),
        ] {
            scale.extend(f32::to_le_bytes(factor));
            scale.extend(offset.to_le_bytes());
        }
        let mut commands = vec![
            1, 0, 0, 3, 0, 17, 1, 25, 0, 0, 3, 0, 23, 3, 0, 0, 2, 0, 3, 0,
        ];
        if obstacles {
            // One nested-polygon range has an initial index followed by one range endpoint.
            commands.extend([15, 0, 0, 1, 0, 0, 3, 0]);
            commands.extend([32, 2, 0, 0]);
        }
        let mut result = b"XPLNEDSF".to_vec();
        result.extend(1u32.to_le_bytes());
        result.extend(atom(*b"HEAD", atom(*b"PROP", props)));
        result.extend(atom(*b"DEFN", atom(*b"TERT", b"terrain\0".to_vec())));
        let mut geodata = atom(*b"POOL", pool);
        geodata.extend(atom(*b"SCAL", scale));
        result.extend(atom(*b"GEOD", geodata));
        result.extend(atom(*b"CMDS", commands));
        result.extend([0; 16]);
        result
    }

    /// A flat 100 m mesh tile carrying one vector network chain. Points are latitude, longitude
    /// and published height above the draped terrain in metres. The elevation plane uses the same
    /// signed-metre scale the global scenery publishes, where the full 32-bit range maps onto
    /// -32768..32767 metres.
    fn network_tile(points: &[[f64; 3]]) -> Vec<u8> {
        const ELEVATION_SCALE: (f64, f64) = (65535.0, -32768.0);
        let mut bytes = mesh_tile(0, 0, 100.0, false, false);
        let mut pool = (points.len() as u32).to_le_bytes().to_vec();
        pool.push(4);
        for plane in 0..4 {
            pool.push(0);
            for point in points {
                let value = match plane {
                    0 => (point[1] / 2.0 * 4_294_967_295.0).round() as u32,
                    1 => (point[0] / 2.0 * 4_294_967_295.0).round() as u32,
                    2 => (((point[2] - ELEVATION_SCALE.1) / ELEVATION_SCALE.0).clamp(0.0, 1.0)
                        * 4_294_967_295.0)
                        .round() as u32,
                    _ => 0,
                };
                pool.extend(value.to_le_bytes());
            }
        }
        let mut scale = Vec::new();
        for (factor, offset) in [
            (2.0f32, 0.0f32),
            (2.0, 0.0),
            (ELEVATION_SCALE.0 as f32, ELEVATION_SCALE.1 as f32),
            (0.0, 0.0),
        ] {
            scale.extend(factor.to_le_bytes());
            scale.extend(offset.to_le_bytes());
        }
        let mut geodata = atom(*b"PO32", pool);
        geodata.extend(atom(*b"SC32", scale));
        append(&mut bytes, *b"GEOD", &geodata);
        let mut commands = vec![1, 0, 0, 9, points.len() as u8];
        for index in 0..points.len() {
            commands.extend((index as u16).to_le_bytes());
        }
        append(&mut bytes, *b"CMDS", &commands);
        bytes
    }

    #[test]
    fn a_network_section_above_the_terrain_becomes_a_bridge_obstacle() {
        let root = std::env::temp_dir().join(format!(
            "openatc-network-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pack = root.join("Global Scenery/X-Plane 12 Global Scenery");
        let path = pack.join("Earth nav data/+00+000/+00+000.dsf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A chain that rises 30 m above the 100 m mesh between two ground points.
        std::fs::write(
            &path,
            network_tile(&[
                [0.5, 0.1, 0.0],
                [0.5, 0.3, 30.0],
                [0.5, 0.5, 30.0],
                [0.5, 0.7, 0.0],
            ]),
        )
        .unwrap();
        let mut terrain = Terrain::new(&root).unwrap();
        let through = |feet| Waypoint {
            latitude: 0.5,
            longitude: 0.1,
            altitude_feet: feet,
        };
        assert!(
            !terrain
                .obstacles_clear(
                    through(400.0),
                    Waypoint {
                        longitude: 0.7,
                        ..through(400.0)
                    },
                    0.0
                )
                .unwrap(),
            "A pass through the elevated section must be refused"
        );
        assert!(
            terrain
                .obstacles_clear(
                    through(500.0),
                    Waypoint {
                        longitude: 0.7,
                        ..through(500.0)
                    },
                    0.0
                )
                .unwrap(),
            "A pass above the elevated section must be cleared"
        );
        assert!(
            !terrain.has_unresolved_obstacles(),
            "A network on known terrain must not force an unresolved tile"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_network_on_the_terrain_is_not_an_obstacle_and_missing_terrain_refuses() {
        let root = std::env::temp_dir().join(format!(
            "openatc-network-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pack = root.join("Global Scenery/X-Plane 12 Global Scenery");
        let path = pack.join("Earth nav data/+00+000/+00+000.dsf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // On the terrain throughout: ground, not a bridge.
        std::fs::write(
            &path,
            network_tile(&[[0.5, 0.1, 0.0], [0.5, 0.5, 0.0], [0.5, 0.7, 0.0]]),
        )
        .unwrap();
        let mut terrain = Terrain::new(&root).unwrap();
        let from = Waypoint {
            latitude: 0.5,
            longitude: 0.1,
            altitude_feet: 50.0,
        };
        let to = Waypoint {
            longitude: 0.7,
            ..from
        };
        assert!(terrain.obstacles_clear(from, to, 0.0).unwrap());
        assert!(!terrain.has_unresolved_obstacles());
        // A point outside the physical mesh leaves the tile unresolved rather than clearing it.
        std::fs::write(&path, network_tile(&[[0.5, 0.5, 30.0], [0.5, 1.5, 30.0]])).unwrap();
        let mut terrain = Terrain::new(&root).unwrap();
        assert!(terrain.obstacles_clear(from, to, 0.0).unwrap());
        assert!(
            terrain.has_unresolved_obstacles(),
            "Missing terrain under a network point must keep the tile unresolved"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_object_anchored_in_a_neighbouring_tile_is_checked_across_the_boundary() {
        // A tower placed just inside +00+000 reaches east across the boundary into +00+001, so a
        // corridor that stays inside +00+001 still has to see it.
        let root = std::env::temp_dir().join(format!(
            "openatc-overhang-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pack = root.join("Global Scenery/X-Plane 12 Global Scenery");
        let write = |latitude: i32, longitude: i32, bytes: Vec<u8>| {
            let path = pack
                .join("Earth nav data")
                .join(format!(
                    "{:+03}{:+04}",
                    latitude.div_euclid(10) * 10,
                    longitude.div_euclid(10) * 10
                ))
                .join(format!("{latitude:+03}{longitude:+04}.dsf"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        let mut tower = mesh_tile(0, 0, 100.0, false, false);
        append(
            &mut tower,
            *b"DEFN",
            &atom(*b"OBJT", b"tower.obj\0".to_vec()),
        );
        let mut pool = 1u32.to_le_bytes().to_vec();
        pool.push(3);
        // Longitude 0.99995, latitude 0.5, heading 0 degrees.
        for value in [((0.99995_f64 * 65535.0).round() as u16), 32768u16, 0] {
            pool.push(0);
            pool.extend(value.to_le_bytes());
        }
        let mut scale = Vec::new();
        for factor in [1.0f32, 1.0, 0.0] {
            scale.extend(factor.to_le_bytes());
            scale.extend(0.0f32.to_le_bytes());
        }
        let mut geodata = atom(*b"POOL", pool);
        geodata.extend(atom(*b"SCAL", scale));
        append(&mut tower, *b"GEOD", &geodata);
        append(&mut tower, *b"CMDS", &[1, 1, 0, 3, 0, 7, 0, 0]);
        write(0, 0, tower);
        write(0, 1, mesh_tile(1, 0, 100.0, false, false));
        std::fs::write(
            pack.join("tower.obj"),
            "I\n800\nOBJ\nPOINT_COUNTS 3 0 0 3\nVT -5 0 -2 0 1 0 0 0\nVT 25 12 -2 0 1 0 0 0\nVT 25 0 2 0 1 0 0 0\nIDX 0\nIDX 1\nIDX 2\nTRIS 0 3\n",
        )
        .unwrap();
        let mut terrain = Terrain::new(&root).unwrap();
        let from = Waypoint {
            latitude: 0.5,
            longitude: 1.00005,
            altitude_feet: 360.0,
        };
        let to = Waypoint {
            longitude: 1.00015,
            ..from
        };
        assert_eq!(terrain.tile(0, 1).unwrap()[0].obstacles.len(), 0);
        assert!(
            !terrain.obstacles_clear(from, to, 0.0).unwrap(),
            "An object overhanging from the neighbouring tile must block the corridor"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unresolved_object_only_refuses_corridors_it_can_reach() {
        // A missing library object has no geometry. Its anchor still has to be close enough to
        // the corridor to matter: a dinghy 50 km away must not refuse a departure, while the same
        // unresolved placement beside the route must.
        let root = std::env::temp_dir().join(format!(
            "openatc-unresolved-object-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pack = root.join("Global Scenery/X-Plane 12 Global Scenery");
        let write = |latitude: i32, longitude: i32, bytes: Vec<u8>| {
            let path = pack
                .join("Earth nav data")
                .join(format!(
                    "{:+03}{:+04}",
                    latitude.div_euclid(10) * 10,
                    longitude.div_euclid(10) * 10
                ))
                .join(format!("{latitude:+03}{longitude:+04}.dsf"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        let object_tile = |longitude: f64| {
            let mut tile = mesh_tile(0, 0, 100.0, false, false);
            append(
                &mut tile,
                *b"DEFN",
                &atom(*b"OBJT", b"missing.obj\0".to_vec()),
            );
            let mut pool = 1u32.to_le_bytes().to_vec();
            pool.push(3);
            for value in [(longitude * 65535.0).round() as u16, 32768u16, 0] {
                pool.push(0);
                pool.extend(value.to_le_bytes());
            }
            let mut scale = Vec::new();
            for factor in [1.0f32, 1.0, 0.0] {
                scale.extend(factor.to_le_bytes());
                scale.extend(0.0f32.to_le_bytes());
            }
            let mut geodata = atom(*b"POOL", pool);
            geodata.extend(atom(*b"SCAL", scale));
            append(&mut tile, *b"GEOD", &geodata);
            append(&mut tile, *b"CMDS", &[1, 1, 0, 3, 0, 7, 0, 0]);
            tile
        };
        write(0, 0, object_tile(0.5));
        write(0, 1, mesh_tile(1, 0, 100.0, false, false));
        let from = Waypoint {
            latitude: 0.5,
            longitude: 1.00005,
            altitude_feet: 360.0,
        };
        let to = Waypoint {
            longitude: 1.00015,
            ..from
        };
        let mut terrain = Terrain::new(&root).unwrap();
        assert!(terrain.obstacles_clear(from, to, 0.0).unwrap());
        assert!(
            !terrain.has_unresolved_obstacles(),
            "A missing object far from the corridor must not refuse it"
        );
        write(0, 0, object_tile(0.99995));
        let mut terrain = Terrain::new(&root).unwrap();
        assert!(terrain.obstacles_clear(from, to, 0.0).unwrap());
        assert!(
            terrain.has_unresolved_obstacles(),
            "A missing object beside the corridor must still refuse it"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn surface_tiles_follow_the_airport_graph_across_degree_boundaries() {
        let mut airport = openatc_core::airport::Airport {
            reference_latitude: 0.5,
            reference_longitude: 0.5,
            ..Default::default()
        };
        assert_eq!(airport_tiles(&airport).unwrap(), vec![(0, 0)]);
        airport.nodes.push(openatc_core::airport::TaxiNode {
            id: 1,
            point: openatc_core::state::Point {
                north: 66792.0,
                ..Default::default()
            },
        });
        assert_eq!(airport_tiles(&airport).unwrap(), vec![(0, 0), (1, 0)]);
        airport.nodes[0].point.east = f64::NAN;
        assert!(airport_tiles(&airport).is_err());
        airport.nodes.clear();
        airport.reference_longitude = 179.999;
        assert!(airport_tiles(&airport).is_err());
    }

    #[test]
    fn supplemental_pavement_obeys_owner_filters_uv_layout_and_higher_exclusions() {
        fn property(bytes: &mut Vec<u8>, text: &[u8]) {
            let at = bytes.windows(4).position(|part| part == b"DAEH").unwrap();
            let old = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap());
            append(bytes, *b"PROP", text);
            bytes[at + 4..at + 8].copy_from_slice(&(old + text.len() as u32).to_le_bytes());
        }
        fn pavement(airport: &str, uv: bool) -> Vec<u8> {
            let mut bytes = mesh_tile(0, 0, 0.0, true, false);
            property(
                &mut bytes,
                format!("sim/filter/aptid\0{airport}\0").as_bytes(),
            );
            append(
                &mut bytes,
                *b"DEFN",
                &atom(*b"POLY", b"paved.pol\0".to_vec()),
            );
            let mut pool = 4u32.to_le_bytes().to_vec();
            let depth = if uv { 4 } else { 2 };
            pool.push(depth);
            for values in [
                [0u16, 65535, 65535, 0],
                [0u16, 0, 65535, 65535],
                [0; 4],
                [0; 4],
            ]
            .iter()
            .take(depth as usize)
            {
                pool.push(0);
                for value in values {
                    pool.extend(value.to_le_bytes());
                }
            }
            let mut scales = Vec::new();
            for _ in 0..depth {
                scales.extend(1.0f32.to_le_bytes());
                scales.extend(0.0f32.to_le_bytes());
            }
            let mut geodata = atom(*b"POOL", pool);
            geodata.extend(atom(*b"SCAL", scales));
            append(&mut bytes, *b"GEOD", &geodata);
            let parameter = if uv { u16::MAX } else { 0 };
            let mut commands = vec![1, 1, 0, 3, 0, 32, 6, 1, 0, 0, 0, 0, 0, 13];
            commands.extend(parameter.to_le_bytes());
            commands.extend([0, 0, 4, 0]);
            append(&mut bytes, *b"CMDS", &commands);
            bytes
        }
        let root = std::env::temp_dir().join(format!(
            "openatc-pavement-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let base = root.join("Global Scenery/X-Plane 12 Global Scenery");
        let relative = "Earth nav data/+00+000/+00+000.dsf";
        std::fs::create_dir_all(base.join("Earth nav data/+00+000")).unwrap();
        std::fs::write(
            base.join("paved.pol"),
            "A\n850\nDRAPED_POLYGON\nSURFACE concrete\n",
        )
        .unwrap();
        let airport = openatc_core::airport::Airport {
            icao: "TEST".into(),
            source: base
                .join("Earth nav data/apt.dat")
                .to_string_lossy()
                .into_owned(),
            reference_latitude: 0.5,
            reference_longitude: 0.5,
            ..Default::default()
        };
        for uv in [false, true] {
            std::fs::write(base.join(relative), pavement("TEST", uv)).unwrap();
            let mut selected = airport.clone();
            assert_eq!(
                Terrain::new(&root)
                    .unwrap()
                    .supplement_airport(&mut selected)
                    .unwrap(),
                1
            );
            assert!(openatc_core::airport::on_pavement(
                &selected,
                Default::default()
            ));
        }
        std::fs::write(base.join(relative), pavement("OTHER", false)).unwrap();
        assert_eq!(
            Terrain::new(&root)
                .unwrap()
                .supplement_airport(&mut airport.clone())
                .unwrap(),
            0
        );
        std::fs::write(base.join(relative), pavement("TEST", false)).unwrap();
        let mut different_owner = airport.clone();
        different_owner.source = root
            .join("Other/Earth nav data/apt.dat")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            Terrain::new(&root)
                .unwrap()
                .supplement_airport(&mut different_owner)
                .unwrap(),
            0
        );
        let higher = root.join("Custom Scenery/Higher");
        std::fs::create_dir_all(higher.join("Earth nav data/+00+000")).unwrap();
        std::fs::write(
            root.join("Custom Scenery/scenery_packs.ini"),
            "SCENERY_PACK Custom Scenery/Higher/\n",
        )
        .unwrap();
        let mut excluded = mesh_tile(0, 0, 0.0, true, false);
        property(
            &mut excluded,
            b"sim/exclude_pol\x000/0/1/1;0,0,1,0,1,1,0,1\0",
        );
        std::fs::write(higher.join(relative), excluded).unwrap();
        assert_eq!(
            Terrain::new(&root)
                .unwrap()
                .supplement_airport(&mut airport.clone())
                .unwrap(),
            0
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires installed airport scenery"]
    fn installed_memphis_pavement_covers_passenger_cargo_and_ga_stands() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let mut airport = openatc_core::airport::load_airport_from_simulator(
            &root,
            "KMEM",
            &|path| Path::new(path).is_file(),
            &|path| std::fs::read_to_string(path).map_err(|error| error.to_string()),
        )
        .unwrap();
        let terrain = Terrain::new(Path::new(&root)).unwrap();
        let before = airport
            .parking
            .iter()
            .filter(|stand| openatc_core::airport::on_pavement(&airport, stand.point))
            .count();
        let added = terrain.supplement_airport(&mut airport).unwrap();
        let after = airport
            .parking
            .iter()
            .filter(|stand| openatc_core::airport::on_pavement(&airport, stand.point))
            .count();
        eprintln!("KMEM: {added} physical polygons; covered stands {before} -> {after}");
        assert!(added > 0 && after > before);
        for prefix in ["Gate A19", "Cargo South", "Signature Flight Support"] {
            assert!(
                airport
                    .parking
                    .iter()
                    .filter(|stand| stand.name.starts_with(prefix))
                    .any(|stand| openatc_core::airport::on_pavement(&airport, stand.point)),
                "No physical coverage for {prefix}"
            );
        }
    }

    #[test]
    #[ignore = "requires installed airport scenery"]
    fn installed_memphis_draped_surfaces_retain_pavement_geometry() {
        let root = PathBuf::from(std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap());
        let pack = root.join("Global Scenery/Global Airports");
        let tile = Tile::load(&pack.join("Earth nav data/+30-090/+35-090.dsf")).unwrap();
        let terrain = Terrain::new(&root).unwrap();
        let mut resources = BTreeMap::new();
        let mut hard = 0;
        for polygon in &tile.polygons {
            if polygon.airport.as_deref() != Some("KMEM") {
                continue;
            }
            if !polygon.rings.iter().flatten().any(|point| {
                (35.0..35.12).contains(&point[1]) && (-90.06..-89.9).contains(&point[0])
            }) {
                continue;
            }
            let paved = *resources
                .entry(polygon.resource.clone())
                .or_insert_with(|| {
                    terrain
                        .library
                        .paved_surface(&pack, &polygon.resource)
                        .unwrap_or(false)
                });
            if paved {
                hard += 1;
            }
            eprintln!(
                "{}: airport={:?}, hard={paved}, depth={}, parameter={}, rings={}, points={}",
                polygon.resource,
                polygon.airport,
                polygon.depth,
                polygon.parameter,
                polygon.rings.len(),
                polygon.rings.iter().map(Vec::len).sum::<usize>()
            );
        }
        assert!(hard > 0, "No verified physical draped pavement at Memphis");
    }

    #[test]
    fn draped_resource_names_require_actual_ground_only_library_variants() {
        let root = std::env::temp_dir().join(format!(
            "openatc-draped-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pack = root.join("Global Scenery/X-Plane 12 Global Scenery");
        let path = pack.join("Earth nav data/+00+000/+00+000.dsf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = mesh_tile(0, 0, 100.0, false, true);
        append(
            &mut bytes,
            *b"DEFN",
            &atom(*b"POLY", b"paint.pol\0".to_vec()),
        );
        std::fs::write(path, bytes).unwrap();
        std::fs::write(
            pack.join("paint.pol"),
            "A\n850\nDRAPED_POLYGON\nTEXTURE paint.png\n",
        )
        .unwrap();
        let from = Waypoint {
            latitude: 0.5,
            longitude: 0.1,
            altitude_feet: 500.0,
        };
        let to = Waypoint {
            longitude: 0.9,
            ..from
        };
        let mut terrain = Terrain::new(&root).unwrap();
        assert!(terrain.terrain_clear(from, to, 0.0).unwrap());
        assert!(
            !terrain.has_unresolved_obstacles(),
            "Paint does not create a vertical obstacle"
        );
        let library = root.join("Custom Scenery/Redirect");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(
            root.join("Custom Scenery/scenery_packs.ini"),
            "SCENERY_PACK Custom Scenery/Redirect/\n",
        )
        .unwrap();
        std::fs::write(library.join("library.txt"), "A\n1200\nLIBRARY\nEXPORT_SEASON spr,sum,fal paint.pol plain.pol\nEXPORT_SEASON win paint.pol replacement.obj\n").unwrap();
        std::fs::write(library.join("plain.pol"), "I\n850\nDRAPED_POLYGON\n").unwrap();
        std::fs::write(
            library.join("replacement.obj"),
            "I\n800\nOBJ\nPOINT_COUNTS 0 0 0 0\n",
        )
        .unwrap();
        let mut redirected = Terrain::new(&root).unwrap();
        assert!(redirected.terrain_clear(from, to, 0.0).unwrap());
        assert!(
            redirected.has_unresolved_obstacles(),
            "The virtual extension cannot hide another resource type"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_local_object_placement_checks_height_rotation_and_aircraft_width() {
        let root = std::env::temp_dir().join(format!(
            "openatc-object-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pack = root.join("Global Scenery/X-Plane 12 Global Scenery");
        let path = pack.join("Earth nav data/+00+000/+00+000.dsf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = mesh_tile(0, 0, 100.0, false, false);
        append(
            &mut bytes,
            *b"DEFN",
            &atom(*b"OBJT", b"tower.obj\0".to_vec()),
        );
        let mut pool = 1u32.to_le_bytes().to_vec();
        pool.push(3);
        for value in [32768u16, 32768, 90] {
            pool.push(0);
            pool.extend(value.to_le_bytes());
        }
        let mut scale = Vec::new();
        for factor in [1.0f32, 1.0, 0.0] {
            scale.extend(factor.to_le_bytes());
            scale.extend(0.0f32.to_le_bytes());
        }
        let mut geodata = atom(*b"POOL", pool);
        geodata.extend(atom(*b"SCAL", scale));
        append(&mut bytes, *b"GEOD", &geodata);
        append(&mut bytes, *b"CMDS", &[1, 1, 0, 3, 0, 7, 0, 0]);
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(pack.join("tower.obj"), "I\n800\nOBJ\nPOINT_COUNTS 3 0 0 3\nVT -2 0 -20 0 1 0 0 0\nVT 2 12 -20 0 1 0 0 0\nVT 0 0 20 0 1 0 0 0\nIDX 0\nIDX 1\nIDX 2\nTRIS 0 3\n").unwrap();
        let mut terrain = Terrain::new(&root).unwrap();
        let from = Waypoint {
            latitude: 32768.0 / 65535.0,
            longitude: 0.4995,
            altitude_feet: 360.0,
        };
        let to = Waypoint {
            longitude: 0.5005,
            ..from
        };
        assert!(terrain.terrain_clear(from, to, 0.0).unwrap());
        assert_eq!(terrain.tile(0, 0).unwrap()[0].obstacles.len(), 1);
        assert!(!terrain.obstacles_clear(from, to, 0.0).unwrap());
        assert!(
            terrain
                .obstacles_clear(
                    Waypoint {
                        altitude_feet: 400.0,
                        ..from
                    },
                    Waypoint {
                        altitude_feet: 400.0,
                        ..to
                    },
                    0.0
                )
                .unwrap()
        );
        let shifted = |p: Waypoint| Waypoint {
            latitude: p.latitude + 12.0 / 111_320.0,
            ..p
        };
        assert!(
            terrain
                .obstacles_clear(shifted(from), shifted(to), 0.0)
                .unwrap()
        );
        assert!(
            !terrain
                .obstacles_clear(shifted(from), shifted(to), 15.0)
                .unwrap()
        );
        assert!(
            !terrain.has_unresolved_obstacles(),
            "A tile whose object placements all resolve must not be marked unresolved"
        );
        let library_pack = root.join("Custom Scenery/Variants");
        std::fs::create_dir_all(&library_pack).unwrap();
        std::fs::write(
            root.join("Custom Scenery/scenery_packs.ini"),
            "SCENERY_PACK Custom Scenery/Variants/\n",
        )
        .unwrap();
        std::fs::write(library_pack.join("library.txt"), "I\n1200\nLIBRARY\nEXPORT_SEASON win tower.obj tall.obj\nEXPORT_RATIO_SEASON spr,sum,fal 1 tower.obj tall.obj\n").unwrap();
        let tower = std::fs::read_to_string(pack.join("tower.obj")).unwrap();
        std::fs::write(
            library_pack.join("tall.obj"),
            tower.replace("2 12 -20", "2 35 -20"),
        )
        .unwrap();
        let mut variants = Terrain::new(&root).unwrap();
        assert!(
            !variants
                .obstacles_clear(
                    Waypoint {
                        altitude_feet: 400.0,
                        ..from
                    },
                    Waypoint {
                        altitude_feet: 400.0,
                        ..to
                    },
                    0.0
                )
                .unwrap(),
            "The taller library variant must not be replaced by the smaller local object"
        );
        assert!(
            variants
                .obstacles_clear(
                    Waypoint {
                        altitude_feet: 450.0,
                        ..from
                    },
                    Waypoint {
                        altitude_feet: 450.0,
                        ..to
                    },
                    0.0
                )
                .unwrap()
        );
        // One unloadable placement is enough to keep a tile unresolved even when its other
        // placements resolve and produce real obstacles.
        std::fs::write(
            pack.join("tower.obj"),
            "I\n800\nOBJ\nPOINT_COUNTS 3 0 0 3\nVT -2 0 -20 0 1 0 0 0\nVT 2 12 -20 0 1 0 0 0\nVT 0 0 20 0 1 0 0 0\nIDX 0\nIDX 1\nIDX 2\nTRIS 0 3\n",
        )
        .unwrap();
        let mut mixed = bytes.clone();
        let name = b"missing.obj\0";
        let at = mixed.windows(4).position(|part| part == b"TJBO").unwrap();
        let size = u32::from_le_bytes(mixed[at + 4..at + 8].try_into().unwrap()) as usize;
        mixed.splice(at + size..at + size, name.iter().copied());
        // The definition table and its enclosing atom both grow by the added name.
        for key in [b"TJBO", b"NFED"] {
            let at = mixed.windows(4).position(|part| part == key).unwrap();
            let size = u32::from_le_bytes(mixed[at + 4..at + 8].try_into().unwrap()) as usize;
            mixed[at + 4..at + 8].copy_from_slice(&((size + name.len()) as u32).to_le_bytes());
        }
        append(&mut mixed, *b"CMDS", &[3, 1, 7, 0, 0]);
        std::fs::write(&path, &mixed).unwrap();
        let partly = Tile::load(&path).unwrap();
        assert_eq!(partly.obstacles.len(), 1);
        assert!(
            partly.unresolved_obstacles,
            "A tile with an unresolvable placement must stay unresolved"
        );
        let mut invalid = bytes.clone();
        let command = invalid.len() - 18;
        invalid[command..command + 2].copy_from_slice(&9u16.to_le_bytes());
        assert!(dsf::parse(&invalid).is_err());
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(
            pack.join("tower.obj"),
            "I\n800\nOBJ\nPOINT_COUNTS 3 0 0 3\nVT 0 0 0 0 1 0 0 0\n",
        )
        .unwrap();
        let unresolved = Tile::load(&path).unwrap();
        assert!(unresolved.obstacles.is_empty());
        assert!(unresolved.unresolved_obstacles);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_airport_ground_polygon_covers_its_catalogue_as_one_box() {
        let root = std::env::temp_dir().join(format!(
            "openatc-agp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pack = root.join("Global Scenery/X-Plane 12 Global Scenery");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("tower.obj"), "I\n800\nOBJ\nPOINT_COUNTS 3 0 0 3\nVT -2 0 -20 0 1 0 0 0\nVT 2 12 -20 0 1 0 0 0\nVT 0 0 20 0 1 0 0 0\nIDX 0\nIDX 1\nIDX 2\nTRIS 0 3\n").unwrap();
        std::fs::write(
            pack.join("cars.agp"),
            "A\n1000\nAG_POINT\nTEXTURE_WIDTH 20.0\nTEXTURE_HEIGHT 10.0\nTEXTURE_SCALE 128 128\nTILE 0 0 128 128\nOBJECT tower.obj\nOBJ_DRAPED 64 64 90 0 1 1\nANCHOR_PT 64 64\n",
        )
        .unwrap();
        let bounds = agp_geometry(&pack, None, &pack.join("cars.agp")).unwrap();
        assert_eq!(bounds.len(), 1);
        assert_eq!(bounds[0].high[1], 12.0);
        // The tower is 40 m deep, so its own reach sets the box when it sits on the anchor.
        assert!(bounds[0].low[0] <= -20.0 && bounds[0].high[0] >= 20.0);
        // A placement away from the anchor adds its texture offset to that reach.
        std::fs::write(
            pack.join("cars.agp"),
            "A\n1000\nAG_POINT\nTEXTURE_WIDTH 20.0\nTEXTURE_HEIGHT 10.0\nTEXTURE_SCALE 128 128\nTILE 0 0 128 128\nOBJECT tower.obj\nOBJ_DRAPED 0 0 90 0 1 1\nANCHOR_PT 64 64\n",
        )
        .unwrap();
        let offset = agp_geometry(&pack, None, &pack.join("cars.agp")).unwrap();
        assert!(offset[0].high[0] >= 29.0);
        assert!(offset[0].high[2] >= 24.0);
        // A catalogue without a placed tile, without extents, or without objects refuses.
        for broken in [
            "A\n1000\nAG_POINT\nTEXTURE_WIDTH 20\nTEXTURE_HEIGHT 10\nOBJECT tower.obj\n",
            "A\n1000\nAG_POINT\nTEXTURE_WIDTH 20\nTEXTURE_HEIGHT 10\nTEXTURE_SCALE 128 128\nTILE 0 0 128 128\n",
            "A\n800\nOBJ\nPOINT_COUNTS 0 0 0 0\n",
        ] {
            std::fs::write(pack.join("cars.agp"), broken).unwrap();
            assert!(agp_geometry(&pack, None, &pack.join("cars.agp")).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dsf_record_bounds_and_nested_polygon_ranges_are_checked_before_mesh_release() {
        let bytes = mesh_tile(0, 0, 200.0, false, true);
        let tile = dsf::parse(&bytes).unwrap();
        assert!(tile.unresolved_obstacles);
        assert_eq!(tile.triangles.len(), 2);
        let a = Waypoint {
            latitude: 0.5,
            longitude: 0.1,
            altitude_feet: 1000.0,
        };
        let b = Waypoint {
            longitude: 0.9,
            ..a
        };
        assert_eq!(tile.terrain_clear(a, b, 100.0), Some(true));
        for length in [0, 8, 12, bytes.len() - 1, bytes.len() / 2] {
            assert!(dsf::parse(&bytes[..length]).is_err());
        }
        let mut bad = bytes.clone();
        bad[16..20].copy_from_slice(&7u32.to_le_bytes());
        assert!(dsf::parse(&bad).is_err());
        let mut bad = bytes;
        let commands = bad.windows(4).position(|s| s == b"SDMC").unwrap() + 8;
        bad[commands] = 200;
        assert!(dsf::parse(&bad).is_err());
    }

    #[test]
    fn enabled_stack_preserves_overlay_heights_base_priority_boundaries_and_unknowns() {
        let root = std::env::temp_dir().join(format!(
            "openatc-mesh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let write = |base: &str, lat: i32, lon: i32, height, overlay, obstacles| {
            let path = root
                .join(base)
                .join("Earth nav data")
                .join(format!(
                    "{:+03}{:+04}",
                    lat.div_euclid(10) * 10,
                    lon.div_euclid(10) * 10
                ))
                .join(format!("{lat:+03}{lon:+04}.dsf"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, mesh_tile(lon, lat, height, overlay, obstacles)).unwrap();
        };
        write(
            "Global Scenery/X-Plane 12 Global Scenery",
            0,
            0,
            100.0,
            false,
            false,
        );
        write("Custom Scenery/Disabled", 0, 0, 1000.0, false, false);
        write("Custom Scenery/Overlay", 0, 0, 200.0, true, true);
        write("Custom Scenery/Base", 0, 0, 50.0, false, false);
        let config = root.join("Custom Scenery/scenery_packs.ini");
        std::fs::write(&config,"SCENERY_PACK_DISABLED Custom Scenery/Disabled/\nSCENERY_PACK Custom Scenery/Overlay/\nSCENERY_PACK Custom Scenery/Base/\n").unwrap();
        let a = Waypoint {
            latitude: 0.5,
            longitude: 0.1,
            altitude_feet: 600.0,
        };
        let b = Waypoint {
            longitude: 0.9,
            ..a
        };
        let mut terrain = Terrain::new(&root).unwrap();
        assert!(!terrain.terrain_clear(a, b, 0.0).unwrap());
        assert!(terrain.has_unresolved_obstacles());
        terrain.begin_check();
        assert!(!terrain.has_unresolved_obstacles());
        assert!(
            terrain
                .terrain_clear(
                    Waypoint {
                        altitude_feet: 1000.0,
                        ..a
                    },
                    Waypoint {
                        altitude_feet: 1000.0,
                        ..b
                    },
                    0.0
                )
                .unwrap()
        );
        assert!(terrain.has_unresolved_obstacles());
        std::fs::write(
            &config,
            "SCENERY_PACK_DISABLED Custom Scenery/Disabled/\nSCENERY_PACK Custom Scenery/Base/\n",
        )
        .unwrap();
        let mut terrain = Terrain::new(&root).unwrap();
        assert!(
            terrain
                .terrain_clear(
                    Waypoint {
                        altitude_feet: 300.0,
                        ..a
                    },
                    Waypoint {
                        altitude_feet: 300.0,
                        ..b
                    },
                    0.0
                )
                .unwrap()
        );
        assert!(!terrain.has_unresolved_obstacles());
        write(
            "Global Scenery/X-Plane 12 Global Scenery",
            0,
            1,
            50.0,
            false,
            false,
        );
        assert!(
            terrain
                .terrain_clear(
                    Waypoint {
                        longitude: 0.9999,
                        ..a
                    },
                    Waypoint {
                        longitude: 1.0001,
                        ..b
                    },
                    0.0
                )
                .unwrap()
        );
        assert!(
            terrain
                .terrain_clear(
                    Waypoint { latitude: 2.0, ..a },
                    Waypoint { latitude: 2.0, ..b },
                    0.0
                )
                .is_err()
        );
        write(
            "Global Scenery/X-Plane 12 Global Scenery",
            -1,
            -1,
            50.0,
            false,
            false,
        );
        assert!(
            terrain
                .terrain_clear(
                    Waypoint {
                        latitude: -0.5,
                        longitude: -0.9,
                        ..a
                    },
                    Waypoint {
                        latitude: -0.5,
                        longitude: -0.1,
                        ..b
                    },
                    0.0
                )
                .unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_facade_becomes_a_checked_obstacle_with_its_published_height() {
        // A terminal building roughly 100 m by 60 m, 35 m tall, near the reference point.
        let ring = vec![[0.0, 0.0], [0.0, 0.0009], [0.00054, 0.0009], [0.00054, 0.0]];
        let facade = |height| dsf::Facade {
            resource: Arc::<str>::from("buildings.fac"),
            rings: vec![ring.clone()],
            height_meters: height,
            forest: false,
        };
        let obstacle = facade_obstacle(facade(35.0)).expect("a valid facade is an obstacle");
        // The aircraft may fly over it at 3000 ft but not through it at 50 ft.
        let through = |feet| {
            obstacle.clear(
                Waypoint {
                    latitude: 0.00027,
                    longitude: -0.0005,
                    altitude_feet: feet,
                },
                Waypoint {
                    latitude: 0.00027,
                    longitude: 0.0014,
                    altitude_feet: feet,
                },
                0.0,
                0.0,
            )
        };
        assert!(
            !through(50.0),
            "a flight straight through the building was cleared"
        );
        assert!(
            through(3000.0),
            "a flight well above the building was refused"
        );
        // Height is the only thing that changes the answer, so it must come from the parameter.
        assert!(facade_obstacle(facade(0.0)).is_ok());
        // Implausible or absent geometry must refuse rather than clear the corridor.
        assert!(facade_obstacle(facade(-1.0)).is_err());
        assert!(facade_obstacle(facade(5000.0)).is_err());
        assert!(facade_obstacle(facade(f64::NAN)).is_err());
        assert!(
            facade_obstacle(dsf::Facade {
                resource: Arc::<str>::from("empty.fac"),
                rings: Vec::new(),
                height_meters: 20.0,
                forest: false,
            })
            .is_err()
        );
    }

    #[test]
    fn a_forest_checks_its_polygon_rather_than_its_bounding_box() {
        const LATITUDE: f64 = 43.0;
        const LONGITUDE: f64 = 7.0;
        let longitude = |east: f64| LONGITUDE + east / (111_320.0 * LATITUDE.to_radians().cos());
        let latitude = |north: f64| LATITUDE + north / 111_320.0;
        let ring = |points: &[(f64, f64)]| {
            points
                .iter()
                .map(|(east, north)| [latitude(*north), longitude(*east)])
                .collect::<Vec<_>>()
        };
        let forest = |rings| dsf::Facade {
            resource: Arc::<str>::from("lib/vegetation/trees/deciduous/maple_medium.for"),
            rings,
            height_meters: 12.0,
            forest: true,
        };
        let waypoint = |east: f64, north: f64, feet: f64| Waypoint {
            latitude: latitude(north),
            longitude: longitude(east),
            altitude_feet: feet,
        };

        // A C-shaped forest open to the east. Its bounding box covers the whole square, but the
        // notch between north 300 and 700 is genuinely clear.
        let concave = ring(&[
            (0.0, 0.0),
            (1000.0, 0.0),
            (1000.0, 300.0),
            (300.0, 300.0),
            (300.0, 700.0),
            (1000.0, 700.0),
            (1000.0, 1000.0),
            (0.0, 1000.0),
        ]);
        let obstacle = facade_obstacle(forest(vec![concave])).expect("a forest is an obstacle");
        assert!(
            obstacle.clear(
                waypoint(450.0, 500.0, 20.0),
                waypoint(950.0, 500.0, 20.0),
                0.0,
                0.0
            ),
            "a corridor through the concavity was refused"
        );
        assert!(
            !obstacle.clear(
                waypoint(150.0, 100.0, 20.0),
                waypoint(150.0, 900.0, 20.0),
                0.0,
                0.0
            ),
            "a corridor through the forest was cleared below the trees"
        );
        assert!(
            obstacle.clear(
                waypoint(150.0, 100.0, 200.0),
                waypoint(150.0, 900.0, 200.0),
                0.0,
                0.0
            ),
            "a corridor well above the trees was refused"
        );

        // A hole is clear, while the ground around it still obstructs.
        let outer = ring(&[(0.0, 0.0), (1000.0, 0.0), (1000.0, 1000.0), (0.0, 1000.0)]);
        let hole = ring(&[
            (300.0, 300.0),
            (300.0, 700.0),
            (700.0, 700.0),
            (700.0, 300.0),
        ]);
        let holed = facade_obstacle(forest(vec![outer.clone(), hole])).expect("a holed forest");
        assert!(
            holed.clear(
                waypoint(350.0, 500.0, 20.0),
                waypoint(650.0, 500.0, 20.0),
                0.0,
                0.0
            ),
            "a corridor through a hole was refused"
        );
        assert!(
            !holed.clear(
                waypoint(100.0, 500.0, 20.0),
                waypoint(900.0, 500.0, 20.0),
                0.0,
                0.0
            ),
            "a corridor crossing the forest around a hole was cleared"
        );

        // A footprint that cannot be nested consistently must refuse.
        assert!(
            facade_obstacle(forest(vec![
                outer.clone(),
                ring(&[
                    (300.0, 300.0),
                    (700.0, 300.0),
                    (700.0, 700.0),
                    (300.0, 700.0)
                ])
            ]))
            .is_err()
        );
        assert!(
            facade_obstacle(forest(vec![
                outer.clone(),
                ring(&[(1200.0, 1200.0), (1200.0, 1400.0), (1400.0, 1400.0)])
            ]))
            .is_err()
        );
        assert!(
            facade_obstacle(forest(vec![ring(&[
                (0.0, 0.0),
                (100.0, 100.0),
                (100.0, 0.0),
                (0.0, 100.0)
            ])]))
            .is_err()
        );

        // A single winding is the outer boundary even when shipped data winds it clockwise.
        let reversed = outer.iter().rev().copied().collect::<Vec<_>>();
        let clockwise = facade_obstacle(forest(vec![reversed])).expect("a single winding is outer");
        assert!(!clockwise.clear(
            waypoint(100.0, 500.0, 20.0),
            waypoint(900.0, 500.0, 20.0),
            0.0,
            0.0
        ));

        // A winding with no interior encloses nothing, so the placement is kept conservatively
        // as its bounding box rather than being treated as clear ground.
        let sliver = facade_obstacle(forest(vec![ring(&[(0.0, 0.0), (1000.0, 0.0)])]))
            .expect("a line winding keeps its box");
        assert!(!sliver.clear(
            waypoint(500.0, -10.0, 20.0),
            waypoint(500.0, 10.0, 20.0),
            0.0,
            0.0
        ));
        assert!(sliver.clear(
            waypoint(500.0, 100.0, 20.0),
            waypoint(500.0, 900.0, 20.0),
            0.0,
            0.0
        ));
        assert!(facade_obstacle(forest(vec![ring(&[(0.0, 0.0); 3])])).is_ok());
    }

    #[test]
    fn retained_tile_limit_evicts_old_stacks_but_never_live_borrows() {
        let root = std::env::temp_dir().join(format!(
            "openatc-retained-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let count = MAXIMUM_TILES + 1;
        for longitude in 0i32..count as i32 {
            let path = root
                .join("Global Scenery/X-Plane 12 Global Scenery/Earth nav data")
                .join(format!("+00{:+04}", longitude.div_euclid(10) * 10))
                .join(format!("+00{longitude:+04}.dsf"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, mesh_tile(longitude, 0, 100.0, false, false)).unwrap();
        }
        let mut terrain = Terrain::new(&root).unwrap();
        terrain.cache = None;
        for longitude in 0..count {
            terrain.tile(0, longitude as i32).unwrap();
        }
        assert_eq!(terrain.tiles.len(), MAXIMUM_TILES);
        assert!(!terrain.tiles.contains_key(&(0, 0)));
        assert!(terrain.retained_bytes <= 512 * 1024 * 1024);
        let held: Vec<_> = terrain
            .tiles
            .values()
            .map(|stack| stack.as_ref().unwrap().clone())
            .collect();
        assert!(terrain.tile(0, 0).is_err());
        assert_eq!(terrain.tiles.len(), MAXIMUM_TILES);
        drop(held);
        assert!(terrain.tile(0, 0).is_ok());
        assert_eq!(terrain.tiles.len(), MAXIMUM_TILES);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terrain_maximum_clips_candidate_triangles_to_the_footprint() {
        let mut tile = dsf::parse(&mesh_tile(0, 0, 0.0, false, false)).unwrap();
        tile.triangles = vec![[
            Vertex {
                longitude: 0.0,
                latitude: 0.0,
                meters: 0.0,
            },
            Vertex {
                longitude: 1.0,
                latitude: 0.0,
                meters: 100.0,
            },
            Vertex {
                longitude: 0.0,
                latitude: 1.0,
                meters: 100.0,
            },
        ]];
        tile.index().unwrap();
        let maximum = mesh_maximum(
            tile.bounds,
            &tile.triangles,
            &tile.bins,
            [0.1, 0.2, 0.1, 0.2],
        );
        assert!((maximum.unwrap() - 40.0).abs() < 1e-8);
        assert_eq!(
            mesh_maximum(
                tile.bounds,
                &tile.triangles,
                &tile.bins,
                [0.8, 0.9, 0.8, 0.9]
            ),
            None
        );
    }

    #[test]
    fn autogen_joins_interleaved_contours_and_preserves_holes() {
        let contours = vec![
            vec![[0.0, 0.0], [0.0, 1.0]],
            vec![[0.3, 0.3], [0.7, 0.3], [0.7, 0.7]],
            vec![[1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
            vec![[0.7, 0.7], [0.3, 0.7], [0.3, 0.3]],
            vec![[0.0, 1.0], [1.0, 1.0]],
        ];
        let footprint = Footprint::autogen(&contours, [0.0, 0.0], 111_320.0).unwrap();
        assert!(
            footprint
                .blocked([11_132.0, -11_132.0], [22_264.0, -22_264.0], 1.0)
                .is_some()
        );
        assert!(
            footprint
                .blocked([44_528.0, -44_528.0], [66_792.0, -66_792.0], 1.0)
                .is_none()
        );
        assert!(Footprint::autogen(&contours[..4], [0.0, 0.0], 111_320.0).is_none());
        let mut ambiguous = contours.clone();
        ambiguous.push(contours[0].clone());
        assert!(Footprint::autogen(&ambiguous, [0.0, 0.0], 111_320.0).is_none());
    }

    #[test]
    fn autogen_top_covers_an_interior_ridge_above_its_anchor() {
        let mut tile = dsf::parse(&mesh_tile(0, 0, 0.0, false, false)).unwrap();
        let vertex = |longitude, latitude, meters| Vertex {
            longitude,
            latitude,
            meters,
        };
        let mut triangles = Vec::new();
        for (left, right, height) in [(0.0, 0.2, 10.0), (0.2, 1.0, 100.0)] {
            triangles.push([
                vertex(left, 0.0, height),
                vertex(right, 0.0, height),
                vertex(right, 1.0, height),
            ]);
            triangles.push([
                vertex(left, 0.0, height),
                vertex(right, 1.0, height),
                vertex(left, 1.0, height),
            ]);
        }
        tile.triangles = triangles;
        tile.index().unwrap();
        tile.strings.push(dsf::StringPolygon {
            resource: "ridge.ags".into(),
            rings: vec![vec![[0.1, 0.1], [0.1, 0.8], [0.8, 0.8], [0.8, 0.1]]],
            autogen: true,
            height_meters: 20.0,
            reach_meters: 0.0,
            ground_reach_meters: Some(50.0),
        });
        tile.strings.push(dsf::StringPolygon {
            resource: "beside-ridge.ags".into(),
            rings: vec![vec![[0.1, 0.1], [0.1, 0.199], [0.8, 0.199], [0.8, 0.1]]],
            autogen: true,
            height_meters: 20.0,
            reach_meters: 0.0,
            ground_reach_meters: None,
        });
        let triangles = std::mem::take(&mut tile.triangles);
        let bins = std::mem::take(&mut tile.bins);
        tile.resolve_strings(tile.bounds, &triangles, &bins);
        tile.index_obstacles();
        let obstacle = &tile.obstacles[0];
        assert_eq!(
            mesh_height(
                tile.bounds,
                &triangles,
                &bins,
                obstacle.placement.coordinate
            ),
            Some(10.0)
        );
        let point = |meters| Waypoint {
            latitude: 0.5,
            longitude: 0.5,
            altitude_feet: meters / 0.3048,
        };
        assert!(!obstacle.clear(point(115.0), point(115.0), 10.0, 1.0));
        assert!(obstacle.clear(point(125.0), point(125.0), 10.0, 1.0));
        // The neighbouring ridge shares a grid bin but lies entirely outside this area's
        // footprint. Trees remain obstacles; the unrelated ridge must not raise their tops.
        let beside = &tile.obstacles[1];
        let beside_point = |meters| Waypoint {
            longitude: 0.15,
            ..point(meters)
        };
        assert!(!beside.clear(beside_point(25.0), beside_point(25.0), 10.0, 1.0));
        assert!(beside.clear(beside_point(40.0), beside_point(40.0), 10.0, 1.0));
        assert!(obstacle.radius > 100_000.0);
        tile.triangles = triangles;
        tile.bins = bins;
        let root = std::env::temp_dir().join(format!(
            "openatc-autogen-local-ground-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut terrain = Terrain::new(&root).unwrap();
        terrain.tiles.insert((0, 0), Ok(vec![Arc::new(tile)]));
        assert!(
            terrain
                .obstacles_clear(beside_point(40.0), beside_point(40.0), 1.0)
                .unwrap(),
            "The distant ridge must not raise locally grounded trees"
        );
        assert!(
            !terrain
                .obstacles_clear(beside_point(25.0), beside_point(25.0), 1.0)
                .unwrap(),
            "The nearby trees still occupy their full height"
        );
        assert!(
            !terrain
                .obstacles_clear(point(115.0), point(115.0), 1.0)
                .unwrap(),
            "Local terrain under the trees still includes the ridge"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mesh_checks_a_ridge_between_endpoints_and_does_not_bridge_unknown_gaps() {
        let vertex = |longitude, latitude, meters| Vertex {
            longitude,
            latitude,
            meters,
        };
        let mut tile = Tile {
            bounds: [0.0, 1.0, 0.0, 1.0],
            overlay: false,
            unresolved_obstacles: false,
            unresolved_classes: BTreeSet::new(),
            unresolved_objects: Vec::new(),
            unresolved_objects_overflow: false,
            objects: Vec::new(),
            obstacles: Vec::new(),
            obstacle_bins: Vec::new(),
            draped_resources: BTreeSet::new(),
            polygons: Vec::new(),
            facades: Vec::new(),
            strings: Vec::new(),
            networks: Vec::new(),
            polygon_exclusions: Vec::new(),
            triangles: vec![
                [
                    vertex(0.0, 0.0, 0.0),
                    vertex(0.5, 0.0, 200.0),
                    vertex(0.5, 1.0, 200.0),
                ],
                [
                    vertex(0.0, 0.0, 0.0),
                    vertex(0.5, 1.0, 200.0),
                    vertex(0.0, 1.0, 0.0),
                ],
                [
                    vertex(0.5, 0.0, 200.0),
                    vertex(1.0, 0.0, 0.0),
                    vertex(1.0, 1.0, 0.0),
                ],
                [
                    vertex(0.5, 0.0, 200.0),
                    vertex(1.0, 1.0, 0.0),
                    vertex(0.5, 1.0, 200.0),
                ],
            ],
            bins: Vec::new(),
        };
        tile.index().unwrap();
        let point = |longitude, altitude_feet| Waypoint {
            latitude: 0.5,
            longitude,
            altitude_feet,
        };
        assert_eq!(
            tile.terrain_clear(point(0.1, 300.0), point(0.9, 300.0), 0.0),
            Some(false)
        );
        assert_eq!(
            tile.terrain_clear(point(0.1, 1000.0), point(0.9, 1000.0), 100.0),
            Some(true)
        );
        assert_eq!(
            tile.terrain_clear(point(0.1, 700.0), point(0.9, 700.0), 100.0),
            Some(false)
        );
        tile.triangles.truncate(2);
        tile.index().unwrap();
        assert_eq!(
            tile.terrain_clear(point(0.1, 1000.0), point(0.9, 1000.0), 0.0),
            None
        );
        assert_eq!(
            tile.terrain_clear(point(0.1, 1000.0), point(1.1, 1000.0), 0.0),
            None
        );
        assert_eq!(
            tile.terrain_clear(point(0.1, f64::NAN), point(0.2, 1000.0), 0.0),
            None
        );
    }

    #[test]
    #[ignore = "requires installed licensed X-Plane scenery"]
    fn installed_enabled_nice_stack_resolves_and_refuses_a_low_pass() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let mut terrain = Terrain::new(Path::new(&root)).unwrap();
        let a = Waypoint {
            latitude: 43.656,
            longitude: 7.2,
            altitude_feet: 10000.0,
        };
        let b = Waypoint {
            longitude: 7.21,
            ..a
        };
        assert!(terrain.terrain_clear(a, b, 1000.0).unwrap());
        assert!(
            !terrain.has_unresolved_obstacles(),
            "the enabled Nice stack resolves every obstacle class in this corridor"
        );
        assert!(
            !terrain
                .terrain_clear(
                    Waypoint {
                        altitude_feet: -100.0,
                        ..a
                    },
                    Waypoint {
                        altitude_feet: -100.0,
                        ..b
                    },
                    0.0
                )
                .unwrap()
        );
    }

    #[test]
    #[ignore = "requires installed licensed X-Plane scenery"]
    fn installed_nice_mesh_decodes_on_the_planning_worker() {
        let root = std::env::var("OPENATC_TEST_SIMULATOR_ROOT").unwrap();
        let tile =
            Tile::load(&Path::new(&root).join(
                "Global Scenery/X-Plane 12 Global Scenery/Earth nav data/+40+000/+43+007.dsf",
            ))
            .unwrap();
        assert_eq!(tile.bounds, [7.0, 8.0, 43.0, 44.0]);
        assert!(!tile.overlay);
        assert!(!tile.triangles.is_empty());
        let a = Waypoint {
            latitude: 43.656,
            longitude: 7.2,
            altitude_feet: 10000.0,
        };
        let b = Waypoint {
            longitude: 7.21,
            ..a
        };
        assert_eq!(tile.terrain_clear(a, b, 1000.0), Some(true));
        assert_eq!(
            tile.terrain_clear(
                Waypoint {
                    altitude_feet: -100.0,
                    ..a
                },
                Waypoint {
                    altitude_feet: -100.0,
                    ..b
                },
                0.0
            ),
            Some(false)
        );
    }
}
