#[cfg(test)]
pub mod test {
    use confique::Config;
    use serde::{Deserialize, Serialize};

    #[derive(Config, Serialize, Deserialize, Debug, PartialEq)]
    pub struct TestConfig {
        /// The application host.
        #[config(default = "localhost")]
        pub host: String,

        /// The port number.
        #[config(default = 8080)]
        pub port: u16,

        /// Enable debug mode.
        #[config(default = false)]
        pub debug: bool,

        /// Database settings.
        #[config(nested)]
        pub database: TestDbConfig,
    }

    #[derive(Config, Serialize, Deserialize, Debug, PartialEq)]
    pub struct TestDbConfig {
        /// Connection string URL.
        pub url: Option<String>,

        /// Connection pool size.
        #[config(default = 5)]
        pub pool_size: usize,
    }

    #[test]
    fn test_config_loads_defaults() {
        let config = TestConfig::builder().load().unwrap();
        assert_eq!(config.host, "localhost");
        assert_eq!(config.port, 8080);
        assert!(!config.debug);
        assert_eq!(config.database.url, None);
        assert_eq!(config.database.pool_size, 5);
    }

    // -- Fixture for enum validation tests --------------------------------------

    #[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum Mode {
        Fast,
        Slow,
    }

    #[derive(Config, Serialize, Deserialize, Debug, PartialEq)]
    pub struct EnumConfig {
        #[config(default = "fast")]
        pub mode: Mode,

        #[config(default = 8080)]
        pub port: u16,
    }

    // -- Fixture for deserialize_with normalization tests ----------------------

    /// Deserialize a string and normalize it to lowercase.
    fn normalize_lowercase<'de, D>(deserializer: D) -> Result<String, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(s.to_lowercase())
    }

    #[derive(Config, Serialize, Deserialize, Debug, PartialEq)]
    pub struct NormalizedConfig {
        /// A color name, normalized to lowercase.
        #[config(deserialize_with = normalize_lowercase, default = "red")]
        pub color: String,

        /// Plain field with no normalization.
        #[config(default = 42)]
        pub count: u32,
    }
}
