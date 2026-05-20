use tracing::Level;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Clone)]
pub struct LoggingConfig {
    level: Level,
    with_env_filter: bool,
}

impl LoggingConfig {
    pub fn new(level: Level) -> Self {
        Self {
            level,
            with_env_filter: true,
        }
    }

    pub fn with_env_filter(mut self, enabled: bool) -> Self {
        self.with_env_filter = enabled;
        self
    }

    pub fn init(&self) {
        let filter = if self.with_env_filter {
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(self.level.as_str()))
        } else {
            EnvFilter::new(self.level.as_str())
        };

        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self::new(Level::INFO)
    }
}
