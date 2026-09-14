use clap::Parser;

use egui::ViewportBuilder;
use image::{DynamicImage, RgbImage, imageops::FilterType::Nearest};
use ndarray::{Array1, Array2, s};

use num::complex::Complex;
use soapy_spec_acc::{
    binary_observation::BinaryObservationWriter,
    daq::run_daq,
    fits_table::{FitsTableWriter, ObservationMetadata},
};
use soapysdr::{Device, Direction};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(target_os = "macos")]
use std::process::Command;

use eframe::{
    Renderer,
    egui::{self, CentralPanel, Context, Key, Slider, TopBottomPanel, Vec2, Visuals},
};
use egui_plotter::EguiBackend;
use plotters::coord::{ranged1d::ValueFormatter, types::RangedCoordf64};
use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};

use crossbeam::channel::{Receiver, TryRecvError, bounded};

type Ftype = f32;
type FileDialogResult = Result<Option<PathBuf>, String>;

// Plotters needs explicit space for axis labels. The old 5 px side areas
// clipped values such as "-102.0" and the first/last frequency ticks.
const AXIS_FONT_SIZE: u32 = 14;
const HORIZONTAL_LABEL_AREA_SIZE: u32 = 56;
const VERTICAL_LABEL_AREA_SIZE: u32 = 72;
const PLOT_SIDE_MARGIN: u32 = 12;
// egui-plotter rotates text around the unrotated text box, so a longer label
// is shifted farther to the left. Use separate, compensated positions to keep
// both vertical titles fully inside the canvas and visually aligned.
const TIME_AXIS_UNIT_X: i32 = 40;
const POWER_AXIS_UNIT_X: i32 = 52;
const MIN_WINDOW_SIZE: Vec2 = Vec2::new(1200.0, 640.0);
const INITIAL_WINDOW_SIZE: Vec2 = Vec2::new(1200.0, 700.0);

struct SaveControl {
    selected_path: Option<PathBuf>,
    writer: Option<OutputWriter>,
    metadata: ObservationMetadata,
    current_frequency_hz: f64,
    error: Option<String>,
}

enum OutputWriter {
    Binary(BinaryObservationWriter),
    Fits(FitsTableWriter),
}

impl SaveControl {
    fn new(selected_path: Option<PathBuf>, metadata: ObservationMetadata) -> Self {
        let current_frequency_hz = metadata.center_frequency_hz;
        Self {
            selected_path,
            writer: None,
            metadata,
            current_frequency_hz,
            error: None,
        }
    }

    fn is_saving(&self) -> bool {
        self.writer.is_some()
    }

    fn select_path(&mut self, path: PathBuf) {
        self.stop_saving();
        self.selected_path = Some(path);
        self.error = None;
    }

    fn start_saving(&mut self, append: bool) {
        let Some(path) = self.selected_path.clone() else {
            self.error = Some("Select a file before starting to save".to_owned());
            return;
        };

        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        let writer = match extension.as_deref() {
            Some("bin") => {
                let mut metadata = self.metadata.clone();
                metadata.center_frequency_hz = self.current_frequency_hz;
                BinaryObservationWriter::open(&path, metadata, append).map(OutputWriter::Binary)
            }
            Some("fits" | "fit") => {
                let mut metadata = self.metadata.clone();
                metadata.center_frequency_hz = self.current_frequency_hz;
                FitsTableWriter::create(&path, metadata).map(OutputWriter::Fits)
            }
            _ => {
                self.writer = None;
                self.error = Some("Unsupported file type; use .bin, .fits, or .fit".to_owned());
                return;
            }
        };

        match writer {
            Ok(writer) => {
                self.writer = Some(writer);
                self.error = None;
            }
            Err(error) => {
                self.writer = None;
                self.error = Some(format!("Could not open {}: {error}", path.display()));
            }
        }
    }

    fn stop_saving(&mut self) {
        let result = match self.writer.take() {
            Some(OutputWriter::Binary(writer)) => writer.finish(),
            Some(OutputWriter::Fits(writer)) => writer.finish(),
            None => return,
        };
        if let Err(error) = result {
            self.error = Some(format!("Could not finish saving: {error}"));
        }
    }

    fn write_spectrum(&mut self, spectrum: &[Ftype]) {
        let result = match self.writer.as_mut() {
            Some(OutputWriter::Binary(writer)) => {
                Some(writer.write_spectrum(self.current_frequency_hz, spectrum))
            }
            Some(OutputWriter::Fits(writer)) => {
                Some(writer.write_spectrum(self.current_frequency_hz, spectrum))
            }
            None => None,
        };
        if let Some(Err(error)) = result {
            self.writer = None;
            self.error = Some(format!("Saving stopped: {error}"));
        }
    }

    fn set_frequency(&mut self, frequency_hz: f64) {
        self.current_frequency_hz = frequency_hz;
    }

    fn selected_format_name(&self) -> &'static str {
        match self
            .selected_path
            .as_deref()
            .and_then(Path::extension)
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("bin") => "HI binary v1",
            Some("fits" | "fit") => "FITS table",
            _ => "Unsupported format",
        }
    }
}

#[cfg(target_os = "macos")]
fn select_save_file() -> Result<Option<PathBuf>, String> {
    let output = Command::new("osascript")
        .args([
            "-e",
            "set outputFile to choose file name with prompt \"Select .bin or .fits file to save\" default name \"observation.bin\"",
            "-e",
            "return POSIX path of outputFile",
        ])
        .output()
        .map_err(|error| format!("Could not open the file chooser: {error}"))?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Ok((!path.is_empty()).then(|| PathBuf::from(path)));
    }

    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.contains("User canceled") || message.contains("(-128)") {
        Ok(None)
    } else {
        Err(if message.is_empty() {
            "The file chooser closed unexpectedly".to_owned()
        } else {
            message
        })
    }
}

#[cfg(target_os = "windows")]
fn select_save_file() -> Result<Option<PathBuf>, String> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
                FOS_PATHMUSTEXIST, FileSaveDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
            },
        },
        core::{HRESULT, w},
    };

    const ERROR_CANCELLED_HRESULT: HRESULT = HRESULT(0x8007_04c7_u32 as i32);

    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|error| format!("Could not initialize the Windows file chooser: {error}"))?;

        let result = (|| {
            let dialog: IFileSaveDialog =
                CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).map_err(|error| {
                    format!("Could not create the Windows file chooser: {error}")
                })?;

            let filters = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Binary data (*.bin)"),
                    pszSpec: w!("*.bin"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("FITS table (*.fits;*.fit)"),
                    pszSpec: w!("*.fits;*.fit"),
                },
            ];
            dialog
                .SetTitle(w!("Select file to save"))
                .and_then(|_| dialog.SetFileName(w!("observation.bin")))
                .and_then(|_| dialog.SetFileTypes(&filters))
                .and_then(|_| dialog.SetFileTypeIndex(1))
                .and_then(|_| dialog.SetDefaultExtension(w!("bin")))
                .and_then(|_| {
                    dialog.SetOptions(
                        dialog.GetOptions()?
                            | FOS_FORCEFILESYSTEM
                            | FOS_PATHMUSTEXIST
                            | FOS_OVERWRITEPROMPT,
                    )
                })
                .map_err(|error| format!("Could not configure the file chooser: {error}"))?;

            match dialog.Show(None) {
                Ok(()) => {}
                Err(error) if error.code() == ERROR_CANCELLED_HRESULT => return Ok(None),
                Err(error) => return Err(format!("The file chooser failed: {error}")),
            }

            let item = dialog
                .GetResult()
                .map_err(|error| format!("Could not read the selected file: {error}"))?;
            let raw_path = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|error| format!("Could not read the selected path: {error}"))?;
            let path = raw_path
                .to_string()
                .map(PathBuf::from)
                .map_err(|error| format!("The selected path is not valid Unicode: {error}"));
            CoTaskMemFree(Some(raw_path.as_ptr().cast()));
            path.map(Some)
        })();

        CoUninitialize();
        result
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn select_save_file() -> Result<Option<PathBuf>, String> {
    Err("The save-file chooser is currently available on macOS and Windows only".to_owned())
}

fn displayed_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn configure_bundled_soapy_modules() {
    if std::env::var_os("SOAPY_SDR_PLUGIN_PATH").is_some() {
        return;
    }

    let Some(release_root) = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent()?.parent().map(Path::to_path_buf))
    else {
        return;
    };
    let module_path = release_root.join("lib/SoapySDR/modules0.8");
    if module_path.is_dir() {
        // This runs before any worker threads or SoapySDR calls are started.
        unsafe {
            std::env::set_var("SOAPY_SDR_PLUGIN_PATH", module_path);
        }
    }
}

#[derive(Debug, Parser)]
#[clap(author, about, version)]
struct Args {
    #[clap(short('f'), long("freq"), value_name("central freq in Hz"))]
    f0: f64,

    #[clap(
        short('n'),
        long("nch"),
        value_name("num of channels, must <=8192"),
        default_value("4096")
    )]
    nch: usize,

    #[clap(
        short('t'),
        long("tap"),
        value_name("pfb tap per ch"),
        default_value("4")
    )]
    ntap: usize,

    #[clap(
        short('y'),
        value_name("num of time points displayed"),
        default_value("128")
    )]
    ntime: usize,

    #[clap(short('k'), value_name("filter param k"), default_value("0.9"))]
    k: f32,

    #[clap(
        short('a'),
        value_name("number of time points to calculate mean"),
        default_value("128")
    )]
    n_average: usize,

    #[clap(long("lna"), value_name("lna gain"), default_value("5"))]
    lna: f64,

    #[clap(long("mix"), value_name("mix gain"), default_value("5"))]
    mix: f64,

    #[clap(long("vga"), value_name("vga gain"), default_value("5"))]
    vga: f64,

    #[clap(short('s'), value_name("sampling rate in MHz"), default_value("6"))]
    sampling_rate: u32,

    #[clap(short('o'), long("out"), value_name("out file name"))]
    outname: Option<String>,

    #[clap(
        short('r'),
        long("renderer"),
        value_name("renderer, wgpu or glow"),
        default_value("glow")
    )]
    renderer: String,
}

#[derive(Clone)]
struct State {
    freq: f64,
    samp_rate: f64,
    min_ch: usize,
    max_ch: usize,
    yscale_min: f64,
    yscale_max: f64,
    ntime: usize,
    nch: usize,
    spectrum_interval_sec: f64,
    device: Device,
    floor: Option<Array1<f32>>,
}

fn db(x: f64) -> f64 {
    x.log10() * 10.0
}

fn main() {
    let args = Args::parse();

    if args.sampling_rate != 3 && args.sampling_rate != 6 {
        eprintln!("Sampling rate can only be either 3 or 6 MSps");
        return;
    }
    if args.nch == 0 || args.nch > 8192 || !args.nch.is_power_of_two() {
        eprintln!("Channel count must be a power of two between 1 and 8192");
        return;
    }
    if args.ntap == 0 || args.ntime == 0 || args.n_average == 0 {
        eprintln!("PFB taps, displayed rows, and averaging count must be positive");
        return;
    }
    if !(0.0..1.0).contains(&args.k) {
        eprintln!("Filter parameter k must be in the range [0, 1)");
        return;
    }

    let sampling_rate = args.sampling_rate as f64 * 1e6;
    configure_bundled_soapy_modules();

    let device = Device::new("driver=airspy").unwrap();

    for g in device.list_gains(Direction::Rx, 0).unwrap() {
        println!("{g}");
    }

    device.set_antenna(Direction::Rx, 0, "RX").unwrap();
    device
        .set_sample_rate(Direction::Rx, 0, sampling_rate)
        .unwrap();
    device
        .set_gain_element(Direction::Rx, 0, "LNA", args.lna)
        .unwrap();
    device
        .set_gain_element(Direction::Rx, 0, "MIX", args.mix)
        .unwrap();
    device
        .set_gain_element(Direction::Rx, 0, "VGA", args.vga)
        .unwrap();

    device.set_frequency(Direction::Rx, 0, args.f0, ()).unwrap();
    let sdr_stream = device.rx_stream::<Complex<Ftype>>(&[0]).unwrap();

    let ctx = Arc::new(Mutex::new(Option::<Context>::default()));
    let ctx1 = Arc::clone(&ctx);

    let waterfall_img_buf = Arc::new(Mutex::new(Array2::<f32>::zeros((args.ntime, args.nch))));
    let spectrum_buf = Arc::new(Mutex::new(Array1::<f32>::zeros(args.nch)));
    let spectrum_interval_sec = args.nch as f64 * args.n_average as f64 / (2.0 * sampling_rate);
    let observation_metadata = ObservationMetadata {
        center_frequency_hz: args.f0,
        sample_rate_hz: sampling_rate,
        channel_count: args.nch,
        average_count: args.n_average,
        pfb_taps: args.ntap,
        time_resolution_sec: spectrum_interval_sec,
        lna_gain_db: args.lna,
        mix_gain_db: args.mix,
        vga_gain_db: args.vga,
    };
    let save_control = Arc::new(Mutex::new(SaveControl::new(
        args.outname.as_deref().map(PathBuf::from),
        observation_metadata,
    )));
    if args.outname.is_some() {
        // Preserve the original command-line behavior for .bin files. FITS
        // starts a fresh standards-compliant table because its header and row
        // count must describe one recording session.
        save_control.lock().unwrap().start_saving(true);
    }

    let wimg = waterfall_img_buf.clone();
    let sbuf = spectrum_buf.clone();

    let (tx_repaint, rx_repaint) = bounded(1);

    let rx_averaged = run_daq(sdr_stream, args.nch, args.ntap, args.n_average);
    device.set_frequency(Direction::Rx, 0, args.f0, ()).unwrap();

    let running = Arc::new(Mutex::new(true));
    let running1 = running.clone();
    ctrlc::set_handler(move || {
        println!("bye!");
        *running1.lock().unwrap() = false;
    })
    .unwrap();

    let running1 = running.clone();
    let save_writer = Arc::clone(&save_control);
    let th_display = std::thread::spawn(move || {
        let spectrum_buf = sbuf;

        let mut waterfall_buf = Array2::<f32>::ones((args.ntime, args.nch));
        let mut waterfall_buf_tmp = Array2::<f32>::ones((args.ntime, args.nch));
        let mut filtered_result = Array1::<f32>::zeros(args.nch);
        loop {
            let averaged = rx_averaged.recv().unwrap();
            if !*running1.lock().unwrap() {
                return;
            }
            save_writer
                .lock()
                .unwrap()
                .write_spectrum(averaged.as_slice().unwrap());

            filtered_result = filtered_result * args.k + &averaged * (1 as Ftype - args.k);

            assert!(filtered_result.iter().all(|&x| { x > 0.0 }));

            waterfall_buf_tmp
                .slice_mut(s![..-1, ..])
                .assign(&waterfall_buf.slice(s![1.., ..]));
            waterfall_buf_tmp.slice_mut(s![-1, ..]).assign(&averaged);
            std::mem::swap(&mut waterfall_buf, &mut waterfall_buf_tmp);

            {
                if let Ok(mut g) = spectrum_buf.try_lock() {
                    g.assign(&filtered_result);
                }

                if let Ok(mut g) = wimg.try_lock() {
                    g.assign(&waterfall_buf);
                }
            }
            if tx_repaint.is_empty() {
                tx_repaint.send(()).unwrap();
            }
        }
    });

    let running1 = running.clone();
    let _th_repaint = std::thread::spawn(move || {
        loop {
            if !*running1.lock().unwrap() {
                return;
            }
            if rx_repaint.recv().is_err() {
                println!("Data source distoryed")
            }
            let ctx2 = ctx1.lock().unwrap();
            if let Some(ref x) = *ctx2 {
                x.request_repaint();
            }
        }
    });

    let ctx1 = Arc::clone(&ctx);

    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size(INITIAL_WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE),
        renderer: match args.renderer.as_str() {
            "glow" => Renderer::Glow,
            "wgpu" => Renderer::Wgpu,
            _ => panic!("renderer can be either wgpu or glow"),
        },
        ..Default::default()
    };

    let wimg = waterfall_img_buf.clone();
    let sbuf = spectrum_buf.clone();
    let save_for_ui = Arc::clone(&save_control);
    let state = State {
        freq: args.f0,
        samp_rate: sampling_rate,
        min_ch: 0,
        max_ch: args.nch - 1,
        yscale_max: 1.0,
        yscale_min: 0.0,
        ntime: args.ntime,
        nch: args.nch,
        // ospfb2 produces two interleaved spectra per `nch` input samples.
        // One displayed row averages `n_average` of those spectra.
        spectrum_interval_sec,
        device,
        floor: None,
    };
    match eframe::run_native(
        "Waterfall",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(PlotWindow::new(
                cc,
                ctx1,
                wimg,
                sbuf,
                state,
                save_for_ui,
            )))
        }),
    ) {
        Ok(_) => {}
        Err(e) => {
            println!("{e}");
            panic!();
        }
    }

    println!("exit!");
    *running.lock().unwrap() = false;
    th_display.join().unwrap();
    save_control.lock().unwrap().stop_saving();
}

struct PlotWindow {
    pub waterfall_img: Arc<Mutex<Array2<f32>>>,
    pub spectrum_buf: Arc<Mutex<Array1<f32>>>,
    pub state: State,
    pub save_control: Arc<Mutex<SaveControl>>,
    file_dialog_receiver: Option<Receiver<FileDialogResult>>,
}

impl PlotWindow {
    fn new(
        cc: &eframe::CreationContext<'_>,
        ctx_holder: Arc<Mutex<Option<Context>>>,
        wimg: Arc<Mutex<Array2<f32>>>,
        sbuf: Arc<Mutex<Array1<f32>>>,
        state: State,
        save_control: Arc<Mutex<SaveControl>>,
    ) -> Self {
        // Disable feathering as it causes artifacts
        let context = &cc.egui_ctx;

        context.tessellation_options_mut(|tess_options| {
            tess_options.feathering = false;
        });

        context.set_visuals(Visuals::light());
        let mut ctx1 = ctx_holder.lock().unwrap();
        *ctx1 = Some(context.clone());
        Self {
            waterfall_img: wimg,
            spectrum_buf: sbuf,
            state,
            save_control,
            file_dialog_receiver: None,
        }
    }

    fn poll_file_dialog(&mut self) {
        let result = match self.file_dialog_receiver.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(
                    "The file chooser closed without returning a result".to_owned(),
                )),
            },
            None => None,
        };

        let Some(result) = result else {
            return;
        };
        self.file_dialog_receiver = None;
        match result {
            Ok(Some(path)) => self.save_control.lock().unwrap().select_path(path),
            Ok(None) => {}
            Err(error) => self.save_control.lock().unwrap().error = Some(error),
        }
    }
}

impl eframe::App for PlotWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_file_dialog();

        let (min_value, max_value) = self
            .waterfall_img
            .lock()
            .unwrap()
            .slice(s![.., self.state.min_ch..=self.state.max_ch])
            .iter()
            .fold((1e99, -1e99), |a, &v| {
                let v = v as f64;
                (if a.0 < v { a.0 } else { v }, if a.1 > v { a.1 } else { v })
            });

        TopBottomPanel::bottom("playmenu").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("min ch");

                let mut min_ch = self.state.min_ch;
                let mut max_ch = self.state.max_ch;
                if ui
                    .add(Slider::new(&mut min_ch, 0..=(self.state.nch - 1)))
                    .changed()
                {
                    self.state.min_ch = min_ch;
                    if self.state.max_ch < self.state.min_ch + 1 {
                        self.state.max_ch = min_ch + 1;
                    }
                }

                ui.label("max ch");
                if ui
                    .add(Slider::new(&mut max_ch, 0..=(self.state.nch - 1)))
                    .changed()
                {
                    self.state.max_ch = max_ch;
                    if self.state.max_ch < self.state.min_ch + 1 {
                        self.state.min_ch = max_ch - 1;
                    }
                }

                ui.label("zoom in");
                let mut yscale_max = self.state.yscale_max;
                let mut yscale_min = self.state.yscale_min;

                if ui.add(Slider::new(&mut yscale_min, 0.0..=1.0)).changed() {
                    self.state.yscale_min = yscale_min;
                    if self.state.yscale_max - self.state.yscale_min < 0.01 {
                        self.state.yscale_max = self.state.yscale_min + 0.01;
                    }
                }

                if ui.add(Slider::new(&mut yscale_max, 0.0..=1.0)).changed() {
                    self.state.yscale_max = yscale_max;
                    if self.state.yscale_max - self.state.yscale_min < 0.01 {
                        self.state.yscale_min = self.state.yscale_max - 0.01;
                    }
                }

                ui.label(format!("F={} MHz", self.state.freq / 1e6));
                if ui.button("Reset").clicked() {
                    self.state.floor = None;
                }

                let is_saving = self.save_control.lock().unwrap().is_saving();
                let file_dialog_open = self.file_dialog_receiver.is_some();
                let select_file_label = if file_dialog_open {
                    "Opening file dialog..."
                } else {
                    "Select file to save"
                };
                if ui
                    .add_enabled(
                        !is_saving && !file_dialog_open,
                        egui::Button::new(select_file_label),
                    )
                    .clicked()
                {
                    let (sender, receiver) = bounded(1);
                    self.file_dialog_receiver = Some(receiver);
                    let context = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = sender.send(select_save_file());
                        context.request_repaint();
                    });
                }

                if ui.button("Excl").clicked() {
                    self.state.floor = Some(self.spectrum_buf.lock().unwrap().clone());
                }
            });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                let (selected_path, selected_format, is_saving, error) = {
                    let save = self.save_control.lock().unwrap();
                    (
                        save.selected_path.clone(),
                        save.selected_format_name(),
                        save.is_saving(),
                        save.error.clone(),
                    )
                };

                ui.label("Save file:");
                if let Some(path) = selected_path.as_ref() {
                    ui.label(displayed_file_name(path))
                        .on_hover_text(path.display().to_string());
                    ui.weak(format!("({selected_format})"));
                } else {
                    ui.weak("No file selected");
                }

                let save_button = if is_saving {
                    ui.button("Stop saving")
                } else {
                    ui.add_enabled(selected_path.is_some(), egui::Button::new("Start to save"))
                };
                if save_button.clicked() {
                    let mut save = self.save_control.lock().unwrap();
                    if is_saving {
                        save.stop_saving();
                    } else {
                        // A file chosen through the save dialog is a new
                        // recording session, so starting replaces its contents.
                        save.start_saving(false);
                    }
                }

                if is_saving {
                    ui.colored_label(egui::Color32::DARK_GREEN, "Recording");
                }
                if let Some(error) = error {
                    ui.colored_label(egui::Color32::RED, error);
                }
            });
        });

        if min_value == max_value || min_value == 0.0 {
            CentralPanel::default().show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label("Awaiting PFB buffer being filled...");
                });
            });
            return;
        }

        CentralPanel::default().show(ctx, |ui| {
            let root_area = EguiBackend::new(ui).into_drawing_area();
            root_area.fill(&WHITE).unwrap();
            let (upper, lower) = {
                let a = root_area.split_evenly((2, 1));
                (a[0].clone(), a[1].clone())
            };

            let colormap = ViridisRGB;
            let x = self
                .waterfall_img
                .lock()
                .unwrap()
                .iter()
                .flat_map(|&v| {
                    let v = v as f64;
                    let v = v.max(min_value);
                    let v = v.min(max_value);
                    let c = colormap.get_color_normalized(db(v), db(min_value), db(max_value));
                    [c.0, c.1, c.2]
                })
                .collect::<Vec<_>>();

            let df = self.state.samp_rate / self.state.nch as f64;
            let fmin_raw = self.state.freq - self.state.samp_rate / 2.0;
            let fmax_raw = self.state.freq + self.state.samp_rate / 2.0;
            let fmin_display = self.state.min_ch as f64 * df + fmin_raw;
            let fmax_display = self.state.max_ch as f64 * df + fmin_raw;
            let waterfall_time_span = self.state.spectrum_interval_sec * self.state.ntime as f64;
            let x1 =
                ((fmin_display - fmin_raw) / self.state.samp_rate * self.state.nch as f64) as u32;
            let x2 =
                ((fmax_display - fmin_raw) / self.state.samp_rate * self.state.nch as f64) as u32;

            let mut cc = ChartBuilder::on(&upper)
                .margin_left(PLOT_SIDE_MARGIN)
                .margin_right(PLOT_SIDE_MARGIN)
                .set_label_area_size(LabelAreaPosition::Top, HORIZONTAL_LABEL_AREA_SIZE)
                .set_label_area_size(LabelAreaPosition::Left, VERTICAL_LABEL_AREA_SIZE)
                .set_label_area_size(LabelAreaPosition::Right, VERTICAL_LABEL_AREA_SIZE)
                .build_cartesian_2d(
                    (fmin_raw / 1e6)..(fmax_raw / 1e6),
                    0.0..-waterfall_time_span,
                )
                .unwrap();

            let (plot_width, plot_height) = cc.plotting_area().dim_in_pixel();
            let waterfall = DynamicImage::ImageRgb8(
                RgbImage::from_vec(self.state.nch as u32, self.state.ntime as u32, x).unwrap(),
            )
            .crop(x1, 0, x2 - x1, self.state.ntime as u32)
            .resize_exact(plot_width.max(1), plot_height.max(1), Nearest);

            let bmp: BitMapElement<_> = ((fmin_display, -waterfall_time_span), waterfall).into();

            let waterfall_y_label_formatter = |value: &f64| {
                if value.abs() < f64::EPSILON {
                    String::new()
                } else {
                    RangedCoordf64::format(value)
                }
            };
            cc.configure_mesh()
                .x_desc("Frequency (MHz)")
                .y_label_formatter(&waterfall_y_label_formatter)
                .label_style(("sans-serif", AXIS_FONT_SIZE))
                .axis_desc_style(("sans-serif", AXIS_FONT_SIZE))
                .draw()
                .unwrap();
            cc.draw_series(std::iter::once(bmp)).unwrap();

            let y_axis_unit_style = TextStyle::from(("sans-serif", AXIS_FONT_SIZE).into_font())
                .color(&BLACK)
                .transform(FontTransform::Rotate270)
                .pos(Pos::new(HPos::Center, VPos::Center));
            let spec = self.spectrum_buf.lock().unwrap();
            let spec = if let Some(ref x) = self.state.floor {
                &spec.view() / x
            } else {
                spec.to_owned()
            };
            let (min_value, max_value) = spec
                .iter()
                .enumerate()
                .filter(|&(ich, _)| ich + 10 >= self.state.min_ch && ich <= self.state.max_ch + 10)
                .fold((1e99, -1e99), |a, (_, &v)| {
                    let v = v as f64;
                    (if a.0 < v { a.0 } else { v }, if a.1 > v { a.1 } else { v })
                });
            let y1 = db(min_value) - 0.5_f64;
            let y2 = db(max_value) + 0.5_f64;
            let ys1 = (y2 - y1) * self.state.yscale_min + y1;
            let ys2 = (y2 - y1) * self.state.yscale_max + y1;

            let spectrum_x_min = fmin_display / 1e6 - 0.1;
            let spectrum_x_max = fmax_display / 1e6 + 0.1;
            let mut cc = ChartBuilder::on(&lower)
                .margin_left(PLOT_SIDE_MARGIN)
                .margin_right(PLOT_SIDE_MARGIN)
                .set_label_area_size(LabelAreaPosition::Left, VERTICAL_LABEL_AREA_SIZE)
                .set_label_area_size(LabelAreaPosition::Right, VERTICAL_LABEL_AREA_SIZE)
                .set_label_area_size(LabelAreaPosition::Bottom, HORIZONTAL_LABEL_AREA_SIZE)
                .build_cartesian_2d(spectrum_x_min..spectrum_x_max, ys1..ys2)
                .unwrap();
            cc.configure_mesh()
                .x_desc("Frequency (MHz)")
                .label_style(("sans-serif", AXIS_FONT_SIZE))
                .axis_desc_style(("sans-serif", AXIS_FONT_SIZE))
                .draw()
                .unwrap();
            cc.draw_series(LineSeries::new(
                (0..self.state.nch).map(|ich| {
                    (
                        (ich as f64 / self.state.nch as f64 * self.state.samp_rate + fmin_raw)
                            / 1e6,
                        db(spec[ich] as f64),
                    )
                }),
                &BLUE,
            ))
            .unwrap();

            let (_, root_height) = root_area.dim_in_pixel();
            root_area
                .draw(&Text::new(
                    "Time (s)",
                    (TIME_AXIS_UNIT_X, root_height as i32 / 4),
                    y_axis_unit_style.clone(),
                ))
                .unwrap();
            root_area
                .draw(&Text::new(
                    "Power (dB)",
                    (POWER_AXIS_UNIT_X, root_height as i32 * 3 / 4),
                    y_axis_unit_style,
                ))
                .unwrap();

            root_area.present().unwrap();
            let df = if ctx
                .input(|input| input.key_pressed(Key::D) | input.key_pressed(Key::ArrowUp))
            {
                0.1e6
            } else if ctx.input(|input| input.key_pressed(Key::S) | input.key_pressed(Key::PageUp))
            {
                1e6
            } else if ctx.input(|input| input.key_pressed(Key::A)) {
                5e6
            } else if ctx
                .input(|input| input.key_pressed(Key::C) | input.key_pressed(Key::ArrowDown))
            {
                -0.1e6
            } else if ctx
                .input(|input| input.key_pressed(Key::X) | input.key_pressed(Key::PageDown))
            {
                -1e6
            } else if ctx.input(|input| input.key_pressed(Key::Z)) {
                -5e6
            } else {
                0.0
            };

            if df != 0.0_f64 {
                let f = self.state.device.frequency(Direction::Rx, 0).unwrap();
                self.state
                    .device
                    .set_frequency(Direction::Rx, 0, f + df, ())
                    .unwrap();
                let f = f + df;
                self.state.freq = f;
                self.save_control.lock().unwrap().set_frequency(f);
                self.state.floor = None;
                println!("freq changed to {f}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_metadata(channel_count: usize) -> ObservationMetadata {
        ObservationMetadata {
            center_frequency_hz: 1_420_405_751.0,
            sample_rate_hz: 6_000_000.0,
            channel_count,
            average_count: 128,
            pfb_taps: 4,
            time_resolution_sec: 0.01,
            lna_gain_db: 5.0,
            mix_gain_db: 5.0,
            vga_gain_db: 5.0,
        }
    }

    #[test]
    fn save_control_writes_selected_file_and_stops_cleanly() {
        let path =
            std::env::temp_dir().join(format!("hi_observer_save_test_{}.bin", std::process::id()));
        let values = [1.25_f32, 2.5_f32, 5.0_f32];
        let mut save = SaveControl::new(Some(path.clone()), test_metadata(values.len()));

        save.start_saving(false);
        assert!(save.is_saving());
        save.write_spectrum(&values);
        save.stop_saving();
        assert!(!save.is_saving());
        assert!(save.error.is_none());

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            &bytes[..8],
            &soapy_spec_acc::binary_observation::BINARY_MAGIC
        );
        assert_eq!(
            bytes.len(),
            soapy_spec_acc::binary_observation::BINARY_HEADER_SIZE
                + soapy_spec_acc::binary_observation::BINARY_ROW_HEADER_SIZE
                + values.len() * size_of::<f32>()
        );

        std::fs::remove_file(path).unwrap();
    }
}
