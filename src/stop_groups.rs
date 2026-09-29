use std::collections::{BTreeMap, BTreeSet, HashMap};

const MERGE_DISTANCE_METRES: f64 = 150.0;
const EARTH_RADIUS_METRES: f64 = 6_371_000.0;

#[derive(Clone, Debug)]
pub(crate) struct Location {
    pub database_id: i64,
    pub source_id: String,
    pub name: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub location_type: u8,
    pub parent_source_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Group {
    pub source_id: String,
    pub name: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub bounds: Option<(f64, f64, f64, f64)>,
    pub member_ids: Vec<i64>,
}

pub(crate) fn derive(locations: &[Location]) -> Vec<Group> {
    let indexes: HashMap<_, _> = locations
        .iter()
        .enumerate()
        .map(|(index, location)| (location.source_id.as_str(), index))
        .collect();
    let mut sets = DisjointSets::new(locations.len());
    for (index, location) in locations.iter().enumerate() {
        if let Some(parent) = location
            .parent_source_id
            .as_deref()
            .and_then(|parent| indexes.get(parent))
        {
            sets.join(index, *parent);
        }
    }

    // Merge independently published representations when an exact normalized
    // name occurs within walking distance. Explicit GTFS parent relationships
    // have already formed components, so every member name acts as an alias.
    let components = components(locations, &mut sets);
    let mut nearby: HashMap<(String, i32, i32), Vec<usize>> = HashMap::new();
    for (component_index, component) in components.iter().enumerate() {
        let Some((latitude, longitude)) = component.position else {
            continue;
        };
        let cell_lat = (latitude / 0.0015).floor() as i32;
        let cell_lon = (longitude / 0.0025).floor() as i32;
        for name in &component.names {
            for latitude_offset in -1..=1 {
                for longitude_offset in -1..=1 {
                    let key = (
                        name.clone(),
                        cell_lat + latitude_offset,
                        cell_lon + longitude_offset,
                    );
                    if let Some(candidates) = nearby.get(&key) {
                        for &candidate in candidates {
                            let other = components[candidate].position.expect("indexed position");
                            if haversine_metres(latitude, longitude, other.0, other.1)
                                <= MERGE_DISTANCE_METRES
                            {
                                sets.join(
                                    component.first_member,
                                    components[candidate].first_member,
                                );
                            }
                        }
                    }
                }
            }
            nearby
                .entry((name.clone(), cell_lat, cell_lon))
                .or_default()
                .push(component_index);
        }
    }

    let mut grouped: BTreeMap<usize, Vec<&Location>> = BTreeMap::new();
    for (index, location) in locations.iter().enumerate() {
        grouped.entry(sets.root(index)).or_default().push(location);
    }
    let mut groups: Vec<_> = grouped.into_values().map(build_group).collect();
    groups.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    groups
}

struct Component {
    first_member: usize,
    names: BTreeSet<String>,
    position: Option<(f64, f64)>,
}

fn components(locations: &[Location], sets: &mut DisjointSets) -> Vec<Component> {
    let mut grouped: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..locations.len() {
        grouped.entry(sets.root(index)).or_default().push(index);
    }
    grouped
        .into_values()
        .map(|members| {
            let names = members
                .iter()
                .map(|&index| normalize_name(&locations[index].name))
                .collect();
            let position = average_position(members.iter().map(|&index| &locations[index]));
            Component {
                first_member: members[0],
                names,
                position,
            }
        })
        .collect()
}

fn build_group(mut members: Vec<&Location>) -> Group {
    members.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    let preferred = members
        .iter()
        .filter(|location| location.location_type == 1)
        .min_by_key(|location| &location.source_id)
        .copied()
        .unwrap_or(members[0]);
    let position_members: Vec<_> = members
        .iter()
        .copied()
        .filter(|location| location.location_type == 1 && location.latitude.is_some())
        .collect();
    let position = if position_members.is_empty() {
        average_position(members.iter().copied())
    } else {
        average_position(position_members.into_iter())
    };
    let coordinates: Vec<_> = members
        .iter()
        .filter_map(|location| Some((location.latitude?, location.longitude?)))
        .collect();
    let bounds = (!coordinates.is_empty()).then(|| {
        coordinates.iter().fold(
            (90.0_f64, 180.0_f64, -90.0_f64, -180.0_f64),
            |(min_lat, min_lon, max_lat, max_lon), &(lat, lon)| {
                (
                    min_lat.min(lat),
                    min_lon.min(lon),
                    max_lat.max(lat),
                    max_lon.max(lon),
                )
            },
        )
    });
    Group {
        source_id: format!("group:{}", preferred.source_id),
        name: preferred.name.clone(),
        latitude: position.map(|position| position.0),
        longitude: position.map(|position| position.1),
        bounds,
        member_ids: members
            .iter()
            .map(|location| location.database_id)
            .collect(),
    }
}

fn average_position<'a>(locations: impl Iterator<Item = &'a Location>) -> Option<(f64, f64)> {
    let positions: Vec<_> = locations
        .filter_map(|location| Some((location.latitude?, location.longitude?)))
        .collect();
    (!positions.is_empty()).then(|| {
        let count = positions.len() as f64;
        let (latitude, longitude) = positions
            .into_iter()
            .fold((0.0, 0.0), |sum, point| (sum.0 + point.0, sum.1 + point.1));
        (latitude / count, longitude / count)
    })
}

fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn haversine_metres(from_lat: f64, from_lon: f64, to_lat: f64, to_lon: f64) -> f64 {
    let latitude_delta = (to_lat - from_lat).to_radians();
    let longitude_delta = (to_lon - from_lon).to_radians();
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let a = (latitude_delta / 2.0).sin().powi(2)
        + from_lat.cos() * to_lat.cos() * (longitude_delta / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_METRES * a.sqrt().asin()
}

struct DisjointSets(Vec<usize>);

impl DisjointSets {
    fn new(length: usize) -> Self {
        Self((0..length).collect())
    }
    fn root(&mut self, index: usize) -> usize {
        if self.0[index] != index {
            self.0[index] = self.root(self.0[index]);
        }
        self.0[index]
    }
    fn join(&mut self, left: usize, right: usize) {
        let left = self.root(left);
        let right = self.root(right);
        if left != right {
            self.0[right] = left;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(
        id: i64,
        source_id: &str,
        name: &str,
        latitude: f64,
        longitude: f64,
        location_type: u8,
        parent: Option<&str>,
    ) -> Location {
        Location {
            database_id: id,
            source_id: source_id.to_owned(),
            name: name.to_owned(),
            latitude: Some(latitude),
            longitude: Some(longitude),
            location_type,
            parent_source_id: parent.map(str::to_owned),
        }
    }

    #[test]
    fn combines_parented_and_nearby_unparented_locations() {
        let groups = derive(&[
            location(1, "station", "Utrecht, Testlaan", 52.1, 5.1, 1, None),
            location(
                2,
                "platform-a",
                "Utrecht, Testlaan",
                52.1001,
                5.1,
                0,
                Some("station"),
            ),
            location(3, "platform-b", "Utrecht, Testlaan", 52.1002, 5.1, 0, None),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].source_id, "group:station");
        assert_eq!(groups[0].member_ids, vec![2, 3, 1]);
        assert_eq!(groups[0].latitude, Some(52.1));
        assert_eq!(groups[0].bounds, Some((52.1, 5.1, 52.1002, 5.1)));
    }

    #[test]
    fn retains_singletons_and_does_not_merge_distant_namesakes() {
        let groups = derive(&[
            location(1, "north", "Centrum", 52.0, 5.0, 0, None),
            location(2, "south", "Centrum", 51.0, 5.0, 0, None),
            location(3, "unique", "Dorpsstraat", 52.2, 5.2, 0, None),
        ]);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|group| group.member_ids.len() == 1));
    }
}
