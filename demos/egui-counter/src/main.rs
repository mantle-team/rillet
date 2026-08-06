//! An egui counter demonstrating the rillet view pattern.
//!
//! The services and the window share only their views: the counter and
//! status services update state and publish snapshots; the window draws
//! the latest snapshots, repainting when a new one is published or the
//! user interacts.
//!
//! Because the two sides share nothing else, either can change without
//! affecting the other, and neither can block the other: painting reads
//! views wait-free, and input sends commands without waiting.

use std::sync::Arc;

use eframe::egui;
use rillet::CheapClone;
use rillet::view::{SmolStr, ViewWatcher};

/// A snapshot of the counter.
#[derive(Clone, PartialEq, CheapClone)]
struct CounterView {
    value: i64,
}

/// A counter mutated from the UI.
#[rillet::service(view = CounterView)]
struct Counter {
    #[rillet(default)]
    value: i64,
}

impl Counter {
    fn view(&self) -> CounterView {
        CounterView { value: self.value }
    }
}

#[rillet::handlers]
impl Counter {
    #[rillet(command)]
    fn add(&mut self, delta: i64) {
        self.value += delta;
    }
}

/// A snapshot of the status line.
#[derive(Clone, PartialEq, CheapClone)]
struct StatusView {
    message: SmolStr,
}

/// A status line derived from the counter's views.
#[rillet::service(view = StatusView)]
struct Status {
    counter: CounterHandle,

    #[rillet(default)]
    message: SmolStr,
}

impl Status {
    fn view(&self) -> StatusView {
        StatusView {
            message: self.message.clone(),
        }
    }
}

#[rillet::handlers]
impl Status {
    #[rillet(watch = counter)]
    fn on_counter_view(&mut self, view: Arc<CounterView>) {
        self.message = SmolStr::new(format!("counter is {}", view.value));
    }
}

fn main() -> eframe::Result<()> {
    let counter = Counter::new().spawn();
    let status = Status::new(counter.clone()).spawn();

    let counter_reader: CounterViewHandle = counter.clone().into();
    let status_reader: StatusViewHandle = status.into();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([340.0, 214.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "rillet + egui",
        options,
        Box::new(move |cc| {
            request_repaint_on_change(&cc.egui_ctx, counter_reader.watch_view());
            request_repaint_on_change(&cc.egui_ctx, status_reader.watch_view());
            Ok(Box::new(App {
                counter,
                counter_reader,
                status_reader,
            }))
        }),
    )
}

/// Spawns a thread that requests one repaint per view published.
///
/// The thread sleeps between publishes, so quiet services cost no wakeups
/// and an untouched window paints nothing.
fn request_repaint_on_change<V>(ctx: &egui::Context, mut watch: ViewWatcher<V>)
where
    V: rillet::view::CheapClone + PartialEq + Send + Sync + 'static,
{
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        loop {
            futures_lite::future::block_on(watch.changed());
            ctx.request_repaint();
        }
    });
}

struct App {
    counter: CounterHandle,
    counter_reader: CounterViewHandle,
    status_reader: StatusViewHandle,
}

impl App {
    /// The status row: the line of text the status service derived from
    /// the counter's views.
    fn status_bar(&self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(egui::RichText::new(self.status_reader.view().message.as_str()).size(16.0));
        });
    }

    /// The counter row, centered in the window: a label showing the view
    /// between two buttons that send commands.
    fn counter_panel(&self, ui: &mut egui::Ui) {
        let view = self.counter_reader.view();
        egui::CentralPanel::default().show(ui, |ui| {
            let button = egui::vec2(48.0, 48.0);
            let label = egui::vec2(96.0, 48.0);
            let row_width = button.x * 2.0 + label.x + ui.spacing().item_spacing.x * 2.0;

            ui.add_space((ui.available_height() - button.y) / 2.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - row_width) / 2.0);
                if ui.add_sized(button, egui::Button::new("-")).clicked() {
                    self.counter.add(-1);
                }
                ui.add_sized(
                    label,
                    egui::Label::new(
                        egui::RichText::new(format!("{}", view.value))
                            .size(36.0)
                            .monospace(),
                    ),
                );
                if ui.add_sized(button, egui::Button::new("+")).clicked() {
                    self.counter.add(1);
                }
            });
        });
    }
}

impl eframe::App for App {
    /// Paints the two panels from the latest views. Nothing here requests
    /// a repaint; those come from input and the watcher threads.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.status_bar(ui);
        self.counter_panel(ui);
    }
}
