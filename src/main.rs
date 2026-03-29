mod blob;
mod fs;
mod reed_solomon;
mod store;

use clap::Parser;
use eframe::egui;
use fs::{FsEntry, FsSnapshot, NofsFS};
use fuser::{FileType, MountOption};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const FUSE_ROOT_INODE: u64 = 1;

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

struct NofsApp {
    snapshot: Arc<Mutex<FsSnapshot>>,
    cached_tree: FsSnapshot,
}

impl eframe::App for NofsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(snap) = self.snapshot.try_lock() {
            self.cached_tree = snap.clone();
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                render_dir(ui, &self.cached_tree, FUSE_ROOT_INODE);
            });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

fn children_of(snapshot: &FsSnapshot, parent_inode: u64) -> Vec<&FsEntry> {
    let mut v: Vec<&FsEntry> = snapshot
        .entries
        .iter()
        .filter(|(p, _)| *p == parent_inode)
        .map(|(_, e)| e)
        .collect();
    v.sort_by(|a, b| match (a.file_type, b.file_type) {
        (FileType::Directory, FileType::Directory) => a.name.cmp(&b.name),
        (FileType::Directory, _) => std::cmp::Ordering::Less,
        (_, FileType::Directory) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    v
}

fn render_dir(ui: &mut egui::Ui, snapshot: &FsSnapshot, parent_inode: u64) {
    for entry in children_of(snapshot, parent_inode) {
        match entry.file_type {
            FileType::Directory => {
                egui::CollapsingHeader::new(format!("📁 {}", entry.name))
                    .id_salt(entry.inode)
                    .show(ui, |ui| render_dir(ui, snapshot, entry.inode));
            }
            FileType::Symlink => {
                ui.label(format!("🔗 {}", entry.name));
            }
            _ => {
                ui.label(format!("📄 {} ({})", entry.name, human_size(entry.size)));
            }
        }
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{} B", b),
        b if b < 1024 * 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b if b < 1_073_741_824 => format!("{:.1} MiB", b as f64 / 1_048_576.0),
        b => format!("{:.1} GiB", b as f64 / 1_073_741_824.0),
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

    let shared_snapshot: Arc<Mutex<FsSnapshot>> = Arc::new(Mutex::new(FsSnapshot::default()));
    let fuse_fs = NofsFS::new(data_dir, Arc::clone(&shared_snapshot));

    if args.no_ui {
        fuser::mount2(fuse_fs, &args.mountpoint, &options).expect("Failed to mount filesystem");
    } else {
        let mount_path = mountpoint.clone();
        let snapshot_for_gui = Arc::clone(&shared_snapshot);

        std::thread::spawn(move || {
            fuser::mount2(fuse_fs, &mount_path, &options).expect("Failed to mount filesystem");
            std::process::exit(0);
        });

        let native_options = eframe::NativeOptions::default();
        eframe::run_native(
            "nofs",
            native_options,
            Box::new(move |_cc| {
                Ok(Box::new(NofsApp {
                    snapshot: snapshot_for_gui,
                    cached_tree: FsSnapshot::default(),
                }))
            }),
        )
        .expect("Failed to run eframe");

        // Window closed — unmount FUSE
        let _ = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(&mountpoint)
            .status();
    }
}
