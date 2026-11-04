mod app;
mod settings;
mod startup;

fn main() -> eframe::Result {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("dory-gui {}", env!("DORY_VERSION"));
        return Ok(());
    }
    let launch = startup::parse_args(args).map_err(|e| {
        eprintln!("dory-gui: {e}\nusage: dory-gui [--demo] [--model PATH] [--workspace DIR]");
        eframe::Error::AppCreation(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            e,
        )))
    })?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1050.0, 760.0])
            .with_min_inner_size([650.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Dory",
        options,
        Box::new(move |cc| Ok(Box::new(app::DoryApp::new(cc, launch)))),
    )
}
