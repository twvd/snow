use eframe::egui;
use snow_core::cpu_m68k::regs::{Register, RegisterFile};
use snow_core::cpu_m68k::{CpuM68kType, M68010, M68020};
use snow_core::types::Long;

use crate::uniform::UniformMethods;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterTab {
    Cpu,
    Fpu,
}

/// egui widget to display Motorola 68000 register state
pub struct RegistersWidget {
    regs: RegisterFile,
    lastregs: RegisterFile,
    // Track editing state using the CpuRegister enum
    editing: Option<(Register, String)>, // (register, current_edit_value)
    edited: Option<(Register, Long)>,    // (register, new_value) - when edit is completed
    selected_tab: RegisterTab,
}

impl RegistersWidget {
    const COLOR_VALUE: egui::Color32 = egui::Color32::WHITE;
    const COLOR_CHANGED: egui::Color32 = egui::Color32::YELLOW;

    pub fn new() -> Self {
        Self {
            regs: RegisterFile::new(),
            lastregs: RegisterFile::new(),
            editing: None,
            edited: None,
            selected_tab: RegisterTab::Cpu,
        }
    }

    /// Updates the current view with a new register state.
    /// Registers that have been changed since then will appear yellow.
    pub fn update_regs(&mut self, regs: RegisterFile) {
        self.lastregs = std::mem::replace(&mut self.regs, regs);
    }

    /// Takes the most recently edited register value, if any
    pub fn take_edited_register(&mut self) -> Option<(Register, Long)> {
        self.edited.take()
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, cpu_type: CpuM68kType) {
        ui.vertical(|ui| {
            // Tab selection
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.selected_tab, RegisterTab::Cpu, "CPU");
                ui.selectable_value(&mut self.selected_tab, RegisterTab::Fpu, "FPU");
            });

            ui.separator();

            let available_height = ui.available_height();

            match self.selected_tab {
                RegisterTab::Cpu => self.draw_cpu_registers(ui, cpu_type, available_height),
                RegisterTab::Fpu => self.draw_fpu_registers(ui, cpu_type, available_height),
            }
        });
    }

    fn draw_cpu_registers(
        &mut self,
        ui: &mut egui::Ui,
        cpu_type: CpuM68kType,
        available_height: f32,
    ) {
        use egui_extras::{Column, TableBuilder};

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .column(Column::exact(40.0))
            .column(Column::remainder().at_least(50.0))
            .column(Column::remainder().at_least(60.0))
            .striped(true)
            .body(|mut body| {
                // Helper function for displaying register rows
                let mut register_row = |reg: Register, value_fn: &dyn Fn(&RegisterFile) -> Long| {
                    let name = reg.to_string();
                    let changed = value_fn(&self.regs) != value_fn(&self.lastregs);
                    let color = if changed {
                        Self::COLOR_CHANGED
                    } else {
                        Self::COLOR_VALUE
                    };

                    body.row(20.0, |mut row| {
                        // Register name
                        row.col(|ui| {
                            ui.label(egui::RichText::new(&name));
                        });

                        // Check if this register is being edited
                        if let Some((edit_reg, ref mut edit_value)) = &mut self.editing {
                            let mut clear_editing = false;

                            if *edit_reg == reg {
                                // This register is being edited, show text input
                                row.col(|ui| {
                                    let response = ui.text_edit_singleline(edit_value);

                                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                        // Escape is cancel
                                        clear_editing = true;
                                    } else if response.lost_focus()
                                        || ui.input(|i| i.key_pressed(egui::Key::Enter))
                                    {
                                        // Try to parse the value
                                        if let Ok(new_value) = Long::from_str_radix(edit_value, 16)
                                        {
                                            self.edited = Some((reg, new_value));
                                        }
                                        clear_editing = true;
                                    }
                                });

                                if clear_editing {
                                    self.editing = None;
                                }

                                // Skip the decimal column while editing
                                row.col(|_| {});
                                return;
                            }
                        }

                        // Normal display (not editing)
                        row.col(|ui| {
                            let value = value_fn(&self.regs);
                            let text = egui::RichText::new(format!("{:08X}", value))
                                .family(egui::FontFamily::Monospace)
                                .color(color);

                            let response =
                                ui.add(egui::Label::new(text).sense(egui::Sense::click()));

                            if response.clicked() {
                                // Start editing this register
                                self.editing = Some((reg, format!("{:08X}", value)));
                            }

                            if matches!(
                                reg,
                                Register::An(_)
                                    | Register::USP
                                    | Register::SSP
                                    | Register::MSP
                                    | Register::ISP
                                    | Register::PC
                            ) {
                                response.context_address(value);
                            }
                        });

                        // Decimal representation
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{}", value_fn(&self.regs)))
                                    .family(egui::FontFamily::Monospace)
                                    .color(color),
                            )
                            .on_hover_cursor(egui::CursorIcon::Default);
                        });
                    });
                };

                // Display all data registers D0-D7
                for i in 0..8 {
                    let reg = Register::Dn(i);
                    let index = i;
                    register_row(reg, &move |r: &RegisterFile| r.read_d::<Long>(index));
                }

                // Display all address registers A0-A7
                for i in 0..8 {
                    let reg = Register::An(i);
                    let index = i;
                    register_row(reg, &move |r: &RegisterFile| r.read_a::<Long>(index));
                }

                // Display special registers
                register_row(Register::PC, &|r: &RegisterFile| r.pc);
                register_row(Register::SSP, &|r: &RegisterFile| *r.ssp());
                register_row(Register::USP, &|r: &RegisterFile| r.usp);
                if cpu_type >= M68010 {
                    register_row(Register::SFC, &|r: &RegisterFile| r.sfc);
                    register_row(Register::DFC, &|r: &RegisterFile| r.dfc);
                    register_row(Register::VBR, &|r: &RegisterFile| r.vbr);
                }
                if cpu_type >= M68020 {
                    register_row(Register::CAAR, &|r: &RegisterFile| r.caar);
                    register_row(Register::CACR, &|r: &RegisterFile| r.cacr.0);
                    register_row(Register::MSP, &|r: &RegisterFile| r.msp);
                    register_row(Register::ISP, &|r: &RegisterFile| r.isp);
                }
                if cpu_type >= M68020 {
                    // FPU stuff
                    register_row(Register::FPCR, &|r: &RegisterFile| r.fpu.fpcr.0);
                    register_row(Register::FPSR, &|r: &RegisterFile| r.fpu.fpsr.0);
                    register_row(Register::FPIAR, &|r: &RegisterFile| r.fpu.fpiar);
                }

                // SR register is handled separately since it's a 16-bit value
                body.row(20.0, |mut row| {
                    row.col(|ui| {
                        ui.label(egui::RichText::new("SR"));
                    });

                    // Check if SR is being edited
                    if let Some((edit_reg, ref mut edit_value)) = &mut self.editing {
                        if *edit_reg == Register::SR {
                            let mut clear_editing = false;
                            row.col(|ui| {
                                let response = ui.text_edit_singleline(edit_value);

                                if response.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                {
                                    if let Ok(new_value) = u16::from_str_radix(edit_value, 16) {
                                        self.edited = Some((Register::SR, new_value as Long));
                                    }
                                    clear_editing = true;
                                }
                            });

                            if clear_editing {
                                self.editing = None;
                            }

                            row.col(|_| {});
                            // Skip the flags when editing
                            return;
                        }
                    }

                    // Normal display (not editing)
                    row.col(|ui| {
                        let text = egui::RichText::new(format!("{:04X}", self.regs.sr.sr()))
                            .family(egui::FontFamily::Monospace)
                            .color(if self.regs.sr == self.lastregs.sr {
                                Self::COLOR_VALUE
                            } else {
                                Self::COLOR_CHANGED
                            });

                        let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));

                        if response.clicked() {
                            self.editing =
                                Some((Register::SR, format!("{:04X}", self.regs.sr.sr())));
                        }
                    });

                    // Flags display
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            let mut flag = |n, v| {
                                ui.label(format!(
                                    "{} {}",
                                    if v {
                                        egui_material_icons::icons::ICON_CHECK_BOX
                                    } else {
                                        egui_material_icons::icons::ICON_CHECK_BOX_OUTLINE_BLANK
                                    },
                                    n
                                ))
                            };
                            flag("C", self.regs.sr.c());
                            flag("V", self.regs.sr.v());
                            flag("Z", self.regs.sr.z());
                            flag("N", self.regs.sr.n());
                            flag("X", self.regs.sr.x());
                            flag("Supervisor", self.regs.sr.supervisor());
                            flag("Trace", self.regs.sr.trace());
                            ui.label(format!("Int mask: {}", self.regs.sr.int_prio_mask()));
                        });
                    });
                });
            });
    }

    fn draw_fpu_registers(
        &mut self,
        ui: &mut egui::Ui,
        cpu_type: CpuM68kType,
        available_height: f32,
    ) {
        use egui_extras::{Column, TableBuilder};

        if cpu_type < M68020 {
            ui.centered_and_justified(|ui| {
                ui.label("FPU not installed");
            });
            return;
        }

        TableBuilder::new(ui)
            .max_scroll_height(available_height)
            .column(Column::exact(50.0))
            .column(Column::remainder().at_least(150.0))
            .striped(true)
            .body(|mut body| {
                // FP0-FP7 floating point registers
                for i in 0..8 {
                    let fp_current = &self.regs.fpu.fp[i];
                    let fp_last = &self.lastregs.fpu.fp[i];

                    // Check if value changed
                    let changed = fp_current.is_inf() != fp_last.is_inf()
                        || fp_current.is_nan() != fp_last.is_nan()
                        || (!fp_current.is_nan()
                            && !fp_current.is_inf()
                            && fp_current.as_f64() != fp_last.as_f64());

                    let color = if changed {
                        Self::COLOR_CHANGED
                    } else {
                        Self::COLOR_VALUE
                    };

                    body.row(20.0, |mut row| {
                        // Register name
                        row.col(|ui| {
                            ui.label(egui::RichText::new(format!("FP{}", i)));
                        });

                        // Value as floating point
                        row.col(|ui| {
                            let value_str = if fp_current.is_nan() {
                                "NaN".to_string()
                            } else if fp_current.is_inf() {
                                if fp_current.is_negative() {
                                    "-Infinity".to_string()
                                } else {
                                    "+Infinity".to_string()
                                }
                            } else {
                                format!("{:+.17e}", fp_current.as_f64())
                            };

                            ui.label(
                                egui::RichText::new(value_str)
                                    .family(egui::FontFamily::Monospace)
                                    .color(color),
                            );
                        });
                    });
                }

                // Add a separator before control registers
                body.row(10.0, |mut row| {
                    row.col(|_| {});
                    row.col(|_| {});
                });

                // Helper function for displaying FPU control register rows
                let mut register_row =
                    |name: &str, reg: Register, value_fn: &dyn Fn(&RegisterFile) -> Long| {
                        let changed = value_fn(&self.regs) != value_fn(&self.lastregs);
                        let color = if changed {
                            Self::COLOR_CHANGED
                        } else {
                            Self::COLOR_VALUE
                        };

                        body.row(20.0, |mut row| {
                            // Register name
                            row.col(|ui| {
                                ui.label(egui::RichText::new(name));
                            });

                            // Check if this register is being edited
                            if let Some((edit_reg, ref mut edit_value)) = &mut self.editing {
                                let mut clear_editing = false;

                                if *edit_reg == reg {
                                    // This register is being edited, show text input
                                    row.col(|ui| {
                                        let response = ui.text_edit_singleline(edit_value);

                                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                            // Escape is cancel
                                            clear_editing = true;
                                        } else if response.lost_focus()
                                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                                        {
                                            // Try to parse the value
                                            if let Ok(new_value) =
                                                Long::from_str_radix(edit_value, 16)
                                            {
                                                self.edited = Some((reg, new_value));
                                            }
                                            clear_editing = true;
                                        }
                                    });

                                    if clear_editing {
                                        self.editing = None;
                                    }

                                    return;
                                }
                            }

                            // Normal display (not editing)
                            row.col(|ui| {
                                let value = value_fn(&self.regs);
                                let text = egui::RichText::new(format!("{:08X}", value))
                                    .family(egui::FontFamily::Monospace)
                                    .color(color);

                                let response =
                                    ui.add(egui::Label::new(text).sense(egui::Sense::click()));

                                if response.clicked() {
                                    // Start editing this register
                                    self.editing = Some((reg, format!("{:08X}", value)));
                                }
                            });
                        });
                    };

                // Display FPU control registers
                register_row("FPCR", Register::FPCR, &|r: &RegisterFile| r.fpu.fpcr.0);
                register_row("FPSR", Register::FPSR, &|r: &RegisterFile| r.fpu.fpsr.0);
                register_row("FPIAR", Register::FPIAR, &|r: &RegisterFile| r.fpu.fpiar);
            });
    }
}
