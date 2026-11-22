"""
Command Line Interface dispatcher for appmgr.
Supports installation, search, linking, multithreaded upgrades, rollbacks,
caching, environment management, and JSON output.
"""

import argparse
import sys
from pathlib import Path

from . import __version__
from .config import Config
from .core import Core
from .doctor import Doctor
from .ui import Colors, c, print_banner


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="appmgr",
        description="Universal Application Manager (Pacman + APT hybrid)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )

    parser.add_argument("-v", "--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument("--system", action="store_true", help="Force system-wide installation (/opt, /usr/local)")
    parser.add_argument("--user", action="store_true", help="Force user-level installation (~/.local)")
    parser.add_argument("-y", "--yes", "--noconfirm", dest="global_yes", action="store_true", help="Automatic yes to prompts")

    subparsers = parser.add_subparsers(dest="command", title="commands", metavar="<command>")

    # install
    install_parser = subparsers.add_parser(
        "install",
        aliases=["i", "in"],
        help="Install application locally, from GitHub (owner/repo[@tag]), or search",
    )
    install_parser.add_argument("package", help="Path to package, GitHub target (owner/repo[@tag]), or app name")
    install_parser.add_argument("-y", "--yes", "--noconfirm", action="store_true", help="Assume yes to all prompts")
    install_parser.add_argument("-f", "--force", action="store_true", help="Force overwrite existing installation")
    install_parser.add_argument("--repo", help="Associate package with GitHub repository (owner/repo)")
    install_parser.add_argument("--prefer", choices=["appimage", "deb", "tar", "zip", "bin"], help="Preferred asset format")
    install_parser.add_argument("--pre", action="store_true", help="Allow pre-release versions")

    # search
    search_parser = subparsers.add_parser(
        "search",
        aliases=["s", "find"],
        help="Search GitHub repositories for applications",
    )
    search_parser.add_argument("query", help="Application name or search query")

    # link
    link_parser = subparsers.add_parser(
        "link",
        help="Link an installed application to a GitHub repository for updates",
    )
    link_parser.add_argument("app", help="Installed application name or ID")
    link_parser.add_argument("repo", help="GitHub repository (owner/repo or URL)")

    # upgrade / update
    upgrade_parser = subparsers.add_parser(
        "upgrade",
        aliases=["up", "update"],
        help="Check and upgrade packages from GitHub Releases",
    )
    upgrade_parser.add_argument("app", nargs="?", help="Specific application to upgrade (optional)")
    upgrade_parser.add_argument("-c", "--check", action="store_true", help="Check for available updates without installing")
    upgrade_parser.add_argument("-y", "--yes", "--noconfirm", action="store_true", help="Assume yes to all prompts")
    upgrade_parser.add_argument("-f", "--force", action="store_true", help="Force upgrade even if pinned")
    upgrade_parser.add_argument("--prefer", choices=["appimage", "deb", "tar", "zip", "bin"], help="Preferred asset format")
    upgrade_parser.add_argument("--pre", action="store_true", help="Include pre-releases in update check")
    upgrade_parser.add_argument("--json", action="store_true", help="Output machine-readable JSON")

    # remove / uninstall
    remove_parser = subparsers.add_parser(
        "remove",
        aliases=["rm", "uninstall", "purge"],
        help="Remove an installed application",
    )
    remove_parser.add_argument("package", help="Name or ID of installed application")
    remove_parser.add_argument("-y", "--yes", "--noconfirm", action="store_true", help="Assume yes to all prompts")

    # list
    list_parser = subparsers.add_parser("list", aliases=["ls"], help="List installed applications")
    list_parser.add_argument("-t", "--type", choices=["deb", "appimage", "tar", "zip", "bin"], help="Filter by package format")
    list_parser.add_argument("--json", action="store_true", help="Output machine-readable JSON")

    # info
    info_parser = subparsers.add_parser("info", help="Display details about a package or file")
    info_parser.add_argument("target", help="Package ID or file path to inspect")

    # doctor
    subparsers.add_parser("doctor", aliases=["check"], help="Check system health, FUSE, and broken symlinks")

    # run
    run_parser = subparsers.add_parser("run", help="Launch an installed application")
    run_parser.add_argument("app", help="Application name or ID")
    run_parser.add_argument("args", nargs=argparse.REMAINDER, help="Arguments passed to application")

    # env
    env_parser = subparsers.add_parser("env", help="Manage custom environment variables for an application")
    env_parser.add_argument("app", help="Application name or ID")
    env_parser.add_argument("pairs", nargs=argparse.REMAINDER, help="Variable pairs to set (e.g. KEY=VALUE) or --unset KEY / --list")

    # args
    args_parser = subparsers.add_parser("args", help="Manage persistent launch flags for an application")
    args_parser.add_argument("app", help="Application name or ID")
    args_parser.add_argument("cli_args", nargs=argparse.REMAINDER, help="Persistent flags to pass (e.g. --ozone-platform=wayland)")

    # pin / unpin
    pin_parser = subparsers.add_parser("pin", help="Freeze an application version to skip general upgrades")
    pin_parser.add_argument("app", help="Application name or ID")

    unpin_parser = subparsers.add_parser("unpin", help="Unfreeze an application version to allow updates")
    unpin_parser.add_argument("app", help="Application name or ID")

    # rollback
    rollback_parser = subparsers.add_parser("rollback", help="Restore previous version of an application from cache")
    rollback_parser.add_argument("app", help="Application name or ID")
    rollback_parser.add_argument("-y", "--yes", "--noconfirm", action="store_true", help="Assume yes to prompt")

    # clean
    clean_parser = subparsers.add_parser("clean", help="Clean rollback backups and temporary cache")
    clean_parser.add_argument("-y", "--yes", "--noconfirm", action="store_true", help="Assume yes to prompt")

    # export
    export_parser = subparsers.add_parser("export", help="Export installed applications to a JSON manifest")
    export_parser.add_argument("file", nargs="?", default="appmgr-manifest.json", help="Destination file (default: appmgr-manifest.json)")

    # import
    import_parser = subparsers.add_parser("import", help="Install applications from a JSON manifest")
    import_parser.add_argument("file", help="Path to manifest JSON file")
    import_parser.add_argument("-y", "--yes", "--noconfirm", action="store_true", help="Assume yes to all prompts")

    # update-db
    subparsers.add_parser("update-db", help="Refresh desktop database and icon theme caches")

    return parser


def main():
    parser = create_parser()
    if len(sys.argv) == 1:
        print_banner()
        parser.print_help()
        sys.exit(0)

    args = parser.parse_args()

    config = Config(force_system=args.system, force_user=args.user)
    core = Core(config)

    assume_yes = getattr(args, "yes", False) or getattr(args, "global_yes", False)

    if args.command in ["install", "i", "in"]:
        print_banner()
        success = core.install(
            args.package,
            force=args.force,
            assume_yes=assume_yes,
            repo=args.repo,
            prefer=args.prefer,
            include_pre=args.pre,
        )
        sys.exit(0 if success else 1)

    elif args.command == "link":
        print_banner()
        success = core.link(args.app, args.repo)
        sys.exit(0 if success else 1)

    elif args.command in ["search", "s", "find"]:
        print_banner()
        core.search(args.query)

    elif args.command in ["upgrade", "up", "update"]:
        if not args.json:
            print_banner()
        success = core.upgrade(
            app_query=args.app,
            check_only=args.check,
            assume_yes=assume_yes,
            prefer=args.prefer,
            include_pre=args.pre,
            force=args.force,
            as_json=args.json,
        )
        sys.exit(0 if success else 1)

    elif args.command in ["remove", "rm", "uninstall", "purge"]:
        print_banner()
        success = core.remove(args.package, assume_yes=assume_yes)
        sys.exit(0 if success else 1)

    elif args.command in ["list", "ls"]:
        core.list_installed(filter_type=args.type, as_json=args.json)

    elif args.command == "info":
        core.info(args.target)

    elif args.command in ["doctor", "check"]:
        doctor = Doctor(config)
        doctor.run_all_checks()

    elif args.command == "run":
        core.run_app(args.app, args.args)

    elif args.command == "env":
        raw_pairs = list(args.pairs)
        unset_key = None
        list_only = False
        final_pairs = []
        i = 0
        while i < len(raw_pairs):
            p = raw_pairs[i]
            if p == "--unset" and i + 1 < len(raw_pairs):
                unset_key = raw_pairs[i + 1]
                i += 2
                continue
            elif p.startswith("--unset="):
                unset_key = p.split("=", 1)[1]
            elif p in ["--list", "-l"]:
                list_only = True
            else:
                final_pairs.append(p)
            i += 1

        success = core.env_manage(
            args.app,
            pairs=final_pairs if (final_pairs and not unset_key and not list_only) else None,
            unset_key=unset_key,
            list_only=list_only or (not final_pairs and not unset_key),
        )
        sys.exit(0 if success else 1)

    elif args.command == "args":
        raw_args = list(args.cli_args)
        clear = False
        list_only = False
        final_args = []
        for a in raw_args:
            if a == "--clear":
                clear = True
            elif a in ["--list", "-l"]:
                list_only = True
            else:
                final_args.append(a)

        success = core.args_manage(
            args.app,
            args_list=final_args if (final_args and not clear and not list_only) else None,
            clear=clear,
            list_only=list_only or (not final_args and not clear),
        )
        sys.exit(0 if success else 1)

    elif args.command == "pin":
        success = core.pin_app(args.app)
        sys.exit(0 if success else 1)

    elif args.command == "unpin":
        success = core.unpin_app(args.app)
        sys.exit(0 if success else 1)

    elif args.command == "rollback":
        print_banner()
        success = core.rollback(args.app, assume_yes=assume_yes)
        sys.exit(0 if success else 1)

    elif args.command == "clean":
        print_banner()
        success = core.clean(assume_yes=assume_yes)
        sys.exit(0 if success else 1)

    elif args.command == "export":
        success = core.export_manifest(args.file)
        sys.exit(0 if success else 1)

    elif args.command == "import":
        print_banner()
        success = core.import_manifest(args.file, assume_yes=assume_yes)
        sys.exit(0 if success else 1)

    elif args.command == "update-db":
        core.run_triggers()
        print(f"{c('::', Colors.BOLD, Colors.BRIGHT_GREEN)} Desktop database and icon caches updated.")

    else:
        parser.print_help()


if __name__ == "__main__":
    main()
