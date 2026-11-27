//! The dialog for the NAT gateway: its network and its DNS server

use eframe::egui;
use snow_core::devices::NatConfig;

/// The network of the NAT gateway: its address, the length of the network's prefix and the DNS
/// server, for the workspace and, with "Save as default", for the workspaces that have none of
/// their own
#[derive(Default)]
pub struct NatDialog {
    open: bool,
    gateway: String,
    prefix_len: u8,
    /// Empty for the host's
    dns: String,
    /// The host's, when the dialog opened
    host_dns: Option<std::net::IpAddr>,
    save_default: bool,
    result: Option<NatDialogResult>,
}

pub struct NatDialogResult {
    pub config: NatConfig,
    /// Also save it as the default
    pub save_default: bool,
}

impl NatDialog {
    pub fn open(&mut self, config: NatConfig) {
        self.open = true;
        self.gateway = std::net::Ipv4Addr::from(config.gateway).to_string();
        self.prefix_len = config.prefix_len;
        self.dns = config.dns.map(|dns| dns.to_string()).unwrap_or_default();
        self.host_dns = snow_core::net::host_dns_server();
        self.save_default = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn take_result(&mut self) -> Option<NatDialogResult> {
        self.result.take()
    }

    /// The network entered, or why it can't be one
    fn config(&self) -> Result<NatConfig, String> {
        let gateway: std::net::Ipv4Addr = self.gateway.trim().parse().map_err(|_| {
            "The gateway's address is four numbers separated by dots, such as 10.0.0.1".to_string()
        })?;
        let dns = self.dns.trim();
        let dns = if dns.is_empty() {
            None
        } else {
            Some(dns.parse().map_err(|_| {
                "The DNS server's address is an IPv4 or IPv6 address, or nothing for the host's"
                    .to_string()
            })?)
        };
        let config = NatConfig {
            gateway: gateway.octets(),
            prefix_len: self.prefix_len,
            dns,
        };
        config.check().map_err(|e| e.to_string())?;
        Ok(config)
    }

    /// Puts back the defaults: 10.0.0.1/8 and the host's DNS server
    fn reset(&mut self) {
        let config = NatConfig::default();
        self.gateway = std::net::Ipv4Addr::from(config.gateway).to_string();
        self.prefix_len = config.prefix_len;
        self.dns.clear();
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        egui::Modal::new(egui::Id::new("NAT gateway")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("NAT gateway");
            let config = self.config();

            ui.add_space(4.0);
            ui.strong("Network");
            egui::Grid::new("nat_gateway_network")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Gateway");
                    ui.text_edit_singleline(&mut self.gateway).on_hover_text(
                        "Choose a network that none of the host's own networks is part of",
                    );
                    ui.end_row();

                    ui.label("Prefix");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.prefix_len)
                                .range(8..=30)
                                .prefix("/"),
                        );
                        let mask = NatConfig {
                            gateway: [0; 4],
                            prefix_len: self.prefix_len,
                            dns: None,
                        }
                        .netmask();
                        let class = match self.prefix_len {
                            8 => " (class A)",
                            16 => " (class B)",
                            24 => " (class C)",
                            _ => "",
                        };
                        ui.weak(format!("{}{}", std::net::Ipv4Addr::from(mask), class));
                    });
                    ui.end_row();

                    ui.label("");
                    match &config {
                        Ok(config) => {
                            let gateway = u32::from_be_bytes(config.gateway);
                            let last = (gateway | !u32::from_be_bytes(config.netmask())) - 1;
                            if gateway < last {
                                ui.weak(format!(
                                    "Adapters get {} \u{2013} {}",
                                    std::net::Ipv4Addr::from(gateway + 1),
                                    std::net::Ipv4Addr::from(last)
                                ));
                            } else {
                                ui.colored_label(
                                    egui::Color32::YELLOW,
                                    "No addresses after the gateway's for the adapters",
                                );
                            }
                        }
                        Err(e) => {
                            ui.colored_label(egui::Color32::RED, e);
                        }
                    }
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.strong("DNS");
            egui::Grid::new("nat_gateway_dns")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Server");
                    ui.add(egui::TextEdit::singleline(&mut self.dns).hint_text("The host's"))
                        .on_hover_text(
                            "DNS queries to the gateway go to this server; empty, to the host's",
                        );
                    ui.end_row();

                    ui.label("");
                    ui.weak(match self.host_dns {
                        Some(dns) => format!("Host's server: {}", dns),
                        None => "Host's server: none found".to_string(),
                    });
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.checkbox(&mut self.save_default, "Save as default for new workspaces")
                .on_hover_text(
                    "Also for the workspaces that have no network of their own, including those \
                     from before this setting",
                );
            ui.separator();

            let mut reset = false;
            egui::Sides::new().show(
                ui,
                |ui| reset = ui.button("Defaults").clicked(),
                |ui| {
                    if ui
                        .add_enabled(config.is_ok(), egui::Button::new("OK"))
                        .clicked()
                        && let Ok(config) = config
                    {
                        self.result = Some(NatDialogResult {
                            config,
                            save_default: self.save_default,
                        });
                        self.open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.open = false;
                    }
                },
            );
            if reset {
                self.reset();
            }
        });
    }
}
