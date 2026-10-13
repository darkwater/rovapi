mod db;

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::mpsc::{Receiver, SyncSender, TrySendError},
    time::{Duration, Instant},
};

use db::{Data, Query, Request, ResultMessage, RouteRow, ShapeSummary, Slot, TripDetail, TripRow};
use eframe::egui::{self, Color32};
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate};
use rovapi::Config;
use rovapi_models::{ScheduledStopCall, ShapePoint};
use walkers::{
    HttpTiles, Map, MapMemory, Plugin, Projector, Tiles, lon_lat, sources::OpenStreetMap,
};

const APP_ID: &str = "red.dark.rovapi-explorer";

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    let path = match (args.next(), args.next(), args.next()) {
        (None, None, None) => None,
        (Some(flag), Some(path), None) if flag == "--database" => Some(path),
        _ => {
            eprintln!("Usage: rovapi-explorer [--database PATH]");
            std::process::exit(2);
        }
    };
    let (path, startup_error) = match path {
        Some(path) => (Some(path), None),
        None => match db::active_database_path(&Config::data_directory_from_env()) {
            Ok(path) => (path.map(|path| path.to_string_lossy().into_owned()), None),
            Err(error) => (None, Some(error)),
        },
    };
    eframe::run_native(
        "ROVAPI Explorer",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_app_id(APP_ID)
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([900.0, 600.0]),
            window_builder: Some(Box::new(|builder| builder.with_app_id(APP_ID))),
            ..Default::default()
        },
        Box::new(move |cc| {
            let mut explorer = Explorer::new(cc.egui_ctx.clone(), path.clone());
            explorer.error = startup_error.clone();
            Ok(Box::new(explorer))
        }),
    )
}

struct Explorer {
    sender: SyncSender<Request>,
    receiver: Receiver<ResultMessage>,
    pending: VecDeque<Request>,
    session: u64,
    committed_session: u64,
    generation: u64,
    next_sequence: u64,
    latest: HashMap<Slot, u64>,
    outstanding: HashSet<Slot>,
    path_input: String,
    opened_path: Option<PathBuf>,
    version: Option<String>,
    feed: Option<String>,
    error: Option<String>,
    filter: String,
    filter_changed: Option<Instant>,
    routes: Vec<RouteRow>,
    routes_more: bool,
    route_cursors: Vec<i64>,
    route_page: usize,
    route: Option<RouteRow>,
    shapes: Vec<ShapeSummary>,
    enabled_shapes: BTreeMap<String, Vec<ShapePoint>>,
    trips: Vec<TripRow>,
    trips_more: bool,
    trip_cursors: Vec<i64>,
    trip_page: usize,
    trip: Option<TripDetail>,
    selected_call: Option<usize>,
    reveal_call: bool,
    map: MapMemory,
    tiles: HttpTiles,
}

impl Explorer {
    fn new(ctx: egui::Context, path: Option<String>) -> Self {
        let (sender, receiver) = db::start(ctx.clone());
        let mut app = Self {
            sender,
            receiver,
            pending: VecDeque::new(),
            session: 0,
            committed_session: 0,
            generation: 0,
            next_sequence: 0,
            latest: HashMap::new(),
            outstanding: HashSet::new(),
            path_input: path.clone().unwrap_or_default(),
            opened_path: None,
            version: None,
            feed: None,
            error: None,
            filter: String::new(),
            filter_changed: None,
            routes: Vec::new(),
            routes_more: false,
            route_cursors: vec![0],
            route_page: 0,
            route: None,
            shapes: Vec::new(),
            enabled_shapes: BTreeMap::new(),
            trips: Vec::new(),
            trips_more: false,
            trip_cursors: vec![0],
            trip_page: 0,
            trip: None,
            selected_call: None,
            reveal_call: false,
            map: MapMemory::default(),
            tiles: HttpTiles::new(OpenStreetMap, ctx),
        };
        if path.is_some() {
            app.open();
        }
        app
    }

    fn enqueue(&mut self, query: Query) {
        self.next_sequence += 1;
        let slot = query.slot();
        self.latest.insert(slot.clone(), self.next_sequence);
        self.outstanding.insert(slot);
        self.pending.push_back(Request {
            session: self.session,
            generation: self.generation,
            sequence: self.next_sequence,
            query,
        });
    }

    fn flush(&mut self) {
        while let Some(request) = self.pending.pop_front() {
            match self.sender.try_send(request) {
                Ok(()) => {}
                Err(TrySendError::Full(request)) => {
                    self.pending.push_front(request);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.error = Some("Database worker stopped".into());
                    self.outstanding.clear();
                    break;
                }
            }
        }
    }

    fn open(&mut self) {
        let path = self.path_input.trim();
        if path.is_empty() {
            self.error = Some("Enter a database path".into());
            return;
        }
        self.session += 1;
        self.generation += 1;
        self.pending.clear();
        self.latest.clear();
        self.outstanding.clear();
        self.error = None;
        self.enqueue(Query::Open(PathBuf::from(path)));
    }

    fn fetch_routes(&mut self) {
        let after = self.route_cursors[self.route_page];
        self.routes.clear();
        self.routes_more = false;
        self.enqueue(Query::Routes {
            filter: self.filter.clone(),
            after,
        });
    }

    fn select_route(&mut self, source_id: String) {
        self.generation += 1;
        self.pending.clear();
        self.latest.clear();
        self.outstanding.clear();
        self.route = None;
        self.shapes.clear();
        self.enabled_shapes.clear();
        self.trips.clear();
        self.trip = None;
        self.trip_page = 0;
        self.trip_cursors = vec![0];
        self.selected_call = None;
        self.reveal_call = false;
        self.enqueue(Query::Route { source_id });
    }

    fn select_trip(&mut self, source_id: String) {
        self.pending
            .retain(|request| !matches!(request.query, Query::Trip { .. }));
        self.trip = None;
        self.selected_call = None;
        self.reveal_call = false;
        self.enqueue(Query::Trip { source_id });
    }

    fn receive(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            let is_open = matches!(&message.result, Ok(Data::Open { .. }));
            if message.session != self.session
                || message.generation != self.generation
                || self.latest.get(&message.slot) != Some(&message.sequence)
            {
                continue;
            }
            self.outstanding.remove(&message.slot);
            match message.result {
                Err(error) => {
                    self.error = Some(error);
                    if self.committed_session != self.session {
                        self.session = self.committed_session;
                    }
                }
                Ok(Data::Open {
                    path,
                    version,
                    feed,
                }) => {
                    self.committed_session = self.session;
                    self.opened_path = Some(path);
                    self.version = version;
                    self.feed = feed;
                    self.route = None;
                    self.trip = None;
                    self.routes.clear();
                    self.trips.clear();
                    self.shapes.clear();
                    self.enabled_shapes.clear();
                    self.filter.clear();
                    self.route_page = 0;
                    self.route_cursors = vec![0];
                    self.trip_page = 0;
                    self.trip_cursors = vec![0];
                    self.error = None;
                    self.fetch_routes();
                }
                Ok(Data::Routes { rows, has_more }) => {
                    self.routes = rows;
                    self.routes_more = has_more;
                }
                Ok(Data::Route { row, shapes }) => {
                    self.route = Some(row.clone());
                    self.shapes = shapes;
                    self.enqueue(Query::Trips {
                        route_id: row.id,
                        after: 0,
                    });
                    if let Some(first) = self.shapes.first() {
                        self.enqueue(Query::Shape {
                            id: first.id.clone(),
                        });
                    }
                }
                Ok(Data::Trips { rows, has_more }) => {
                    self.trips = rows;
                    self.trips_more = has_more;
                }
                Ok(Data::Shape { id, points }) => {
                    self.fit_points(&points, &[]);
                    self.enabled_shapes.insert(id, points);
                }
                Ok(Data::Trip(detail)) => {
                    self.fit_points(&detail.shape, &detail.calls);
                    self.trip = Some(detail);
                }
            }
            if is_open {
                break;
            }
        }
    }

    fn fit_points(&mut self, points: &[ShapePoint], calls: &[ScheduledStopCall]) {
        let positions = points.iter().map(|p| (p.longitude, p.latitude)).chain(
            calls
                .iter()
                .filter_map(|c| Some((c.stop.longitude?, c.stop.latitude?))),
        );
        let mut bounds = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (lon, lat) in positions {
            bounds.0 = bounds.0.min(lon);
            bounds.1 = bounds.1.min(lat);
            bounds.2 = bounds.2.max(lon);
            bounds.3 = bounds.3.max(lat);
        }
        if bounds.0 == f64::MAX {
            return;
        }
        let lat = (bounds.1 + bounds.3) / 2.0;
        self.map
            .center_at(lon_lat((bounds.0 + bounds.2) / 2.0, lat));
        let width = (bounds.2 - bounds.0).max(0.005) * lat.to_radians().cos();
        let height = (bounds.3 - bounds.1).max(0.005);
        let span = width.max(height);
        let zoom = (360.0 / span).log2() + 0.3;
        let _ = self.map.set_zoom(zoom.clamp(3.0, 17.0));
    }

    fn route_table(&mut self, ui: &mut egui::Ui) {
        let rows: Vec<Vec<String>> = self
            .routes
            .iter()
            .map(|r| {
                vec![
                    r.id.to_string(),
                    r.route.source_id.to_string(),
                    format!(
                        "{} {}",
                        r.route.short_name.as_deref().unwrap_or(""),
                        r.route.long_name.as_deref().unwrap_or("")
                    ),
                ]
            })
            .collect();
        let mut delegate = Grid::new(["ID", "Source ID", "Route name"], rows);
        let height = (ui.available_height() - 38.0).max(120.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                Table::new()
                    .id_salt("routes")
                    .headers(vec![HeaderRow::new(24.0)])
                    .columns(vec![
                        Column::new(45.0),
                        Column::new(125.0),
                        Column::new(280.0),
                    ])
                    .num_rows(delegate.rows.len() as u64)
                    .show(ui, &mut delegate);
            },
        );
        if let Some(index) = delegate.clicked
            && let Some(row) = self.routes.get(index)
        {
            self.select_route(row.route.source_id.to_string());
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.route_page > 0, egui::Button::new("Previous"))
                .clicked()
            {
                self.route_page -= 1;
                self.fetch_routes();
            }
            ui.label(format!("Page {}", self.route_page + 1));
            if ui
                .add_enabled(self.routes_more, egui::Button::new("Next"))
                .clicked()
                && let Some(last) = self.routes.last()
            {
                self.route_cursors.truncate(self.route_page + 1);
                self.route_cursors.push(last.id);
                self.route_page += 1;
                self.fetch_routes();
            }
        });
    }

    fn trip_table(&mut self, ui: &mut egui::Ui) {
        let rows: Vec<Vec<String>> = self
            .trips
            .iter()
            .map(|r| {
                vec![
                    r.id.to_string(),
                    r.trip.source_id.to_string(),
                    r.trip.headsign.clone().unwrap_or_default(),
                    r.trip
                        .direction_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    r.trip.service_id.to_string(),
                    r.trip
                        .shape_id
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                ]
            })
            .collect();
        let mut delegate = Grid::new(["ID", "Trip", "Headsign", "Dir", "Service", "Shape"], rows);
        ui.push_id("trips-table", |ui| {
            Table::new()
                .id_salt("trips")
                .headers(vec![HeaderRow::new(24.0)])
                .columns(vec![
                    Column::new(55.0),
                    Column::new(150.0),
                    Column::new(180.0),
                    Column::new(40.0),
                    Column::new(100.0),
                    Column::new(120.0),
                ])
                .num_rows(delegate.rows.len() as u64)
                .show(ui, &mut delegate);
        });
        if let Some(index) = delegate.clicked
            && let Some(row) = self.trips.get(index)
        {
            self.select_trip(row.trip.source_id.to_string());
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.trip_page > 0, egui::Button::new("Previous"))
                .clicked()
            {
                self.trip_page -= 1;
                self.fetch_trips();
            }
            ui.label(format!("Page {}", self.trip_page + 1));
            if ui
                .add_enabled(self.trips_more, egui::Button::new("Next"))
                .clicked()
                && let Some(last) = self.trips.last()
            {
                self.trip_cursors.truncate(self.trip_page + 1);
                self.trip_cursors.push(last.id);
                self.trip_page += 1;
                self.fetch_trips();
            }
        });
    }

    fn fetch_trips(&mut self) {
        if let Some(route) = &self.route {
            self.trips.clear();
            self.trips_more = false;
            self.enqueue(Query::Trips {
                route_id: route.id,
                after: self.trip_cursors[self.trip_page],
            });
        }
    }

    fn detail(&mut self, ui: &mut egui::Ui) {
        let Some(route) = &self.route else {
            ui.label("Select a route to inspect it.");
            return;
        };
        ui.heading(format!(
            "Route {}",
            route
                .route
                .short_name
                .as_deref()
                .unwrap_or(route.route.source_id.as_str())
        ));
        ui.label(format!(
            "Database ID: {}   Source ID: {}",
            route.id, route.route.source_id
        ));
        if ui.button("Copy route ID").clicked() {
            ui.ctx().copy_text(route.route.source_id.to_string());
        }
        ui.label(format!(
            "Short name: {}   Long name: {}   Agency: {}   Type: {}",
            option(&route.route.short_name),
            option(&route.route.long_name),
            route
                .route
                .agency_source_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "NULL".into()),
            route.route.route_type
        ));
        ui.label(format!(
            "Color: {}   Text color: {}",
            option(&route.route.color),
            option(&route.route.text_color)
        ));
        ui.separator();
        ui.label(format!(
            "Shapes (showing up to 500; {}) — select up to 20",
            self.shapes.len()
        ));
        let shapes = self.shapes.clone();
        egui::ScrollArea::horizontal()
            .max_height(36.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for shape in shapes {
                        let active = self.enabled_shapes.contains_key(&shape.id);
                        if ui
                            .add_enabled(
                                active || self.enabled_shapes.len() < 20,
                                egui::Button::new(format!(
                                    "{} ({}) {}",
                                    shape.id,
                                    shape.trip_count,
                                    if active { "✓" } else { "+" }
                                )),
                            )
                            .clicked()
                        {
                            if active {
                                self.enabled_shapes.remove(&shape.id);
                            } else {
                                self.enqueue(Query::Shape { id: shape.id });
                            }
                        }
                    }
                })
            });
        ui.horizontal(|ui| {
            ui.heading("Trips");
            if ui.button("Fit map").clicked() {
                if let Some(trip) = &self.trip {
                    let (shape, calls) = (trip.shape.clone(), trip.calls.clone());
                    self.fit_points(&shape, &calls);
                } else if let Some(points) = self.enabled_shapes.values().next().cloned() {
                    self.fit_points(&points, &[]);
                }
            }
        });
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 180.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| self.trip_table(ui),
        );
        if let Some(trip) = &self.trip {
            ui.separator();
            ui.heading(format!("Trip {}", trip.trip.source_id));
            ui.label(format!("Database ID: {}   Route: {}   Service: {}   Headsign: {}   Short name: {}   Direction: {}   Shape: {}", trip.id, trip.trip.route_id, trip.trip.service_id, option(&trip.trip.headsign), option(&trip.trip.short_name), trip.trip.direction_id.map(|v| v.to_string()).unwrap_or_else(|| "NULL".into()), trip.trip.shape_id.as_ref().map(ToString::to_string).unwrap_or_else(|| "NULL".into())));
            if ui.button("Copy trip ID").clicked() {
                ui.ctx().copy_text(trip.trip.source_id.to_string());
            }
        }
        let map_height = 420.0;
        let overlays: Vec<&[ShapePoint]> =
            self.enabled_shapes.values().map(Vec::as_slice).collect();
        let trip_line = self.trip.as_ref().map_or(&[][..], |t| t.shape.as_slice());
        let markers = self.trip.as_ref().map_or(&[][..], |t| t.calls.as_slice());
        let center = self.map.detached().unwrap_or_else(|| lon_lat(5.12, 52.09));
        let before_map_selection = self.selected_call;
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), map_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add_sized(
                    ui.available_size(),
                    Map::new(Some(&mut self.tiles), &mut self.map, center).with_plugin(
                        TransitOverlay {
                            lines: overlays,
                            selected_line: trip_line,
                            markers,
                            selected: &mut self.selected_call,
                        },
                    ),
                );
            },
        );
        let attribution = self.tiles.attribution();
        ui.hyperlink_to(attribution.text, attribution.url);
        if self.selected_call != before_map_selection {
            self.reveal_call = true;
        }
        if let Some(trip) = &self.trip {
            ui.heading(format!("Stop calls ({})", trip.calls.len()));
            let mut delegate = CallsGrid {
                calls: &trip.calls,
                clicked: None,
                selected: self.selected_call,
            };
            let height = (trip.calls.len() as f32 * 24.0 + 32.0).clamp(90.0, 320.0);
            let reveal = if std::mem::take(&mut self.reveal_call) {
                self.selected_call
            } else {
                None
            };
            let table = ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    let mut table = Table::new()
                        .id_salt("calls")
                        .headers(vec![HeaderRow::new(24.0)])
                        .columns(vec![
                            Column::new(45.0),
                            Column::new(150.0),
                            Column::new(200.0),
                            Column::new(90.0),
                            Column::new(75.0),
                            Column::new(90.0),
                            Column::new(75.0),
                        ])
                        .num_rows(trip.calls.len() as u64);
                    if let Some(index) = reveal {
                        table = table.scroll_to_row(index as u64, Some(egui::Align::Center));
                    }
                    ui.push_id("calls-table", |ui| table.show(ui, &mut delegate));
                },
            );
            if reveal.is_some() {
                ui.scroll_to_rect(table.response.rect, Some(egui::Align::Center));
            }
            if let Some(index) = delegate.clicked {
                self.selected_call = Some(index);
            }
            if let Some(selected) = self.selected_call.and_then(|index| trip.calls.get(index)) {
                let overlap: Vec<_> = trip
                    .calls
                    .iter()
                    .enumerate()
                    .filter(|(_, call)| {
                        call.stop.latitude == selected.stop.latitude
                            && call.stop.longitude == selected.stop.longitude
                            && call.stop.latitude.is_some()
                    })
                    .collect();
                if overlap.len() > 1 {
                    egui::ComboBox::from_label("Calls at this point")
                        .selected_text(format!(
                            "{}: {}",
                            selected.stop_sequence, selected.stop.name
                        ))
                        .show_ui(ui, |ui| {
                            for (index, call) in overlap {
                                ui.selectable_value(
                                    &mut self.selected_call,
                                    Some(index),
                                    format!("{}: {}", call.stop_sequence, call.stop.name),
                                );
                            }
                        });
                }
            }
        }
    }
}

impl eframe::App for Explorer {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.receive();
        self.flush();
        if let Some(changed) = self.filter_changed {
            if changed.elapsed() >= Duration::from_millis(200) && self.opened_path.is_some() {
                self.filter_changed = None;
                self.route_page = 0;
                self.route_cursors = vec![0];
                self.fetch_routes();
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        egui::TopBottomPanel::top("database").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Schedule database:");
                ui.text_edit_singleline(&mut self.path_input);
                if ui.button("Open").clicked() {
                    self.open();
                }
                if !self.outstanding.is_empty() {
                    ui.spinner();
                }
            });
            if let Some(path) = &self.opened_path {
                ui.label(format!(
                    "{}  •  Version: {}  •  Feed: {}",
                    path.display(),
                    self.version.as_deref().unwrap_or("unknown"),
                    self.feed.as_deref().unwrap_or("unknown")
                ));
            }
            if let Some(error) = &self.error {
                ui.colored_label(Color32::RED, error);
            }
        });
        egui::SidePanel::left("routes")
            .resizable(true)
            .default_width(500.0)
            .min_width(460.0)
            .show(ctx, |ui| {
                ui.heading("Routes");
                if ui.text_edit_singleline(&mut self.filter).changed() {
                    self.filter_changed = Some(Instant::now());
                }
                if self.opened_path.is_some() {
                    self.route_table(ui);
                }
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.detail(ui));
        });
    }
}

fn option(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("NULL")
}

struct Grid<const N: usize> {
    headers: [&'static str; N],
    rows: Vec<Vec<String>>,
    clicked: Option<usize>,
    selected: Option<usize>,
}
impl<const N: usize> Grid<N> {
    fn new(headers: [&'static str; N], rows: Vec<Vec<String>>) -> Self {
        Self {
            headers,
            rows,
            clicked: None,
            selected: None,
        }
    }
}
impl<const N: usize> TableDelegate for Grid<N> {
    fn header_cell_ui(&mut self, ui: &mut egui::Ui, cell: &HeaderCellInfo) {
        ui.strong(self.headers[cell.col_range.start]);
    }
    fn cell_ui(&mut self, ui: &mut egui::Ui, cell: &CellInfo) {
        if let Some(row) = self.rows.get(cell.row_nr as usize)
            && ui
                .selectable_label(
                    self.selected == Some(cell.row_nr as usize),
                    &row[cell.col_nr],
                )
                .clicked()
        {
            self.clicked = Some(cell.row_nr as usize);
        }
    }
}

struct CallsGrid<'a> {
    calls: &'a [ScheduledStopCall],
    clicked: Option<usize>,
    selected: Option<usize>,
}

impl TableDelegate for CallsGrid<'_> {
    fn header_cell_ui(&mut self, ui: &mut egui::Ui, cell: &HeaderCellInfo) {
        const HEADERS: [&str; 7] = ["#", "Stop", "Name", "Arrival", "s", "Departure", "s"];
        ui.strong(HEADERS[cell.col_range.start]);
    }

    fn cell_ui(&mut self, ui: &mut egui::Ui, cell: &CellInfo) {
        let Some(call) = self.calls.get(cell.row_nr as usize) else {
            return;
        };
        let value: Cow<'_, str> = match cell.col_nr {
            0 => call.stop_sequence.to_string().into(),
            1 => call.stop.source_id.as_str().into(),
            2 => call.stop.name.as_str().into(),
            3 => option(&call.scheduled_arrival).into(),
            4 => call
                .scheduled_arrival_seconds
                .map(|v| v.to_string())
                .unwrap_or_else(|| "NULL".into())
                .into(),
            5 => option(&call.scheduled_departure).into(),
            6 => call
                .scheduled_departure_seconds
                .map(|v| v.to_string())
                .unwrap_or_else(|| "NULL".into())
                .into(),
            _ => return,
        };
        if ui
            .selectable_label(self.selected == Some(cell.row_nr as usize), value.as_ref())
            .clicked()
        {
            self.clicked = Some(cell.row_nr as usize);
        }
    }
}

struct TransitOverlay<'a> {
    lines: Vec<&'a [ShapePoint]>,
    selected_line: &'a [ShapePoint],
    markers: &'a [ScheduledStopCall],
    selected: &'a mut Option<usize>,
}
impl Plugin for TransitOverlay<'_> {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        response: &egui::Response,
        projector: &Projector,
        _: &MapMemory,
    ) {
        let painter = ui.painter_at(response.rect);
        for line in &self.lines {
            draw_line(&painter, projector, line, Color32::BLACK, 5.);
            draw_line(&painter, projector, line, Color32::WHITE, 1.5);
        }
        draw_line(&painter, projector, self.selected_line, Color32::BLACK, 6.);
        draw_line(
            &painter,
            projector,
            self.selected_line,
            Color32::LIGHT_BLUE,
            2.5,
        );
        let mut closest = None;
        if response.clicked()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            for (index, call) in self.markers.iter().enumerate() {
                let (Some(lon), Some(lat)) = (call.stop.longitude, call.stop.latitude) else {
                    continue;
                };
                let p = projector.project(lon_lat(lon, lat));
                let d = pointer.distance(egui::pos2(p.x, p.y));
                if d < 12.0 && closest.is_none_or(|(_, best)| d < best) {
                    closest = Some((index, d));
                }
            }
        }
        for (index, call) in self.markers.iter().enumerate() {
            let (Some(lon), Some(lat)) = (call.stop.longitude, call.stop.latitude) else {
                continue;
            };
            let p = projector.project(lon_lat(lon, lat));
            painter.circle_filled(
                egui::pos2(p.x, p.y),
                if *self.selected == Some(index) {
                    7.0
                } else {
                    4.0
                },
                if *self.selected == Some(index) {
                    Color32::YELLOW
                } else {
                    Color32::from_rgb(35, 175, 100)
                },
            );
        }
        if let Some((index, _)) = closest {
            *self.selected = Some(index);
        }
    }
}

fn draw_line(
    painter: &egui::Painter,
    projector: &Projector,
    points: &[ShapePoint],
    color: Color32,
    width: f32,
) {
    let step = (points.len() / 2_000).max(1);
    let mut sampled = points.iter().step_by(step).chain(points.last());
    let Some(mut previous) = sampled.next() else {
        return;
    };
    for point in sampled {
        let a = projector.project(lon_lat(previous.longitude, previous.latitude));
        let b = projector.project(lon_lat(point.longitude, point.latitude));
        painter.line_segment(
            [egui::pos2(a.x, a.y), egui::pos2(b.x, b.y)],
            egui::Stroke::new(width, color),
        );
        previous = point;
    }
}
