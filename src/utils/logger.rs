use super::shutdown;
use crate::{callback, database, error::Error, server};
use tracing_subscriber::{fmt::time::UtcTime, EnvFilter};

const TARGETS: &[&str] = &[
    callback::MODULE,
    database::MODULE,
    server::MODULE,
    shutdown::MODULE,
    env!("CARGO_PKG_NAME"),
];
const COMMA: &str = ",";
const INFO: &str = "=info";
const OFF: &str = "off";

pub fn initialize(directives: impl AsRef<str>) -> Result<(), Error> {
    let filter = EnvFilter::try_new(directives)?;

    tracing_subscriber::fmt()
        .with_timer(UtcTime::rfc_3339())
        .with_env_filter(filter)
        .init();

    Ok(())
}

fn default_filter_capacity() -> usize {
    OFF.len().saturating_add(
        TARGETS
            .iter()
            .map(|module| {
                COMMA
                    .len()
                    .saturating_add(module.len())
                    .saturating_add(INFO.len())
            })
            .sum(),
    )
}

pub fn default_filter() -> String {
    let mut filter = String::with_capacity(default_filter_capacity());

    filter.push_str(OFF);

    for target in TARGETS {
        filter.push_str(COMMA);
        filter.push_str(target);
        filter.push_str(INFO);
    }

    filter
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::EnvFilter;

    #[test]
    fn default_filter_capacity() {
        assert_eq!(
            super::default_filter().len(),
            super::default_filter_capacity()
        );
    }

    #[test]
    fn default_filter_is_valid() {
        assert!(EnvFilter::try_new(super::default_filter()).is_ok());
    }
}
