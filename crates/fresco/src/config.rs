//! `config.toml` in the plugin's data folder.

use serde::Deserialize;

pub const PATH: &str = "config.toml";
pub const DEFAULT: &str = include_str!("../examples/config.toml");

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Namespace of every model id: `textures/a/b.png` becomes `<namespace>:a/b`.
    pub namespace: String,
    /// Path prefixes whose textures are held like tools instead of like flat items.
    pub handheld: Vec<String>,
    /// Rebuild when something under `textures/` or the config changes.
    pub watch: bool,
    /// Shown in the client's resource pack list.
    pub description: String,
    /// Resource pack formats the pack claims to support, lowest and highest.
    pub formats: [u32; 2],
    /// Players who decline the pack are kicked.
    pub force: bool,
    /// Text on the client's "accept this pack?" screen. Empty for none.
    pub prompt: String,
    pub serve: Serve,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Serve {
    /// Serve the pack over HTTP from inside the plugin and send it on join.
    pub enabled: bool,
    pub port: u16,
    /// Where players download the pack from. Empty: the address the player
    /// joined with, on `port`. Set it when the pack is hosted elsewhere, or
    /// when players reach the server through a proxy.
    pub url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            namespace: "sanctum".into(),
            handheld: Vec::new(),
            watch: true,
            description: "Server textures".into(),
            formats: [65, 999],
            force: false,
            prompt: String::new(),
            serve: Serve::default(),
        }
    }
}

impl Default for Serve {
    fn default() -> Self {
        Self { enabled: true, port: 8163, url: String::new() }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if !valid_part(&config.namespace) || config.namespace.contains('/') {
            return Err(format!("namespace {:?}: only a-z 0-9 _ - . are allowed", config.namespace));
        }
        if config.formats[0] > config.formats[1] {
            return Err("formats: the first number must not be above the second".into());
        }
        Ok(config)
    }
}

/// Whether `s` can be (part of) a resource location path.
pub fn valid_part(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_default_parses_to_the_defaults() {
        let shipped = Config::parse(DEFAULT).unwrap();
        assert_eq!(format!("{shipped:?}"), format!("{:?}", Config::default()));
    }

    #[test]
    fn bad_namespace_is_refused() {
        assert!(Config::parse("namespace = \"My Pack\"").is_err());
        assert!(Config::parse("namespace = \"a/b\"").is_err());
        assert!(Config::parse("namspace = \"typo\"").is_err());
    }
}
