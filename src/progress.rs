use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

pub struct Progress {
    bar: ProgressBar,
}

impl Progress {
    pub fn new(message: impl Into<String>) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg} [{elapsed_precise}]")
                .expect("valid progress template")
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        bar.set_message(message.into());
        bar.enable_steady_tick(Duration::from_millis(80));
        Self { bar }
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.bar.set_message(message.into());
    }

    pub fn finish(self, message: impl Into<String>) {
        self.bar
            .finish_with_message(format!("✓ {}", message.into()));
    }
}
