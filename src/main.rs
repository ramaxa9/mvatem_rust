use anyhow::Result;
use eframe::egui;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::sync::Arc;
use tokio::sync::mpsc;

// --- Types for ATEM Control ---

#[derive(Debug, Clone)]
enum AtemCommand {
    SetOutputMode(String, OutputMode), // IP, Mode
}

#[derive(Debug, Clone, PartialEq)]
enum OutputMode {
    Multiview,
    Preview,
    Program,
}

// --- Application State ---

struct AtemApp {
    connected_ip: Option<String>,
    discovered_ips: Vec<String>,
    manual_ip_input: String,
    is_searching: bool,
    // Channels for communication with background tasks
    rx_discovery: mpsc::Receiver<String>,
    tx_command: mpsc::Sender<AtemCommand>,
}

impl AtemApp {
    fn new(
        _cc: &eframe::CreationContext<'_>,
        rx_discovery: mpsc::Receiver<String>,
        tx_command: mpsc::Sender<AtemCommand>,
    ) -> Self {
        Self {
            connected_ip: None,
            discovered_ips: Vec::new(),
            manual_ip_input: String::new(),
            is_searching: true,
            rx_discovery,
            tx_command,
        }
    }
}

// --- GUI Implementation ---

impl eframe::App for AtemApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Poll for discovered IPs from the background task
        while let Ok(ip) = self.rx_discovery.try_recv() {
            if !self.discovered_ips.contains(&ip) {
                log::info!("Discovered ATEM device at: {}", ip);
                self.discovered_ips.push(ip);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Blackmagic ATEM USB Streamer");
            });
            ui.add_space(10.0);

            // Connection Status Info
            if let Some(ip) = &self.connected_ip {
                ui.label(format!("✅ Connected to: {}", ip));
            } else {
                ui.label("❌ No device connected.");
            }

            ui.separator();

            // 2. Video Stream Display Area (Placeholder for OpenCV integration)
            // Note: To use real video, you will need 'opencv' crate and libclang installed.
            let (rect, response) = ui.allocate_at_least(egui::vec2(640.0, 360.0), egui::Sense::click());
            
            // Draw a dark rectangle for the video placeholder
            ui.painter().rect_filled(rect, 5.0, egui::Color32::from_rgb(20, 20, 20));
            
            // Text in the middle of the "video" area
            let text = if self.connected_ip.is_some() {
                format!("Streaming from: {}", self.connected_ip.as_ref().unwrap())
            } else {
                "No active stream\n(Right-click to connect)".to_string()
            };
            
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(18.0),
                egui::Color32::GRAY,
            );

            // 3. Right-Click Context Menu
            response.context_menu(|ui| {
                if let Some(ip) = &self.connected_ip {
                    ui.label(format!("Device IP: {}", ip));
                    ui.separator();
                    
                    ui.label("Switch Output Type:");
                    if ui.button("Multiview").clicked() {
                        let _ = self.tx_command.try_send(AtemCommand::SetOutputMode(ip.clone(), OutputMode::Multiview));
                        ui.close_menu();
                    }
                    if ui.button("Preview").clicked() {
                        let _ = self.tx_command.try_send(AtemCommand::SetOutputMode(ip.clone(), OutputMode::Preview));
                        ui.close_menu();
                    }
                    if ui.button("Program").clicked() {
                        let _ = self.tx_command.try_send(AtemCommand::SetOutputMode(ip.clone(), OutputMode::Program));
                        ui.close_menu();
                    }
                } else {
                    ui.label("Connect to a device:");
                    ui.separator();
                    
                    ui.horizontal(|ui| {
                        ui.label("IP:");
                        ui.text_edit_singleline(&mut self.manual_ip_input);
                    });

                    if ui.button("Connect Manually").clicked() {
                        if !self.manual_ip_input.trim().is_empty() {
                            self.connected_ip = Some(self.manual_ip_input.trim().to_string());
                            ui.close_menu();
                        }
                    }
                }
            });

            ui.add_space(20.0);
            ui.separator();

            // 4. Discovery and Connection Section
            if self.is_searching {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Searching for ATEM devices on the network...");
                });
            } else {
                if ui.button("Restart Search").clicked() {
                    self.discovered_ips.clear();
                    self.is_searching = true;
                }
            }

            if !self.discovered_ips.is_empty() {
                ui.label(format!("Discovered {} devices:", self.discovered_ips.len()));
                egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                    for ip in &self.discovered_ips {
                        if ui.button(format!("Connect to {}", ip)).clicked() {
                            self.connected_ip = Some(ip.clone());
                        }
                    }
                });
            }
        });

        // Continuously repaint so UI updates when background tasks send data via channels
        ctx.request_repaint();
    }
}

// --- Main Entry Point ---

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let (tx_discovery, rx_discovery) = mpsc::channel(100);
    let (tx_command, mut rx_command) = mpsc::channel::<AtemCommand>(100);

    // Background Task: mDNS Discovery
    tokio::spawn(async move {
        log::info!("Starting mDNS discovery task...");
        match ServiceDaemon::new() {
            Ok(mdns) => {
                // Blackmagic ATEM devices often use specific service types. 
                // For this implementation, we'll try a common pattern or search for all resolved services.
                // In a production app, you'd use the exact ATEM mDNS service string.
                let service_type = "_atems._tcp.local."; 
                
                match mdns.browse(service_type) {
                    Ok(receiver) => {
                        log::info!("mDNS browsing started for {}", service_type);
                        while let Ok(event) = receiver.recv_async().await {
                            if let ServiceEvent::ServiceResolved(info) = event {
                                for addr in info.get_addresses() {
                                    let ip = addr.to_string();
                                    if tx_discovery.send(ip).await.is_err() {
                                        break; 
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => log::error!("Failed to browse mDNS: {}", e),
                }
            }
            Err(e) => log::error!("Failed to create mDNS daemon: {}", e),
        }
    });

    // Background Task: ATEM Network Control (Mocked)
    tokio::spawn(async move {
        log::info!("Starting ATEM command handler...");
        while let Some(command) = rx_command.recv().await {
            match command {
                AtemCommand::SetOutputMode(ip, mode) => {
                    // TODO: Implement actual Blackmagic ATEM protocol over TCP (Port 9990)
                    log::info!("COMMAND SENT to {} -> Set Output Mode: {:?}", ip, mode);
                }
            }
        }
    });

    // GUI Setup
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("Blackmagic ATEM USB Streamer"),
        ..Default::default()
    };

    eframe::run_native(
        "atem_app",
        native_options,
        Box::new(|cc| {
            // In a real app, we might want to pass the context or other things here.
            Ok(Box::new(AtemApp::new(cc, rx_discovery, tx_command)))
        }),
    ).map_err(|e| anyhow::anyhow!(e.to_string()))?;

    Ok(())
}