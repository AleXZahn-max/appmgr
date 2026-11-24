use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct Colors;

#[allow(dead_code)]
impl Colors {
    pub const RESET: &'static str = "\x1b[0m";
    pub const BOLD: &'static str = "\x1b[1m";
    pub const DIM: &'static str = "\x1b[2m";

    pub const RED: &'static str = "\x1b[31m";
    pub const GREEN: &'static str = "\x1b[32m";
    pub const YELLOW: &'static str = "\x1b[33m";
    pub const BLUE: &'static str = "\x1b[34m";
    pub const MAGENTA: &'static str = "\x1b[35m";
    pub const CYAN: &'static str = "\x1b[36m";
    pub const WHITE: &'static str = "\x1b[37m";
    pub const GRAY: &'static str = "\x1b[90m";

    pub const BRIGHT_RED: &'static str = "\x1b[91m";
    pub const BRIGHT_GREEN: &'static str = "\x1b[92m";
    pub const BRIGHT_YELLOW: &'static str = "\x1b[93m";
    pub const BRIGHT_BLUE: &'static str = "\x1b[94m";
    pub const BRIGHT_MAGENTA: &'static str = "\x1b[95m";
    pub const BRIGHT_CYAN: &'static str = "\x1b[96m";
    pub const BRIGHT_WHITE: &'static str = "\x1b[97m";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

static COLOR_MODE: AtomicU8 = AtomicU8::new(0);

pub fn set_color_mode(mode: ColorMode) {
    let val = match mode {
        ColorMode::Auto => 0,
        ColorMode::Always => 1,
        ColorMode::Never => 2,
    };
    COLOR_MODE.store(val, Ordering::Relaxed);
}

pub fn get_color_mode() -> ColorMode {
    match COLOR_MODE.load(Ordering::Relaxed) {
        1 => ColorMode::Always,
        2 => ColorMode::Never,
        _ => ColorMode::Auto,
    }
}

pub fn env_disables_color() -> bool {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let clicolor_zero = std::env::var("CLICOLOR").as_deref() == Ok("0");
    no_color || clicolor_zero
}

pub fn should_color_for(
    is_tty: bool,
    mode: ColorMode,
    no_color: bool,
    clicolor_zero: bool,
) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => {
            if no_color || clicolor_zero {
                false
            } else {
                is_tty
            }
        }
    }
}

pub fn should_color_stdout() -> bool {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let clicolor_zero = std::env::var("CLICOLOR").as_deref() == Ok("0");
    should_color_for(
        io::stdout().is_terminal(),
        get_color_mode(),
        no_color,
        clicolor_zero,
    )
}

pub fn should_color_stderr() -> bool {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let clicolor_zero = std::env::var("CLICOLOR").as_deref() == Ok("0");
    should_color_for(
        io::stderr().is_terminal(),
        get_color_mode(),
        no_color,
        clicolor_zero,
    )
}

pub fn c_if(enabled: bool, text: &str, styles: &[&str]) -> String {
    if !enabled || styles.is_empty() {
        return text.to_string();
    }
    format!("{}{}{}", styles.join(""), text, Colors::RESET)
}

pub fn c(text: &str, styles: &[&str]) -> String {
    c_if(should_color_stdout(), text, styles)
}

pub fn c_stderr(text: &str, styles: &[&str]) -> String {
    c_if(should_color_stderr(), text, styles)
}

/// Returns a prefix of `s` containing at most `max_chars` Unicode scalar values.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[allow(dead_code)]
pub fn pacman_prompt(msg: &str) -> String {
    format!(
        "{} {}",
        c("::", &[Colors::BOLD, Colors::CYAN]),
        c(msg, &[Colors::BOLD, Colors::WHITE])
    )
}

fn format_apt_prefix<F>(status: &str, colorize: F) -> String
where
    F: Fn(&str, &[&str]) -> String,
{
    match status {
        "OK" => colorize("[ OK ]", &[Colors::BOLD, Colors::GREEN]),
        "DONE" => colorize("[DONE]", &[Colors::BOLD, Colors::GREEN]),
        "INFO" => colorize("[INFO]", &[Colors::BOLD, Colors::CYAN]),
        "WARN" => colorize("[WARN]", &[Colors::BOLD, Colors::YELLOW]),
        "FAIL" => colorize("[FAIL]", &[Colors::BOLD, Colors::RED]),
        "WORK" => colorize("[WORK]", &[Colors::BOLD, Colors::BLUE]),
        "TRIG" => colorize("[TRIG]", &[Colors::BOLD, Colors::BLUE]),
        "GET" => colorize("[GET ]", &[Colors::BOLD, Colors::CYAN]),
        _ => format!("[{:<4}]", truncate_chars(status, 4)),
    }
}

pub fn apt_prefix(status: &str) -> String {
    format_apt_prefix(status, c)
}

pub fn apt_prefix_err(status: &str) -> String {
    format_apt_prefix(status, c_stderr)
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.2} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{} B", bytes)
    }
}

pub struct PacmanBar {
    pub total_bytes: u64,
    pub current_bytes: u64,
    pub prefix: String,
    pub frame: usize,
    is_tty: bool,
    started_at: Instant,
    last_render: Option<Instant>,
}

impl PacmanBar {
    const RENDER_THROTTLE: Duration = Duration::from_millis(65);

    pub fn new(total_bytes: u64, prefix: &str) -> Self {
        Self {
            total_bytes,
            current_bytes: 0,
            prefix: prefix.to_string(),
            frame: 0,
            is_tty: io::stdout().is_terminal(),
            started_at: Instant::now(),
            last_render: None,
        }
    }

    pub fn update(&mut self, current_bytes: u64) {
        self.current_bytes = if self.total_bytes > 0 {
            current_bytes.min(self.total_bytes)
        } else {
            current_bytes
        };

        if !self.is_tty {
            return;
        }

        if let Some(last) = self.last_render {
            if last.elapsed() < Self::RENDER_THROTTLE {
                return;
            }
        }
        self.last_render = Some(Instant::now());
        self.frame = self.frame.wrapping_add(1);

        let cols = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(80);
        let elapsed = self.started_at.elapsed().as_secs_f64();
        let line = self.render_line_with_elapsed(cols, false, None, should_color_stdout(), elapsed);
        print!("\r{}\x1b[K", line);
        let _ = io::stdout().flush();
    }

    pub fn finish(&mut self, final_text: &str) {
        if self.total_bytes > 0 && self.current_bytes < self.total_bytes {
            self.current_bytes = self.total_bytes;
        }
        if self.is_tty {
            let cols = terminal_size::terminal_size()
                .map(|(w, _)| w.0 as usize)
                .unwrap_or(80);
            let elapsed = self.started_at.elapsed().as_secs_f64();
            let line = self.render_line_with_elapsed(
                cols,
                true,
                Some(final_text),
                should_color_stdout(),
                elapsed,
            );
            print!("\r{}\x1b[K\n", line);
            let _ = io::stdout().flush();
        } else if self.current_bytes > 0 {
            let label = if final_text.is_empty() {
                self.prefix.as_str()
            } else {
                final_text
            };
            println!(
                "{} {} ({})",
                apt_prefix("GET"),
                label,
                c(&format_bytes(self.current_bytes), &[Colors::DIM])
            );
        }
    }

    pub fn render_line_with_elapsed(
        &self,
        cols: usize,
        finished: bool,
        status_text: Option<&str>,
        colorize: bool,
        elapsed_secs: f64,
    ) -> String {
        let cols = cols.max(20);
        let mib_per_sec = if elapsed_secs >= 0.01 {
            (self.current_bytes as f64 / (1024.0 * 1024.0)) / elapsed_secs
        } else {
            0.0
        };
        let speed_str = format!("{:.2} MiB/s", mib_per_sec);

        let (pct_str, bytes_str, ratio_opt) = if finished {
            let final_bytes = if self.total_bytes > 0 {
                self.total_bytes
            } else {
                self.current_bytes
            };
            ("100%".to_string(), format_bytes(final_bytes), Some(1.0))
        } else if self.total_bytes > 0 {
            let ratio = (self.current_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0);
            let pct = (ratio * 100.0) as usize;
            (
                format!("{:3}%", pct),
                format!(
                    "{}/{}",
                    format_bytes(self.current_bytes),
                    format_bytes(self.total_bytes)
                ),
                Some(ratio),
            )
        } else {
            (" ...".to_string(), format_bytes(self.current_bytes), None)
        };

        let raw_label = if finished {
            match status_text {
                Some(s) if !s.is_empty() => s,
                _ => self.prefix.as_str(),
            }
        } else {
            self.prefix.as_str()
        };

        let stats_plain = format!("{} [{}] ({})", pct_str, bytes_str, speed_str);
        let stats_len = stats_plain.chars().count();

        // Layout: "<label> [<bar>] <stats>" or "<label> <stats>" on extreme narrow terminals
        let min_bar = 8usize;
        let max_bar = 28usize;

        let max_label_budget = cols.saturating_sub(stats_len + min_bar + 4);
        let label_plain = truncate_with_ellipsis(raw_label, max_label_budget.max(4));
        let label_len = label_plain.chars().count();

        let overhead = label_len + 1 + 2 + 1 + stats_len;
        if cols <= overhead + 4 {
            // Very narrow fallback: omit bar track so total width never exceeds cols
            let avail_label = cols.saturating_sub(stats_len + 1);
            let short_label = truncate_with_ellipsis(raw_label, avail_label);
            let combined_plain = if short_label.is_empty() {
                truncate_with_ellipsis(&stats_plain, cols)
            } else {
                format!("{} {}", short_label, stats_plain)
            };
            return truncate_with_ellipsis(&combined_plain, cols);
        }

        let bar_width = (cols - overhead).clamp(min_bar, max_bar);

        let bar_styled = match ratio_opt {
            Some(ratio) => {
                let filled = ((bar_width as f64) * ratio).round() as usize;
                let filled = filled.min(bar_width);
                if finished || filled >= bar_width {
                    let style = if finished {
                        &[Colors::BOLD, Colors::GREEN][..]
                    } else {
                        &[Colors::BOLD, Colors::CYAN][..]
                    };
                    c_if(colorize, &"━".repeat(bar_width), style)
                } else if filled == 0 {
                    if self.current_bytes > 0 {
                        format!(
                            "{}{}",
                            c_if(colorize, "╸", &[Colors::BOLD, Colors::CYAN]),
                            c_if(colorize, &"─".repeat(bar_width - 1), &[Colors::DIM])
                        )
                    } else {
                        c_if(colorize, &"─".repeat(bar_width), &[Colors::DIM])
                    }
                } else {
                    let solid = "━".repeat(filled - 1);
                    let rem = "─".repeat(bar_width - filled);
                    format!(
                        "{}{}{}",
                        c_if(colorize, &solid, &[Colors::BOLD, Colors::CYAN]),
                        c_if(colorize, "╸", &[Colors::BOLD, Colors::BLUE]),
                        c_if(colorize, &rem, &[Colors::DIM])
                    )
                }
            }
            None => {
                let pulse_w = (bar_width / 4).clamp(2, 5);
                let travel = bar_width.saturating_sub(pulse_w);
                let pos = if travel == 0 {
                    0
                } else {
                    let cycle = travel * 2;
                    let step = self.frame % cycle;
                    if step <= travel {
                        step
                    } else {
                        cycle - step
                    }
                };
                let left = "─".repeat(pos);
                let pulse = if pulse_w >= 2 {
                    format!("{}╸", "━".repeat(pulse_w - 1))
                } else {
                    "━".repeat(pulse_w)
                };
                let right = "─".repeat(bar_width.saturating_sub(pos + pulse_w));
                format!(
                    "{}{}{}",
                    c_if(colorize, &left, &[Colors::DIM]),
                    c_if(colorize, &pulse, &[Colors::BOLD, Colors::CYAN]),
                    c_if(colorize, &right, &[Colors::DIM])
                )
            }
        };

        let label_styled = if finished {
            c_if(colorize, &label_plain, &[Colors::BOLD, Colors::GREEN])
        } else {
            c_if(colorize, &label_plain, &[Colors::WHITE])
        };
        let pct_styled = if finished {
            c_if(colorize, &pct_str, &[Colors::BOLD, Colors::GREEN])
        } else {
            c_if(colorize, &pct_str, &[Colors::BOLD, Colors::CYAN])
        };
        let bytes_styled = c_if(colorize, &format!("[{}]", bytes_str), &[Colors::DIM]);
        let speed_styled = c_if(colorize, &format!("({})", speed_str), &[Colors::DIM]);

        format!(
            "{} {}{}{} {} {} {}",
            label_styled,
            c_if(colorize, "[", &[Colors::DIM]),
            bar_styled,
            c_if(colorize, "]", &[Colors::DIM]),
            pct_styled,
            bytes_styled,
            speed_styled
        )
    }
}

pub fn format_spinner_line(
    frame: &str,
    message: &str,
    completed: usize,
    total: Option<usize>,
    verb: &str,
    elapsed_secs: f64,
    colorize: bool,
) -> String {
    let icon = c_if(colorize, frame, &[Colors::BOLD, Colors::CYAN]);
    let msg = c_if(colorize, message, &[Colors::WHITE]);
    let meta = match total {
        Some(t) => format!("({}/{} {}, {:.1}s)", completed, t, verb, elapsed_secs),
        None => format!("({:.1}s)", elapsed_secs),
    };
    format!("{} {} {}", icon, msg, c_if(colorize, &meta, &[Colors::DIM]))
}

pub struct Spinner {
    running: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn new(message: &str) -> Self {
        Self::spawn(message, None, "")
    }

    pub fn with_counter(message: &str, total: usize, verb: &str) -> Self {
        Self::spawn(message, Some(total), verb)
    }

    fn spawn(message: &str, total: Option<usize>, verb: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let completed = Arc::new(AtomicUsize::new(0));
        let running_clone = running.clone();
        let completed_clone = completed.clone();
        let msg = message.to_string();
        let verb_owned = verb.to_string();

        let handle = if io::stdout().is_terminal() {
            Some(thread::spawn(move || {
                let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let started = Instant::now();
                let mut idx = 0usize;
                while running_clone.load(Ordering::Relaxed) {
                    let frame = frames[idx % frames.len()];
                    let done = completed_clone.load(Ordering::Relaxed);
                    let line = format_spinner_line(
                        frame,
                        &msg,
                        done,
                        total,
                        &verb_owned,
                        started.elapsed().as_secs_f64(),
                        should_color_stdout(),
                    );
                    print!("\r{}\x1b[K", line);
                    let _ = io::stdout().flush();
                    thread::sleep(Duration::from_millis(80));
                    idx = idx.wrapping_add(1);
                }
                print!("\r\x1b[K");
                let _ = io::stdout().flush();
            }))
        } else {
            None
        };

        Self {
            running,
            completed,
            handle,
        }
    }

    pub fn counter(&self) -> Arc<AtomicUsize> {
        self.completed.clone()
    }

    pub fn stop(mut self) {
        self.stop_internal();
    }

    fn stop_internal(&mut self) {
        if self.running.swap(false, Ordering::Relaxed) {
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop_internal();
    }
}

pub fn confirm_dialog(prompt: &str, default: bool) -> bool {
    if !io::stdin().is_terminal() {
        eprintln!(
            "{} Non-interactive stdin detected; pass -y/--yes to confirm.",
            apt_prefix_err("WARN")
        );
        return false;
    }

    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    print!(
        "{} {} {}: ",
        c("::", &[Colors::BOLD, Colors::CYAN]),
        c(prompt, &[Colors::BOLD, Colors::WHITE]),
        c(suffix, &[Colors::BOLD, Colors::YELLOW])
    );
    let _ = io::stdout().flush();

    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(0) | Err(_) => return false,
        Ok(_) => {}
    }
    let trimmed = input.trim().to_lowercase();
    if trimmed.is_empty() {
        return default;
    }
    trimmed == "y" || trimmed == "yes"
}

pub fn render_banner_quick_help(colorize: bool) -> Vec<String> {
    let header_line1 = format!(
        "{} {} {}",
        c_if(colorize, "::", &[Colors::BOLD, Colors::CYAN]),
        c_if(
            colorize,
            &format!("appmgr v{}", env!("CARGO_PKG_VERSION")),
            &[Colors::BOLD, Colors::WHITE]
        ),
        c_if(
            colorize,
            "- Universal Linux Application Combine",
            &[Colors::DIM]
        )
    );
    let header_line2 = c_if(
        colorize,
        "   Pacman + APT hybrid (.deb, .AppImage, .tar.*, .zip, ELF)",
        &[Colors::DIM],
    );

    let cmds = [
        (
            "install <pkg...>",
            "Install from file, GitHub, registry, or search",
        ),
        ("search <query>", "Search configured sources and GitHub"),
        ("update", "Synchronize package databases and registries"),
        ("upgrade [app]", "Check and apply package updates"),
        ("remove <pkg...>", "Uninstall managed applications"),
        (
            "list / info <pkg>",
            "List installed apps or inspect package card",
        ),
        (
            "deps / doctor",
            "Inspect libraries and run system diagnostics",
        ),
        (
            "pin / unpin <app...>",
            "Freeze or unfreeze application versions",
        ),
    ];

    let mut lines = vec![
        header_line1,
        header_line2,
        String::new(),
        c_if(
            colorize,
            "Usage: appmgr [OPTIONS] <COMMAND> [ARGS]",
            &[Colors::BOLD, Colors::WHITE],
        ),
        String::new(),
        c_if(colorize, "Commands:", &[Colors::BOLD, Colors::CYAN]),
    ];

    for (cmd, desc) in cmds {
        let padded = format!("{:<22}", cmd);
        lines.push(format!(
            "  {} {}",
            c_if(colorize, &padded, &[Colors::BOLD, Colors::WHITE]),
            c_if(colorize, desc, &[Colors::DIM])
        ));
    }

    lines.push(String::new());
    lines.push(c_if(
        colorize,
        "Options: -y/--yes  --user | --system  --color <auto|always|never>  -h/--help",
        &[Colors::DIM],
    ));
    lines.push(format!(
        "Run {} for the complete command reference.",
        c_if(colorize, "'appmgr --help'", &[Colors::BOLD, Colors::YELLOW])
    ));

    lines
}

pub fn print_banner() {
    for line in render_banner_quick_help(should_color_stdout()) {
        println!("{}", line);
    }
}

pub fn restore_cursor() {
    print!("\x1b[?25h");
    let _ = io::stdout().flush();
}

pub fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let clean = strip_ansi(text);
    let clean_count = clean.chars().count();
    if clean_count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return truncate_chars(&clean, max_chars).to_string();
    }

    // Preserve ANSI CSI codes while counting only visible characters
    let keep_visible = max_chars - 3;
    let mut out = String::with_capacity(text.len() + 8);
    let mut visible = 0usize;
    let mut in_escape = false;
    let mut saw_ansi = false;

    for ch in text.chars() {
        if ch == '\x1b' {
            in_escape = true;
            saw_ansi = true;
            out.push(ch);
        } else if in_escape {
            out.push(ch);
            if ('@'..='~').contains(&ch) && ch != '[' {
                in_escape = false;
            }
        } else if visible < keep_visible {
            out.push(ch);
            visible += 1;
        } else {
            break;
        }
    }

    out.push_str("...");
    if saw_ansi {
        out.push_str(Colors::RESET);
    }
    out
}

fn is_priority_flex_header(header: &str) -> bool {
    let upper = header.to_uppercase();
    upper.contains("DESCRIPTION") || upper.contains("URL") || upper.contains("REPO")
}

fn shrink_pool_proportionally(
    widths: &mut [usize],
    indices: &[usize],
    floors: &[usize],
    overflow: &mut usize,
) {
    if *overflow == 0 || indices.is_empty() {
        return;
    }

    let slacks: Vec<(usize, usize)> = indices
        .iter()
        .copied()
        .map(|i| (i, widths[i].saturating_sub(floors[i])))
        .filter(|&(_, s)| s > 0)
        .collect();

    let total_slack: usize = slacks.iter().map(|(_, s)| *s).sum();
    if total_slack == 0 {
        return;
    }

    let to_cut = (*overflow).min(total_slack);
    let mut cut_so_far = 0usize;
    let mut cuts = vec![0usize; widths.len()];

    for &(idx, slack) in &slacks {
        let share = (to_cut * slack) / total_slack;
        let actual = share.min(slack);
        cuts[idx] = actual;
        cut_so_far += actual;
    }

    // Distribute rounding remainder one char at a time from widest remaining slack
    let mut rem = to_cut.saturating_sub(cut_so_far);
    if rem > 0 {
        let mut sorted = slacks.clone();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        while rem > 0 {
            let mut progressed = false;
            for &(idx, slack) in &sorted {
                if rem == 0 {
                    break;
                }
                if cuts[idx] < slack {
                    cuts[idx] += 1;
                    rem -= 1;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    for (idx, cut) in cuts.into_iter().enumerate() {
        widths[idx] = widths[idx].saturating_sub(cut);
        *overflow = overflow.saturating_sub(cut);
    }
}

pub fn compute_table_widths(headers: &[&str], rows: &[Vec<String>], max_cols: usize) -> Vec<usize> {
    if headers.is_empty() {
        return Vec::new();
    }

    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                let clean_len = strip_ansi(cell).chars().count();
                widths[i] = widths[i].max(clean_len);
            }
        }
    }

    let spacing = 2 + headers.len().saturating_sub(1) * 3;
    let total_width: usize = widths.iter().sum::<usize>() + spacing;

    if total_width <= max_cols {
        return widths;
    }

    let mut overflow = total_width - max_cols;

    // Tier 1: Proportionally shrink priority long columns (DESCRIPTION, URL, REPO)
    let flex_indices: Vec<usize> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| is_priority_flex_header(h))
        .map(|(i, _)| i)
        .collect();
    let flex_floors: Vec<usize> = headers
        .iter()
        .map(|h| h.chars().count().clamp(4, 12))
        .collect();
    shrink_pool_proportionally(&mut widths, &flex_indices, &flex_floors, &mut overflow);

    // Tier 2: Proportionally shrink any remaining columns down to readable minimum
    if overflow > 0 {
        let all_indices: Vec<usize> = (0..widths.len()).collect();
        let floors: Vec<usize> = headers
            .iter()
            .map(|h| h.chars().count().clamp(3, 8))
            .collect();
        shrink_pool_proportionally(&mut widths, &all_indices, &floors, &mut overflow);
    }

    // Tier 3: Extreme narrow fallback (shrink down to 1 char minimum)
    if overflow > 0 {
        let all_indices: Vec<usize> = (0..widths.len()).collect();
        let min_floors = vec![1usize; widths.len()];
        shrink_pool_proportionally(&mut widths, &all_indices, &min_floors, &mut overflow);
    }

    widths
}

pub fn render_table(
    headers: &[&str],
    rows: &[Vec<String>],
    max_cols: usize,
    colorize: bool,
) -> Vec<String> {
    if headers.is_empty() {
        return Vec::new();
    }

    if rows.is_empty() {
        return vec![format!(
            "  {}",
            c_if(colorize, "(no entries found)", &[Colors::DIM])
        )];
    }

    let widths = compute_table_widths(headers, rows, max_cols);
    let mut lines = Vec::with_capacity(rows.len() + 2);

    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let col_limit = widths[i];
            let clean_h = truncate_with_ellipsis(h, col_limit);
            let pad = col_limit.saturating_sub(clean_h.chars().count());
            format!(
                "{}{}",
                c_if(colorize, &clean_h, &[Colors::BOLD, Colors::CYAN]),
                " ".repeat(pad)
            )
        })
        .collect();
    lines.push(format!("  {}", header_line.join("   ")));

    let sep_line: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
    lines.push(format!(
        "  {}",
        c_if(colorize, &sep_line.join("───"), &[Colors::DIM])
    ));

    for row in rows {
        let row_line: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let cell = row.get(i).map(String::as_str).unwrap_or("");
                let col_limit = widths[i];
                let display_cell = truncate_with_ellipsis(cell, col_limit);
                let actual_len = strip_ansi(&display_cell).chars().count();
                let pad = col_limit.saturating_sub(actual_len);
                format!("{}{}", display_cell, " ".repeat(pad))
            })
            .collect();
        lines.push(format!("  {}", row_line.join("   ")));
    }

    lines
}

pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let max_cols = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);
    let lines = render_table(headers, rows, max_cols, should_color_stdout());
    for line in lines {
        println!("{}", line);
    }
    println!();
}

pub fn format_card_section(title: &str, width: usize, colorize: bool) -> String {
    let prefix_len = 3 + title.chars().count() + 1;
    let fill_len = width.saturating_sub(prefix_len);
    format!(
        "  {} {} {}",
        c_if(colorize, "──", &[Colors::DIM]),
        c_if(colorize, title, &[Colors::BOLD, Colors::CYAN]),
        c_if(colorize, &"─".repeat(fill_len), &[Colors::DIM])
    )
}

pub fn format_card_row(label: &str, label_width: usize, value: &str, colorize: bool) -> String {
    let padded_label = format!("{:<width$}", label, width = label_width);
    format!(
        "  {} {}",
        c_if(colorize, &padded_label, &[Colors::BOLD, Colors::CYAN]),
        value
    )
}

pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ('@'..='~').contains(&ch) && ch != '[' {
                in_escape = false;
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_chars_utf8_safe() {
        assert_eq!(truncate_chars("", 20), "");
        assert_eq!(truncate_chars("short", 20), "short");
        assert_eq!(
            truncate_chars("123456789012345678901", 20),
            "12345678901234567890"
        );
        let multibyte = "app_café_résumé_über_naïve_long_name";
        let truncated = truncate_chars(multibyte, 20);
        assert_eq!(truncated.chars().count(), 20);
        assert_eq!(truncated, "app_café_résumé_über");
    }

    #[test]
    fn test_strip_ansi_sequences() {
        assert_eq!(strip_ansi("plain text"), "plain text");
        assert_eq!(strip_ansi("\x1b[1;36mhello\x1b[0m"), "hello");
        assert_eq!(
            strip_ansi("\r\x1b[32m[ OK ]\x1b[0m done\x1b[K"),
            "\r[ OK ] done"
        );
        assert_eq!(strip_ansi("\x1b[?25hcursor"), "cursor");
        assert_eq!(
            strip_ansi("\x1b[1;35m[PINNED]\x1b[0m café — 1.2 MiB"),
            "[PINNED] café — 1.2 MiB"
        );
    }

    #[test]
    fn test_apt_prefix_uniform_width() {
        let statuses = ["OK", "DONE", "INFO", "WARN", "FAIL", "WORK", "GET", "TRIG"];
        let widths: Vec<usize> = statuses
            .iter()
            .map(|s| {
                strip_ansi(&format_apt_prefix(s, |t, st| c_if(true, t, st)))
                    .chars()
                    .count()
            })
            .collect();
        assert!(widths.iter().all(|&w| w == 6));
        assert!(apt_prefix("GET").contains("GET"));
        assert!(apt_prefix("OK").contains("OK"));
    }

    #[test]
    fn test_color_mode_resolution() {
        assert!(should_color_for(true, ColorMode::Auto, false, false));
        assert!(!should_color_for(false, ColorMode::Auto, false, false));
        assert!(!should_color_for(true, ColorMode::Auto, true, false));
        assert!(!should_color_for(true, ColorMode::Auto, false, true));
        assert!(!should_color_for(true, ColorMode::Never, false, false));
        assert!(should_color_for(false, ColorMode::Always, true, true));
    }

    #[test]
    fn test_pacman_bar_width_and_pulsing_mode() {
        let mut bar = PacmanBar::new(
            10 * 1024 * 1024,
            "Downloading appimage-über-release.AppImage",
        );
        bar.current_bytes = 5 * 1024 * 1024;

        for cols in [45, 80, 120] {
            for colored in [false, true] {
                let line = bar.render_line_with_elapsed(cols, false, None, colored, 2.0);
                let visible = strip_ansi(&line);
                assert!(
                    visible.chars().count() <= cols,
                    "Line overflowed cols={}: '{}' ({})",
                    cols,
                    visible,
                    visible.chars().count()
                );
                assert!(visible.contains("MiB/s"));
                assert!(visible.contains("50%"));
                if cols >= 60 {
                    assert!(visible.contains('━') || visible.contains('╸'));
                }
            }
        }

        // Pulsing mode when total_bytes == 0
        let mut pulse_bar = PacmanBar::new(0, "Downloading stream.tar.zst");
        pulse_bar.current_bytes = 2 * 1024 * 1024;
        pulse_bar.frame = 3;
        let pulse_line = pulse_bar.render_line_with_elapsed(80, false, None, true, 1.0);
        let pulse_visible = strip_ansi(&pulse_line);
        assert!(pulse_visible.chars().count() <= 80);
        assert!(pulse_visible.contains("2.00 MiB"));
        assert!(pulse_visible.contains("2.00 MiB/s"));
        assert!(!pulse_visible.contains("100%"));
    }

    #[test]
    fn test_print_table_proportional_shrinking_and_empty_state() {
        let empty = render_table(&["PACKAGE", "VERSION"], &[], 80, false);
        assert_eq!(empty.len(), 1);
        assert!(empty[0].contains("(no entries found)"));

        let headers = ["SOURCE", "PACKAGE ID", "REPO", "DESCRIPTION"];
        let rows = vec![vec![
            "official".to_string(),
            c_if(true, "ripgrep", &[Colors::BOLD, Colors::CYAN]),
            "https://github.com/BurntSushi/ripgrep/releases/latest".to_string(),
            "ripgrep recursively searches directories for a regex pattern while respecting gitignore".to_string(),
        ]];

        let lines = render_table(&headers, &rows, 80, true);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let visible = strip_ansi(line);
            assert!(
                visible.chars().count() <= 80,
                "Table row exceeded 80 chars ({}): {}",
                visible.chars().count(),
                visible
            );
        }
        let row_visible = strip_ansi(&lines[2]);
        assert!(row_visible.contains("ripgrep"));
        assert!(row_visible.contains("..."));
    }

    #[test]
    fn test_banner_quick_help_fits_80x24() {
        let lines = render_banner_quick_help(true);
        assert!(lines.len() <= 24, "Banner had {} lines", lines.len());
        for line in &lines {
            let visible = strip_ansi(line);
            assert!(
                visible.chars().count() <= 80,
                "Banner line exceeded 80 chars ({}): {}",
                visible.chars().count(),
                visible
            );
        }
    }

    #[test]
    fn test_card_row_alignment_with_and_without_ansi() {
        let r1_color = format_card_row("Name:", 15, "btop", true);
        let r2_color = format_card_row("Description:", 15, "Resource monitor", true);
        let r1_plain = strip_ansi(&r1_color);
        let r2_plain = strip_ansi(&r2_color);

        assert_eq!(r1_plain.find("btop"), r2_plain.find("Resource"));
        assert_eq!(r1_plain, "  Name:           btop");
        assert_eq!(r2_plain, "  Description:    Resource monitor");
    }
}
