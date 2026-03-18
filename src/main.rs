mod fs;

use clap::Parser;
use fs::PassthroughFS;
use fuser::MountOption;
use eframe::egui;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "nofs", about = "Mount a directory as a passthrough FUSE filesystem")]
struct Args {
    /// Source directory to mirror
    source: PathBuf,

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

    let source = std::fs::canonicalize(&args.source).expect("Failed to canonicalize source path");
    let mountpoint = args.mountpoint.clone();

    let mut options = vec![
        MountOption::FSName("nofs".to_string()),
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
    ];
    if args.allow_other {
        options.push(MountOption::AllowOther);
    }

    let fuse_fs = PassthroughFS::new(source);

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
