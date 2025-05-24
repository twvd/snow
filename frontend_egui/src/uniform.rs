//! Uniform UI patterns used throughout the GUI
//!
//! For example; right-clicking an address anywhere in the debugger UI should
//! present actions to add breakpoints, watchpoints, etc.
//!
//! This uses a global variable to defer the chosen action to
//! the next egui render pass in egui::App::update(). This avoids having to
//! pass the SnowGui object around (because actions may require access to
//! widgets other than the one initiating the action) and associated spaghetti
//! and borrow checker issues.

use std::cell::Cell;

use eframe::egui;
use snow_core::bus::Address;
use snow_core::cpu_m68k::cpu::{Breakpoint, BusBreakpoint};

use crate::widgets::watchpoints::WatchpointType;

thread_local! {
    pub static UNIFORM_ACTION: Cell<UniformAction> = Cell::new(Default::default());
}

#[derive(Default)]
pub enum UniformAction {
    #[default]
    None,
    AddressWatch(Address, WatchpointType),
    Breakpoint(Breakpoint),
    AddressMemoryViewer(Address),
}

pub trait UniformMethods {
    fn context_address(&self, addr: Address) -> Option<egui::InnerResponse<()>>;
}

impl UniformMethods for egui::Response {
    fn context_address(&self, addr: Address) -> Option<egui::InnerResponse<()>> {
        self.context_menu(|ui| {
            if ui.button("Copy address (24-bit hex)").clicked() {
                ui.output_mut(|o| o.copied_text = format!("{:06X}", addr & 0xFFFFFF));
                ui.close_menu();
            }
            if ui.button("Copy address (32-bit hex)").clicked() {
                ui.output_mut(|o| o.copied_text = format!("{:08X}", addr));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Add execution breakpoint").clicked() {
                UNIFORM_ACTION.set(UniformAction::Breakpoint(Breakpoint::Execution(addr)));
                ui.close_menu();
            }
            if ui.button("Add read access breakpoint").clicked() {
                UNIFORM_ACTION.set(UniformAction::Breakpoint(Breakpoint::Bus(
                    BusBreakpoint::Read,
                    addr,
                )));
                ui.close_menu();
            }
            if ui.button("Add write access breakpoint").clicked() {
                UNIFORM_ACTION.set(UniformAction::Breakpoint(Breakpoint::Bus(
                    BusBreakpoint::Write,
                    addr,
                )));
                ui.close_menu();
            }
            if ui.button("Add read/write access breakpoint").clicked() {
                UNIFORM_ACTION.set(UniformAction::Breakpoint(Breakpoint::Bus(
                    BusBreakpoint::ReadWrite,
                    addr,
                )));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Add watch (u8)").clicked() {
                UNIFORM_ACTION.set(UniformAction::AddressWatch(addr, WatchpointType::U8));
                ui.close_menu();
            }
            if ui.button("Add watch (u16)").clicked() {
                UNIFORM_ACTION.set(UniformAction::AddressWatch(addr, WatchpointType::U16));
                ui.close_menu();
            }
            if ui.button("Add watch (u32)").clicked() {
                UNIFORM_ACTION.set(UniformAction::AddressWatch(addr, WatchpointType::U32));
                ui.close_menu();
            }
            ui.separator();
            if ui.button("View in memory viewer").clicked() {
                UNIFORM_ACTION.set(UniformAction::AddressMemoryViewer(addr));
                ui.close_menu();
            }
        })
    }
}
