mod blob;
mod fs;
mod reed_solomon;
mod store;

use clap::Parser;
use eframe::egui;
use fs::NofsFS;
use fuser::MountOption;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "nofs", about = "A FUSE filesystem with content-addressed storage")]
struct Args {
    /// Data directory for metadata and blobs
    data_dir: PathBuf,

    /// Directory to mount the filesystem at
    mountpoint: PathBuf,

    /// Allow other users to access the mount
    #[arg(long)]
    allow_other: bool,

    /// Enable debug logging
    #[arg(long)]
    debug: bool,

    /// Enable FUSE debug output
    #[arg(long)]
    debug_fuse: bool,

    /// Disable the GUI window
    #[arg(long)]
    no_ui: bool,
}

struct NofsApp;

impl eframe::App for NofsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("nofs");
        });
    }
}

fn main() {
    let args = Args::parse();

    if args.debug {
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Debug)
            .init();
    } else {
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .init();
    }

    // Create data directory and blobs subdirectory
    std::fs::create_dir_all(args.data_dir.join("blobs")).expect("Failed to create data directory");

    let data_dir = std::fs::canonicalize(&args.data_dir).expect("Failed to canonicalize data dir");
    let mountpoint = args.mountpoint.clone();

    let mut options = vec![
        MountOption::FSName("nofs".to_string()),
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
    ];
    if args.allow_other {
        options.push(MountOption::AllowOther);
    }

    let fuse_fs = NofsFS::new(data_dir);

    if args.no_ui {
        fuser::mount2(fuse_fs, &args.mountpoint, &options).expect("Failed to mount filesystem");
    } else {
        let mount_path = mountpoint.clone();

        std::thread::spawn(move || {
            fuser::mount2(fuse_fs, &mount_path, &options).expect("Failed to mount filesystem");
            std::process::exit(0);
        });

        let native_options = eframe::NativeOptions::default();
        eframe::run_native(
            "nofs",
            native_options,
            Box::new(|_cc| Ok(Box::new(NofsApp))),
        )
        .expect("Failed to run eframe");

        // Window closed — unmount FUSE
        let _ = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(&mountpoint)
            .status();
    }
}
