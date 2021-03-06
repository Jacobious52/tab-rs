use crate::state::terminal::TerminalMode;

use crate::bus::MainBus;
use crate::prelude::*;

use crate::{
    bus::TerminalBus,
    message::terminal::{TerminalRecv, TerminalSend},
};

use crossterm_mode::TerminalCrosstermService;
use echo_mode::TerminalEchoService;

use terminal_event::TerminalEventService;

mod crossterm_mode;
mod echo_mode;
mod terminal_event;

pub use echo_mode::disable_raw_mode;
pub use echo_mode::reset_cursor;

/// Reads TerminalMode, and launches/cancels the TerminalEchoService / TerminalCrosstermService
pub struct TerminalService {
    _main_terminal: MainTerminalCarrier,
    _terminal_mode: Lifeline,
    _terminal_event: TerminalEventService,
}

enum ServiceLifeline {
    Echo(TerminalEchoService),
    Crossterm(TerminalCrosstermService),
    None,
}

impl Service for TerminalService {
    type Bus = MainBus;
    type Lifeline = anyhow::Result<Self>;

    fn spawn(bus: &MainBus) -> Self::Lifeline {
        let terminal_bus = TerminalBus::default();
        terminal_bus.capacity::<TerminalSend>(2048)?;
        terminal_bus.capacity::<TerminalRecv>(2048)?;

        let _main_terminal = terminal_bus.carry_from(bus)?;
        let _terminal_event = TerminalEventService::spawn(&terminal_bus)?;

        let mut rx_terminal_mode = terminal_bus.rx::<TerminalMode>()?;

        let _terminal_mode = Self::try_task("dispatch_mode", async move {
            let mut service = ServiceLifeline::None;

            while let Some(mode) = rx_terminal_mode.recv().await {
                service = match mode {
                    TerminalMode::Echo => {
                        if let ServiceLifeline::Echo(ref _echo) = service {
                            continue;
                        }

                        info!("TerminalService switching to echo mode");

                        let service = TerminalEchoService::spawn(&terminal_bus)?;
                        ServiceLifeline::Echo(service)
                    }
                    TerminalMode::Crossterm => {
                        if let ServiceLifeline::Crossterm(ref _crossterm) = service {
                            continue;
                        }

                        info!("TerminalService switching to crossterm mode");

                        let service = TerminalCrosstermService::spawn(&terminal_bus)?;
                        ServiceLifeline::Crossterm(service)
                    }
                }
            }

            Ok(())
        });

        Ok(Self {
            _main_terminal,
            _terminal_mode,
            _terminal_event,
        })
    }
}
