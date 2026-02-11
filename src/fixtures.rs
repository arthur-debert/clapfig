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
}
