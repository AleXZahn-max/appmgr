use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

    pub const BRIGHT_RED: &'static str = "\x1b[91m";
    pub const BRIGHT_GREEN: &'static str = "\x1b[92m";
    pub const BRIGHT_YELLOW: &'static str = "\x1b[93m";
    pub const BRIGHT_BLUE: &'static str = "\x1b[94m";
    pub const BRIGHT_MAGENTA: &'static str = "\x1b[95m";
    pub const BRIGHT_CYAN: &'static str = "\x1b[96m";
    pub const BRIGHT_WHITE: &'static str = "\x1b[97m";
}

pub fn c(text: &str, styles: &[&str]) -> String {
    if !io::stdout().is_terminal() {
        return text.to_string();
    }
    format!("{}{}{}", styles.join(""), text, Colors::RESET)
}

#[allow(dead_code)]
pub fn pacman_prompt(msg: &str) -> String {
    format!(
        "{} {}",
        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        c(msg, &[Colors::BOLD, Colors::BRIGHT_WHITE])
    )
}

pub fn apt_prefix(status: &str) -> String {
    match status {
        "OK" => c("[  OK  ]", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
        "DONE" => c("[ DONE ]", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
        "INFO" => c("[ INFO ]", &[Colors::BOLD, Colors::BRIGHT_CYAN]),
        "WARN" => c("[ WARN ]", &[Colors::BOLD, Colors::BRIGHT_YELLOW]),
        "FAIL" => c("[ FAIL ]", &[Colors::BOLD, Colors::BRIGHT_RED]),
        "WORK" => c("[ WORK ]", &[Colors::BOLD, Colors::BRIGHT_MAGENTA]),
        "TRIG" => c("[ TRIG ]", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        _ => format!("[{}]", status),
    }
}

pub fn apt_preflight_animation() {
    println!("Reading package lists... Done");
    println!("Building dependency tree... Done");
    println!("Reading state information... Done");
}

pub struct PacmanBar {
    pub total: usize,
    pub current: usize,
    pub prefix: String,
    pub unit: String,
    pub frame: usize,
    is_tty: bool,
    last_render_len: usize,
}

impl PacmanBar {
    pub fn new(total: usize, prefix: &str, unit: &str) -> Self {
        Self {
            total: total.max(1),
            current: 0,
            prefix: prefix.to_string(),
            unit: unit.to_string(),
            frame: 0,
            is_tty: io::stdout().is_terminal(),
            last_render_len: 0,
        }
    }

    pub fn update(&mut self, current: usize, extra_text: &str) {
        self.current = current.min(self.total);
        self.frame += 1;

        if !self.is_tty {
            return;
        }

        let cols = terminal_size::terminal_size()
            .map(|(w, _)| w.0 as usize)
            .unwrap_or(80);

        let ratio = self.current as f64 / self.total as f64;
        let percent = (ratio * 100.0) as usize;

        let pacman_chars = ["C", "c", "O", "c"];
        let pacman_mouth = pacman_chars[self.frame % pacman_chars.len()];

        let fixed_part = format!(
            " {} {:3}% [{}/{} {}] {}",
            self.prefix, percent, self.current, self.total, self.unit, extra_text
        );
        let available_bar_width = (cols.saturating_sub(fixed_part.len() + 10)).clamp(10, 36);

        let completed_chars = ((available_bar_width as f64) * ratio) as usize;
        let remaining_chars = available_bar_width.saturating_sub(completed_chars);

        let pellets = ["•", "·", "•", "·"];
        let pellet_char = pellets[(self.frame / 2) % pellets.len()];

        let bar_body = c(
            &"#".repeat(completed_chars),
            &[Colors::BOLD, Colors::BRIGHT_YELLOW],
        );

        let (pacman_sprite, candy_trail) = if completed_chars >= available_bar_width {
            (
                c(" (•‿•)", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
                String::new(),
            )
        } else {
            let sprite = c(
                &format!(" {}", pacman_mouth),
                &[Colors::BOLD, Colors::BRIGHT_YELLOW],
            );
            let trail_len = remaining_chars.saturating_sub(1);
            let raw_trail = format!(" {} ", pellet_char).repeat(trail_len / 2 + 1);
            let trail = if raw_trail.chars().count() > trail_len {
                raw_trail.chars().take(trail_len).collect::<String>()
            } else {
                raw_trail
            };
            (sprite, c(&trail, &[Colors::BRIGHT_WHITE]))
        };

        let rendered = format!(
            "\r{}{}{}{}{} {} {} {}",
            c("(", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            bar_body,
            pacman_sprite,
            candy_trail,
            c(")", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
            c(&format!("{:3}%", percent), &[Colors::BOLD, Colors::BRIGHT_CYAN]),
            c(
                &format!("[{}/{} {}]", self.current, self.total, self.unit),
                &[Colors::DIM, Colors::WHITE]
            ),
            c(extra_text, &[Colors::BRIGHT_WHITE])
        );

        let pad = self.last_render_len.saturating_sub(rendered.len());
        print!("{}{}", rendered, " ".repeat(pad));
        let _ = io::stdout().flush();
        self.last_render_len = rendered.len();
    }

    pub fn finish(&mut self, final_text: &str) {
        if self.is_tty {
            let bar_width = 30;
            let bar_body = c(&"#".repeat(bar_width), &[Colors::BOLD, Colors::BRIGHT_GREEN]);
            let sprite = c(" (•‿•)", &[Colors::BOLD, Colors::BRIGHT_GREEN]);

            let rendered = format!(
                "\r{}{}{}{} {} {} {}\n",
                c("(", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
                bar_body,
                sprite,
                c(")", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
                c("100%", &[Colors::BOLD, Colors::BRIGHT_GREEN]),
                c(
                    &format!("[{}/{} {}]", self.total, self.total, self.unit),
                    &[Colors::DIM, Colors::WHITE]
                ),
                c(final_text, &[Colors::BOLD, Colors::BRIGHT_GREEN])
            );
            let pad = self.last_render_len.saturating_sub(rendered.len());
            print!("{}{}", rendered, " ".repeat(pad));
            let _ = io::stdout().flush();
        }
    }
}

pub struct Spinner {
    _message: String,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn new(message: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let msg = message.to_string();

        let handle = if io::stdout().is_terminal() {
            Some(thread::spawn(move || {
                let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let mut idx = 0;
                while running_clone.load(Ordering::Relaxed) {
                    let frame = frames[idx % frames.len()];
                    print!(
                        "\r{} {}",
                        c(frame, &[Colors::BOLD, Colors::BRIGHT_CYAN]),
                        c(&msg, &[Colors::DIM, Colors::WHITE])
                    );
                    let _ = io::stdout().flush();
                    thread::sleep(Duration::from_millis(80));
                    idx += 1;
                }
                print!("\r{}\r", " ".repeat(msg.len() + 10));
                let _ = io::stdout().flush();
            }))
        } else {
            None
        };

        Self {
            _message: message.to_string(),
            running,
            handle,
        }
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
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    print!(
        "{} {} {}: ",
        c("::", &[Colors::BOLD, Colors::BRIGHT_BLUE]),
        c(prompt, &[Colors::BOLD, Colors::BRIGHT_WHITE]),
        c(suffix, &[Colors::BOLD, Colors::BRIGHT_YELLOW])
    );
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return default;
    }
    let trimmed = input.trim().to_lowercase();
    if trimmed.is_empty() {
        return default;
    }
    trimmed == "y" || trimmed == "yes"
}

pub fn print_banner() {
    let art = r#"
               _____  _____  __  __  _____  _____ 
    /\   _ __ |  __ \|  __ \|  \/  |/ ____||  __ \
   /  \ | '_ \| |__) | |__) | \  / | |  __ | |__) |
  / /\ \| |_) |  ___/|  ___/| |\/| | | |_ ||  _  / 
 / ____ \ .__/| |    | |    | |  | | |__| || | \ \ 
/_/    \_\_|  |_|    |_|    |_|  |_|\_____||_|  \_\
    "#;
    println!("{}", c(art, &[Colors::BOLD, Colors::BRIGHT_CYAN]));
    println!(
        "   {} {}\n",
        c(
            "Universal Linux Application Combine",
            &[Colors::BOLD, Colors::BRIGHT_WHITE]
        ),
        c(
            &format!("v{} (Rust)", env!("CARGO_PKG_VERSION")),
            &[Colors::BOLD, Colors::BRIGHT_YELLOW]
        )
    );
}

pub fn restore_cursor() {
    print!("\x1b[?25h");
    let _ = io::stdout().flush();
}

pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                // Strip ANSI escape codes when calculating cell width
                let clean_len = strip_ansi(cell).chars().count();
                widths[i] = widths[i].max(clean_len);
            }
        }
    }

    let max_cols = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);

    let total_spacing = if widths.is_empty() { 0 } else { (widths.len() - 1) * 3 + 4 };
    let total_width: usize = widths.iter().sum::<usize>() + total_spacing;

    if total_width > max_cols && !widths.is_empty() {
        let overflow = total_width - max_cols;
        if let Some((max_idx, max_w)) = widths.iter().copied().enumerate().max_by_key(|(_, w)| *w) {
            if max_w > overflow + 15 {
                widths[max_idx] = max_w - overflow;
            } else if max_w > 20 {
                widths[max_idx] = 20;
            }
        }
    }

    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let col_limit = *widths.get(i).unwrap_or(&h.len());
            let clean_h = if h.len() > col_limit {
                let mut t: String = h.chars().take(col_limit.saturating_sub(3)).collect();
                t.push_str("...");
                t
            } else {
                h.to_string()
            };
            let pad = col_limit.saturating_sub(clean_h.len());
            format!("{}{}", c(&clean_h, &[Colors::BOLD, Colors::BRIGHT_WHITE]), " ".repeat(pad))
        })
        .collect();
    println!("  {}", header_line.join("   "));

    let sep_line: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
    println!("  {}", c(&sep_line.join("───"), &[Colors::DIM, Colors::WHITE]));

    for row in rows {
        let row_line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let clean_str = strip_ansi(cell);
                let cell_len = clean_str.chars().count();
                let col_limit = *widths.get(i).unwrap_or(&cell_len);
                let display_cell = if cell_len > col_limit {
                    let mut truncated: String = clean_str.chars().take(col_limit.saturating_sub(3)).collect();
                    truncated.push_str("...");
                    truncated
                } else {
                    cell.clone()
                };
                let actual_len = strip_ansi(&display_cell).chars().count();
                let pad = col_limit.saturating_sub(actual_len);
                format!("{}{}", display_cell, " ".repeat(pad))
            })
            .collect();
        println!("  {}", row_line.join("   "));
    }
    println!();
}

pub fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else {
            result.push(ch);
        }
    }
    result
}
