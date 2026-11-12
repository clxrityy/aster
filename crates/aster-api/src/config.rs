//! Validate configuration without logging connection strings or credentials.

use std::{env, net::SocketAddr};

use thiserror::Error;
use url::Url;

use crate::auth::OwnerToken;

pub struct DatabaseConfig {
    // Deliberately no Debug implementation: URLs can contain credentials.
    pub url: String,
}

pub struct ServerConfig {
    pub database: DatabaseConfig,
    pub bind_addr: SocketAddr,
    pub owner_token: OwnerToken,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("DATABASE_URL must be a PostgreSQL URL with a host and database name")]
    DatabaseUrl,
    #[error("ASTER_BIND_ADDR must be an IP socket address with a nonzero port")]
    BindAddress,
    #[error("ASTER_ALLOW_REMOTE must be true or false")]
    RemoteFlag,
    #[error(
        "non-loopback binding requires ASTER_ALLOW_REMOTE=true and an explicit security boundary"
    )]
    RemoteBinding,
    #[error(transparent)]
    OwnerToken(#[from] crate::auth::InvalidOwnerToken),
}

impl DatabaseConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let value = lookup("DATABASE_URL").ok_or(ConfigError::DatabaseUrl)?;
        let url = Url::parse(&value).map_err(|_| ConfigError::DatabaseUrl)?;
        if !matches!(url.scheme(), "postgres" | "postgresql")
            || url.host_str().is_none()
            || url.path().trim_matches('/').is_empty()
            || url.fragment().is_some()
            || value.trim() != value
        {
            return Err(ConfigError::DatabaseUrl);
        }
        Ok(Self { url: value })
    }
}

impl ServerConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let database = DatabaseConfig::from_lookup(&lookup)?;
        let bind_addr: SocketAddr = lookup("ASTER_BIND_ADDR")
            .unwrap_or_else(|| "127.0.0.1:3000".into())
            .parse()
            .map_err(|_| ConfigError::BindAddress)?;
        if bind_addr.port() == 0 {
            return Err(ConfigError::BindAddress);
        }
        let remote = match lookup("ASTER_ALLOW_REMOTE").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            _ => return Err(ConfigError::RemoteFlag),
        };
        if !bind_addr.ip().is_loopback() && !remote {
            return Err(ConfigError::RemoteBinding);
        }
        let value = lookup("ASTER_OWNER_TOKEN").unwrap_or_default();
        let owner_token = OwnerToken::new(&value)?;
        Ok(Self {
            database,
            bind_addr,
            owner_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const TOKEN: &str = "a84276d93670f15ce499bd5680ef2c317690e803a1d59ba7f986ca451d2b036e";

    fn values() -> HashMap<&'static str, String> {
        HashMap::from([
            (
                "DATABASE_URL",
                "postgres://aster:password@localhost/aster".into(),
            ),
            ("ASTER_OWNER_TOKEN", TOKEN.into()),
        ])
    }

    #[test]
    fn defaults_are_loopback_only() {
        let values = values();
        let config = ServerConfig::from_lookup(|key| values.get(key).cloned()).unwrap();
        assert_eq!(config.bind_addr, "127.0.0.1:3000".parse().unwrap());
    }

    #[test]
    fn ipv6_loopback_is_allowed_without_opt_in() {
        let mut values = values();
        values.insert("ASTER_BIND_ADDR", "[::1]:3000".into());
        assert!(ServerConfig::from_lookup(|key| values.get(key).cloned()).is_ok());
    }

    #[test]
    fn remote_binding_is_an_explicit_opt_in() {
        let mut values = values();
        for address in ["0.0.0.0:3000", "192.168.1.5:3000", "[::]:3000"] {
            values.insert("ASTER_BIND_ADDR", address.into());
            assert!(matches!(
                ServerConfig::from_lookup(|key| values.get(key).cloned()),
                Err(ConfigError::RemoteBinding)
            ));
        }
        values.insert("ASTER_ALLOW_REMOTE", "true".into());
        assert!(ServerConfig::from_lookup(|key| values.get(key).cloned()).is_ok());
    }

    #[test]
    fn invalid_bind_and_remote_flags_are_rejected() {
        let mut values = values();
        for address in ["localhost:3000", "bad", "127.0.0.1:0"] {
            values.insert("ASTER_BIND_ADDR", address.into());
            assert!(matches!(
                ServerConfig::from_lookup(|key| values.get(key).cloned()),
                Err(ConfigError::BindAddress)
            ));
        }
        values.remove("ASTER_BIND_ADDR");
        values.insert("ASTER_ALLOW_REMOTE", "yes".into());
        assert!(matches!(
            ServerConfig::from_lookup(|key| values.get(key).cloned()),
            Err(ConfigError::RemoteFlag)
        ));
    }

    #[test]
    fn invalid_database_urls_are_rejected_without_echoing_them() {
        for value in [
            "",
            "postgres://localhost",
            "postgres://localhost/",
            "sqlite://db",
            "https://owner:secret@localhost/aster",
            "postgres:///aster",
            "postgres://localhost/aster#secret",
            " postgres://localhost/aster",
        ] {
            let result = DatabaseConfig::from_lookup(|_| Some(value.into()));
            assert!(matches!(result, Err(ConfigError::DatabaseUrl)), "{value}");
            let error = result.err().unwrap().to_string();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn database_operations_do_not_require_an_owner_token() {
        let mut values = values();
        values.remove("ASTER_OWNER_TOKEN");
        assert!(DatabaseConfig::from_lookup(|key| values.get(key).cloned()).is_ok());
        assert!(matches!(
            ServerConfig::from_lookup(|key| values.get(key).cloned()),
            Err(ConfigError::OwnerToken(_))
        ));
    }

    #[test]
    fn encoded_password_and_postgresql_scheme_are_accepted() {
        let config = DatabaseConfig::from_lookup(|_| {
            Some("postgresql://aster:encoded%40password@127.0.0.1:5432/aster".into())
        });
        assert!(config.is_ok());
    }

    #[test]
    fn missing_database_is_an_explicit_error() {
        assert!(matches!(
            DatabaseConfig::from_lookup(|_| None),
            Err(ConfigError::DatabaseUrl)
        ));
    }
}
