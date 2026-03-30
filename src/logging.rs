use tracing_subscriber::EnvFilter;

use crate::error::LoggingError;

pub fn init_logging(level: &str) -> Result<(), LoggingError> {
    let filter = EnvFilter::try_new(level)
        .or_else(|_| EnvFilter::try_new(level.to_ascii_lowercase()))
        .map_err(|error| LoggingError::InitFailed(error.to_string()))?;

    match tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init()
    {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .to_string()
                .contains("global default trace dispatcher") =>
        {
            Ok(())
        }
        Err(error) => Err(LoggingError::InitFailed(error.to_string())),
    }
}
