//! Headless output. Stdout carries only the selected format; diagnostics go to
//! stderr. A broken output pipe cancels local scheduling instead of leaving
//! unobserved mutation running.

use std::io::Write;

use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::events::EventEnvelope;
use bollo_protocol::{ndjson, EventType};
use bollo_tui::Presenter;

pub struct HeadlessRenderer {
    ndjson: bool,
    presenter: Presenter,
    cancel: CancellationToken,
    broken: bool,
    events: u64,
}

impl HeadlessRenderer {
    pub fn new(ndjson: bool, cancel: CancellationToken) -> Self {
        Self {
            ndjson,
            presenter: Presenter::from_env(false),
            cancel,
            broken: false,
            events: 0,
        }
    }

    pub fn events(&self) -> u64 {
        self.events
    }

    pub fn broken_pipe(&self) -> bool {
        self.broken
    }

    pub fn on_event(&mut self, event: &EventEnvelope) {
        if self.broken {
            return;
        }
        self.events += 1;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let result = if self.ndjson {
            ndjson::write_event(&mut lock, event)
        } else if event.event_type == EventType::AssistantDelta {
            let text = event
                .data
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            lock.write_all(text.as_bytes())
                .and_then(|_| lock.flush())
        } else {
            match self.presenter.render(event) {
                Some(line) => writeln!(lock, "{line}"),
                None => Ok(()),
            }
        };
        if let Err(err) = result {
            self.broken = true;
            self.cancel.cancel();
            eprintln!("bollo: output stream failed ({err}); cancelling local scheduling");
        }
    }
}
