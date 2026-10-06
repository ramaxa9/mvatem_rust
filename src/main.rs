use anyhow::Result;
use eframe::egui;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use nokhwa::{
    pixel_format::RgbFormat,
    query,
    utils::{
        ApiBackend, CameraIndex, CameraInfo, FrameFormat, RequestedFormat, RequestedFormatType,
    },
    Camera,
};
use std::{
    cmp::Reverse,
    collections::HashMap,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

// --- Application State ---

#[derive(Clone)]
struct AtemCamera {
    index: CameraIndex,
    name: String,
}

enum CameraCommand {
    Refresh,
    Select(CameraIndex),
}

#[derive(Debug, Clone, PartialEq)]
enum OutputMode {
    Multiview,
    Preview,
    Program,
}

enum EthernetCommand {
    Refresh,
    SetOutputMode(String, OutputMode),
}

enum CameraEvent {
    Devices(Vec<AtemCamera>),
    Connected(String),
    Disconnected,
    Error(String),
}

enum EthernetEvent {
    Devices(Vec<String>),
    Error(String),
}

struct CameraFrame {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

const CAMERA_FRAME_INTERVAL: Duration = Duration::from_millis(16);

struct AtemApp {
    connected_camera: Option<String>,
    cameras: Vec<AtemCamera>,
    camera_status: String,
    is_searching_camera: bool,
    atem_ips: Vec<String>,
    connected_atem_ip: Option<String>,
    is_searching_atem: bool,
    camera_texture: Option<egui::TextureHandle>,
    rx_camera_event: Receiver<CameraEvent>,
    rx_frame: Receiver<CameraFrame>,
    tx_camera_command: mpsc::Sender<CameraCommand>,
    rx_ethernet_event: Receiver<EthernetEvent>,
    tx_ethernet_command: mpsc::Sender<EthernetCommand>,
}

impl AtemApp {
    fn new(
        rx_camera_event: Receiver<CameraEvent>,
        rx_frame: Receiver<CameraFrame>,
        tx_camera_command: mpsc::Sender<CameraCommand>,
        rx_ethernet_event: Receiver<EthernetEvent>,
        tx_ethernet_command: mpsc::Sender<EthernetCommand>,
    ) -> Self {
        Self {
            connected_camera: None,
            cameras: Vec::new(),
            camera_status: "Looking for a Blackmagic ATEM Mini USB camera...".to_string(),
            is_searching_camera: true,
            atem_ips: Vec::new(),
            connected_atem_ip: None,
            is_searching_atem: true,
            camera_texture: None,
            rx_camera_event,
            rx_frame,
            tx_camera_command,
            rx_ethernet_event,
            tx_ethernet_command,
        }
    }
}

// --- GUI Implementation ---

impl eframe::App for AtemApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.rx_camera_event.try_recv() {
            match event {
                CameraEvent::Devices(cameras) => {
                    self.cameras = cameras;
                    self.is_searching_camera = false;
                    self.camera_status = if self.cameras.is_empty() {
                        "Blackmagic ATEM Mini USB camera not found.".to_string()
                    } else {
                        format!("Found {} Blackmagic camera(s).", self.cameras.len())
                    };
                }
                CameraEvent::Connected(name) => {
                    self.connected_camera = Some(name);
                    self.camera_status.clear();
                }
                CameraEvent::Disconnected => {
                    self.connected_camera = None;
                    self.camera_texture = None;
                }
                CameraEvent::Error(error) => {
                    log::error!("{error}");
                    self.connected_camera = None;
                    self.camera_texture = None;
                    self.is_searching_camera = false;
                    self.camera_status = error;
                }
            }
        }

        while let Ok(event) = self.rx_ethernet_event.try_recv() {
            match event {
                EthernetEvent::Devices(ips) => {
                    if self
                        .connected_atem_ip
                        .as_ref()
                        .is_some_and(|ip| !ips.contains(ip))
                    {
                        self.connected_atem_ip = None;
                    }
                    self.atem_ips = ips;
                    self.is_searching_atem = false;
                }
                EthernetEvent::Error(error) => {
                    log::error!("{error}");
                    self.is_searching_atem = false;
                }
            }
        }

        if self.connected_camera.is_some() {
            let mut latest_frame = None;
            while let Ok(frame) = self.rx_frame.try_recv() {
                latest_frame = Some(frame);
            }
            if let Some(frame) = latest_frame {
                let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.rgb);
                if let Some(texture) = &mut self.camera_texture {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.camera_texture = Some(ctx.load_texture(
                        "atem-mini-usb-video",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_rgb(20, 20, 20));

                if let Some(texture) = &self.camera_texture {
                    let image_size = texture.size_vec2();
                    let scale = (rect.width() / image_size.x).min(rect.height() / image_size.y);
                    let display_size = image_size * scale;
                    let image_rect = egui::Rect::from_center_size(rect.center(), display_size);
                    ui.painter().image(
                        texture.id(),
                        image_rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    let text = if self.camera_status.is_empty() {
                        "Waiting for video from the ATEM Mini USB webcam".to_string()
                    } else {
                        self.camera_status.clone()
                    };
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(18.0),
                        egui::Color32::GRAY,
                    );
                }

                let indicator_rect = egui::Rect::from_min_size(
                    rect.min + egui::vec2(12.0, 12.0),
                    egui::vec2(20.0, 20.0),
                );
                if self.connected_atem_ip.is_some() {
                    ui.painter()
                        .circle_filled(indicator_rect.center(), 5.0, egui::Color32::GREEN);
                } else if self.is_searching_atem {
                    ui.allocate_ui_at_rect(indicator_rect, |ui| {
                        ui.spinner();
                    });
                }

                response.context_menu(|ui| {
                    if let Some(ip) = &self.connected_atem_ip {
                        ui.label(format!("ATEM Mini (Ethernet): {ip}"));
                        ui.separator();
                        ui.label("Switch Output Type:");
                        for (label, mode) in [
                            ("Multiview", OutputMode::Multiview),
                            ("Preview", OutputMode::Preview),
                            ("Program", OutputMode::Program),
                        ] {
                            if ui.button(label).clicked() {
                                if let Err(error) = self
                                    .tx_ethernet_command
                                    .send(EthernetCommand::SetOutputMode(ip.clone(), mode))
                                {
                                    log::error!("ATEM command worker stopped: {error}");
                                }
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                    } else {
                        ui.label("Connect to an ATEM Mini over Ethernet:");
                        if self.atem_ips.is_empty() {
                            ui.label(if self.is_searching_atem {
                                "Searching for ATEM devices..."
                            } else {
                                "No ATEM devices found."
                            });
                        }
                        for ip in &self.atem_ips {
                            if ui.button(format!("Connect to {ip}")).clicked() {
                                self.connected_atem_ip = Some(ip.clone());
                                ui.close_menu();
                            }
                        }
                    }

                    ui.separator();
                    ui.label(match &self.connected_camera {
                        Some(name) => format!("USB video: {name}"),
                        None => "USB video: no ATEM webcam connected".to_string(),
                    });
                    for device in &self.cameras {
                        if self.connected_camera.as_deref() != Some(device.name.as_str())
                            && ui
                                .button(format!("Use USB camera: {}", device.name))
                                .clicked()
                        {
                            if let Err(error) = self
                                .tx_camera_command
                                .send(CameraCommand::Select(device.index.clone()))
                            {
                                self.camera_status = format!("Camera worker stopped: {error}");
                            } else {
                                self.camera_status = format!("Opening {}...", device.name);
                            }
                            ui.close_menu();
                        }
                    }
                    if ui.button("Reconnect USB camera").clicked() {
                        self.is_searching_camera = true;
                        self.camera_status =
                            "Searching for the ATEM Mini USB webcam...".to_string();
                        if let Err(error) = self.tx_camera_command.send(CameraCommand::Refresh) {
                            self.camera_status = format!("Camera worker stopped: {error}");
                            self.is_searching_camera = false;
                        }
                        ui.close_menu();
                    }
                    if ui.button("Reconnect ATEM Ethernet search").clicked() {
                        self.connected_atem_ip = None;
                        self.atem_ips.clear();
                        self.is_searching_atem = true;
                        if let Err(error) = self.tx_ethernet_command.send(EthernetCommand::Refresh)
                        {
                            log::error!("ATEM discovery worker stopped: {error}");
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        });

        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

fn is_blackmagic_camera(info: &CameraInfo) -> bool {
    let identity = format!("{} {}", info.human_name(), info.description()).to_lowercase();
    identity.contains("blackmagic") || identity.contains("atem mini")
}

fn discover_cameras() -> anyhow::Result<Vec<AtemCamera>> {
    Ok(query(ApiBackend::MediaFoundation)?
        .into_iter()
        .filter(is_blackmagic_camera)
        .map(|info| AtemCamera {
            index: info.index().clone(),
            name: info.human_name(),
        })
        .collect())
}

fn camera_worker(
    command_rx: Receiver<CameraCommand>,
    event_tx: mpsc::Sender<CameraEvent>,
    frame_tx: SyncSender<CameraFrame>,
) {
    let mut cameras = Vec::new();
    refresh_cameras(&mut cameras, &event_tx);
    let mut camera = cameras
        .first()
        .and_then(|device| open_camera(device, &event_tx));
    let mut last_scan = Instant::now();
    let mut last_frame = Instant::now();
    let mut has_logged_frame = false;

    loop {
        match command_rx.recv_timeout(Duration::from_millis(16)) {
            Ok(CameraCommand::Refresh) => {
                drop(camera.take());
                let _ = event_tx.send(CameraEvent::Disconnected);
                refresh_cameras(&mut cameras, &event_tx);
                camera = cameras
                    .first()
                    .and_then(|device| open_camera(device, &event_tx));
                last_scan = Instant::now();
            }
            Ok(CameraCommand::Select(index)) => {
                let Some(device) = cameras.iter().find(|device| device.index == index) else {
                    let _ = event_tx.send(CameraEvent::Error(
                        "That camera is not a discovered Blackmagic device.".to_string(),
                    ));
                    continue;
                };

                drop(camera.take());
                let _ = event_tx.send(CameraEvent::Disconnected);
                camera = open_camera(device, &event_tx);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        if camera.is_none() && last_scan.elapsed() >= Duration::from_secs(2) {
            refresh_cameras(&mut cameras, &event_tx);
            camera = cameras
                .first()
                .and_then(|device| open_camera(device, &event_tx));
            last_scan = Instant::now();
        }

        if camera.is_some() && last_frame.elapsed() < CAMERA_FRAME_INTERVAL {
            thread::sleep(Duration::from_millis(5));
            continue;
        }

        if let Some(active_camera) = &mut camera {
            last_frame = Instant::now();
            let capture_started = Instant::now();
            match active_camera
                .frame()
                .and_then(|frame| frame.decode_image::<RgbFormat>())
            {
                Ok(frame) => {
                    if !has_logged_frame {
                        log::info!(
                            "Received first USB webcam frame: {}x{}",
                            frame.width(),
                            frame.height()
                        );
                        has_logged_frame = true;
                    }
                    if capture_started.elapsed() > Duration::from_millis(500) {
                        log::warn!(
                            "USB frame capture and decode took {:?}",
                            capture_started.elapsed()
                        );
                    }
                    match frame_tx.try_send(CameraFrame {
                        width: frame.width() as usize,
                        height: frame.height() as usize,
                        rgb: frame.into_raw(),
                    }) {
                        Ok(()) | Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
                Err(error) => {
                    camera = None;
                    let _ = event_tx.send(CameraEvent::Error(format!(
                        "Lost the Blackmagic USB video feed: {error}"
                    )));
                }
            }
        }
    }
}

fn open_camera(device: &AtemCamera, event_tx: &mpsc::Sender<CameraEvent>) -> Option<Camera> {
    log::info!("Opening ATEM Mini USB webcam: {}", device.name);
    let initial_format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::None);
    match Camera::new(device.index.clone(), initial_format).and_then(|mut camera| {
        let mut formats = camera.compatible_camera_formats()?;
        formats.retain(|format| {
            matches!(
                format.format(),
                FrameFormat::MJPEG
                    | FrameFormat::YUYV
                    | FrameFormat::RAWRGB
                    | FrameFormat::RAWBGR
                    | FrameFormat::GRAY
            )
        });
        formats.sort_by_key(|format| {
            Reverse((
                format.frame_rate(),
                u64::from(format.resolution().width()) * u64::from(format.resolution().height()),
            ))
        });

        log::info!(
            "ATEM USB webcam reports {} supported RGB-decodable formats",
            formats.len()
        );
        let mut last_error = None;
        let mut selected_format = false;
        for format in formats {
            log::info!(
                "Trying USB webcam mode: {}x{} {} fps ({:?})",
                format.resolution().width(),
                format.resolution().height(),
                format.frame_rate(),
                format.format()
            );
            let request = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(format));
            match camera.set_camera_requset(request) {
                Ok(_) => {
                    selected_format = true;
                    break;
                }
                Err(error) => {
                    log::warn!("Camera rejected supported mode: {error}");
                    last_error = Some(error);
                }
            }
        }

        if !selected_format {
            return Err(last_error.unwrap_or_else(|| {
                nokhwa::NokhwaError::GeneralError(
                    "No RGB-decodable USB webcam formats were reported".to_string(),
                )
            }));
        }

        camera.open_stream()?;
        Ok(camera)
    }) {
        Ok(camera) => {
            let format = camera.camera_format();
            log::info!(
                "USB webcam opened: {} at {}x{} {} fps ({:?})",
                device.name,
                format.resolution().width(),
                format.resolution().height(),
                format.frame_rate(),
                format.format()
            );
            let _ = event_tx.send(CameraEvent::Connected(device.name.clone()));
            Some(camera)
        }
        Err(error) => {
            let _ = event_tx.send(CameraEvent::Error(format!(
                "Could not open {}: {error}",
                device.name
            )));
            None
        }
    }
}

fn refresh_cameras(cameras: &mut Vec<AtemCamera>, event_tx: &mpsc::Sender<CameraEvent>) {
    match discover_cameras() {
        Ok(discovered) => {
            log::info!("Found {} matching ATEM USB webcam(s)", discovered.len());
            *cameras = discovered;
            if event_tx
                .send(CameraEvent::Devices(cameras.clone()))
                .is_err()
            {
                log::debug!("Camera UI closed while refreshing devices");
            }
        }
        Err(error) => {
            log::error!("Failed to discover USB cameras: {error:#}");
            let _ = event_tx.send(CameraEvent::Error(format!(
                "Could not scan USB cameras: {error}"
            )));
        }
    }
}

fn ethernet_worker(command_rx: Receiver<EthernetCommand>, event_tx: mpsc::Sender<EthernetEvent>) {
    let service_type = "_atems._tcp.local.";
    let mdns = match ServiceDaemon::new() {
        Ok(mdns) => mdns,
        Err(error) => {
            let _ = event_tx.send(EthernetEvent::Error(format!(
                "Could not start ATEM network discovery: {error}"
            )));
            return;
        }
    };
    let mut service_events = match mdns.browse(service_type) {
        Ok(events) => events,
        Err(error) => {
            let _ = event_tx.send(EthernetEvent::Error(format!(
                "Could not search for ATEM devices: {error}"
            )));
            return;
        }
    };
    let mut services = HashMap::<String, Vec<String>>::new();

    loop {
        loop {
            match command_rx.try_recv() {
                Ok(EthernetCommand::Refresh) => {
                    services.clear();
                    let _ = event_tx.send(EthernetEvent::Devices(Vec::new()));
                    let _ = mdns.stop_browse(service_type);
                    match mdns.browse(service_type) {
                        Ok(events) => service_events = events,
                        Err(error) => {
                            let _ = event_tx.send(EthernetEvent::Error(format!(
                                "Could not restart ATEM device search: {error}"
                            )));
                        }
                    }
                }
                Ok(EthernetCommand::SetOutputMode(ip, mode)) => {
                    // TODO: Send the ATEM protocol command to the selected device over Ethernet.
                    log::info!("ATEM command requested for {ip} -> Set Output Mode: {mode:?}");
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    let _ = mdns.shutdown();
                    return;
                }
            }
        }

        match service_events.recv_timeout(Duration::from_millis(100)) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                services.insert(
                    info.get_fullname().to_string(),
                    info.get_addresses()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                );
                let mut ips: Vec<String> = services
                    .values()
                    .flatten()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                ips.sort();
                if event_tx.send(EthernetEvent::Devices(ips)).is_err() {
                    let _ = mdns.shutdown();
                    return;
                }
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                services.remove(&fullname);
                let mut ips: Vec<String> = services
                    .values()
                    .flatten()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                ips.sort();
                let _ = event_tx.send(EthernetEvent::Devices(ips));
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
}

// --- Main Entry Point ---

fn main() -> Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    // GUI Setup
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_maximized(true)
            .with_decorations(false),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            supported_backends: eframe::wgpu::Backends::DX12,
            power_preference: eframe::wgpu::PowerPreference::HighPerformance,
            desired_maximum_frame_latency: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "atem_app",
        native_options,
        Box::new(|_cc| {
            if let Some(render_state) = &_cc.wgpu_render_state {
                log::info!(
                    "Using hardware renderer: {:?}",
                    render_state.adapter.get_info()
                );
            } else {
                log::error!("The WGPU renderer did not provide a render state");
            }

            let (tx_camera_command, rx_camera_command) = mpsc::channel();
            let (tx_camera_event, rx_camera_event) = mpsc::channel();
            let (tx_frame, rx_frame) = mpsc::sync_channel(1);
            thread::spawn(move || camera_worker(rx_camera_command, tx_camera_event, tx_frame));

            let (tx_ethernet_command, rx_ethernet_command) = mpsc::channel();
            let (tx_ethernet_event, rx_ethernet_event) = mpsc::channel();
            thread::spawn(move || ethernet_worker(rx_ethernet_command, tx_ethernet_event));

            Ok(Box::new(AtemApp::new(
                rx_camera_event,
                rx_frame,
                tx_camera_command,
                rx_ethernet_event,
                tx_ethernet_command,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    Ok(())
}
