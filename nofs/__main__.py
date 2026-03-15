import argparse
import logging

import trio

import pyfuse3

from nofs.fs import PassthroughFS


def main():
    parser = argparse.ArgumentParser(
        prog="nofs",
        description="Mount a directory as a passthrough FUSE filesystem",
    )
    parser.add_argument("source", help="Source directory to mirror")
    parser.add_argument("mountpoint", help="Directory to mount the filesystem at")
    parser.add_argument(
        "--allow-other",
        action="store_true",
        default=False,
        help="Allow other users to access the mount (requires user_allow_other in /etc/fuse.conf)",
    )
    parser.add_argument(
        "--debug",
        action="store_true",
        default=False,
        help="Enable debug logging",
    )
    parser.add_argument(
        "--debug-fuse",
        action="store_true",
        default=False,
        help="Enable FUSE debug output",
    )
    args = parser.parse_args()

    if args.debug:
        logging.basicConfig(level=logging.DEBUG)
    else:
        logging.basicConfig(level=logging.INFO)

    operations = PassthroughFS(args.source)

    fuse_options = set(pyfuse3.default_options)
    fuse_options.add("fsname=nofs")
    if args.allow_other:
        fuse_options.add("allow_other")
    if args.debug_fuse:
        fuse_options.add("debug")

    pyfuse3.init(operations, args.mountpoint, fuse_options)
    try:
        trio.run(pyfuse3.main)
    except KeyboardInterrupt:
        pass
    finally:
        pyfuse3.close()


if __name__ == "__main__":
    main()
