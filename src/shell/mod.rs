//! Interactive shell module.

pub mod commands;

use crate::drivers::console::Color;
use alloc::string::{String, ToString};

/// The interactive shell state: current directory, input line buffer, and current colors.
pub struct Shell {
    pub cwd: String,
    input_buf: String,
    prompt_name: String,
    pub fg_color: Color,
    pub bg_color: Color,
}

impl Shell {
    pub fn new() -> Self {
        Shell {
            cwd: String::from("/"),
            input_buf: String::new(),
            prompt_name: String::from("rustos"),
            fg_color: Color::Yellow,
            bg_color: Color::Black,
        }
    }

    pub fn rsh() -> Self {
        let mut shell = Self::new();
        shell.prompt_name = "rsh".to_string();
        shell
    }

    /// Process a single decoded unicode character from the keyboard.
    pub fn handle_char(&mut self, c: char) {
        match c {
            // Ctrl+C — cancel the current input line and show a fresh prompt.
            '\x03' => {
                crate::println!("^C");
                self.input_buf.clear();
                self.print_prompt();
            }
            '\n' | '\r' => {
                crate::println!();
                let line = self.input_buf.clone();
                self.input_buf.clear();
                self.execute(&line);
                self.print_prompt();
            }
            // Backspace / DEL
            '\x08' | '\x7f' if self.input_buf.pop().is_some() => {
                crate::print!("\x08 \x08");
            }
            '\x08' | '\x7f' => {}
            c if c.is_ascii() && !c.is_ascii_control() => {
                self.input_buf.push(c);
                crate::print!("{}", c);
            }
            _ => {}
        }
    }

    /// Print the shell prompt, including the current working directory.
    pub fn print_prompt(&self) {
        crate::print!("{}:{}> ", self.prompt_name, self.cwd);
    }

    /// Resolve a path relative to the shell's current working directory.
    pub fn resolve_path(&self, path: &str) -> String {
        // Handle home directory shortcut (~)
        let resolved_home = if path == "~" {
            "/".to_string()
        } else if path.starts_with("~/") {
            path[1..].to_string() // Remove ~ but keep /
        } else {
            path.to_string()
        };

        if resolved_home.starts_with('/') {
            crate::vfs::RamFs::pub_normalize(&resolved_home)
        } else if resolved_home.is_empty() || resolved_home == "." {
            self.cwd.clone()
        } else {
            let base = if self.cwd == "/" {
                String::from("/")
            } else {
                alloc::format!("{}/", self.cwd)
            };
            crate::vfs::RamFs::pub_normalize(&alloc::format!("{}{}", base, resolved_home))
        }
    }

    fn execute(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let (cmd, rest) = match line.find(' ') {
            Some(pos) => (&line[..pos], line[pos + 1..].trim()),
            None => (line, ""),
        };
        let args: alloc::vec::Vec<&str> = rest.split_whitespace().collect();
        commands::dispatch(self, cmd, &args);
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}
