"""
UI Engine for appmgr: Pacman + APT Hybrid Interface.
Combines Pacman's animated candy-eating progress bar, double colons ::,
and sleek prompts with APT's detailed dependency analysis, triggers, and summaries.
"""

import os
import sys
import time
import shutil
import threading
from typing import Optional, List, Dict, Any


# Terminal color escape codes
class Colors:
    RESET = "\033[0m"
    BOLD = "\033[1m"
    DIM = "\033[2m"
    ITALIC = "\033[3m"
    UNDERLINE = "\033[4m"

    # Standard colors
    BLACK = "\033[30m"
    RED = "\033[31m"
    GREEN = "\033[32m"
    YELLOW = "\033[33m"
    BLUE = "\033[34m"
    MAGENTA = "\033[35m"
    CYAN = "\033[36m"
    WHITE = "\033[37m"

    # Bright colors
    BRIGHT_BLACK = "\033[90m"
    BRIGHT_RED = "\033[91m"
    BRIGHT_GREEN = "\033[92m"
    BRIGHT_YELLOW = "\033[93m"
    BRIGHT_BLUE = "\033[94m"
    BRIGHT_MAGENTA = "\033[95m"
    BRIGHT_CYAN = "\033[96m"
    BRIGHT_WHITE = "\033[97m"

    # Backgrounds
    BG_BLUE = "\033[44m"
    BG_MAGENTA = "\033[45m"
    BG_CYAN = "\033[46m"


# Formatting helpers
def c(text: str, *styles: str) -> str:
    """Colorize text with given styles."""
    if not sys.stdout.isatty():
        return text
    return "".join(styles) + text + Colors.RESET


def pacman_prompt(msg: str) -> str:
    """Pacman style header prefix: :: """
    return f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c(msg, Colors.BOLD, Colors.BRIGHT_WHITE)}"


def apt_prefix(status: str = "INFO") -> str:
    """APT style status prefix badge."""
    badges = {
        "OK": c("[  OK  ]", Colors.BOLD, Colors.BRIGHT_GREEN),
        "DONE": c("[ DONE ]", Colors.BOLD, Colors.BRIGHT_GREEN),
        "INFO": c("[ INFO ]", Colors.BOLD, Colors.BRIGHT_CYAN),
        "WARN": c("[ WARN ]", Colors.BOLD, Colors.BRIGHT_YELLOW),
        "FAIL": c("[ FAIL ]", Colors.BOLD, Colors.BRIGHT_RED),
        "WORK": c("[ WORK ]", Colors.BOLD, Colors.BRIGHT_MAGENTA),
        "TRIG": c("[ TRIG ]", Colors.BOLD, Colors.BRIGHT_BLUE),
    }
    return badges.get(status, f"[{status}]")


class PacmanBar:
    """
    Pacman animated candy eating progress bar (ILoveCandy style).
    Alternates between mouth open and mouth closed eating candy dots.
    """

    def __init__(self, total: int, prefix: str = "", unit: str = "files"):
        self.total = max(1, total)
        self.current = 0
        self.prefix = prefix
        self.unit = unit
        self.start_time = time.time()
        self.frame = 0
        self.is_tty = sys.stdout.isatty()
        self.last_render_len = 0

    def update(self, current: int, extra_text: str = ""):
        self.current = min(current, self.total)
        self.frame += 1

        if not self.is_tty:
            return

        cols = shutil.get_terminal_size(fallback=(80, 24)).columns
        ratio = self.current / self.total
        percent = int(ratio * 100)

        # Pacman animation frames
        # C = mouth open, c = mouth small, o = mouth almost closed
        pacman_chars = ["C", "c", "O", "c"]
        pacman_mouth = pacman_chars[self.frame % len(pacman_chars)]

        # Bar width calculation
        fixed_part = f" {self.prefix} {percent:3d}% [{self.current}/{self.total} {self.unit}] {extra_text}"
        available_bar_width = max(10, min(36, cols - len(fixed_part) - 10))

        completed_chars = int(available_bar_width * ratio)
        remaining_chars = available_bar_width - completed_chars

        # Candy pellets
        pellets = ["•", "·", "•", "·"]
        pellet_char = pellets[(self.frame // 2) % len(pellets)]

        bar_body = c("#" * completed_chars, Colors.BOLD, Colors.BRIGHT_YELLOW)

        if completed_chars >= available_bar_width:
            pacman_sprite = c(" (•‿•)", Colors.BOLD, Colors.BRIGHT_GREEN)
            candy_trail = ""
        else:
            pacman_sprite = c(f" {pacman_mouth}", Colors.BOLD, Colors.BRIGHT_YELLOW)
            trail_len = max(0, remaining_chars - 1)
            candy_trail = c(f" {pellet_char} " * (trail_len // 2), Colors.BRIGHT_WHITE)
            if len(candy_trail) > trail_len:
                candy_trail = candy_trail[:trail_len]

        rendered = (
            f"\r{c('(', Colors.BOLD, Colors.BRIGHT_BLUE)}"
            f"{bar_body}{pacman_sprite}{candy_trail}"
            f"{c(')', Colors.BOLD, Colors.BRIGHT_BLUE)} "
            f"{c(f'{percent:3d}%', Colors.BOLD, Colors.BRIGHT_CYAN)} "
            f"{c(f'[{self.current}/{self.total} {self.unit}]', Colors.DIM, Colors.WHITE)} "
            f"{c(extra_text, Colors.BRIGHT_WHITE)}"
        )

        pad = max(0, self.last_render_len - len(rendered))
        sys.stdout.write(rendered + (" " * pad))
        sys.stdout.flush()
        self.last_render_len = len(rendered)

    def finish(self, final_text: str = "Done"):
        if self.is_tty:
            cols = shutil.get_terminal_size(fallback=(80, 24)).columns
            ratio = 1.0
            percent = 100
            bar_width = 30
            bar_body = c("#" * bar_width, Colors.BOLD, Colors.BRIGHT_GREEN)
            sprite = c(" (•‿•)", Colors.BOLD, Colors.BRIGHT_GREEN)

            rendered = (
                f"\r{c('(', Colors.BOLD, Colors.BRIGHT_BLUE)}"
                f"{bar_body}{sprite}"
                f"{c(')', Colors.BOLD, Colors.BRIGHT_BLUE)} "
                f"{c('100%', Colors.BOLD, Colors.BRIGHT_GREEN)} "
                f"{c(f'[{self.total}/{self.total} {self.unit}]', Colors.DIM, Colors.WHITE)} "
                f"{c(final_text, Colors.BOLD, Colors.BRIGHT_GREEN)}"
            )
            pad = max(0, self.last_render_len - len(rendered))
            sys.stdout.write(rendered + (" " * pad) + "\n")
            sys.stdout.flush()


class Spinner:
    """Animated CLI Spinner for indeterminate operations."""

    def __init__(self, message: str):
        self.message = message
        self.running = False
        self.thread = None
        self.frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        self.is_tty = sys.stdout.isatty()

    def _spin(self):
        idx = 0
        while self.running:
            frame = self.frames[idx % len(self.frames)]
            sys.stdout.write(
                f"\r{apt_prefix('WORK')} {c(frame, Colors.BOLD, Colors.BRIGHT_CYAN)} {self.message}"
            )
            sys.stdout.flush()
            idx += 1
            time.sleep(0.08)

    def __enter__(self):
        if self.is_tty:
            self.running = True
            self.thread = threading.Thread(target=self._spin, daemon=True)
            self.thread.start()
        else:
            sys.stdout.write(f"{self.message}...\n")
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        if self.is_tty and self.running:
            self.running = False
            if self.thread:
                self.thread.join()
            sys.stdout.write("\r" + " " * (len(self.message) + 20) + "\r")
            sys.stdout.flush()


def print_banner():
    """Display modern stylized AppMgr banner."""
    banner = f"""{Colors.BOLD}{Colors.BRIGHT_CYAN}
   ___       ___       __  ___  _______  ___ 
  / _ | ___ / _ \\ __ _/  |/  / / ___/ _ \\/ _ \\
 / __ |/ _ / ___//  ' / /|_/ / / (_ / , _/ // /
/_/ |_| ._/_/   /_/_/_/_/  /_/  \\___/_/|_/____/ 
     /_/   {Colors.BRIGHT_YELLOW}Universal .deb & .AppImage Engine{Colors.RESET}
"""
    if sys.stdout.isatty():
        sys.stdout.write(banner)
        sys.stdout.flush()


def apt_preflight_animation():
    """Simulate the classic APT reading package database steps."""
    steps = [
        "Reading package lists...",
        "Building dependency tree...",
        "Reading state information...",
    ]
    for step in steps:
        sys.stdout.write(c(step, Colors.WHITE) + " ")
        sys.stdout.flush()
        time.sleep(0.07)
        sys.stdout.write(c("Done\n", Colors.BOLD, Colors.BRIGHT_GREEN))
        sys.stdout.flush()


def confirm_dialog(prompt_text: str, default: bool = True) -> bool:
    """Pacman style confirmation prompt: :: Proceed with transaction? [Y/n] """
    opts = "[Y/n]" if default else "[y/N]"
    prompt_str = f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c(prompt_text, Colors.BOLD, Colors.BRIGHT_WHITE)} {c(opts, Colors.BRIGHT_YELLOW)} "
    try:
        res = input(prompt_str).strip().lower()
        if not res:
            return default
        return res in ["y", "yes", "д", "да"]
    except (KeyboardInterrupt, EOFError):
        sys.stdout.write("\n")
        return False


def print_apt_transaction_summary(
    packages_to_install: List[Dict[str, Any]],
    packages_to_remove: Optional[List[Dict[str, Any]]] = None,
):
    """Prints the APT-style transaction overview."""
    print()
    if packages_to_install:
        print(c("The following NEW packages will be installed:", Colors.BOLD, Colors.BRIGHT_WHITE))
        for p in packages_to_install:
            pkg_type = p.get("format", "pkg").upper()
            name = p.get("name", "unknown")
            version = p.get("version", "unknown")
            size_mb = p.get("size", 0) / (1024 * 1024)
            print(
                f"  {c(f'[{pkg_type}]', Colors.BOLD, Colors.BRIGHT_MAGENTA)} "
                f"{c(name, Colors.BOLD, Colors.BRIGHT_GREEN)} "
                f"({c(version, Colors.BRIGHT_YELLOW)}) "
                f"{c(f'~{size_mb:.1f} MB', Colors.DIM, Colors.WHITE)}"
            )

    if packages_to_remove:
        print(c("The following packages will be REMOVED:", Colors.BOLD, Colors.BRIGHT_RED))
        for p in packages_to_remove:
            pkg_type = p.get("format", "pkg").upper()
            name = p.get("name", "unknown")
            version = p.get("version", "unknown")
            print(
                f"  {c(f'[{pkg_type}]', Colors.BOLD, Colors.RED)} "
                f"{c(name, Colors.BOLD, Colors.BRIGHT_WHITE)} "
                f"({c(version, Colors.DIM, Colors.WHITE)})"
            )

    inst_count = len(packages_to_install or [])
    rem_count = len(packages_to_remove or [])
    total_size = sum(p.get("size", 0) for p in (packages_to_install or [])) / (1024 * 1024)

    print()
    print(
        f"{c('0', Colors.BOLD, Colors.WHITE)} upgraded, "
        f"{c(str(inst_count), Colors.BOLD, Colors.BRIGHT_GREEN)} newly installed, "
        f"{c(str(rem_count), Colors.BOLD, Colors.BRIGHT_RED)} to remove and "
        f"{c('0', Colors.BOLD, Colors.WHITE)} not upgraded."
    )
    if inst_count > 0:
        print(f"Estimated disk space to be used: {c(f'{total_size * 1.5:.1f} MB', Colors.BOLD, Colors.BRIGHT_CYAN)}")
    print()


def print_apt_upgrade_summary(upgrades: List[Dict[str, Any]]):
    """Prints the APT-style upgrade summary."""
    print()
    print(c("The following packages will be UPGRADED:", Colors.BOLD, Colors.BRIGHT_WHITE))
    total_size = 0
    for u in upgrades:
        pkg_type = u.get("format", "pkg").upper()
        name = u.get("name", "unknown")
        old_v = u.get("old_version", "?")
        new_v = u.get("new_version", "?")
        repo = u.get("repo", "")
        size = u.get("size", 0)
        total_size += size
        size_mb = size / (1024 * 1024)
        repo_str = f" [{c(repo, Colors.DIM, Colors.WHITE)}]" if repo else ""
        print(
            f"  {c(f'[{pkg_type}]', Colors.BOLD, Colors.BRIGHT_MAGENTA)} "
            f"{c(name, Colors.BOLD, Colors.BRIGHT_GREEN)} "
            f"({c(old_v, Colors.DIM, Colors.WHITE)} -> {c(new_v, Colors.BOLD, Colors.BRIGHT_YELLOW)})"
            f"{repo_str} {c(f'~{size_mb:.1f} MB', Colors.DIM, Colors.WHITE)}"
        )
    print()
    print(
        f"{c(str(len(upgrades)), Colors.BOLD, Colors.BRIGHT_GREEN)} upgraded, "
        f"{c('0', Colors.BOLD, Colors.WHITE)} newly installed, "
        f"{c('0', Colors.BOLD, Colors.WHITE)} to remove and "
        f"{c('0', Colors.BOLD, Colors.WHITE)} not upgraded."
    )
    if total_size > 0:
        print(f"Need to get {c(f'{total_size / (1024*1024):.1f} MB', Colors.BOLD, Colors.BRIGHT_CYAN)} of archives.")
    print()


def prompt_select_asset(candidates: List[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
    """Interactive asset selector when multiple matching assets exist."""
    print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c('Multiple matching assets found:', Colors.BOLD, Colors.BRIGHT_WHITE)}")
    for idx, cand in enumerate(candidates, 1):
        pkg_type = cand["format"].upper()
        size_mb = cand["size"] / (1024 * 1024)
        print(
            f"   {c(f'{idx})', Colors.BOLD, Colors.BRIGHT_YELLOW)} "
            f"{c(cand['name'], Colors.BOLD, Colors.WHITE)} "
            f"{c(f'[{pkg_type}]', Colors.BOLD, Colors.BRIGHT_MAGENTA)} "
            f"({size_mb:.1f} MB)"
        )
    default_choice = 1
    prompt_str = f"{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c(f'Select asset [1-{len(candidates)}] (default {default_choice}):', Colors.BOLD, Colors.BRIGHT_WHITE)} "
    try:
        val = input(prompt_str).strip()
        if not val:
            return candidates[default_choice - 1]
        choice = int(val)
        if 1 <= choice <= len(candidates):
            return candidates[choice - 1]
    except (ValueError, KeyboardInterrupt, EOFError):
        pass
    return candidates[0]


def prompt_select_repository(repos: List[Dict[str, Any]], query: str) -> Optional[Dict[str, Any]]:
    """Interactive repository selector for search results."""
    print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c('GitHub Search Results for', Colors.BOLD, Colors.BRIGHT_WHITE)} '{c(query, Colors.BOLD, Colors.BRIGHT_CYAN)}' {c('(sorted by stars):', Colors.DIM, Colors.WHITE)}")
    cols = shutil.get_terminal_size(fallback=(80, 24)).columns

    for idx, repo in enumerate(repos, 1):
        name = repo.get("full_name", "")
        stars = repo.get("stargazers_count", 0)
        desc = (repo.get("description") or "No description provided").strip().replace("\n", " ")

        star_str = f"⭐ {stars:,}"
        prefix = f"   {c(f'{idx})', Colors.BOLD, Colors.BRIGHT_YELLOW)} {c(name.ljust(32), Colors.BOLD, Colors.BRIGHT_GREEN)} {c(star_str.rjust(11), Colors.BRIGHT_YELLOW)}  "
        clean_prefix_len = 3 + len(str(idx)) + 2 + 32 + 1 + 11 + 2
        avail_desc = max(20, cols - clean_prefix_len - 2)
        if len(desc) > avail_desc:
            desc = desc[:avail_desc - 3] + "..."
        print(f"{prefix}{c(desc, Colors.DIM, Colors.WHITE)}")

    print(f"   {c('0)', Colors.BOLD, Colors.BRIGHT_RED)} {c('Cancel / Exit', Colors.DIM, Colors.WHITE)}")

    default_choice = 1
    prompt_str = f"\n{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c(f'Select repository [1-{len(repos)}] or 0 to cancel (default {default_choice}):', Colors.BOLD, Colors.BRIGHT_WHITE)} "
    try:
        val = input(prompt_str).strip().lower()
        if not val:
            return repos[default_choice - 1]
        if val in ["0", "q", "exit", "cancel", "n", "no"]:
            return None
        choice = int(val)
        if 1 <= choice <= len(repos):
            return repos[choice - 1]
    except (ValueError, KeyboardInterrupt, EOFError):
        pass
    return None


def print_search_results(repos: List[Dict[str, Any]], query: str):
    """Print repository search results table."""
    print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_BLUE)} {c('GitHub Search Results for', Colors.BOLD, Colors.BRIGHT_WHITE)} '{c(query, Colors.BOLD, Colors.BRIGHT_CYAN)}':\n")
    cols = shutil.get_terminal_size(fallback=(80, 24)).columns

    for idx, repo in enumerate(repos, 1):
        name = repo.get("full_name", "")
        stars = repo.get("stargazers_count", 0)
        desc = (repo.get("description") or "No description").strip().replace("\n", " ")
        star_str = f"⭐ {stars:,}"
        prefix = f"  {c(f'{idx}.', Colors.BOLD, Colors.BRIGHT_YELLOW)} {c(name.ljust(32), Colors.BOLD, Colors.BRIGHT_GREEN)} {c(star_str.rjust(11), Colors.BRIGHT_YELLOW)}  "
        clean_prefix_len = 2 + len(str(idx)) + 2 + 32 + 1 + 11 + 2
        avail_desc = max(20, cols - clean_prefix_len - 2)
        if len(desc) > avail_desc:
            desc = desc[:avail_desc - 3] + "..."
        print(f"{prefix}{c(desc, Colors.DIM, Colors.WHITE)}")

    print(f"\n{c('::', Colors.BOLD, Colors.BRIGHT_CYAN)} Install any application via: {c('appmgr install <owner/repo>', Colors.BOLD, Colors.BRIGHT_YELLOW)}\n")


def print_table(headers: List[str], rows: List[List[str]]):
    """Print an attractive, aligned ASCII table."""
    if not rows:
        print(c("  (No packages found)", Colors.DIM, Colors.WHITE))
        return

    # Calculate column widths
    col_widths = [len(h) for h in headers]
    for row in rows:
        for idx, cell in enumerate(row):
            # strip ansi length if needed
            clean_cell = cell
            col_widths[idx] = max(col_widths[idx], len(clean_cell))

    # Header line
    header_str = "  ".join(
        c(headers[i].ljust(col_widths[i]), Colors.BOLD, Colors.BRIGHT_CYAN)
        for i in range(len(headers))
    )
    separator = "  ".join("─" * col_widths[i] for i in range(len(headers)))

    print(header_str)
    print(c(separator, Colors.DIM, Colors.BRIGHT_BLUE))

    for row in rows:
        row_str = "  ".join(row[i].ljust(col_widths[i]) for i in range(len(row)))
        print(row_str)
