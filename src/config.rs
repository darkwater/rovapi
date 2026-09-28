use std::{env, env::VarError, fmt, net::SocketAddr};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub bind_address: SocketAddr,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ConfigError {
    variable: &'static str,
    value: String,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let value = match env::var("OVAPI_BIND_ADDRESS") {
            Ok(value) => value,
            Err(VarError::NotPresent) => DEFAULT_BIND_ADDRESS.to_owned(),
            Err(VarError::NotUnicode(value)) => {
                return Err(ConfigError {
                    variable: "OVAPI_BIND_ADDRESS",
                    value: value.to_string_lossy().into_owned(),
                });
            }
        };
        let bind_address = value.parse().map_err(|_| ConfigError {
            variable: "OVAPI_BIND_ADDRESS",
            value,
        })?;

        Ok(Self { bind_address })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_address: DEFAULT_BIND_ADDRESS
                .parse()
                .expect("the default bind address must be valid"),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} must be a socket address, got {:?}",
            self.variable, self.value
        )
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_binds_to_localhost() {
        assert_eq!(Config::default().bind_address.to_string(), "127.0.0.1:3000");
    }
}
