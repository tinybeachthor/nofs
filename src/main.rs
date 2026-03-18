mod fs;

use clap::Parser;
use fs::PassthroughFS;
use fuser::MountOption;
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

    let mut options = vec![
        MountOption::FSName("nofs".to_string()),
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
    ];
    if args.allow_other {
        options.push(MountOption::AllowOther);
    }

    let fs = PassthroughFS::new(source);
    fuser::mount2(fs, &args.mountpoint, &options).expect("Failed to mount filesystem");
}
