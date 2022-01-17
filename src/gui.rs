use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::Duration,
};

use audio_capture::win::capture::AudioCapture;
use buttplug::client::{ButtplugClient, VibrateCommand};
use clap::Parser;
use eframe::{
    egui::{self, Button, Color32, ProgressBar, RichText, Slider},
    epi,
};

use crate::util;

#[derive(Parser, Default)]
pub struct Gui {
    server_addr: Option<String>,
}

pub fn gui(_args: Gui) {
    let app = GuiApp::new();
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(Box::new(app), native_options);
}

struct GuiApp {
    runtime: tokio::runtime::Runtime,
    client: ButtplugClient,
    devices: HashMap<u32, DeviceProps>,
    current_sound_power: Arc<AtomicU32>,
    _capture_thread: JoinHandle<()>,
    is_scanning: bool,
}

struct DeviceProps {
    is_enabled: bool,
    multiplier: f32,
    max: f32,
}

impl Default for DeviceProps {
    fn default() -> Self {
        Self {
            is_enabled: false,
            multiplier: 1.0,
            max: 1.0,
        }
    }
}

fn capture_thread(current_sound_power: Arc<AtomicU32>) -> ! {
    let dur = Duration::from_millis(1);
    let mut capture = AudioCapture::init(dur).unwrap();

    let format = capture.format().unwrap();
    // time to fill about half of AudioCapture's buffer
    let actual_duration = Duration::from_secs_f32(
        dur.as_secs_f32() * capture.buffer_frame_size as f32
            / format.sample_rate as f32
            / 1000.,
    ) / 2;

    let buffer_size = (format.sample_rate as f32 * dur.as_secs_f32()) as usize
        * format.channels as usize;
    let mut buf = VecDeque::new();
    buf.resize(buffer_size, 0.0);

    capture.start().unwrap();
    loop {
        std::thread::sleep(actual_duration);
        capture
            .read_samples::<(), _>(|samples, _| {
                for value in samples {
                    buf.push_front(*value);
                }
                buf.truncate(buffer_size);
                Ok(())
            })
            .unwrap();

        let buf = buf.make_contiguous();
        let speeds = util::calculate_power(&buf, format.channels as _);
        let avg = util::avg(&speeds).clamp(0.0, 1.0);
        current_sound_power.store(avg.to_bits(), Ordering::Relaxed);
    }
}

impl GuiApp {
    fn new() -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = runtime.block_on(util::start_bp_server()).unwrap();
        let devices = Default::default();
        let current_sound_power = Arc::new(AtomicU32::new(0));
        let current_sound_power2 = current_sound_power.clone();

        let _capture_thread =
            std::thread::spawn(|| capture_thread(current_sound_power2));

        GuiApp {
            runtime,
            client,
            devices,
            current_sound_power,
            _capture_thread,
            is_scanning: false,
        }
    }

    fn load_sound_power(&self) -> f32 {
        f32::from_bits(self.current_sound_power.load(Ordering::Relaxed))
    }
}

impl epi::App for GuiApp {
    fn name(&self) -> &str {
        "Music Vibes"
    }

    fn update(&mut self, ctx: &egui::CtxRef, _frame: &epi::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                let scan_label = if self.is_scanning {
                    "Stop scanning"
                } else {
                    "Start scanning"
                };
                if ui.selectable_label(self.is_scanning, scan_label).clicked() {
                    self.is_scanning = !self.is_scanning;
                    if self.is_scanning {
                        self.runtime.spawn(self.client.start_scanning());
                    } else {
                        self.runtime.spawn(self.client.stop_scanning());
                    }
                }
                let stop_button = Button::new(
                    RichText::new("Stop all devices").color(Color32::BLACK),
                )
                .fill(Color32::from_rgb(240, 0, 0));
                if ui.add_sized([60.0, 30.0], stop_button).clicked() {
                    self.runtime.spawn(self.client.stop_all_devices());
                    for device in self.devices.values_mut() {
                        device.is_enabled = false;
                    }
                }
            });
            ui.separator();
            let sound_power = self.load_sound_power();
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Current volume: {:.2}%",
                    sound_power * 100.0
                ));
                ui.add(ProgressBar::new(sound_power));
            });
            ui.heading("Devices");
            for device in self.client.devices() {
                let props = self.devices.entry(device.index()).or_default();
                ui.group(|ui| {
                    #[cfg(debug_assertions)]
                    ui.label(format!("({}) {}", device.index(), device.name));
                    #[cfg(not(debug_assertions))]
                    ui.label(&device.name);
                    if let Ok(bat) =
                        self.runtime.block_on(device.battery_level())
                    {
                        ui.label(format!("Battery: {}", bat));
                    }
                    let speed = if props.is_enabled {
                        (sound_power * props.multiplier).clamp(0.0, props.max)
                    } else {
                        0.0
                    };

                    ui.horizontal(|ui| {
                        ui.label(format!("{:.2}%", speed * 100.0));
                        ui.add(ProgressBar::new(speed));
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .selectable_label(props.is_enabled, "Enable")
                            .clicked()
                        {
                            props.is_enabled = !props.is_enabled;
                        }
                        ui.label("Multiplier: ");
                        ui.add(Slider::new(&mut props.multiplier, 0.0..=20.0));
                        ui.label("Maximum: ");
                        ui.add(Slider::new(&mut props.max, 0.0..=1.0));
                    });
                    self.runtime.spawn(
                        device.vibrate(VibrateCommand::Speed(speed as _)),
                    );
                });
            }
        });
        ctx.request_repaint();
    }
}
